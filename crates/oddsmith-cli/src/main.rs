//! `oddsmith` command-line entry point.

mod commands;

use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use oddsmith_analysis::cli::AnalyzeCommand;
use oddsmith_ingest::cli::IngestCommand;
use oddsmith_store::Ctx;

use commands::{backtest, sweep};

/// Polymarket Favorite-Longshot Bias tool. Gather political prediction market data from
/// Polymarket, benchmark against polls, and backtest a maker-on-favorites strategy.
#[derive(Debug, Parser)]
#[command(name = "oddsmith", version)]
struct Cli {
    /// Verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Supabase DB URL (overrides SUPABASE_DB_URL env)
    #[arg(long, global = true, value_name = "URL")]
    db_url: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run database migrations against Supabase
    Migrate(MigrateArgs),
    /// Ingest Polymarket political markets, trades, and polling data
    #[command(subcommand)]
    Ingest(IngestCommand),
    /// Simulate a trading strategy on resolved political markets
    Backtest(backtest::BacktestArgs),
    /// Run a parameter sweep over a strategy and rank the variants
    Sweep(sweep::SweepArgs),
    /// Generate statistics, CSVs and charts from ingested data
    #[command(subcommand)]
    Analyze(AnalyzeCommand),
    /// Real-time terminal display of live political markets
    Monitor(MonitorArgs),
    /// Start the web UI for configuring and running backtests
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
struct MigrateArgs {
    /// Migrations directory
    #[arg(long = "dir", default_value = "./migrations")]
    migrations_dir: String,
}

#[derive(Debug, Args)]
struct MonitorArgs {
    /// Filter by race type: presidential,senate
    #[arg(long, default_value = "")]
    race: String,
    /// Minimum market liquidity
    #[arg(long, default_value_t = 1000.0)]
    min_liquidity: f64,
    /// Terminal refresh interval
    #[arg(long, default_value = "30s")]
    refresh: String,
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// Port to listen on
    #[arg(long, default_value_t = 8080)]
    port: u16,
}

fn run(cli: Cli) -> Result<()> {
    let ctx = Ctx { verbose: cli.verbose, db_url: cli.db_url };
    match cli.command {
        Command::Migrate(args) => {
            let cfg = ctx.config()?;
            let db = ctx.connect(&cfg)?;
            println!("running migrations...");
            db.run_migrations(&args.migrations_dir).context("migration failed")?;
            println!("migrations complete");
            Ok(())
        }
        Command::Ingest(cmd) => cmd.run(&ctx),
        Command::Backtest(args) => backtest::run(&args, &ctx),
        Command::Sweep(args) => sweep::run(&args, &ctx),
        Command::Analyze(cmd) => cmd.run(&ctx),
        Command::Monitor(_) => {
            println!("monitor mode...");
            bail!("not yet implemented")
        }
        Command::Serve(args) => {
            let cfg = ctx.config()?;
            let db = ctx.connect(&cfg)?;
            println!("backtest UI available at http://localhost:{}", args.port);
            oddsmith_server::serve(db, args.port)
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
