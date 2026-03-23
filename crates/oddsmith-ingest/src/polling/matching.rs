//! Match polls to Polymarket markets.

use anyhow::{Result, bail};
use oddsmith_store::Db;
use oddsmith_store::markets::Market;
use oddsmith_store::polls::PollMarketMatch;

use crate::text::{equal_fold, lower, split_ws};

/// Match polls to markets by race type, state and candidate name, upserting a
/// `poll_market_matches` row per match. Returns the number of matches written.
pub fn match_polls_to_markets(db: &Db) -> Result<u64> {
    let mut markets = db.get_resolved_markets("")?;
    let active = db.get_active_markets("")?;
    let n_active = active.len();
    markets.extend(active);
    println!("  loaded {} markets ({} resolved, {} active)", markets.len(), markets.len() - n_active, n_active);

    // Polymarket launched in 2020; earlier polls can't match any market.
    let keys = db.poll_keys_since("2020-01-01")?;
    println!("  found {} distinct (race_type, state, candidate) poll keys", keys.len());

    let mut matched = 0u64;
    let mut skipped = 0u64;
    for (i, (race_type, state, candidate)) in keys.iter().enumerate() {
        let mut key_matches = 0;
        for mkt in &markets {
            let conf = match_score(race_type, state.as_deref(), candidate, mkt)?;
            if conf < 0.3 {
                continue;
            }
            let m = PollMarketMatch {
                market_id: mkt.id.clone(),
                race_type: race_type.clone(),
                state: state.clone(),
                candidate: candidate.clone(),
                match_method: "auto".into(),
                confidence: conf,
                ..Default::default()
            };
            if db.upsert_poll_market_match(&m).is_err() {
                skipped += 1;
                continue;
            }
            matched += 1;
            key_matches += 1;
        }
        if (i + 1) % 100 == 0 || i + 1 == keys.len() {
            println!("  [{}/{}] candidates processed; {matched} matches so far", i + 1, keys.len());
        }
        if key_matches > 0 {
            let state = state.as_deref().unwrap_or("no state");
            println!("    {candidate} ({race_type}, {state}) → {key_matches} markets matched");
        }
    }

    if skipped > 0 {
        println!("  warning: {skipped} matches failed to insert");
    }
    Ok(matched)
}

/// Confidence that a poll key refers to a market: race type must match (+0.3); a state on
/// both sides must match (+0.3), no state on either side adds 0.2; the candidate's full name
/// in the question adds 0.4, else their last name adds 0.3.
///
/// A NULL poll state against a market with a state is an error.
pub fn match_score(race_type: &str, state: Option<&str>, candidate: &str, mkt: &Market) -> Result<f64> {
    let mut score = 0.0;

    if equal_fold(&mkt.race_type, race_type) {
        score += 0.3;
    } else {
        return Ok(0.0);
    }

    if state != Some("") && !mkt.state.is_empty() {
        let Some(state) = state else { bail!("missing poll state: market {} has state {}", mkt.id, mkt.state) };
        if equal_fold(&mkt.state, state) {
            score += 0.3;
        } else {
            return Ok(0.0);
        }
    } else if state == Some("") && mkt.state.is_empty() {
        score += 0.2;
    }

    let q = lower(&mkt.question);
    let cand = lower(candidate);
    if q.contains(&cand) {
        return Ok(score + 0.4);
    }
    if let Some(last_name) = split_ws(&cand).last() {
        if q.contains(last_name) {
            return Ok(score + 0.3);
        }
    }
    Ok(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkt(race: &str, state: &str, q: &str) -> Market {
        Market { race_type: race.into(), state: state.into(), question: q.into(), ..Default::default() }
    }

    fn score(race: &str, state: Option<&str>, cand: &str, m: &Market) -> f64 {
        match_score(race, state, cand, m).unwrap()
    }

    #[test]
    fn scores() {
        let pres = mkt("presidential", "", "Will Donald Trump win the 2024 election?");
        assert_eq!(score("presidential", Some(""), "Donald Trump", &pres), 0.3 + 0.2 + 0.4);
        assert_eq!(score("Presidential", Some(""), "Melania Trump", &pres), 0.3 + 0.2 + 0.3);
        assert_eq!(score("presidential", Some(""), "Joe Biden", &pres), 0.5);
        assert_eq!(score("senate", Some(""), "Donald Trump", &pres), 0.0);
        // Poll has a state, national market: no state bonus or penalty.
        assert_eq!(score("presidential", Some("PA"), "Donald Trump", &pres), 0.3 + 0.4);
        assert_eq!(score("presidential", None, "Nobody", &pres), 0.3);

        let pa = mkt("senate", "PA", "Will Dave McCormick win the Pennsylvania Senate race?");
        assert_eq!(score("senate", Some("pa"), "Dave McCormick", &pa), 0.3 + 0.3 + 0.4);
        assert_eq!(score("senate", Some("OH"), "Dave McCormick", &pa), 0.0);
        assert_eq!(score("senate", Some(""), "Dave McCormick", &pa), 0.3 + 0.4);
        assert!(match_score("senate", None, "Dave McCormick", &pa).is_err());
        assert_eq!(score("senate", Some("PA"), "", &pa), 0.3 + 0.3 + 0.4);
    }

    /// State extraction feeds matching: a West Virginia market must not match Virginia polls.
    #[test]
    fn west_virginia_is_not_virginia() {
        let q = "Will Jim Justice win the West Virginia Senate race?";
        let wv = mkt("senate", &crate::polymarket::gamma::extract_state(q), q);
        assert_eq!(wv.state, "WV");
        assert_eq!(score("senate", Some("VA"), "Jim Justice", &wv), 0.0);
        assert_eq!(score("senate", Some("WV"), "Jim Justice", &wv), 1.0);
    }
}
