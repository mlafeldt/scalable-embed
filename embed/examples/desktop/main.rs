//! Minimal desktop app: portfolio total, and its change and the holdings ranked
//! by their return over a timeframe picked in the Timeframe menu. Loads at
//! startup and on Refresh (button or Cmd/Ctrl+R), in one query. Built with
//! saudade, a small retained-mode GUI toolkit. Shares its files with the
//! terminal example.
//!
//!   cargo run --example desktop
//!   cargo run --example desktop -- --file-session
//!
//! The session goes to the OS keychain, or with `--file-session` to a file in
//! the config folder, which spares unsigned dev builds a keychain prompt after
//! every rebuild.
//!
//! API calls block, so a worker thread owns the `Client` (`worker.rs`). The
//! GUI thread sends it requests and polls for replies on saudade's ~60 Hz tick
//! while any are outstanding. `dashboard.rs` and `login.rs` draw the window
//! and the login dialog.

mod dashboard;
mod format;
mod login;
mod worker;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use saudade::{
    App, Button, Column, Event, EventCtx, Menu, MenuBar, MenuItem, Modal, Painter, PopupRequest,
    Rect, Size, Theme, Widget, WindowConfig,
};
use scalable_embed::{DeviceCodePrompt, LoginEvent, Timeframe};

use dashboard::Dashboard;
use login::{LOGIN_H, LOGIN_W, login_dialog};
use worker::{Reply, Request, Snapshot};

const WINDOW_W: i32 = 720;
const WINDOW_H: i32 = 480;
const MENU_H: i32 = 20;

/// An entry in the Timeframe menu.
struct TimeframeChoice {
    label: &'static str,
    /// For the holdings' returns.
    holdings: Timeframe,
    /// For the portfolio's change.
    portfolio: Timeframe,
}

const fn choice(label: &'static str, holdings: Timeframe, portfolio: Timeframe) -> TimeframeChoice {
    TimeframeChoice {
        label,
        holdings,
        portfolio,
    }
}

const TIMEFRAMES: [TimeframeChoice; 6] = {
    use Timeframe::*;
    [
        choice("Today", Intraday, Intraday),
        choice("Week", OneWeek, OneWeek),
        choice("Month", OneMonth, OneMonth),
        choice("Year to date", YearToDate, YearToDate),
        choice("Year", OneYear, OneYear),
        // The portfolio has no since-buy change. Scalable shows its MAX one
        // instead, which also counts gains on positions sold since.
        choice("Since buy", SinceBuy, Max),
    ]
};

fn main() {
    let file_session = std::env::args().any(|arg| arg == "--file-session");
    let (requests, replies) = worker::spawn(file_session);

    let state = Rc::new(RefCell::new(State {
        session: Session::LoggedIn(None),
        status: String::new(),
        timeframe: 0,
        pending: 0,
        actions: Vec::new(),
    }));
    let modal = Rc::new(RefCell::new(Modal::new().on_dismiss({
        // Escape, the close button or Cancel.
        let state = state.clone();
        move |_| {
            let mut state = state.borrow_mut();
            let Session::LoggingIn(login) = &state.session else {
                return;
            };
            login.cancel.store(true, Ordering::Relaxed);
            state.session = Session::LoggedOut;
            state.status = "Not logged in. Use Actions > Log In.".to_string();
        }
    })));

    let session_is = |want: fn(&Session) -> bool| {
        let state = state.clone();
        move || want(&state.borrow().session)
    };
    let can_refresh = {
        let state = state.clone();
        move || !state.borrow().busy()
    };
    let push = |action: Action| {
        let state = state.clone();
        move |_: &mut EventCtx| state.borrow_mut().actions.push(action)
    };

    let timeframes = TIMEFRAMES
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            let (pick, check) = (state.clone(), state.clone());
            MenuItem::action(choice.label, move |cx| {
                pick.borrow_mut().timeframe = index;
                cx.request_paint();
            })
            .with_checked(move || check.borrow().timeframe == index)
        })
        .collect();
    let menu = MenuBar::new(Rect::new(0, 0, WINDOW_W, MENU_H))
        .add_menu(Menu::new(
            "&Actions",
            vec![
                MenuItem::action("&Refresh", push(Action::Refresh))
                    .with_accel("Ctrl+R")
                    .with_enabled(can_refresh),
                MenuItem::separator(),
                MenuItem::action("&Log In", push(Action::LogIn))
                    .with_enabled(session_is(|s| matches!(s, Session::LoggedOut))),
                MenuItem::action("Log &Out", push(Action::LogOut))
                    .with_enabled(session_is(|s| matches!(s, Session::LoggedIn(_)))),
                MenuItem::separator(),
                MenuItem::action("&Quit", |cx| cx.close()),
            ],
        ))
        .add_menu(Menu::new("&Timeframe", timeframes));

    let refresh = Button::new(Rect::new(0, 0, 0, 0), "Refresh").on_click(push(Action::Refresh));
    let ui = Column::new()
        .add_fixed(menu, MENU_H)
        .add_fill(Dashboard::new(state.clone(), refresh))
        .add_overlay(SharedModal(modal.clone()));

    let mut root = Root {
        ui,
        state,
        modal,
        requests,
        replies,
        last_login: 0,
    };
    root.refresh();
    let window = WindowConfig::new("scalable-embed", WINDOW_W, WINDOW_H)
        .resizable(true)
        .min_size(480, 320);
    App::new(window, root).run();
}

// ---------------------------------------------------------------------------
// GUI state, shared by the widgets on the GUI thread.

struct State {
    session: Session,
    status: String,
    /// Index into `TIMEFRAMES`: whose returns the window shows and sorts by.
    timeframe: usize,
    /// Requests the worker hasn't answered yet.
    pending: usize,
    /// Menu and button picks, handled once the event is dispatched.
    actions: Vec<Action>,
}

impl State {
    fn busy(&self) -> bool {
        self.pending > 0
    }

    fn snapshot(&self) -> Option<&Snapshot> {
        match &self.session {
            Session::LoggedIn(snapshot) => snapshot.as_ref(),
            _ => None,
        }
    }
}

enum Session {
    LoggedOut,
    /// The login dialog is open.
    LoggingIn(LoginView),
    /// Logged in, or assumed to be at startup, with data once it arrives.
    LoggedIn(Option<Snapshot>),
}

struct LoginView {
    /// Tells this login's replies from those of logins cancelled before.
    id: u64,
    cancel: Arc<AtomicBool>,
    prompt: Option<DeviceCodePrompt>,
    status: &'static str,
}

impl LoginView {
    fn update(&mut self, event: LoginEvent) {
        self.status = match event {
            LoginEvent::DeviceCode(prompt) => {
                self.prompt = Some(prompt);
                "Open the browser and confirm this code."
            }
            LoginEvent::WaitingForBrowser => "Open the browser and confirm this code.",
            LoginEvent::WaitingForSecondFactor => "Approve the login on your linked device.",
        };
    }
}

#[derive(Clone, Copy)]
enum Action {
    Refresh,
    LogIn,
    LogOut,
}

/// The window's root: forwards everything to the UI, runs the actions events
/// leave behind, and polls for worker replies on ticks. It sits above the
/// column so it keeps ticking while the login dialog captures events.
struct Root {
    ui: Column,
    state: Rc<RefCell<State>>,
    modal: Rc<RefCell<Modal>>,
    requests: Sender<Request>,
    replies: Receiver<Reply>,
    last_login: u64,
}

impl Root {
    fn poll_replies(&mut self, ctx: &mut EventCtx) {
        let mut changed = false;
        while let Ok(reply) = self.replies.try_recv() {
            if reply.is_final() {
                let mut state = self.state.borrow_mut();
                state.pending = state.pending.saturating_sub(1);
            }
            self.handle(reply);
            changed = true;
        }
        if changed {
            self.sync_modal();
            ctx.request_paint();
        }
    }

    fn run_actions(&mut self, ctx: &mut EventCtx) {
        let actions = std::mem::take(&mut self.state.borrow_mut().actions);
        if actions.is_empty() {
            return;
        }
        for action in actions {
            match action {
                Action::Refresh => self.refresh(),
                Action::LogIn => self.start_login(),
                Action::LogOut => self.send(Request::Logout),
            }
        }
        self.sync_modal();
        ctx.request_tick();
        ctx.request_paint();
    }

    fn refresh(&mut self) {
        if !self.state.borrow().busy() {
            self.state.borrow_mut().status = "Refreshing...".to_string();
            self.send(Request::Refresh);
        }
    }

    fn handle(&mut self, reply: Reply) {
        let mut state = self.state.borrow_mut();
        match reply {
            Reply::Data(snapshot) => {
                state.session = Session::LoggedIn(Some(snapshot));
                state.status = format!(
                    "Updated {}",
                    jiff::ZonedDateTime::now().strftime("%H:%M:%S")
                );
            }
            Reply::Error(err) => state.status = format!("Error: {err}"),
            Reply::NeedsLogin => {
                drop(state);
                self.start_login();
            }
            Reply::Login { id, event } => {
                if let Session::LoggingIn(login) = &mut state.session
                    && login.id == id
                {
                    login.update(event);
                }
            }
            Reply::LoginDone { id, result } => {
                // A cancelled login's dialog is gone already.
                if !matches!(&state.session, Session::LoggingIn(login) if login.id == id) {
                    return;
                }
                match result {
                    Ok(()) => {
                        state.session = Session::LoggedIn(None);
                        drop(state);
                        self.refresh();
                    }
                    Err(err) => {
                        state.session = Session::LoggedOut;
                        state.status = format!("Login failed: {err}");
                    }
                }
            }
            Reply::LoggedOut(Ok(())) => {
                state.session = Session::LoggedOut;
                state.status = "Logged out. Use Actions > Log In.".to_string();
            }
            Reply::LoggedOut(Err(err)) => state.status = format!("Logout failed: {err}"),
        }
    }

    fn start_login(&mut self) {
        let mut state = self.state.borrow_mut();
        if matches!(state.session, Session::LoggingIn(_)) {
            return;
        }
        self.last_login += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        state.session = Session::LoggingIn(LoginView {
            id: self.last_login,
            cancel: cancel.clone(),
            prompt: None,
            status: "Requesting a login code...",
        });
        state.status = "Logging in...".to_string();
        drop(state);
        self.send(Request::Login {
            id: self.last_login,
            cancel,
        });
    }

    fn send(&mut self, request: Request) {
        if self.requests.send(request).is_ok() {
            self.state.borrow_mut().pending += 1;
        }
    }

    /// Opens or closes the login dialog to match the session.
    fn sync_modal(&mut self) {
        let want_open = matches!(self.state.borrow().session, Session::LoggingIn(_));
        let mut modal = self.modal.borrow_mut();
        if want_open && !modal.is_open() {
            modal.show(
                "Log In to Scalable",
                Size::new(LOGIN_W, LOGIN_H),
                Box::new(login_dialog(&self.state)),
            );
        } else if !want_open && modal.is_open() {
            modal.dismiss();
        }
    }
}

impl Widget for Root {
    fn bounds(&self) -> Rect {
        self.ui.bounds()
    }
    fn paint(&mut self, painter: &mut Painter, theme: &Theme) {
        self.ui.paint(painter, theme);
    }
    fn paint_overlay(&mut self, painter: &mut Painter, theme: &Theme) {
        self.ui.paint_overlay(painter, theme);
    }
    fn event(&mut self, event: &Event, ctx: &mut EventCtx) {
        if matches!(event, Event::Tick) {
            self.poll_replies(ctx);
        }
        self.ui.event(event, ctx);
        self.run_actions(ctx);
    }
    fn on_cancel(&mut self, ctx: &mut EventCtx) {
        self.ui.on_cancel(ctx);
    }
    fn captures_pointer(&self) -> bool {
        self.ui.captures_pointer()
    }
    fn focusable(&self) -> bool {
        self.ui.focusable()
    }
    fn set_focused(&mut self, focused: bool) {
        self.ui.set_focused(focused);
    }
    fn accepts_accelerators(&self) -> bool {
        self.ui.accepts_accelerators()
    }
    fn layout(&mut self, bounds: Rect) {
        self.ui.layout(bounds);
    }
    fn popup_request(&self) -> Option<PopupRequest> {
        self.ui.popup_request()
    }
    fn collect_popups(&self, out: &mut Vec<PopupRequest>) {
        self.ui.collect_popups(out);
    }
    fn focus_first(&mut self) -> bool {
        self.ui.focus_first()
    }
    fn paints_own_background(&self) -> bool {
        self.ui.paints_own_background()
    }
    fn wants_ticks(&self) -> bool {
        self.state.borrow().busy()
    }
}

/// Lets `Root` open and close the dialog the column holds as its overlay.
struct SharedModal(Rc<RefCell<Modal>>);

impl Widget for SharedModal {
    fn bounds(&self) -> Rect {
        self.0.borrow().bounds()
    }
    fn paint(&mut self, painter: &mut Painter, theme: &Theme) {
        self.0.borrow_mut().paint(painter, theme);
    }
    fn paint_overlay(&mut self, painter: &mut Painter, theme: &Theme) {
        self.0.borrow_mut().paint_overlay(painter, theme);
    }
    fn event(&mut self, event: &Event, ctx: &mut EventCtx) {
        self.0.borrow_mut().event(event, ctx);
    }
    fn captures_pointer(&self) -> bool {
        self.0.borrow().captures_pointer()
    }
    fn accepts_accelerators(&self) -> bool {
        self.0.borrow().accepts_accelerators()
    }
    fn layout(&mut self, bounds: Rect) {
        self.0.borrow_mut().layout(bounds);
    }
    fn popup_request(&self) -> Option<PopupRequest> {
        self.0.borrow().popup_request()
    }
    fn collect_popups(&self, out: &mut Vec<PopupRequest>) {
        self.0.borrow().collect_popups(out);
    }
}
