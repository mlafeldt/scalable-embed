//! Fetches the portfolio in one query and maps it to the screen model.

use scalable_embed::{Client, Timeframe, decimal};
use serde::Deserialize;
use serde_json::json;

use super::model::{PerTimeframe, Row, Snapshot, TIMEFRAMES, TimeframeChoice};

/// Everything the screen shows, in one request: upstream's overview and
/// holdings queries trimmed to the fields used here, plus each security's
/// performances, which upstream only fetches per security in `broker quote`.
/// Both per-timeframe lists alias their number to `value`, to share a type.
const SNAPSHOT_QUERY: &str = r#"
query ExampleSnapshot($accountId: ID!, $portfolioId: ID!) {
  account(id: $accountId) {
    brokerPortfolio(id: $portfolioId) {
      valuation(includeYearToDate: true) {
        valuation
        timeWeightedReturnByTimeframe {
          timeframe
          value: simpleAbsoluteReturn
        }
      }
      inventory {
        items {
          isin
          name
          portfolioIsinPerformance {
            valuation
          }
          quoteTick(source: CONSOLIDATED, includeYearToDate: true) {
            midPrice
            performancesByTimeframe {
              timeframe
              value: performance
            }
          }
        }
      }
    }
  }
}
"#;

#[derive(Deserialize)]
struct Data {
    account: Account,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    broker_portfolio: Portfolio,
}

#[derive(Deserialize)]
struct Portfolio {
    valuation: Valuation,
    inventory: Inventory,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Valuation {
    #[serde(default, deserialize_with = "decimal")]
    valuation: Option<f64>,
    time_weighted_return_by_timeframe: Option<Vec<TimedValue>>,
}

#[derive(Deserialize)]
struct TimedValue {
    timeframe: Timeframe,
    #[serde(default, deserialize_with = "decimal")]
    value: Option<f64>,
}

#[derive(Deserialize)]
struct Inventory {
    items: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    isin: String,
    name: Option<String>,
    portfolio_isin_performance: Option<ItemValuation>,
    quote_tick: Option<QuoteTick>,
}

#[derive(Deserialize)]
struct ItemValuation {
    #[serde(default, deserialize_with = "decimal")]
    valuation: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuoteTick {
    #[serde(default, deserialize_with = "decimal")]
    mid_price: Option<f64>,
    performances_by_timeframe: Option<Vec<TimedValue>>,
}

pub fn fetch(client: &mut Client) -> scalable_embed::Result<Snapshot> {
    let ids = client.broker_ids(None)?;
    let variables = json!({"accountId": ids.account_id, "portfolioId": ids.portfolio_id});
    let data: Data = client.query_as("ExampleSnapshot", SNAPSHOT_QUERY, variables)?;
    Ok(Snapshot::from(data))
}

impl From<Data> for Snapshot {
    fn from(data: Data) -> Self {
        let Portfolio {
            valuation,
            inventory,
        } = data.account.broker_portfolio;

        let returns = valuation
            .time_weighted_return_by_timeframe
            .unwrap_or_default();
        let rows = inventory
            .items
            .into_iter()
            .map(|item| {
                let quote = item.quote_tick;
                let performances = quote
                    .as_ref()
                    .and_then(|q| q.performances_by_timeframe.as_deref())
                    .unwrap_or_default();
                Row {
                    name: item.name.unwrap_or(item.isin),
                    value: item.portfolio_isin_performance.and_then(|p| p.valuation),
                    price: quote.as_ref().and_then(|q| q.mid_price),
                    returns: per_timeframe(performances, |choice| choice.holdings),
                }
            })
            .collect();
        Snapshot {
            total: valuation.valuation,
            changes: per_timeframe(&returns, |choice| choice.portfolio),
            rows,
        }
    }
}

/// The value for each of `TIMEFRAMES`, by the API timeframe `pick` gives it.
fn per_timeframe(values: &[TimedValue], pick: fn(TimeframeChoice) -> Timeframe) -> PerTimeframe {
    TIMEFRAMES.map(pick).map(|timeframe| {
        values
            .iter()
            .find(|v| v.timeframe == timeframe)
            .and_then(|v| v.value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_snapshot_query_response() {
        let data: Data = serde_json::from_value(json!({"account": {"brokerPortfolio": {
            "valuation": {
                "valuation": "1234.56",
                "timeWeightedReturnByTimeframe": [
                    {"timeframe": "INTRADAY", "value": 12.5},
                    {"timeframe": "MAX", "value": "300"}
                ]
            },
            "inventory": {"items": [
                {
                    "isin": "IE00B4L5Y983",
                    "name": "iShares Core MSCI World",
                    "portfolioIsinPerformance": {"valuation": 1000.0},
                    "quoteTick": {"midPrice": "98.76", "performancesByTimeframe": [
                        {"timeframe": "SINCE_BUY", "value": 0.25},
                        {"timeframe": "TEN_YEARS", "value": 1.5}
                    ]}
                },
                {"isin": "DE0001", "name": null, "portfolioIsinPerformance": null, "quoteTick": null}
            ]}
        }}}))
        .expect("response");

        let snapshot = Snapshot::from(data);

        let since_buy = TIMEFRAMES.len() - 1;
        assert_eq!(snapshot.total, Some(1234.56));
        assert_eq!(snapshot.changes[0], Some(12.5));
        assert_eq!(snapshot.changes[since_buy], Some(300.0));
        let [held, bare] = &snapshot.rows[..] else {
            panic!("two rows");
        };
        assert_eq!(held.price, Some(98.76));
        assert_eq!(held.returns[since_buy], Some(0.25));
        assert_eq!(held.returns[0], None);
        assert_eq!(bare.name, "DE0001");
        assert_eq!((bare.value, bare.price), (None, None));
    }
}
