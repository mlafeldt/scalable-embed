//! Helpers for typing the results of your own queries.

use serde::{Deserialize, Deserializer};

/// The API's timeframes for returns and performances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Timeframe {
    Intraday,
    TwoDays,
    OneWeek,
    OneMonth,
    ThreeMonths,
    SixMonths,
    OneYear,
    YearToDate,
    Max,
    SinceBuy,
    /// A timeframe this version of the fork doesn't know yet.
    #[serde(other)]
    Unknown,
}

/// Deserializes a JSON number, a decimal string (the API sends some decimals
/// as strings) or null. For your own types, e.g. for `Client::query_as`:
/// `#[serde(default, deserialize_with = "scalable_embed::decimal")]`.
pub fn decimal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(f64),
        Text(String),
    }
    match Option::<Raw>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Raw::Number(n)) => Ok(Some(n)),
        Some(Raw::Text(s)) => s.trim().parse().map(Some).map_err(serde::de::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[derive(Debug, PartialEq, Deserialize)]
    struct Return {
        timeframe: Timeframe,
        #[serde(default, deserialize_with = "decimal")]
        value: Option<f64>,
    }

    fn parse(value: serde_json::Value) -> Return {
        serde_json::from_value(value).expect("typed")
    }

    #[test]
    fn reads_decimals_as_numbers_strings_or_nothing() {
        let value = |json| parse(json).value;

        assert_eq!(
            value(json!({"timeframe": "MAX", "value": 12.34})),
            Some(12.34)
        );
        assert_eq!(
            value(json!({"timeframe": "MAX", "value": "-5.5"})),
            Some(-5.5)
        );
        assert_eq!(value(json!({"timeframe": "MAX", "value": null})), None);
        assert_eq!(value(json!({"timeframe": "MAX"})), None);
    }

    #[test]
    fn reads_timeframes_including_new_ones() {
        let timeframe = |name: &str| parse(json!({"timeframe": name})).timeframe;

        assert_eq!(timeframe("YEAR_TO_DATE"), Timeframe::YearToDate);
        assert_eq!(timeframe("SINCE_BUY"), Timeframe::SinceBuy);
        assert_eq!(timeframe("TEN_YEARS"), Timeframe::Unknown);
    }
}
