# oddsmith

Prediction markets are supposed to be good at pricing probabilities, but they have a well-known blind spot: people overpay for longshots and underpay for favorites. oddsmith is a Rust toolkit for testing whether that favorite-longshot bias shows up on Polymarket and whether a strategy could actually trade it. It started as an offshoot of a behavioral finance project and has grown into a research tool built around its backtester.

It pulls Polymarket markets and trades, 538 polls, and on-chain wallet transfers into Postgres. From there it checks how well prices line up with real outcomes, compares markets against the polls, profiles the biggest wallets, and backtests strategies like buying favorites as a maker or betting against longshots. Everything ships as a single `oddsmith` binary, including a small web UI for running backtests in the browser.

## What the backtester does

The project this grew out of answered one question: summed over every resolved market at once, did a strategy make money? That's a useful start, but it can't tell a durable edge from a lucky stretch or an overfit parameter choice, so most of the work here has gone into tools for checking whether an edge is real.

- **Realistic runs.** Trades replay in the order they happened, a run can be limited to a date window (by entry date or by resolution date), and `--enforce-capital` holds the strategy to a bankroll, so it only takes positions it could have paid for and drawdown is measured on actual equity.
- **Parameter sweeps.** `oddsmith sweep` backtests a whole grid of settings at once. Trades are loaded once per side and every variant is simulated in memory, and the results come back ranked, with bootstrap confidence intervals and an HTML heatmap.
- **Stability over time.** `--rolling` slices a run into sliding windows and reports the edge in each one (the realized win rate minus the price paid), which shows whether the bias held up or only appeared in one part of the data.
- **Speed.** The simulation lives in a pure Rust crate with no I/O. On a million synthetic trades, simulating a backtest takes about 0.35 seconds and computing its metrics about 0.2 seconds, and 200 bootstrap resamples finish in under a second across 12 threads, so large sweeps and walk-forward testing stay interactive.
- **Tested core.** The simulation has hand-checked tests for PnL, fees, Sharpe, drawdown and the capital ledger, and CI runs formatting, clippy and the test suite on every push.

## Quickstart

You need a Rust toolchain (1.85 or newer) and a Postgres database. This project uses Supabase.

```bash
make install            # cargo install the oddsmith binary, with parquet export
cp .env.example .env    # add SUPABASE_DB_URL
oddsmith migrate
oddsmith ingest markets -v && oddsmith ingest trades && oddsmith ingest polls && oddsmith ingest match
oddsmith backtest --strategy strategies/maker_favorites.json --from 2024-01-01 --to 2024-11-05 --enforce-capital
oddsmith sweep --strategy strategies/maker_favorites.json --param min_price=0.50:0.90:0.05 --param role=MAKER,TAKER --taker-fee-rate 0.07
oddsmith backtest --strategy strategies/maker_favorites.json --rolling 90 --step 30 --bootstrap 1000
oddsmith serve          # web UI on http://localhost:8080
```

The full write-up, with the thesis, every command and the data sources, is in [docs/OVERVIEW.md](docs/OVERVIEW.md).

## Where I want to take it

I'm looking to keep extending the backtesting capabilities here. The next pieces are walk-forward testing, so parameters tuned on one window are judged on the next, and splitting a strategy into swappable stages (which markets, the entry signal, position sizing, exits and costs) so ideas beyond a price band are easy to try. Beyond the backtester, these are the bigger steps I have planned:

- **MCP support.** An `oddsmith mcp` server so Claude Code, Claude Desktop and other MCP clients can query the data and run backtests and sweeps directly, through read-only tools with the same guardrails as everything else.
- **A desktop app.** The web UI in a native Tauri window with the Rust crates linked in and a local SQLite or DuckDB database, so anyone can download it and start exploring without installing a toolchain or running a hosted Postgres.
- **Questions in plain English.** `oddsmith ask "which senate races had the biggest poll-vs-market gap a week out?"` returns an answer along with the SQL that produced it.
- **More markets and live use.** Kalshi and other venues behind a common interface, sports and crypto markets, live monitoring with alerts, and paper trading to compare a strategy's forward results with its backtest.

The details are in [ROADMAP.md](ROADMAP.md).

## License

MIT. See [LICENSE](LICENSE).
