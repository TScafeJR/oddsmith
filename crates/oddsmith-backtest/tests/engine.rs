//! `Engine::run` cases for the engine and date windows, run through `run_strategy` with
//! an in-memory trade source.

use std::cell::RefCell;
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use oddsmith_backtest::{is_cancelled, run_strategy};
use oddsmith_core::research::TradeSource;
use oddsmith_core::{BacktestTrade, Strategy, Window, WindowBy};

fn trade(market: &str, price: f64, resolution: &str, day: i64, hold_days: i64) -> BacktestTrade {
    let entered = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap() + TimeDelta::days(day);
    BacktestTrade {
        market_id: market.into(),
        price,
        resolution: resolution.into(),
        traded_at: entered,
        end_date: entered + TimeDelta::days(hold_days),
    }
}

fn fixture() -> Vec<BacktestTrade> {
    vec![trade("m1", 0.60, "Yes", 0, 30), trade("m2", 0.70, "No", 1, 10), trade("m3", 0.80, "Yes", 2, 100)]
}

fn yes_strategy() -> Strategy {
    Strategy {
        name: "t".into(),
        side: "YES".into(),
        role: "MAKER".into(),
        min_price: 0.5,
        max_price: 0.95,
        position_size: 100.0,
        ..Strategy::default()
    }
}

/// (min_price, max_price, side, race_filter, window) of one fetch.
type Call = (f64, f64, String, String, Window);

#[derive(Default)]
struct Recording {
    trades: Vec<BacktestTrade>,
    calls: RefCell<Vec<Call>>,
}

impl TradeSource for Recording {
    fn backtest_trades(&self, lo: f64, hi: f64, side: &str, race: &str, w: &Window) -> Result<Vec<BacktestTrade>> {
        self.calls.borrow_mut().push((lo, hi, side.into(), race.into(), *w));
        Ok(self.trades.clone())
    }
}

fn quiet(_: &str) {}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn engine_run_uses_db_trades() {
    let db = Recording { trades: fixture(), ..Recording::default() };
    let result = run_strategy(&db, &yes_strategy(), "", None, &quiet).unwrap();
    assert!(close(result.metrics.total_pnl, -10.0));
    assert_eq!(result.positions.len(), 3);
}

#[test]
fn engine_run_with_no_trades_fails() {
    let err = run_strategy(&Recording::default(), &yes_strategy(), "", None, &quiet).unwrap_err();
    assert_eq!(err.to_string(), "no qualifying trades found in price range [0.50, 0.95]");
    assert!(!is_cancelled(&err));
}

#[test]
fn engine_run_honours_cancel() {
    let db = Recording { trades: fixture(), ..Recording::default() };
    let cancel = AtomicBool::new(true);
    let err = run_strategy(&db, &yes_strategy(), "", Some(&cancel), &quiet).unwrap_err();
    assert!(is_cancelled(&err));
    assert_eq!(err.to_string(), "backtest cancelled");
}

#[test]
fn engine_passes_window_and_applies_capital() {
    let db = Recording { trades: fixture(), ..Recording::default() };
    let strat = Strategy {
        initial_capital: 150.0,
        start_date: "2024-01-01".into(),
        end_date: "2024-03-31".into(),
        window_by: "resolved".into(),
        enforce_capital: true,
        ..yes_strategy()
    };
    let lines = RefCell::new(String::new());
    let log = |m: &str| lines.borrow_mut().push_str(m);
    let result = run_strategy(&db, &strat, "senate", None, &log).unwrap();

    let calls = db.calls.borrow();
    let (lo, hi, side, race, w) = &calls[0];
    assert_eq!((*lo, *hi, side.as_str(), race.as_str()), (0.5, 0.95, "YES", "senate"));
    let day = |m, d| -> Option<DateTime<Utc>> { Some(Utc.with_ymd_and_hms(2024, m, d, 0, 0, 0).unwrap()) };
    assert_eq!(w.start, day(1, 1));
    assert_eq!(w.end, day(4, 1));
    assert_eq!(w.by, WindowBy::Resolved);
    assert_eq!(result.metrics.total_trades, 2);
    assert_eq!(result.metrics.skipped_trades, 1);
    assert!(close(result.metrics.final_equity, 120.0));
    assert_eq!(
        lines.borrow().as_str(),
        "date window: 2024-01-01 to 2024-03-31 (by resolved)\n\
         fetching all matching trades in one query...\n\
         loaded 3 trades, building positions...\n\
         built 3 positions across 3 markets\n\
         capital limit $150: took 2 positions, skipped 1 for lack of cash (peak exposure $130.00)\n\
         computing portfolio metrics...\n"
    );
}

#[test]
fn engine_logs_fees() {
    let db = Recording { trades: fixture(), ..Recording::default() };
    let strat = Strategy { role: "taker".into(), taker_fee_rate: 0.07, ..yes_strategy() };
    let lines = RefCell::new(String::new());
    let log = |m: &str| lines.borrow_mut().push_str(m);
    run_strategy(&db, &strat, "", None, &log).unwrap();
    assert!(lines.borrow().starts_with("applying TAKER fees: 0.0700 \u{d7} p \u{d7} (1\u{2212}p) per contract\n"));
}
