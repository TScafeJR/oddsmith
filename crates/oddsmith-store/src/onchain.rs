//! On-chain transfers and the aggregated wallet edges.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::db::Db;

/// One row for `wallet_onchain_transfers`.
#[derive(Debug, Clone, PartialEq)]
pub struct OnchainTransferRow {
    pub dedupe_id: String,
    pub seed_wallet: String,
    pub counterparty: String,
    pub direction: String,
    pub tx_hash: String,
    pub block_number: i64,
    pub block_time: Option<DateTime<Utc>>,
    pub asset: String,
    pub category: String,
    /// Decimal string for the NUMERIC column.
    pub value_raw: String,
}

impl Db {
    /// Insert transfers in one transaction, skipping known dedupe ids. Returns rows inserted.
    pub fn batch_insert_onchain_transfers(&self, rows: &[OnchainTransferRow]) -> Result<u64> {
        if rows.is_empty() {
            return Ok(0);
        }
        self.tx(|tx| {
            let stmt = tx.prepare(
                "
                INSERT INTO wallet_onchain_transfers (
                    dedupe_id, seed_wallet, counterparty, direction, tx_hash,
                    block_number, block_time, asset, category, value_raw)
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9, $10::text::numeric)
                ON CONFLICT (dedupe_id) DO NOTHING",
            )?;
            let mut n = 0;
            for r in rows {
                n += tx.execute(
                    &stmt,
                    &[
                        &r.dedupe_id,
                        &r.seed_wallet,
                        &r.counterparty,
                        &r.direction,
                        &r.tx_hash,
                        &r.block_number,
                        &r.block_time,
                        &r.asset,
                        &r.category,
                        &r.value_raw,
                    ],
                )?;
            }
            Ok(n)
        })
        .context("batch insert onchain transfer")
    }

    /// Rebuild `wallet_onchain_edges` from `wallet_onchain_transfers`; returns the edge count.
    pub fn rebuild_onchain_edges(&self) -> Result<u64> {
        self.tx(|tx| {
            tx.execute("DELETE FROM wallet_onchain_edges", &[])?;
            let n = tx.execute(
                "
                INSERT INTO wallet_onchain_edges (wallet_a, wallet_b, transfer_count, first_seen, last_seen, link_score)
                SELECT
                    LEAST(seed_wallet, counterparty),
                    GREATEST(seed_wallet, counterparty),
                    COUNT(*)::INTEGER,
                    MIN(block_time),
                    MAX(block_time),
                    LEAST(1.0::NUMERIC,
                        (COUNT(*)::NUMERIC / GREATEST(COUNT(DISTINCT tx_hash), 1)) *
                        LEAST(1.0::NUMERIC, COUNT(*)::NUMERIC / 20.0))
                FROM wallet_onchain_transfers
                WHERE seed_wallet != counterparty
                GROUP BY LEAST(seed_wallet, counterparty), GREATEST(seed_wallet, counterparty)
                HAVING COUNT(*) >= 1",
                &[],
            )?;
            Ok(n)
        })
    }

    /// Up to `limit` addresses from `wallet_profiles`, highest `total_volume` first.
    pub fn top_wallet_addresses(&self, limit: i64) -> Result<Vec<String>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let rows = self
            .query("SELECT address FROM wallet_profiles ORDER BY total_volume DESC NULLS LAST LIMIT $1", &[&limit])?;
        Ok(rows.iter().filter_map(|r| r.get::<_, Option<String>>(0)).filter(|a| !a.is_empty()).collect())
    }

    pub fn count_onchain_edges(&self) -> Result<i64> {
        let row = self.query_one("SELECT COUNT(*) FROM wallet_onchain_edges", &[])?;
        Ok(row.map(|r| r.get::<_, i64>(0)).unwrap_or(0))
    }
}
