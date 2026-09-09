//! `oddsmith sweep`.

use std::collections::HashSet;
use std::io::Write as _;

use anyhow::{Result, bail};
use clap::Args;
use oddsmith_core::research::{METRICS, parse_param, sweep, write_sweep_csv, write_sweep_heatmap};
use oddsmith_core::strategy::{default_strategy, load_strategy};
use oddsmith_core::textfmt::{self, fixed, signed};
use oddsmith_store::Ctx;

use super::backtest::{float_arg, int_arg};

const LONG: &str = "Backtest every combination of one or more parameters and rank the results.

Trades are fetched once per side, then each variant is simulated in memory, so a
grid of dozens of variants costs about as much as one backtest.

Examples:
  oddsmith sweep --strategy strategies/maker_favorites.json \\
      --param min_price=0.50:0.90:0.05 --param max_price=0.90,0.95,0.99
  oddsmith sweep --param side=YES,NO --param min_price=0.0:0.9:0.1 --metric edge
  oddsmith sweep --strategy strategies/fade_longshots.json --param max_price=0.05:0.30:0.05 \\
      --param role=MAKER,TAKER --taker-fee-rate 0.07 --from 2024-01-01

Sweepable: min_price, max_price, side, role, position_size, maker_fee_rate, taker_fee_rate.
Values are a comma list (a,b,c) or an inclusive numeric range (start:stop:step).

Writes sweep_<name>.csv to --output-dir, plus sweep_<name>.html (a heatmap of
--metric) when exactly two parameters are swept.";

#[derive(Debug, Args)]
#[command(after_long_help = LONG)]
pub struct SweepArgs {
    /// Base strategy JSON file (default: maker_favorites)
    #[arg(long = "strategy", default_value = "", hide_default_value = true)]
    strategy_file: String,
    /// Parameter to vary; repeat for a grid
    #[arg(long = "param", value_name = "NAME=VALUES")]
    params: Vec<String>,
    /// Filter by race type
    #[arg(long = "race", default_value = "", hide_default_value = true)]
    race_filter: String,
    /// Window start (YYYY-MM-DD)
    #[arg(long = "from")]
    start_date: Option<String>,
    /// Window end, inclusive (YYYY-MM-DD)
    #[arg(long = "to")]
    end_date: Option<String>,
    /// Timestamp the window applies to
    #[arg(long, value_parser = ["traded", "resolved"])]
    window_by: Option<String>,
    /// Cap open exposure at the strategy's initial capital
    #[arg(long)]
    enforce_capital: bool,
    /// Override the maker fee coefficient
    #[arg(long, value_parser = float_arg, allow_negative_numbers = true)]
    maker_fee_rate: Option<f64>,
    /// Override the taker fee coefficient
    #[arg(long, value_parser = float_arg, allow_negative_numbers = true)]
    taker_fee_rate: Option<f64>,
    /// Metric to rank and plot by
    #[arg(long, default_value = "avg_return", value_parser = METRICS.to_vec())]
    metric: String,
    /// Bootstrap resamples for the avg-return confidence interval (0 to skip)
    #[arg(long, default_value = "200", value_parser = int_arg, allow_negative_numbers = true)]
    bootstrap: i64,
    /// Rows to print
    #[arg(long, default_value = "20", value_parser = int_arg, allow_negative_numbers = true)]
    top: i64,
    /// Directory for CSV/HTML output
    #[arg(long, default_value = "./output")]
    output_dir: String,
}

pub fn run(args: &SweepArgs, ctx: &Ctx) -> Result<()> {
    if args.params.is_empty() {
        bail!("give at least one --param, e.g. --param min_price=0.5:0.9:0.1");
    }
    let params = args.params.iter().map(|spec| parse_param(spec)).collect::<Result<Vec<_>>>()?;
    let names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
    if names.iter().collect::<HashSet<_>>().len() != names.len() {
        bail!("each parameter can only be swept once");
    }

    let mut base = if args.strategy_file.is_empty() { default_strategy() } else { load_strategy(&args.strategy_file)? };
    if let Some(v) = &args.start_date {
        base.start_date = v.clone();
    }
    if let Some(v) = &args.end_date {
        base.end_date = v.clone();
    }
    if let Some(v) = &args.window_by {
        base.window_by = v.clone();
    }
    if args.enforce_capital {
        base.enforce_capital = true;
    }
    if let Some(v) = args.maker_fee_rate {
        base.maker_fee_rate = v;
    }
    if let Some(v) = args.taker_fee_rate {
        base.taker_fee_rate = v;
    }
    let window = base.window()?;

    let n_variants: usize = params.iter().map(|(_, v)| v.len()).product();
    println!("sweeping {}: {n_variants} variants over {}; window={}", base.name, names.join(", "), window.describe());

    let cfg = ctx.config()?;
    let db = ctx.connect(&cfg)?;
    let log = |m: &str| {
        print!("{m}");
        let _ = std::io::stdout().flush();
    };
    let rows = sweep(&db, &base, &params, &args.race_filter, args.bootstrap.max(0) as usize, Some(&log))?;

    let rows: Vec<_> = rows.into_iter().filter(|r| r.total_trades > 0).collect();
    if rows.is_empty() {
        bail!("no variant had any qualifying trades");
    }
    let metric = |r: &oddsmith_core::SweepRow| r.metric(&args.metric).unwrap_or(f64::NAN);
    let mut ranked = rows.clone();
    ranked.sort_by(|a, b| metric(b).partial_cmp(&metric(a)).unwrap_or(std::cmp::Ordering::Equal));

    let widths: Vec<usize> = names.iter().map(|n| n.chars().count().max(8)).collect();
    let header: Vec<String> = names.iter().zip(&widths).map(|(n, w)| format!("{n:>w$}")).collect();
    println!(
        "\n{}  {:>7} {:>8} {:>17} {:>6} {:>7} {:>7} {:>11}",
        header.join("  "),
        "Trades",
        "Avg Ret",
        "95% CI",
        "Win%",
        "Edge",
        "Sharpe",
        "PnL"
    );
    // The first `top` rows; a negative `top` drops that many rows from the end.
    let len = ranked.len() as i64;
    let shown = if args.top >= 0 { args.top.min(len) } else { (len + args.top).max(0) } as usize;
    for r in &ranked[..shown] {
        let vals: Vec<String> = names
            .iter()
            .zip(&widths)
            .map(|(n, w)| format!("{:>w$}", r.param(n).map(|v| v.short()).unwrap_or_default()))
            .collect();
        let ci_text = r
            .avg_return_ci
            .map_or(String::new(), |ci| format!("[{}, {}]%", signed(ci.low * 100.0, 1), signed(ci.high * 100.0, 1)));
        println!(
            "{}  {:>7} {:>7}% {:>17} {:>5}% {:>6}% {:>7} {:>11}",
            vals.join("  "),
            r.total_trades,
            fixed(r.avg_return * 100.0, 2),
            ci_text,
            fixed(r.win_rate * 100.0, 1),
            signed(r.edge * 100.0, 2),
            fixed(r.sharpe, 3),
            fixed(r.total_pnl, 2)
        );
    }
    if len > args.top {
        println!("... {} more in the CSV", len - args.top);
    }

    std::fs::create_dir_all(&args.output_dir)?;
    let name = if base.name.is_empty() { "strategy" } else { base.name.as_str() };
    let stem = textfmt::path_join(&args.output_dir, &format!("sweep_{name}"));
    write_sweep_csv(format!("{stem}.csv").as_ref(), &ranked, &names)?;
    println!("\nwrote {stem}.csv");
    let varying: Vec<&str> = params.iter().filter(|(_, v)| v.len() > 1).map(|(n, _)| n.as_str()).collect();
    if let [row_param, col_param] = varying[..] {
        write_sweep_heatmap(
            format!("{stem}.html").as_ref(),
            &rows,
            row_param,
            col_param,
            &args.metric,
            &format!("Sweep: {}", base.name),
        )?;
        println!("wrote {stem}.html");
    }
    Ok(())
}
