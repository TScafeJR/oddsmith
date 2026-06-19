//! Fixture helpers shared by the integration tests.
#![allow(dead_code)]

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use oddsmith_core::{BacktestTrade, Strategy};

pub fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()
}

/// A trade entered `day` days after 2024-01-01 that resolves `hold_days` later.
pub fn trade(market: &str, price: f64, resolution: &str, day: i64, hold_days: f64) -> BacktestTrade {
    let entered = t0() + TimeDelta::days(day);
    let hold = TimeDelta::microseconds((hold_days * 86_400_000_000.0).round() as i64);
    BacktestTrade {
        market_id: market.into(),
        price,
        resolution: resolution.into(),
        traded_at: entered,
        end_date: entered + hold,
    }
}

/// YES maker strategy on 0.50-0.95 with 100-contract positions.
pub fn yes_strategy() -> Strategy {
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

pub fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1e-6)
}

#[macro_export]
macro_rules! assert_approx {
    ($a:expr, $b:expr) => {{
        let (a, b): (f64, f64) = ($a, $b);
        assert!($crate::common::approx(a, b), "{} = {a}, want {b}", stringify!($a));
    }};
}
