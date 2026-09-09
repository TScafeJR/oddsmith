//! Parameter sweeps, rolling windows, bootstrap intervals,
//! and their output files.

mod common;

use std::cell::RefCell;

use anyhow::Result;
use common::trade;
use oddsmith_core::research::{
    ParamValue, TradeSource, bootstrap_ci, edge, mean, parse_param, rolling, sweep, write_rolling_csv, write_sweep_csv,
    write_sweep_heatmap,
};
use oddsmith_core::{BacktestTrade, Strategy, Window, simulate};

fn t(market: &str, price: f64, resolution: &str, day: i64) -> BacktestTrade {
    trade(market, price, resolution, day, 10.0)
}

fn yes_trades() -> Vec<BacktestTrade> {
    vec![t("m1", 0.60, "Yes", 0), t("m2", 0.70, "No", 1), t("m3", 0.80, "Yes", 2)]
}

fn no_trades() -> Vec<BacktestTrade> {
    vec![t("m4", 0.20, "No", 3), t("m5", 0.40, "Yes", 4)]
}

fn base() -> Strategy {
    Strategy { initial_capital: 10000.0, ..common::yes_strategy() }
}

fn num(v: f64) -> ParamValue {
    ParamValue::Num(v)
}

fn text(v: &str) -> ParamValue {
    ParamValue::Text(v.into())
}

#[test]
fn parse_param_range_is_inclusive_and_clean() {
    let (name, values) = parse_param("min_price=0.5:0.9:0.1").unwrap();
    assert_eq!(name, "min_price");
    assert_eq!(values, [0.5, 0.6, 0.7, 0.8, 0.9].map(num));
}

#[test]
fn parse_param_list_normalises_case_and_dashes() {
    assert_eq!(parse_param("side=yes,No").unwrap(), ("side".into(), vec![text("YES"), text("NO")]));
    assert_eq!(parse_param("taker-fee-rate=0,0.07").unwrap(), ("taker_fee_rate".into(), vec![num(0.0), num(0.07)]));
}

#[test]
fn parse_param_errors() {
    let cases = [
        ("start_date=2024", "cannot sweep"),
        ("min_price", "expected name=values"),
        ("min_price=0.9:0.5:0.1", "stop >= start"),
        ("min_price=0.5:0.9:0", "step > 0"),
        ("side=A:B:C", "comma-separated"),
        ("min_price=low,high", "invalid values"),
    ];
    for (spec, message) in cases {
        let err = parse_param(spec).unwrap_err().to_string();
        assert!(err.contains(message), "{spec}: {err:?} should contain {message:?}");
    }
    let err = |s: &str| parse_param(s).unwrap_err().to_string();
    assert_eq!(
        err("start_date=2024"),
        "cannot sweep 'start_date': choose from min_price, max_price, position_size, maker_fee_rate, taker_fee_rate, side, role"
    );
    assert_eq!(err("min_price"), "invalid --param 'min_price': expected name=values");
    assert_eq!(err("min_price=low,high"), "invalid values for min_price: 'low,high'");
    assert_eq!(err("min_price=1:2"), "invalid range '1:2': expected start:stop:step");
    assert_eq!(err("min_price=,"), "no values given for min_price");
}

#[test]
fn bootstrap_ci_is_seeded_and_brackets_the_mean() {
    let values = [0.1, -0.2, 0.3, 0.05, -0.1, 0.25, 0.0, 0.15];
    let a = bootstrap_ci(&values, mean, 500, 0.05, 1).unwrap();
    let b = bootstrap_ci(&values, mean, 500, 0.05, 1).unwrap();
    assert_eq!(a, b);
    let m = mean(&values);
    assert!(a.low < m && m < a.high, "{a:?} should bracket {m}");
}

#[test]
fn bootstrap_ci_edge_cases() {
    assert_eq!(bootstrap_ci(&[1.0], mean, 100, 0.05, 0), None);
    assert_eq!(bootstrap_ci(&[1.0, 2.0], mean, 0, 0.05, 0), None);
    let flat = bootstrap_ci(&[0.5; 10], mean, 50, 0.05, 0).unwrap();
    assert_eq!((flat.low, flat.high), (0.5, 0.5));
}

#[test]
fn bootstrap_ci_large_input() {
    // Large enough to run on several threads.
    let values: Vec<f64> = (0..5000).map(|i| ((i * 7919) % 1000) as f64 / 1000.0 - 0.4).collect();
    let a = bootstrap_ci(&values, mean, 200, 0.05, 3).unwrap();
    assert_eq!(a, bootstrap_ci(&values, mean, 200, 0.05, 3).unwrap());
    assert!(a.low < mean(&values) && mean(&values) < a.high);
}

#[test]
fn edge_is_win_rate_minus_entry_price() {
    let positions = simulate(&yes_trades(), &base(), None, None).unwrap();
    assert_approx!(edge(&positions), 2.0 / 3.0 - 0.7);
    assert_eq!(edge(&[]), 0.0);
}

#[derive(Default)]
struct StubDb {
    calls: RefCell<Vec<(f64, f64, String)>>,
}

impl TradeSource for StubDb {
    fn backtest_trades(&self, lo: f64, hi: f64, side: &str, _race: &str, _w: &Window) -> Result<Vec<BacktestTrade>> {
        self.calls.borrow_mut().push((lo, hi, side.to_string()));
        Ok(if side == "NO" { no_trades() } else { yes_trades() })
    }
}

fn params(specs: &[&str]) -> Vec<(String, Vec<ParamValue>)> {
    specs.iter().map(|s| parse_param(s).unwrap()).collect()
}

#[test]
fn sweep_fetches_once_per_side_and_filters_in_memory() {
    let db = StubDb::default();
    let rows = sweep(
        &db,
        &Strategy { max_price: 0.9, ..base() },
        &params(&["side=YES,NO", "min_price=0.5,0.65"]),
        "",
        0,
        None,
    )
    .unwrap();

    let mut sides: Vec<String> = db.calls.borrow().iter().map(|c| c.2.clone()).collect();
    sides.sort();
    assert_eq!(sides, ["NO", "YES"]);
    assert!(db.calls.borrow().iter().all(|c| (c.0, c.1) == (0.5, 0.9))); // widest band across the grid
    let row = |side: &str, min: f64| {
        rows.iter().find(|r| r.param("side") == Some(&text(side)) && r.param("min_price") == Some(&num(min))).unwrap()
    };
    assert_eq!(row("YES", 0.5).total_trades, 3);
    assert_eq!(row("YES", 0.65).total_trades, 2); // 0.60 drops out
    // NO tokens at 0.20 and 0.40 imply YES at 0.80 and 0.60; only 0.80 clears min 0.65.
    assert_eq!(row("NO", 0.5).total_trades, 2);
    assert_eq!(row("NO", 0.65).total_trades, 1);
    assert_approx!(row("YES", 0.5).total_pnl, -10.0);
}

#[test]
fn sweep_skips_inverted_bands_and_reports_capital() {
    let db = StubDb::default();
    let s = Strategy { max_price: 0.9, enforce_capital: true, initial_capital: 150.0, ..base() };
    let rows = sweep(&db, &s, &params(&["min_price=0.5,0.99"]), "", 50, None).unwrap();
    let mins: Vec<&ParamValue> = rows.iter().map(|r| r.param("min_price").unwrap()).collect();
    assert_eq!(mins, [&num(0.5)]);
    assert_eq!(rows[0].skipped_trades, 1);
    assert_approx!(rows[0].final_equity.unwrap(), 120.0);
    assert!(rows[0].avg_return_ci.is_some());
}

#[test]
fn rolling_windows_slide_over_entry_time() {
    let trades: Vec<BacktestTrade> =
        (0..6).map(|i| t(&format!("m{i}"), 0.6, if i % 2 == 1 { "Yes" } else { "No" }, i)).collect();
    let rows = rolling(&simulate(&trades, &base(), None, None).unwrap(), 3, 2).unwrap();
    let starts: Vec<String> = rows.iter().map(|r| r.start.date_naive().to_string()).collect();
    assert_eq!(starts, ["2024-01-01", "2024-01-03", "2024-01-05"]);
    assert_eq!(rows.iter().map(|r| r.total_trades).collect::<Vec<_>>(), [3, 3, 2]);
    assert!(rolling(&[], 0, 1).is_err());
}

#[test]
fn output_files() {
    let dir = tempfile::tempdir().unwrap();
    let db = StubDb::default();
    let rows = sweep(
        &db,
        &Strategy { max_price: 0.9, ..base() },
        &params(&["side=YES,NO", "min_price=0.5,0.65"]),
        "",
        20,
        None,
    )
    .unwrap();

    let csv_path = dir.path().join("s.csv");
    write_sweep_csv(&csv_path, &rows, &["side".into(), "min_price".into()]).unwrap();
    let text = std::fs::read_to_string(&csv_path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5);
    let header: Vec<&str> = lines[0].split(',').collect();
    let ci_col = header.iter().position(|h| *h == "avg_return_ci_low").unwrap();
    assert_ne!(lines[1].split(',').nth(ci_col).unwrap(), "");
    assert!(lines[1].starts_with("YES,0.5,3,"), "{}", lines[1]);

    let html_path = dir.path().join("s.html");
    write_sweep_heatmap(&html_path, &rows, "side", "min_price", "avg_return", "Sweep: <t>").unwrap();
    let page = std::fs::read_to_string(&html_path).unwrap();
    assert!(!page.contains("@@"));
    assert!(page.contains("Sweep: &lt;t&gt;"));
    assert_eq!(page.matches("<td").count(), 4);

    let roll_path = dir.path().join("r.csv");
    write_rolling_csv(&roll_path, &rolling(&simulate(&yes_trades(), &base(), None, None).unwrap(), 2, 1).unwrap())
        .unwrap();
    assert!(std::fs::read_to_string(&roll_path).unwrap().starts_with("window_start,window_end,total_trades"));
}

#[test]
fn rolling_aligns_to_utc_days() {
    // Every timestamp is UTC; windows must start at UTC midnight even when entries fall
    // late in the day.
    let mut trades = vec![t("m1", 0.6, "Yes", 0), t("m2", 0.6, "No", 1)];
    for tr in &mut trades {
        tr.traded_at += chrono::TimeDelta::hours(23);
        tr.end_date += chrono::TimeDelta::hours(23);
    }
    let rows = rolling(&simulate(&trades, &base(), None, None).unwrap(), 1, 1).unwrap();
    let starts: Vec<String> = rows.iter().map(|r| r.start.to_rfc3339()).collect();
    assert_eq!(starts, ["2024-01-01T00:00:00+00:00", "2024-01-02T00:00:00+00:00"]);
}
