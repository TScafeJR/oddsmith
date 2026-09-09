//! The pure core of the backtester. Nothing here touches the network or a database:
//! callers hand in [`BacktestTrade`] rows and get positions and metrics back, so the
//! same code runs from the CLI, the web UI and the tests.

pub mod engine;
pub mod metrics;
pub mod research;
pub mod strategy;
pub mod textfmt;

use chrono::{DateTime, Utc};

pub use engine::{Cancelled, CapitalLedger, apply_capital, simulate};
pub use metrics::{BucketJson, BucketMetrics, PortfolioMetrics, Position, compute_portfolio_metrics};
pub use research::{Interval, ParamValue, RollingRow, SweepRow, TradeSource};
pub use strategy::{JsonObject, Strategy, Window, WindowBy, load_strategy, strategy_from_json};

/// One qualifying trade on a resolved market, as loaded for a backtest.
///
/// `price` is the price of the token the strategy buys (the NO-token price when the
/// strategy's side is NO). `traded_at` is the entry time and `end_date` the market's
/// resolution time.
#[derive(Debug, Clone, PartialEq)]
pub struct BacktestTrade {
    pub market_id: String,
    pub price: f64,
    pub resolution: String,
    pub traded_at: DateTime<Utc>,
    pub end_date: DateTime<Utc>,
}
