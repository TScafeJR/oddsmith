# Roadmap

This file lists where `oddsmith` is headed. Items are grouped by theme and ordered roughly by priority within each theme. Checked items are done.

The main thing I want to extend here is the backtesting. Date windows, capital limits, sweeps and rolling windows are in place, and the next steps are walk-forward testing (§2), pluggable strategy stages (§3), and baselines that show whether an edge beats doing something simple. Around that core, I want `oddsmith` to be usable from AI tools through MCP (§4), to answer questions in plain English (§5), and to ship as a desktop app that runs without any setup or a hosted database (§6).

**Themes**

1. [Foundations](#1-foundations): cleanup that the other themes depend on
2. [Backtesting over a date range](#2-backtesting-over-a-date-range)
3. [Flexible strategies and research tooling](#3-flexible-strategies-and-research-tooling)
4. [MCP support](#4-mcp-support)
5. [Natural-language access to the database (LLMs)](#5-natural-language-access-to-the-database)
6. [Desktop app](#6-desktop-app)
7. [Data coverage and more venues](#7-data-coverage-and-more-venues)
8. [Live monitoring, alerts, and paper trading](#8-live-monitoring-alerts-and-paper-trading)
9. [Extensibility and integrations](#9-extensibility-and-integrations)

---

## 1. Foundations

- [x] **One binary.** The engine, ingest, analysis and web UI are a Rust workspace that builds a single `oddsmith` binary, and the simulation sits in its own crate with no I/O, so the CLI, the web UI and anything built later all run the same code.
- [x] **Keep secrets out of the repo.** Credentials live only in `.env` or the deploy environment. `deploy.sh` reads its values from the shell.
- [ ] **Read-only database role.** Create a Postgres role with `SELECT` only on the analysis tables. The web UI, the MCP server (§4) and the LLM features (§5) should connect through it.
- [x] **CI.** Run `cargo fmt`, `clippy` and the test suite on every push, and build the release binary (GitHub Actions).
- [x] **Engine tests.** Fixture trades with known outcomes, so PnL, fees, Sharpe, and drawdown are checked by hand. The simulation core (`simulate()`) is a pure function, so tests need no database.

## 2. Backtesting over a date range

The backtest this project grew out of ran over every resolved market in the database, ordered by `market_id`, so two engine issues needed fixing before results from different windows could be compared:

- [x] **Chronological replay.** Positions are ordered by `traded_at` instead of `market_id`, so max drawdown and the equity curve reflect the order trades actually happened in.
- [x] **Enforce capital.** `--enforce-capital` tracks cash and open exposure, skips entries when cash runs out, and releases capital when a market resolves. Drawdown is then measured on realized equity.

Then the range filters themselves:

- [x] `--from` / `--to` flags, plus `start_date` / `end_date` in strategy JSON and the web API.
- [ ] Date pickers for the window in the web UI form.
- [x] `--window-by` to filter on entry time (`traded`) or resolution time (`resolved`). These answer different questions. For example, "markets that resolved in Q4 2024" isn't the same as "trades placed in Q4 2024".
- [ ] `--window-by created` for market creation time. This needs Polymarket's own creation timestamp; `markets.created_at` is the ingestion time.
- [ ] Named presets such as `--period 2024-cycle` or `--period midterms-2022` for common election windows.
- [x] **Rolling windows.** `oddsmith backtest --rolling 90 --step 30` produces a time series of return, win rate, Sharpe and edge (win rate minus the market-implied probability), which shows whether the bias is stable or shrinking.
- [ ] **Walk-forward runs.** Fit parameters with a sweep on one window and test the winner on the next, so in-sample tuning can't masquerade as edge.
- [x] Store the date window (and capital settings) on `backtest_runs` so you can compare runs in SQL.
- [ ] Mark the window on equity-curve and calibration charts.

## 3. Flexible strategies and research tooling

Today a strategy is a fixed set of parameters: side, price band, role, fees, and size. The goal is to make each part of a strategy pluggable while keeping simple strategies as one JSON file.

### 3a. A strategy interface

- [ ] Split the engine into stages: **universe → signal → entry → sizing → exit → costs**. Each stage becomes a Rust trait, and the current behavior becomes the default implementation of each.
- [ ] Have the engine iterate over a time-ordered event stream (trades, price ticks, resolutions) rather than a flat list of trades. Exits, mark-to-market capital tracking, and live monitoring (§8) all need this.

### 3b. Building blocks available from JSON

| Stage | Options to add |
|-------|----------------|
| **Universe** | `race_type`, `domain`, state, min liquidity/volume, days-to-resolution band, tag include/exclude, explicit market lists |
| **Signal** | price band (today); poll-vs-market gap; price momentum/mean reversion from CLOB history; whale-flow (follow or fade top wallets); calibration-adjusted fair value |
| **Entry** | every qualifying trade (today); first touch per market; once per market per day; only when a taker crosses (realistic maker fill) |
| **Sizing** | fixed (today); fixed fraction of equity; fractional Kelly using calibration-curve edge; volatility-scaled |
| **Exit** | hold to resolution (today); take-profit / stop-loss on price; time-based exit N days before resolution; exit on signal flip |
| **Costs** | fee schedule (today); spread and slippage from order-book snapshots; gas/bridge costs |

- [ ] **Version the strategy files** (`"version": 2`). Version 1 files keep working unchanged.
- [ ] **Composite strategies.** Combine several sub-strategies in one portfolio with a capital split between them (for example, maker on favorites plus a longshot fade).

### 3c. Research tooling

- [x] **Parameter sweeps.** `oddsmith sweep --param min_price=0.5:0.9:0.05 --param side=YES,NO` writes one row per combination and an HTML heatmap. Trades are fetched once per side and every variant is simulated in memory.
- [ ] **Run comparison.** `oddsmith backtest compare <run_id> <run_id>…` shows metrics side by side and overlays the equity curves.
- [x] **Bootstrap intervals.** `--bootstrap N` gives 95% confidence intervals on mean return and win rate; sweeps report one per variant.
- [ ] **Multiple-testing checks.** A deflated Sharpe ratio when many sweep variants are tested, and bootstrap intervals on Sharpe itself.
- [ ] **Walk-forward and holdout reports.** Pair a sweep on one window with a test on the next (§2) and flag variants whose edge disappears out of sample.
- [ ] **Benchmarks in the report.** Report each run against buy-all-favorites and random entry so the edge is measured against a baseline.
- [ ] **Web UI.** Add a strategy builder for the new building blocks, a sweep runner, and a run-history page.

## 4. MCP support

The goal is to let Claude Code, Claude Desktop, and any other MCP client use `oddsmith` directly: look at the data, run backtests and sweeps, and read the results, all through tools that can't damage the database.

- [ ] **`oddsmith mcp` server** built on the official Rust MCP SDK (`rmcp`), over stdio for local clients and streamable HTTP for remote use and the desktop app (§6).
- [ ] **Data tools.** `list_tables`, `describe_table`, and `run_query` (a single read-only `SELECT` with a row cap), shared with `oddsmith ask` (§5) so both get the same guardrails.
- [ ] **Research tools.** `backtest`, `sweep`, and `rolling` call the core crates directly and return summary metrics plus the path to the full CSV/HTML output. Long sweeps report progress as they run.
- [ ] **Resources.** Strategy files, recent rows from `backtest_runs`, and generated reports are exposed as MCP resources, so a client can read them without file-system access.
- [ ] **Prompts.** Canned workflows such as "check whether the bias held over the last 90 days" or "compare maker and taker results for this strategy".
- [ ] **Safety.** Tools never write to the database unless a backtest is explicitly asked to persist, and connection strings never leave the server.
- [ ] **Setup docs** with copy-paste configuration for Claude Code (`claude mcp add`) and Claude Desktop.

## 5. Natural-language access to the database

The goal is to let anyone ask questions like "which senate markets had the largest gap between polls and price a week before resolution?" without writing SQL. The answer should show the SQL behind it so it can be checked.

### 5a. `oddsmith ask`

- [ ] `oddsmith ask "<question>"` runs an LLM tool-use loop with three tools:
  - `list_tables` and `describe_table`, which return the schema plus column descriptions,
  - `run_query`, which executes one `SELECT` and returns at most N rows.
- [ ] Default to Claude through the Anthropic Messages API. Set the model with `--model` or `ODDSMITH_LLM_MODEL` and the key with `ANTHROPIC_API_KEY`.
- [ ] **Output.** Print the generated SQL, a results table, and a short plain-English answer. `--sql-only` prints just the query, and `--csv` writes the results to a file.
- [ ] **Follow-ups.** `oddsmith ask -i` keeps the conversation state so you can refine a question ("now only 2024", "break that out by bucket").

### 5b. Guardrails

- [ ] Connect through the read-only role from §1, with `statement_timeout` and a row cap set on every session.
- [ ] Parse the generated SQL and reject anything other than a single `SELECT` or `WITH … SELECT`.
- [ ] Never send `.env` contents or connection strings to the model. Send only result rows, and cap how many.
- [ ] Log every question, the SQL it produced, and the result size to an `llm_queries` table for auditing and evals.

### 5c. Schema context

- [ ] Add `COMMENT ON` descriptions to the tables and columns in a new migration. Cover units (price in 0–1, size in USDC), enum values (`outcome`, `role`, `race_type`), and join keys (`condition_id` vs `market_id`).
- [ ] Include a short glossary in the system prompt: favorite-longshot bias, maker/taker, calibration, resolution. The model can then map finance terms to columns.
- [ ] Add a few worked example question-to-SQL pairs for the trickier joins (polls ↔ markets, YES/NO token pricing).

### 5d. Beyond the CLI

- [ ] **Web UI chat tab** in `oddsmith serve`, built on the same tools. It shows the SQL in an editable box and can chart numeric results.
- [ ] **Run analysis from a question.** For example, "backtest fading longshots in senate races during 2022" becomes a strategy JSON (§3), which is shown for confirmation and then run.
- [ ] **Evals.** Keep a fixed set of about 30 questions with known answers, run it whenever the prompt or model changes, and track accuracy over time.

## 6. Desktop app

The goal is a download-and-run app for people who want to explore the bias without installing a toolchain or running a hosted Postgres database.

- [ ] **Shell.** Wrap the web UI in a native window with Tauri. The Rust crates link straight into the app, so there is no sidecar process or bundled interpreter to ship.
- [ ] **Local-first storage.** A SQLite or DuckDB backend behind the `store` layer, so the app works offline with no database server. Postgres stays available for shared setups.
- [ ] **Guided setup.** A first-run flow that ingests a starter dataset (one election cycle) with visible progress, then opens a ready-made backtest.
- [ ] **Workspace.** Saved strategies, run history with side-by-side comparison (§3c), sweep heatmaps and rolling-window charts inside the app, and export to CSV or parquet.
- [ ] **Built-in MCP.** The app runs the MCP server (§4), so Claude Desktop can connect to it in one step.
- [ ] **Packaging.** Signed and notarized builds for macOS and Windows, a Linux AppImage, and auto-update.

## 7. Data coverage and more venues

- [ ] **Remove the 10k-trade cap.** The Data API returns at most 10,000 trades per market. Backfill busy markets from on-chain `OrderFilled` events or the Polymarket subgraph.
- [ ] **Keep maker/taker labels.** Store the maker/taker side for every fill so maker-only backtests and wallet-pair analysis use real labels.
- [ ] **Order-book history.** Snapshot order books on a schedule so spread and slippage costs (§3b) use observed depth instead of assumptions.
- [ ] **Incremental ingest.** Fetch only new trades and newly resolved markets, and run the update on a schedule.
- [ ] **More domains.** Extend the analysis to sports, crypto, and economics markets to see whether the bias carries over beyond politics.
- [ ] **Kalshi.** Ingest Kalshi data for a direct cross-venue comparison with the source paper.
- [ ] **A venue interface.** Put ingest behind a common interface so new venues with public data can be added without touching the backtester, and so the same strategy can be run on each venue and compared.

## 8. Live monitoring, alerts, and paper trading

- [ ] **`oddsmith monitor`.** Implement the existing stub as a terminal dashboard of live markets, with each market's price, its calibration-adjusted fair value, and current spread.
- [ ] **Signal alerts.** Alert when any saved strategy's entry conditions are met on a live market, delivered to the terminal, a webhook (Slack or Discord), or a desktop notification from the app (§6).
- [ ] **Paper trading.** Run a strategy forward against live order books, record simulated maker fills, and compare the results with the backtest.
- [ ] **Drift reports.** A scheduled rolling-window report (§2) that flags when a strategy's live or recent edge falls outside its backtested confidence interval.

## 9. Extensibility and integrations

- [ ] **Strategy plugins.** Third-party implementations of the strategy stages (§3a) written as Rust trait implementations, so a custom signal doesn't need a fork.
- [ ] **Dataset export.** `oddsmith export dataset` writes markets, trades, and resolutions to parquet or DuckDB for analysis outside oddsmith.
- [ ] **Scheduled jobs.** Recipes for cron and GitHub Actions that run incremental ingest and nightly reports.
- [ ] **Releases.** Ship prebuilt `oddsmith` binaries through GitHub releases and Homebrew, so trying it doesn't require a Rust toolchain.
