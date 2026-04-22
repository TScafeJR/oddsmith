//! `oddsmith ingest` subcommands.

mod onchain;

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use oddsmith_store::markets::{Event, Market};
use oddsmith_store::trades::Trade;
use oddsmith_store::{Ctx, Db, price_bucket};
use serde_json::{Value as Json, json};

pub use onchain::OnchainTransfersArgs;

use crate::polling::ingest::parse_directory;
use crate::polling::matching::match_polls_to_markets;
use crate::polling::transform::compute_implied_probabilities;
use crate::polymarket::clob::DataApiClient;
use crate::polymarket::decode::Number;
use crate::polymarket::gamma::{GammaClient, GammaEvent, GammaMarket, GammaTag, extract_market_type, extract_state};
use crate::polymarket::ratelimit::RateLimiter;
use crate::text::{equal_fold, lower, parse_rfc3339, strip_ws};

/// Ingest data from Polymarket, Polygon and polling sources.
#[derive(Debug, Subcommand)]
pub enum IngestCommand {
    /// Discover and ingest markets from Polymarket
    Markets(MarketsArgs),
    /// Fetch historical trades for ingested markets
    Trades(TradesArgs),
    /// Import polling data from CSV files (538, RCP)
    Polls(PollsArgs),
    /// Auto-match polls to Polymarket markets by race/state/candidate
    Match(MatchArgs),
    /// Ingest Polygon asset transfers for Polymarket wallets (Alchemy)
    #[command(
        name = "onchain-transfers",
        long_about = "Requires ALCHEMY_POLYGON_URL. Seeds from wallet_profiles (--top) or --addresses-file. \
                      Filters known Polymarket protocol contracts."
    )]
    OnchainTransfers(OnchainTransfersArgs),
}

#[derive(Debug, Args)]
pub struct MarketsArgs {
    /// market domain: politics, sports
    #[arg(long, default_value = "politics")]
    pub domain: String,
    /// filter by market type: presidential,senate,nba,nfl (comma-separated)
    #[arg(long = "type", default_value = "", hide_default_value = true)]
    pub race_types: String,
    /// max events to fetch (0 = all)
    #[arg(long, default_value_t = 0, hide_default_value = true, allow_negative_numbers = true)]
    pub limit: i64,
}

#[derive(Debug, Args)]
pub struct TradesArgs {
    /// filter by race type
    #[arg(long = "race", default_value = "", hide_default_value = true)]
    pub race_types: String,
    /// concurrent fetch workers
    #[arg(long, default_value_t = 4, allow_negative_numbers = true)]
    pub workers: i64,
    /// trades per batch insert
    #[arg(long, default_value_t = 500, allow_negative_numbers = true)]
    pub batch_size: i64,
}

#[derive(Debug, Args)]
pub struct PollsArgs {
    /// polling data source: 538, rcp, manual
    #[arg(long, default_value = "538")]
    pub source: String,
    /// directory containing poll CSV files
    #[arg(long, default_value = "./data/polls")]
    pub data_dir: String,
}

#[derive(Debug, Args)]
pub struct MatchArgs {}

impl IngestCommand {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        match self {
            IngestCommand::Markets(a) => run_markets(a, ctx),
            IngestCommand::Trades(a) => run_trades(a, ctx),
            IngestCommand::Polls(a) => run_polls(a, ctx),
            IngestCommand::Match(_) => run_match(ctx),
            IngestCommand::OnchainTransfers(a) => onchain::run(a, ctx),
        }
    }
}

// ---- ingest markets ----

fn run_markets(a: &MarketsArgs, ctx: &Ctx) -> Result<()> {
    let cfg = ctx.config()?;
    let db = ctx.connect(&cfg)?;
    let gamma = GammaClient::new(&cfg.gamma_host, Arc::new(RateLimiter::new()));
    let domain = a.domain.as_str();
    let type_filter: Vec<&str> = if a.race_types.is_empty() { Vec::new() } else { a.race_types.split(',').collect() };

    println!("fetching {domain} events from Polymarket...");

    let (mut total_events, mut total_markets) = (0u64, 0u64);
    let mut offset: i64 = 0;
    let page_size: i64 = 100;

    loop {
        println!("  fetching page {} (offset {offset})...", offset.div_euclid(page_size) + 1);
        let events = gamma.fetch_events(domain, page_size, offset).context("fetching events")?;
        if events.is_empty() {
            println!("  no more events found");
            break;
        }
        println!("  got {} events, processing...", events.len());

        for evt in &events {
            // Skip sports events when ingesting politics.
            if domain == "politics" && is_sports_event(&evt.title) {
                if cfg.verbose {
                    println!("    skipping sports event: {}", evt.title);
                }
                continue;
            }

            let tag_slugs: Vec<Json> = evt.tags.iter().map(|t| Json::String(t.slug.clone())).collect();
            let sub_cat = extract_market_type(domain, &evt.title, &evt.tags);
            let store_evt = Event {
                id: evt.id.clone(),
                title: evt.title.clone(),
                slug: evt.slug.clone(),
                description: evt.description.clone(),
                category: domain.to_string(),
                sub_category: sub_cat.to_string(),
                starts_at: parse_time(&evt.start_date),
                ends_at: parse_time(&evt.end_date),
                active: evt.active,
                closed: evt.closed,
                volume: evt.volume.float64(),
                liquidity: evt.liquidity.float64(),
                tags: Some(Json::Array(tag_slugs)),
                raw_json: Some(gamma_event_json(evt)),
            };
            if let Err(e) = db.upsert_event(&store_evt) {
                println!("  warning: failed to upsert event {}: {e:#}", evt.id);
                continue;
            }
            total_events += 1;

            let mut mkt_count = 0;
            for mkt in &evt.markets {
                let market_type = extract_market_type(domain, &mkt.question, &mkt.tags);
                if !type_filter.is_empty() && !type_filter.iter().any(|tf| equal_fold(market_type, strip_ws(tf))) {
                    continue;
                }
                let (yes_token, no_token) = mkt.parse_clob_token_ids().unwrap_or_default();
                let (resolved, resolution) = mkt.infer_resolution();
                let store_mkt = Market {
                    id: mkt.id.clone(),
                    event_id: evt.id.clone(),
                    question: mkt.question.clone(),
                    slug: mkt.slug.clone(),
                    clob_token_yes: yes_token,
                    clob_token_no: no_token,
                    condition_id: mkt.condition_id.clone(),
                    active: mkt.active,
                    closed: mkt.closed,
                    resolved,
                    resolution: (!resolution.is_empty()).then(|| resolution.to_string()),
                    end_date: parse_time(&mkt.end_date),
                    volume: mkt.volume.float64(),
                    liquidity: mkt.liquidity.float64(),
                    last_trade_price: mkt.last_trade_price.float64(),
                    best_bid: mkt.best_bid.float64(),
                    best_ask: mkt.best_ask.float64(),
                    spread: mkt.spread.float64(),
                    race_type: market_type.to_string(),
                    state: extract_state(&mkt.question),
                    raw_json: Some(gamma_market_json(mkt)),
                    ..Default::default()
                };
                if let Err(e) = db.upsert_market(&store_mkt) {
                    println!("    warning: failed to upsert market {}: {e:#}", mkt.id);
                    continue;
                }
                total_markets += 1;
                mkt_count += 1;
            }

            if cfg.verbose {
                println!("    event: {} ({sub_cat}); {mkt_count} markets", evt.title);
            }
        }

        println!("  page done; running total: {total_events} events, {total_markets} markets");

        offset += page_size;
        if a.limit > 0 && offset >= a.limit {
            break;
        }
        if (events.len() as i64) < page_size {
            break;
        }
    }

    println!("\ndone: {total_events} events, {total_markets} markets ingested");
    Ok(())
}

// ---- ingest trades ----

fn run_trades(a: &TradesArgs, ctx: &Ctx) -> Result<()> {
    let cfg = ctx.config()?;
    let db = ctx.connect(&cfg)?;
    let data_api = DataApiClient::new(&cfg.data_api_host, Arc::new(RateLimiter::new()));

    let mut markets = db.get_active_markets(&a.race_types).context("querying markets")?;
    // Also include closed/resolved markets.
    markets.extend(db.get_resolved_markets(&a.race_types).context("querying resolved markets")?);

    if markets.is_empty() {
        println!("no markets found; run 'oddsmith ingest markets' first");
        return Ok(());
    }

    println!("fetching trades for {} markets (workers={})...", markets.len(), a.workers);

    // Markets are processed one at a time: the rate limit is the bottleneck, not CPU.
    let (mut total_trades, mut skipped) = (0u64, 0u64);
    let n = markets.len();
    for (i, mkt) in markets.iter().enumerate() {
        if mkt.condition_id.is_empty() {
            skipped += 1;
            continue;
        }

        // Skip markets that already have trades (a failed count reads as 0).
        let existing = db.count_trades_for_market(&mkt.id).unwrap_or(0);
        if existing > 0 {
            if cfg.verbose {
                println!("  [{}/{n}] skipping {} (already has {existing} trades)", i + 1, mkt.id);
            }
            continue;
        }

        println!("  [{}/{n}] fetching trades for {}...", i + 1, mkt.question);

        let trades = match fetch_all_trades(&data_api, mkt) {
            Ok(t) => t,
            Err(e) => {
                println!("    warning: failed to fetch trades for {}: {e:#}", mkt.id);
                continue;
            }
        };
        if trades.is_empty() {
            println!("    no trades found");
            continue;
        }

        if a.batch_size == 0 {
            bail!("range() arg 3 must not be zero");
        }
        let mut mkt_inserted = 0;
        if a.batch_size > 0 {
            for chunk in trades.chunks(a.batch_size as usize) {
                match db.batch_insert_trades(chunk) {
                    Ok(n) => mkt_inserted += n,
                    Err(e) => println!("    warning: batch insert failed: {e:#}"),
                }
            }
        }
        total_trades += mkt_inserted;
        println!("    {} trades fetched, {mkt_inserted} new; running total: {total_trades}", trades.len());
    }

    println!("\ndone: {total_trades} trades ingested ({skipped} markets skipped, no condition ID)");
    Ok(())
}

/// Every trade the Data API returns for a market (it caps the offset at 10000).
fn fetch_all_trades(data_api: &DataApiClient, mkt: &Market) -> Result<Vec<Trade>> {
    let mut all = Vec::new();
    let page_size: i64 = 10_000;
    let mut offset: i64 = 0;
    loop {
        let results = data_api.fetch_trades(&mkt.condition_id, page_size, offset)?;
        for dt in &results {
            let mut maker = strip_ws(&dt.maker_address).to_string();
            if maker.is_empty() {
                maker = strip_ws(&dt.maker).to_string();
            }
            let traded_at = DateTime::<Utc>::from_timestamp(dt.timestamp, 0)
                .ok_or_else(|| anyhow!("timestamp {} is out of range", dt.timestamp))?;
            all.push(Trade {
                id: dt.transaction_hash.clone(),
                market_id: mkt.id.clone(),
                event_id: mkt.event_id.clone(),
                asset_id: dt.asset.clone(),
                side: dt.side.clone(),
                price: dt.price,
                size: dt.size,
                // The Data API returns taker trades.
                is_taker_trade: Some(true),
                maker_address: maker,
                taker_address: dt.proxy_wallet.clone(),
                outcome: dt.outcome.clone(),
                transaction_hash: dt.transaction_hash.clone(),
                bucket_label: price_bucket(dt.price),
                traded_at: Some(traded_at),
            });
        }
        if (results.len() as i64) < page_size {
            break;
        }
        offset += page_size;
        if offset >= 10_000 {
            break;
        }
    }
    Ok(all)
}

// ---- ingest polls ----

fn run_polls(a: &PollsArgs, ctx: &Ctx) -> Result<()> {
    let cfg = ctx.config()?;
    let db = ctx.connect(&cfg)?;

    println!("parsing polls from {} (source={})...", a.data_dir, a.source);
    let mut polls = parse_directory(Path::new(&a.data_dir), &a.source).context("parsing polls")?;
    if polls.is_empty() {
        println!("no polls found; place CSV files in {}", a.data_dir);
        return Ok(());
    }

    compute_implied_probabilities(&mut polls);

    let mut total = 0;
    for chunk in polls.chunks(500) {
        total += db.batch_insert_polls(chunk).context("inserting polls")?;
    }
    println!("done: {total} polls ingested");
    Ok(())
}

// ---- ingest match ----

fn run_match(ctx: &Ctx) -> Result<()> {
    let cfg = ctx.config()?;
    let db: Db = ctx.connect(&cfg)?;
    println!("matching polls to markets...");
    let matched = match_polls_to_markets(&db).context("matching")?;
    println!("done: {matched} poll-market matches created");
    Ok(())
}

// ---- helpers ----

const SPORTS_KEYWORDS: &[&str] = &[
    "nba",
    "nfl",
    "mlb",
    "nhl",
    "ufc",
    "mma",
    "super bowl",
    "world series",
    "stanley cup",
    "champions league",
    "premier league",
    "la liga",
    "bundesliga",
    "serie a",
    "basketball",
    "football champion",
    "baseball",
    "hockey",
    "grand prix",
    "formula 1",
    "f1",
    "tennis",
    "wimbledon",
    "us open tennis",
];

/// Whether an event title looks like a sports market.
pub fn is_sports_event(title: &str) -> bool {
    let t = lower(title);
    SPORTS_KEYWORDS.iter().any(|kw| t.contains(kw))
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    parse_rfc3339(s).map(|t| t.with_timezone(&Utc))
}

/// A Gamma number as JSON: its literal when it is an integer, else the parsed float; empty
/// is 0.
pub fn number_json(n: &Number) -> Json {
    let s = n.as_str();
    if s.is_empty() {
        return json!(0);
    }
    let digits = s.strip_prefix('-').unwrap_or(s);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(i) = s.parse::<i64>() {
            return json!(i);
        }
        if let Ok(u) = s.parse::<u64>() {
            return json!(u);
        }
    }
    serde_json::Number::from_f64(n.float64()).map(Json::Number).unwrap_or(Json::Null)
}

fn tag_json(t: &GammaTag) -> Json {
    json!({"id": t.id, "label": t.label, "slug": t.slug})
}

/// A Gamma market as JSON with the Gamma API key names.
pub fn gamma_market_json(m: &GammaMarket) -> Json {
    json!({
        "id": m.id,
        "question": m.question,
        "slug": m.slug,
        "description": m.description,
        "conditionId": m.condition_id,
        "clobTokenIds": m.clob_token_ids,
        "active": m.active,
        "closed": m.closed,
        "volume": number_json(&m.volume),
        "liquidity": number_json(&m.liquidity),
        "endDate": m.end_date,
        "outcomePrices": m.outcome_prices,
        "outcomes": m.outcomes,
        "bestBid": number_json(&m.best_bid),
        "bestAsk": number_json(&m.best_ask),
        "lastTradePrice": number_json(&m.last_trade_price),
        "spread": number_json(&m.spread),
        "event_id": m.event_id,
        "tags": m.tags.iter().map(tag_json).collect::<Vec<_>>(),
    })
}

/// A Gamma event as JSON with the Gamma API key names.
pub fn gamma_event_json(e: &GammaEvent) -> Json {
    json!({
        "id": e.id,
        "title": e.title,
        "slug": e.slug,
        "description": e.description,
        "startDate": e.start_date,
        "endDate": e.end_date,
        "active": e.active,
        "closed": e.closed,
        "volume": number_json(&e.volume),
        "liquidity": number_json(&e.liquidity),
        "tags": e.tags.iter().map(tag_json).collect::<Vec<_>>(),
        "markets": e.markets.iter().map(gamma_market_json).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: IngestCommand,
    }

    fn parse(args: &[&str]) -> IngestCommand {
        TestCli::try_parse_from(std::iter::once("ingest").chain(args.iter().copied())).unwrap().cmd
    }

    #[test]
    fn flags_and_defaults() {
        let IngestCommand::Markets(m) = parse(&["markets"]) else { panic!() };
        assert_eq!((m.domain.as_str(), m.race_types.as_str(), m.limit), ("politics", "", 0));
        let IngestCommand::Markets(m) = parse(&["markets", "--type", "senate,house", "--limit", "3"]) else { panic!() };
        assert_eq!((m.race_types.as_str(), m.limit), ("senate,house", 3));
        let IngestCommand::Trades(t) = parse(&["trades", "--race", "senate", "--batch-size", "50"]) else { panic!() };
        assert_eq!((t.race_types.as_str(), t.workers, t.batch_size), ("senate", 4, 50));
        let IngestCommand::Polls(p) = parse(&["polls"]) else { panic!() };
        assert_eq!((p.source.as_str(), p.data_dir.as_str()), ("538", "./data/polls"));
        assert!(matches!(parse(&["match"]), IngestCommand::Match(_)));
        let IngestCommand::OnchainTransfers(o) = parse(&["onchain-transfers", "--top", "5", "--clear"]) else {
            panic!()
        };
        assert_eq!((o.top_n, o.direction.as_str(), o.max_pages, o.batch_rows, o.clear), (5, "both", 100, 300, true));
        assert_eq!(o.addresses_file, "");
    }

    #[test]
    fn sports_titles() {
        assert!(is_sports_event("NBA Finals Champion"));
        assert!(is_sports_event("Will Max win the F1 title?"));
        assert!(!is_sports_event("Presidential Election Winner 2024"));
    }

    #[test]
    fn numbers_as_json() {
        let n = |s: &str| number_json(&Number(s.into()));
        assert_eq!(n(""), json!(0));
        assert_eq!(n("12"), json!(12));
        assert_eq!(n("-0"), json!(0));
        assert_eq!(n("12.50"), json!(12.5));
        assert_eq!(n("1e5"), json!(100000.0));
        assert_eq!(n("18446744073709551615"), json!(18446744073709551615u64));
    }

    #[test]
    fn market_json_uses_api_keys() {
        let m = GammaMarket { id: "1".into(), volume: Number("10.5".into()), ..Default::default() };
        let j = gamma_market_json(&m);
        assert_eq!(j["conditionId"], json!(""));
        assert_eq!(j["volume"], json!(10.5));
        assert_eq!(j["bestBid"], json!(0));
        assert_eq!(j["tags"], json!([]));
    }
}
