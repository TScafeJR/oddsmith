//! Wallet profiling, pair detection and the interactive wallet graph.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use oddsmith_ingest::chain::blocklist::is_polymarket_protocol;
use oddsmith_store::{Db, Num, RowExt};
use postgres::Row;

use crate::crypto::http_get;
use crate::json::{Json, JsonStyle, compact_html_json, serialize};
use crate::jsonvalue::{self, Value};
use crate::textfmt::{ff, round_half_away_floor};
use crate::util::{Csv, join, make_dirs, os_err, pg, render};

const TEMPLATE: &str = include_str!("../templates/wallet_graph.html");

const BUILD_PROFILES_SQL: &str = "
		INSERT INTO wallet_profiles (address, first_seen, last_seen, total_trades, total_volume,
			as_maker_count, as_taker_count, maker_volume, taker_volume,
			unique_markets, avg_trade_size)
		SELECT
			addr,
			MIN(traded_at),
			MAX(traded_at),
			COUNT(*),
			SUM(price * size),
			SUM(CASE WHEN role = 'maker' THEN 1 ELSE 0 END),
			SUM(CASE WHEN role = 'taker' THEN 1 ELSE 0 END),
			SUM(CASE WHEN role = 'maker' THEN price * size ELSE 0 END),
			SUM(CASE WHEN role = 'taker' THEN price * size ELSE 0 END),
			COUNT(DISTINCT market_id),
			AVG(price * size)
		FROM (
			SELECT maker_address AS addr, 'maker' AS role, market_id, price, size, traded_at
			FROM trades WHERE maker_address IS NOT NULL AND maker_address != ''
			UNION ALL
			SELECT taker_address AS addr, 'taker' AS role, market_id, price, size, traded_at
			FROM trades WHERE taker_address IS NOT NULL AND taker_address != ''
		) all_activity
		GROUP BY addr
		ON CONFLICT (address) DO UPDATE SET
			first_seen = LEAST(wallet_profiles.first_seen, EXCLUDED.first_seen),
			last_seen = GREATEST(wallet_profiles.last_seen, EXCLUDED.last_seen),
			total_trades = EXCLUDED.total_trades,
			total_volume = EXCLUDED.total_volume,
			as_maker_count = EXCLUDED.as_maker_count,
			as_taker_count = EXCLUDED.as_taker_count,
			maker_volume = EXCLUDED.maker_volume,
			taker_volume = EXCLUDED.taker_volume,
			unique_markets = EXCLUDED.unique_markets,
			avg_trade_size = EXCLUDED.avg_trade_size,
			updated_at = NOW()";

const DETECT_PAIRS_SQL: &str = "
		INSERT INTO wallet_pairs (wallet_a, wallet_b, pair_trades, pair_volume,
			unique_markets, first_trade, last_trade, is_self_trade, suspicion_score)
		SELECT
			LEAST(maker_address, taker_address) AS wallet_a,
			GREATEST(maker_address, taker_address) AS wallet_b,
			COUNT(*) AS pair_trades,
			SUM(price * size) AS pair_volume,
			COUNT(DISTINCT market_id) AS unique_markets,
			MIN(traded_at),
			MAX(traded_at),
			maker_address = taker_address AS is_self_trade,
			-- Suspicion score: high trade count between same pair + low market diversity = suspicious
			LEAST(1.0,
				(COUNT(*)::NUMERIC / GREATEST(COUNT(DISTINCT market_id), 1)) *  -- trades per market ratio
				LEAST(1.0, COUNT(*)::NUMERIC / 20.0) *                          -- raw frequency factor
				CASE WHEN maker_address = taker_address THEN 1.0 ELSE 0.5 END  -- self-trade boost
			)
		FROM trades
		WHERE maker_address IS NOT NULL AND maker_address != ''
			AND taker_address IS NOT NULL AND taker_address != ''
		GROUP BY wallet_a, wallet_b, is_self_trade
		HAVING COUNT(*) >= 2  -- at least 2 trades between the pair
		ON CONFLICT (wallet_a, wallet_b) DO UPDATE SET
			pair_trades = EXCLUDED.pair_trades,
			pair_volume = EXCLUDED.pair_volume,
			unique_markets = EXCLUDED.unique_markets,
			first_trade = EXCLUDED.first_trade,
			last_trade = EXCLUDED.last_trade,
			is_self_trade = EXCLUDED.is_self_trade,
			suspicion_score = EXCLUDED.suspicion_score";

/// Aggregate trade data into wallet profiles and persist them; returns the profile count.
pub fn build_wallet_profiles(db: &Db) -> Result<i64> {
    db.exec(BUILD_PROFILES_SQL, &[]).map_err(pg).context("building wallet profiles")?;
    count(db, "SELECT COUNT(*) FROM wallet_profiles")
}

/// Find wallets that trade with each other and score suspicion; returns the pair count.
pub fn detect_wallet_pairs(db: &Db) -> Result<i64> {
    db.exec(DETECT_PAIRS_SQL, &[]).map_err(pg).context("detecting wallet pairs")?;
    count(db, "SELECT COUNT(*) FROM wallet_pairs")
}

fn count(db: &Db, sql: &str) -> Result<i64> {
    let row = db.query_one(sql, &[]).map_err(pg)?;
    Ok(row.map_or(0, |r| r.get::<_, i64>(0)))
}

/// Generate all wallet-related outputs.
pub fn run_wallet_analysis(db: &Db, output_dir: &str, verbose: bool, top_n: i64, sort_by: &str) -> Result<()> {
    make_dirs(output_dir).map_err(|e| anyhow!(os_err(&e, output_dir)))?;
    let step = |msg: &str| {
        if verbose {
            println!("  {msg}...");
        }
    };

    // Step 1: Build wallet profiles
    step("building wallet profiles");
    let n_profiles = match build_wallet_profiles(db) {
        Ok(n) => n,
        Err(e) => {
            println!("  warning: profile build failed (read-only DB?): {e:#}");
            println!("  continuing with existing profile data...");
            scalar_i64(db, "SELECT COUNT(*) FROM wallet_profiles")
        }
    };
    println!("  {n_profiles} wallet profiles built");

    // Step 2: Detect pairs and score suspicion
    step("detecting wallet pairs and scoring suspicion");
    let n_pairs = match detect_wallet_pairs(db) {
        Ok(n) => n,
        Err(e) => {
            println!("  warning: pair detection failed (read-only DB?): {e:#}");
            println!("  continuing with existing pair data...");
            scalar_i64(db, "SELECT COUNT(*) FROM wallet_pairs")
        }
    };
    println!("  {n_pairs} wallet pairs detected");

    step("generating whale_profiles.csv");
    write_whale_profiles_csv(db, &join(output_dir, "whale_profiles.csv"), top_n, sort_by);

    step("generating suspicious_pairs.csv");
    write_suspicious_pairs_csv(db, &join(output_dir, "suspicious_pairs.csv"));

    step("generating onchain_edges.csv");
    write_onchain_edges_csv(db, &join(output_dir, "onchain_edges.csv"));

    step("generating volume_concentration.csv");
    write_volume_concentration_csv(db, &join(output_dir, "volume_concentration.csv"));

    step("generating wallet_summary.json");
    write_wallet_summary_json(db, &join(output_dir, "wallet_summary.json"));

    step("generating wallet_graph.html");
    write_wallet_graph_html(db, &join(output_dir, "wallet_graph.html"));
    Ok(())
}

// --- small helpers ---

fn num(v: Option<Num>) -> f64 {
    v.map_or(0.0, |n| n.0)
}

fn int(v: Option<i32>) -> i64 {
    v.map_or(0, i64::from)
}

/// First column of the first row as a count, or 0 on error or NULL.
fn scalar_i64(db: &Db, sql: &str) -> i64 {
    match db.query_one(sql, &[]) {
        Ok(Some(row)) => row.get::<_, Option<i64>>(0).unwrap_or(0),
        _ => 0,
    }
}

/// First column of the first row as a number, or 0 on error or NULL.
fn scalar_f64(db: &Db, sql: &str) -> f64 {
    match db.query_one(sql, &[]) {
        Ok(Some(row)) => row.opt_num(0).unwrap_or(0.0),
        _ => 0.0,
    }
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// --- CSV / JSON outputs ---

const VALID_SORTS: [&str; 6] =
    ["total_volume", "avg_trade_size", "total_trades", "unique_markets", "maker_volume", "taker_volume"];

fn write_whale_profiles_csv(db: &Db, path: &str, top_n: i64, sort_by: &str) {
    // Whitelist valid sort columns to prevent SQL injection.
    let sort_by = if VALID_SORTS.contains(&sort_by) { sort_by } else { "total_volume" };
    let query = format!(
        "
		SELECT address, total_trades, total_volume, as_maker_count, as_taker_count,
			maker_volume, taker_volume, unique_markets, avg_trade_size
		FROM wallet_profiles
		ORDER BY {sort_by} DESC
		LIMIT $1"
    );
    let rows = match db.query(&query, &[&top_n]).map_err(pg) {
        Ok(rows) => rows,
        Err(e) => {
            println!("  error querying whales: {e:#}");
            return;
        }
    };

    let mut w = Csv::new();
    w.row(&[
        "address",
        "total_trades",
        "total_volume",
        "as_maker",
        "as_taker",
        "maker_volume",
        "taker_volume",
        "unique_markets",
        "avg_trade_size",
        "maker_pct",
    ]);
    for r in &rows {
        let trades = int(r.get(1));
        let maker = int(r.get(3));
        let taker = int(r.get(4));
        let markets = int(r.get(7));
        let maker_pct = if trades > 0 { maker as f64 / trades as f64 } else { 0.0 };
        w.row(&[
            r.get::<_, Option<String>>(0).unwrap_or_default(),
            trades.to_string(),
            ff(num(r.get(2))),
            maker.to_string(),
            taker.to_string(),
            ff(num(r.get(5))),
            ff(num(r.get(6))),
            markets.to_string(),
            ff(num(r.get(8))),
            ff(maker_pct),
        ]);
    }
    let _ = w.save(path);
}

fn write_suspicious_pairs_csv(db: &Db, path: &str) {
    let rows = match db
        .query(
            "
		SELECT wallet_a, wallet_b, pair_trades, pair_volume, unique_markets,
			is_self_trade, suspicion_score
		FROM wallet_pairs
		WHERE suspicion_score > 0.3
		ORDER BY suspicion_score DESC
		LIMIT 200",
            &[],
        )
        .map_err(pg)
    {
        Ok(rows) => rows,
        Err(e) => {
            println!("  error querying suspicious pairs: {e:#}");
            return;
        }
    };

    let mut w = Csv::new();
    w.row(&[
        "wallet_a",
        "wallet_b",
        "pair_trades",
        "pair_volume",
        "unique_markets",
        "is_self_trade",
        "suspicion_score",
    ]);
    for r in &rows {
        let self_trade = r.get::<_, Option<bool>>(5).unwrap_or(false);
        w.row(&[
            r.get::<_, Option<String>>(0).unwrap_or_default(),
            r.get::<_, Option<String>>(1).unwrap_or_default(),
            int(r.get(2)).to_string(),
            ff(num(r.get(3))),
            int(r.get(4)).to_string(),
            (if self_trade { "true" } else { "false" }).to_string(),
            ff(num(r.get(6))),
        ]);
    }
    let _ = w.save(path);
}

fn write_onchain_edges_csv(db: &Db, path: &str) {
    let rows = match db
        .query(
            "
		SELECT wallet_a, wallet_b, transfer_count, first_seen, last_seen, link_score
		FROM wallet_onchain_edges
		ORDER BY transfer_count DESC, link_score DESC NULLS LAST
		LIMIT 10000",
            &[],
        )
        .map_err(pg)
    {
        Ok(rows) => rows,
        Err(e) => {
            println!("  onchain_edges.csv skipped: {e:#}");
            return;
        }
    };

    let mut w = Csv::new();
    w.row(&["wallet_a", "wallet_b", "transfer_count", "first_seen", "last_seen", "link_score"]);
    let mut n = 0;
    for r in &rows {
        let (Some(a), Some(b), Some(cnt)) =
            (r.get::<_, Option<String>>(0), r.get::<_, Option<String>>(1), r.get::<_, Option<i32>>(2))
        else {
            continue;
        };
        let fs = r.get::<_, Option<DateTime<Utc>>>(3).map(rfc3339).unwrap_or_default();
        let ls = r.get::<_, Option<DateTime<Utc>>>(4).map(rfc3339).unwrap_or_default();
        let ss = r.opt_num(5).map(ff).unwrap_or_default();
        w.row(&[a, b, cnt.to_string(), fs, ls, ss]);
        n += 1;
    }
    if let Err(e) = w.save(path) {
        println!("  error creating onchain_edges.csv: {}", os_err(&e, path));
        return;
    }
    if n == 0 {
        println!("  onchain_edges.csv: no rows (run: oddsmith ingest onchain-transfers)");
    }
}

fn write_volume_concentration_csv(db: &Db, path: &str) {
    // What % of total volume comes from the top N wallets?
    let total_vol = scalar_f64(db, "SELECT COALESCE(SUM(total_volume), 0) FROM wallet_profiles");
    if total_vol == 0.0 {
        return;
    }
    let Ok(rows) = db.query(
        "
		SELECT total_volume
		FROM wallet_profiles
		ORDER BY total_volume DESC",
        &[],
    ) else {
        return;
    };

    let mut w = Csv::new();
    w.row(&["top_n_wallets", "cumulative_volume", "pct_of_total"]);
    let checkpoints = [1, 5, 10, 25, 50, 100, 250, 500];
    let mut cum_vol = 0.0;
    for (i, r) in rows.iter().enumerate() {
        let rank = i + 1;
        cum_vol += num(r.get(0));
        if checkpoints.contains(&rank) {
            w.row(&[rank.to_string(), ff(cum_vol), ff(cum_vol / total_vol)]);
        }
    }
    let _ = w.save(path);
}

fn write_wallet_summary_json(db: &Db, path: &str) {
    let total_wallets = scalar_i64(db, "SELECT COUNT(*) FROM wallet_profiles");
    let total_pairs = scalar_i64(db, "SELECT COUNT(*) FROM wallet_pairs");
    let self_trades = scalar_i64(db, "SELECT COUNT(*) FROM wallet_pairs WHERE is_self_trade = true");
    let suspicious_pairs = scalar_i64(db, "SELECT COUNT(*) FROM wallet_pairs WHERE suspicion_score > 0.5");
    let onchain_transfers = scalar_i64(db, "SELECT COUNT(*) FROM wallet_onchain_transfers");
    let onchain_edges = scalar_i64(db, "SELECT COUNT(*) FROM wallet_onchain_edges");
    let total_volume = scalar_f64(db, "SELECT COALESCE(SUM(total_volume), 0) FROM wallet_profiles");
    let top10_vol = scalar_f64(
        db,
        "
		SELECT COALESCE(SUM(total_volume), 0)
		FROM (SELECT total_volume FROM wallet_profiles ORDER BY total_volume DESC LIMIT 10) t",
    );
    let top10_pct = if total_volume > 0.0 { top10_vol / total_volume } else { 0.0 };

    // Gini coefficient of the volume distribution.
    let g = compute_gini(db);

    let summary = Json::obj([
        ("total_unique_wallets", Json::Int(total_wallets)),
        ("total_wallet_pairs", Json::Int(total_pairs)),
        ("self_trade_pairs", Json::Int(self_trades)),
        ("suspicious_pairs_gt_05", Json::Int(suspicious_pairs)),
        ("onchain_transfer_rows", Json::Int(onchain_transfers)),
        ("onchain_wallet_edges", Json::Int(onchain_edges)),
        ("total_volume", Json::Float(round_half_away_floor(total_volume * 100.0) / 100.0)),
        ("top_10_wallets_volume", Json::Float(round_half_away_floor(top10_vol * 100.0) / 100.0)),
        ("top_10_wallets_pct", Json::Float(round_half_away_floor(top10_pct * 10000.0) / 10000.0)),
        ("volume_gini_coefficient", Json::Float(round_half_away_floor(g * 10000.0) / 10000.0)),
    ]);

    // Keys are sorted. A NaN/Inf value makes serialization fail and an empty file is written.
    let opts = JsonStyle { ascii_only: true, ..JsonStyle::SORTED_2 };
    let data = serialize(&summary, opts).unwrap_or_default();
    let _ = fs::write(path, data);
}

// --- interactive graph ---

#[derive(Debug, Clone, Default)]
struct GraphEdge {
    source: String,
    target: String,
    trades: i64,
    volume: f64,
    score: f64,
    self_trade: bool,
    onchain: bool,
}

impl GraphEdge {
    fn to_json(&self) -> Json {
        Json::obj([
            ("source", Json::str(self.source.clone())),
            ("target", Json::str(self.target.clone())),
            ("trades", Json::Int(self.trades)),
            ("volume", Json::Float(self.volume)),
            ("score", Json::Float(self.score)),
            ("selfTrade", Json::Bool(self.self_trade)),
            ("onchain", Json::Bool(self.onchain)),
        ])
    }
}

#[derive(Debug, Clone, Default)]
struct Node {
    id: String,
    volume: f64,
    trades: i64,
    maker_vol: f64,
    taker_vol: f64,
    markets: i64,
    avg_trade_size: f64,
    maker_pct: f64,
}

impl Node {
    fn to_json(&self) -> Json {
        Json::obj([
            ("id", Json::str(self.id.clone())),
            ("volume", Json::Float(self.volume)),
            ("trades", Json::Int(self.trades)),
            ("makerVol", Json::Float(self.maker_vol)),
            ("takerVol", Json::Float(self.taker_vol)),
            ("markets", Json::Int(self.markets)),
            ("avgTradeSize", Json::Float(self.avg_trade_size)),
            ("makerPct", Json::Float(self.maker_pct)),
        ])
    }
}

#[derive(Debug, Clone, Default)]
struct WalletRow {
    address: String,
    total_trades: i64,
    total_volume: f64,
    maker_volume: f64,
    taker_volume: f64,
    unique_markets: i64,
    avg_trade_size: f64,
    maker_pct: f64,
    pct_volume: f64,
    pct_trades: f64,
    pct_avg_size: f64,
    pct_markets: f64,
    profit: f64,
    pseudonym: String,
}

impl WalletRow {
    fn to_json(&self) -> Json {
        Json::obj([
            ("address", Json::str(self.address.clone())),
            ("totalTrades", Json::Int(self.total_trades)),
            ("totalVolume", Json::Float(self.total_volume)),
            ("makerVolume", Json::Float(self.maker_volume)),
            ("takerVolume", Json::Float(self.taker_volume)),
            ("uniqueMarkets", Json::Int(self.unique_markets)),
            ("avgTradeSize", Json::Float(self.avg_trade_size)),
            ("makerPct", Json::Float(self.maker_pct)),
            ("pctVolume", Json::Float(self.pct_volume)),
            ("pctTrades", Json::Float(self.pct_trades)),
            ("pctAvgSize", Json::Float(self.pct_avg_size)),
            ("pctMarkets", Json::Float(self.pct_markets)),
            ("profit", Json::Float(self.profit)),
            ("pseudonym", Json::str(self.pseudonym.clone())),
        ])
    }
}

fn onchain_graph_edges(db: &Db) -> Result<Vec<GraphEdge>> {
    let rows = db
        .query(
            "
			SELECT wallet_a, wallet_b, transfer_count, COALESCE(link_score, 0)
			FROM wallet_onchain_edges
			ORDER BY transfer_count DESC, link_score DESC NULLS LAST
			LIMIT 5000",
            &[],
        )
        .map_err(pg)?;
    let mut edges = Vec::new();
    for r in &rows {
        let (Some(src), Some(tgt), Some(trades), Some(sc)) =
            (r.get::<_, Option<String>>(0), r.get::<_, Option<String>>(1), r.get::<_, Option<i32>>(2), r.opt_num(3))
        else {
            continue;
        };
        // Skip edges involving protocol/infrastructure contracts.
        if is_polymarket_protocol(&src) || is_polymarket_protocol(&tgt) {
            continue;
        }
        edges.push(GraphEdge {
            source: src,
            target: tgt,
            trades: trades as i64,
            volume: 0.0,
            score: sc,
            self_trade: false,
            onchain: true,
        });
    }
    Ok(edges)
}

fn pair_graph_edges(db: &Db) -> Result<Vec<GraphEdge>> {
    let rows = db
        .query(
            "
			SELECT wallet_a, wallet_b, pair_trades, pair_volume, suspicion_score, is_self_trade
			FROM wallet_pairs
			WHERE suspicion_score > 0.2
			ORDER BY suspicion_score DESC
			LIMIT 500",
            &[],
        )
        .map_err(pg)?;
    let mut edges = Vec::new();
    for r in &rows {
        let (Some(src), Some(tgt), Some(trades), Some(vol), Some(score), Some(self_trade)) = (
            r.get::<_, Option<String>>(0),
            r.get::<_, Option<String>>(1),
            r.get::<_, Option<i32>>(2),
            r.opt_num(3),
            r.opt_num(4),
            r.get::<_, Option<bool>>(5),
        ) else {
            continue;
        };
        edges.push(GraphEdge {
            source: src,
            target: tgt,
            trades: trades as i64,
            volume: vol,
            score,
            self_trade,
            onchain: false,
        });
    }
    Ok(edges)
}

fn node_profile(db: &Db, addr: &str) -> Node {
    let mut n = Node { id: addr.to_string(), ..Default::default() };
    let row = db
        .query_one(
            "
			SELECT COALESCE(total_volume,0), COALESCE(total_trades,0),
				COALESCE(maker_volume,0), COALESCE(taker_volume,0),
				COALESCE(unique_markets,0), COALESCE(avg_trade_size,0),
				COALESCE(as_maker_count,0), COALESCE(as_taker_count,0)
			FROM wallet_profiles WHERE LOWER(address) = LOWER($1)",
            &[&addr],
        )
        .ok()
        .flatten();
    let mut maker = 0;
    if let Some(r) = row {
        n.volume = num(r.get(0));
        n.trades = int(r.get(1));
        n.maker_vol = num(r.get(2));
        n.taker_vol = num(r.get(3));
        n.markets = int(r.get(4));
        n.avg_trade_size = num(r.get(5));
        maker = int(r.get(6));
    }
    if n.trades > 0 {
        n.maker_pct = maker as f64 / n.trades as f64;
    }
    n
}

fn leaderboard_row(r: &Row) -> WalletRow {
    let mut w = WalletRow {
        address: r.get::<_, Option<String>>(0).unwrap_or_default(),
        total_trades: int(r.get(1)),
        total_volume: num(r.get(2)),
        maker_volume: num(r.get(3)),
        taker_volume: num(r.get(4)),
        unique_markets: int(r.get(5)),
        avg_trade_size: num(r.get(6)),
        pct_volume: num(r.get(9)),
        pct_trades: num(r.get(10)),
        pct_avg_size: num(r.get(11)),
        pct_markets: num(r.get(12)),
        ..Default::default()
    };
    let maker = int(r.get(7));
    if w.total_trades > 0 {
        w.maker_pct = maker as f64 / w.total_trades as f64;
    }
    w
}

fn write_wallet_graph_html(db: &Db, path: &str) {
    let onchain_graph = scalar_i64(db, "SELECT COUNT(*) FROM wallet_onchain_edges") > 0;
    let edges = if onchain_graph {
        match onchain_graph_edges(db) {
            Ok(e) => e,
            Err(e) => {
                println!("  error querying on-chain edges for graph: {e:#}");
                return;
            }
        }
    } else {
        match pair_graph_edges(db) {
            Ok(e) => e,
            Err(e) => {
                println!("  error querying pairs for graph: {e:#}");
                return;
            }
        }
    };

    if edges.is_empty() {
        println!("  no graph edges (ingest on-chain transfers or trade pairs with suspicion > 0.2)");
        return;
    }

    // Unique node ids in first-seen order.
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for e in &edges {
        for id in [&e.source, &e.target] {
            if seen.insert(id.clone()) {
                ids.push(id.clone());
            }
        }
    }
    // Full profile for each node (case-insensitive address match).
    let nodes: Vec<Node> = ids.iter().map(|addr| node_profile(db, addr)).collect();

    // Also get top wallets that may not be in the graph (for the leaderboard).
    let w_rows = db
        .query(
            "
		SELECT address, total_trades, total_volume,
			COALESCE(maker_volume,0), COALESCE(taker_volume,0),
			unique_markets, avg_trade_size,
			COALESCE(as_maker_count,0), COALESCE(as_taker_count,0),
			PERCENT_RANK() OVER (ORDER BY total_volume) as pct_vol,
			PERCENT_RANK() OVER (ORDER BY total_trades) as pct_trades,
			PERCENT_RANK() OVER (ORDER BY avg_trade_size) as pct_avg_size,
			PERCENT_RANK() OVER (ORDER BY unique_markets) as pct_markets
		FROM wallet_profiles ORDER BY total_volume DESC LIMIT 500",
            &[],
        )
        .unwrap_or_default();
    let mut all_wallets: Option<Vec<WalletRow>> = None;
    for r in &w_rows {
        all_wallets.get_or_insert_with(Vec::new).push(leaderboard_row(r));
    }

    // Profit data from the Polymarket leaderboard API.
    if let Some(wallets) = all_wallets.as_mut().filter(|w| !w.is_empty()) {
        println!("  fetching profit data for {} wallets from Polymarket...", wallets.len());
        let addrs: Vec<String> = wallets.iter().map(|w| w.address.clone()).collect();
        let profit_map = fetch_wallet_profits(&addrs);
        for w in wallets.iter_mut() {
            if let Some(entry) = profit_map.get(&w.address) {
                w.profit = entry.amount;
                w.pseudonym = entry.name.clone();
            }
        }
        println!("  got profit data for {} wallets", profit_map.len());
    }

    // Values that cannot be serialized (NaN/Inf) become an empty string.
    let marshal = |v: Json| compact_html_json(&v).unwrap_or_default();
    let nodes_json = marshal(Json::Arr(nodes.iter().map(Node::to_json).collect()));
    let edges_json = marshal(Json::Arr(edges.iter().map(GraphEdge::to_json).collect()));
    let wallets_json =
        marshal(all_wallets.as_ref().map_or(Json::Null, |ws| Json::Arr(ws.iter().map(WalletRow::to_json).collect())));

    let (graph_title, slider, self_row_display, legend_extra) = if onchain_graph {
        ("On-chain transfer links (Polygon)", ["0", "1", "0.02", "0"], " style=\"display:none\"", "")
    } else {
        (
            "Trade-pair network (suspicion)",
            ["0.2", "1", "0.05", "0.2"],
            "",
            "<div><span style=\"background:#a855f7\"></span> Self-trade</div>",
        )
    };
    let n_nodes = nodes.len().to_string();
    let n_edges = edges.len().to_string();
    let html = render(
        TEMPLATE,
        &[
            ("GRAPH_TITLE", graph_title),
            ("N_NODES", &n_nodes),
            ("N_EDGES", &n_edges),
            ("SLIDER_MIN", slider[0]),
            ("SLIDER_MAX", slider[1]),
            ("SLIDER_STEP", slider[2]),
            ("SLIDER_VAL", slider[3]),
            ("SELF_ROW_DISPLAY", self_row_display),
            ("LEGEND_EXTRA", legend_extra),
            ("GRAPH_ONCHAIN", if onchain_graph { "true" } else { "false" }),
            ("NODES_JSON", &nodes_json),
            ("EDGES_JSON", &edges_json),
            ("WALLETS_JSON", &wallets_json),
        ],
    );
    let _ = fs::write(path, html);
    println!("  wallet_graph.html: {} nodes, {} edges", nodes.len(), edges.len());
}

#[derive(Debug, Clone, PartialEq)]
struct LbEntry {
    amount: f64,
    name: String,
}

/// Total profit and display name from the Polymarket leaderboard API, keyed by address.
/// Failed lookups are skipped.
fn fetch_wallet_profits(addresses: &[String]) -> HashMap<String, LbEntry> {
    let mut result = HashMap::new();
    for (i, addr) in addresses.iter().enumerate() {
        let url = format!("https://lb-api.polymarket.com/profit?window=all&address={addr}");
        let Ok((status, body)) = http_get(&url, Duration::from_secs(10)) else { continue };
        if status != 200 {
            continue;
        }
        let Some(entry) = parse_profit(&body) else { continue };
        result.insert(addr.clone(), entry);
        // Rate limit: ~10 req/s.
        if (i + 1) % 10 == 0 {
            thread::sleep(Duration::from_secs(1));
        }
        if (i + 1) % 50 == 0 {
            println!("    {}/{} wallets fetched...", i + 1, addresses.len());
        }
    }
    result
}

/// The first entry of a leaderboard response, or `None` when the body is not a non-empty
/// list starting with an object or its amount is not a number.
fn parse_profit(body: &[u8]) -> Option<LbEntry> {
    let body = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
    let text = std::str::from_utf8(body).ok()?;
    let Value::Arr(entries) = jsonvalue::parse(text).ok()? else { return None };
    let Some(Value::Obj(first)) = entries.first() else { return None };
    // A duplicated key keeps its last value.
    let get = |k: &str| first.iter().rev().find(|(key, _)| key == k).map(|(_, v)| v);
    let amount = match get("amount") {
        None | Some(Value::Null) => 0.0,
        Some(Value::Num(lit)) => lit.parse::<f64>().ok()?,
        Some(Value::Const(f)) => *f,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Str(s)) => jsonvalue::parse_float(s)?,
        Some(Value::Arr(_) | Value::Obj(_)) => return None,
    };
    let name = match get("name") {
        None | Some(Value::Null) => String::new(),
        Some(Value::Str(s)) => s.clone(),
        Some(v) => jsonvalue::value_text(v),
    };
    Some(LbEntry { amount, name })
}

fn compute_gini(db: &Db) -> f64 {
    let Ok(rows) = db.query(
        "
		SELECT total_volume FROM wallet_profiles ORDER BY total_volume ASC",
        &[],
    ) else {
        return 0.0;
    };
    let volumes: Vec<f64> = rows.iter().map(|r| num(r.get(0))).collect();
    gini(&volumes)
}

/// Gini coefficient of values sorted ascending: `2 * sum(i * x_i) / (n * sum(x)) - (n+1)/n`
/// with 1-based ranks. Fewer than two values give 0; a zero total gives NaN.
pub fn gini(sorted_volumes: &[f64]) -> f64 {
    let n = sorted_volumes.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let mut total = 0.0;
    for v in sorted_volumes {
        total += v;
    }
    let mut numerator = 0.0;
    for (i, v) in sorted_volumes.iter().enumerate() {
        numerator += (i + 1) as f64 * v;
    }
    let denom = n * total;
    if denom == 0.0 {
        return f64::NAN;
    }
    (2.0 * numerator) / denom - (n + 1.0) / n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gini_values() {
        assert_eq!(gini(&[5.0]), 0.0);
        assert!(gini(&[1.0, 1.0, 1.0, 1.0]).abs() < 1e-12);
        // All volume in one of four wallets: (n-1)/n.
        assert!((gini(&[0.0, 0.0, 0.0, 10.0]) - 0.75).abs() < 1e-12);
        // 1, 2, 3, 4: 2*30/(4*10) - 5/4 = 0.25.
        assert!((gini(&[1.0, 2.0, 3.0, 4.0]) - 0.25).abs() < 1e-12);
        assert!(gini(&[0.0, 0.0]).is_nan());
    }

    #[test]
    fn profit_entries() {
        let e = parse_profit(br#"[{"amount": 12.5, "name": "whale", "amount": "1_000"}]"#).unwrap();
        assert_eq!(e, LbEntry { amount: 1000.0, name: "whale".into() });
        let e = parse_profit(br#"[{"name": 7}]"#).unwrap();
        assert_eq!(e, LbEntry { amount: 0.0, name: "7".into() });
        let e = parse_profit(br#"[{"amount": true, "name": ["a"]}]"#).unwrap();
        assert_eq!(e, LbEntry { amount: 1.0, name: r#"["a"]"#.into() });
        assert!(parse_profit(b"[]").is_none());
        assert!(parse_profit(br#"[1]"#).is_none());
        assert!(parse_profit(br#"[{"amount": "abc"}]"#).is_none());
        assert!(parse_profit(br#"[{"amount": [1]}]"#).is_none());
        assert!(parse_profit(br#"{"amount": 1}"#).is_none());
        assert!(parse_profit(b"not json").is_none());
        assert!(parse_profit(br#"[{"amount": NaN}]"#).unwrap().amount.is_nan());
    }
}
