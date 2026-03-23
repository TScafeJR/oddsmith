//! Backtests against the database: the [`Engine`] loads qualifying trades, runs the pure
//! simulation from `oddsmith-core`, and optionally persists the run and its parquet export.

pub mod export;
pub mod runner;

pub use runner::{Engine, LogFn, RunResult, is_cancelled, run_strategy};
