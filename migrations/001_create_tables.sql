-- oddsmith: Core schema
-- Run with: oddsmith migrate
-- Idempotent: safe to run multiple times

CREATE TABLE IF NOT EXISTS events (
    id            TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    slug          TEXT NOT NULL,
    description   TEXT,
    category      TEXT,
    sub_category  TEXT,
    starts_at     TIMESTAMPTZ,
    ends_at       TIMESTAMPTZ,
    active        BOOLEAN DEFAULT TRUE,
    closed        BOOLEAN DEFAULT FALSE,
    volume        NUMERIC(20,2),
    liquidity     NUMERIC(20,2),
    tags          JSONB,
    raw_json      JSONB,
    created_at    TIMESTAMPTZ DEFAULT NOW(),
    updated_at    TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS markets (
    id                TEXT PRIMARY KEY,
    event_id          TEXT REFERENCES events(id),
    question          TEXT NOT NULL,
    slug              TEXT,
    clob_token_id_yes TEXT,
    clob_token_id_no  TEXT,
    condition_id      TEXT,
    active            BOOLEAN DEFAULT TRUE,
    closed            BOOLEAN DEFAULT FALSE,
    resolved          BOOLEAN DEFAULT FALSE,
    resolution        TEXT,
    end_date          TIMESTAMPTZ,
    volume            NUMERIC(20,2),
    liquidity         NUMERIC(20,2),
    last_trade_price  NUMERIC(10,6),
    best_bid          NUMERIC(10,6),
    best_ask          NUMERIC(10,6),
    spread            NUMERIC(10,6),
    race_type         TEXT,
    state             TEXT,
    candidate         TEXT,
    party             TEXT,
    raw_json          JSONB,
    created_at        TIMESTAMPTZ DEFAULT NOW(),
    updated_at        TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS trades (
    id               TEXT PRIMARY KEY,
    market_id        TEXT NOT NULL REFERENCES markets(id),
    event_id         TEXT REFERENCES events(id),
    asset_id         TEXT NOT NULL,
    side             TEXT NOT NULL,
    price            NUMERIC(10,6) NOT NULL,
    size             NUMERIC(20,6) NOT NULL,
    is_taker_trade   BOOLEAN,
    maker_address    TEXT,
    taker_address    TEXT,
    outcome          TEXT,
    transaction_hash TEXT,
    bucket_label     TEXT,
    traded_at        TIMESTAMPTZ NOT NULL,
    created_at       TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS polls (
    id             BIGSERIAL PRIMARY KEY,
    source         TEXT NOT NULL,
    race_type      TEXT NOT NULL,
    state          TEXT,
    candidate      TEXT NOT NULL,
    party          TEXT,
    poll_pct       NUMERIC(6,2) NOT NULL,
    implied_prob   NUMERIC(10,6),
    pollster       TEXT,
    sample_size    INTEGER,
    methodology    TEXT,
    poll_date      DATE NOT NULL,
    market_id      TEXT REFERENCES markets(id),
    raw_json       JSONB,
    created_at     TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS poll_market_matches (
    id             BIGSERIAL PRIMARY KEY,
    market_id      TEXT NOT NULL REFERENCES markets(id),
    race_type      TEXT NOT NULL,
    state          TEXT,
    candidate      TEXT NOT NULL,
    match_method   TEXT,
    confidence     NUMERIC(4,2),
    created_at     TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(market_id, candidate)
);

CREATE TABLE IF NOT EXISTS orderbook_snapshots (
    id             BIGSERIAL PRIMARY KEY,
    market_id      TEXT NOT NULL REFERENCES markets(id),
    asset_id       TEXT NOT NULL,
    snapshot_at    TIMESTAMPTZ NOT NULL,
    mid_price      NUMERIC(10,6),
    spread         NUMERIC(10,6),
    bid_depth_5pct NUMERIC(20,6),
    ask_depth_5pct NUMERIC(20,6),
    best_bid       NUMERIC(10,6),
    best_ask       NUMERIC(10,6),
    bids_json      JSONB,
    asks_json      JSONB,
    created_at     TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS price_history (
    id             BIGSERIAL PRIMARY KEY,
    market_id      TEXT NOT NULL REFERENCES markets(id),
    asset_id       TEXT NOT NULL,
    interval       TEXT NOT NULL,
    timestamp      TIMESTAMPTZ NOT NULL,
    price          NUMERIC(10,6) NOT NULL,
    created_at     TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(market_id, asset_id, interval, timestamp)
);

CREATE TABLE IF NOT EXISTS backtest_runs (
    id              BIGSERIAL PRIMARY KEY,
    name            TEXT NOT NULL,
    description     TEXT,
    strategy        TEXT NOT NULL,
    params          JSONB NOT NULL,
    race_filter     TEXT,
    date_from       TIMESTAMPTZ,
    date_to         TIMESTAMPTZ,
    total_trades    INTEGER,
    total_pnl       NUMERIC(20,6),
    avg_return_pct  NUMERIC(10,6),
    sharpe_ratio    NUMERIC(10,6),
    win_rate        NUMERIC(10,6),
    max_drawdown    NUMERIC(10,6),
    results_json    JSONB,
    created_at      TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS backtest_trades (
    id              BIGSERIAL PRIMARY KEY,
    run_id          BIGINT NOT NULL REFERENCES backtest_runs(id),
    market_id       TEXT NOT NULL REFERENCES markets(id),
    entry_price     NUMERIC(10,6) NOT NULL,
    exit_price      NUMERIC(10,6),
    size            NUMERIC(20,6) NOT NULL,
    side            TEXT NOT NULL,
    role            TEXT NOT NULL,
    pnl             NUMERIC(20,6),
    return_pct      NUMERIC(10,6),
    bucket_label    TEXT,
    resolved        BOOLEAN DEFAULT FALSE,
    resolution      TEXT,
    entered_at      TIMESTAMPTZ,
    resolved_at     TIMESTAMPTZ,
    created_at      TIMESTAMPTZ DEFAULT NOW()
);
