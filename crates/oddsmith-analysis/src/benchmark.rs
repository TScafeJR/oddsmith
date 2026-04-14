//! Polls-vs-market benchmark.

use anyhow::{Context, Result};
use oddsmith_store::{Db, RowExt, price_bucket};

use crate::util::pg;

/// Compares a Polymarket price to a poll-implied probability.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BenchmarkRow {
    pub market_id: String,
    pub race_type: Option<String>,
    pub state: String,
    pub candidate: String,
    pub poll_implied_prob: f64,
    pub market_price: f64,
    /// 1 or 0.
    pub actual_outcome: i64,
    /// `|actual - poll|`.
    pub poll_error: f64,
    /// `|actual - market|`.
    pub market_error: f64,
    /// Market minus poll (positive means the market is more bullish).
    pub bias: f64,
    pub price_bucket: String,
}

const POLLS_VS_MARKET_SQL: &str = "
		SELECT
			m.id,
			m.race_type,
			COALESCE(m.state, ''),
			pmm.candidate,
			m.last_trade_price,
			m.resolution,
			(
				SELECT p.implied_prob
				FROM polls p
				WHERE p.race_type = pmm.race_type
					AND (p.state = pmm.state OR (p.state IS NULL AND pmm.state IS NULL))
					AND p.candidate = pmm.candidate
				ORDER BY ABS(EXTRACT(EPOCH FROM (p.poll_date - COALESCE(m.end_date, NOW()))))
				LIMIT 1
			) AS poll_implied_prob
		FROM markets m
		JOIN poll_market_matches pmm ON pmm.market_id = m.id
		WHERE m.resolved = true AND m.resolution IS NOT NULL
			AND m.last_trade_price IS NOT NULL AND m.last_trade_price > 0";

/// For each resolved market with matched polls, compare the closest poll to the last trade
/// price.
pub fn compute_polls_vs_market(db: &Db) -> Result<Vec<BenchmarkRow>> {
    let rows = db.query(POLLS_VS_MARKET_SQL, &[]).map_err(pg).context("computing polls vs market")?;
    let mut result = Vec::new();
    for r in &rows {
        // No matching poll found.
        let Some(poll_prob) = r.opt_num(6) else { continue };
        let resolution: String = r.get(5);
        result.push(benchmark_row(r.get(0), r.get(1), r.get(2), r.get(3), r.num(4), &resolution, poll_prob));
    }
    Ok(result)
}

/// Build one comparison from a market's last price, its resolution and the closest poll.
pub fn benchmark_row(
    market_id: String,
    race_type: Option<String>,
    state: String,
    candidate: String,
    market_price: f64,
    resolution: &str,
    poll_implied_prob: f64,
) -> BenchmarkRow {
    let actual_outcome = i64::from(matches!(resolution, "YES" | "Yes" | "1"));
    let actual = actual_outcome as f64;
    BenchmarkRow {
        market_id,
        race_type,
        state,
        candidate,
        poll_implied_prob,
        market_price,
        actual_outcome,
        poll_error: (actual - poll_implied_prob).abs(),
        market_error: (actual - market_price).abs(),
        bias: market_price - poll_implied_prob,
        price_bucket: price_bucket(market_price),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_and_bias() {
        let r = benchmark_row("m".into(), None, "WV".into(), "A".into(), 0.75, "Yes", 0.6);
        assert_eq!(r.actual_outcome, 1);
        assert!((r.poll_error - 0.4).abs() < 1e-12);
        assert!((r.market_error - 0.25).abs() < 1e-12);
        assert!((r.bias - 0.15).abs() < 1e-12);
        assert_eq!(r.price_bucket, "70-80");
        let r = benchmark_row("m".into(), None, String::new(), "A".into(), 0.3, "NO", 0.6);
        assert_eq!(r.actual_outcome, 0);
        assert!((r.market_error - 0.3).abs() < 1e-12);
    }
}
