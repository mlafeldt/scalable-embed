//! Login dialog: the device code, the login's progress, and buttons to open
//! the browser or cancel.

use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;

use saudade::{Button, Container, FontFamily, FontStyle, Painter, Rect, Theme, Widget};

use crate::format::{bold, bold_width};
use crate::{Session, State};

pub const LOGIN_W: i32 = 340;
pub const LOGIN_H: i32 = 170;

pub fn login_dialog(state: &Rc<RefCell<State>>) -> Container {
    let (bw, bh) = (110, 24);
    let open = Button::new(
        Rect::new(LOGIN_W - 2 * bw - 24, LOGIN_H - bh - 12, bw, bh),
        "Open Browser",
    )
    .default(true)
    .on_click({
        let state = state.clone();
        move |_| {
            if let Session::LoggingIn(login) = &state.borrow().session
                && let Some(prompt) = &login.prompt
            {
                open_in_browser(prompt.url());
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
        let Session::LoggingIn(login) = &state.session else {
            return;
        };
        let size = theme.font_size;
        let line = p.measure_text("", size).h;
        let r = self.rect.inset(16);
        let mut y = r.y;
        if let Some(prompt) = &login.prompt {
            let big = size * 2.0;
            let code = &prompt.user_code;
            let w = bold_width(p, code, big);
            bold(p, r.x + (r.w - w) / 2, y, code, big, theme.text);
            y += p
                .measure_text_styled("", big, FontFamily::Sans, FontStyle::Bold)
                .h
                + 12;
        }
        for text in p.wrap_text(login.status, size, r.w) {
            p.text(r.x, y, text, size, theme.text);
            y += line;
        }
    }
}

fn open_in_browser(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(program).arg(url).spawn();
}
