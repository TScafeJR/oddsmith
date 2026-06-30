CREATE TABLE IF NOT EXISTS crypto_prices (
    id          BIGSERIAL PRIMARY KEY,
    symbol      TEXT NOT NULL,
    price_date  DATE NOT NULL,
    close_price NUMERIC(20,2) NOT NULL,
    volume      NUMERIC(30,2),
    created_at  TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(symbol, price_date)
);

CREATE INDEX IF NOT EXISTS idx_crypto_prices_symbol_date ON crypto_prices(symbol, price_date);
