//! CLOB API client (trades, order books, price history) and the public Data API client.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};

use super::decode::{self, DResult, Fields, Number, Value};
use super::ratelimit::RateLimiter;
use super::{HttpClient, body_text, encode_params};

/// A single trade from the CLOB API.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClobTrade {
    pub id: String,
    pub taker_order_id: String,
    pub market: String,
    pub asset: String,
    /// "BUY" or "SELL".
    pub side: String,
    pub size: Number,
    pub price: Number,
    pub status: String,
    pub match_time: String,
    pub transaction_hash: String,
    /// "Yes" or "No".
    pub outcome: String,
    pub maker_address: String,
    /// Taker address.
    pub owner: String,
    /// JSON `fee_rate_bps`.
    pub fee: Number,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClobTradesResponse {
    pub data: Vec<ClobTrade>,
    pub next_cursor: String,
}

/// A single order book price level.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OrderBookLevel {
    pub price: Number,
    pub size: Number,
}

/// Response from GET /book.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OrderBook {
    pub market: String,
    pub asset_id: String,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub hash: String,
    pub timestamp: String,
}

/// A single price history point.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PricePoint {
    /// Unix timestamp, JSON `t`.
    pub timestamp: i64,
    /// JSON `p`.
    pub price: f64,
}

/// Response shape from data-api.polymarket.com/trades.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DataApiTrade {
    pub proxy_wallet: String,
    pub maker_address: String,
    /// Alternate field name for the maker, if the API uses it.
    pub maker: String,
    pub side: String,
    pub asset: String,
    pub condition_id: String,
    pub size: f64,
    pub price: f64,
    pub timestamp: i64,
    pub title: String,
    pub slug: String,
    pub outcome: String,
    pub outcome_index: i64,
    pub pseudonym: String,
    pub transaction_hash: String,
}

pub fn dec_clob_trade(v: Option<&Value>, path: &str) -> DResult<ClobTrade> {
    let Some(f) = Fields::new(v, path, "CLOBTrade")? else { return Ok(ClobTrade::default()) };
    Ok(ClobTrade {
        id: f.str("id")?,
        taker_order_id: f.str("taker_order_id")?,
        market: f.str("market")?,
        asset: f.str("asset")?,
        side: f.str("side")?,
        size: f.number("size")?,
        price: f.number("price")?,
        status: f.str("status")?,
        match_time: f.str("match_time")?,
        transaction_hash: f.str("transaction_hash")?,
        outcome: f.str("outcome")?,
        maker_address: f.str("maker_address")?,
        owner: f.str("owner")?,
        fee: f.number("fee_rate_bps")?,
    })
}

pub fn dec_clob_trades_response(v: Option<&Value>, path: &str) -> DResult<ClobTradesResponse> {
    let Some(f) = Fields::new(v, path, "CLOBTradesResponse")? else { return Ok(ClobTradesResponse::default()) };
    Ok(ClobTradesResponse { data: f.list("data", "[]CLOBTrade", dec_clob_trade)?, next_cursor: f.str("next_cursor")? })
}

pub fn dec_order_book_level(v: Option<&Value>, path: &str) -> DResult<OrderBookLevel> {
    let Some(f) = Fields::new(v, path, "OrderBookLevel")? else { return Ok(OrderBookLevel::default()) };
    Ok(OrderBookLevel { price: f.number("price")?, size: f.number("size")? })
}

pub fn dec_order_book(v: Option<&Value>, path: &str) -> DResult<OrderBook> {
    let Some(f) = Fields::new(v, path, "OrderBook")? else { return Ok(OrderBook::default()) };
    Ok(OrderBook {
        market: f.str("market")?,
        asset_id: f.str("asset_id")?,
        bids: f.list("bids", "[]OrderBookLevel", dec_order_book_level)?,
        asks: f.list("asks", "[]OrderBookLevel", dec_order_book_level)?,
        hash: f.str("hash")?,
        timestamp: f.str("timestamp")?,
    })
}

pub fn dec_price_point(v: Option<&Value>, path: &str) -> DResult<PricePoint> {
    let Some(f) = Fields::new(v, path, "PricePoint")? else { return Ok(PricePoint::default()) };
    Ok(PricePoint { timestamp: f.int("t")?, price: f.float("p")? })
}

pub fn dec_data_api_trade(v: Option<&Value>, path: &str) -> DResult<DataApiTrade> {
    let Some(f) = Fields::new(v, path, "DataAPITrade")? else { return Ok(DataApiTrade::default()) };
    Ok(DataApiTrade {
        proxy_wallet: f.str("proxyWallet")?,
        maker_address: f.str("makerAddress")?,
        maker: f.str("maker")?,
        side: f.str("side")?,
        asset: f.str("asset")?,
        condition_id: f.str("conditionId")?,
        size: f.float("size")?,
        price: f.float("price")?,
        timestamp: f.int("timestamp")?,
        title: f.str("title")?,
        slug: f.str("slug")?,
        outcome: f.str("outcome")?,
        outcome_index: f.int("outcomeIndex")?,
        pseudonym: f.str("pseudonym")?,
        transaction_hash: f.str("transactionHash")?,
    })
}

pub fn decode_trades_response(body: &[u8]) -> DResult<ClobTradesResponse> {
    decode::decode_body(body, dec_clob_trades_response)
}

pub fn decode_order_book(body: &[u8]) -> DResult<OrderBook> {
    decode::decode_body(body, dec_order_book)
}

pub fn decode_price_history(body: &[u8]) -> DResult<Vec<PricePoint>> {
    decode::decode_body(body, |v, p| {
        let Some(f) = Fields::new(v, p, "PriceHistoryResponse")? else { return Ok(Vec::new()) };
        f.list("history", "[]PricePoint", dec_price_point)
    })
}

pub fn decode_data_api_trades(body: &[u8]) -> DResult<Vec<DataApiTrade>> {
    decode::decode_body(body, |v, p| decode::dec_list(v, p, "[]DataAPITrade", dec_data_api_trade))
}

pub struct ClobClient {
    pub host: String,
    http: HttpClient,
    rl: Arc<RateLimiter>,
}

impl ClobClient {
    pub fn new(host: &str, rl: Arc<RateLimiter>) -> ClobClient {
        ClobClient { host: host.to_string(), http: HttpClient::new(30), rl }
    }

    fn get(&self, url: &str, what: &str) -> Result<(u16, Vec<u8>)> {
        self.http.get(url).map_err(|e| anyhow!("fetching {what}: {e}"))
    }

    /// Trades for a market condition ID; `taker_only` asks for taker-initiated trades only.
    /// (The CLOB /trades endpoint needs an API key; ingest uses the Data API instead.)
    pub fn fetch_trades(&self, condition_id: &str, taker_only: bool, cursor: &str) -> Result<ClobTradesResponse> {
        self.rl.wait_trades()?;
        let mut params = vec![("market", condition_id.to_string())];
        if taker_only {
            params.push(("maker_address", String::new()));
        }
        if !cursor.is_empty() {
            params.push(("next_cursor", cursor.to_string()));
        }
        let (status, body) = self.get(&format!("{}/trades?{}", self.host, encode_params(&params)), "trades")?;
        if status != 200 {
            bail!("trades API returned {status}: {}", body_text(&body));
        }
        decode_trades_response(&body).map_err(|e| anyhow!("decoding trades: {e}"))
    }

    /// The full order book for a token.
    pub fn fetch_order_book(&self, token_id: &str) -> Result<OrderBook> {
        self.rl.wait_general()?;
        let params = [("token_id", token_id.to_string())];
        let (status, body) = self.get(&format!("{}/book?{}", self.host, encode_params(&params)), "order book")?;
        if status != 200 {
            bail!("book API returned {status}: {}", body_text(&body));
        }
        decode_order_book(&body).map_err(|e| anyhow!("decoding order book: {e}"))
    }

    /// Historical prices for a token. Interval: "1m", "1h", "6h", "1d", "1w", "max", "all".
    pub fn fetch_price_history(&self, token_id: &str, interval: &str) -> Result<Vec<PricePoint>> {
        self.rl.wait_general()?;
        let params = [("market", token_id.to_string()), ("interval", interval.to_string())];
        let url = format!("{}/prices-history?{}", self.host, encode_params(&params));
        let (status, body) = self.get(&url, "price history")?;
        if status != 200 {
            bail!("prices-history API returned {status}: {}", body_text(&body));
        }
        decode_price_history(&body).map_err(|e| anyhow!("decoding price history: {e}"))
    }
}

/// Fetches public trade data from data-api.polymarket.com.
pub struct DataApiClient {
    pub host: String,
    http: HttpClient,
    rl: Arc<RateLimiter>,
}

impl DataApiClient {
    pub fn new(host: &str, rl: Arc<RateLimiter>) -> DataApiClient {
        DataApiClient { host: host.to_string(), http: HttpClient::new(30), rl }
    }

    /// Trades for a market; `condition_id` is the 0x-prefixed hex condition ID.
    pub fn fetch_trades(&self, condition_id: &str, limit: i64, offset: i64) -> Result<Vec<DataApiTrade>> {
        self.rl.wait_general()?;
        let params =
            [("market", condition_id.to_string()), ("limit", limit.to_string()), ("offset", offset.to_string())];
        let url = format!("{}/trades?{}", self.host, encode_params(&params));
        let (status, body) = self.http.get(&url).map_err(|e| anyhow!("fetching trades: {e}"))?;
        if status != 200 {
            bail!("data API returned {status}: {}", body_text(&body));
        }
        decode_data_api_trades(&body).map_err(|e| anyhow!("decoding trades: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_data_api_trades() {
        let body = br#"[{"proxyWallet":"0xabc","side":"BUY","asset":"123","conditionId":"0xc",
            "size":12.5,"price":0.53,"timestamp":1730000000,"title":"T","slug":"s","outcome":"Yes",
            "outcomeIndex":0,"name":"n","pseudonym":"p","transactionHash":"0xtx","MAKER":"0xm"}]"#;
        let trades = decode_data_api_trades(body).unwrap();
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.proxy_wallet, "0xabc");
        assert_eq!(t.maker, "0xm");
        assert_eq!(t.size, 12.5);
        assert_eq!(t.price, 0.53);
        assert_eq!(t.timestamp, 1_730_000_000);
        assert_eq!(t.transaction_hash, "0xtx");
        assert_eq!(
            decode_data_api_trades(br#"[{"price":"0.5"}]"#).unwrap_err().0,
            r#"invalid value for field "DataAPITrade.price": expected number, got string"#
        );
        assert_eq!(
            decode_data_api_trades(br#"[{"timestamp":1.5}]"#).unwrap_err().0,
            r#"invalid value for field "DataAPITrade.timestamp": expected integer, got number 1.5"#
        );
    }

    #[test]
    fn decodes_book_and_history() {
        let book = decode_order_book(
            br#"{"market":"0xm","asset_id":"1","bids":[{"price":"0.48","size":"100"}],"asks":[],"hash":"h","timestamp":"1"}"#,
        )
        .unwrap();
        assert_eq!(book.bids[0].price.float64(), 0.48);
        assert!(book.asks.is_empty());
        let hist =
            decode_price_history(br#"{"history":[{"t":1700000000,"p":0.51},{"t":1700003600,"p":0.5}]}"#).unwrap();
        assert_eq!(
            hist,
            vec![
                PricePoint { timestamp: 1_700_000_000, price: 0.51 },
                PricePoint { timestamp: 1_700_003_600, price: 0.5 }
            ]
        );
        assert_eq!(
            decode_price_history(br#"{"history":[{"t":"x"}]}"#).unwrap_err().0,
            r#"invalid value for field "PricePoint.t": expected integer, got string"#
        );
        assert_eq!(decode_price_history(b"null").unwrap(), vec![]);
    }

    #[test]
    fn decodes_clob_trades() {
        let r = decode_trades_response(
            br#"{"data":[{"id":"1","side":"BUY","size":"5","price":0.4,"fee_rate_bps":"0"}],"next_cursor":"MA=="}"#,
        )
        .unwrap();
        assert_eq!(r.next_cursor, "MA==");
        assert_eq!(r.data[0].price.as_str(), "0.4");
        assert_eq!(r.data[0].fee.as_str(), "0");
    }
}
