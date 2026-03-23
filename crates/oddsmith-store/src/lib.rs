//! Everything that talks to the outside world at the lowest level: configuration from
//! `.env` and the environment, the Postgres connection pool with typed queries for each
//! table, and the shared HTTP client.
//!
//! SQL uses Postgres placeholders (`$1`, `$2`, ...). NUMERIC columns are read and written
//! as `f64` through [`Num`] (see [`RowExt`] for the read helpers).

pub mod backtest;
pub mod config;
pub mod ctx;
pub mod db;
pub mod http;
pub mod markets;
pub mod migrations;
pub mod orderbook;
pub mod polls;
pub mod trades;

pub use config::Config;
pub use ctx::Ctx;
pub use db::{Conn, Db, Num, Param, RowExt};
pub use trades::price_bucket;
