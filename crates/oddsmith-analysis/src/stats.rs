//! Calibration, maker/taker, Mincer-Zarnowitz and spread/liquidity queries.

use anyhow::{Context, Result};
use oddsmith_store::{Db, RowExt};

use crate::util::pg;

/// One row in the calibration curve output.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalibrationRow {
    pub bucket: String,
    /// Midpoint of the bucket (e.g. 0.55 for 50-60).
    pub implied_prob_mid: f64,
    pub num_contracts: i64,
    /// Fraction that resolved YES.
    pub realized_freq: f64,
    /// Realized minus implied.
    pub bias: f64,
}

/// Returns split by maker/taker role.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MakerTakerRow {
    pub role: String,
    pub bucket: Option<String>,
    pub num_trades: i64,
    pub avg_return: f64,
    pub median_return: f64,
    pub win_rate: f64,
    pub std_dev: f64,
}

/// A single data point for the Mincer-Zarnowitz regression.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MzRow {
    pub market_id: String,
    pub implied_prob: f64,
    /// 1 or 0.
    pub realized_outcome: i64,
}

/// Spread/liquidity analysis for a single market.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpreadRow {
    pub market_id: String,
    pub race_type: Option<String>,
    pub avg_spread: f64,
    pub volume: f64,
    pub maker_return: f64,
}

/// Midpoint of a `price_bucket` label, 0 for an unknown label.
pub fn bucket_midpoint(label: &str) -> f64 {
    match label {
        "0-10" => 0.05,
        "10-20" => 0.15,
        "20-30" => 0.25,
        "30-40" => 0.35,
        "40-50" => 0.45,
        "50-60" => 0.55,
        "60-70" => 0.65,
        "70-80" => 0.75,
        "80-90" => 0.85,
        "90-100" => 0.95,
        _ => 0.0,
    }
}

const CALIBRATION_SQL: &str = "
		WITH market_avg_price AS (
			SELECT t.market_id,
				AVG(t.price) AS avg_price,
				m.resolution
			FROM trades t
			JOIN markets m ON m.id = t.market_id
			WHERE m.resolved = true AND m.resolution IS NOT NULL
			GROUP BY t.market_id, m.resolution
		)
		SELECT
			CASE
				WHEN avg_price < 0.1 THEN '0-10'
				WHEN avg_price < 0.2 THEN '10-20'
				WHEN avg_price < 0.3 THEN '20-30'
				WHEN avg_price < 0.4 THEN '30-40'
				WHEN avg_price < 0.5 THEN '40-50'
				WHEN avg_price < 0.6 THEN '50-60'
				WHEN avg_price < 0.7 THEN '60-70'
				WHEN avg_price < 0.8 THEN '70-80'
				WHEN avg_price < 0.9 THEN '80-90'
				ELSE '90-100'
			END AS bucket,
			COUNT(*) AS n,
			AVG(CASE WHEN resolution IN ('YES','Yes','1') THEN 1.0 ELSE 0.0 END) AS realized_freq
		FROM market_avg_price
		GROUP BY bucket
		ORDER BY bucket";

/// Group resolved markets by price bucket and compare how often YES occurred with the
/// implied probability.
pub fn compute_calibration_curve(db: &Db) -> Result<Vec<CalibrationRow>> {
    let rows = db.query(CALIBRATION_SQL, &[]).map_err(pg).context("computing calibration curve")?;
    Ok(rows
        .iter()
        .map(|r| {
            let bucket: String = r.get(0);
            let mid = bucket_midpoint(&bucket);
            let realized = r.num(2);
            CalibrationRow {
                bucket,
                implied_prob_mid: mid,
                num_contracts: r.get(1),
                realized_freq: realized,
                bias: realized - mid,
            }
        })
        .collect())
}

const MAKER_TAKER_SQL: &str = "
		SELECT
			CASE WHEN t.is_taker_trade THEN 'TAKER' ELSE 'MAKER' END AS role,
			t.bucket_label,
			COUNT(*) AS n,
			AVG(CASE WHEN m.resolution IN ('YES','Yes','1')
				THEN (1.0 - t.price) / t.price
				ELSE -1.0
			END) AS avg_return,
			AVG(CASE WHEN m.resolution IN ('YES','Yes','1') THEN 1.0 ELSE 0.0 END) AS win_rate
		FROM trades t
		JOIN markets m ON m.id = t.market_id
		WHERE m.resolved = true AND m.resolution IS NOT NULL
			AND t.is_taker_trade IS NOT NULL
		GROUP BY role, t.bucket_label
		ORDER BY role, t.bucket_label";

/// Split trades by maker/taker and compute returns per bucket.
pub fn compute_maker_taker_returns(db: &Db) -> Result<Vec<MakerTakerRow>> {
    let rows = db.query(MAKER_TAKER_SQL, &[]).map_err(pg).context("computing maker/taker returns")?;
    Ok(rows
        .iter()
        .map(|r| MakerTakerRow {
            role: r.get(0),
            bucket: r.get(1),
            num_trades: r.get(2),
            avg_return: r.num(3),
            win_rate: r.num(4),
            ..Default::default()
        })
        .collect())
}

const MZ_SQL: &str = "
		SELECT m.id, m.last_trade_price,
			CASE WHEN m.resolution IN ('YES','Yes','1') THEN 1 ELSE 0 END
		FROM markets m
		WHERE m.resolved = true AND m.resolution IS NOT NULL
			AND m.last_trade_price IS NOT NULL AND m.last_trade_price > 0";

/// `(implied_probability, realized_outcome)` pairs for the Mincer-Zarnowitz regression.
pub fn compute_mz_data(db: &Db) -> Result<Vec<MzRow>> {
    let rows = db.query(MZ_SQL, &[]).map_err(pg).context("computing MZ data")?;
    Ok(rows
        .iter()
        .map(|r| MzRow { market_id: r.get(0), implied_prob: r.num(1), realized_outcome: r.get::<_, i32>(2) as i64 })
        .collect())
}

const SPREAD_SQL: &str = "
		SELECT m.id, m.race_type,
			COALESCE(m.spread, 0),
			COALESCE(m.volume, 0),
			COALESCE(AVG(CASE WHEN m.resolution IN ('YES','Yes','1')
				THEN (1.0 - bt.entry_price) / bt.entry_price
				ELSE -1.0
			END), 0) AS maker_return
		FROM markets m
		LEFT JOIN backtest_trades bt ON bt.market_id = m.id
		WHERE m.resolved = true AND m.resolution IS NOT NULL
		GROUP BY m.id, m.race_type, m.spread, m.volume
		ORDER BY m.volume DESC";

/// Spread and liquidity per resolved market.
pub fn compute_spread_liquidity(db: &Db) -> Result<Vec<SpreadRow>> {
    let rows = db.query(SPREAD_SQL, &[]).map_err(pg).context("computing spread/liquidity")?;
    Ok(rows
        .iter()
        .map(|r| SpreadRow {
            market_id: r.get(0),
            race_type: r.get(1),
            avg_spread: r.num(2),
            volume: r.num(3),
            maker_return: r.num(4),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midpoints_cover_every_bucket() {
        for p in [0.0, 0.05, 0.19, 0.5, 0.999, 1.0] {
            let label = oddsmith_store::price_bucket(p);
            assert!((bucket_midpoint(&label) - (p.min(0.99) * 10.0).floor() / 10.0 - 0.05).abs() < 1e-12, "{p}");
        }
        assert_eq!(bucket_midpoint("nope"), 0.0);
    }
}
