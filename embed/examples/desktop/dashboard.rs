//! Main view: total, its change and the Refresh button on top, holdings ranked
//! by return below, status at the bottom.

use std::cell::RefCell;
use std::rc::Rc;

use saudade::{Button, Event, EventCtx, FontFamily, Painter, Rect, Theme, Widget};

use crate::format::{
    RaisedCents, big_amount, bold, bold_width, eur, field, fit, gain_color, signed_eur, signed_pct,
};
use crate::worker::Row;
use crate::{Session, State, TIMEFRAMES};

const PAD: i32 = 12;
const GAP: i32 = 16;

pub struct Dashboard {
    rect: Rect,
    state: Rc<RefCell<State>>,
    refresh: Button,
    /// First visible holdings row.
    scroll: f32,
    /// Set while painting, as only the painter can measure text.
    max_scroll: usize,
}

impl Dashboard {
    pub fn new(state: Rc<RefCell<State>>, refresh: Button) -> Self {
        Self {
            rect: Rect::new(0, 0, 0, 0),
            state,
            refresh,
            scroll: 0.0,
            max_scroll: 0,
        }
    }

    /// Total, change and Refresh button. Returns where the list starts.
    fn paint_header(&mut self, p: &mut Painter, theme: &Theme, state: &State) -> i32 {
        let r = self.rect;
        let size = theme.font_size;
        let line = p.measure_text("", size).h;
        let x = r.x + PAD;
        let snapshot = state.snapshot();

        let top = r.y + PAD;
        let mut y = top;
        y += big_amount(
            p,
            x,
            y,
            snapshot.and_then(|s| s.total),
            size * 2.6,
            theme.text,
        );
        let change = snapshot.and_then(|s| s.changes[state.timeframe]);
        let change_text = signed_eur(change);
        p.text(x, y, &change_text, size, gain_color(theme, change));
        let w = p.measure_text(&change_text, size).w;
        let label = TIMEFRAMES[state.timeframe].label;
        p.text(x + w + 6, y, label, size, theme.disabled_text);
        y += line;

        let button_w = p
            .measure_text_styled("Refresh", size, FontFamily::Sans, theme.button_style)
            .w
            + 24;
        let button_h = line + 10;
        let button_y = top + (y - top - button_h) / 2;
        let button_x = r.right() - PAD - button_w;
        self.refresh
            .layout(Rect::new(button_x, button_y, button_w, button_h));
        self.refresh.set_enabled(!state.busy());
        self.refresh.paint(p, theme);
        y + PAD
    }

    /// Holdings, or why there are none.
    fn paint_list(&mut self, p: &mut Painter, theme: &Theme, list: Rect, state: &State) {
        field(p, list, theme);
        let inner = list.inset(2);
        let saved = p.push_clip(inner);
        let placeholder = match &state.session {
            Session::LoggedIn(Some(snapshot)) if !snapshot.rows.is_empty() => {
                let rows = snapshot.ranked(state.timeframe);
                self.paint_rows(p, theme, inner, &rows, state.timeframe);
                None
            }
            Session::LoggedIn(Some(_)) => Some("No holdings"),
            Session::LoggedIn(None) if state.busy() => Some("Loading..."),
            // The status bar says what went wrong.
            Session::LoggedIn(None) => Some("No data"),
            Session::LoggedOut | Session::LoggingIn(_) => Some("Not logged in"),
        };
        if let Some(text) = placeholder {
            p.text_centered(inner, text, theme.font_size, theme.disabled_text);
        }
        p.restore_clip(saved);
    }

    fn paint_rows(
        &mut self,
        p: &mut Painter,
        theme: &Theme,
        inner: Rect,
        rows: &[&Row],
        timeframe: usize,
    ) {
        let size = theme.font_size;
        let cells: Vec<Cells> = rows.iter().map(|row| Cells::new(row, timeframe)).collect();
        let columns = Columns::new(p, inner, size, &cells);

        let row_h = p.measure_text("", size).h + 6;
        let mut y = inner.y + 3;
        bold(p, columns.name_x, y, "Security", size, theme.text);
        for (header, right) in Columns::HEADERS.iter().zip(columns.rights) {
            bold(
                p,
                right - bold_width(p, header, size),
                y,
                header,
                size,
                theme.text,
            );
        }
        y += row_h;
        p.h_line(inner.x, y - 3, inner.w, theme.shadow);

        let visible = ((inner.bottom() - y) / row_h).max(1) as usize;
        self.max_scroll = rows.len().saturating_sub(visible);
        let first = (self.scroll as usize).min(self.max_scroll);
        for (row, cells) in rows.iter().zip(&cells).skip(first) {
            if y + row_h > inner.bottom() {
                break;
            }
            let name = fit(p, &row.name, size, columns.name_w);
            p.text(columns.name_x, y, &name, size, theme.text);
            let [value_right, return_right, price_right] = columns.rights;
            let w = p.measure_text(&cells.value, size).w;
            p.text(value_right - w, y, &cells.value, size, theme.text);
            let w = p.measure_text(&cells.ret, size).w;
            let color = gain_color(theme, row.returns[timeframe]);
            p.text(return_right - w, y, &cells.ret, size, color);
            cells.price.paint(p, price_right, y, size, theme.text);
            y += row_h;
        }
    }
}

/// A row's formatted Position, Return and Price.
struct Cells {
    value: String,
    ret: String,
    price: RaisedCents,
}

impl Cells {
    fn new(row: &Row, timeframe: usize) -> Self {
        Self {
            value: eur(row.value),
            ret: signed_pct(row.returns[timeframe].map(|r| r * 100.0)),
            price: RaisedCents::new(row.price),
        }
    }
}

/// Where the list's columns go.
struct Columns {
    name_x: i32,
    name_w: i32,
    /// Right edges of the number columns, in `HEADERS` order.
    rights: [i32; 3],
}

impl Columns {
    const HEADERS: [&str; 3] = ["Position", "Return", "Price"];

    fn new(p: &Painter, inner: Rect, size: f32, cells: &[Cells]) -> Self {
        let mut widths = Self::HEADERS.map(|h| bold_width(p, h, size));
        for cells in cells {
            widths[0] = widths[0].max(p.measure_text(&cells.value, size).w);
            widths[1] = widths[1].max(p.measure_text(&cells.ret, size).w);
            widths[2] = widths[2].max(cells.price.width(p, size));
        }
        // The number columns spread evenly over the right half, so they keep
        // apart from each other instead of leaving all the slack after the
        // names.
        let gap = ((inner.w / 2 - widths.iter().sum::<i32>()) / 3).clamp(GAP, 6 * GAP);
        let mut rights = [0; 3];
        let mut edge = inner.right() - 8;
        for i in (0..3).rev() {
            rights[i] = edge;
            edge -= widths[i] + gap;
        }
        let name_x = inner.x + 8;
        Self {
            name_x,
            name_w: edge - name_x,
            rights,
        }
    }
}

fn paint_status(p: &mut Painter, theme: &Theme, r: Rect, status: &str) -> Rect {
    let size = theme.font_size;
    let status_h = p.measure_text("", size).h + 8;
    let rect = Rect::new(
        r.x + PAD,
        r.bottom() - PAD - status_h,
        r.w - 2 * PAD,
        status_h,
    );
    field(p, rect, theme);
    let saved = p.push_clip(rect.inset(2));
    p.text(rect.x + 6, rect.y + 4, status, size, theme.text);
    p.restore_clip(saved);
    rect
}

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
        let state = Rc::clone(&self.state);
        let state = state.borrow();
        let r = self.rect;
        p.fill_rect(r, theme.background);
        let list_y = self.paint_header(p, theme, &state);
        let status = paint_status(p, theme, r, &state.status);
        let list = Rect::new(
            r.x + PAD,
            list_y,
            r.w - 2 * PAD,
            status.y - PAD / 2 - list_y,
        );
        self.paint_list(p, theme, list, &state);
    }
}
