//! Pairwise price correlations between markets in the same event.

use std::collections::HashMap;

use anyhow::{Context, Result};
use oddsmith_store::{Db, RowExt};

use crate::util::pg;

/// Pairwise correlation between related markets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CorrelationRow {
    pub event_name: String,
    pub market_a: String,
    pub market_b: String,
    pub pearson_r: f64,
    pub num_obs: i64,
}

const EVENTS_SQL: &str = "
		SELECT e.id, e.title, array_agg(m.id) AS market_ids
		FROM events e
		JOIN markets m ON m.event_id = e.id
		WHERE m.resolved = true
		GROUP BY e.id, e.title
		HAVING COUNT(m.id) > 1
		ORDER BY e.title";

const HOURLY_PRICES_SQL: &str = "
		SELECT EXTRACT(EPOCH FROM timestamp)::BIGINT, price
		FROM price_history
		WHERE market_id = $1 AND interval = '1h'
		ORDER BY timestamp";

/// Pairwise hourly price correlations for markets within the same event.
pub fn compute_correlation_matrix(db: &Db) -> Result<Vec<CorrelationRow>> {
    let rows = db.query(EVENTS_SQL, &[]).map_err(pg).context("finding multi-market events")?;
    let events: Vec<(String, Vec<String>)> = rows.iter().map(|r| (r.get(1), r.get(2))).collect();

    // Hourly series are fetched once per market; a failed fetch skips the pairs that need it.
    let mut cache: HashMap<String, Vec<(i64, f64)>> = HashMap::new();
    let mut result = Vec::new();
    for (title, ids) in &events {
        for i in 0..ids.len() {
            for j in i + 1..ids.len() {
                let Ok((r, n)) = pair_correlation(db, &mut cache, &ids[i], &ids[j]) else { continue };
                if n < 5 {
                    continue;
                }
                result.push(CorrelationRow {
                    event_name: title.clone(),
                    market_a: ids[i].clone(),
                    market_b: ids[j].clone(),
                    pearson_r: r,
                    num_obs: n as i64,
                });
            }
        }
    }
    Ok(result)
}

fn hourly_prices<'a>(
    db: &Db,
    cache: &'a mut HashMap<String, Vec<(i64, f64)>>,
    market_id: &str,
) -> Result<&'a Vec<(i64, f64)>> {
    if !cache.contains_key(market_id) {
        let rows = db.query(HOURLY_PRICES_SQL, &[&market_id])?;
        let prices = rows.iter().map(|r| (r.get::<_, i64>(0), r.num(1))).collect();
        cache.insert(market_id.to_string(), prices);
    }
    Ok(&cache[market_id])
}

fn pair_correlation(db: &Db, cache: &mut HashMap<String, Vec<(i64, f64)>>, a: &str, b: &str) -> Result<(f64, usize)> {
    if hourly_prices(db, cache, a)?.is_empty() {
        return Ok((0.0, 0));
    }
    let prices_b = hourly_prices(db, cache, b)?.clone();
    if prices_b.is_empty() {
        return Ok((0.0, 0));
    }
    let prices_a = &cache[a];
    Ok(align_and_correlate(prices_a, &prices_b))
}

/// Align two `(timestamp, price)` series on timestamp (a later duplicate in `b` wins) and
/// correlate them; fewer than 5 aligned points report r = 0.
pub fn align_and_correlate(prices_a: &[(i64, f64)], prices_b: &[(i64, f64)]) -> (f64, usize) {
    let b_map: HashMap<i64, f64> = prices_b.iter().copied().collect();
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for (ts, price) in prices_a {
        if let Some(pb) = b_map.get(ts) {
            xs.push(*price);
            ys.push(*pb);
        }
    }
    if xs.len() < 5 {
        return (0.0, xs.len());
    }
    (pearson(&xs, &ys), xs.len())
}

/// Pearson r from single-pass sums. 0 when either series has no variance; NaN when
/// rounding pushes the variance product below zero.
pub fn pearson(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let (mut sum_x, mut sum_y, mut sum_xy, mut sum_x2, mut sum_y2) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for i in 0..x.len() {
        sum_x += x[i];
        sum_y += y[i];
        sum_xy += x[i] * y[i];
        sum_x2 += x[i] * x[i];
        sum_y2 += y[i] * y[i];
    }
    let num = n * sum_xy - sum_x * sum_y;
    let den = safe_sqrt((n * sum_x2 - sum_x * sum_x) * (n * sum_y2 - sum_y * sum_y));
    if den == 0.0 {
        return 0.0;
    }
    num / den
}

fn safe_sqrt(v: f64) -> f64 {
    if v < 0.0 { f64::NAN } else { v.sqrt() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pearson_basics() {
        let x = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert!((pearson(&x, &[2.0, 4.0, 6.0, 8.0, 10.0]) - 1.0).abs() < 1e-12);
        assert!((pearson(&x, &[5.0, 4.0, 3.0, 2.0, 1.0]) + 1.0).abs() < 1e-12);
        assert_eq!(pearson(&x, &[3.0; 5]), 0.0);
        assert_eq!(pearson(&[1.0], &[1.0]), 0.0);
        // r for a known small sample: x = 1..5, y = (2, 1, 4, 3, 5) gives 0.8.
        assert!((pearson(&x, &[2.0, 1.0, 4.0, 3.0, 5.0]) - 0.8).abs() < 1e-12);
    }

    #[test]
    fn alignment_needs_five_points() {
        let a: Vec<(i64, f64)> = (0..6).map(|i| (i * 3600, i as f64)).collect();
        let b: Vec<(i64, f64)> = (2..8).map(|i| (i * 3600, (i * 2) as f64)).collect();
        assert_eq!(align_and_correlate(&a, &b), (0.0, 4));
        let b: Vec<(i64, f64)> = (0..6).map(|i| (i * 3600, (10 - i) as f64)).collect();
        let (r, n) = align_and_correlate(&a, &b);
        assert_eq!(n, 6);
        assert!((r + 1.0).abs() < 1e-12);
    }
}
