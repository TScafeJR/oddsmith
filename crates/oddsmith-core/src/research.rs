//! Parameter sweeps, rolling windows, bootstrap confidence intervals and their report
//! files.
//!
//! Sweeps fetch trades once per side, using the widest price band in the grid, then
//! simulate every combination in memory with [`simulate`]. That keeps a sweep of dozens of
//! variants close to the cost of a single backtest.

use std::fmt;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike, TimeDelta, Utc};

use crate::BacktestTrade;
use crate::engine::{apply_capital, simulate};
use crate::metrics::{Position, compute_portfolio_metrics};
use crate::strategy::{Strategy, Window};
use crate::textfmt;

/// Strategy fields a sweep may vary.
pub const SWEEPABLE: &[&str] =
    &["min_price", "max_price", "position_size", "maker_fee_rate", "taker_fee_rate", "side", "role"];

/// Metrics a sweep can rank and plot by.
pub const METRICS: &[&str] = &["avg_return", "sharpe", "edge", "total_pnl", "win_rate"];

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

/// One swept parameter value.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Num(f64),
    Text(String),
}

impl ParamValue {
    /// Numbers in `%g` form with 6 significant digits, text as is (the sweep table's format).
    pub fn short(&self) -> String {
        match self {
            ParamValue::Num(v) => textfmt::g(*v, 6),
            ParamValue::Text(s) => s.clone(),
        }
    }
}

/// Numbers in their shortest round-trip form (`0.5`, `100.0`, `1e-05`), text as is.
impl fmt::Display for ParamValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParamValue::Num(v) => f.write_str(&textfmt::shortest(*v)),
            ParamValue::Text(s) => f.write_str(s),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interval {
    pub low: f64,
    pub high: f64,
}

// ---------------------------------------------------------------------------
// Statistics

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(GOLDEN);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// xoshiro256**, seeded through SplitMix64. Small, fast and good enough for resampling.
#[derive(Debug, Clone)]
pub struct Rng([u64; 4]);

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut sm = seed;
        Rng([splitmix64(&mut sm), splitmix64(&mut sm), splitmix64(&mut sm), splitmix64(&mut sm)])
    }

    /// An independent stream for one resample, so resamples can run on any thread and
    /// still give the same numbers.
    fn stream(seed: u64, index: u64) -> Self {
        let mut a = seed;
        let mut b = index ^ 0xD1B5_4A32_D192_ED03;
        Rng::new(splitmix64(&mut a) ^ splitmix64(&mut b).rotate_left(17))
    }

    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.0;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// Uniform in `[0, 1)` from 53 random bits.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

/// Percentile bootstrap confidence interval for `stat` over `values` (seeded; `None` with
/// fewer than two values or no resamples).
///
/// Each resample draws `n` values with replacement (`floor(u * n)` indexes) from its own
/// seeded stream. The resamples run in parallel for large inputs; the result doesn't
/// depend on the thread count. The percentile indexes are `floor(alpha/2 * B)` and
/// `ceil((1 - alpha/2) * B) - 1`.
pub fn bootstrap_ci(
    values: &[f64],
    stat: fn(&[f64]) -> f64,
    resamples: usize,
    alpha: f64,
    seed: u64,
) -> Option<Interval> {
    if values.len() < 2 || resamples == 0 {
        return None;
    }
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get());
    let threads = if values.len() * resamples < 200_000 { 1 } else { threads };
    let mut stats = resample_stats(values, stat, resamples, seed, threads);
    stats.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let b = resamples as f64;
    let lo_idx = ((alpha / 2.0 * b).floor() as i64).max(0) as usize;
    let hi_idx = ((((1.0 - alpha / 2.0) * b).ceil() as i64) - 1).min(resamples as i64 - 1).max(0) as usize;
    Some(Interval { low: stats[lo_idx.min(resamples - 1)], high: stats[hi_idx] })
}

/// The statistic of each resample, in resample order, computed on up to `threads` threads.
fn resample_stats(values: &[f64], stat: fn(&[f64]) -> f64, resamples: usize, seed: u64, threads: usize) -> Vec<f64> {
    let n = values.len();
    let nf = n as f64;
    let one = |i: usize, buf: &mut Vec<f64>| {
        let mut rng = Rng::stream(seed, i as u64);
        buf.clear();
        buf.extend((0..n).map(|_| values[((rng.next_f64() * nf) as usize).min(n - 1)]));
        stat(buf)
    };
    let mut stats = vec![0.0; resamples];
    let per = resamples.div_ceil(threads.clamp(1, resamples.max(1))).max(1);
    std::thread::scope(|scope| {
        for (chunk_no, chunk) in stats.chunks_mut(per).enumerate() {
            let one = &one;
            let mut work = move || {
                let mut buf = Vec::with_capacity(n);
                for (j, s) in chunk.iter_mut().enumerate() {
                    *s = one(chunk_no * per + j, &mut buf);
                }
            };
            if threads <= 1 {
                work();
            } else {
                scope.spawn(work);
            }
        }
    });
    stats
}

/// Arithmetic mean with an exactly rounded sum; 0 for an empty slice. The default
/// bootstrap statistic.
pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() { 0.0 } else { textfmt::fsum(values.iter().copied()) / values.len() as f64 }
}

/// Realized win rate minus the average entry price (the market-implied win probability).
///
/// Positive edge means the contracts bought won more often than their price implied,
/// which is what the favorite-longshot bias predicts for underpriced favorites.
pub fn edge(positions: &[Position]) -> f64 {
    if positions.is_empty() {
        return 0.0;
    }
    let wins = positions.iter().filter(|p| p.pnl + p.fee > 0.0).count();
    let n = positions.len() as f64;
    wins as f64 / n - textfmt::fsum(positions.iter().map(|p| p.entry_price)) / n
}

// ---------------------------------------------------------------------------
// Parameter sweeps

fn is_numeric_param(name: &str) -> bool {
    !matches!(name, "side" | "role")
}

/// `x` rounded down to an integer; NaN and infinity are errors.
fn floor_to_int(x: f64) -> Result<i64> {
    if x.is_nan() {
        bail!("cannot convert float NaN to integer");
    }
    if x.is_infinite() {
        bail!("cannot convert float infinity to integer");
    }
    Ok(x.floor() as i64)
}

/// Parse `name=values` where values are `a,b,c` or an inclusive range `start:stop:step`.
pub fn parse_param(spec: &str) -> Result<(String, Vec<ParamValue>)> {
    let (name, raw) = match spec.split_once('=') {
        Some((n, r)) if !r.trim().is_empty() => (n, r),
        _ => bail!("invalid --param {}: expected name=values", textfmt::quote_str(spec)),
    };
    let name = name.trim().replace('-', "_");
    if !SWEEPABLE.contains(&name.as_str()) {
        bail!("cannot sweep {}: choose from {}", textfmt::quote_str(&name), SWEEPABLE.join(", "));
    }
    let numeric = is_numeric_param(&name);
    if raw.contains(':') {
        if !numeric {
            bail!("{name} takes a comma-separated list, not a range");
        }
        let parts: Vec<Option<f64>> = raw.split(':').map(textfmt::parse_float).collect();
        let [Some(start), Some(stop), Some(step)] = parts[..] else {
            bail!("invalid range {}: expected start:stop:step", textfmt::quote_str(raw));
        };
        if step <= 0.0 || stop < start {
            bail!("invalid range {}: need step > 0 and stop >= start", textfmt::quote_str(raw));
        }
        let count = floor_to_int((stop - start) / step + 1e-9)? + 1;
        let values = (0..count.max(0)).map(|i| ParamValue::Num(textfmt::round(start + i as f64 * step, 10))).collect();
        return Ok((name, values));
    }
    let mut values = Vec::new();
    for v in raw.split(',').filter(|v| !v.trim().is_empty()) {
        if numeric {
            match textfmt::parse_float(v) {
                Some(x) => values.push(ParamValue::Num(x)),
                None => bail!("invalid values for {name}: {}", textfmt::quote_str(raw)),
            }
        } else {
            values.push(ParamValue::Text(v.trim().to_uppercase()));
        }
    }
    if values.is_empty() {
        bail!("no values given for {name}");
    }
    Ok((name, values))
}

/// Set one sweepable field on a strategy.
pub fn set_param(s: &mut Strategy, name: &str, value: &ParamValue) -> Result<()> {
    match (name, value) {
        ("min_price", ParamValue::Num(v)) => s.min_price = *v,
        ("max_price", ParamValue::Num(v)) => s.max_price = *v,
        ("position_size", ParamValue::Num(v)) => s.position_size = *v,
        ("maker_fee_rate", ParamValue::Num(v)) => s.maker_fee_rate = *v,
        ("taker_fee_rate", ParamValue::Num(v)) => s.taker_fee_rate = *v,
        ("side", ParamValue::Text(v)) => s.side = v.clone(),
        ("role", ParamValue::Text(v)) => s.role = v.clone(),
        _ => bail!("cannot set {name} to {value}"),
    }
    Ok(())
}

/// Read one sweepable field back from a strategy.
pub fn get_param(s: &Strategy, name: &str) -> Option<ParamValue> {
    Some(match name {
        "min_price" => ParamValue::Num(s.min_price),
        "max_price" => ParamValue::Num(s.max_price),
        "position_size" => ParamValue::Num(s.position_size),
        "maker_fee_rate" => ParamValue::Num(s.maker_fee_rate),
        "taker_fee_rate" => ParamValue::Num(s.taker_fee_rate),
        "side" => ParamValue::Text(s.side.clone()),
        "role" => ParamValue::Text(s.role.clone()),
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct SweepRow {
    /// Swept parameter values, in the order the parameters were given.
    pub params: Vec<(String, ParamValue)>,
    pub total_trades: usize,
    pub total_pnl: f64,
    pub avg_return: f64,
    pub win_rate: f64,
    pub edge: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub avg_return_ci: Option<Interval>,
    pub final_equity: Option<f64>,
    pub skipped_trades: usize,
}

impl SweepRow {
    pub fn metric(&self, name: &str) -> Option<f64> {
        Some(match name {
            "avg_return" => self.avg_return,
            "sharpe" => self.sharpe,
            "edge" => self.edge,
            "total_pnl" => self.total_pnl,
            "win_rate" => self.win_rate,
            _ => return None,
        })
    }

    pub fn param(&self, name: &str) -> Option<&ParamValue> {
        self.params.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }
}

/// Every combination of the parameter values (cartesian product, first parameter slowest).
pub fn grid(params: &[(String, Vec<ParamValue>)]) -> Vec<Vec<(String, ParamValue)>> {
    let mut combos: Vec<Vec<(String, ParamValue)>> = vec![Vec::new()];
    for (name, values) in params {
        combos = combos
            .into_iter()
            .flat_map(|prefix| {
                values.iter().map(move |v| {
                    let mut c = prefix.clone();
                    c.push((name.clone(), v.clone()));
                    c
                })
            })
            .collect();
    }
    combos
}

/// Backtest every combination of `params` on top of `base`. Trades are fetched once per
/// side with the widest band in the grid and every variant is simulated in memory.
pub fn sweep(
    source: &dyn TradeSource,
    base: &Strategy,
    params: &[(String, Vec<ParamValue>)],
    race_filter: &str,
    bootstrap: usize,
    log: Option<&dyn Fn(&str)>,
) -> Result<Vec<SweepRow>> {
    let window = base.window()?;
    let mut by_side: Vec<(String, Vec<Strategy>)> = Vec::new();
    for combo in grid(params) {
        let mut s = base.clone();
        for (name, value) in &combo {
            set_param(&mut s, name, value)?;
        }
        s.side = if s.side.is_empty() { "YES".into() } else { s.side.to_uppercase() };
        if s.min_price > s.max_price {
            continue;
        }
        match by_side.iter_mut().find(|(side, _)| *side == s.side) {
            Some((_, list)) => list.push(s),
            None => by_side.push((s.side.clone(), vec![s])),
        }
    }

    let mut rows = Vec::new();
    for (side, strategies) in &by_side {
        let lo = strategies.iter().map(|s| s.min_price).reduce(|a, b| if b < a { b } else { a }).unwrap_or(0.0);
        let hi = strategies.iter().map(|s| s.max_price).reduce(|a, b| if b > a { b } else { a }).unwrap_or(0.0);
        if let Some(log) = log {
            log(&format!(
                "fetching {side} trades for YES probability [{}, {}]...\n",
                textfmt::fixed(lo, 2),
                textfmt::fixed(hi, 2)
            ));
        }
        let trades = source.backtest_trades(lo, hi, side, race_filter, &window)?;
        if let Some(log) = log {
            log(&format!("  {} trades; simulating {} variants\n", trades.len(), strategies.len()));
        }
        for s in strategies {
            let subset: Vec<BacktestTrade> = trades.iter().filter(|t| in_band(t, s)).cloned().collect();
            let (metrics, positions) = run_variant(&subset, s);
            let returns: Vec<f64> = positions.iter().map(|p| p.return_pct).collect();
            rows.push(SweepRow {
                params: params.iter().map(|(name, _)| (name.clone(), get_param(s, name).expect("sweepable"))).collect(),
                total_trades: metrics.total_trades,
                total_pnl: metrics.total_pnl,
                avg_return: metrics.avg_return_pct,
                win_rate: metrics.win_rate,
                edge: edge(&positions),
                sharpe: metrics.sharpe_ratio,
                max_drawdown: metrics.max_drawdown,
                avg_return_ci: bootstrap_ci(&returns, mean, bootstrap, 0.05, 0),
                final_equity: s.enforce_capital.then_some(metrics.final_equity),
                skipped_trades: metrics.skipped_trades,
            });
        }
    }
    Ok(rows)
}

fn in_band(t: &BacktestTrade, s: &Strategy) -> bool {
    let eps = 1e-12;
    if s.side == "NO" {
        return 1.0 - s.max_price - eps <= t.price && t.price <= 1.0 - s.min_price + eps;
    }
    s.min_price - eps <= t.price && t.price <= s.max_price + eps
}

fn run_variant(trades: &[BacktestTrade], s: &Strategy) -> (crate::PortfolioMetrics, Vec<Position>) {
    let positions = simulate(trades, s, None, None).expect("no cancel flag");
    if !s.enforce_capital {
        return (compute_portfolio_metrics(&positions, 0.0), positions);
    }
    let ledger = apply_capital(&positions, s.initial_capital);
    let mut metrics = compute_portfolio_metrics(&ledger.accepted, s.initial_capital);
    metrics.skipped_trades = ledger.skipped;
    metrics.peak_exposure = ledger.peak_exposure;
    (metrics, ledger.accepted)
}

// ---------------------------------------------------------------------------
// Rolling windows

#[derive(Debug, Clone, PartialEq)]
pub struct RollingRow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub total_trades: usize,
    pub total_pnl: f64,
    pub avg_return: f64,
    pub win_rate: f64,
    pub edge: f64,
    pub sharpe: f64,
}

fn days_delta(days: i64) -> Result<TimeDelta> {
    if days.abs() > 999_999_999 {
        bail!("days={days}; must have magnitude <= 999999999");
    }
    Ok(TimeDelta::days(days))
}

/// Metrics over sliding windows of entry time, aligned to UTC days, to see whether the
/// edge is stable or fading.
pub fn rolling(positions: &[Position], window_days: i64, step_days: i64) -> Result<Vec<RollingRow>> {
    if window_days <= 0 || step_days <= 0 {
        bail!("window and step must be positive");
    }
    // Positions usually arrive in entry order already; then windows are plain slices.
    let in_order = positions.iter().all(|p| p.entered_at.is_some())
        && positions.windows(2).all(|w| w[0].entered_at <= w[1].entered_at);
    let sorted_copy: Vec<Position>;
    let dated: &[Position] = if in_order {
        positions
    } else {
        let mut v: Vec<Position> = positions.iter().filter(|p| p.entered_at.is_some()).cloned().collect();
        v.sort_by_key(|p| p.entered_at);
        sorted_copy = v;
        &sorted_copy
    };
    let (Some(first), Some(last)) = (dated.first(), dated.last()) else {
        return Ok(Vec::new());
    };
    let window = days_delta(window_days)?;
    let step = days_delta(step_days)?;
    let first = first.entered_at.expect("dated").date_naive().and_hms_opt(0, 0, 0).expect("midnight").and_utc();
    let last = last.entered_at.expect("dated");
    let entered: Vec<DateTime<Utc>> = dated.iter().map(|p| p.entered_at.expect("dated")).collect();

    let mut rows = Vec::new();
    let mut start = first;
    while start <= last {
        let end = start + window;
        if end.year() > 9999 {
            bail!("date value out of range");
        }
        let lo = entered.partition_point(|t| *t < start);
        let hi = entered.partition_point(|t| *t < end);
        if hi > lo {
            let chunk = &dated[lo..hi];
            let m = compute_portfolio_metrics(chunk, 0.0);
            rows.push(RollingRow {
                start,
                end,
                total_trades: m.total_trades,
                total_pnl: m.total_pnl,
                avg_return: m.avg_return_pct,
                win_rate: m.win_rate,
                edge: edge(chunk),
                sharpe: m.sharpe_ratio,
            });
        }
        start += step;
    }
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Output

fn iso_date(t: DateTime<Utc>) -> String {
    t.date_naive().format("%Y-%m-%d").to_string()
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

pub fn write_sweep_csv(path: &Path, rows: &[SweepRow], param_names: &[String]) -> Result<()> {
    let mut out = String::new();
    let mut header: Vec<String> = param_names.to_vec();
    header.extend(
        [
            "total_trades",
            "total_pnl",
            "avg_return",
            "avg_return_ci_low",
            "avg_return_ci_high",
            "win_rate",
            "edge",
            "sharpe",
            "max_drawdown",
            "final_equity",
            "skipped_trades",
        ]
        .map(String::from),
    );
    out.push_str(&textfmt::csv_row(&header));
    for r in rows {
        let mut fields: Vec<String> =
            param_names.iter().map(|n| r.param(n).map(ToString::to_string).unwrap_or_default()).collect();
        let ci = r.avg_return_ci;
        fields.extend([
            r.total_trades.to_string(),
            textfmt::fixed(r.total_pnl, 4),
            textfmt::fixed(r.avg_return, 6),
            ci.map_or(String::new(), |c| textfmt::fixed(c.low, 6)),
            ci.map_or(String::new(), |c| textfmt::fixed(c.high, 6)),
            textfmt::fixed(r.win_rate, 6),
            textfmt::fixed(r.edge, 6),
            textfmt::fixed(r.sharpe, 6),
            textfmt::fixed(r.max_drawdown, 6),
            r.final_equity.map_or(String::new(), |v| textfmt::fixed(v, 2)),
            r.skipped_trades.to_string(),
        ]);
        out.push_str(&textfmt::csv_row(&fields));
    }
    write_text(path, &out)
}

pub fn write_rolling_csv(path: &Path, rows: &[RollingRow]) -> Result<()> {
    let mut out = textfmt::csv_row(&[
        "window_start",
        "window_end",
        "total_trades",
        "total_pnl",
        "avg_return",
        "win_rate",
        "edge",
        "sharpe",
    ]);
    for r in rows {
        out.push_str(&textfmt::csv_row(&[
            iso_date(r.start),
            iso_date(r.end),
            r.total_trades.to_string(),
            textfmt::fixed(r.total_pnl, 4),
            textfmt::fixed(r.avg_return, 6),
            textfmt::fixed(r.win_rate, 6),
            textfmt::fixed(r.edge, 6),
            textfmt::fixed(r.sharpe, 6),
        ]));
    }
    write_text(path, &out)
}

/// Sort order for heatmap axes: numbers first by value, then text.
fn cmp_param(a: &ParamValue, b: &ParamValue) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (ParamValue::Num(x), ParamValue::Num(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (ParamValue::Num(_), ParamValue::Text(_)) => Ordering::Less,
        (ParamValue::Text(_), ParamValue::Num(_)) => Ordering::Greater,
        (ParamValue::Text(x), ParamValue::Text(y)) => x.cmp(y),
    }
}

fn axis_values(rows: &[SweepRow], param: &str) -> Vec<ParamValue> {
    let mut vals: Vec<ParamValue> = Vec::new();
    for r in rows {
        if let Some(v) = r.param(param)
            && !vals.contains(v)
        {
            vals.push(v.clone());
        }
    }
    vals.sort_by(cmp_param);
    vals
}

/// Self-contained HTML heatmap of `metric` over two swept parameters.
pub fn write_sweep_heatmap(
    path: &Path,
    rows: &[SweepRow],
    row_param: &str,
    col_param: &str,
    metric: &str,
    title: &str,
) -> Result<()> {
    if !METRICS.contains(&metric) {
        bail!("unknown metric {}", textfmt::quote_str(metric));
    }
    let value = |r: &SweepRow| r.metric(metric).unwrap_or(f64::NAN);
    let row_vals = axis_values(rows, row_param);
    let col_vals = axis_values(rows, col_param);
    let scale = rows
        .iter()
        .filter(|r| value(r).is_finite() && r.total_trades > 0)
        .map(|r| value(r).abs())
        .reduce(|a, b| if b > a { b } else { a })
        .unwrap_or(1.0);
    let scale = if scale == 0.0 { 1.0 } else { scale };

    let fmt = |v: f64| match metric {
        "avg_return" | "edge" | "win_rate" => format!("{}%", textfmt::fixed(v * 100.0, 2)),
        "total_pnl" => format!("${}", textfmt::grouped0(v)),
        _ => textfmt::fixed(v, 3),
    };

    let head: String = col_vals.iter().map(|c| format!("<th>{}</th>", textfmt::html_escape(&c.to_string()))).collect();
    let mut body = Vec::new();
    for rv in &row_vals {
        let mut tds = String::new();
        for cv in &col_vals {
            // When a cell repeats, the last row wins.
            let cell = rows.iter().rev().find(|r| r.param(row_param) == Some(rv) && r.param(col_param) == Some(cv));
            let Some(r) = cell.filter(|r| r.total_trades > 0) else {
                tds.push_str("<td class=\"empty\">\u{b7}</td>");
                continue;
            };
            let v = value(r);
            if !v.is_finite() {
                tds.push_str("<td class=\"empty\">n/a</td>");
                continue;
            }
            let v_color = if metric == "win_rate" { v - 0.5 } else { v };
            let ratio = v_color.abs() / scale;
            let alpha = (if 1.0 < ratio { 1.0 } else { ratio }) * 0.85 + 0.05;
            let tone = if v_color >= 0.0 { "var(--pos)" } else { "var(--neg)" };
            let ci = match r.avg_return_ci {
                Some(c) if metric == "avg_return" => format!(
                    "<span>95% CI {}% to {}%</span>",
                    textfmt::fixed(c.low * 100.0, 1),
                    textfmt::fixed(c.high * 100.0, 1)
                ),
                _ => String::new(),
            };
            tds.push_str(&format!(
                "<td style=\"--tone:{tone};--a:{}\"><b>{}</b><span>{} trades</span>{ci}</td>",
                textfmt::fixed(alpha, 3),
                fmt(v),
                r.total_trades
            ));
        }
        body.push(format!("<tr><th>{}</th>{tds}</tr>", textfmt::html_escape(&rv.to_string())));
    }

    let mut page = HEATMAP_TEMPLATE.to_string();
    for (key, value) in [
        ("@@TITLE@@", textfmt::html_escape(title)),
        ("@@METRIC@@", textfmt::html_escape(&metric.replace('_', " "))),
        ("@@ROW@@", textfmt::html_escape(row_param)),
        ("@@COL@@", textfmt::html_escape(col_param)),
        ("@@HEAD@@", head),
        ("@@BODY@@", body.join("\n")),
    ] {
        page = page.replace(key, &value);
    }
    write_text(path, &page)
}

const HEATMAP_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>@@TITLE@@</title>
<style>
:root { --bg: #fafafa; --fg: #1d1d1f; --muted: #6b6b70; --line: #e2e2e6;
        --pos: 39, 174, 96; --neg: 231, 76, 60; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #151517; --fg: #ececf0; --muted: #9a9aa3; --line: #2b2b30; }
}
body { margin: 0; padding: 24px 16px; background: var(--bg); color: var(--fg);
       font: 14px/1.4 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
h1 { font-size: 1.3rem; margin: 0 0 4px; }
p { color: var(--muted); margin: 0 0 20px; }
.wrap { overflow-x: auto; }
table { border-collapse: collapse; }
th, td { border: 1px solid var(--line); padding: 8px 10px; text-align: center; white-space: nowrap; }
thead th { font-weight: 600; }
tbody th { text-align: right; font-weight: 600; }
td { background: rgba(var(--tone), var(--a)); min-width: 92px; }
td b { display: block; font-size: 0.95rem; }
td span { display: block; font-size: 0.72rem; color: var(--fg); opacity: 0.75; }
td.empty { background: transparent; color: var(--muted); }
.corner { color: var(--muted); font-weight: 400; }
</style>
</head>
<body>
<h1>@@TITLE@@</h1>
<p>@@METRIC@@ by @@ROW@@ (rows) and @@COL@@ (columns). Green is better, red is worse.</p>
<div class="wrap">
<table>
<thead><tr><th class="corner">@@ROW@@ \ @@COL@@</th>@@HEAD@@</tr></thead>
<tbody>
@@BODY@@
</tbody>
</table>
</div>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_do_not_depend_on_thread_count() {
        let values: Vec<f64> = (0..3000).map(|i| ((i * 7919) % 1000) as f64 / 1000.0 - 0.4).collect();
        let serial = resample_stats(&values, mean, 37, 9, 1);
        assert_eq!(serial, resample_stats(&values, mean, 37, 9, 4));
        assert_eq!(serial, resample_stats(&values, mean, 37, 9, 64));
        assert_ne!(serial, resample_stats(&values, mean, 37, 10, 1));
    }

    #[test]
    fn rng_is_uniform_enough() {
        let mut rng = Rng::new(0);
        let draws: Vec<f64> = (0..100_000).map(|_| rng.next_f64()).collect();
        assert!(draws.iter().all(|u| (0.0..1.0).contains(u)));
        let m = mean(&draws);
        assert!((m - 0.5).abs() < 0.005, "{m}");
    }
}
