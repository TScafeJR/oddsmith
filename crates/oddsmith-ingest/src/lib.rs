//! Data ingestion: Polymarket's Gamma, CLOB and Data APIs and 538 poll CSVs, plus the
//! `oddsmith ingest` subcommands.

pub mod cli;
pub mod polling;
pub mod polymarket;
pub mod text;
