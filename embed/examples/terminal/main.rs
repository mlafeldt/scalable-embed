//! Minimal terminal app: portfolio total, and its change and the holdings ranked
//! by their return over a timeframe picked with Tab. Loads at startup and on
//! `r`, in one query. Built with ratatui.
//!
//!   cargo run --example terminal
//!   cargo run --example terminal -- --file-session
//!
//! The session goes to the OS keychain, or with `--file-session` to a file in
//! the config folder, which spares unsigned dev builds a keychain prompt after
//! every rebuild.
//!
//! API calls block, so a worker thread owns the `Client` (`worker.rs`). The
//! main thread sends it requests and, while any are outstanding, checks for
//! replies between short waits for keys.

mod model;
mod numbers;
mod portfolio;
mod worker;

use std::io;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use anyhow::Context;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Padding, Paragraph, Row, Table, Tabs};
use ratatui::{DefaultTerminal, Frame};
use scalable_embed::{Client, DeviceCodePrompt, LoginEvent, Options, SessionBackendPreference};

use model::{Snapshot, TIMEFRAMES};
use numbers::{eur, number, signed_eur, signed_pct};
use worker::{Reply, Request};

/// How long to wait for a key before checking for replies again.
const TICK: Duration = Duration::from_millis(50);
const COLUMN_GAP: u16 = 3;
const LOGIN_W: u16 = 46;
const LOGIN_H: u16 = 9;

fn main() -> anyhow::Result<()> {
    let file_session = std::env::args().any(|arg| arg == "--file-session");
    // With files and a keychain entry of its own, apart from the `sc` CLI's.
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let config_dir = std::path::Path::new(&home).join(".config/scalable-embed-example");
    let mut options = Options::new(config_dir, "dev.example.scalable-embed");
    if file_session {
        options.config.auth.session_backend = SessionBackendPreference::File;
    }
    let client = Client::new(options)?;
    let (requests, replies) = worker::spawn(client);
    ratatui::run(|terminal| App::new(requests, replies).run(terminal))?;
    Ok(())
}

struct App {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
    session: Session,
    status: String,
    /// Index into `TIMEFRAMES`: whose returns the screen shows and sorts by.
    timeframe: usize,
    /// Requests the worker hasn't answered yet.
    pending: usize,
    /// First visible holdings row.
    scroll: usize,
    quit: bool,
}

enum Session {
    LoggedOut,
    /// The login dialog is open.
    LoggingIn(LoginView),
    /// Logged in, or assumed to be at startup, with data once it arrives.
    LoggedIn(Option<Snapshot>),
}

struct LoginView {
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
            LoginEvent::WaitingForSecondFactor => "Approve the login on your linked device.",
        };
    }
}

impl App {
    fn new(requests: Sender<Request>, replies: Receiver<Reply>) -> Self {
        Self {
            requests,
            replies,
            session: Session::LoggedIn(None),
            status: String::new(),
            timeframe: 0,
            pending: 0,
            scroll: 0,
            quit: false,
        }
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        self.refresh();
        while !self.quit {
            terminal.draw(|frame| self.draw(frame))?;
            // Replies don't end the wait for a key, so it's short while any
            // are outstanding.
            if (!self.busy() || event::poll(TICK)?)
                && let Event::Key(key) = event::read()?
                && key.is_press()
            {
                self.on_key(key);
            }
            while let Ok(reply) = self.replies.try_recv() {
                self.handle(reply);
            }
        }
        Ok(())
    }

    fn busy(&self) -> bool {
        self.pending > 0
    }

    // -----------------------------------------------------------------------
    // Keys and worker replies.

    fn on_key(&mut self, key: KeyEvent) {
        // The terminal is in raw mode, so Ctrl+C arrives as a key.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if let Session::LoggingIn(login) = &self.session {
            match key.code {
                KeyCode::Enter => {
                    if let Some(prompt) = &login.prompt {
                        open_in_browser(prompt.url());
                    }
                }
                KeyCode::Esc => {
                    login.cancel.store(true, Ordering::Relaxed);
                    self.session = Session::LoggedOut;
                    self.status = "Not logged in".to_string();
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('l') if !self.busy() && matches!(self.session, Session::LoggedOut) => {
                self.start_login();
            }
            KeyCode::Char('o') if !self.busy() && matches!(self.session, Session::LoggedIn(_)) => {
                self.send(Request::Logout);
            }
            KeyCode::Tab => self.timeframe = (self.timeframe + 1) % TIMEFRAMES.len(),
            KeyCode::BackTab => {
                self.timeframe = (self.timeframe + TIMEFRAMES.len() - 1) % TIMEFRAMES.len();
            }
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            // Drawing keeps it within the list.
            KeyCode::Down => self.scroll += 1,
            _ => {}
        }
    }

    fn refresh(&mut self) {
        if !self.busy() {
            self.status = "Refreshing...".to_string();
            self.send(Request::Refresh);
        }
    }

    fn handle(&mut self, reply: Reply) {
        if reply.is_final() {
            self.pending = self.pending.saturating_sub(1);
        }
        match reply {
            Reply::Data(snapshot) => {
                self.session = Session::LoggedIn(Some(snapshot));
                self.status = format!(
                    "Updated {}",
                    jiff::ZonedDateTime::now().strftime("%H:%M:%S")
                );
            }
            Reply::Error(err) => self.status = format!("Error: {err}"),
            Reply::NeedsLogin => self.start_login(),
            Reply::Login(event) => {
                if let Session::LoggingIn(login) = &mut self.session {
                    login.update(event);
                }
            }
            Reply::LoginDone(result) => {
                // A cancelled login's dialog is gone already.
                if !matches!(self.session, Session::LoggingIn(_)) {
                    return;
                }
                match result {
                    Ok(()) => {
                        self.session = Session::LoggedIn(None);
                        self.refresh();
                    }
                    Err(err) => {
                        self.session = Session::LoggedOut;
                        self.status = format!("Login failed: {err}");
                    }
                }
            }
            Reply::LoggedOut(Ok(())) => {
                self.session = Session::LoggedOut;
                self.status = "Logged out".to_string();
            }
            Reply::LoggedOut(Err(err)) => self.status = format!("Logout failed: {err}"),
        }
    }

    fn start_login(&mut self) {
        if self.busy() || matches!(self.session, Session::LoggingIn(_)) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.session = Session::LoggingIn(LoginView {
            cancel: cancel.clone(),
            prompt: None,
            status: "Requesting a login code...",
        });
        self.status = "Logging in...".to_string();
        self.send(Request::Login(cancel));
    }

    fn send(&mut self, request: Request) {
        if self.requests.send(request).is_ok() {
            self.pending += 1;
        }
    }

    // -----------------------------------------------------------------------
    // Drawing: timeframes on top, then the total and its change, the holdings
    // ranked by return, and the keys with the status beside them.

    fn draw(&mut self, frame: &mut Frame) {
        let [timeframes, _, total, change, holdings, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .horizontal_margin(1)
        .areas(frame.area());

        let labels = TIMEFRAMES.iter().map(|choice| choice.label);
        let tabs = Tabs::new(labels)
            .select(self.timeframe)
            .padding("", "")
            .divider(" │ ");
        frame.render_widget(tabs, timeframes);

        let snapshot = match &self.session {
            Session::LoggedIn(snapshot) => snapshot.as_ref(),
            _ => None,
        };
        let amount = snapshot.and_then(|s| s.total);
        frame.render_widget(Line::from(eur(amount)).bold(), total);
        let amount = snapshot.and_then(|s| s.changes[self.timeframe]);
        let line = Line::from(vec![
            Span::styled(signed_eur(amount), gain_style(amount)),
            Span::raw("  "),
            TIMEFRAMES[self.timeframe].label.dim(),
        ]);
        frame.render_widget(line, change);

        self.draw_holdings(frame, holdings);
        // The status goes in the corner. If the line is too short for both, it
        // covers the keys, as it may be an error.
        let status = Line::from(self.status.as_str());
        let [keys, corner] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(status.width() as u16),
        ])
        .spacing(2)
        .areas(footer);
        frame.render_widget(self.key_hints(), keys);
        frame.render_widget(status, corner);

        if let Session::LoggingIn(login) = &self.session {
            draw_login(frame, login);
        }
    }

    /// Holdings, or why there are none.
    fn draw_holdings(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().padding(Padding::horizontal(1));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let placeholder = match &self.session {
            Session::LoggedIn(Some(snapshot)) if !snapshot.rows.is_empty() => {
                let table = holdings_table(snapshot, self.timeframe, inner, &mut self.scroll);
                frame.render_widget(table, inner);
                return;
            }
            Session::LoggedIn(Some(_)) => "No holdings",
            Session::LoggedIn(None) if self.busy() => "Loading...",
            // The status says what went wrong.
            Session::LoggedIn(None) => "No data",
            Session::LoggedOut | Session::LoggingIn(_) => "Not logged in",
        };
        frame.render_widget(
            Line::from(placeholder).dim().centered(),
            inner.centered_vertically(Constraint::Length(1)),
        );
    }

    /// The keys, with those that do nothing right now dimmed.
    fn key_hints(&self) -> Line<'_> {
        let hints = [
            ("r", "Refresh", !self.busy()),
            (
                "l",
                "Log In",
                !self.busy() && matches!(self.session, Session::LoggedOut),
            ),
            (
                "o",
                "Log Out",
                !self.busy() && matches!(self.session, Session::LoggedIn(_)),
            ),
            ("Tab", "Timeframe", true),
            ("q", "Quit", true),
        ];
        let mut spans = Vec::new();
        for (key, label, enabled) in hints {
            let style = if enabled {
                Style::new()
            } else {
                Style::new().dim()
            };
            spans.push(Span::styled(key, style.bold()));
            spans.push(Span::styled(format!(" {label}   "), style));
        }
        Line::from(spans)
    }
}

/// The rows by their return over `TIMEFRAMES[timeframe]` that fit `area`, from
/// row `scroll` on, which it keeps within the list.
fn holdings_table(
    snapshot: &Snapshot,
    timeframe: usize,
    area: Rect,
    scroll: &mut usize,
) -> Table<'static> {
    const HEADERS: [&str; 3] = ["Position", "Return", "Price"];
    let rows = snapshot.ranked(timeframe);
    let cells: Vec<[String; 3]> = rows
        .iter()
        .map(|row| {
            [
                eur(row.value),
                signed_pct(row.returns[timeframe].map(|r| r * 100.0)),
                row.price.map_or("–".to_string(), |p| number(p, 2)),
            ]
        })
        .collect();
    let mut widths = HEADERS.map(|header| header.len() as u16);
    for cells in &cells {
        for (width, cell) in widths.iter_mut().zip(cells) {
            *width = (*width).max(cell.chars().count() as u16);
        }
    }
    let numbers = widths.iter().sum::<u16>() + 3 * COLUMN_GAP;
    let name_width = area.width.saturating_sub(numbers) as usize;

    let visible = area.height.saturating_sub(1).max(1) as usize;
    *scroll = (*scroll).min(rows.len().saturating_sub(visible));

    let right = |text: String| Cell::from(Line::from(text).right_aligned());
    let header = Row::new(
        std::iter::once(Cell::from("Security"))
            .chain(HEADERS.map(|header| right(header.to_string()))),
    )
    .bold();
    let body =
        rows.iter()
            .zip(cells)
            .skip(*scroll)
            .take(visible)
            .map(|(row, [value, ret, price])| {
                Row::new([
                    Cell::from(leader(&row.name, name_width)),
                    right(value),
                    right(ret).style(gain_style(row.returns[timeframe])),
                    right(price),
                ])
            });
    let [value, ret, price] = widths.map(Constraint::Length);
    Table::new(body, [Constraint::Fill(1), value, ret, price])
        .header(header)
        .column_spacing(COLUMN_GAP)
}

/// The login dialog: the device code, the login's progress, and the keys to
/// open the browser or cancel.
fn draw_login(frame: &mut Frame, login: &LoginView) {
    let area = frame
        .area()
        .centered(Constraint::Length(LOGIN_W), Constraint::Length(LOGIN_H));
    let block = Block::bordered()
        .title("Log In to Scalable")
        .padding(Padding::uniform(1));
    let code = login.prompt.as_ref().map_or("", |p| p.user_code.as_str());
    let open = if login.prompt.is_some() {
        Style::new()
    } else {
        Style::new().dim()
    };
    let text = vec![
        Line::from(code).bold(),
        Line::default(),
        Line::from(login.status),
        Line::default(),
        Line::from(vec![
            Span::styled("Enter", open.bold()),
            Span::styled(" Open Browser   ", open),
            "Esc".bold(),
            " Cancel".into(),
        ]),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(text).centered().block(block), area);
}

/// `name` followed by dots up to `width` columns, which lead the eye to the
/// row's numbers.
fn leader(name: &str, width: usize) -> Line<'static> {
    let name = fit(name, width);
    let dots = width.saturating_sub(name.chars().count() + 1);
    Line::from(vec![name.into(), " ".into(), "·".repeat(dots).dim()])
}

/// Shortens `text` with an ellipsis to fit `width` columns.
fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let short: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{}…", short.trim_end())
}

fn gain_style(value: Option<f64>) -> Style {
    match value {
        Some(v) if v > 0.0 => Style::new().green(),
        Some(v) if v < 0.0 => Style::new().red(),
        _ => Style::new(),
    }
}

fn open_in_browser(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    // Its output would land on the screen.
    let _ = Command::new(program)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn row(name: &str, value: f64, price: f64, today: Option<f64>) -> model::Row {
        let mut returns = [None; TIMEFRAMES.len()];
        returns[0] = today;
        model::Row {
            name: name.to_string(),
            value: Some(value),
            price: Some(price),
            returns,
        }
    }

    #[test]
    fn cancelled_login_finishes_before_another_can_start() {
        let (requests, worker_requests) = mpsc::channel();
        let (_, replies) = mpsc::channel();
        let mut app = App::new(requests, replies);
        app.session = Session::LoggedOut;
        let login_key = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);

        app.on_key(login_key);
        let Request::Login(cancel) = worker_requests.try_recv().expect("login request") else {
            panic!("login request");
        };
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(cancel.load(Ordering::Relaxed));
        assert!(app.busy());

        app.on_key(login_key);
        assert!(worker_requests.try_recv().is_err(), "login is still busy");
        let hints = app.key_hints();
        let login_hint = hints
            .spans
            .iter()
            .find(|span| span.content == "l")
            .expect("login hint");
        assert!(
            login_hint
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::DIM)
        );

        // Replies sent before the worker saw the cancellation are ignored.
        app.handle(Reply::Login(LoginEvent::WaitingForSecondFactor));
        assert!(matches!(app.session, Session::LoggedOut));
        app.handle(Reply::LoginDone(Err("login cancelled".to_string())));
        assert!(!app.busy());
        assert_eq!(app.status, "Not logged in");

        app.on_key(login_key);
        let Request::Login(cancel) = worker_requests.try_recv().expect("new login request") else {
            panic!("login request");
        };
        assert!(!cancel.load(Ordering::Relaxed));
        app.handle(Reply::Login(LoginEvent::WaitingForSecondFactor));
        let Session::LoggingIn(login) = &app.session else {
            panic!("new login dialog");
        };
        assert_eq!(login.status, "Approve the login on your linked device.");

        app.handle(Reply::LoginDone(Ok(())));
        assert!(matches!(app.session, Session::LoggedIn(None)));
        assert!(matches!(worker_requests.try_recv(), Ok(Request::Refresh)));
    }

    #[test]
    fn logout_waits_for_the_current_request() {
        let (requests, worker_requests) = mpsc::channel();
        let (_, replies) = mpsc::channel();
        let mut app = App::new(requests, replies);
        app.refresh();
        assert!(matches!(worker_requests.try_recv(), Ok(Request::Refresh)));
        let logout_key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        app.on_key(logout_key);
        assert!(worker_requests.try_recv().is_err());

        app.handle(Reply::Error("try again".to_string()));
        app.on_key(logout_key);
        assert!(matches!(worker_requests.try_recv(), Ok(Request::Logout)));
    }

    #[test]
    fn draws_the_holdings_ranked_by_return() {
        let (requests, _) = mpsc::channel();
        let (_, replies) = mpsc::channel();
        let mut app = App::new(requests, replies);
        let mut changes = [None; TIMEFRAMES.len()];
        changes[0] = Some(1366.69);
        app.session = Session::LoggedIn(Some(Snapshot {
            total: Some(185638.95),
            changes,
            rows: vec![
                row("SAP", 12607.5, 252.15, Some(-0.0118)),
                row("iShares Core MSCI World", 52418.6, 108.74, None),
                row("NVIDIA", 23912.4, 158.22, Some(0.0284)),
            ],
        }));
        app.status = "Updated 12:08:36".to_string();

        let mut terminal = Terminal::new(TestBackend::new(80, 11)).expect("terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw");

        let screen = terminal.backend().to_string();
        let expected = [
            " Today │ Week │ Month │ Year to date │ Year │ Since buy",
            "",
            " 185.638,95 €",
            " +1.366,69 €  Today",
            " ┌────────────────────────────────────────────────────────────────────────────┐",
            " │ Security                                       Position    Return    Price │",
            " │ NVIDIA ··································   23.912,40 €   +2,84 %   158,22 │",
            " │ SAP ·····································   12.607,50 €   -1,18 %   252,15 │",
            " │ iShares Core MSCI World ·················   52.418,60 €         –   108,74 │",
            " └────────────────────────────────────────────────────────────────────────────┘",
            " r Refresh   l Log In   o Log Out   Tab Timeframe   q Quit     Updated 12:08:36",
        ];
        let lines: Vec<&str> = screen
            .lines()
            .map(|line| line.trim_matches('"').trim_end())
            .collect();
        assert_eq!(lines, expected);
    }
}
