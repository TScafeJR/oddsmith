# oddsmith

A Rust toolkit for measuring and trading the **favorite-longshot bias** on [Polymarket](https://polymarket.com) prediction markets.

oddsmith pulls market metadata, trades, and on-chain wallet transfers from Polymarket and Polygon into Postgres. From there it checks how well prices are calibrated against outcomes, compares markets with polls, and backtests strategies that trade against the bias. Results come out as CSV, JSON and HTML for write-ups, and a small web UI lets you run backtests interactively.

It started as an offshoot of a behavioral finance project. See [ROADMAP.md](../ROADMAP.md) for planned work.

---

## The thesis

**The anomaly.** In betting and prediction markets, low-probability outcomes (longshots) tend to be overpriced and high-probability outcomes (favorites) tend to be underpriced.

**Why it exists.** The behavioral explanation comes from *Makers and Takers: The Economics of the Kalshi Prediction Market* (CEPR DP20631):

- **Probability weighting.** Traders overweight small probabilities and overpay for lottery-like contracts.
- **Belief sorting.** Takers, who cross the spread, hold more extreme beliefs than makers, who post limit orders. Takers pay up for longshots, and makers collect the difference.

**Why it isn't arbitraged away.** Liquidity is thin, payoffs are all-or-nothing and high-variance, and most retail participants don't know about the maker/taker return gap.

**Why Polymarket.** Every fill settles on Polygon, so the full trade ledger and wallet graph are public. That allows transaction-level and wallet-level analysis that a centralized venue like Kalshi doesn't expose.

**The strategy being tested.** Act as a maker on favorites (YES at 50–95¢) and stay out of taker positions on longshots. Kalshi evidence puts the maker edge at about 2.6% per contract. oddsmith also ships the mirror-image strategies (fading longshots and fading extreme favorites) so the bias can be checked from both sides.

**Risks.** Makers face adverse selection: they tend to get filled exactly when news moves the true probability. Limit orders may also not fill at all. Thin books cap how much capital the strategy can absorb, and Polymarket adds smart-contract and stablecoin risk that Kalshi doesn't have.

---

## Features

| Area | What it does |
|------|--------------|
| **Ingest** | Political (or sports) events and markets from the Gamma API; trade history from the public Data API; 538 polls; Polygon P2P transfers via Alchemy |
| **Calibration** | Calibration curve, Mincer-Zarnowitz regression, Brier decomposition / overconfidence index, maker vs. taker returns by price bucket |
| **Benchmarks** | Market vs. 538 poll error; cross-market price correlation; strategy beta against BTC/ETH |
| **Backtest** | Strategy-file or flag-driven simulation on resolved markets, replayed in time order: YES/NO side, price band, maker/taker fee schedule, custom buckets, date windows, optional capital limits; Sharpe, win rate, edge, drawdown, per-bucket breakeven |
| **Research** | Parameter sweeps with ranked results and HTML heatmaps, rolling-window stability reports, and bootstrap confidence intervals |
| **Wallets** | Whale profiles, volume concentration (Gini), suspicious counterparty pairs, interactive D3 graph of on-chain wallet links |
| **Web UI** | `oddsmith serve` streams backtests to the browser over SSE and exports trades/markets as CSV |

---

## Quickstart

**Prerequisites:** a Rust toolchain (1.85 or newer), a Postgres database (this project uses Supabase), and optionally an Alchemy Polygon key for on-chain ingest.

From a checkout of this repo:

```bash
make install                # cargo install --locked --path crates/oddsmith-cli --features parquet
cp .env.example .env        # fill in SUPABASE_DB_URL (and ALCHEMY_POLYGON_URL if needed)
```

Typical end-to-end run:

```bash
oddsmith migrate                                # create tables
oddsmith ingest markets --domain politics -v    # events + markets
oddsmith ingest trades                          # trade history (Data API, see below)
oddsmith ingest polls --data-dir ./data/polls   # 538 polls
oddsmith ingest match                           # link polls → markets
oddsmith ingest onchain-transfers --top 1000    # optional: wallet graph edges
oddsmith backtest --strategy strategies/maker_favorites.json
oddsmith analyze all                            # CSV/JSON into ./output
oddsmith analyze wallets -v                     # whales + wallet_graph.html
```

### Environment

| Variable | Required | Notes |
|----------|----------|-------|
| `SUPABASE_DB_URL` | yes | Postgres URI (Supabase session pooler, port 5432) |
| `ALCHEMY_POLYGON_URL` | for `ingest onchain-transfers` | Full URL including API key |
| `SUPABASE_URL`, `SUPABASE_ANON_KEY` | for parquet upload | Used by `oddsmith export parquet --upload` |
| `GAMMA_HOST`, `CLOB_HOST`, `DATA_API_HOST` | no | Override Polymarket API base URLs |

`--db-url` overrides `SUPABASE_DB_URL` for a single command. `-v` turns on verbose logging everywhere.

---

## Commands

```
oddsmith migrate                     Run SQL migrations in ./migrations
oddsmith ingest markets              Discover events/markets (--domain, --type, --limit)
oddsmith ingest trades               Fetch trades per market (--race, --batch-size)
oddsmith ingest polls                Import 538 CSVs (--data-dir, --source)
oddsmith ingest match                Fuzzy-match polls to markets
oddsmith ingest onchain-transfers    Polygon transfers for top wallets (--top | --addresses-file)
oddsmith backtest                    Simulate a strategy on resolved markets (--from/--to, --enforce-capital, --rolling)
oddsmith sweep                       Backtest a parameter grid and rank the variants (--param, --metric)
oddsmith analyze all                 Calibration, MZ regression, maker/taker, polls, correlation, spreads
oddsmith analyze overconfidence      Calibration error + Brier decomposition (--race)
oddsmith analyze charts              HTML calibration curve and price analysis
oddsmith analyze beta                Strategy returns vs. BTC/ETH (--risk-free-rate)
oddsmith analyze wallets             Whales, concentration, suspicious pairs, wallet graph (--top, --sort)
oddsmith export parquet              Export backtest trades to parquet (--all | <run_id>, --upload)
oddsmith export csv                  Write the markets and trades tables to markets.csv and trades.csv
oddsmith serve                       Web UI for running backtests (--port, default 8080)
oddsmith monitor                     Live market monitor (not yet implemented)
```

Run `oddsmith <command> --help` for the full flag list. [CLAUDE.md](../CLAUDE.md) has deeper notes on each command and the data sources.

### Backtesting

You can define strategies as JSON in [`strategies/`](../strategies/) or pass them as flags. Flags override values from the file.

```bash
oddsmith backtest --strategy strategies/fade_longshots.json
oddsmith backtest --side YES --min-price 0.60 --max-price 0.90 --race presidential
oddsmith backtest --strategy strategies/no_below_33.json --role TAKER --taker-fee-rate 0.07
```

| Strategy file | Side | Price band (YES prob.) | Idea |
|---------------|------|------------------------|------|
| `maker_favorites.json` | YES | 0.50–0.95 | Buy underpriced favorites |
| `fade_longshots.json` | NO | 0.00–0.10 | Bet against overpriced longshots |
| `no_below_33.json` | NO | 0.00–0.33 | Wider longshot fade |
| `fade_extreme_favorites.json` | NO | 0.95–1.00 | Test whether near-certain favorites are overpriced |

**Fees** are charged per contract as `rate × p × (1 − p)`, where `p` is the price of the token you buy. This is the Kalshi-style schedule: 0.0175 for makers and 0.07 for takers. Both rates default to 0 because most Polymarket markets charge no exchange fee. Reported PnL and returns are net of fees.

**Date windows and capital.** Trades are replayed in the order they happened. `--from` / `--to` restrict a run to a date window on entry time, or on resolution time with `--window-by resolved`. With `--enforce-capital`, each position ties up its cost until its market resolves and entries are skipped once cash runs out, so the results show what a fixed bankroll could actually have done, and drawdown is measured on realized equity.

**Current simplifications.** Every historical trade in the price band still counts as a filled position held to resolution, so there are no exits and no model of whether a maker's limit order would really have filled. Those are the next things the backtester needs, and [ROADMAP.md](../ROADMAP.md) covers them.

### Research tools

`oddsmith sweep` backtests every combination of one or more parameters. Values are a list (`side=YES,NO`) or an inclusive range (`min_price=0.50:0.90:0.05`), and trades are fetched once per side so the whole grid runs in memory. The output is a ranked table with a bootstrap confidence interval for each variant's average return, a CSV, and, when two parameters vary, a self-contained HTML heatmap.

```bash
oddsmith sweep --strategy strategies/maker_favorites.json --param min_price=0.50:0.90:0.05 --param max_price=0.90,0.95,0.99
oddsmith sweep --param side=YES,NO --param min_price=0.0:0.9:0.1 --metric edge
```

`oddsmith backtest --rolling 90 --step 30` adds a table of 90-day windows to a normal run, each with its return, win rate, Sharpe, and edge, where edge is the realized win rate minus the average price paid (the probability the market implied). If the favorite-longshot bias is real and stable, favorites should show positive edge in most windows rather than in one lucky stretch. `--bootstrap N` adds 95% confidence intervals for the average return and win rate.

---

## Data sources

| Source | Endpoint | Used for |
|--------|----------|----------|
| Polymarket Gamma | `gamma-api.polymarket.com` | Events, markets, tags, resolutions |
| Polymarket Data API | `data-api.polymarket.com/trades?market=<condition_id>` | Public trade history (capped at 10,000 trades per market). |
| Polymarket CLOB | `clob.polymarket.com` | Order books, price history |
| Alchemy (Polygon) | `alchemy_getAssetTransfers` | Wallet-to-wallet transfers, excluding protocol contracts ([`blocklist.rs`](../crates/oddsmith-ingest/src/chain/blocklist.rs)) |
| FiveThirtyEight | CSVs in `data/polls/` | Poll benchmarks |
| CoinGecko | `/coins/{id}/market_chart/range` | BTC/ETH prices for beta analysis |

---

## Project layout

The code is a cargo workspace. Each crate has one job, and the engine at the center has no I/O, so the CLI and the web UI run exactly the same simulation.

```
crates/oddsmith-core/      Strategies, simulation, capital ledger, metrics, sweeps, rolling windows, bootstrap (pure, no I/O)
crates/oddsmith-store/     Config, Postgres pool with NUMERIC <-> f64, typed queries, the shared HTTP client
crates/oddsmith-backtest/  Database-backed backtest runner, run persistence, parquet export and upload
crates/oddsmith-ingest/    Gamma, CLOB, Data API and Alchemy clients, rate limiters, 538 parsing, poll matching
crates/oddsmith-analysis/  Calibration, regressions, overconfidence, beta, correlation, wallets, charts, CoinGecko prices
crates/oddsmith-server/    Web UI (std::net, SSE backtest runner) and `export csv`
crates/oddsmith-cli/       The `oddsmith` binary (clap)
migrations/                SQL schema
strategies/                Strategy JSON files
```

## Development

```bash
make build    # cargo build
make test     # cargo test (set ODDSMITH_TEST_DB_URL to include the database tests)
make lint     # cargo fmt --check and clippy -D warnings
make run ARGS="backtest --min-price 0.6"
```

Dependencies are pinned to exact versions in the workspace `Cargo.toml`, and the transitive set is kept small: blocking `postgres` and `ureq` clients on the platform TLS stack, `clap`, `chrono` and `serde_json`, with no async runtime. Parquet export is behind the `parquet` cargo feature. GitHub Actions checks formatting, runs clippy and the tests, and builds the release binary on every push.

The `Dockerfile` builds the release binary in a Rust image and copies it into a slim Debian image that runs `oddsmith serve` on port 8080.

## License

[MIT](../LICENSE)
