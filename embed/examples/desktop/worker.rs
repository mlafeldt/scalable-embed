//! Worker thread: owns the client and answers one request at a time.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::Context;
use scalable_embed::{Client, LoginEvent, Options, SessionBackendPreference, Timeframe, decimal};
use serde::Deserialize;
use serde_json::json;

use crate::TIMEFRAMES;

pub enum Request {
    Refresh,
    /// `cancel` cancels this login only.
    Login {
        id: u64,
        cancel: Arc<AtomicBool>,
    },
    Logout,
}

pub enum Reply {
    Data(Snapshot),
    NeedsLogin,
    Error(String),
    /// Progress of the login with this `id`.
    Login {
        id: u64,
        event: LoginEvent,
    },
    LoginDone {
        id: u64,
        result: Result<(), String>,
    },
    LoggedOut(Result<(), String>),
}

impl Reply {
    /// Whether this reply ends a request, as opposed to reporting progress.
    pub fn is_final(&self) -> bool {
        !matches!(self, Reply::Login { .. })
    }
}

/// One value per entry in `TIMEFRAMES`.
pub type PerTimeframe = [Option<f64>; TIMEFRAMES.len()];

pub struct Snapshot {
    pub total: Option<f64>,
    /// The portfolio's change in €.
    pub changes: PerTimeframe,
    pub rows: Vec<Row>,
}

pub struct Row {
    pub name: String,
    pub value: Option<f64>,
    pub price: Option<f64>,
    /// Price performance, as a fraction (0.05 is +5 %).
    pub returns: PerTimeframe,
}

impl Snapshot {
    /// The rows by their return over `TIMEFRAMES[timeframe]`, best first.
    /// Missing returns go last.
    pub fn ranked(&self, timeframe: usize) -> Vec<&Row> {
        let mut rows: Vec<&Row> = self.rows.iter().collect();
        rows.sort_by(|a, b| match (a.returns[timeframe], b.returns[timeframe]) {
            (Some(a), Some(b)) => b.total_cmp(&a),
            (a, b) => b.is_some().cmp(&a.is_some()),
        });
        rows
    }
}

pub fn spawn(file_session: bool) -> (Sender<Request>, Receiver<Reply>) {
    let (requests, worker_requests) = mpsc::channel();
    let (worker_replies, replies) = mpsc::channel();
    thread::spawn(move || run(worker_requests, worker_replies, file_session));
    (requests, replies)
}

fn run(requests: Receiver<Request>, replies: Sender<Reply>, file_session: bool) {
    // Without a client, every request gets the reason, so each still ends.
    let mut client = client(file_session).map_err(|err| format!("{err:#}"));
    for request in requests {
        let reply = match (&mut client, request) {
            (Err(err), Request::Login { id, .. }) => Reply::LoginDone {
                id,
                result: Err(err.clone()),
            },
            (Err(err), Request::Refresh | Request::Logout) => Reply::Error(err.clone()),
            (Ok(client), Request::Refresh) => match snapshot(client) {
                Ok(snapshot) => Reply::Data(snapshot),
                Err(err) if err.needs_login() => Reply::NeedsLogin,
                Err(err) => Reply::Error(err.to_string()),
            },
            (Ok(client), Request::Login { id, cancel }) => Reply::LoginDone {
                id,
                result: client
                    .login(&cancel, |event| {
                        let _ = replies.send(Reply::Login { id, event });
                    })
                    .map(drop)
                    .map_err(|err| err.to_string()),
            },
            (Ok(client), Request::Logout) => {
                Reply::LoggedOut(client.logout().map(drop).map_err(|e| e.to_string()))
            }
        };
        if replies.send(reply).is_err() {
            return;
        }
    }
}

/// Same files and keychain entry as the terminal example.
fn client(file_session: bool) -> anyhow::Result<Client> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let config_dir = std::path::Path::new(&home).join(".config/scalable-embed-example");
    let mut options = Options::new(config_dir, "dev.example.scalable-embed");
    if file_session {
        options.config.auth.session_backend = SessionBackendPreference::File;
    }
    Ok(Client::new(options)?)
}

/// Everything the window shows, in one request: upstream's overview and
/// holdings queries trimmed to the fields used here, plus each security's
/// performances, which upstream only fetches per security in `broker quote`.
const SNAPSHOT_QUERY: &str = r#"
query DesktopSnapshot($accountId: ID!, $portfolioId: ID!) {
  account(id: $accountId) {
    brokerPortfolio(id: $portfolioId) {
      valuation(includeYearToDate: true) {
        valuation
        timeWeightedReturnByTimeframe {
          timeframe
          simpleAbsoluteReturn
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
              performance
            }
          }
        }
      }
    }
  }
}
"#;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
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
    #[serde(default)]
    time_weighted_return_by_timeframe: Option<Vec<Change>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Change {
    timeframe: Timeframe,
    #[serde(default, deserialize_with = "decimal")]
    simple_absolute_return: Option<f64>,
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
    #[serde(default)]
    performances_by_timeframe: Option<Vec<Performance>>,
}

#[derive(Deserialize)]
struct Performance {
    timeframe: Timeframe,
    #[serde(default, deserialize_with = "decimal")]
    performance: Option<f64>,
}

fn snapshot(client: &mut Client) -> scalable_embed::Result<Snapshot> {
    let ids = client.broker_ids(None)?;
    let variables = json!({"accountId": ids.account_id, "portfolioId": ids.portfolio_id});
    let data: Data = client.query_as("DesktopSnapshot", SNAPSHOT_QUERY, variables)?;
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
        let changes = TIMEFRAMES.map(|choice| {
            returns
                .iter()
                .find(|r| r.timeframe == choice.portfolio)
                .and_then(|r| r.simple_absolute_return)
        });
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
                    returns: TIMEFRAMES.map(|choice| {
                        performances
                            .iter()
                            .find(|p| p.timeframe == choice.holdings)
                            .and_then(|p| p.performance)
                    }),
                }
            })
            .collect();
        Snapshot {
            total: valuation.valuation,
            changes,
            rows,
        }
    }
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
                    {"timeframe": "INTRADAY", "simpleAbsoluteReturn": 12.5},
                    {"timeframe": "MAX", "simpleAbsoluteReturn": "300"}
                ]
            },
            "inventory": {"items": [
                {
                    "isin": "IE00B4L5Y983",
                    "name": "iShares Core MSCI World",
                    "portfolioIsinPerformance": {"valuation": 1000.0},
                    "quoteTick": {"midPrice": "98.76", "performancesByTimeframe": [
                        {"timeframe": "SINCE_BUY", "performance": 0.25},
                        {"timeframe": "TEN_YEARS", "performance": 1.5}
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
