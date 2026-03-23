//! Parquet export of backtest trades and Supabase Storage upload. Parquet writing is
//! behind the `parquet` cargo feature; without it the functions return an error saying so.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use oddsmith_core::strategy::os_error_text;
use oddsmith_store::{Db, RowExt};

/// One exported trade. Field names are the parquet column names.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParquetTrade {
    pub run_id: i64,
    pub market_id: String,
    pub entry_price: f64,
    pub exit_price: f64,
    pub size: f64,
    pub side: String,
    pub role: String,
    pub pnl: f64,
    pub fee: f64,
    pub return_pct: f64,
    pub bucket: String,
    pub resolution: String,
}

/// Rows per parquet row group.
#[cfg(feature = "parquet")]
const ROW_GROUP_ROWS: usize = 1024 * 1024;

/// The parquet schema: every column required, strings as UTF-8, numbers as int64/double.
#[cfg(feature = "parquet")]
const SCHEMA: &str = "message schema {
    REQUIRED INT64 run_id;
    REQUIRED BYTE_ARRAY market_id (STRING);
    REQUIRED DOUBLE entry_price;
    REQUIRED DOUBLE exit_price;
    REQUIRED DOUBLE size;
    REQUIRED BYTE_ARRAY side (STRING);
    REQUIRED BYTE_ARRAY role (STRING);
    REQUIRED DOUBLE pnl;
    REQUIRED DOUBLE fee;
    REQUIRED DOUBLE return_pct;
    REQUIRED BYTE_ARRAY bucket (STRING);
    REQUIRED BYTE_ARRAY resolution (STRING);
}";

/// Write trades to a parquet file (snappy-compressed, with the columns and types of
/// [`SCHEMA`]).
#[cfg(feature = "parquet")]
pub fn write_parquet_file(path: &Path, trades: &[ParquetTrade]) -> Result<()> {
    use std::sync::Arc;

    use parquet::basic::Compression;
    use parquet::data_type::{ByteArray, ByteArrayType, DoubleType, Int64Type};
    use parquet::file::properties::WriterProperties;
    use parquet::file::writer::SerializedFileWriter;
    use parquet::schema::parser::parse_message_type;

    let schema = Arc::new(parse_message_type(SCHEMA)?);
    let props = Arc::new(WriterProperties::builder().set_compression(Compression::SNAPPY).build());
    let file = fs::File::create(path).map_err(|e| anyhow!("{}", open_error_text(&e, &path.to_string_lossy())))?;
    let mut writer = SerializedFileWriter::new(file, schema, props)?;

    let groups: Vec<&[ParquetTrade]> =
        if trades.is_empty() { vec![trades] } else { trades.chunks(ROW_GROUP_ROWS).collect() };
    for group in groups {
        let mut rg = writer.next_row_group()?;
        let mut index = 0;
        while let Some(mut col) = rg.next_column()? {
            let text = |f: fn(&ParquetTrade) -> &str| -> Vec<ByteArray> {
                group.iter().map(|t| ByteArray::from(f(t).as_bytes().to_vec())).collect()
            };
            let num = |f: fn(&ParquetTrade) -> f64| -> Vec<f64> { group.iter().map(f).collect() };
            match index {
                0 => {
                    let ids: Vec<i64> = group.iter().map(|t| t.run_id).collect();
                    col.typed::<Int64Type>().write_batch(&ids, None, None)?;
                }
                1 => _ = col.typed::<ByteArrayType>().write_batch(&text(|t| &t.market_id), None, None)?,
                2 => _ = col.typed::<DoubleType>().write_batch(&num(|t| t.entry_price), None, None)?,
                3 => _ = col.typed::<DoubleType>().write_batch(&num(|t| t.exit_price), None, None)?,
                4 => _ = col.typed::<DoubleType>().write_batch(&num(|t| t.size), None, None)?,
                5 => _ = col.typed::<ByteArrayType>().write_batch(&text(|t| &t.side), None, None)?,
                6 => _ = col.typed::<ByteArrayType>().write_batch(&text(|t| &t.role), None, None)?,
                7 => _ = col.typed::<DoubleType>().write_batch(&num(|t| t.pnl), None, None)?,
                8 => _ = col.typed::<DoubleType>().write_batch(&num(|t| t.fee), None, None)?,
                9 => _ = col.typed::<DoubleType>().write_batch(&num(|t| t.return_pct), None, None)?,
                10 => _ = col.typed::<ByteArrayType>().write_batch(&text(|t| &t.bucket), None, None)?,
                11 => _ = col.typed::<ByteArrayType>().write_batch(&text(|t| &t.resolution), None, None)?,
                _ => bail!("unexpected parquet column {index}"),
            }
            col.close()?;
            index += 1;
        }
        rg.close()?;
    }
    writer.close()?;
    Ok(())
}

/// The error message for a parquet file that can't be created.
#[cfg(feature = "parquet")]
fn open_error_text(e: &std::io::Error, path: &str) -> String {
    format!("failed to open local file {path}: {e}")
}

/// Without the `parquet` feature there is no writer; this always fails.
#[cfg(not(feature = "parquet"))]
pub fn write_parquet_file(path: &Path, trades: &[ParquetTrade]) -> Result<()> {
    let _ = (path, trades);
    bail!("parquet support not compiled in: rebuild oddsmith with --features parquet")
}

const TRADES_FOR_RUN_SQL: &str = "
		SELECT run_id, market_id, entry_price, exit_price, size,
			side, role, pnl, return_pct, bucket_label, resolution
		FROM backtest_trades
		WHERE run_id = $1";

/// Parquet columns that can hold a NULL in `backtest_trades`, in schema order, with their
/// query column.
const NULLABLE: [(usize, &str); 5] =
    [(3, "exit_price"), (7, "pnl"), (8, "return_pct"), (9, "bucket"), (10, "resolution")];

/// A run's rows from `backtest_trades`. That table has no fee column, so fee stays 0.
/// NULLs read as 0 or "" and are remembered per row (a bit per [`NULLABLE`] column), so
/// writing fails when a required column holds nulls.
struct RunTrades {
    trades: Vec<ParquetTrade>,
    nulls: Vec<u8>,
}

impl RunTrades {
    /// Fails if a row in `lo..hi` had a NULL, naming the first such column in schema order.
    fn check(&self, lo: usize, hi: usize) -> Result<()> {
        let mask = self.nulls[lo..hi].iter().fold(0u8, |m, n| m | n);
        if mask != 0 {
            let col = NULLABLE[mask.trailing_zeros() as usize].1;
            bail!("Column '{col}' is declared non-nullable but contains nulls");
        }
        Ok(())
    }

    /// Write the trades to one file.
    fn write(&self, path: &Path) -> Result<()> {
        self.check(0, self.trades.len())?;
        write_parquet_file(path, &self.trades)
    }
}

fn trades_for_run(db: &Db, run_id: i64) -> Result<RunTrades> {
    let rows = db.query(TRADES_FOR_RUN_SQL, &[&run_id]).map_err(|e| anyhow!("querying trades: {e:#}"))?;
    let text = |r: &postgres::Row, i: usize| r.get::<_, Option<String>>(i).unwrap_or_default();
    let mut nulls = Vec::with_capacity(rows.len());
    let trades = rows
        .iter()
        .map(|r| {
            let mut mask = 0u8;
            for (bit, (i, _)) in NULLABLE.iter().enumerate() {
                let is_null = if *i >= 9 { r.get::<_, Option<String>>(*i).is_none() } else { r.opt_num(*i).is_none() };
                if is_null {
                    mask |= 1 << bit;
                }
            }
            nulls.push(mask);
            ParquetTrade {
                run_id: r.get(0),
                market_id: text(r, 1),
                entry_price: r.num(2),
                exit_price: r.num(3),
                size: r.num(4),
                side: text(r, 5),
                role: text(r, 6),
                pnl: r.num(7),
                fee: 0.0,
                return_pct: r.num(8),
                bucket: text(r, 9),
                resolution: text(r, 10),
            }
        })
        .collect();
    Ok(RunTrades { trades, nulls })
}

fn file_size(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path).map_err(|e| anyhow!("{}", os_error_text(&e, &path.to_string_lossy())))?.len())
}

fn mb(bytes: u64) -> String {
    oddsmith_core::textfmt::fixed(bytes as f64 / (1024.0 * 1024.0), 1)
}

fn verbose_log(verbose: bool) -> impl Fn(&str) {
    move |msg: &str| {
        if verbose {
            print!("{msg}");
        }
    }
}

/// Read `backtest_trades` for a run and write a parquet file locally. Returns the path and
/// the trade count.
pub fn export_run_to_parquet(db: &Db, run_id: i64, output_path: &str, verbose: bool) -> Result<(String, usize)> {
    let logf = verbose_log(verbose);
    logf(&format!("querying backtest_trades for run {run_id}...\n"));
    let run = trades_for_run(db, run_id)?;
    if run.trades.is_empty() {
        bail!("no trades found for run {run_id}");
    }
    logf(&format!("writing {} trades to {output_path}...\n", run.trades.len()));
    run.write(Path::new(output_path)).map_err(|e| anyhow!("writing parquet: {e:#}"))?;
    let size = file_size(Path::new(output_path))?;
    logf(&format!("wrote {output_path} ({} MB)\n", mb(size)));
    Ok((output_path.to_string(), run.trades.len()))
}

/// Export every run in `backtest_trades` to `{output_dir}/backtest_run_{id}.parquet`.
/// Per-run failures are printed and skipped.
pub fn export_all_runs_to_parquet(db: &Db, output_dir: &str, verbose: bool) -> Result<()> {
    for id in all_run_ids(db)? {
        let path = format!("{output_dir}/backtest_run_{id}.parquet");
        match export_run_to_parquet(db, id, &path, verbose) {
            Ok((_, count)) => {
                if verbose {
                    println!("  exported run {id}: {count} trades");
                }
            }
            Err(e) => println!("  warning: run {id}: {e:#}"),
        }
    }
    Ok(())
}

/// Every run id that has rows in `backtest_trades`, ascending.
pub fn all_run_ids(db: &Db) -> Result<Vec<i64>> {
    Ok(db
        .query("SELECT DISTINCT run_id FROM backtest_trades ORDER BY run_id", &[])?
        .iter()
        .map(|r| r.get::<_, i64>(0))
        .collect())
}

/// Upload a file to Supabase Storage (`POST {url}/storage/v1/object/{bucket}/{object}`),
/// upserting, with up to three attempts.
pub fn upload_to_supabase_storage(
    file_path: &str,
    bucket: &str,
    object_name: &str,
    supabase_url: &str,
    anon_key: &str,
) -> Result<()> {
    let upload_url = format!("{supabase_url}/storage/v1/object/{bucket}/{object_name}");
    let agent = oddsmith_store::http::agent(Duration::from_secs(600));
    let mut last_err = None;
    for attempt in 0..3u64 {
        if attempt > 0 {
            thread::sleep(Duration::from_secs(attempt * 2));
        }
        let data = fs::read(file_path).map_err(|e| anyhow!("opening file: {}", os_error_text(&e, file_path)))?;
        let resp = agent
            .post(&upload_url)
            .header("Authorization", &format!("Bearer {anon_key}"))
            .header("apikey", anon_key)
            .header("Content-Type", "application/octet-stream")
            .header("x-upsert", "true")
            .header("User-Agent", "oddsmith/0.1")
            .send(&data[..]);
        let mut resp = match resp {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(anyhow!("upload request: {e}"));
                continue;
            }
        };
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_vec().unwrap_or_default();
        if status >= 300 {
            last_err = Some(anyhow!("upload failed ({status}): {}", String::from_utf8_lossy(&body)));
            continue;
        }
        return Ok(());
    }
    match last_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Split point for large runs: about 250k trades makes a ~17MB file, well under
/// Supabase's 50MB object limit.
pub const MAX_TRADES_PER_PART: usize = 250_000;

/// Write trades to parquet file(s) under /tmp and upload them to the `backtest-data`
/// bucket, splitting into parts above [`MAX_TRADES_PER_PART`].
pub fn upload_parquet_chunked(
    trades: &[ParquetTrade],
    run_id: i64,
    supabase_url: &str,
    anon_key: &str,
    logf: &dyn Fn(&str),
) -> Result<()> {
    upload_chunked(trades, run_id, supabase_url, anon_key, logf, &|_, _| Ok(()))
}

/// [`upload_parquet_chunked`] with a check run on each file's row range before writing it.
fn upload_chunked(
    trades: &[ParquetTrade],
    run_id: i64,
    supabase_url: &str,
    anon_key: &str,
    logf: &dyn Fn(&str),
    check: &dyn Fn(usize, usize) -> Result<()>,
) -> Result<()> {
    if trades.len() <= MAX_TRADES_PER_PART {
        let tmp_file = format!("/tmp/backtest_run_{run_id}.parquet");
        check(0, trades.len())
            .and_then(|()| write_parquet_file(Path::new(&tmp_file), trades))
            .map_err(|e| anyhow!("writing parquet: {e:#}"))?;
        let result = (|| {
            let size = file_size(Path::new(&tmp_file))?;
            logf(&format!("  parquet file: {} MB ({} trades)\n", mb(size), trades.len()));
            let object_name = format!("backtest_run_{run_id}.parquet");
            logf(&format!("  uploading to supabase storage (backtest-data/{object_name})...\n"));
            upload_to_supabase_storage(&tmp_file, "backtest-data", &object_name, supabase_url, anon_key)
                .map_err(|e| anyhow!("uploading: {e:#}"))?;
            logf(&format!("  uploaded {} trades\n", trades.len()));
            Ok(())
        })();
        let _ = fs::remove_file(&tmp_file);
        return result;
    }

    let num_parts = trades.len().div_ceil(MAX_TRADES_PER_PART);
    logf(&format!("  splitting {} trades into {num_parts} parts...\n", trades.len()));
    for (i, chunk) in trades.chunks(MAX_TRADES_PER_PART).enumerate() {
        let part = i + 1;
        let tmp_file = format!("/tmp/backtest_run_{run_id}_part{part}.parquet");
        let lo = i * MAX_TRADES_PER_PART;
        check(lo, lo + chunk.len())
            .and_then(|()| write_parquet_file(Path::new(&tmp_file), chunk))
            .map_err(|e| anyhow!("writing parquet part {part}: {e:#}"))?;
        let size = file_size(Path::new(&tmp_file))?;
        logf(&format!("  part {part}/{num_parts}: {} MB ({} trades)\n", mb(size), chunk.len()));
        let object_name = format!("backtest_run_{run_id}_part{part}.parquet");
        let uploaded = upload_to_supabase_storage(&tmp_file, "backtest-data", &object_name, supabase_url, anon_key);
        let _ = fs::remove_file(&tmp_file);
        uploaded.map_err(|e| anyhow!("uploading part {part}: {e:#}"))?;
    }
    logf(&format!("  uploaded all {num_parts} parts\n"));
    Ok(())
}

/// Export a run to parquet and upload it to Supabase Storage.
pub fn export_and_upload(db: &Db, run_id: i64, supabase_url: &str, anon_key: &str, verbose: bool) -> Result<()> {
    let logf = verbose_log(verbose);
    logf(&format!("querying backtest_trades for run {run_id}...\n"));
    let run = trades_for_run(db, run_id)?;
    if run.trades.is_empty() {
        bail!("no trades found for run {run_id}");
    }
    logf(&format!("loaded {} trades for run {run_id}\n", run.trades.len()));
    upload_chunked(&run.trades, run_id, supabase_url, anon_key, &logf, &|lo, hi| run.check(lo, hi))
}

#[cfg(all(test, feature = "parquet"))]
mod tests {
    use super::*;

    #[test]
    fn writes_a_readable_file() {
        use parquet::file::reader::{FileReader, SerializedFileReader};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.parquet");
        let trades: Vec<ParquetTrade> = (0..5)
            .map(|i| ParquetTrade {
                run_id: 7,
                market_id: format!("m{i}"),
                entry_price: 0.5,
                side: "YES".into(),
                bucket: "50-55".into(),
                ..ParquetTrade::default()
            })
            .collect();
        write_parquet_file(&path, &trades).unwrap();
        let reader = SerializedFileReader::new(fs::File::open(&path).unwrap()).unwrap();
        let meta = reader.metadata();
        assert_eq!(meta.file_metadata().num_rows(), 5);
        let names: Vec<&str> = meta.file_metadata().schema_descr().columns().iter().map(|c| c.name()).collect();
        assert_eq!(
            names,
            [
                "run_id",
                "market_id",
                "entry_price",
                "exit_price",
                "size",
                "side",
                "role",
                "pnl",
                "fee",
                "return_pct",
                "bucket",
                "resolution"
            ]
        );
    }
}
