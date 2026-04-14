//! Generate all analysis deliverables (CSV/JSON/HTML) into an output directory.

use std::fs;

use anyhow::{Context, Result, anyhow};
use chrono::NaiveDate;
use oddsmith_store::{Db, Num};
use postgres::Row;

use crate::benchmark::{BenchmarkRow, compute_polls_vs_market};
use crate::charts::{compute_calibration_chart_data, write_calibration_chart_html};
use crate::correlation::{CorrelationRow, compute_correlation_matrix};
use crate::json::{Json, JsonStyle, serialize};
use crate::overconfidence::{compute_overconfidence, write_overconfidence_csv, write_overconfidence_summary_json};
use crate::stats::{
    CalibrationRow, MakerTakerRow, MzRow, SpreadRow, compute_calibration_curve, compute_maker_taker_returns,
    compute_mz_data, compute_spread_liquidity,
};
use crate::textfmt::ff;
use crate::util::{join, make_dirs, os_err, pg, write_csv};

/// Generate all deliverables and write them to `output_dir`.
pub fn run_all(db: &Db, output_dir: &str, verbose: bool) -> Result<()> {
    make_dirs(output_dir).map_err(|e| anyhow!(os_err(&e, output_dir))).context("creating output dir")?;
    let path = |name: &str| join(output_dir, name);
    let step = |name: &str| {
        if verbose {
            println!("  generating {name}...");
        }
    };

    // 1. Calibration curve
    step("calibration_curve.csv");
    match compute_calibration_curve(db) {
        Ok(cal) => write_calibration_csv(&path("calibration_curve.csv"), &cal),
        Err(e) => println!("  warning: calibration curve: {e:#}"),
    }

    // 2. Maker/taker returns
    step("maker_taker_returns.csv");
    match compute_maker_taker_returns(db) {
        Ok(mt) => write_maker_taker_csv(&path("maker_taker_returns.csv"), &mt),
        Err(e) => println!("  warning: maker/taker returns: {e:#}"),
    }

    // 3. Polls vs market benchmark
    step("polls_vs_market.csv");
    match compute_polls_vs_market(db) {
        Ok(bench) => write_benchmark_csv(&path("polls_vs_market.csv"), &bench),
        Err(e) => println!("  warning: polls vs market: {e:#}"),
    }

    // 4. Backtest summary JSON
    step("backtest_summary.json");
    write_backtest_summary_json(db, &path("backtest_summary.json"));

    // 5. Cumulative PnL
    step("cumulative_pnl.csv");
    write_cumulative_pnl_csv(db, &path("cumulative_pnl.csv"));

    // 6. Correlation matrix
    step("correlation_matrix.csv");
    match compute_correlation_matrix(db) {
        Ok(corr) => write_correlation_csv(&path("correlation_matrix.csv"), &corr),
        Err(e) => println!("  warning: correlation matrix: {e:#}"),
    }

    // 7. MZ regression data
    step("mz_regression.csv");
    match compute_mz_data(db) {
        Ok(mz) => write_mz_csv(&path("mz_regression.csv"), &mz),
        Err(e) => println!("  warning: MZ data: {e:#}"),
    }

    // 8. Spread/liquidity
    step("spread_liquidity.csv");
    match compute_spread_liquidity(db) {
        Ok(sp) => write_spread_csv(&path("spread_liquidity.csv"), &sp),
        Err(e) => println!("  warning: spread/liquidity: {e:#}"),
    }

    // 9. Overconfidence metrics
    if verbose {
        println!("  generating overconfidence.csv + overconfidence_summary.json...");
    }
    match compute_overconfidence(db, "") {
        // Write errors are ignored (best-effort output).
        Ok(oc) => {
            let _ = write_overconfidence_csv(&path("overconfidence.csv"), &oc);
            let _ = write_overconfidence_summary_json(&path("overconfidence_summary.json"), &oc);
        }
        Err(e) => println!("  warning: overconfidence: {e:#}"),
    }

    // 10. Calibration chart (HTML)
    step("calibration_chart.html");
    match compute_calibration_chart_data(db, "", 0.04) {
        Ok(chart) => {
            let _ = write_calibration_chart_html(&path("calibration_chart.html"), &chart);
        }
        Err(e) => println!("  warning: calibration chart: {e:#}"),
    }

    println!("all outputs written to {output_dir}/");
    Ok(())
}

fn opt(s: &Option<String>) -> String {
    s.clone().unwrap_or_default()
}

fn write_calibration_csv(path: &str, rows: &[CalibrationRow]) {
    let out: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![r.bucket.clone(), ff(r.implied_prob_mid), r.num_contracts.to_string(), ff(r.realized_freq), ff(r.bias)]
        })
        .collect();
    let header = ["bucket", "implied_prob_midpoint", "n_contracts", "realized_frequency", "bias"];
    if let Err(e) = write_csv(path, &header, &out) {
        println!("  error creating {path}: {}", os_err(&e, path));
    }
}

fn write_maker_taker_csv(path: &str, rows: &[MakerTakerRow]) {
    let out: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                r.role.clone(),
                opt(&r.bucket),
                r.num_trades.to_string(),
                ff(r.avg_return),
                ff(r.win_rate),
                ff(r.std_dev),
            ]
        })
        .collect();
    let _ = write_csv(path, &["role", "bucket", "n_trades", "avg_return", "win_rate", "std_dev"], &out);
}

fn write_benchmark_csv(path: &str, rows: &[BenchmarkRow]) {
    let out: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                r.market_id.clone(),
                opt(&r.race_type),
                r.state.clone(),
                r.candidate.clone(),
                ff(r.poll_implied_prob),
                ff(r.market_price),
                r.actual_outcome.to_string(),
                ff(r.poll_error),
                ff(r.market_error),
                ff(r.bias),
                r.price_bucket.clone(),
            ]
        })
        .collect();
    let header = [
        "market_id",
        "race_type",
        "state",
        "candidate",
        "poll_implied_prob",
        "market_price",
        "actual_outcome",
        "poll_error",
        "market_error",
        "bias",
        "price_bucket",
    ];
    let _ = write_csv(path, &header, &out);
}

/// The latest backtest run as the summary object, or the warning text when it is missing
/// or has a NULL in a required column.
fn backtest_summary(db: &Db) -> Result<Json> {
    let row = db
        .query_one(
            "
		SELECT name, strategy, params, race_filter, total_trades, total_pnl,
			avg_return_pct, sharpe_ratio, win_rate, max_drawdown, results_json
		FROM backtest_runs ORDER BY id DESC LIMIT 1",
            &[],
        )
        .map_err(pg)?
        .ok_or_else(|| anyhow!("no rows in result set"))?;
    // Only params (2) and results_json (10) may be NULL; any other NULL is an error.
    let text = |row: &Row, i: usize| -> Result<String> {
        row.get::<_, Option<String>>(i).ok_or_else(|| anyhow!("can't scan into dest[{i}]: cannot scan NULL"))
    };
    let num = |row: &Row, i: usize| -> Result<f64> {
        row.get::<_, Option<Num>>(i).map(|n| n.0).ok_or_else(|| anyhow!("can't scan into dest[{i}]: cannot scan NULL"))
    };
    let name = text(&row, 0)?;
    let strategy = text(&row, 1)?;
    let race_filter = text(&row, 3)?;
    let total_trades =
        row.get::<_, Option<i32>>(4).ok_or_else(|| anyhow!("can't scan into dest[4]: cannot scan NULL"))?;
    let total_pnl = num(&row, 5)?;
    let avg_return = num(&row, 6)?;
    let sharpe = num(&row, 7)?;
    let win_rate = num(&row, 8)?;
    let max_dd = num(&row, 9)?;
    Ok(Json::obj([
        ("name", Json::str(name)),
        ("strategy", Json::str(strategy)),
        ("race_filter", Json::str(race_filter)),
        ("total_trades", Json::Int(total_trades as i64)),
        ("total_pnl_usdc", Json::Float(total_pnl)),
        ("avg_return_pct", Json::Float(avg_return)),
        ("sharpe_ratio", Json::Float(sharpe)),
        ("win_rate", Json::Float(win_rate)),
        ("max_drawdown", Json::Float(max_dd)),
    ]))
}

fn write_backtest_summary_json(db: &Db, path: &str) {
    let summary = match backtest_summary(db) {
        Ok(s) => s,
        Err(e) => {
            println!("  warning: no backtest runs found: {e:#}");
            return;
        }
    };
    // Best effort: a NaN/Inf value writes an empty file, and write errors are ignored.
    let data = serialize(&summary, JsonStyle::SORTED_2).unwrap_or_default();
    let _ = fs::write(path, data);
}

fn write_cumulative_pnl_csv(db: &Db, path: &str) {
    let Ok(rows) = db.query(
        "
		SELECT entered_at::DATE AS day, SUM(pnl) AS daily_pnl, COUNT(*) AS n_trades
		FROM backtest_trades bt
		JOIN (SELECT id FROM backtest_runs ORDER BY id DESC LIMIT 1) r ON r.id = bt.run_id
		WHERE entered_at IS NOT NULL
		GROUP BY day ORDER BY day",
        &[],
    ) else {
        return;
    };
    let mut out = Vec::with_capacity(rows.len());
    let mut cum_pnl = 0.0;
    for r in &rows {
        let day: NaiveDate = r.get(0);
        // A NULL sum counts as 0.
        let daily = r.get::<_, Option<Num>>(1).map_or(0.0, |n| n.0);
        cum_pnl += daily;
        let n: i64 = r.get(2);
        out.push(vec![day.format("%Y-%m-%d").to_string(), ff(daily), ff(cum_pnl), n.to_string()]);
    }
    let _ = write_csv(path, &["date", "daily_pnl", "cumulative_pnl", "n_trades"], &out);
}

fn write_correlation_csv(path: &str, rows: &[CorrelationRow]) {
    let out: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![r.event_name.clone(), r.market_a.clone(), r.market_b.clone(), ff(r.pearson_r), r.num_obs.to_string()]
        })
        .collect();
    let _ = write_csv(path, &["event", "market_a", "market_b", "pearson_r", "n_observations"], &out);
}

fn write_mz_csv(path: &str, rows: &[MzRow]) {
    let out: Vec<Vec<String>> =
        rows.iter().map(|r| vec![r.market_id.clone(), ff(r.implied_prob), r.realized_outcome.to_string()]).collect();
    let _ = write_csv(path, &["market_id", "implied_probability", "realized_outcome"], &out);
}

fn write_spread_csv(path: &str, rows: &[SpreadRow]) {
    let out: Vec<Vec<String>> = rows
        .iter()
        .map(|r| vec![r.market_id.clone(), opt(&r.race_type), ff(r.avg_spread), ff(r.volume), ff(r.maker_return)])
        .collect();
    let _ = write_csv(path, &["market_id", "race_type", "avg_spread", "volume", "maker_return"], &out);
}
