//! `oddsmith analyze` subcommands.

use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use oddsmith_store::Ctx;

use crate::charts::{compute_calibration_chart_data, write_calibration_chart_html};
use crate::overconfidence::{compute_overconfidence, write_overconfidence_csv, write_overconfidence_summary_json};
use crate::textfmt::{fixed, fixed_signed};
use crate::util::{join, make_dirs, os_err};

/// Generate statistics and CSVs for the write-up.
#[derive(Debug, Subcommand)]
pub enum AnalyzeCommand {
    /// Generate all market analysis outputs (calibration, backtest, polls, etc.)
    All(AllArgs),
    /// Analyze wallet activity: whales, trade pairs, on-chain transfer graph (after onchain ingest)
    Wallets(WalletsArgs),
    /// Measure overconfidence: calibration errors, Brier score decomposition
    Overconfidence(OverconfidenceArgs),
    /// Generate FLB calibration curve chart and price analysis (HTML)
    Charts(ChartsArgs),
}

#[derive(Debug, Args)]
pub struct AllArgs {
    /// directory for CSV/JSON output (default ./output)
    #[arg(long, value_name = "OUTPUT_DIR", default_value = "./output", hide_default_value = true)]
    pub output_dir: String,
}

#[derive(Debug, Args)]
pub struct WalletsArgs {
    /// directory for CSV/JSON output (default ./output)
    #[arg(long, value_name = "OUTPUT_DIR", default_value = "./output", hide_default_value = true)]
    pub output_dir: String,
    /// number of top wallets to include in whale profiles (default 500)
    #[arg(
        long = "top",
        value_name = "TOP_N",
        default_value_t = 500,
        hide_default_value = true,
        allow_negative_numbers = true
    )]
    pub top_n: i64,
    /// sort wallets by: total_volume, avg_trade_size, total_trades, unique_markets, maker_volume, taker_volume (default total_volume)
    #[arg(long = "sort", value_name = "SORT_BY", default_value = "total_volume", hide_default_value = true)]
    pub sort_by: String,
}

#[derive(Debug, Args)]
pub struct OverconfidenceArgs {
    /// directory for CSV/JSON output (default ./output)
    #[arg(long, value_name = "OUTPUT_DIR", default_value = "./output", hide_default_value = true)]
    pub output_dir: String,
    /// filter by race type (e.g., presidential, senate)
    #[arg(long = "race", value_name = "RACE_FILTER", default_value = "", hide_default_value = true)]
    pub race_filter: String,
}

#[derive(Debug, Args)]
pub struct ChartsArgs {
    /// directory for HTML output (default ./output)
    #[arg(long, value_name = "OUTPUT_DIR", default_value = "./output", hide_default_value = true)]
    pub output_dir: String,
    /// filter by race type (e.g., presidential)
    #[arg(long = "race", value_name = "RACE_FILTER", default_value = "", hide_default_value = true)]
    pub race_filter: String,
    /// annualized risk-free rate (default 4%)
    #[arg(
        long,
        value_name = "RISK_FREE_RATE",
        default_value_t = 0.04,
        hide_default_value = true,
        allow_negative_numbers = true
    )]
    pub risk_free_rate: f64,
}

impl AnalyzeCommand {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        match self {
            AnalyzeCommand::All(a) => run_all(a, ctx),
            AnalyzeCommand::Wallets(a) => run_wallets(a, ctx),
            AnalyzeCommand::Overconfidence(a) => run_overconfidence(a, ctx),
            AnalyzeCommand::Charts(a) => run_charts(a, ctx),
        }
    }
}

fn run_all(args: &AllArgs, ctx: &Ctx) -> Result<()> {
    let cfg = ctx.config()?;
    let db = ctx.connect(&cfg)?;
    println!("generating market analysis outputs...");
    crate::export::run_all(&db, &args.output_dir, cfg.verbose)
}

fn run_wallets(args: &WalletsArgs, ctx: &Ctx) -> Result<()> {
    let cfg = ctx.config()?;
    let db = ctx.connect(&cfg)?;
    println!("running wallet analysis...");
    crate::wallets::run_wallet_analysis(&db, &args.output_dir, cfg.verbose, args.top_n, &args.sort_by)
}

fn mkdir(dir: &str) -> Result<()> {
    make_dirs(dir).map_err(|e| anyhow!(os_err(&e, dir)))
}

fn run_overconfidence(args: &OverconfidenceArgs, ctx: &Ctx) -> Result<()> {
    let cfg = ctx.config()?;
    let output_dir = &args.output_dir;
    let db = ctx.connect(&cfg)?;
    println!("computing overconfidence metrics...");
    let result = compute_overconfidence(&db, &args.race_filter)?;

    mkdir(output_dir)?;
    let csv_path = join(output_dir, "overconfidence.csv");
    write_overconfidence_csv(&csv_path, &result).map_err(|e| anyhow!("writing CSV: {}", os_err(&e, &csv_path)))?;
    write_overconfidence_summary_json(&join(output_dir, "overconfidence_summary.json"), &result)
        .map_err(|e| anyhow!("writing JSON: {e:#}"))?;

    println!("\n=== Overconfidence Analysis ({} markets) ===", result.num_markets);
    println!("Overconfidence Index (favorites >50%): {}", fixed(result.overconfidence_index, 4));
    println!("Underconfidence Index (longshots <50%): {}", fixed(result.underconfidence_index, 4));
    println!("Brier Score:       {}", fixed(result.brier_score, 4));
    println!("  Reliability:     {}  (lower = better calibrated)", fixed(result.brier_reliability, 4));
    println!("  Resolution:      {}  (higher = better discrimination)", fixed(result.brier_resolution, 4));
    println!("  Uncertainty:     {}", fixed(result.brier_uncertainty, 4));
    println!("Vol-Weighted Bias: {}", fixed(result.confidence_weighted_bias, 4));

    println!("\nPer-bucket breakdown:");
    println!("{:<10} {:>8} {:>8} {:>10} {:>12}", "Bucket", "N", "Implied", "Realized", "Cal Error");
    for b in &result.buckets {
        if b.num_contracts == 0 {
            continue;
        }
        println!(
            "{:<10} {:>8} {:>8} {:>10} {:>12}",
            b.bucket,
            b.num_contracts,
            fixed(b.implied_prob_mid, 2),
            fixed(b.realized_freq, 4),
            fixed_signed(b.calibration_error, 4)
        );
    }
    println!("\nOutputs written to {output_dir}/");
    Ok(())
}

fn run_charts(args: &ChartsArgs, ctx: &Ctx) -> Result<()> {
    let cfg = ctx.config()?;
    let output_dir = &args.output_dir;
    let db = ctx.connect(&cfg)?;
    mkdir(output_dir)?;

    println!("computing calibration chart data...");
    let data = compute_calibration_chart_data(&db, &args.race_filter, args.risk_free_rate)?;

    let out_path = join(output_dir, "calibration_chart.html");
    write_calibration_chart_html(&out_path, &data)?;

    println!("calibration chart written to {out_path}");
    println!(
        "  {} price points, {} trades, {} contracts",
        data.points.as_ref().map_or(0, Vec::len),
        data.total_trades,
        data.total_markets
    );
    Ok(())
}
