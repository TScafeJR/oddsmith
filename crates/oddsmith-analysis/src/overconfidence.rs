//! Overconfidence metrics: per-bucket calibration error and Brier score decomposition.

use std::fs;

use anyhow::{Context, Result, anyhow};
use oddsmith_store::{Db, Param, RowExt, price_bucket};

use crate::json::{Json, JsonStyle, serialize};
use crate::stats::bucket_midpoint;
use crate::textfmt::{ff, round_half_away_trunc};
use crate::util::{Csv, pg};

/// Per-bucket overconfidence metrics.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverconfidenceBucket {
    pub bucket: String,
    pub implied_prob_mid: f64,
    pub num_contracts: i64,
    pub realized_freq: f64,
    /// Implied minus realized (positive means overconfident).
    pub calibration_error: f64,
    pub abs_calib_error: f64,
    pub total_volume: f64,
    pub brier_in_bucket: f64,
}

/// All overconfidence metrics.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverconfidenceResult {
    pub buckets: Vec<OverconfidenceBucket>,
    /// Mean signed calibration error for favorites (> 50%).
    pub overconfidence_index: f64,
    /// Mean signed calibration error for longshots (< 50%).
    pub underconfidence_index: f64,
    pub brier_score: f64,
    pub brier_reliability: f64,
    pub brier_resolution: f64,
    pub brier_uncertainty: f64,
    pub confidence_weighted_bias: f64,
    pub num_markets: i64,
}

/// One resolved market: its average trade price, outcome (1 or 0) and volume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarketData {
    pub implied_prob: f64,
    pub outcome: i64,
    pub volume: f64,
}

/// Overconfidence metrics from resolved markets, optionally for one race type.
pub fn compute_overconfidence(db: &Db, race_filter: &str) -> Result<OverconfidenceResult> {
    let mut query = String::from(
        "
		SELECT AVG(t.price) as implied_prob,
			CASE WHEN m.resolution IN ('YES','Yes','1') THEN 1 ELSE 0 END as outcome,
			COALESCE(m.volume, 0) as volume
		FROM trades t
		JOIN markets m ON m.id = t.market_id
		WHERE m.resolved = true AND m.resolution IS NOT NULL",
    );
    let mut args: Vec<Param<'_>> = Vec::new();
    if !race_filter.is_empty() {
        query.push_str(" AND m.race_type = $1");
        args.push(&race_filter);
    }
    query.push_str(" GROUP BY m.id, m.resolution, m.volume");

    let rows = db.query(&query, &args).map_err(pg).context("querying markets for overconfidence")?;
    let markets: Vec<MarketData> = rows
        .iter()
        .map(|r| MarketData { implied_prob: r.num(0), outcome: r.get::<_, i32>(1) as i64, volume: r.num(2) })
        .collect();
    if markets.is_empty() {
        return Ok(OverconfidenceResult::default());
    }
    compute_from_markets(&markets)
}

const BUCKET_ORDER: [&str; 10] =
    ["0-10", "10-20", "20-30", "30-40", "40-50", "50-60", "60-70", "70-80", "80-90", "90-100"];

#[derive(Debug, Clone, Copy, Default)]
struct BucketAcc {
    count: i64,
    yes_count: i64,
    volume: f64,
    brier: f64,
}

/// Bucket the markets and compute calibration errors and the Brier decomposition
/// (reliability, resolution, uncertainty). `markets` must not be empty.
pub fn compute_from_markets(markets: &[MarketData]) -> Result<OverconfidenceResult> {
    let mut buckets = [BucketAcc::default(); 10];

    let mut total_brier = 0.0;
    let mut total_yes = 0i64;
    let n = markets.len() as f64;

    for m in markets {
        let label = price_bucket(m.implied_prob);
        let idx = BUCKET_ORDER.iter().position(|b| *b == label).ok_or_else(|| anyhow!("'{label}'"))?;
        let b = &mut buckets[idx];
        b.count += 1;
        b.volume += m.volume;
        if m.outcome == 1 {
            b.yes_count += 1;
            total_yes += 1;
        }
        let o = m.outcome as f64;
        let bs = (m.implied_prob - o) * (m.implied_prob - o);
        b.brier += bs;
        total_brier += bs;
    }

    let base_rate = total_yes as f64 / n;

    let mut result = OverconfidenceResult {
        num_markets: markets.len() as i64,
        brier_score: total_brier / n,
        brier_uncertainty: base_rate * (1.0 - base_rate),
        ..Default::default()
    };

    let mut reliability = 0.0;
    let mut resolution = 0.0;
    for (label, b) in BUCKET_ORDER.iter().zip(buckets.iter()) {
        let mid = bucket_midpoint(label);
        if b.count == 0 {
            result.buckets.push(OverconfidenceBucket {
                bucket: label.to_string(),
                implied_prob_mid: mid,
                ..Default::default()
            });
            continue;
        }
        let realized = b.yes_count as f64 / b.count as f64;
        let cal_err = mid - realized;
        let nk = b.count as f64;
        reliability += nk * (mid - realized) * (mid - realized);
        resolution += nk * (realized - base_rate) * (realized - base_rate);
        result.buckets.push(OverconfidenceBucket {
            bucket: label.to_string(),
            implied_prob_mid: mid,
            num_contracts: b.count,
            realized_freq: realized,
            calibration_error: cal_err,
            abs_calib_error: cal_err.abs(),
            total_volume: b.volume,
            brier_in_bucket: b.brier / nk,
        });
    }
    result.brier_reliability = reliability / n;
    result.brier_resolution = resolution / n;

    let (mut fav_sum, mut fav_n, mut ls_sum, mut ls_n) = (0.0, 0.0, 0.0, 0.0);
    let (mut vol_weighted_bias, mut total_vol) = (0.0, 0.0);
    for bk in &result.buckets {
        if bk.num_contracts == 0 {
            continue;
        }
        if bk.implied_prob_mid > 0.50 {
            fav_sum += bk.calibration_error;
            fav_n += 1.0;
        } else {
            ls_sum += bk.calibration_error;
            ls_n += 1.0;
        }
        vol_weighted_bias += bk.total_volume * bk.calibration_error;
        total_vol += bk.total_volume;
    }
    if fav_n > 0.0 {
        result.overconfidence_index = fav_sum / fav_n;
    }
    if ls_n > 0.0 {
        result.underconfidence_index = ls_sum / ls_n;
    }
    if total_vol > 0.0 {
        result.confidence_weighted_bias = vol_weighted_bias / total_vol;
    }
    Ok(result)
}

/// Write the per-bucket breakdown.
pub fn write_overconfidence_csv(path: &str, r: &OverconfidenceResult) -> std::io::Result<()> {
    let mut w = Csv::new();
    w.row(&[
        "bucket",
        "implied_prob_mid",
        "n",
        "realized_freq",
        "calibration_error",
        "abs_cal_error",
        "total_volume",
        "brier_score",
    ]);
    for b in &r.buckets {
        w.row(&[
            b.bucket.clone(),
            ff(b.implied_prob_mid),
            b.num_contracts.to_string(),
            ff(b.realized_freq),
            ff(b.calibration_error),
            ff(b.abs_calib_error),
            ff(b.total_volume),
            ff(b.brier_in_bucket),
        ]);
    }
    w.save(path)
}

/// The aggregate metrics as JSON with sorted keys.
pub fn overconfidence_summary_json(r: &OverconfidenceResult) -> Result<String, String> {
    let summary = Json::obj([
        ("num_markets", Json::Int(r.num_markets)),
        ("overconfidence_index", Json::Float(round4(r.overconfidence_index))),
        ("underconfidence_index", Json::Float(round4(r.underconfidence_index))),
        ("brier_score", Json::Float(round4(r.brier_score))),
        ("brier_reliability", Json::Float(round4(r.brier_reliability))),
        ("brier_resolution", Json::Float(round4(r.brier_resolution))),
        ("brier_uncertainty", Json::Float(round4(r.brier_uncertainty))),
        ("confidence_weighted_bias", Json::Float(round4(r.confidence_weighted_bias))),
    ]);
    serialize(&summary, JsonStyle::SORTED_2)
}

/// Write the aggregate metrics as JSON with sorted keys.
pub fn write_overconfidence_summary_json(path: &str, r: &OverconfidenceResult) -> Result<()> {
    let data = overconfidence_summary_json(r).map_err(|e| anyhow!(e))?;
    fs::write(path, data).map_err(|e| anyhow!(crate::util::os_err(&e, path)))
}

fn round4(v: f64) -> f64 {
    round_half_away_trunc(v * 10000.0) / 10000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: f64, o: i64, v: f64) -> MarketData {
        MarketData { implied_prob: p, outcome: o, volume: v }
    }

    #[test]
    fn brier_decomposition() {
        let markets = [m(0.35, 0, 100.0), m(0.45, 1, 50.0), m(0.42, 0, 50.0), m(0.55, 1, 200.0), m(0.58, 0, 100.0)];
        let r = compute_from_markets(&markets).unwrap();
        assert_eq!(r.num_markets, 5);
        let bs = (0.35f64.powi(2) + 0.55f64.powi(2) + 0.42f64.powi(2) + 0.45f64.powi(2) + 0.58f64.powi(2)) / 5.0;
        assert!((r.brier_score - bs).abs() < 1e-12);
        // Base rate 0.4.
        assert!((r.brier_uncertainty - 0.24).abs() < 1e-12);
        // Buckets: 30-40 (1 market, 0 yes), 40-50 (2, 1 yes), 50-60 (2, 1 yes).
        let rel = (0.35f64.powi(2) + 2.0 * 0.05f64.powi(2) + 2.0 * 0.05f64.powi(2)) / 5.0;
        assert!((r.brier_reliability - rel).abs() < 1e-12);
        let res = (0.4f64.powi(2) + 2.0 * 0.1f64.powi(2) + 2.0 * 0.1f64.powi(2)) / 5.0;
        assert!((r.brier_resolution - res).abs() < 1e-12);
        let b = &r.buckets[3];
        assert_eq!((b.bucket.as_str(), b.num_contracts), ("30-40", 1));
        assert!((b.calibration_error - 0.35).abs() < 1e-12);
        assert!((r.overconfidence_index - 0.05).abs() < 1e-12);
        assert!((r.underconfidence_index - (0.35 + -0.05) / 2.0).abs() < 1e-12);
        let vw = (100.0 * 0.35 + 100.0 * -0.05 + 300.0 * 0.05) / 500.0;
        assert!((r.confidence_weighted_bias - vw).abs() < 1e-12);
        assert_eq!(r.buckets.len(), 10);
    }

    #[test]
    fn summary_json_shape() {
        let r = compute_from_markets(&[m(0.55, 1, 0.0), m(0.65, 1, 0.0)]).unwrap();
        let s = overconfidence_summary_json(&r).unwrap();
        assert!(s.starts_with("{\n  \"brier_reliability\": "), "{s}");
        assert!(s.contains("\"num_markets\": 2,"));
        assert!(s.contains("\"confidence_weighted_bias\": 0.0,"));
        assert!(!s.ends_with('\n'));
    }
}
