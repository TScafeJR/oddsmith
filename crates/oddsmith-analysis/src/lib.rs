//! Market analysis and the `oddsmith analyze` subcommands: calibration, maker/taker returns,
//! polls-vs-market benchmarks, correlations, overconfidence, crypto beta and wallet
//! analysis, written as CSV/JSON/HTML reports with byte-for-byte stable formatting.

pub mod benchmark;
pub mod beta;
pub mod charts;
pub mod cli;
pub mod correlation;
pub mod crypto;
pub mod export;
pub mod json;
pub mod jsonvalue;
pub mod overconfidence;
pub mod stats;
pub mod textfmt;
pub mod util;
pub mod wallets;
