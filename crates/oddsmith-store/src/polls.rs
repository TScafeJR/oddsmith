//! Polls and poll-to-market matches.

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, NaiveDate};

use crate::db::{Db, Num, RowExt};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Poll {
    pub id: i64,
    pub source: String,
    pub race_type: String,
    pub state: String,
    pub candidate: String,
    pub party: String,
    pub poll_pct: f64,
    pub implied_prob: f64,
    pub pollster: String,
    pub sample_size: Option<i64>,
    pub methodology: String,
    /// The poll's date as parsed (midnight UTC for date-only formats). On insert it is bound
    /// as `timestamptz` and cast to the DATE column in the session time zone; on read it is
    /// the DATE at midnight UTC.
    pub poll_date: Option<DateTime<FixedOffset>>,
    pub market_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PollMarketMatch {
    pub id: i64,
    pub market_id: String,
    pub race_type: String,
    /// `None` stores NULL (poll keys can have a NULL state).
    pub state: Option<String>,
    pub candidate: String,
    pub match_method: String,
    pub confidence: f64,
}

fn date_at_midnight_utc(d: NaiveDate) -> Option<DateTime<FixedOffset>> {
    d.and_hms_opt(0, 0, 0)?.and_local_timezone(FixedOffset::east_opt(0)?).single()
}

impl Db {
    /// Insert polls in one transaction. Returns the number of polls given.
    pub fn batch_insert_polls(&self, polls: &[Poll]) -> Result<u64> {
        if polls.is_empty() {
            return Ok(0);
        }
        self.tx(|tx| {
            let stmt = tx.prepare(
                "
                INSERT INTO polls (source, race_type, state, candidate, party, poll_pct, implied_prob,
                    pollster, sample_size, methodology, poll_date, market_id)
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9::int8,$10,$11::timestamptz,$12)",
            )?;
            for p in polls {
                let (pct, prob) = (Num(p.poll_pct), Num(p.implied_prob));
                tx.execute(
                    &stmt,
                    &[
                        &p.source,
                        &p.race_type,
                        &p.state,
                        &p.candidate,
                        &p.party,
                        &pct,
                        &prob,
                        &p.pollster,
                        &p.sample_size,
                        &p.methodology,
                        &p.poll_date,
                        &p.market_id,
                    ],
                )?;
            }
            Ok(())
        })
        .context("batch insert polls")?;
        Ok(polls.len() as u64)
    }

    pub fn upsert_poll_market_match(&self, m: &PollMarketMatch) -> Result<()> {
        self.exec(
            "
            INSERT INTO poll_market_matches (market_id, race_type, state, candidate, match_method, confidence)
            VALUES ($1,$2,$3,$4,$5,$6)
            ON CONFLICT (market_id, candidate) DO UPDATE SET
                match_method=EXCLUDED.match_method, confidence=EXCLUDED.confidence",
            &[&m.market_id, &m.race_type, &m.state, &m.candidate, &m.match_method, &Num(m.confidence)],
        )?;
        Ok(())
    }

    /// Distinct `(race_type, state, candidate)` keys of polls dated `since` or later.
    pub fn poll_keys_since(&self, since: &str) -> Result<Vec<(String, Option<String>, String)>> {
        let rows = self.query(
            "
        SELECT DISTINCT race_type, state, candidate FROM polls
        WHERE poll_date >= $1::text::date",
            &[&since],
        )?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect())
    }

    /// Polls matched to a market, newest first.
    pub fn get_polls_for_market(&self, market_id: &str) -> Result<Vec<Poll>> {
        let rows = self
            .query(
                "
                SELECT p.id, p.source, p.race_type, p.state, p.candidate, p.party,
                    p.poll_pct, p.implied_prob, p.pollster, p.sample_size, p.methodology, p.poll_date
                FROM polls p
                JOIN poll_market_matches m ON m.market_id = $1
                    AND m.race_type = p.race_type
                    AND (m.state = p.state OR (m.state IS NULL AND p.state IS NULL))
                    AND m.candidate = p.candidate
                ORDER BY p.poll_date DESC",
                &[&market_id],
            )
            .context("querying polls for market")?;
        Ok(rows
            .iter()
            .map(|r| {
                let text = |i: usize| r.get::<_, Option<String>>(i).unwrap_or_default();
                Poll {
                    id: r.get(0),
                    source: text(1),
                    race_type: text(2),
                    state: text(3),
                    candidate: text(4),
                    party: text(5),
                    poll_pct: r.num(6),
                    implied_prob: r.num(7),
                    pollster: text(8),
                    sample_size: r.get::<_, Option<i32>>(9).map(i64::from),
                    methodology: text(10),
                    poll_date: r.get::<_, Option<NaiveDate>>(11).and_then(date_at_midnight_utc),
                    market_id: None,
                }
            })
            .collect())
    }
}
