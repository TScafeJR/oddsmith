-- On-chain transfer rows (Polygon via Alchemy) and aggregated wallet-to-wallet edges

CREATE TABLE IF NOT EXISTS wallet_onchain_transfers (
    id                   BIGSERIAL PRIMARY KEY,
    dedupe_id            TEXT        NOT NULL UNIQUE,
    seed_wallet          TEXT        NOT NULL,
    counterparty         TEXT        NOT NULL,
    direction            TEXT        NOT NULL CHECK (direction IN ('out', 'in')),
    tx_hash              TEXT        NOT NULL,
    block_number         BIGINT      NOT NULL,
    block_time           TIMESTAMPTZ,
    asset                TEXT        NOT NULL DEFAULT '',
    category             TEXT,
    value_raw            NUMERIC(78, 0),
    created_at           TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_onchain_transfers_seed ON wallet_onchain_transfers(seed_wallet);
CREATE INDEX IF NOT EXISTS idx_onchain_transfers_counterparty ON wallet_onchain_transfers(counterparty);
CREATE INDEX IF NOT EXISTS idx_onchain_transfers_seed_counterparty ON wallet_onchain_transfers(seed_wallet, counterparty);

CREATE TABLE IF NOT EXISTS wallet_onchain_edges (
    id              BIGSERIAL PRIMARY KEY,
    wallet_a        TEXT        NOT NULL,
    wallet_b        TEXT        NOT NULL,
    transfer_count  INTEGER     NOT NULL,
    first_seen      TIMESTAMPTZ,
    last_seen       TIMESTAMPTZ,
    link_score      NUMERIC(8, 4),
    created_at      TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE (wallet_a, wallet_b)
);

CREATE INDEX IF NOT EXISTS idx_onchain_edges_score ON wallet_onchain_edges(link_score DESC);
CREATE INDEX IF NOT EXISTS idx_onchain_edges_count ON wallet_onchain_edges(transfer_count DESC);
