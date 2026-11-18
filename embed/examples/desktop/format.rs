//! Drawing and formatting helpers.

use saudade::{Color, FontFamily, FontStyle, Painter, Rect, Theme};

pub fn bold(p: &mut Painter, x: i32, y: i32, text: &str, size: f32, color: Color) {
    p.text_styled(x, y, text, size, color, FontFamily::Sans, FontStyle::Bold);
}

pub fn bold_width(p: &Painter, text: &str, size: f32) -> i32 {
    p.measure_text_styled(text, size, FontFamily::Sans, FontStyle::Bold)
        .w
}

/// A sunken white field, like a Windows 3.1 list box.
pub fn field(p: &mut Painter, rect: Rect, theme: &Theme) {
    p.fill_rect(rect, theme.background);
    p.sunken_bevel(rect, theme.highlight, theme.shadow);
    p.stroke_rect(rect, theme.border);
}

/// A big amount the way the Scalable app shows it: euros, then small cents
/// with the currency below them. Returns the height drawn.
pub fn big_amount(
    p: &mut Painter,
    x: i32,
    y: i32,
    value: Option<f64>,
    size: f32,
    color: Color,
) -> i32 {
    let height = p
        .measure_text_styled("", size, FontFamily::Sans, FontStyle::Bold)
        .h;
    let Some(value) = value else {
        bold(p, x, y, "–", size, color);
        return height;
    };
    let (euros, cents) = split_cents(value);
    bold(p, x, y, &euros, size, color);
    let w = bold_width(p, &euros, size);
    let small = size * 0.4;
    let small_h = p.measure_text("", small).h;
    let inset = height / 8;
    p.text(x + w + 3, y + inset, &cents, small, color);
    p.text(x + w + 3, y + height - small_h - inset, "€", small, color);
    height
}

/// An amount with small raised cents and no currency, as the Scalable app
/// shows prices. Measures and draws from the same layout.
pub struct RaisedCents {
    euros: String,
    cents: String,
}

impl RaisedCents {
    const CENTS_SCALE: f32 = 0.7;
    const GAP: i32 = 1;

    pub fn new(value: Option<f64>) -> Self {
        let (euros, cents) = value.map_or(("–".to_string(), String::new()), split_cents);
        Self { euros, cents }
    }

    pub fn width(&self, p: &Painter, size: f32) -> i32 {
        p.measure_text(&self.euros, size).w + self.cents_width(p, size)
    }

    /// Draws it right-aligned at `right`.
    pub fn paint(&self, p: &mut Painter, right: i32, y: i32, size: f32, color: Color) {
        let euros_right = right - self.cents_width(p, size);
        if !self.cents.is_empty() {
            let small = size * Self::CENTS_SCALE;
            p.text(euros_right + Self::GAP, y - 1, &self.cents, small, color);
        }
        let w = p.measure_text(&self.euros, size).w;
        p.text(euros_right - w, y, &self.euros, size, color);
    }

    fn cents_width(&self, p: &Painter, size: f32) -> i32 {
        if self.cents.is_empty() {
            return 0;
        }
        Self::GAP + p.measure_text(&self.cents, size * Self::CENTS_SCALE).w
    }
}

/// Shortens `text` with an ellipsis to fit `max_w`.
pub fn fit(p: &Painter, text: &str, size: f32, max_w: i32) -> String {
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

pub fn gain_color(theme: &Theme, value: Option<f64>) -> Color {
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

/// `value` rounded the way it's shown, so its sign matches the digits.
fn rounded(value: f64, decimals: usize) -> f64 {
    format!("{value:.decimals$}").parse().unwrap_or(value)
}

/// German format: `1.234,56`.
fn number(value: f64, decimals: usize) -> String {
    let value = rounded(value, decimals);
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
    if value < 0.0 { format!("-{out}") } else { out }
}

fn signed(value: f64, decimals: usize) -> String {
    let text = number(value, decimals);
    if rounded(value, decimals) > 0.0 {
        format!("+{text}")
    } else {
        text
    }
}

/// Euros and cents in German format: `("1.234", "56")`.
fn split_cents(value: f64) -> (String, String) {
    let text = number(value, 2);
    let (euros, cents) = text.split_once(',').unwrap_or((&text, ""));
    (euros.to_string(), cents.to_string())
}

pub fn eur(value: Option<f64>) -> String {
    value.map_or("–".to_string(), |v| format!("{} €", number(v, 2)))
}

pub fn signed_eur(value: Option<f64>) -> String {
    value.map_or("–".to_string(), |v| format!("{} €", signed(v, 2)))
}

pub fn signed_pct(value: Option<f64>) -> String {
    value.map_or("–".to_string(), |v| format!("{} %", signed(v, 2)))
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
        assert_eq!(signed_pct(Some(0.004)), "0,00 %");
        assert_eq!(
            split_cents(-1234.5),
            ("-1.234".to_string(), "50".to_string())
        );
        assert_eq!(eur(None), "–");
    }
}
