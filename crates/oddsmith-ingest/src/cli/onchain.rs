//! `oddsmith ingest onchain-transfers`.

use std::fs;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use chrono::Utc;
use clap::Args;
use oddsmith_store::onchain::OnchainTransferRow;
use oddsmith_store::{Ctx, Db};

use crate::chain::alchemy::{
    AlchemyClient, AssetTransfer, TransferDirection, asset_label, dedupe_id, paginate, parse_block_num,
    parse_block_time, value_raw,
};
use crate::chain::blocklist::{is_polymarket_protocol, normalize_address};
use crate::polling::csv::split_lines;
use crate::polymarket::ratelimit::RateLimiter;
use crate::text::strip_ws;

#[derive(Debug, Args)]
pub struct OnchainTransfersArgs {
    /// take top N wallets by total_volume from wallet_profiles (0 = do not use)
    #[arg(long = "top", default_value_t = 0, hide_default_value = true, allow_negative_numbers = true)]
    pub top_n: i64,
    /// path to file with one 0x address per line
    #[arg(long, default_value = "", hide_default_value = true)]
    pub addresses_file: String,
    /// both, out, or in (Alchemy fromAddress / toAddress)
    #[arg(long, default_value = "both")]
    pub direction: String,
    /// max result pages per wallet per direction (Alchemy pagination)
    #[arg(long, default_value_t = 100, allow_negative_numbers = true)]
    pub max_pages: i64,
    /// rows per DB batch insert
    #[arg(long, default_value_t = 300, allow_negative_numbers = true)]
    pub batch_rows: i64,
    /// delete all rows in wallet_onchain_transfers before ingest
    #[arg(long)]
    pub clear: bool,
}

pub fn run(a: &OnchainTransfersArgs, ctx: &Ctx) -> Result<()> {
    let cfg = ctx.config()?;
    if cfg.alchemy_polygon_url.is_empty() {
        bail!("ALCHEMY_POLYGON_URL is required (full https URL with API key)");
    }

    let db = ctx.connect(&cfg)?;
    let addrs = resolve_seed_addresses(&db, a.top_n, &a.addresses_file)?;
    if addrs.is_empty() {
        bail!("no seed addresses: use --top N (needs wallet_profiles) or --addresses-file");
    }

    if a.clear {
        db.exec("DELETE FROM wallet_onchain_transfers", &[]).map_err(|e| anyhow!("clear transfers: {e:#}"))?;
        if cfg.verbose {
            println!("  cleared wallet_onchain_transfers");
        }
    }

    let ac = AlchemyClient::new(&cfg.alchemy_polygon_url, Arc::new(RateLimiter::new()));

    let dirs = match a.direction.to_lowercase().as_str() {
        "out" => vec![TransferDirection::Out],
        "in" => vec![TransferDirection::In],
        "both" => vec![TransferDirection::Out, TransferDirection::In],
        _ => bail!("invalid --direction \"{}\" (use both, out, in)", a.direction),
    };

    let batch_rows = if a.batch_rows <= 0 { 300 } else { a.batch_rows as usize };

    let mut total_inserted = 0u64;
    for (wi, w) in addrs.iter().enumerate() {
        let w = normalize_address(w);
        if w.is_empty() {
            continue;
        }
        if cfg.verbose {
            let prefix = if w.chars().count() > 12 {
                format!("{}...", w.chars().take(10).collect::<String>())
            } else {
                w.clone()
            };
            println!("  [{}/{}] {prefix}", wi + 1, addrs.len());
        }

        for &d in &dirs {
            paginate(
                a.max_pages,
                |page_key| ac.fetch_asset_transfers(&w, d, page_key, 1000).map_err(|e| anyhow!("wallet {w}: {e:#}")),
                |transfers| {
                    let mut batch: Vec<OnchainTransferRow> = Vec::new();
                    for t in transfers {
                        let Some(row) = transfer_to_row(&w, t, d) else { continue };
                        batch.push(row);
                        if batch.len() >= batch_rows {
                            total_inserted += db.batch_insert_onchain_transfers(&batch)?;
                            batch.clear();
                        }
                    }
                    if !batch.is_empty() {
                        total_inserted += db.batch_insert_onchain_transfers(&batch)?;
                    }
                    Ok(())
                },
            )?;
        }
    }

    let edge_n = db.rebuild_onchain_edges().map_err(|e| anyhow!("rebuild edges: {e:#}"))?;
    println!("done: {total_inserted} new transfer rows (deduped inserts), {edge_n} wallet_onchain_edges");
    Ok(())
}

/// Seed addresses from `--addresses-file` then `--top`, normalized and deduplicated in order.
fn resolve_seed_addresses(db: &Db, top_n: i64, addr_file: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    if !addr_file.is_empty() {
        let bytes = fs::read(addr_file).map_err(|e| anyhow!("{e}: '{addr_file}'"))?;
        for raw in split_lines(&String::from_utf8_lossy(&bytes)) {
            let line = strip_ws(raw);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let a = normalize_address(line);
            if !a.is_empty() {
                out.push(a);
            }
        }
    }
    if top_n > 0 {
        for a in db.top_wallet_addresses(top_n)? {
            let n = normalize_address(&a);
            if !n.is_empty() {
                out.push(n);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|a| seen.insert(a.clone()));
    Ok(out)
}

/// The stored row for one transfer seen from `seed`, or `None` if it should be dropped
/// (self-transfer, protocol contract counterparty, or unparseable block number).
pub fn transfer_to_row(seed: &str, t: &AssetTransfer, d: TransferDirection) -> Option<OnchainTransferRow> {
    let counterparty = match d {
        TransferDirection::Out => normalize_address(&t.to),
        TransferDirection::In => normalize_address(&t.from),
    };
    if counterparty.is_empty() || counterparty == seed || is_polymarket_protocol(&counterparty) {
        return None;
    }
    let block_number = parse_block_num(&t.block_num).ok()?;
    let block_time = parse_block_time(&t.metadata.block_timestamp).ok().map(|bt| bt.with_timezone(&Utc));
    let direction = d.as_str();
    Some(OnchainTransferRow {
        dedupe_id: format!("{}:{seed}:{direction}", dedupe_id(t)),
        seed_wallet: seed.to_string(),
        counterparty,
        direction: direction.to_string(),
        tx_hash: strip_ws(&t.hash).to_lowercase(),
        block_number,
        block_time,
        asset: asset_label(t),
        category: t.category.clone(),
        value_raw: value_raw(t),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::alchemy::decode_transfers_response;

    const SEED: &str = "0x1111111111111111111111111111111111111111";

    #[test]
    fn rows_from_transfers() {
        let body = br#"{"result":{"transfers":[
            {"blockNum":"0x10","hash":" 0xABC ","from":"0x1111111111111111111111111111111111111111",
             "to":"0x2222222222222222222222222222222222222222","category":"erc20","asset":"USDC",
             "rawContract":{"value":"0x64","address":"0xC0FFEE0000000000000000000000000000000000"},
             "metadata":{"blockTimestamp":"2024-01-01T00:00:00Z"}},
            {"blockNum":"0x11","hash":"0xdef","from":"0x1111111111111111111111111111111111111111",
             "to":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","category":"erc20"},
            {"blockNum":"0x12","hash":"0xdef","from":"0x1111111111111111111111111111111111111111",
             "to":"0x1111111111111111111111111111111111111111","category":"external"},
            {"blockNum":"zz","hash":"0xdef","from":"0x1111111111111111111111111111111111111111",
             "to":"0x3333333333333333333333333333333333333333","category":"external"},
            {"blockNum":"0x13","hash":"0x99","from":"0x1111111111111111111111111111111111111111",
             "to":"0x3333333333333333333333333333333333333333","category":"external","asset":"",
             "metadata":{"blockTimestamp":"not a time"}}
        ]}}"#;
        let (ts, _) = decode_transfers_response(body).unwrap();
        let rows: Vec<_> = ts.iter().filter_map(|t| transfer_to_row(SEED, t, TransferDirection::Out)).collect();
        assert_eq!(rows.len(), 2);
        let r = &rows[0];
        assert_eq!(r.counterparty, "0x2222222222222222222222222222222222222222");
        assert_eq!(r.tx_hash, "0xabc");
        assert_eq!(r.block_number, 16);
        assert_eq!(r.value_raw, "100");
        assert_eq!(r.asset, "0xc0ffee0000000000000000000000000000000000");
        assert_eq!(
            r.dedupe_id,
            " 0xABC :0x1111111111111111111111111111111111111111:0x2222222222222222222222222222222222222222:0xc0ffee0000000000000000000000000000000000:0x64:0x1111111111111111111111111111111111111111:out"
        );
        assert_eq!(r.block_time.unwrap().to_rfc3339(), "2024-01-01T00:00:00+00:00");
        let r = &rows[1];
        assert_eq!((r.asset.as_str(), r.block_time), ("native", None));
        // Incoming direction uses `from` as the counterparty: here that's the seed itself.
        assert!(transfer_to_row(SEED, &ts[0], TransferDirection::In).is_none());
    }
}
