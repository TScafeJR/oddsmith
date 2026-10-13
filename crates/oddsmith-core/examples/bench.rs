//! Times the hot path on synthetic data: simulate, portfolio metrics, and 200 bootstrap
//! resamples of the returns.
//!
//!     cargo run --release -p oddsmith-core --example bench [-- TRADES]

use std::time::Instant;

use chrono::{TimeDelta, TimeZone, Utc};
use oddsmith_core::research::{Rng, bootstrap_ci, mean};
use oddsmith_core::strategy::generate_buckets;
use oddsmith_core::{BacktestTrade, Strategy, compute_portfolio_metrics, simulate};

fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    let mut rng = Rng::new(42);
    let t0 = Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap();
    let trades: Vec<BacktestTrade> = (0..n)
        .map(|i| {
            let price = 0.50 + 0.45 * rng.next_f64();
            let entered = t0 + TimeDelta::seconds((i as i64) * 60);
            let hold = TimeDelta::seconds((rng.next_f64() * 200.0 * 86_400.0) as i64);
            BacktestTrade {
                market_id: format!("m{}", i % 2000),
                price: (price * 1e6).round() / 1e6,
                resolution: if rng.next_f64() < price + 0.02 { "Yes" } else { "No" }.into(),
                traded_at: entered,
                end_date: entered + hold,
            }
        })
        .collect();
    let strategy = Strategy {
        name: "maker_favorites".into(),
        side: "YES".into(),
        role: "MAKER".into(),
        min_price: 0.50,
        max_price: 0.95,
        position_size: 100.0,
        initial_capital: 10000.0,
        maker_fee_rate: 0.0175,
        buckets: generate_buckets(0.50, 0.95, 5.0),
        ..Strategy::default()
    };

    let start = Instant::now();
    let positions = simulate(&trades, &strategy, None, None).unwrap();
    let t_sim = start.elapsed();

    let start = Instant::now();
    let metrics = compute_portfolio_metrics(&positions, 0.0);
    let t_metrics = start.elapsed();

    let returns: Vec<f64> = positions.iter().map(|p| p.return_pct).collect();
    let start = Instant::now();
    let ci = bootstrap_ci(&returns, mean, 200, 0.05, 0).unwrap();
    let t_boot = start.elapsed();

    println!("{n} trades, {} positions, {} buckets", positions.len(), metrics.bucket_breakdown.len());
    println!("avg return {:.6}, 95% CI [{:.6}, {:.6}]", metrics.avg_return_pct, ci.low, ci.high);
    println!("simulate:        {:>8.3} s", t_sim.as_secs_f64());
    println!("metrics:         {:>8.3} s", t_metrics.as_secs_f64());
    println!("bootstrap (200): {:>8.3} s", t_boot.as_secs_f64());
}
