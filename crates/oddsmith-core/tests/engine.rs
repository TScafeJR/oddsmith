//! Pure-engine tests: fixture trades whose results can be worked out by hand.
//! (The `Engine::run` cases live in oddsmith-backtest's tests.)

mod common;

use std::path::PathBuf;

use common::{trade, yes_strategy};
use oddsmith_core::strategy::load_strategy;
use oddsmith_core::{BacktestTrade, Strategy, compute_portfolio_metrics, simulate};

// Three YES entries of 100 contracts each:
//   0.60 -> resolves Yes: +40     (return 40/60)
//   0.70 -> resolves No:  -70     (return -1)
//   0.80 -> resolves Yes: +20     (return 20/80)
fn fixture() -> Vec<BacktestTrade> {
    vec![trade("m1", 0.60, "Yes", 0, 30.0), trade("m2", 0.70, "No", 1, 10.0), trade("m3", 0.80, "Yes", 2, 100.0)]
}

fn run(trades: &[BacktestTrade], s: &Strategy) -> Vec<oddsmith_core::Position> {
    simulate(trades, s, None, None).unwrap()
}

#[test]
fn pnl_returns_and_win_rate() {
    let positions = run(&fixture(), &yes_strategy());
    for (p, want) in positions.iter().zip([40.0, -70.0, 20.0]) {
        assert_approx!(p.pnl, want);
    }
    for (p, want) in positions.iter().zip([40.0 / 60.0, -1.0, 0.25]) {
        assert_approx!(p.return_pct, want);
    }
    let m = compute_portfolio_metrics(&positions, 0.0);
    assert_eq!(m.total_trades, 3);
    assert_approx!(m.total_pnl, -10.0);
    assert_approx!(m.win_rate, 2.0 / 3.0);
    assert_approx!(m.avg_hold_days, 140.0 / 3.0);
}

#[test]
fn max_drawdown_is_peak_to_trough_over_peak() {
    // Cumulative PnL: 40, -30, -10. Peak 40, trough -30, so drawdown = 70 / 40.
    let m = compute_portfolio_metrics(&run(&fixture(), &yes_strategy()), 0.0);
    assert_approx!(m.max_drawdown, 1.75);
}

#[test]
fn sharpe_is_annualized_excess_return() {
    let m = compute_portfolio_metrics(&run(&fixture(), &yes_strategy()), 0.0);
    let holds = [30.0, 10.0, 100.0];
    let returns = [40.0 / 60.0, -1.0, 0.25];
    let excess: Vec<f64> = returns.iter().zip(holds).map(|(r, h)| r - 0.04 * h / 365.0).collect();
    let mean = excess.iter().sum::<f64>() / 3.0;
    let sd = (excess.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / 2.0).sqrt();
    let expected = mean / sd * (365.0 / (holds.iter().sum::<f64>() / 3.0)).sqrt();
    assert_approx!(m.sharpe_ratio, expected);
}

#[test]
fn taker_fee_reduces_pnl_and_raises_cost_basis() {
    let s = Strategy { role: "TAKER".into(), taker_fee_rate: 0.07, ..yes_strategy() };
    let positions = run(&fixture()[..1], &s);
    let fee = 0.07 * 0.6 * 0.4 * 100.0; // rate * p * (1 - p) * contracts
    assert_approx!(positions[0].fee, fee);
    assert_approx!(positions[0].pnl, 40.0 - fee);
    assert_approx!(positions[0].return_pct, (40.0 - fee) / (60.0 + fee));
}

#[test]
fn maker_role_ignores_taker_rate() {
    let s = Strategy { role: "MAKER".into(), taker_fee_rate: 0.07, ..yes_strategy() };
    assert_eq!(run(&fixture()[..1], &s)[0].fee, 0.0);
}

#[test]
fn no_side_buckets_by_yes_probability() {
    // Buying the NO token at 0.20 means YES was priced at 0.80.
    let s = Strategy { side: "NO".into(), ..yes_strategy() };
    let positions = run(&[trade("m1", 0.20, "No", 0, 5.0)], &s);
    assert_approx!(positions[0].pnl, 80.0);
    assert_eq!(positions[0].bucket, "80-90");
}

#[test]
fn buckets_sort_numerically() {
    let s = Strategy { min_price: 0.0, buckets: Some(vec![0.0, 5.0, 10.0, 30.0, 40.0]), ..yes_strategy() };
    let trades = [trade("a", 0.35, "Yes", 0, 1.0), trade("b", 0.07, "No", 0, 1.0)];
    let m = compute_portfolio_metrics(&run(&trades, &s), 0.0);
    let labels: Vec<&str> = m.bucket_breakdown.iter().map(|b| b.bucket.as_str()).collect();
    assert_eq!(labels, ["5-10", "30-40"]);
}

#[test]
fn hold_days_never_negative() {
    let positions = run(&[trade("m1", 0.6, "Yes", 0, -3.0)], &yes_strategy());
    assert_eq!(positions[0].hold_days, 0.0);
}

#[test]
fn simulate_honours_cancel() {
    let cancel = std::sync::atomic::AtomicBool::new(true);
    let lines = std::cell::RefCell::new(Vec::new());
    let log = |m: &str| lines.borrow_mut().push(m.to_string());
    let err = simulate(&fixture(), &yes_strategy(), Some(&cancel), Some(&log)).unwrap_err();
    assert_eq!(err, oddsmith_core::Cancelled);
    assert_eq!(lines.borrow().as_slice(), ["backtest cancelled after 0/3 trades\n"]);
}

fn strategies_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../strategies")
}

#[test]
fn strategy_files_load() {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(strategies_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    for path in paths {
        let s = load_strategy(&path).unwrap();
        assert!(s.side == "YES" || s.side == "NO", "{}", path.display());
        assert!(s.buckets.as_ref().is_some_and(|b| b.len() >= 2), "{}", path.display());
    }
}

#[test]
fn bucket_width_generates_boundaries() {
    let s = load_strategy(strategies_dir().join("maker_favorites.json")).unwrap();
    assert_eq!(s.buckets, Some(vec![50.0, 55.0, 60.0, 65.0, 70.0, 75.0, 80.0, 85.0, 90.0, 95.0]));
}
