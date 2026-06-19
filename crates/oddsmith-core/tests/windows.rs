//! Date windows, capital limits, and equity drawdown.
//! (The `Engine::run` case lives in oddsmith-backtest's tests.)

mod common;

use chrono::{TimeZone, Utc};
use common::trade;
use oddsmith_core::strategy::{load_strategy, parse_window};
use oddsmith_core::{BacktestTrade, Strategy, apply_capital, compute_portfolio_metrics, simulate};

fn strategy() -> Strategy {
    Strategy { initial_capital: 150.0, ..common::yes_strategy() }
}

// Costs 60, 70, 80 (100 contracts each); m1 wins, m2 loses, m3 wins.
fn trades() -> Vec<BacktestTrade> {
    vec![
        trade("m1", 0.60, "Yes", 0, 30.0), // resolves day 30, pays 100
        trade("m2", 0.70, "No", 1, 10.0),  // resolves day 11, pays 0
        trade("m3", 0.80, "Yes", 2, 100.0),
    ]
}

fn ids(ps: &[oddsmith_core::Position]) -> Vec<&str> {
    ps.iter().map(|p| p.market_id.as_str()).collect()
}

#[test]
fn window_end_date_is_inclusive() {
    let w = parse_window("2024-01-01", "2024-01-31", "").unwrap();
    assert_eq!(w.start, Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()));
    assert_eq!(w.end, Some(Utc.with_ymd_and_hms(2024, 2, 1, 0, 0, 0).unwrap()));
    assert!(w.contains(Utc.with_ymd_and_hms(2024, 1, 31, 23, 59, 0).unwrap()));
    assert!(!w.contains(Utc.with_ymd_and_hms(2024, 2, 1, 0, 0, 0).unwrap()));
    assert_eq!(w.describe(), "2024-01-01 to 2024-01-31 (by traded)");
}

#[test]
fn open_ended_window() {
    let w = parse_window("", "", "").unwrap();
    assert!(!w.is_set());
    assert_eq!(w.describe(), "all time");
    assert_eq!(parse_window("2024-06-01", "", "").unwrap().end, None);
}

#[test]
fn invalid_windows() {
    let cases = [
        ("2024-13-01", "", "", "expected YYYY-MM-DD"),
        ("2024-02-01", "2024-01-01", "", "is after"),
        ("", "", "created", "must be traded or resolved"),
    ];
    for (start, end, by, message) in cases {
        let err = parse_window(start, end, by).unwrap_err().to_string();
        assert!(err.contains(message), "{err:?} should contain {message:?}");
    }
    assert_eq!(
        parse_window("2024-13-01", "", "").unwrap_err().to_string(),
        "invalid start_date \"2024-13-01\": expected YYYY-MM-DD"
    );
    assert_eq!(
        parse_window("", "", "Created").unwrap_err().to_string(),
        "invalid window_by \"Created\": must be traded or resolved"
    );
}

#[test]
fn capital_skips_entries_when_cash_runs_out() {
    let ledger = apply_capital(&simulate(&trades(), &strategy(), None, None).unwrap(), 150.0);
    // m1 (60) and m2 (70) fit; m3 (80) doesn't, with only 20 left and nothing resolved yet.
    assert_eq!(ids(&ledger.accepted), ["m1", "m2"]);
    assert_eq!(ledger.skipped, 1);
    assert_approx!(ledger.peak_exposure, 130.0);
    assert_approx!(ledger.final_cash, 150.0 - 130.0 + 100.0);
}

#[test]
fn capital_is_recycled_when_markets_resolve() {
    let mut ts = trades();
    ts[2] = trade("m3", 0.80, "Yes", 31, 100.0); // enters after m1 pays out
    let ledger = apply_capital(&simulate(&ts, &strategy(), None, None).unwrap(), 150.0);
    assert_eq!(ids(&ledger.accepted), ["m1", "m2", "m3"]);
    assert_eq!(ledger.skipped, 0);
}

#[test]
fn equity_drawdown_follows_resolution_order() {
    let positions = apply_capital(&simulate(&trades(), &strategy(), None, None).unwrap(), 150.0).accepted;
    let m = compute_portfolio_metrics(&positions, 150.0);
    // m2 resolves first (150 -> 80), then m1 (80 -> 120): drawdown 70 / 150.
    assert_approx!(m.max_drawdown, 70.0 / 150.0);
    assert_approx!(m.final_equity, 120.0);
    assert_approx!(m.return_on_capital, -30.0 / 150.0);
}

#[test]
fn strategy_file_window_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    std::fs::write(
        &path,
        r#"{"side": "YES", "min_price": 0.5, "max_price": 0.9, "start_date": "2022-01-01", "end_date": "2022-12-31", "enforce_capital": true}"#,
    )
    .unwrap();
    let s = load_strategy(&path).unwrap();
    assert!(s.enforce_capital);
    assert_eq!(s.window().unwrap().describe(), "2022-01-01 to 2022-12-31 (by traded)");

    std::fs::write(&path, r#"{"enforce_capital": "yes"}"#).unwrap();
    let err = load_strategy(&path).unwrap_err().to_string();
    assert!(err.contains("expected boolean"), "{err}");
    assert_eq!(err, "parsing strategy file: invalid value for field \"enforce_capital\": expected boolean, got string");

    std::fs::write(&path, r#"{"start_date": "Jan 1"}"#).unwrap();
    let err = load_strategy(&path).unwrap_err().to_string();
    assert!(err.contains("YYYY-MM-DD"), "{err}");

    let missing = dir.path().join("nope.json");
    let err = load_strategy(&missing).unwrap_err().to_string();
    assert_eq!(err, format!("reading strategy file: {}: No such file or directory (os error 2)", missing.display()));
}
