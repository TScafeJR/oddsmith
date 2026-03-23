# oddsmith

Tools for studying the favorite-longshot bias on Polymarket. oddsmith pulls markets, trades and 538 polls into Postgres, measures how well market prices match real outcomes, and backtests strategies that trade against the bias, such as buying favorites as a maker.

## Usage

You need a Rust toolchain and a Postgres database (this project uses Supabase).

```bash
cp .env.example .env    # add SUPABASE_DB_URL
make release            # release build with parquet support for saved runs
./target/release/oddsmith migrate
./target/release/oddsmith ingest markets -v
./target/release/oddsmith ingest trades
./target/release/oddsmith backtest --strategy strategies/maker_favorites.json
```

Run `oddsmith --help` to see every command.
