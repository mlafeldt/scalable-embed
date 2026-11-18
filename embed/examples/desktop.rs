//! Minimal desktop app: portfolio total, and its change and the holdings ranked
//! by their return over a timeframe picked in the Timeframe menu. Loads at
//! startup and on Refresh (button or Cmd/Ctrl+R), in one request. Built with
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
//! API calls block, so a worker thread owns the `Client`. The GUI thread sends
//! it requests and polls for replies on saudade's ~60 Hz tick while any are
//! outstanding.

use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use saudade::{
    App, Button, Color, Column, Container, Event, EventCtx, FontFamily, FontStyle, Menu, MenuBar,
    MenuItem, Modal, Painter, PopupRequest, Rect, Size, Theme, Widget, WindowConfig,
};
use scalable_embed::{
    Client, DeviceCodePrompt, LoginProgressSink, Options, SessionBackendPreference,
};
use serde_json::Value;

const WINDOW_W: i32 = 720;
const WINDOW_H: i32 = 480;
const MENU_H: i32 = 20;
const LOGIN_W: i32 = 340;
const LOGIN_H: i32 = 170;

/// The timeframes in the Timeframe menu: API name, and menu and header label.
const TIMEFRAMES: [(&str, &str); 6] = [
    ("INTRADAY", "Today"),
    ("ONE_WEEK", "Week"),
    ("ONE_MONTH", "Month"),
    ("YEAR_TO_DATE", "Year to date"),
    ("ONE_YEAR", "Year"),
    ("SINCE_BUY", "Since buy"),
];

/// One value per entry in `TIMEFRAMES`.
type PerTimeframe = [Option<f64>; TIMEFRAMES.len()];

fn main() {
    let file_session = std::env::args().any(|arg| arg == "--file-session");
    let (requests, worker_requests) = mpsc::channel();
    let (worker_replies, replies) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = cancel.clone();
        thread::spawn(move || worker(worker_requests, worker_replies, cancel, file_session));
    }

    let state = Rc::new(RefCell::new(State::default()));
    let modal = Rc::new(RefCell::new(Modal::new().on_dismiss({
        // Escape, the close button or Cancel: stop polling for the login.
        let state = state.clone();
        let cancel = cancel.clone();
        move |_| {
            cancel.store(true, Ordering::Relaxed);
            let mut state = state.borrow_mut();
            state.login = None;
            state.status = "Not logged in. Use Actions > Log In.".to_string();
        }
    })));

    let can_log_in = {
        let state = state.clone();
        move || {
            let state = state.borrow();
            !state.logged_in && state.login.is_none()
        }
    };
    let can_log_out = {
        let state = state.clone();
        move || state.borrow().logged_in
    };
    let can_refresh = {
        let state = state.clone();
        move || !state.borrow().busy
    };
    let push = |action: Action| {
        let state = state.clone();
        move |_: &mut EventCtx| state.borrow_mut().actions.push(action)
    };

    let timeframes = TIMEFRAMES
        .iter()
        .enumerate()
        .map(|(index, (_, label))| {
            let (pick, check) = (state.clone(), state.clone());
            MenuItem::action(*label, move |cx| {
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
                MenuItem::action("&Log In", push(Action::LogIn)).with_enabled(can_log_in),
                MenuItem::action("Log &Out", push(Action::LogOut)).with_enabled(can_log_out),
                MenuItem::separator(),
                MenuItem::action("&Quit", |cx| cx.close()),
            ],
        ))
        .add_menu(Menu::new("&Timeframe", timeframes));

    let ui = Column::new()
        .add_fixed(menu, MENU_H)
        .add_fill(Dashboard {
            rect: Rect::new(0, 0, 0, 0),
            state: state.clone(),
            refresh: Button::new(Rect::new(0, 0, 0, 0), "Refresh").on_click(push(Action::Refresh)),
            scroll: 0.0,
            max_scroll: 0,
        })
        .add_overlay(SharedModal(modal.clone()));

    let mut root = Root {
        ui,
        state,
        modal,
        requests,
        replies,
        cancel,
        in_flight: 0,
    };
    root.refresh();
    let window = WindowConfig::new("scalable-embed", WINDOW_W, WINDOW_H)
        .resizable(true)
        .min_size(480, 320);
    App::new(window, root).run();
}

// ---------------------------------------------------------------------------
// Worker thread: owns the client and runs one request at a time.

enum Request {
    Refresh,
    Login,
    Logout,
}

enum Reply {
    Data(Snapshot),
    NeedsLogin,
    Error(String),
    DeviceCode(DeviceCodePrompt),
    LoginStatus(String),
    LoginDone(Result<(), String>),
    LoggedOut(Result<(), String>),
}

impl Reply {
    /// Whether this reply ends a request, as opposed to reporting progress.
    fn is_final(&self) -> bool {
        !matches!(self, Reply::DeviceCode(_) | Reply::LoginStatus(_))
    }
}

struct Snapshot {
    total: Option<f64>,
    /// The portfolio's change in €.
    changes: PerTimeframe,
    rows: Vec<Row>,
}

struct Row {
    name: String,
    value: Option<f64>,
    price: Option<f64>,
    /// Price performance, as a fraction (0.05 is +5 %).
    returns: PerTimeframe,
}

fn worker(
    requests: Receiver<Request>,
    replies: Sender<Reply>,
    cancel: Arc<AtomicBool>,
    file_session: bool,
) {
    let mut client = match client(file_session) {
        Ok(client) => client,
        Err(err) => {
            let _ = replies.send(Reply::Error(format!("{err:#}")));
            return;
        }
    };
    for request in requests {
        let reply = match request {
            Request::Refresh => refresh(&mut client),
            Request::Login => Reply::LoginDone(
                login(&mut client, &replies, &cancel).map_err(|err| format!("{err:#}")),
            ),
            Request::Logout => {
                Reply::LoggedOut(client.logout().map(drop).map_err(|err| format!("{err:#}")))
            }
        };
        if replies.send(reply).is_err() {
            return;
        }
    }
}

/// Same files and keychain entry as the terminal example.
fn client(file_session: bool) -> Result<Client> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let config_dir = std::path::Path::new(&home).join(".config/scalable-embed-example");
    let mut options = Options::new(config_dir, "dev.example.scalable-embed");
    if file_session {
        options.config.auth.session_backend = SessionBackendPreference::File;
    }
    Client::new(options)
}

fn refresh(client: &mut Client) -> Reply {
    match client.is_logged_in() {
        Ok(true) => {}
        Ok(false) => return Reply::NeedsLogin,
        Err(err) => return Reply::Error(format!("{err:#}")),
    }
    match snapshot(client) {
        Ok(snapshot) => Reply::Data(snapshot),
        Err(_) if matches!(client.is_logged_in(), Ok(false)) => Reply::NeedsLogin,
        Err(err) => Reply::Error(format!("{err:#}")),
    }
}

/// Everything the window shows, in one request: upstream's overview and
/// holdings queries trimmed to the fields used here, plus each security's
/// performances, which upstream only fetches per security in `broker quote`.
const SNAPSHOT_QUERY: &str = r#"
query DesktopSnapshot($accountId: ID!, $portfolioId: ID!) {
  account(id: $accountId) {
    brokerPortfolio(id: $portfolioId) {
      valuation(includeYearToDate: true) {
        valuation
        timeWeightedReturnByTimeframe {
          timeframe
          simpleAbsoluteReturn
        }
      }
      inventory {
        items {
          isin
          name
          inventory {
            position {
              filled
              fifoPrice
            }
          }
          portfolioIsinPerformance {
            valuation
          }
          quoteTick(source: CONSOLIDATED, includeYearToDate: true) {
            midPrice
            performancesByTimeframe {
              timeframe
              performance
            }
          }
        }
      }
    }
  }
}
"#;

fn snapshot(client: &mut Client) -> Result<Snapshot> {
    let data = client.graphql("DesktopSnapshot", SNAPSHOT_QUERY, None, Default::default())?;
    let portfolio = &data["account"]["brokerPortfolio"];
    let valuation = &portfolio["valuation"];
    let items = portfolio["inventory"]["items"]
        .as_array()
        .context("unexpected DesktopSnapshot response")?;

    let mut changes = per_timeframe(
        &valuation["timeWeightedReturnByTimeframe"],
        "simpleAbsoluteReturn",
    );
    // The overview has no since-buy figure, so add up each position's gain
    // over its average buy price.
    if let Some(since_buy) = TIMEFRAMES.iter().position(|(t, _)| *t == "SINCE_BUY") {
        changes[since_buy] = items
            .iter()
            .map(|item| {
                let position = &item["inventory"]["position"];
                Some(
                    decimal(&item["portfolioIsinPerformance"]["valuation"])?
                        - decimal(&position["filled"])? * decimal(&position["fifoPrice"])?,
                )
            })
            .sum();
    }
    let rows = items
        .iter()
        .map(|item| Row {
            name: item["name"]
                .as_str()
                .or(item["isin"].as_str())
                .unwrap_or_default()
                .to_string(),
            value: decimal(&item["portfolioIsinPerformance"]["valuation"]),
            price: decimal(&item["quoteTick"]["midPrice"]),
            returns: per_timeframe(&item["quoteTick"]["performancesByTimeframe"], "performance"),
        })
        .collect();
    Ok(Snapshot {
        total: decimal(&valuation["valuation"]),
        changes,
        rows,
    })
}

/// Picks `field` from a list of `{timeframe, ...}` entries for each of
/// `TIMEFRAMES`.
fn per_timeframe(list: &Value, field: &str) -> PerTimeframe {
    let list = list.as_array().map(Vec::as_slice).unwrap_or_default();
    TIMEFRAMES.map(|(timeframe, _)| {
        list.iter()
            .find(|entry| entry["timeframe"] == timeframe)
            .and_then(|entry| decimal(&entry[field]))
    })
}

/// The API sends some decimals as strings.
fn decimal(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
}

fn login(client: &mut Client, replies: &Sender<Reply>, cancel: &Arc<AtomicBool>) -> Result<()> {
    let mut progress = Progress {
        replies: replies.clone(),
        cancel: cancel.clone(),
    };
    client.login(
        |prompt| {
            let _ = replies.send(Reply::DeviceCode(prompt.clone()));
            Ok(())
        },
        &mut progress,
    )?;
    Ok(())
}

struct Progress {
    replies: Sender<Reply>,
    cancel: Arc<AtomicBool>,
}

impl Progress {
    fn status(&self, text: &str) -> Result<()> {
        let _ = self.replies.send(Reply::LoginStatus(text.to_string()));
        Ok(())
    }

    /// Sleeps in small steps so Cancel takes effect quickly.
    fn wait(&self, duration: Duration) -> Result<()> {
        let end = Instant::now() + duration;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                bail!("login cancelled");
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            thread::sleep(left.min(Duration::from_millis(100)));
        }
    }
}

impl LoginProgressSink for Progress {
    fn note_browser_confirmation_pending(&mut self, _polls: u64) -> Result<()> {
        self.status("Waiting for confirmation in the browser...")
    }
    fn note_browser_confirmation_slow_down(&mut self, _secs: u64) -> Result<()> {
        Ok(())
    }
    fn wait_for_browser_confirmation(&mut self, duration: Duration) -> Result<()> {
        self.wait(duration)
    }
    fn note_second_factor_wait(&mut self) -> Result<()> {
        self.status("Waiting for the second factor...")
    }
    fn wait_for_second_factor(&mut self, duration: Duration) -> Result<()> {
        self.wait(duration)
    }
    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// GUI state, shared by the widgets on the GUI thread.

#[derive(Default)]
struct State {
    snapshot: Option<Snapshot>,
    status: String,
    /// Set while the login dialog is open.
    login: Option<LoginView>,
    logged_in: bool,
    /// Index into `TIMEFRAMES`: whose returns the list shows and sorts by.
    timeframe: usize,
    /// Set while the worker has requests to answer.
    busy: bool,
    /// Menu and button picks, handled once the event is dispatched.
    actions: Vec<Action>,
}

struct LoginView {
    prompt: Option<DeviceCodePrompt>,
    status: String,
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
    cancel: Arc<AtomicBool>,
    in_flight: usize,
}

impl Root {
    fn poll_replies(&mut self, ctx: &mut EventCtx) {
        let mut changed = false;
        while let Ok(reply) = self.replies.try_recv() {
            if reply.is_final() {
                self.in_flight = self.in_flight.saturating_sub(1);
            }
            self.handle(reply);
            changed = true;
        }
        if changed {
            self.state.borrow_mut().busy = self.in_flight > 0;
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
        if self.in_flight == 0 {
            self.state.borrow_mut().status = "Refreshing...".to_string();
            self.send(Request::Refresh);
        }
    }

    fn handle(&mut self, reply: Reply) {
        let mut state = self.state.borrow_mut();
        match reply {
            Reply::Data(snapshot) => {
                state.snapshot = Some(snapshot);
                state.logged_in = true;
                state.status = format!(
                    "Updated {}",
                    jiff::ZonedDateTime::now().strftime("%H:%M:%S")
                );
            }
            Reply::Error(err) => {
                state.status = format!("Error: {err}");
            }
            Reply::NeedsLogin => {
                state.logged_in = false;
                drop(state);
                self.start_login();
            }
            Reply::DeviceCode(prompt) => {
                if let Some(login) = &mut state.login {
                    login.prompt = Some(prompt);
                    login.status = "Open the browser and confirm this code.".to_string();
                }
            }
            Reply::LoginStatus(text) => {
                if let Some(login) = &mut state.login {
                    login.status = text;
                }
            }
            Reply::LoginDone(Ok(())) => {
                state.login = None;
                state.logged_in = true;
                drop(state);
                self.refresh();
            }
            Reply::LoginDone(Err(err)) => {
                // After Cancel, the dialog is already closed and says why.
                if state.login.take().is_some() {
                    state.status = format!("Login failed: {err}");
                }
            }
            Reply::LoggedOut(result) => {
                state.snapshot = None;
                state.logged_in = false;
                state.status = match result {
                    Ok(()) => "Logged out. Use Actions > Log In.".to_string(),
                    Err(err) => format!("Logout failed: {err}"),
                };
            }
        }
    }

    fn start_login(&mut self) {
        self.cancel.store(false, Ordering::Relaxed);
        let mut state = self.state.borrow_mut();
        state.status = "Logging in...".to_string();
        state.login = Some(LoginView {
            prompt: None,
            status: "Requesting a login code...".to_string(),
        });
        drop(state);
        self.send(Request::Login);
    }

    fn send(&mut self, request: Request) {
        if self.requests.send(request).is_ok() {
            self.in_flight += 1;
            self.state.borrow_mut().busy = true;
        }
    }

    /// Opens or closes the login dialog to match the state.
    fn sync_modal(&mut self) {
        let want_open = self.state.borrow().login.is_some();
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
        self.in_flight > 0
    }
}

// ---------------------------------------------------------------------------
// Main view: total, its change and the Refresh button on top, holdings ranked
// by return below, status at the bottom.

struct Dashboard {
    rect: Rect,
    state: Rc<RefCell<State>>,
    refresh: Button,
    /// First visible holdings row.
    scroll: f32,
    max_scroll: usize,
}

const PAD: i32 = 12;
const GAP: i32 = 16;

impl Widget for Dashboard {
    fn bounds(&self) -> Rect {
        self.rect
    }

    fn layout(&mut self, bounds: Rect) {
        self.rect = bounds;
    }

    fn paints_own_background(&self) -> bool {
        true
    }

    fn captures_pointer(&self) -> bool {
        self.refresh.captures_pointer()
    }

    fn event(&mut self, event: &Event, ctx: &mut EventCtx) {
        if let Event::Scroll { pos, delta_y, .. } = event {
            if self.rect.contains(*pos) {
                let scroll = (self.scroll + delta_y).clamp(0.0, self.max_scroll as f32);
                if scroll.floor() != self.scroll.floor() {
                    ctx.request_paint();
                }
                self.scroll = scroll;
            }
            return;
        }
        let on_button = event
            .position()
            .is_some_and(|pos| self.refresh.bounds().contains(pos));
        if on_button || self.refresh.captures_pointer() {
            self.refresh.event(event, ctx);
        }
    }

    fn paint(&mut self, p: &mut Painter, theme: &Theme) {
        let state = self.state.borrow();
        let r = self.rect;
        let size = theme.font_size;
        let line = p.measure_text("", size).h;
        let (x, right) = (r.x + PAD, r.right() - PAD);
        let snapshot = state.snapshot.as_ref();
        p.fill_rect(r, theme.background);

        // Header: total with its change below it, Refresh button right.
        let mut y = r.y + PAD;
        y += big_amount(
            p,
            x,
            y,
            snapshot.and_then(|s| s.total),
            size * 2.6,
            theme.text,
        );
        let (timeframe, label) = (state.timeframe, TIMEFRAMES[state.timeframe].1);
        let change = snapshot.and_then(|s| s.changes[timeframe]);
        let change_text = signed_eur(change);
        p.text(x, y, &change_text, size, gain_color(theme, change));
        let w = p.measure_text(&change_text, size).w;
        p.text(x + w + 6, y, label, size, theme.disabled_text);
        y += line;

        let button_w = p
            .measure_text_styled("Refresh", size, FontFamily::Sans, theme.button_style)
            .w
            + 24;
        let button_h = line + 10;
        let button_y = r.y + PAD + (y - r.y - PAD - button_h) / 2;
        self.refresh
            .layout(Rect::new(right - button_w, button_y, button_w, button_h));
        self.refresh.set_enabled(!state.busy);
        self.refresh.paint(p, theme);
        y += PAD;

        // Status bar.
        let status_h = line + 8;
        let status = Rect::new(x, r.bottom() - PAD - status_h, r.w - 2 * PAD, status_h);
        field(p, status, theme);
        let saved = p.push_clip(status.inset(2));
        p.text(status.x + 6, status.y + 4, &state.status, size, theme.text);
        p.restore_clip(saved);

        // Holdings, best return first. Missing returns go last.
        let list = Rect::new(x, y, r.w - 2 * PAD, status.y - PAD / 2 - y);
        field(p, list, theme);
        let inner = list.inset(2);
        let saved = p.push_clip(inner);
        let Some(snapshot) = snapshot.filter(|s| !s.rows.is_empty()) else {
            let text = if state.snapshot.is_some() {
                "No holdings"
            } else if state.busy && state.login.is_none() {
                "Loading..."
            } else {
                "Not logged in"
            };
            p.text_centered(inner, text, size, theme.disabled_text);
            p.restore_clip(saved);
            return;
        };

        let mut rows: Vec<(&Row, Option<f64>)> = snapshot
            .rows
            .iter()
            .map(|row| (row, row.returns[timeframe]))
            .collect();
        rows.sort_by(|(_, a), (_, b)| match (a, b) {
            (Some(a), Some(b)) => b.total_cmp(a),
            (a, b) => b.is_some().cmp(&a.is_some()),
        });
        let cells: Vec<[String; 2]> = rows
            .iter()
            .map(|(row, ret)| [eur(row.value), signed_pct(ret.map(|r| r * 100.0))])
            .collect();

        let headers = ["Position", "Return", "Price"];
        let mut widths = headers.map(|h| {
            p.measure_text_styled(h, size, FontFamily::Sans, FontStyle::Bold)
                .w
        });
        for ((row, _), texts) in rows.iter().zip(&cells) {
            widths[0] = widths[0].max(p.measure_text(&texts[0], size).w);
            widths[1] = widths[1].max(p.measure_text(&texts[1], size).w);
            widths[2] = widths[2].max(raised_cents_width(p, row.price, size));
        }
        // The number columns spread evenly over the right half, so they keep
        // apart from each other instead of leaving all the slack after the
        // names. Their right edges, right to left:
        let gap = ((inner.w / 2 - widths.iter().sum::<i32>()) / 3).clamp(GAP, 6 * GAP);
        let mut edges = [0; 3];
        let mut edge = inner.right() - 8;
        for i in (0..3).rev() {
            edges[i] = edge;
            edge -= widths[i] + gap;
        }
        let name_x = inner.x + 8;
        let name_w = edge - name_x;

        let row_h = line + 6;
        let mut y = inner.y + 3;
        bold(p, name_x, y, "Security", size, theme.text);
        for (i, h) in headers.iter().enumerate() {
            let w = p
                .measure_text_styled(h, size, FontFamily::Sans, FontStyle::Bold)
                .w;
            bold(p, edges[i] - w, y, h, size, theme.text);
        }
        y += row_h;
        p.h_line(inner.x, y - 3, inner.w, theme.shadow);

        let visible = ((inner.bottom() - y) / row_h).max(1) as usize;
        self.max_scroll = rows.len().saturating_sub(visible);
        let first = (self.scroll as usize).min(self.max_scroll);
        for ((row, ret), texts) in rows.iter().zip(&cells).skip(first) {
            if y + row_h > inner.bottom() {
                break;
            }
            let name = fit(p, &row.name, size, name_w);
            p.text(name_x, y, &name, size, theme.text);
            let w = p.measure_text(&texts[0], size).w;
            p.text(edges[0] - w, y, &texts[0], size, theme.text);
            let w = p.measure_text(&texts[1], size).w;
            p.text(edges[1] - w, y, &texts[1], size, gain_color(theme, *ret));
            raised_cents(p, edges[2], y, row.price, size, theme.text);
            y += row_h;
        }
        p.restore_clip(saved);
    }
}

// ---------------------------------------------------------------------------
// Login dialog.

fn login_dialog(state: &Rc<RefCell<State>>) -> Container {
    let (bw, bh) = (110, 24);
    let open = Button::new(
        Rect::new(LOGIN_W - 2 * bw - 24, LOGIN_H - bh - 12, bw, bh),
        "Open Browser",
    )
    .default(true)
    .on_click({
        let state = state.clone();
        move |_| {
            let state = state.borrow();
            if let Some(prompt) = state.login.as_ref().and_then(|l| l.prompt.as_ref()) {
                let url = prompt
                    .verification_uri_complete
                    .as_deref()
                    .unwrap_or(&prompt.verification_uri);
                open_in_browser(url);
            }
        }
    });
    let cancel = Button::new(
        Rect::new(LOGIN_W - bw - 12, LOGIN_H - bh - 12, bw, bh),
        "Cancel",
    )
    .on_click(|cx| cx.request_dismiss());

    Container::new(LOGIN_W, LOGIN_H)
        .add(LoginText {
            rect: Rect::new(0, 0, LOGIN_W, LOGIN_H - bh - 24),
            state: state.clone(),
        })
        .add(open)
        .add(cancel)
}

/// The device code and the login's progress.
struct LoginText {
    rect: Rect,
    state: Rc<RefCell<State>>,
}

impl Widget for LoginText {
    fn bounds(&self) -> Rect {
        self.rect
    }

    fn layout(&mut self, bounds: Rect) {
        self.rect = bounds;
    }

    fn paint(&mut self, p: &mut Painter, theme: &Theme) {
        let state = self.state.borrow();
        let Some(login) = &state.login else { return };
        let size = theme.font_size;
        let line = p.measure_text("", size).h;
        let r = self.rect.inset(16);
        let mut y = r.y;
        if let Some(prompt) = &login.prompt {
            let big = size * 2.0;
            let code = &prompt.user_code;
            let w = p
                .measure_text_styled(code, big, FontFamily::Sans, FontStyle::Bold)
                .w;
            bold(p, r.x + (r.w - w) / 2, y, code, big, theme.text);
            y += p
                .measure_text_styled("", big, FontFamily::Sans, FontStyle::Bold)
                .h
                + 12;
        }
        for text in p.wrap_text(&login.status, size, r.w) {
            p.text(r.x, y, text, size, theme.text);
            y += line;
        }
    }
}

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

// ---------------------------------------------------------------------------
// Drawing and formatting helpers.

fn bold(p: &mut Painter, x: i32, y: i32, text: &str, size: f32, color: Color) {
    p.text_styled(x, y, text, size, color, FontFamily::Sans, FontStyle::Bold);
}

/// A sunken white field, like a Windows 3.1 list box.
fn field(p: &mut Painter, rect: Rect, theme: &Theme) {
    p.fill_rect(rect, theme.background);
    p.sunken_bevel(rect, theme.highlight, theme.shadow);
    p.stroke_rect(rect, theme.border);
}

/// A big amount the way the Scalable app shows it: euros, then small cents
/// with the currency below them. Returns the height drawn.
fn big_amount(p: &mut Painter, x: i32, y: i32, value: Option<f64>, size: f32, color: Color) -> i32 {
    let height = p
        .measure_text_styled("", size, FontFamily::Sans, FontStyle::Bold)
        .h;
    let Some(value) = value else {
        bold(p, x, y, "–", size, color);
        return height;
    };
    let text = number(value, 2);
    let (euros, cents) = text.split_once(',').unwrap_or((&text, ""));
    bold(p, x, y, euros, size, color);
    let w = p
        .measure_text_styled(euros, size, FontFamily::Sans, FontStyle::Bold)
        .w;
    let small = size * 0.4;
    let small_h = p.measure_text("", small).h;
    let inset = height / 8;
    p.text(x + w + 3, y + inset, cents, small, color);
    p.text(x + w + 3, y + height - small_h - inset, "€", small, color);
    height
}

/// A price with small raised cents and no currency, right-aligned at `right`.
fn raised_cents(p: &mut Painter, right: i32, y: i32, value: Option<f64>, size: f32, color: Color) {
    let Some(value) = value else {
        let w = p.measure_text("–", size).w;
        p.text(right - w, y, "–", size, color);
        return;
    };
    let text = number(value, 2);
    let (euros, cents) = text.split_once(',').unwrap_or((&text, ""));
    let small = size * 0.7;
    let cents_w = p.measure_text(cents, small).w;
    let euros_w = p.measure_text(euros, size).w;
    p.text(right - cents_w, y - 1, cents, small, color);
    p.text(right - cents_w - 1 - euros_w, y, euros, size, color);
}

fn raised_cents_width(p: &Painter, value: Option<f64>, size: f32) -> i32 {
    let Some(value) = value else {
        return p.measure_text("–", size).w;
    };
    let text = number(value, 2);
    let (euros, cents) = text.split_once(',').unwrap_or((&text, ""));
    p.measure_text(euros, size).w + 1 + p.measure_text(cents, size * 0.7).w
}

/// Shortens `text` with an ellipsis to fit `max_w`.
fn fit(p: &Painter, text: &str, size: f32, max_w: i32) -> String {
    if p.measure_text(text, size).w <= max_w {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while chars.pop().is_some() {
        let short = format!("{}…", chars.iter().collect::<String>().trim_end());
        if p.measure_text(&short, size).w <= max_w {
            return short;
        }
    }
    String::new()
}

fn gain_color(theme: &Theme, value: Option<f64>) -> Color {
    let bg = theme.background;
    let dark = (bg.red() as u32 + bg.green() as u32 + bg.blue() as u32) < 3 * 128;
    match value {
        Some(v) if v > 0.0 && dark => Color::rgb(0x5C, 0xD6, 0x5C),
        Some(v) if v > 0.0 => Color::rgb(0x00, 0x80, 0x00),
        Some(v) if v < 0.0 && dark => Color::rgb(0xFF, 0x6B, 0x6B),
        Some(v) if v < 0.0 => Color::rgb(0xC0, 0x00, 0x00),
        _ => theme.text,
    }
}

/// German format: `1.234,56`.
fn number(value: f64, decimals: usize) -> String {
    let fixed = format!("{:.*}", decimals, value.abs());
    let (int, frac) = fixed.split_once('.').unwrap_or((&fixed, ""));
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    if !frac.is_empty() {
        out.push(',');
        out.push_str(frac);
    }
    let zero = fixed.bytes().all(|b| b == b'0' || b == b'.');
    if value < 0.0 && !zero {
        format!("-{out}")
    } else {
        out
    }
}

fn signed(value: f64, decimals: usize) -> String {
    let text = number(value, decimals);
    if value > 0.0 && text.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
        format!("+{text}")
    } else {
        text
    }
}

fn eur(value: Option<f64>) -> String {
    value.map_or("–".to_string(), |v| format!("{} €", number(v, 2)))
}

fn signed_eur(value: Option<f64>) -> String {
    value.map_or("–".to_string(), |v| format!("{} €", signed(v, 2)))
}

fn signed_pct(value: Option<f64>) -> String {
    value.map_or("–".to_string(), |v| format!("{} %", signed(v, 2)))
}

fn open_in_browser(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(program).arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_german_numbers() {
        assert_eq!(eur(Some(123456.785)), "123.456,79 €");
        assert_eq!(eur(Some(-0.001)), "0,00 €");
        assert_eq!(signed_eur(Some(1234.5)), "+1.234,50 €");
        assert_eq!(signed_eur(Some(-12.0)), "-12,00 €");
        assert_eq!(signed_pct(Some(4.6123)), "+4,61 %");
        assert_eq!(signed_pct(Some(-0.004)), "0,00 %");
        assert_eq!(eur(None), "–");
    }
}
