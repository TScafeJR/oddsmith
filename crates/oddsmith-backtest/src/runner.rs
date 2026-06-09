//! The database-backed backtest runner: loads trades, simulates, and persists the run.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Result, anyhow, bail};
use oddsmith_core::research::TradeSource;
use oddsmith_core::textfmt::fixed;
use oddsmith_core::{
    Cancelled, PortfolioMetrics, Position, Strategy, apply_capital, compute_portfolio_metrics, simulate,
};
use oddsmith_store::Db;
use serde_json::{Value, json};

use crate::export::{ParquetTrade, upload_parquet_chunked, write_parquet_file};

/// Callback that receives log lines (each ends in `\n`); used to stream progress to the web UI.
pub type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

/// Output of a backtest run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunResult {
    pub metrics: PortfolioMetrics,
    pub positions: Vec<Position>,
}

/// Runs backtests against resolved markets in the database.
pub struct Engine {
    pub db: Db,
    pub strategy: Strategy,
    pub verbose: bool,
    /// Skip writing results to the database (web UI, library calls).
    pub skip_persist: bool,
    pub log: Option<LogFn>,
    /// For parquet upload to Supabase Storage.
    pub supabase_url: String,
    pub supabase_key: String,
}

impl Engine {
    pub fn new(db: Db, strategy: Strategy) -> Self {
        Engine {
            db,
            strategy,
            verbose: false,
            skip_persist: false,
            log: None,
            supabase_url: String::new(),
            supabase_key: String::new(),
        }
    }

    /// Send a line to the log callback and, in verbose mode, to stdout.
    fn emit(&self, msg: &str) {
        if let Some(log) = &self.log {
            log(msg);
        }
        if self.verbose {
            print!("{msg}");
            let _ = std::io::stdout().flush();
        }
    }

    /// Execute the backtest. When `cancel` is set the run stops at the next check and
    /// returns an error for which [`is_cancelled`] is true. Unless `skip_persist` is set,
    /// the run is saved to `backtest_runs` and its trades to parquet; a failure there is
    /// only logged as a warning.
    pub fn run(&self, race_filter: &str, cancel: Option<&AtomicBool>) -> Result<RunResult> {
        let log = |m: &str| self.emit(m);
        let result = run_strategy(&self.db, &self.strategy, race_filter, cancel, &log)?;
        if !self.skip_persist {
            self.emit("persisting results to database...\n");
            if let Err(e) = self.persist_results(race_filter, &result.metrics, &result.positions) {
                self.emit(&format!("  warning: failed to persist backtest results: {e:#}\n"));
            }
        }
        Ok(result)
    }

    fn persist_results(&self, race_filter: &str, metrics: &PortfolioMetrics, positions: &[Position]) -> Result<()> {
        let s = &self.strategy;
        let window = s.window()?;
        let params_json = finite_json(
            &[s.min_price, s.max_price, s.position_size, s.initial_capital, s.maker_fee_rate, s.taker_fee_rate],
            json!({
                "start_date": s.start_date,
                "end_date": s.end_date,
                "window_by": window.by.as_str(),
                "enforce_capital": s.enforce_capital,
                "min_price": s.min_price,
                "max_price": s.max_price,
                "side": s.side,
                "role": s.role,
                "position_size": s.position_size,
                "initial_capital": s.initial_capital,
                "maker_fee_rate": s.maker_fee_rate,
                "taker_fee_rate": s.taker_fee_rate,
            }),
        );
        let results_json = metrics
            .bucket_breakdown
            .iter()
            .all(|b| b.is_finite())
            .then(|| Value::Array(metrics.bucket_breakdown.iter().map(|b| b.to_json()).collect()));

        let name = format!("{}_{race_filter}_{}", s.name, chrono::Local::now().format("%Y%m%d_%H%M%S"));
        let total_trades = metrics.total_trades as i64;
        let skipped = metrics.skipped_trades as i64;
        let final_equity = s.enforce_capital.then_some(metrics.final_equity);
        // Floats go in as float8 and are cast to the column types by the server.
        let row = self
            .db
            .query_one(
                "
		INSERT INTO backtest_runs (name, strategy, params, race_filter,
			total_trades, total_pnl, avg_return_pct, sharpe_ratio, win_rate, max_drawdown,
			results_json, date_from, date_to, window_by, enforce_capital,
			skipped_trades, final_equity, created_at)
		VALUES ($1,$2,$3,$4,$5::int8,$6::float8,$7::float8,$8::float8,$9::float8,$10::float8,
			$11,$12,$13,$14,$15,$16::int8,$17::float8,NOW())
		RETURNING id",
                &[
                    &name,
                    &s.name,
                    &params_json,
                    &race_filter,
                    &total_trades,
                    &metrics.total_pnl,
                    &metrics.avg_return_pct,
                    &metrics.sharpe_ratio,
                    &metrics.win_rate,
                    &metrics.max_drawdown,
                    &results_json,
                    &window.start,
                    &window.end,
                    &window.by.as_str(),
                    &s.enforce_capital,
                    &skipped,
                    &final_equity,
                ],
            )
            .map_err(|e| anyhow!("inserting backtest_run: {e:#}"))?;
        let Some(row) = row else { bail!("inserting backtest_run: no rows in result set") };
        let run_id: i64 = row.get(0);

        self.emit(&format!("  writing {} trades to parquet...\n", positions.len()));
        let trades: Vec<ParquetTrade> = positions.iter().map(|p| parquet_trade(run_id, p)).collect();

        if !self.supabase_url.is_empty() && !self.supabase_key.is_empty() {
            let log = |m: &str| self.emit(m);
            if let Err(e) = upload_parquet_chunked(&trades, run_id, &self.supabase_url, &self.supabase_key, &log) {
                self.emit(&format!("  warning: upload failed: {e:#}\n"));
            }
        } else {
            create_dir_755("output")?;
            let output_path = format!("output/backtest_run_{run_id}.parquet");
            write_parquet_file(Path::new(&output_path), &trades).map_err(|e| anyhow!("writing parquet: {e:#}"))?;
            let size = std::fs::metadata(&output_path)?.len();
            self.emit(&format!("  saved to {output_path} ({} MB)\n", fixed(size as f64 / (1024.0 * 1024.0), 1)));
        }

        self.emit(&format!("  saved backtest run #{run_id} with {} trades\n", positions.len()));
        Ok(())
    }
}

/// The value, or NULL when one of its floats isn't finite (strict JSON has no NaN or infinity).
fn finite_json(floats: &[f64], value: Value) -> Option<Value> {
    floats.iter().all(|f| f.is_finite()).then_some(value)
}

fn create_dir_755(path: &str) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o755);
    builder.create(path).map_err(|e| anyhow!("{}", oddsmith_core::strategy::os_error_text(&e, path)))
}

/// A position as an exported trade: exit price 1 when the held token won, else 0.
pub fn parquet_trade(run_id: i64, p: &Position) -> ParquetTrade {
    let resolved_yes = matches!(p.resolution.as_str(), "YES" | "Yes" | "1");
    let won = (p.side == "YES" && resolved_yes) || (p.side == "NO" && !resolved_yes);
    ParquetTrade {
        run_id,
        market_id: p.market_id.clone(),
        entry_price: p.entry_price,
        exit_price: if won { 1.0 } else { 0.0 },
        size: p.size,
        side: p.side.clone(),
        role: p.role.clone(),
        pnl: p.pnl,
        fee: p.fee,
        return_pct: p.return_pct,
        bucket: p.bucket.clone(),
        resolution: p.resolution.clone(),
    }
}

/// The body of [`Engine::run`] without persistence: fetch the strategy's trades from
/// `source`, simulate, apply the capital limit if set, and compute metrics. Progress
/// goes to `log`. Cancellation is a [`Cancelled`] error.
pub fn run_strategy(
    source: &dyn TradeSource,
    strategy: &Strategy,
    race_filter: &str,
    cancel: Option<&AtomicBool>,
    log: &dyn Fn(&str),
) -> Result<RunResult> {
    let side = if strategy.side.is_empty() { "YES" } else { strategy.side.as_str() };
    let mut role = strategy.role.to_uppercase();
    if role.is_empty() {
        role = "MAKER".into();
    }
    let fee_rate = strategy.fee_rate();
    if fee_rate > 0.0 {
        log(&format!("applying {role} fees: {} \u{d7} p \u{d7} (1\u{2212}p) per contract\n", fixed(fee_rate, 4)));
    }

    let window = strategy.window()?;
    if window.is_set() {
        log(&format!("date window: {}\n", window.describe()));
    }

    log("fetching all matching trades in one query...\n");
    let trades = source
        .backtest_trades(strategy.min_price, strategy.max_price, side, race_filter, &window)
        .map_err(|e| anyhow!("fetching trades: {e:#}"))?;
    if trades.is_empty() {
        bail!(
            "no qualifying trades found in price range [{}, {}]",
            fixed(strategy.min_price, 2),
            fixed(strategy.max_price, 2)
        );
    }

    log(&format!("loaded {} trades, building positions...\n", trades.len()));
    let mut positions = simulate(&trades, strategy, cancel, Some(log)).map_err(anyhow::Error::new)?;
    let market_count = trades.iter().map(|t| t.market_id.as_str()).collect::<HashSet<_>>().len();
    log(&format!("built {} positions across {market_count} markets\n", positions.len()));

    let mut capital = 0.0;
    let mut ledger = None;
    if strategy.enforce_capital {
        capital = strategy.initial_capital;
        let l = apply_capital(&positions, capital);
        positions = l.accepted;
        log(&format!(
            "capital limit ${}: took {} positions, skipped {} for lack of cash (peak exposure ${})\n",
            fixed(capital, 0),
            positions.len(),
            l.skipped,
            fixed(l.peak_exposure, 2)
        ));
        ledger = Some((l.skipped, l.peak_exposure));
    }

    log("computing portfolio metrics...\n");
    let mut metrics = compute_portfolio_metrics(&positions, capital);
    if let Some((skipped, peak_exposure)) = ledger {
        metrics.skipped_trades = skipped;
        metrics.peak_exposure = peak_exposure;
    }
    Ok(RunResult { metrics, positions })
}

/// Whether an error from [`Engine::run`] means the run was cancelled.
pub fn is_cancelled(err: &anyhow::Error) -> bool {
    err.downcast_ref::<Cancelled>().is_some()
}
