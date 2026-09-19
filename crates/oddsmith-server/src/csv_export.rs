//! `oddsmith export csv`: the markets and trades tables to `markets.csv` and `trades.csv` in
//! the current directory.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use clap::Args;
use oddsmith_core::strategy::os_error_text;
use oddsmith_store::{Ctx, Db};

use crate::format;

/// Overall time budget for the export.
const TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Args)]
pub struct CsvExportArgs {}

pub fn run(_args: &CsvExportArgs, ctx: &Ctx) -> Result<()> {
    let cfg = ctx.config().map_err(|e| anyhow!("config: {e:#}"))?;
    let deadline = Instant::now() + TIMEOUT;
    let db = Db::connect(&cfg.supabase_db_url).map_err(|e| anyhow!("db: {e:#}"))?;
    export_markets(&db, deadline).map_err(|e| anyhow!("markets: {e:#}"))?;
    export_trades(&db, deadline).map_err(|e| anyhow!("trades: {e:#}"))?;
    Ok(())
}

fn check_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() > deadline {
        bail!("deadline exceeded");
    }
    Ok(())
}

fn create(path: &str) -> Result<BufWriter<File>> {
    File::create(path).map(BufWriter::new).map_err(|e| anyhow!("{}", os_error_text(&e, path)))
}

fn export_markets(db: &Db, deadline: Instant) -> Result<()> {
    check_deadline(deadline)?;
    let rows = db.query(format::MARKETS_SQL, &[])?;
    let mut f = create("markets.csv")?;
    f.write_all(format::csv_line(&format::MARKETS_HEADER).as_bytes())?;
    for row in &rows {
        f.write_all(format::csv_line(&format::market_fields(row)).as_bytes())?;
    }
    f.flush()?;
    println!("markets.csv: {} rows", rows.len());
    Ok(())
}

fn export_trades(db: &Db, deadline: Instant) -> Result<()> {
    let mut f = create("trades.csv")?;
    f.write_all(format::csv_line(&format::TRADES_HEADER).as_bytes())?;
    let mut total = 0usize;
    let mut cursor = DateTime::<Utc>::UNIX_EPOCH;
    let mut last_id = String::new();
    loop {
        check_deadline(deadline)?;
        let rows = db.query(format::TRADES_SQL, &[&cursor, &last_id, &format::TRADES_PAGE_SIZE])?;
        for row in &rows {
            let (fields, traded_at, id) = format::trade_fields(row);
            f.write_all(format::csv_line(&fields).as_bytes())?;
            if let Some(t) = traded_at {
                cursor = t;
            }
            if let Some(id) = id {
                last_id = id;
            }
        }
        total += rows.len();
        print!("\rtrades.csv: {total} rows exported...");
        std::io::stdout().flush()?;
        f.flush()?;
        if (rows.len() as i64) < format::TRADES_PAGE_SIZE {
            break;
        }
    }
    println!("\rtrades.csv: {total} rows          ");
    Ok(())
}
