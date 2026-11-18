//! Number formatting.

/// `value` rounded the way it's shown, so its sign matches the digits.
fn rounded(value: f64, decimals: usize) -> f64 {
    format!("{value:.decimals$}").parse().unwrap_or(value)
}

/// German format: `1.234,56`.
pub fn number(value: f64, decimals: usize) -> String {
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
        assert_eq!(eur(None), "–");
    }
}
