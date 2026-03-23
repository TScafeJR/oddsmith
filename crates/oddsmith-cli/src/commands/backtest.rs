//! `oddsmith backtest`.

use anyhow::{Result, anyhow, bail};
use clap::Args;
use oddsmith_backtest::Engine;
use oddsmith_core::strategy::{load_strategy, parse_buckets};
use oddsmith_core::textfmt::{self, fixed, signed};
use oddsmith_core::{Strategy, Window};
use oddsmith_store::Ctx;

const LONG: &str = "Run a backtest using either a strategy JSON file or CLI flags.

Strategy file example (strategies/fade_longshots.json):
  {
    \"name\": \"fade_longshots\",
    \"side\": \"NO\",
    \"min_price\": 0.00,
    \"max_price\": 0.10,
    \"description\": \"Buy NO tokens when YES implied prob is <10%\"
  }

Usage:
  oddsmith backtest --strategy strategies/fade_longshots.json
  oddsmith backtest --side YES --min-price 0.50 --max-price 0.95
  oddsmith backtest --side NO --min-price 0.00 --max-price 0.10 --race presidential
  oddsmith backtest --strategy strategies/no_below_33.json --role TAKER --taker-fee-rate 0.07
  oddsmith backtest --strategy strategies/maker_favorites.json --from 2024-01-01 --to 2024-11-05
  oddsmith backtest --strategy strategies/fade_longshots.json --window-by resolved --enforce-capital

Fees are charged per contract as rate \u{d7} p \u{d7} (1\u{2212}p), where p is the price of the
token bought (Kalshi-style schedule). Both rates default to 0 (Polymarket has no
exchange fee on most markets); --role picks which rate applies.

Trades are replayed in time order. --from/--to restrict the run to a date window
(inclusive, YYYY-MM-DD) on entry time, or on resolution time with
--window-by resolved. --enforce-capital caps open exposure at --initial-capital:
entries are skipped when cash runs out, and cash comes back when markets resolve.";

/// A float flag value: surrounding whitespace, `_` between digits, `inf` and `nan` allowed.
pub(crate) fn float_arg(s: &str) -> Result<f64, String> {
    textfmt::parse_float(s).ok_or_else(|| format!("invalid float value: {}", textfmt::quote_str(s)))
}

// Flags left unset keep the strategy file's values; set ones override them.
#[derive(Debug, Args)]
#[command(after_long_help = LONG)]
pub struct BacktestArgs {
    /// Path to strategy JSON file
    #[arg(long = "strategy", default_value = "", hide_default_value = true)]
    strategy_file: String,
    /// Token side to buy: YES or NO [default: YES]
    #[arg(long)]
    side: Option<String>,
    /// Fill role, selects which fee rate applies: MAKER or TAKER [default: MAKER]
    #[arg(long)]
    role: Option<String>,
    /// Maker fee coefficient: fee/contract = rate × p × (1−p) (Kalshi: 0.0175) [default: 0]
    #[arg(long, value_parser = float_arg, allow_negative_numbers = true)]
    maker_fee_rate: Option<f64>,
    /// Taker fee coefficient: fee/contract = rate × p × (1−p) (Kalshi: 0.07) [default: 0]
    #[arg(long, value_parser = float_arg, allow_negative_numbers = true)]
    taker_fee_rate: Option<f64>,
    /// Min YES implied probability to enter [default: 0.5]
    #[arg(long, value_parser = float_arg, allow_negative_numbers = true)]
    min_price: Option<f64>,
    /// Max YES implied probability [default: 0.95]
    #[arg(long, value_parser = float_arg, allow_negative_numbers = true)]
    max_price: Option<f64>,
    /// Filter by race type: presidential,senate
    #[arg(long = "race", default_value = "", hide_default_value = true)]
    race_filter: String,
    /// Starting capital in USDC [default: 10000]
    #[arg(long, value_parser = float_arg, allow_negative_numbers = true)]
    initial_capital: Option<f64>,
    /// Max per-position size [default: 100]
    #[arg(long, value_parser = float_arg, allow_negative_numbers = true)]
    position_size: Option<f64>,
    /// Human-readable run name
    #[arg(long)]
    name: Option<String>,
    /// Only include trades on or after this date (YYYY-MM-DD)
    #[arg(long = "from")]
    start_date: Option<String>,
    /// Only include trades on or before this date (YYYY-MM-DD)
    #[arg(long = "to")]
    end_date: Option<String>,
    /// Timestamp the date window applies to: entry time or market resolution [default: traded]
    #[arg(long, value_parser = ["traded", "resolved"])]
    window_by: Option<String>,
    /// Skip entries once open exposure would exceed --initial-capital
    #[arg(long)]
    enforce_capital: bool,
    /// Custom bucket boundaries (e.g. "0,5,10,20,30,40,50,60,70,80,90,95,98,100")
    #[arg(long = "buckets", default_value = "", hide_default_value = true)]
    buckets_str: String,
}

impl BacktestArgs {
    /// The strategy from the file (with explicit flags overriding it) or from the flags.
    fn strategy(&self) -> Result<Strategy> {
        if !self.strategy_file.is_empty() {
            let mut s = load_strategy(&self.strategy_file)?;
            if let Some(v) = self.min_price {
                s.min_price = v;
            }
            if let Some(v) = self.max_price {
                s.max_price = v;
            }
            if let Some(v) = &self.side {
                s.side = v.clone();
            }
            if let Some(v) = &self.role {
                s.role = v.clone();
            }
            if let Some(v) = self.maker_fee_rate {
                s.maker_fee_rate = v;
            }
            if let Some(v) = self.taker_fee_rate {
                s.taker_fee_rate = v;
            }
            if let Some(v) = self.initial_capital {
                s.initial_capital = v;
            }
            if let Some(v) = self.position_size {
                s.position_size = v;
            }
            if let Some(v) = &self.name {
                s.name = v.clone();
            }
            if let Some(v) = &self.start_date {
                s.start_date = v.clone();
            }
            if let Some(v) = &self.end_date {
                s.end_date = v.clone();
            }
            if let Some(v) = &self.window_by {
                s.window_by = v.clone();
            }
            if self.enforce_capital {
                s.enforce_capital = true;
            }
            return Ok(s);
        }
        let buckets = parse_buckets(&self.buckets_str).map_err(|e| anyhow!("invalid --buckets: {e}"))?;
        let mut s = Strategy {
            name: self.name.clone().unwrap_or_default(),
            side: self.side.clone().unwrap_or_else(|| "YES".into()),
            min_price: self.min_price.unwrap_or(0.50),
            max_price: self.max_price.unwrap_or(0.95),
            role: self.role.clone().unwrap_or_else(|| "MAKER".into()),
            initial_capital: self.initial_capital.unwrap_or(10000.0),
            position_size: self.position_size.unwrap_or(100.0),
            maker_fee_rate: self.maker_fee_rate.unwrap_or(0.0),
            taker_fee_rate: self.taker_fee_rate.unwrap_or(0.0),
            buckets,
            start_date: self.start_date.clone().unwrap_or_default(),
            end_date: self.end_date.clone().unwrap_or_default(),
            window_by: self.window_by.clone().unwrap_or_else(|| "traded".into()),
            enforce_capital: self.enforce_capital,
            ..Strategy::default()
        };
        if s.name.is_empty() {
            s.name = if s.side == "NO" { "fade_longshots".into() } else { "maker_favorites".into() };
        }
        Ok(s)
    }
}

pub fn run(args: &BacktestArgs, ctx: &Ctx) -> Result<()> {
    let cfg = ctx.config()?;
    let db = ctx.connect(&cfg)?;

    let mut strat = args.strategy()?;
    if strat.side.is_empty() {
        strat.side = "YES".into();
    }
    strat.role = strat.role.to_uppercase();
    if strat.role.is_empty() {
        strat.role = "MAKER".into();
    }
    if strat.role != "MAKER" && strat.role != "TAKER" {
        bail!("invalid role {}: must be MAKER or TAKER", textfmt::json_str(&strat.role));
    }
    let window: Window = strat.window()?;

    let mut engine = Engine::new(db, strat.clone());
    engine.verbose = cfg.verbose;
    engine.supabase_url = cfg.supabase_url.clone();
    engine.supabase_key = cfg.supabase_anon_key.clone();

    let desc = if strat.description.is_empty() { String::new() } else { format!(" ({})", strat.description) };
    println!(
        "running backtest: strategy={}, side={}, price=[{}, {}], race={}, window={}{desc}",
        strat.name,
        strat.side,
        fixed(strat.min_price, 2),
        fixed(strat.max_price, 2),
        args.race_filter,
        window.describe()
    );

    let result = engine.run(&args.race_filter, None)?;
    let m = &result.metrics;

    println!("\n=== Backtest Results ===");
    println!("Strategy:         {} (side={}, role={})", strat.name, strat.side, strat.role);
    println!("Total trades:     {}", m.total_trades);
    println!("Total PnL:        ${} (net of fees)", fixed(m.total_pnl, 2));
    println!("Total fees:       ${} (rate={})", fixed(m.total_fees, 2), fixed(strat.fee_rate(), 4));
    println!("Avg return/trade: {}%", fixed(m.avg_return_pct * 100.0, 2));
    println!("Win rate:         {}%", fixed(m.win_rate * 100.0, 1));
    println!("Sharpe ratio:     {} (annualized, rf=4%)", fixed(m.sharpe_ratio, 3));
    println!("Max drawdown:     {}%", fixed(m.max_drawdown * 100.0, 1));
    println!("Avg hold time:    {} days", fixed(m.avg_hold_days, 1));
    if strat.enforce_capital {
        println!(
            "Capital:          ${} -> ${} ({}%)",
            fixed(m.initial_capital, 2),
            fixed(m.final_equity, 2),
            signed(m.return_on_capital * 100.0, 2)
        );
        println!("Peak exposure:    ${}", fixed(m.peak_exposure, 2));
        println!("Skipped (cash):   {}", m.skipped_trades);
    }

    println!("\nBy price bucket:");
    println!(
        "  {:<10} {:>6} {:>10} {:>10} {:>10} {:>8} {:>8} {:>12}",
        "Bucket", "Trades", "Avg Ret", "Exp Win%", "B/E Win%", "Win%", "StdDev", "Fees"
    );
    for b in &m.bucket_breakdown {
        println!(
            "  {:<10} {:>6} {:>9}% {:>9}% {:>9}% {:>7}% {:>7}% {:>12}",
            b.bucket,
            b.trade_count,
            fixed(b.avg_return * 100.0, 2),
            fixed(b.expected_win_pct * 100.0, 1),
            fixed(b.breakeven_win_pct * 100.0, 1),
            fixed(b.win_rate * 100.0, 1),
            fixed(b.std_dev * 100.0, 2),
            fixed(b.total_fees, 2)
        );
    }
    Ok(())
}
