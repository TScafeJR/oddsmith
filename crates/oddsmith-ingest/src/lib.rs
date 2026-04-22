//! Data ingestion: Polymarket's Gamma, CLOB and Data APIs, Polygon transfers through
//! Alchemy, and 538 poll CSVs, plus the `oddsmith ingest` subcommands.

pub mod chain;
pub mod cli;
pub mod polling;
pub mod polymarket;
pub mod text;
