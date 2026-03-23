//! Research helpers built on the engine.

use anyhow::Result;

use crate::BacktestTrade;
use crate::strategy::Window;

/// Anything that can load backtest trades (the database, or a fixture in tests).
pub trait TradeSource {
    /// Trades on resolved markets inside the YES-probability band, oldest first.
    fn backtest_trades(
        &self,
        min_price: f64,
        max_price: f64,
        side: &str,
        race_filter: &str,
        window: &Window,
    ) -> Result<Vec<BacktestTrade>>;
}
