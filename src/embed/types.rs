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

/// Where the command got its account and portfolio IDs from.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Resolution {
    pub account: IdSource,
    pub portfolio: IdSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdSource {
    /// Passed to the command (portfolios only).
    Explicit,
    /// Saved at login or with `sc broker context`.
    SelectedContext,
    /// The logged-in person's ID (accounts only).
    AutoSessionPersonId,
    /// Looked up from the API, as nothing was saved.
    AutoResolve,
    /// A source this version of the fork doesn't know yet.
    #[serde(other)]
    Unknown,
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
    #[serde(default, deserialize_with = "decimal")]
    pub total: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub securities: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
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
    #[serde(default, rename = "simpleAbsoluteReturn", deserialize_with = "decimal")]
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

/// `sc broker holdings`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Holdings {
    pub requested_quote_source: Option<String>,
    pub count: Option<u64>,
    pub security_count: Option<u64>,
    pub crypto_count: Option<u64>,
    pub items: Vec<SecurityHolding>,
    pub crypto_items: Vec<CryptoHolding>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SecurityHolding {
    pub kind: String,
    pub isin: Option<String>,
    pub name: Option<String>,
    pub security_type: Option<String>,
    #[serde(default, deserialize_with = "decimal")]
    pub quantity: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub pending_quantity: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub blocked_quantity: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub fifo_price: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub valuation: Option<f64>,
    pub valuation_currency: Option<String>,
    pub quote_venue: Option<String>,
    #[serde(default, deserialize_with = "decimal")]
    pub quote_mid_price: Option<f64>,
    pub quote_currency: Option<String>,
    pub quote_timestamp_utc: Option<String>,
    pub quote_is_outdated: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CryptoHolding {
    pub kind: String,
    pub ticker: Option<String>,
    pub name: Option<String>,
    pub position: CryptoPosition,
    pub etp_positions: Vec<EtpPosition>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CryptoPosition {
    #[serde(default, deserialize_with = "decimal")]
    pub filled: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub sellable: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub pending: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EtpPosition {
    pub isin: Option<String>,
    pub name: Option<String>,
    pub position: Option<Position>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Position {
    #[serde(default, deserialize_with = "decimal")]
    pub blocked: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub filled: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub pending: Option<f64>,
}

/// `sc broker quote`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Quote {
    pub requested_quote_source: Option<String>,
    pub security_id: Option<String>,
    pub isin: String,
    pub name: Option<String>,
    pub security_type: Option<String>,
    pub quote_tick_id: Option<String>,
    pub quote_venue: Option<String>,
    #[serde(default, deserialize_with = "decimal")]
    pub quote_mid_price: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub quote_bid_price: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub quote_ask_price: Option<f64>,
    pub quote_currency: Option<String>,
    pub quote_timestamp_utc: Option<String>,
    pub quote_is_outdated: Option<bool>,
    pub quote_performance_date: Option<String>,
    pub quote_performances: Vec<QuotePerformance>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct QuotePerformance {
    pub timeframe: Timeframe,
    #[serde(default, deserialize_with = "decimal")]
    pub performance: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub simple_absolute_return: Option<f64>,
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
    use serde_json::{Value, json};

    use super::*;
    use crate::broker_projections::{
        project_broker_holdings_response, project_broker_overview_response,
        project_broker_quote_response,
    };
    use crate::broker_queries::BrokerInput;
    use crate::broker_shared::{ResolvedBrokerIds, broker_result_envelope};

    /// Runs a GraphQL response through upstream's projection and envelope,
    /// exactly as `sc broker <command> --json` does.
    fn output(
        project: fn(&BrokerInput, &Value) -> anyhow::Result<Value>,
        graphql_response: Value,
    ) -> Value {
        let input = BrokerInput::new("acc-1", "port-1", true).expect("input");
        let projected = project(&input, &graphql_response).expect("projection");
        let ids = ResolvedBrokerIds {
            account_id: "acc-1".to_string(),
            portfolio_id: "port-1".to_string(),
            account_source: "selected_context",
            portfolio_source: "explicit",
        };
        broker_result_envelope(&ids, projected)
    }

    fn overview_output(graphql_response: Value) -> Value {
        output(project_broker_overview_response, graphql_response)
    }

    fn holdings_output(graphql_response: Value) -> Value {
        output(project_broker_holdings_response, graphql_response)
    }

    fn quote_output(graphql_response: Value) -> Value {
        output(
            |input, response| project_broker_quote_response(input, "IE00B4L5Y983", response),
            graphql_response,
        )
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
                    account: IdSource::SelectedContext,
                    portfolio: IdSource::Explicit,
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

    #[test]
    fn holdings_types_match_upstream_projection() {
        let output = holdings_output(json!({"account": {"brokerPortfolio": {
            "inventory": {"items": [{
                "isin": "IE00B4L5Y983",
                "name": "iShares Core MSCI World",
                "type": "ETF",
                "inventory": {"position": {
                    "filled": 12.5, "pending": 0, "blocked": "0", "fifoPrice": "80.10"
                }},
                "portfolioIsinPerformance": {"valuation": 1234.56, "currency": "EUR"},
                "quoteTick": {
                    "venue": "MUNC",
                    "midPrice": 98.76,
                    "currency": "EUR",
                    "timestampUtc": {"time": "2026-03-04T10:15:30Z"},
                    "isOutdated": false
                }
            }]},
            "crypto": {"coins": [{
                "ticker": "BTC",
                "name": "Bitcoin",
                "position": {"filled": 0.25, "sellable": 0.2, "pending": 0.05},
                "inventory": [
                    {"isin": "DE000A3GPMN9", "etpSecurity": {"name": "Bitcoin ETP", "inventory": null}},
                    {"isin": "CH0454664001", "etpSecurity": {"name": "Bitcoin ETP A", "inventory": {
                        "position": {"blocked": 0, "filled": 1.5, "pending": 0.25}
                    }}}
                ]
            }]}
        }}}));

        let parsed: BrokerResult<Holdings> = serde_json::from_value(output).expect("typed");

        assert_eq!(
            parsed.result,
            Holdings {
                requested_quote_source: Some("CONSOLIDATED".to_string()),
                count: Some(1),
                security_count: Some(1),
                crypto_count: Some(1),
                items: vec![SecurityHolding {
                    kind: "security".to_string(),
                    isin: Some("IE00B4L5Y983".to_string()),
                    name: Some("iShares Core MSCI World".to_string()),
                    security_type: Some("ETF".to_string()),
                    quantity: Some(12.5),
                    pending_quantity: Some(0.0),
                    blocked_quantity: Some(0.0),
                    fifo_price: Some(80.10),
                    valuation: Some(1234.56),
                    valuation_currency: Some("EUR".to_string()),
                    quote_venue: Some("MUNC".to_string()),
                    quote_mid_price: Some(98.76),
                    quote_currency: Some("EUR".to_string()),
                    quote_timestamp_utc: Some("2026-03-04T10:15:30Z".to_string()),
                    quote_is_outdated: Some(false),
                }],
                crypto_items: vec![CryptoHolding {
                    kind: "crypto".to_string(),
                    ticker: Some("BTC".to_string()),
                    name: Some("Bitcoin".to_string()),
                    position: CryptoPosition {
                        filled: Some(0.25),
                        sellable: Some(0.2),
                        pending: Some(0.05),
                    },
                    etp_positions: vec![
                        EtpPosition {
                            isin: Some("CH0454664001".to_string()),
                            name: Some("Bitcoin ETP A".to_string()),
                            position: Some(Position {
                                blocked: Some(0.0),
                                filled: Some(1.5),
                                pending: Some(0.25),
                            }),
                        },
                        EtpPosition {
                            isin: Some("DE000A3GPMN9".to_string()),
                            name: Some("Bitcoin ETP".to_string()),
                            position: None,
                        },
                    ],
                }],
            }
        );
    }

    #[test]
    fn holdings_tolerate_missing_values() {
        let output = holdings_output(json!({"account": {"brokerPortfolio": {
            "inventory": {"items": [{"isin": "A", "inventory": {}, "quoteTick": {}}]}
        }}}));

        let parsed: BrokerResult<Holdings> = serde_json::from_value(output).expect("typed");

        let holding = &parsed.result.items[0];
        assert_eq!(holding.name, None);
        assert_eq!(holding.quantity, None);
        assert_eq!(holding.valuation, None);
        assert_eq!(holding.quote_timestamp_utc, None);
        assert!(parsed.result.crypto_items.is_empty());
    }

    #[test]
    fn quote_types_match_upstream_projection() {
        let output = quote_output(json!({"account": {"brokerPortfolio": {"security": {
            "id": "security-1",
            "isin": "IE00B4L5Y983",
            "name": "iShares Core MSCI World",
            "type": "ETF",
            "quoteTick": {
                "venue": "MUNC",
                "id": "tick-1",
                "midPrice": 98.76,
                "bidPrice": "98.70",
                "askPrice": 98.82,
                "currency": "EUR",
                "timestampUtc": {"time": "2026-03-04T10:15:30Z"},
                "isOutdated": false,
                "performanceDate": {"date": "2026-03-04"},
                "performancesByTimeframe": [
                    {"timeframe": "SINCE_BUY", "performance": 0.2331, "simpleAbsoluteReturn": 18.66},
                    {"timeframe": "INTRADAY", "performance": "-0.0045", "simpleAbsoluteReturn": -0.45}
                ]
            }
        }}}}));

        let parsed: BrokerResult<Quote> = serde_json::from_value(output).expect("typed");

        assert_eq!(
            parsed.result,
            Quote {
                requested_quote_source: Some("CONSOLIDATED".to_string()),
                security_id: Some("security-1".to_string()),
                isin: "IE00B4L5Y983".to_string(),
                name: Some("iShares Core MSCI World".to_string()),
                security_type: Some("ETF".to_string()),
                quote_tick_id: Some("tick-1".to_string()),
                quote_venue: Some("MUNC".to_string()),
                quote_mid_price: Some(98.76),
                quote_bid_price: Some(98.70),
                quote_ask_price: Some(98.82),
                quote_currency: Some("EUR".to_string()),
                quote_timestamp_utc: Some("2026-03-04T10:15:30Z".to_string()),
                quote_is_outdated: Some(false),
                quote_performance_date: Some("2026-03-04".to_string()),
                quote_performances: vec![
                    QuotePerformance {
                        timeframe: Timeframe::Intraday,
                        performance: Some(-0.0045),
                        simple_absolute_return: Some(-0.45),
                    },
                    QuotePerformance {
                        timeframe: Timeframe::SinceBuy,
                        performance: Some(0.2331),
                        simple_absolute_return: Some(18.66),
                    },
                ],
            }
        );
    }

    #[test]
    fn quote_tolerates_missing_values() {
        let output = quote_output(json!({"account": {"brokerPortfolio": {"security": {
            "isin": "IE00B4L5Y983",
            "quoteTick": {"performancesByTimeframe": [{"timeframe": "TEN_YEARS"}]}
        }}}}));

        let parsed: BrokerResult<Quote> = serde_json::from_value(output).expect("typed");

        let quote = parsed.result;
        assert_eq!(quote.name, None);
        assert_eq!(quote.quote_mid_price, None);
        assert_eq!(quote.quote_performance_date, None);
        assert_eq!(quote.quote_performances[0].timeframe, Timeframe::Unknown);
        assert_eq!(quote.quote_performances[0].performance, None);
    }
}
