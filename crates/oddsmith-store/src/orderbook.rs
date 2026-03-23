//! Order book snapshots and price history.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::db::{Db, Num};

#[derive(Debug, Clone, PartialEq)]
pub struct OrderbookSnapshot {
    pub id: i64,
    pub market_id: String,
    pub asset_id: String,
    pub snapshot_at: DateTime<Utc>,
    pub mid_price: f64,
    pub spread: f64,
    pub bid_depth_5: f64,
    pub ask_depth_5: f64,
    pub best_bid: f64,
    pub best_ask: f64,
    pub bids_json: Option<serde_json::Value>,
    pub asks_json: Option<serde_json::Value>,
}

impl Db {
    pub fn insert_orderbook_snapshot(&self, s: &OrderbookSnapshot) -> Result<()> {
        let nums = [s.mid_price, s.spread, s.bid_depth_5, s.ask_depth_5, s.best_bid, s.best_ask].map(Num);
        self.exec(
            "
                INSERT INTO orderbook_snapshots (market_id, asset_id, snapshot_at, mid_price, spread,
                    bid_depth_5pct, ask_depth_5pct, best_bid, best_ask, bids_json, asks_json)
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
            &[
                &s.market_id,
                &s.asset_id,
                &s.snapshot_at,
                &nums[0],
                &nums[1],
                &nums[2],
                &nums[3],
                &nums[4],
                &nums[5],
                &s.bids_json,
                &s.asks_json,
            ],
        )
        .context("inserting orderbook snapshot")?;
        Ok(())
    }

    /// Insert one price point; an existing `(market, asset, interval, timestamp)` is kept.
    pub fn insert_price_history(
        &self,
        market_id: &str,
        asset_id: &str,
        interval: &str,
        ts: DateTime<Utc>,
        price: f64,
    ) -> Result<()> {
        self.exec(
            "
            INSERT INTO price_history (market_id, asset_id, interval, timestamp, price)
            VALUES ($1,$2,$3,$4,$5)
            ON CONFLICT (market_id, asset_id, interval, timestamp) DO NOTHING",
            &[&market_id, &asset_id, &interval, &ts, &Num(price)],
        )?;
        Ok(())
    }
}
