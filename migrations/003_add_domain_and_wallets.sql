-- Add domain support for multi-domain extensibility (politics, sports, etc.)
-- Add wallet analysis support

-- Domain column on events and markets
ALTER TABLE events ADD COLUMN IF NOT EXISTS domain TEXT DEFAULT 'politics';
ALTER TABLE markets ADD COLUMN IF NOT EXISTS domain TEXT DEFAULT 'politics';

-- Rename-friendly aliases: document that for sports:
--   race_type  -> league/sport (e.g., 'nba', 'nfl', 'epl')
--   state      -> region/city
--   candidate  -> team/player name
--   party      -> conference/division
-- No column renames needed; just interpret per domain.

-- Generic metadata for domain-specific fields that don't fit the core columns
ALTER TABLE markets ADD COLUMN IF NOT EXISTS metadata JSONB;
ALTER TABLE events ADD COLUMN IF NOT EXISTS metadata JSONB;

-- Wallet analysis tables

CREATE TABLE IF NOT EXISTS wallet_profiles (
    address         TEXT PRIMARY KEY,
    first_seen      TIMESTAMPTZ,
    last_seen       TIMESTAMPTZ,
    total_trades    INTEGER DEFAULT 0,
    total_volume    NUMERIC(20,6) DEFAULT 0,
    as_maker_count  INTEGER DEFAULT 0,
    as_taker_count  INTEGER DEFAULT 0,
    maker_volume    NUMERIC(20,6) DEFAULT 0,
    taker_volume    NUMERIC(20,6) DEFAULT 0,
    unique_markets  INTEGER DEFAULT 0,
    avg_trade_size  NUMERIC(20,6) DEFAULT 0,
    domains         JSONB,             -- ["politics","sports"]
    updated_at      TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS wallet_pairs (
    id              BIGSERIAL PRIMARY KEY,
    wallet_a        TEXT NOT NULL,
    wallet_b        TEXT NOT NULL,
    pair_trades     INTEGER NOT NULL,   -- number of times they traded with each other
    pair_volume     NUMERIC(20,6),
    unique_markets  INTEGER,
    first_trade     TIMESTAMPTZ,
    last_trade      TIMESTAMPTZ,
    is_self_trade   BOOLEAN DEFAULT FALSE,  -- wallet_a == wallet_b or linked wallets
    suspicion_score NUMERIC(6,4),       -- 0-1 score for wash trade likelihood
    created_at      TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(wallet_a, wallet_b)
);

-- Indexes for wallet analysis
CREATE INDEX IF NOT EXISTS idx_trades_maker ON trades(maker_address);
CREATE INDEX IF NOT EXISTS idx_trades_taker ON trades(taker_address);
CREATE INDEX IF NOT EXISTS idx_wallet_profiles_volume ON wallet_profiles(total_volume DESC);
CREATE INDEX IF NOT EXISTS idx_wallet_pairs_suspicion ON wallet_pairs(suspicion_score DESC);
CREATE INDEX IF NOT EXISTS idx_wallet_pairs_volume ON wallet_pairs(pair_volume DESC);
CREATE INDEX IF NOT EXISTS idx_markets_domain ON markets(domain);
CREATE INDEX IF NOT EXISTS idx_events_domain ON events(domain);
