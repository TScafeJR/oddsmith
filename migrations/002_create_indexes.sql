-- Performance indexes
-- Idempotent: IF NOT EXISTS on all indexes

CREATE INDEX IF NOT EXISTS idx_trades_market_id ON trades(market_id);
CREATE INDEX IF NOT EXISTS idx_trades_traded_at ON trades(traded_at);
CREATE INDEX IF NOT EXISTS idx_trades_bucket ON trades(bucket_label);
CREATE INDEX IF NOT EXISTS idx_trades_taker ON trades(is_taker_trade);
CREATE INDEX IF NOT EXISTS idx_markets_event ON markets(event_id);
CREATE INDEX IF NOT EXISTS idx_markets_race ON markets(race_type);
CREATE INDEX IF NOT EXISTS idx_markets_state ON markets(state);
CREATE INDEX IF NOT EXISTS idx_markets_closed ON markets(closed);
CREATE INDEX IF NOT EXISTS idx_polls_race_date ON polls(race_type, state, poll_date);
CREATE INDEX IF NOT EXISTS idx_polls_market ON polls(market_id);
CREATE INDEX IF NOT EXISTS idx_poll_matches_market ON poll_market_matches(market_id);
CREATE INDEX IF NOT EXISTS idx_orderbook_lookup ON orderbook_snapshots(market_id, snapshot_at);
CREATE INDEX IF NOT EXISTS idx_price_history_lookup ON price_history(market_id, asset_id, interval, timestamp);
CREATE INDEX IF NOT EXISTS idx_backtest_trades_run ON backtest_trades(run_id);
