//! Trades table: inserts and per-market reads. The backtest query lives in `backtest.rs`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use postgres::Row;

use crate::db::{Db, Num, RowExt};

/// Default 10-cent bucket label for a price, e.g. `0.63 -> "60-70"`.
pub fn price_bucket(price: f64) -> String {
    let mut bucket = (price * 10.0).floor() as i64 * 10;
    if bucket >= 100 {
        bucket = 90;
    }
    format!("{bucket}-{}", bucket + 10)
}

/// A trades row. Text columns read as `""` when NULL.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Trade {
    pub id: String,
    pub market_id: String,
    pub event_id: String,
    pub asset_id: String,
    pub side: String,
    pub price: f64,
    pub size: f64,
    pub is_taker_trade: Option<bool>,
    pub maker_address: String,
    pub taker_address: String,
    pub outcome: String,
    pub transaction_hash: String,
    /// Filled from [`price_bucket`] on insert when empty.
    pub bucket_label: String,
    pub traded_at: Option<DateTime<Utc>>,
}

const TRADE_COLUMNS: &str = "id, market_id, event_id, asset_id, side, price, size,
    is_taker_trade, maker_address, taker_address, outcome, transaction_hash,
    bucket_label, traded_at";

fn trade_from_row(r: &Row) -> Trade {
    let text = |i: usize| r.get::<_, Option<String>>(i).unwrap_or_default();
    Trade {
        id: text(0),
        market_id: text(1),
        event_id: text(2),
        asset_id: text(3),
        side: text(4),
        price: r.num(5),
        size: r.num(6),
        is_taker_trade: r.get(7),
        maker_address: text(8),
        taker_address: text(9),
        outcome: text(10),
        transaction_hash: text(11),
        bucket_label: text(12),
        traded_at: r.get(13),
    }
}

impl Db {
    /// Insert trades in one transaction, skipping ids that already exist. Returns the number
    /// of rows inserted.
    pub fn batch_insert_trades(&self, trades: &[Trade]) -> Result<u64> {
        if trades.is_empty() {
            return Ok(0);
        }
        self.tx(|tx| {
            let stmt = tx.prepare(&format!(
                "
                INSERT INTO trades ({TRADE_COLUMNS})
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
                ON CONFLICT (id) DO NOTHING"
            ))?;
            let mut n = 0;
            for t in trades {
                let bucket = if t.bucket_label.is_empty() { price_bucket(t.price) } else { t.bucket_label.clone() };
                let (price, size) = (Num(t.price), Num(t.size));
                n += tx.execute(
                    &stmt,
                    &[
                        &t.id,
                        &t.market_id,
                        &t.event_id,
                        &t.asset_id,
                        &t.side,
                        &price,
                        &size,
                        &t.is_taker_trade,
                        &t.maker_address,
                        &t.taker_address,
                        &t.outcome,
                        &t.transaction_hash,
                        &bucket,
                        &t.traded_at,
                    ],
                )?;
            }
            Ok(n)
        })
        .context("batch insert trades")
    }

    /// Trades for one market inside a YES-probability band, oldest first.
    ///
    /// - `YES`: YES-token trades with price in `[min_price, max_price]`
    /// - `NO`: NO-token trades whose implied YES probability (1 - price) is in the band,
    ///   i.e. NO-token price in `[1 - max_price, 1 - min_price]`
    pub fn get_trades_for_market(
        &self,
        market_id: &str,
        min_price: f64,
        max_price: f64,
        side: &str,
    ) -> Result<Vec<Trade>> {
        let (q_min, q_max, outcome_clause) = if side == "NO" {
            (1.0 - max_price, 1.0 - min_price, "outcome = 'No'")
        } else {
            (min_price, max_price, "outcome != 'No'")
        };
        let sql = format!(
            "
            SELECT {TRADE_COLUMNS}
            FROM trades
            WHERE market_id = $1 AND price >= $2 AND price <= $3
                AND {outcome_clause}
            ORDER BY traded_at"
        );
        let rows = self.query(&sql, &[&market_id, &Num(q_min), &Num(q_max)]).context("querying trades")?;
        Ok(rows.iter().map(trade_from_row).collect())
    }

    /// Trade counts per bucket label, ordered by label (NULL labels last).
    pub fn get_trade_count_by_bucket(&self) -> Result<Vec<(Option<String>, i64)>> {
        let rows =
            self.query("SELECT bucket_label, COUNT(*) FROM trades GROUP BY bucket_label ORDER BY bucket_label", &[])?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    /// Number of trades already stored for a market.
    pub fn count_trades_for_market(&self, market_id: &str) -> Result<i64> {
        let row = self.query_one("SELECT COUNT(*) FROM trades WHERE market_id = $1 LIMIT 1", &[&market_id])?;
        Ok(row.and_then(|r| r.get::<_, Option<i64>>(0)).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets() {
        assert_eq!(price_bucket(0.63), "60-70");
        assert_eq!(price_bucket(0.0), "0-10");
        assert_eq!(price_bucket(1.0), "90-100");
        assert_eq!(price_bucket(0.999), "90-100");
    }
}
