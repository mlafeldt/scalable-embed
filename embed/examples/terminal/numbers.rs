//! Number formatting, with two decimals and a dash for missing values.

/// `value` rounded the way it's shown, so its sign matches the digits.
fn rounded(value: f64) -> f64 {
    format!("{value:.2}").parse().unwrap_or(value)
}

/// German format: `1.234,56`.
fn number(value: f64) -> String {
    let value = rounded(value);
    let fixed = format!("{:.2}", value.abs());
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

fn signed(value: f64) -> String {
    let text = number(value);
    if rounded(value) > 0.0 {
        format!("+{text}")
    } else {
        text
    }
}

fn or_dash(value: Option<f64>, format: impl FnOnce(f64) -> String) -> String {
    value.map_or_else(|| "–".to_string(), format)
}

pub fn plain(value: Option<f64>) -> String {
    or_dash(value, number)
}

pub fn eur(value: Option<f64>) -> String {
    or_dash(value, |v| format!("{} €", number(v)))
}

pub fn signed_eur(value: Option<f64>) -> String {
    or_dash(value, |v| format!("{} €", signed(v)))
}

pub fn signed_pct(value: Option<f64>) -> String {
    or_dash(value, |v| format!("{} %", signed(v)))
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
        assert_eq!(plain(Some(1234.5)), "1.234,50");
        assert_eq!(eur(None), "–");
    }
}
