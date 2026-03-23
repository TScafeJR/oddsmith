//! Portfolio and per-bucket metrics.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

pub const ANNUAL_RISK_FREE_RATE: f64 = 0.04;

/// Aggregated metrics for a price bucket.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct BucketMetrics {
    pub bucket: String,
    pub trade_count: usize,
    pub total_pnl: f64,
    pub avg_return: f64,
    pub median_return: f64,
    pub win_rate: f64,
    /// Market-implied win probability (avg entry price).
    pub expected_win_pct: f64,
    /// Win rate needed to break even after fees.
    pub breakeven_win_pct: f64,
    pub total_fees: f64,
    pub std_dev: f64,
}

/// [`BucketMetrics`] with the PascalCase keys of the stored `results_json` format, in
/// the stored key order.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct BucketJson<'a> {
    pub bucket: &'a str,
    pub trade_count: usize,
    #[serde(rename = "TotalPnL")]
    pub total_pnl: f64,
    pub avg_return: f64,
    pub median_return: f64,
    pub win_rate: f64,
    pub expected_win_pct: f64,
    pub breakeven_win_pct: f64,
    pub total_fees: f64,
    pub std_dev: f64,
}

impl BucketMetrics {
    /// The PascalCase view used for `results_json`.
    pub fn pascal(&self) -> BucketJson<'_> {
        BucketJson {
            bucket: &self.bucket,
            trade_count: self.trade_count,
            total_pnl: self.total_pnl,
            avg_return: self.avg_return,
            median_return: self.median_return,
            win_rate: self.win_rate,
            expected_win_pct: self.expected_win_pct,
            breakeven_win_pct: self.breakeven_win_pct,
            total_fees: self.total_fees,
            std_dev: self.std_dev,
        }
    }

    /// PascalCase JSON object, matching the stored `results_json` format. Non-finite
    /// floats become `null` here; check [`BucketMetrics::is_finite`] first where they
    /// must be rejected instead.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self.pascal()).unwrap_or(serde_json::Value::Null)
    }

    /// Whether every float field is finite (strict JSON can represent it).
    pub fn is_finite(&self) -> bool {
        [
            self.total_pnl,
            self.avg_return,
            self.median_return,
            self.win_rate,
            self.expected_win_pct,
            self.breakeven_win_pct,
            self.total_fees,
            self.std_dev,
        ]
        .iter()
        .all(|v| v.is_finite())
    }
}

/// Portfolio-level backtesting results.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PortfolioMetrics {
    pub total_trades: usize,
    pub total_pnl: f64,
    pub total_fees: f64,
    pub avg_return_pct: f64,
    pub sharpe_ratio: f64,
    pub win_rate: f64,
    pub max_drawdown: f64,
    pub avg_hold_days: f64,
    pub bucket_breakdown: Vec<BucketMetrics>,
    // Filled in when capital is enforced (initial_capital > 0).
    pub initial_capital: f64,
    pub final_equity: f64,
    pub return_on_capital: f64,
    pub peak_exposure: f64,
    pub skipped_trades: usize,
}

/// A single simulated trade.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Position {
    pub market_id: String,
    pub entry_price: f64,
    pub size: f64,
    pub side: String,
    pub role: String,
    /// Actual outcome as stored on the market ("Yes", "No", ...).
    pub resolution: String,
    /// Net of fees.
    pub pnl: f64,
    /// Total fee paid on entry.
    pub fee: f64,
    pub return_pct: f64,
    pub bucket: String,
    /// Days from trade to market resolution.
    pub hold_days: f64,
    pub entered_at: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// Aggregate positions into portfolio-level stats. With `initial_capital > 0`, drawdown is
/// measured on the realized equity curve (capital plus PnL booked at each market's
/// resolution) instead of raw cumulative PnL.
pub fn compute_portfolio_metrics(positions: &[Position], initial_capital: f64) -> PortfolioMetrics {
    if positions.is_empty() {
        return PortfolioMetrics { initial_capital, final_equity: initial_capital, ..PortfolioMetrics::default() };
    }

    let n = positions.len();
    let mut pm = PortfolioMetrics { total_trades: n, ..PortfolioMetrics::default() };

    let mut returns = Vec::with_capacity(n);
    let mut excess_returns = Vec::with_capacity(n);
    let mut hold_days_sum = 0.0;
    let mut wins = 0usize;
    for p in positions {
        pm.total_pnl += p.pnl;
        pm.total_fees += p.fee;
        returns.push(p.return_pct);
        if p.pnl > 0.0 {
            wins += 1;
        }
        hold_days_sum += p.hold_days;
        // Pro-rate the risk-free rate to the holding period, floored at one day.
        let mut hold_years = p.hold_days / 365.0;
        if hold_years < 1.0 / 365.0 {
            hold_years = 1.0 / 365.0;
        }
        excess_returns.push(p.return_pct - ANNUAL_RISK_FREE_RATE * hold_years);
    }

    pm.win_rate = wins as f64 / n as f64;
    pm.avg_return_pct = avg(&returns);
    pm.avg_hold_days = hold_days_sum / n as f64;

    // Sharpe ratio: mean excess return / std dev, annualized by the average hold period.
    if excess_returns.len() > 1 {
        let mean_excess = avg(&excess_returns);
        let sd = stddev(&excess_returns, mean_excess);
        if sd > 0.0 {
            let per_trade_sharpe = mean_excess / sd;
            let trades_per_year = float_div(365.0, pm.avg_hold_days);
            pm.sharpe_ratio = per_trade_sharpe * trades_per_year.sqrt();
        }
    }

    if initial_capital > 0.0 {
        pm.initial_capital = initial_capital;
        pm.final_equity = initial_capital + pm.total_pnl;
        pm.return_on_capital = pm.total_pnl / initial_capital;
        pm.max_drawdown = equity_drawdown(positions, initial_capital);
    } else {
        pm.max_drawdown = max_drawdown(positions);
    }

    pm.bucket_breakdown = compute_bucket_metrics(positions);
    pm
}

fn compute_bucket_metrics(positions: &[Position]) -> Vec<BucketMetrics> {
    // Group in first-seen order.
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut groups: Vec<(&str, Vec<&Position>)> = Vec::new();
    for p in positions {
        let i = *index.entry(&p.bucket).or_insert_with(|| {
            groups.push((&p.bucket, Vec::new()));
            groups.len() - 1
        });
        groups[i].1.push(p);
    }

    let mut result: Vec<BucketMetrics> = groups
        .into_iter()
        .map(|(bucket, ps)| {
            let mut returns = Vec::with_capacity(ps.len());
            let mut entry_prices = Vec::with_capacity(ps.len());
            let mut breakevens = Vec::with_capacity(ps.len());
            let mut wins = 0usize;
            let mut total_pnl = 0.0;
            let mut total_fees = 0.0;
            for p in &ps {
                returns.push(p.return_pct);
                entry_prices.push(p.entry_price);
                breakevens.push(p.entry_price + float_div(p.fee, p.size));
                total_pnl += p.pnl;
                total_fees += p.fee;
                if p.pnl > 0.0 {
                    wins += 1;
                }
            }
            let mean = avg(&returns);
            BucketMetrics {
                bucket: bucket.to_string(),
                trade_count: ps.len(),
                total_pnl,
                avg_return: mean,
                median_return: median(&returns),
                win_rate: wins as f64 / ps.len() as f64,
                expected_win_pct: avg(&entry_prices),
                breakeven_win_pct: avg(&breakevens),
                total_fees,
                std_dev: stddev(&returns, mean),
            }
        })
        .collect();

    // Sort numerically by the leading number in the label ("5-10" -> 5); stable.
    result.sort_by(|a, b| {
        bucket_leading_num(&a.bucket).partial_cmp(&bucket_leading_num(&b.bucket)).unwrap_or(std::cmp::Ordering::Equal)
    });
    result
}

/// Leading number of a bucket label like "5-10" -> 5 (0 if it doesn't parse).
fn bucket_leading_num(label: &str) -> f64 {
    let head = label.split_once('-').map_or(label, |(h, _)| h);
    crate::textfmt::parse_float(head).unwrap_or(0.0)
}

/// IEEE division: `x/0` is ±inf and `0/0` is NaN.
pub fn float_div(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        if a == 0.0 || a.is_nan() {
            return f64::NAN;
        }
        return f64::INFINITY.copysign(a) * 1f64.copysign(b);
    }
    a / b
}

/// Mean by sequential summation (0 for an empty slice).
pub(crate) fn avg(vals: &[f64]) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    let mut total = 0.0;
    for v in vals {
        total += v;
    }
    total / vals.len() as f64
}

/// Sample standard deviation around `mean` (0 for fewer than two values).
pub(crate) fn stddev(vals: &[f64], mean: f64) -> f64 {
    if vals.len() < 2 {
        return 0.0;
    }
    let mut sum_sq = 0.0;
    for v in vals {
        let d = v - mean;
        sum_sq += d * d;
    }
    (sum_sq / (vals.len() - 1) as f64).sqrt()
}

fn median(vals: &[f64]) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    let mut s = vals.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = s.len() / 2;
    if s.len() % 2 == 0 { (s[mid - 1] + s[mid]) / 2.0 } else { s[mid] }
}

fn max_drawdown(positions: &[Position]) -> f64 {
    let mut cum_pnl = 0.0;
    let mut peak = 0.0;
    let mut max_dd = 0.0;
    for p in positions {
        cum_pnl += p.pnl;
        if cum_pnl > peak {
            peak = cum_pnl;
        }
        let dd = peak - cum_pnl;
        if dd > max_dd {
            max_dd = dd;
        }
    }
    if peak > 0.0 { max_dd / peak } else { max_dd }
}

/// `a` unless `b` is strictly greater (ties and NaN `b` keep `a`).
pub(crate) fn max_first(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

/// Positions in timestamp order, missing timestamps last (a stable sort).
pub(crate) fn sorted_by_time(
    positions: &[Position],
    key: impl Fn(&Position) -> Option<DateTime<Utc>>,
) -> Vec<&Position> {
    let mut ordered: Vec<&Position> = positions.iter().collect();
    ordered.sort_by_key(|p| {
        let t = key(p);
        (t.is_none(), t)
    });
    ordered
}

/// Largest peak-to-trough fall of realized equity, as a fraction of the peak. PnL is
/// booked when each market resolves, so positions are replayed in resolution order.
fn equity_drawdown(positions: &[Position], initial_capital: f64) -> f64 {
    let mut equity = initial_capital;
    let mut peak = initial_capital;
    let mut max_dd = 0.0;
    for p in sorted_by_time(positions, |p| p.resolved_at) {
        equity += p.pnl;
        peak = max_first(peak, equity);
        if peak > 0.0 {
            max_dd = max_first(max_dd, (peak - equity) / peak);
        }
    }
    max_dd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_div_follows_ieee() {
        assert_eq!(float_div(1.0, 0.0), f64::INFINITY);
        assert_eq!(float_div(-1.0, 0.0), f64::NEG_INFINITY);
        assert_eq!(float_div(1.0, -0.0), f64::NEG_INFINITY);
        assert!(float_div(0.0, 0.0).is_nan());
        assert_eq!(float_div(1.0, 4.0), 0.25);
    }

    #[test]
    fn pascal_json_keys_in_field_order() {
        let b = BucketMetrics { bucket: "5-10".into(), trade_count: 2, ..BucketMetrics::default() };
        let text = serde_json::to_string(&b.pascal()).unwrap();
        assert!(text.starts_with(r#"{"Bucket":"5-10","TradeCount":2,"TotalPnL":0.0,"AvgReturn""#), "{text}");
        assert!(text.ends_with(r#""TotalFees":0.0,"StdDev":0.0}"#), "{text}");
    }
}
