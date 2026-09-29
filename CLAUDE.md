# oddsmith

Rust toolkit for analyzing the **favorite-longshot bias** on Polymarket prediction markets. An offshoot of a behavioral finance project. The CSV/JSON/HTML output formats, printed messages and number formatting are fixed and covered by tests, so route formatting through the shared helpers in the `textfmt` modules instead of ad hoc `format!` calls.

## Project Structure

Cargo workspace; `oddsmith-core` is pure (no I/O) and everything else builds on it.

```
crates/
  oddsmith-core/              # Pure engine
    strategy.rs               # Strategy, Window, strategy_from_json/load_strategy, buckets, contract_fee
    engine.rs                 # simulate() (pure core), apply_capital() (cash ledger, heapq-ordered)
    metrics.rs                # Sharpe, win rate, drawdown (PnL or equity), bucket breakdown
    research.rs               # TradeSource trait, sweep(), rolling(), bootstrap_ci(), edge(), CSV/HTML heatmap writers
    textfmt.rs                # float formatting (shortest round-trip, fixed, general), rounding, JSON/CSV/HTML quoting
  oddsmith-store/             # Infrastructure
    config.rs                 # .env + env vars (SUPABASE_DB_URL, CLOB_HOST, ...)
    ctx.rs                    # Global CLI flags (-v, --db-url): ctx.config(), ctx.connect()
    db.rs                     # Blocking Postgres pool (max 10), Num (NUMERIC <-> f64), RowExt
    http.rs                   # Shared ureq agent (native TLS, non-2xx returned as responses)
    migrations.rs             # SQL migration runner
    markets.rs trades.rs polls.rs orderbook.rs onchain.rs   # Typed table access
    backtest.rs               # get_all_backtest_trades + TradeSource for Db
  oddsmith-backtest/
    runner.rs                 # Engine (DB runner, persistence to backtest_runs), run_strategy()
    export.rs                 # Parquet export (feature `parquet`) + Supabase Storage upload
  oddsmith-ingest/
    polymarket/               # gamma.rs, clob.rs (CLOB + Data API), ratelimit.rs, decode.rs (lenient JSON)
    chain/                    # alchemy.rs (alchemy_getAssetTransfers), blocklist.rs
    polling/                  # csv.rs, ingest.rs (538 wide/long), matching.rs, transform.rs
    cli.rs, cli/onchain.rs    # `oddsmith ingest ...`
  oddsmith-analysis/          # stats, benchmark, correlation, overconfidence, beta, charts, export (run_all),
                              # wallets, crypto (CoinGecko), cli.rs (`oddsmith analyze ...`), templates/*.html
  oddsmith-server/            # Web UI: http.rs (std::net, thread per connection), lib.rs (routes, SSE),
                              # json.rs, format.rs, index.html, csv_export.rs (`oddsmith export csv`)
  oddsmith-cli/               # The `oddsmith` binary: main.rs (clap), commands/{backtest,sweep,export}.rs
migrations/                   # SQL migration files (CREATE TABLE IF NOT EXISTS)
strategies/                   # Strategy JSON files
data/polls/                   # 538 polling CSVs
```

## Database

Remote **Supabase PostgreSQL**, shared via connection string.

- Connection string goes in `.env` as `SUPABASE_DB_URL`
- Session mode pooler, port 5432; TLS follows libpq (`prefer`/`require` encrypt without verifying, `verify-full` verifies)
- `oddsmith_store::Db`: blocking pool (max 10, min 2); `Db` is `Clone` and thread-safe
- SQL uses `$1` placeholders. Bind NUMERIC parameters as `Num(x)` (a bare `f64` only binds to float8) and read NUMERIC/float columns with `row.num(i)` / `row.opt_num(i)`
- Database tests run only when `ODDSMITH_TEST_DB_URL` is set

## API Endpoints Used

| API | Base URL | Auth | Used For |
|-----|----------|------|----------|
| Gamma | `gamma-api.polymarket.com` | None | Events, markets, metadata, tags |
| CLOB | `clob.polymarket.com` | None | Order books, price history |
| Data API | `data-api.polymarket.com` | None | **Public trade data** (trades endpoint) |
| Alchemy Polygon | `polygon-mainnet.g.alchemy.com/v2/...` | API key in URL | **On-chain transfers** (`alchemy_getAssetTransfers`) |
| CoinGecko | `api.coingecko.com` | None | BTC/ETH daily prices |

**Important:** The CLOB `/trades` endpoint returns 401 (requires API key). We use the Data API instead (`data-api.polymarket.com/trades?market=CONDITION_ID`). The `market` param must be the `0x`-prefixed condition ID hash.

### Dune: exploratory on-chain links (no `oddsmith` required)

On **[Dune](https://dune.com)** (free tier), you can link Polymarket proxy wallets via Polygon ERC-20 / native transfers before committing to `oddsmith ingest onchain-transfers`:

1. Export seed addresses (e.g. `SELECT address FROM wallet_profiles ORDER BY total_volume DESC LIMIT 2000`) or use `output/whale_profiles.csv`.
2. In Dune, create a query parameter or small seed table of addresses (lowercase `0x…`).
3. Join to Polygon decoded transfers, e.g. `erc20_polygon.evt_Transfer`, on `from` / `to` matching your seed.
4. **Exclude** known Polymarket protocol contracts (same list as [`blocklist.rs`](crates/oddsmith-ingest/src/chain/blocklist.rs)) so edges reflect wallet-to-wallet flows rather than settlement contracts.
5. Aggregate `COUNT(*)` and `SUM(amount)` grouped by `(seed, counterparty)` for a transfer graph.

Dune is ideal for slides and ad-hoc hypotheses; `oddsmith` ingests the same logic into Postgres for reproducible CSVs/HTML.

## Commands

### `oddsmith migrate`
Run SQL migrations against the database.
```bash
oddsmith migrate [--dir ./migrations]
```

### `oddsmith ingest markets`
Discover and ingest political events/markets from Polymarket's Gamma API.
```bash
oddsmith ingest markets --domain politics -v
oddsmith ingest markets --domain politics --type presidential,senate --limit 200
```
- `--domain`: politics (default) or sports
- `--type`: filter by market type (presidential, senate, house, governor)
- `--limit`: max events to fetch
- Sports events are automatically filtered out when domain=politics

### `oddsmith ingest trades`
Fetch historical trades for all ingested markets via the public Data API.
```bash
oddsmith ingest trades
oddsmith ingest trades --race presidential
```
- `--race`: filter to specific race type
- `--workers`: concurrent workers (default 4, but currently sequential due to rate limits)
- `--batch-size`: trades per DB insert batch (default 500)
- Data API caps at 10,000 trades per market

### `oddsmith ingest polls`
Import 538 polling CSVs from a local directory.
```bash
oddsmith ingest polls --data-dir ./data/polls --source 538
```
- Handles 538's wide format (cand1_name/cand1_pct, cand2_name/cand2_pct) and long format
- CSVs are in `data/polls/` (raw_polls_2014_2023.csv is the main one)

### `oddsmith ingest match`
Auto-match polls to Polymarket markets by race type, state, and candidate name.
```bash
oddsmith ingest match
```
- Only matches polls from 2020+ (when Polymarket existed)
- Fuzzy matching with confidence scoring (race type required, state must match if present, candidate name substring)

### `oddsmith ingest onchain-transfers`
Fetch Polygon asset transfers for Polymarket wallets (Alchemy), filter protocol contracts, store P2P edges for wallet graph analysis.
```bash
oddsmith ingest onchain-transfers --top 500 -v
oddsmith ingest onchain-transfers --addresses-file ./seeds.txt
```
- Requires `ALCHEMY_POLYGON_URL` (full HTTPS URL including API key, e.g. `https://polygon-mainnet.g.alchemy.com/v2/YOUR_KEY`)
- Seed addresses: `--top N` from `wallet_profiles` by `total_volume`, or one address per line in `--addresses-file`
- `--direction`: `both` (default), `out`, or `in`
- `--max-pages`: max Alchemy pages per wallet per direction (default 100; each page up to 1000 transfers)
- Run `oddsmith migrate` after pulling migrations so `005_onchain_transfers.sql` applies
- Full history for ~100k wallets is impractical on free tiers; start with top 500–5000 whales

### `oddsmith backtest`
Simulate the maker-on-favorites strategy on resolved markets.
```bash
oddsmith backtest
oddsmith backtest --strategy strategies/fade_longshots.json
oddsmith backtest --min-price 0.60 --max-price 0.90 --race presidential
oddsmith backtest --initial-capital 50000 --position-size 200
oddsmith backtest --strategy strategies/maker_favorites.json --from 2024-01-01 --to 2024-11-05
oddsmith backtest --strategy strategies/fade_longshots.json --window-by resolved --enforce-capital
```
- `--strategy`: load a strategy JSON file (flags override its values)
- `--side`: YES (default) or NO
- `--min-price`: min contract price to enter (default 0.50 = favorites)
- `--max-price`: max contract price (default 0.95)
- `--race`: filter by race type
- `--initial-capital`: starting USDC (default 10000)
- `--position-size`: per-position max (default 100)
- `--buckets`: custom bucket boundaries in percent, e.g. `0,5,10,20,50,100`
- Trades are replayed in time order (`traded_at`), so drawdown follows the real sequence of entries
- `--from` / `--to`: inclusive `YYYY-MM-DD` date window (`start_date` / `end_date` in strategy JSON)
- `--window-by`: `traded` (default, entry time) or `resolved` (market resolution time)
- `--enforce-capital`: cap open exposure at `--initial-capital`; entries are skipped when cash runs out and payouts return to cash at resolution. Drawdown is then measured on realized equity, and the report adds final equity, peak exposure and skipped entries
- The window and capital settings are stored on `backtest_runs` (`date_from`, `date_to`, `window_by`, `enforce_capital`, `skipped_trades`, `final_equity`; migration `004_backtest_windows.sql`)
- `--role`: MAKER (default) or TAKER; selects which fee rate applies
- `--maker-fee-rate` / `--taker-fee-rate`: price-dependent fee coefficient, charged per contract as `rate × p × (1−p)` where p is the price of the token bought (Kalshi: 0.0175 maker / 0.07 taker; default 0). Also settable as `role` / `maker_fee_rate` / `taker_fee_rate` in strategy JSON and in the web UI
- PnL and return are net of fees; return = net PnL / (contract cost + fee)
- Outputs: total PnL, total fees, Sharpe ratio (annualized, rf=4%), win rate, max drawdown, avg hold time, per-bucket breakdown (incl. fee-adjusted breakeven win %)

- `--rolling N --step M`: metrics over N-day windows of entry time, stepping M days; writes `rolling_<name>.csv`
- `--bootstrap N`: 95% confidence intervals for avg return and win rate from N resamples
- Every run also prints **edge**: realized win rate minus the average entry price (the market-implied win probability)

### `oddsmith sweep`
Backtest every combination of one or more parameters and rank the variants.
```bash
oddsmith sweep --strategy strategies/maker_favorites.json --param min_price=0.50:0.90:0.05 --param max_price=0.90,0.95,0.99
oddsmith sweep --param side=YES,NO --param min_price=0.0:0.9:0.1 --metric edge
```
- `--param name=values`: comma list (`a,b,c`) or inclusive range (`start:stop:step`); repeat for a grid
- Sweepable: `min_price`, `max_price`, `side`, `role`, `position_size`, `maker_fee_rate`, `taker_fee_rate`
- Accepts the same `--race`, `--from`, `--to`, `--window-by`, `--enforce-capital`, and fee-rate overrides as `backtest`
- `--metric`: rank/plot by `avg_return` (default), `sharpe`, `edge`, `total_pnl`, or `win_rate`
- Trades are fetched once per side with the widest band in the grid; variants are simulated in memory
- Writes `sweep_<name>.csv` (with bootstrap CI per variant, `--bootstrap`, default 200) and, when exactly two parameters vary, `sweep_<name>.html` (self-contained heatmap)

### `oddsmith analyze all`
Generate analysis CSVs/JSON for reporting.
```bash
oddsmith analyze all [--output-dir ./output]
```
Outputs:
- `calibration_curve.csv`: price buckets vs realized resolution frequency (bias proof)
- `maker_taker_returns.csv`: returns by role and price bucket
- `polls_vs_market.csv`: poll error vs market error comparison
- `backtest_summary.json`: strategy performance summary
- `cumulative_pnl.csv`: time-series PnL
- `correlation_matrix.csv`: cross-market price correlations
- `mz_regression.csv`: Mincer-Zarnowitz regression data
- `spread_liquidity.csv`: bid-ask spread analysis

### `oddsmith analyze overconfidence` / `charts` / `beta`
- `overconfidence`: calibration error per bucket and Brier score decomposition (`--race`)
- `charts`: self-contained HTML calibration curve and price analysis
- `beta`: strategy daily returns regressed on BTC/ETH (`--risk-free-rate`, default 0.04)

### `oddsmith analyze wallets`
Wallet-level analysis: whales, self-trading, wash trade detection.
```bash
oddsmith analyze wallets -v
oddsmith analyze wallets --top 1000 --sort avg_trade_size
```
- `--top`: number of wallets in whale profiles (default 500)
- `--sort`: sort by total_volume, avg_trade_size, total_trades, unique_markets, maker_volume, taker_volume

Outputs:
- `whale_profiles.csv`: top N wallets by chosen metric
- `suspicious_pairs.csv`: Polymarket trade-counterparty pairs (maker/taker) with suspicion score > 0.3, when present
- `onchain_edges.csv`: aggregated Polygon transfer links between wallets (after `ingest onchain-transfers`)
- `volume_concentration.csv`: cumulative volume by top N wallets, Gini coefficient
- `wallet_summary.json`: aggregate stats
- `wallet_graph.html`: interactive D3.js visualization with two tabs:
  - **Network Graph**: uses **on-chain transfer edges** when `wallet_onchain_edges` is non-empty; otherwise trade-based pairs (if any)
  - **Whale Leaderboard**: sortable table of top wallets (click any column header to sort)

### `oddsmith export parquet`
Export `backtest_trades` rows for a run (or `--all`) to parquet; `--upload` sends them to Supabase Storage. Needs a binary built with the `parquet` feature (`make release` or `make install`).

### `oddsmith export csv`
Write the `markets` and `trades` tables to `markets.csv` and `trades.csv` in the current directory (30 minute time budget).

### `oddsmith serve`
Web UI for configuring and running backtests (`--port`, default 8080). Results stream to the browser over SSE.

### `oddsmith monitor` (not implemented)
Stub for real-time market monitoring.

## Build & Run

```bash
make build          # cargo build
make release        # release binary with parquet export
make install        # cargo install the `oddsmith` binary
make run ARGS="..." # cargo run -p oddsmith-cli -- ...
make test           # cargo test (ODDSMITH_TEST_DB_URL enables DB tests)
make lint           # cargo fmt --check + clippy -D warnings
```

Dependencies are pinned with `=x.y.z` in the root `Cargo.toml`; keep the transitive set small (no async runtime).

## Environment Variables

```bash
SUPABASE_DB_URL=postgresql://postgres.xxx:password@aws-0-us-east-1.pooler.supabase.com:5432/postgres
CLOB_HOST=https://clob.polymarket.com       # optional
GAMMA_HOST=https://gamma-api.polymarket.com  # optional
DATA_API_HOST=https://data-api.polymarket.com # optional
ALCHEMY_POLYGON_URL=https://polygon-mainnet.g.alchemy.com/v2/YOUR_ALCHEMY_KEY  # on-chain ingest
SUPABASE_URL=...  SUPABASE_ANON_KEY=...       # parquet upload only
```

## Typical Workflow

```bash
oddsmith migrate                              # 1. create tables
oddsmith ingest markets --domain politics -v  # 2. pull markets
oddsmith ingest trades                        # 3. pull trade data
oddsmith ingest onchain-transfers --top 1000  # 3b. optional: Polygon P2P transfers for wallet graph
oddsmith ingest polls                         # 4. import 538 polls
oddsmith ingest match                         # 5. match polls → markets
oddsmith backtest                             # 6. run strategy simulation
oddsmith analyze all                          # 7. generate analysis CSVs
oddsmith analyze wallets -v                   # 8. wallet analysis + graph
```

## Key Concepts

- **Favorite-longshot bias**: longshots are overpriced, favorites are underpriced in prediction markets
- **Maker vs Taker**: makers provide liquidity and earn ~2.6% edge per contract; takers pay the spread
- **Strategy**: enter as maker on YES side for favorites (price 0.50-0.95), collect edge on resolution
- **Wallet analysis**: whale profiles from trades; **network graph** from Polygon P2P transfers (excluding protocol contracts) when ingested; legacy trade-pair suspicion when maker/taker addresses exist in `trades`
