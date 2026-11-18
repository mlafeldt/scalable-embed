//! The portfolio values, rows and timeframes the UI shows.

use scalable_embed::Timeframe;

/// A timeframe to pick.
pub struct TimeframeChoice {
    pub label: &'static str,
    /// For the holdings' returns.
    pub holdings: Timeframe,
    /// For the portfolio's change.
    pub portfolio: Timeframe,
}

const fn choice(label: &'static str, holdings: Timeframe, portfolio: Timeframe) -> TimeframeChoice {
    TimeframeChoice {
        label,
        holdings,
        portfolio,
    }
}

pub const TIMEFRAMES: [TimeframeChoice; 6] = {
    use Timeframe::*;
    [
        choice("Today", Intraday, Intraday),
        choice("Week", OneWeek, OneWeek),
        choice("Month", OneMonth, OneMonth),
        choice("Year to date", YearToDate, YearToDate),
        choice("Year", OneYear, OneYear),
        // The portfolio has no since-buy change. Scalable shows its MAX one
        // instead, which also counts gains on positions sold since.
        choice("Since buy", SinceBuy, Max),
    ]
};

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
