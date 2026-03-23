//! Events and markets tables.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use postgres::Row;

use crate::db::{Db, Num, Param, RowExt};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Event {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub description: String,
    pub category: String,
    pub sub_category: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub active: bool,
    pub closed: bool,
    pub volume: f64,
    pub liquidity: f64,
    pub tags: Option<serde_json::Value>,
    pub raw_json: Option<serde_json::Value>,
}

/// A market row. Text columns read as `""` when NULL.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Market {
    pub id: String,
    pub event_id: String,
    pub question: String,
    pub slug: String,
    pub clob_token_yes: String,
    pub clob_token_no: String,
    pub condition_id: String,
    pub active: bool,
    pub closed: bool,
    pub resolved: bool,
    pub resolution: Option<String>,
    pub end_date: Option<DateTime<Utc>>,
    pub volume: f64,
    pub liquidity: f64,
    pub last_trade_price: f64,
    pub best_bid: f64,
    pub best_ask: f64,
    pub spread: f64,
    pub race_type: String,
    pub state: String,
    pub candidate: String,
    pub party: String,
    /// Written on upsert; not read back by the queries here.
    pub raw_json: Option<serde_json::Value>,
}

const UPSERT_MARKET: &str = "
    INSERT INTO markets (id, event_id, question, slug, clob_token_id_yes, clob_token_id_no, condition_id,
        active, closed, resolved, resolution, end_date, volume, liquidity,
        last_trade_price, best_bid, best_ask, spread,
        race_type, state, candidate, party, raw_json)
    VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23)
    ON CONFLICT (id) DO UPDATE SET
        active=EXCLUDED.active, closed=EXCLUDED.closed, resolved=EXCLUDED.resolved,
        resolution=EXCLUDED.resolution, volume=EXCLUDED.volume, liquidity=EXCLUDED.liquidity,
        last_trade_price=EXCLUDED.last_trade_price, best_bid=EXCLUDED.best_bid,
        best_ask=EXCLUDED.best_ask, spread=EXCLUDED.spread,
        race_type=EXCLUDED.race_type, state=EXCLUDED.state, candidate=EXCLUDED.candidate,
        party=EXCLUDED.party, raw_json=EXCLUDED.raw_json, updated_at=NOW()";

const MARKET_COLUMNS: &str = "id, event_id, question, slug, clob_token_id_yes, clob_token_id_no, condition_id,
    active, closed, resolved, resolution, end_date, volume, liquidity,
    last_trade_price, best_bid, best_ask, spread,
    race_type, state, candidate, party";

/// The owned parameter values for one market upsert.
struct MarketParams {
    nums: [Num; 6],
}

impl MarketParams {
    fn new(m: &Market) -> Self {
        MarketParams {
            nums: [
                Num(m.volume),
                Num(m.liquidity),
                Num(m.last_trade_price),
                Num(m.best_bid),
                Num(m.best_ask),
                Num(m.spread),
            ],
        }
    }

    fn params<'a>(&'a self, m: &'a Market) -> [Param<'a>; 23] {
        [
            &m.id,
            &m.event_id,
            &m.question,
            &m.slug,
            &m.clob_token_yes,
            &m.clob_token_no,
            &m.condition_id,
            &m.active,
            &m.closed,
            &m.resolved,
            &m.resolution,
            &m.end_date,
            &self.nums[0],
            &self.nums[1],
            &self.nums[2],
            &self.nums[3],
            &self.nums[4],
            &self.nums[5],
            &m.race_type,
            &m.state,
            &m.candidate,
            &m.party,
            &m.raw_json,
        ]
    }
}

fn text(r: &Row, i: usize) -> String {
    r.get::<_, Option<String>>(i).unwrap_or_default()
}

fn flag(r: &Row, i: usize) -> bool {
    r.get::<_, Option<bool>>(i).unwrap_or_default()
}

fn market_from_row(r: &Row) -> Market {
    Market {
        id: text(r, 0),
        event_id: text(r, 1),
        question: text(r, 2),
        slug: text(r, 3),
        clob_token_yes: text(r, 4),
        clob_token_no: text(r, 5),
        condition_id: text(r, 6),
        active: flag(r, 7),
        closed: flag(r, 8),
        resolved: flag(r, 9),
        resolution: r.get(10),
        end_date: r.get(11),
        volume: r.num(12),
        liquidity: r.num(13),
        last_trade_price: r.num(14),
        best_bid: r.num(15),
        best_ask: r.num(16),
        spread: r.num(17),
        race_type: text(r, 18),
        state: text(r, 19),
        candidate: text(r, 20),
        party: text(r, 21),
        raw_json: None,
    }
}

impl Db {
    pub fn upsert_event(&self, e: &Event) -> Result<()> {
        let (volume, liquidity) = (Num(e.volume), Num(e.liquidity));
        self.exec(
            "
            INSERT INTO events (id, title, slug, description, category, sub_category, starts_at, ends_at,
                active, closed, volume, liquidity, tags, raw_json)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
            ON CONFLICT (id) DO UPDATE SET
                title=EXCLUDED.title, active=EXCLUDED.active, closed=EXCLUDED.closed,
                volume=EXCLUDED.volume, liquidity=EXCLUDED.liquidity, tags=EXCLUDED.tags,
                raw_json=EXCLUDED.raw_json, updated_at=NOW()",
            &[
                &e.id,
                &e.title,
                &e.slug,
                &e.description,
                &e.category,
                &e.sub_category,
                &e.starts_at,
                &e.ends_at,
                &e.active,
                &e.closed,
                &volume,
                &liquidity,
                &e.tags,
                &e.raw_json,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_market(&self, m: &Market) -> Result<()> {
        let p = MarketParams::new(m);
        self.exec(UPSERT_MARKET, &p.params(m))?;
        Ok(())
    }

    /// Upsert many markets in one transaction.
    pub fn batch_upsert_markets(&self, markets: &[Market]) -> Result<()> {
        self.tx(|tx| {
            let stmt = tx.prepare(UPSERT_MARKET)?;
            for m in markets {
                let p = MarketParams::new(m);
                tx.execute(&stmt, &p.params(m))?;
            }
            Ok(())
        })
        .context("batch upsert markets")
    }

    /// Closed, resolved markets with a resolution, optionally for one race type.
    pub fn get_resolved_markets(&self, race_filter: &str) -> Result<Vec<Market>> {
        let mut sql = format!(
            "SELECT {MARKET_COLUMNS} FROM markets WHERE closed = true AND resolved = true AND resolution IS NOT NULL"
        );
        self.markets_query(&mut sql, race_filter).context("querying resolved markets")
    }

    /// Active, open markets, optionally for one race type.
    pub fn get_active_markets(&self, race_filter: &str) -> Result<Vec<Market>> {
        let mut sql = format!("SELECT {MARKET_COLUMNS} FROM markets WHERE active = true AND closed = false");
        self.markets_query(&mut sql, race_filter).context("querying active markets")
    }

    fn markets_query(&self, sql: &mut String, race_filter: &str) -> Result<Vec<Market>> {
        let rows = if race_filter.is_empty() {
            self.query(sql, &[])?
        } else {
            sql.push_str(" AND race_type = $1");
            self.query(sql, &[&race_filter])?
        };
        Ok(rows.iter().map(market_from_row).collect())
    }
}
