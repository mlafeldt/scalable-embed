//! Typed views of `sc --json` command results.
//!
//! These mirror the CLI's JSON output, which upstream aims to keep stable but
//! does not guarantee. Following upstream's advice, the structs ignore unknown
//! fields and treat values as optional where the CLI may emit null. The tests
//! feed upstream's real projection code, so a shape change fails `cargo test`
//! after an upstream sync.

use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BrokerResult<T> {
    pub account_id: String,
    pub portfolio_id: String,
    pub resolution: Resolution,
    pub result: T,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Resolution {
    /// `auto_session_person_id` or `selected_context`.
    pub account: String,
    /// `explicit`, `selected_context` or `auto_resolved`.
    pub portfolio: String,
}

/// `sc broker overview`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Overview {
    pub valuation: Valuation,
    pub timestamps: OverviewTimestamps,
    pub performance: Vec<TimeframeReturn>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Valuation {
    #[serde(default, deserialize_with = "number")]
    pub total: Option<f64>,
    #[serde(default, deserialize_with = "number")]
    pub securities: Option<f64>,
    #[serde(default, deserialize_with = "number")]
    pub crypto: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OverviewTimestamps {
    pub valuation_timestamp_utc: Option<String>,
    pub inventory_timestamp_utc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TimeframeReturn {
    pub timeframe: Timeframe,
    #[serde(default, rename = "simpleAbsoluteReturn", deserialize_with = "number")]
    pub simple_absolute_return: Option<f64>,
}

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

/// Accepts JSON numbers and decimal strings.
fn number<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f64>, D::Error> {
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
    use serde_json::{Value, json};

    use super::*;
    use crate::broker_projections::project_broker_overview_response;
    use crate::broker_queries::BrokerInput;
    use crate::broker_shared::{ResolvedBrokerIds, broker_result_envelope};

    /// Runs a GraphQL response through upstream's projection and envelope,
    /// exactly as `sc broker overview --json` does.
    fn overview_output(graphql_response: Value) -> Value {
        let input = BrokerInput::new("acc-1", "port-1", true).expect("input");
        let projected =
            project_broker_overview_response(&input, &graphql_response).expect("projection");
        let ids = ResolvedBrokerIds {
            account_id: "acc-1".to_string(),
            portfolio_id: "port-1".to_string(),
            account_source: "selected_context",
            portfolio_source: "explicit",
        };
        broker_result_envelope(&ids, projected)
    }

    #[test]
    fn overview_types_match_upstream_projection() {
        let output = overview_output(json!({"account": {"brokerPortfolio": {"valuation": {
            "valuation": 1234.56,
            "securitiesValuation": "1200.00",
            "cryptoValuation": 34.56,
            "timestampUtc": {"time": "2026-03-04T10:15:30Z"},
            "lastInventoryUpdateTimestampUtc": {"time": "2026-03-04T09:00:00Z"},
            "timeWeightedReturnByTimeframe": [
                {"timeframe": "INTRADAY", "simpleAbsoluteReturn": 12.34},
                {"timeframe": "YEAR_TO_DATE", "simpleAbsoluteReturn": "-5.5"}
            ]
        }}}}));

        let parsed: BrokerResult<Overview> = serde_json::from_value(output).expect("typed");

        assert_eq!(
            parsed,
            BrokerResult {
                account_id: "acc-1".to_string(),
                portfolio_id: "port-1".to_string(),
                resolution: Resolution {
                    account: "selected_context".to_string(),
                    portfolio: "explicit".to_string(),
                },
                result: Overview {
                    valuation: Valuation {
                        total: Some(1234.56),
                        securities: Some(1200.0),
                        crypto: Some(34.56),
                    },
                    timestamps: OverviewTimestamps {
                        valuation_timestamp_utc: Some("2026-03-04T10:15:30Z".to_string()),
                        inventory_timestamp_utc: Some("2026-03-04T09:00:00Z".to_string()),
                    },
                    performance: vec![
                        TimeframeReturn {
                            timeframe: Timeframe::Intraday,
                            simple_absolute_return: Some(12.34),
                        },
                        TimeframeReturn {
                            timeframe: Timeframe::YearToDate,
                            simple_absolute_return: Some(-5.5),
                        },
                    ],
                },
            }
        );
    }

    #[test]
    fn overview_tolerates_missing_values_and_new_timeframes() {
        let output = overview_output(json!({"account": {"brokerPortfolio": {"valuation": {
            "timeWeightedReturnByTimeframe": [{"timeframe": "TEN_YEARS"}]
        }}}}));

        let parsed: BrokerResult<Overview> = serde_json::from_value(output).expect("typed");

        let overview = parsed.result;
        assert_eq!(overview.valuation.total, None);
        assert_eq!(overview.timestamps.valuation_timestamp_utc, None);
        assert_eq!(overview.performance[0].timeframe, Timeframe::Unknown);
        assert_eq!(overview.performance[0].simple_absolute_return, None);
    }
}
