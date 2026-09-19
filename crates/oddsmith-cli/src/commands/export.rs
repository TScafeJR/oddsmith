//! `oddsmith export parquet`.

use anyhow::{Result, bail};
use clap::Args;
use oddsmith_backtest::export::{all_run_ids, export_and_upload, export_run_to_parquet};
use oddsmith_store::Ctx;

use super::backtest::int_arg;

const LONG: &str = "Export backtest_trades rows to parquet format and optionally upload to Supabase Storage.

Examples:
  oddsmith export parquet --all --upload          # export all runs and upload
  oddsmith export parquet 4 --upload              # export run #4 and upload
  oddsmith export parquet --all --output-dir ./output  # export locally only";

#[derive(Debug, Args)]
#[command(after_long_help = LONG)]
pub struct ParquetArgs {
    /// Backtest run ID to export
    #[arg(value_name = "run_id")]
    run_id_arg: Option<String>,
    /// Backtest run ID to export
    #[arg(long = "run", default_value = "0", hide_default_value = true, value_parser = int_arg, allow_negative_numbers = true)]
    run_id: i64,
    /// Export all runs
    #[arg(long)]
    all: bool,
    /// Upload to Supabase Storage after export
    #[arg(long)]
    upload: bool,
    /// Local output directory
    #[arg(long = "output-dir", default_value = "./output")]
    out_dir: String,
}

pub fn run_parquet(args: &ParquetArgs, ctx: &Ctx) -> Result<()> {
    let cfg = ctx.config()?;
    let db = ctx.connect(&cfg)?;

    let mut run_id = args.run_id;
    if let Some(arg) = &args.run_id_arg
        && run_id == 0
    {
        run_id = int_arg(arg).unwrap_or(0);
    }
    if !args.all && run_id == 0 {
        bail!("specify a run_id or use --all");
    }
    if args.upload && cfg.supabase_anon_key.is_empty() {
        bail!("SUPABASE_ANON_KEY env var required for upload (find it in Supabase Dashboard > Settings > API)");
    }

    if args.all {
        let ids = all_run_ids(&db)?;
        println!("exporting {} backtest runs...", ids.len());
        for id in ids {
            if args.upload {
                if let Err(e) = export_and_upload(&db, id, &cfg.supabase_url, &cfg.supabase_anon_key, cfg.verbose) {
                    println!("  warning: run {id}: {e:#}");
                }
            } else {
                let path = format!("{}/backtest_run_{id}.parquet", args.out_dir);
                match export_run_to_parquet(&db, id, &path, cfg.verbose) {
                    Ok((_, count)) => println!("  run {id}: {count} trades -> {path}"),
                    Err(e) => println!("  warning: run {id}: {e:#}"),
                }
            }
        }
        return Ok(());
    }
    if args.upload {
        return export_and_upload(&db, run_id, &cfg.supabase_url, &cfg.supabase_anon_key, cfg.verbose);
    }
    let path = format!("{}/backtest_run_{run_id}.parquet", args.out_dir);
    let (_, count) = export_run_to_parquet(&db, run_id, &path, cfg.verbose)?;
    println!("exported {count} trades -> {path}");
    Ok(())
}
