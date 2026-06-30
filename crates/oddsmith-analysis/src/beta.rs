//! Beta of the backtest portfolio against crypto benchmarks.

use std::collections::HashMap;
use std::fs;

use anyhow::{Context, Result, anyhow, bail};
use chrono::NaiveDate;
use oddsmith_store::{Db, Num};

use crate::correlation::pearson;
use crate::crypto::DailyPrice;
use crate::json::{Json, JsonStyle, serialize};
use crate::textfmt::fixed;
use crate::util::{os_err, pg};

/// Beta analysis for one benchmark.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BetaResult {
    pub benchmark: String,
    pub beta: f64,
    /// Annualized Jensen's alpha.
    pub alpha: f64,
    /// Pearson r.
    pub correlation: f64,
    pub r_squared: f64,
    /// Annualized.
    pub tracking_error: f64,
    pub num_observations: i64,
    /// Annualized.
    pub portfolio_mean_ret: f64,
    /// Annualized.
    pub benchmark_mean_ret: f64,
    /// Annualized.
    pub portfolio_std_dev: f64,
    /// Annualized.
    pub benchmark_std_dev: f64,
    pub period_start: String,
    pub period_end: String,
}

/// Beta of the latest backtest run's daily returns against a crypto benchmark.
pub fn compute_crypto_beta(
    db: &Db,
    crypto_prices: &[DailyPrice],
    benchmark_symbol: &str,
    initial_capital: f64,
    risk_free_rate: f64,
) -> Result<BetaResult> {
    let daily_pnl = get_daily_pnl(db).context("getting daily PnL")?;
    if daily_pnl.is_empty() {
        bail!("no backtest data; run 'oddsmith backtest' first");
    }
    beta_from_series(&daily_pnl, crypto_prices, benchmark_symbol, initial_capital, risk_free_rate)
}

/// The beta computation on a daily PnL series (`YYYY-MM-DD`, PnL) and daily closes.
pub fn beta_from_series(
    daily_pnl: &[(String, f64)],
    crypto_prices: &[DailyPrice],
    benchmark_symbol: &str,
    initial_capital: f64,
    risk_free_rate: f64,
) -> Result<BetaResult> {
    // Portfolio daily returns.
    let mut portfolio_returns: HashMap<&str, f64> = HashMap::new();
    let mut portfolio_value = initial_capital;
    for (date, pnl) in daily_pnl {
        if portfolio_value > 0.0 {
            portfolio_returns.insert(date.as_str(), pnl / portfolio_value);
        }
        portfolio_value += pnl;
    }

    // Crypto daily returns.
    let mut crypto_returns: HashMap<String, f64> = HashMap::new();
    for i in 1..crypto_prices.len() {
        let prev = crypto_prices[i - 1].close_price;
        if prev > 0.0 {
            crypto_returns.insert(day(crypto_prices[i].date), (crypto_prices[i].close_price - prev) / prev);
        }
    }

    // Align by benchmark date; portfolio gaps are 0 (no trades that day = 0 return).
    let mut p_rets = Vec::new();
    let mut b_rets = Vec::new();
    let mut start_date = String::new();
    let mut end_date = String::new();
    for p in crypto_prices.iter().skip(1) {
        let date = day(p.date);
        let Some(&b_ret) = crypto_returns.get(&date) else { continue };
        let p_ret = portfolio_returns.get(date.as_str()).copied().unwrap_or(0.0);
        p_rets.push(p_ret);
        b_rets.push(b_ret);
        if start_date.is_empty() {
            start_date = date.clone();
        }
        end_date = date;
    }

    if p_rets.len() < 10 {
        bail!("insufficient overlapping data points ({}) for beta calculation", p_rets.len());
    }

    let n = p_rets.len() as f64;
    let daily_rf = risk_free_rate / 252.0;
    let mean_p = mean(&p_rets);
    let mean_b = mean(&b_rets);

    let mut cov = 0.0;
    let mut var_b = 0.0;
    for i in 0..p_rets.len() {
        cov += (p_rets[i] - mean_p) * (b_rets[i] - mean_b);
        var_b += (b_rets[i] - mean_b) * (b_rets[i] - mean_b);
    }
    cov /= n - 1.0;
    var_b /= n - 1.0;

    let beta = if var_b > 0.0 { cov / var_b } else { 0.0 };
    let corr = pearson(&p_rets, &b_rets);

    // Jensen's alpha (annualized).
    let alpha = (mean_p - daily_rf - beta * (mean_b - daily_rf)) * 252.0;

    let track_diffs: Vec<f64> = p_rets.iter().zip(&b_rets).map(|(p, b)| p - beta * b).collect();
    let te = stddev(&track_diffs) * 252f64.sqrt();

    Ok(BetaResult {
        benchmark: benchmark_symbol.to_string(),
        beta,
        alpha,
        correlation: corr,
        r_squared: corr * corr,
        tracking_error: te,
        num_observations: p_rets.len() as i64,
        portfolio_mean_ret: mean_p * 252.0,
        benchmark_mean_ret: mean_b * 252.0,
        portfolio_std_dev: stddev(&p_rets) * 252f64.sqrt(),
        benchmark_std_dev: stddev(&b_rets) * 252f64.sqrt(),
        period_start: start_date,
        period_end: end_date,
    })
}

fn day(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn get_daily_pnl(db: &Db) -> Result<Vec<(String, f64)>> {
    let rows = db
        .query(
            "
		SELECT entered_at::DATE AS day, SUM(pnl) AS daily_pnl
		FROM backtest_trades bt
		JOIN (SELECT id FROM backtest_runs ORDER BY id DESC LIMIT 1) r ON r.id = bt.run_id
		WHERE entered_at IS NOT NULL
		GROUP BY day ORDER BY day",
            &[],
        )
        .map_err(pg)?;
    rows.iter()
        .map(|r| {
            let d: NaiveDate = r.get(0);
            let pnl =
                r.get::<_, Option<Num>>(1).ok_or_else(|| anyhow!("daily pnl for {d}: expected a number, got null"))?;
            Ok((day(d), pnl.0))
        })
        .collect()
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut s = 0.0;
    for x in xs {
        s += x;
    }
    s / xs.len() as f64
}

/// Sample standard deviation (n-1).
fn stddev(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let m = mean(xs);
    let mut ss = 0.0;
    for x in xs {
        ss += (x - m) * (x - m);
    }
    (ss / (xs.len() - 1) as f64).sqrt()
}

/// The beta results for all benchmarks as JSON.
pub fn beta_summary_json(results: &[BetaResult]) -> Result<String, String> {
    let arr = results
        .iter()
        .map(|r| {
            Json::obj([
                ("benchmark", Json::str(r.benchmark.clone())),
                ("beta", Json::Float(r.beta)),
                ("alpha", Json::Float(r.alpha)),
                ("correlation", Json::Float(r.correlation)),
                ("r_squared", Json::Float(r.r_squared)),
                ("tracking_error", Json::Float(r.tracking_error)),
                ("num_observations", Json::Int(r.num_observations)),
                ("portfolio_mean_ret", Json::Float(r.portfolio_mean_ret)),
                ("benchmark_mean_ret", Json::Float(r.benchmark_mean_ret)),
                ("portfolio_std_dev", Json::Float(r.portfolio_std_dev)),
                ("benchmark_std_dev", Json::Float(r.benchmark_std_dev)),
                ("period_start", Json::str(r.period_start.clone())),
                ("period_end", Json::str(r.period_end.clone())),
            ])
        })
        .collect();
    // NaN/Inf are not valid JSON, so this fails instead of writing them.
    serialize(&Json::Arr(arr), JsonStyle::INDENT_2)
}

/// Write beta results for all benchmarks.
pub fn write_beta_summary_json(path: &str, results: &[BetaResult]) -> Result<()> {
    let data = beta_summary_json(results).map_err(|e| anyhow!(e))?;
    fs::write(path, data).map_err(|e| anyhow!(os_err(&e, path)))
}

/// The comparison table `print_beta_table` prints.
pub fn beta_table(results: &[BetaResult]) -> String {
    if results.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let period = format!("{} to {}", results[0].period_start, results[0].period_end);
    out.push_str(&format!("\n=== Crypto Beta Analysis ({period}) ===\n"));
    let mut line = format!("{:<20}", "");
    for r in results {
        line.push_str(&format!("{:>12}", r.benchmark));
    }
    out.push_str(&line);
    out.push('\n');

    type Field = fn(&BetaResult) -> f64;
    let rows: [(&str, Field, bool); 7] = [
        ("Beta:", |r| r.beta, false),
        ("Alpha (ann.):", |r| r.alpha, true),
        ("Correlation:", |r| r.correlation, false),
        ("R-squared:", |r| r.r_squared, false),
        ("Tracking Error:", |r| r.tracking_error, true),
        ("Portfolio Vol:", |r| r.portfolio_std_dev, true),
        ("Benchmark Vol:", |r| r.benchmark_std_dev, true),
    ];
    for (label, get, pct) in rows {
        let mut line = format!("{label:<20}");
        for r in results {
            let v = get(r);
            if pct {
                line.push_str(&format!("{:>11}%", fixed(v * 100.0, 1)));
            } else {
                line.push_str(&format!("{:>12}", fixed(v, 4)));
            }
        }
        out.push_str(&line);
        out.push('\n');
    }
    let mut line = format!("{:<20}", "Observations:");
    for r in results {
        line.push_str(&format!("{:>12}", r.num_observations));
    }
    out.push_str(&line);
    out.push('\n');
    out
}

/// Print a formatted comparison table.
pub fn print_beta_table(results: &[BetaResult]) {
    print!("{}", beta_table(results));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prices(closes: &[f64]) -> Vec<DailyPrice> {
        let start = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| DailyPrice { date: start + chrono::Days::new(i as u64), close_price: *c, volume: 0.0 })
            .collect()
    }

    #[test]
    fn beta_is_the_ols_slope() {
        // Benchmark returns alternate; the portfolio return is 0.5 * benchmark + 0.001 each day.
        let closes: Vec<f64> = (0..21)
            .scan(100.0, |c, i| {
                let out = *c;
                *c *= if i % 2 == 0 { 1.02 } else { 0.99 };
                Some(out)
            })
            .collect();
        let px = prices(&closes);
        let mut value = 10000.0;
        let mut pnl = Vec::new();
        for i in 1..px.len() {
            let b = (px[i].close_price - px[i - 1].close_price) / px[i - 1].close_price;
            let p = 0.5 * b + 0.001;
            pnl.push((day(px[i].date), p * value));
            value += p * value;
        }
        let r = beta_from_series(&pnl, &px, "BTC", 10000.0, 0.0).unwrap();
        assert_eq!(r.num_observations, 20);
        assert!((r.beta - 0.5).abs() < 1e-9, "{}", r.beta);
        assert!((r.correlation - 1.0).abs() < 1e-9);
        assert!(r.tracking_error < 1e-9);
        assert_eq!((r.period_start.as_str(), r.period_end.as_str()), ("2024-01-02", "2024-01-21"));
    }

    #[test]
    fn needs_ten_points() {
        let px = prices(&[1.0, 2.0, 3.0]);
        let err = beta_from_series(&[("2024-01-02".into(), 1.0)], &px, "BTC", 1.0, 0.0).unwrap_err();
        assert_eq!(err.to_string(), "insufficient overlapping data points (2) for beta calculation");
    }

    #[test]
    fn table_layout() {
        let r = BetaResult {
            benchmark: "BTC".into(),
            beta: 0.12345,
            alpha: -0.5,
            num_observations: 7,
            ..Default::default()
        };
        let t = beta_table(&[r]);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[1], "=== Crypto Beta Analysis ( to ) ===");
        assert_eq!(lines[2], format!("{}{:>12}", " ".repeat(20), "BTC"));
        assert_eq!(lines[3], format!("{:<20}{:>12}", "Beta:", "0.1235"));
        assert_eq!(lines[4], format!("{:<20}{:>11}%", "Alpha (ann.):", "-50.0"));
        assert_eq!(lines[10], format!("{:<20}{:>12}", "Observations:", "7"));
    }
}
