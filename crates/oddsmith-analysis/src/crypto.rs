//! Daily crypto prices from CoinGecko, cached in the `crypto_prices` table.
//! Includes the small HTTP and lenient JSON decoding subset it needs.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, NaiveDate, Utc};
use oddsmith_store::{Db, Num, RowExt};

use crate::jsonvalue::{self, Value};
use crate::textfmt::fixed;
use crate::util::pg;

/// The User-Agent sent with CoinGecko requests.
pub const USER_AGENT: &str = "oddsmith/0.1";

/// One day of price data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DailyPrice {
    /// The UTC day.
    pub date: NaiveDate,
    pub close_price: f64,
    pub volume: f64,
}

/// Send a GET and return `(status, body)`. Non-2xx responses are returned, not errors.
pub fn http_get(url: &str, timeout: Duration) -> Result<(u16, Vec<u8>)> {
    let agent = oddsmith_store::http::agent(timeout);
    let mut resp = agent.get(url).header("User-Agent", USER_AGENT).call()?;
    let status = resp.status().as_u16();
    let mut body = Vec::new();
    resp.body_mut().as_reader().read_to_end(&mut body)?;
    Ok((status, body))
}

/// The body as text, invalid UTF-8 replaced.
pub fn body_text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).into_owned()
}

/// Decode the first JSON value in a response body and ignore anything after it. `NaN` and
/// `Infinity` are rejected.
pub fn decode_body(body: &[u8]) -> Result<Value, String> {
    let text = body_text(body);
    let trimmed = text.trim_start_matches([' ', '\t', '\r', '\n']);
    if trimmed.is_empty() {
        return Err("EOF".into());
    }
    let start = text.len() - trimmed.len();
    jsonvalue::decode_prefix(&text, start, false).map(|(v, _)| v)
}

fn json_kind(v: &Value) -> String {
    match v {
        Value::Num(lit) => format!("number {lit}"),
        Value::Bool(_) => "bool".into(),
        Value::Str(_) => "string".into(),
        Value::Arr(_) => "array".into(),
        Value::Obj(_) => "object".into(),
        Value::Const(_) => "float".into(),
        Value::Null => "null".into(),
    }
}

fn describe_type(t: &str) -> String {
    if let Some(rest) = t.strip_prefix("[]") {
        return format!("list of {}", describe_type(rest));
    }
    match t {
        "float64" => "number".into(),
        "int64" => "integer".into(),
        "json.Number" => "number".into(),
        "slice" => "list".into(),
        other => other.into(),
    }
}

fn type_error(v: &Value, path: &str, expected: &str) -> String {
    let wher =
        if path.is_empty() { "invalid value".to_string() } else { format!("invalid value for field \"{path}\"") };
    format!("{wher}: expected {}, got {}", describe_type(expected), json_kind(v))
}

fn dec_float(v: Option<&Value>, path: &str) -> Result<f64, String> {
    match v {
        None | Some(Value::Null) => Ok(0.0),
        Some(Value::Num(lit)) => {
            let f: f64 = lit.parse().map_err(|_| type_error(&Value::Num(lit.clone()), path, "float64"))?;
            if f.is_infinite() { Err(type_error(&Value::Num(lit.clone()), path, "float64")) } else { Ok(f) }
        }
        Some(other) => Err(type_error(other, path, "float64")),
    }
}

fn dec_float_list(v: Option<&Value>, path: &str) -> Result<Vec<f64>, String> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Arr(items)) => items.iter().map(|x| dec_float(Some(x), path)).collect(),
        Some(other) => Err(type_error(other, path, "[]float64")),
    }
}

fn dec_float_matrix(v: Option<&Value>, path: &str) -> Result<Vec<Vec<f64>>, String> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Arr(items)) => items.iter().map(|x| dec_float_list(Some(x), path)).collect(),
        Some(other) => Err(type_error(other, path, "[][]float64")),
    }
}

/// Keys match case-insensitively, later keys win, and null is a no-op.
fn fold_keys(pairs: &[(String, Value)]) -> HashMap<String, &Value> {
    let mut out: HashMap<String, &Value> = HashMap::new();
    for (k, v) in pairs {
        let lk = k.to_lowercase();
        if matches!(v, Value::Null) && out.contains_key(&lk) {
            continue;
        }
        out.insert(lk, v);
    }
    out
}

#[derive(Debug, Default, PartialEq)]
struct MarketChartRange {
    prices: Vec<Vec<f64>>,
    total_volumes: Vec<Vec<f64>>,
}

fn dec_market_chart_range(v: &Value) -> Result<MarketChartRange, String> {
    match v {
        Value::Null => Ok(MarketChartRange::default()),
        Value::Obj(pairs) => {
            let folded = fold_keys(pairs);
            Ok(MarketChartRange {
                prices: dec_float_matrix(folded.get("prices").copied(), "MarketChartRange.prices")?,
                total_volumes: dec_float_matrix(
                    folded.get("total_volumes").copied(),
                    "MarketChartRange.total_volumes",
                )?,
            })
        }
        other => Err(type_error(other, "", "MarketChartRange")),
    }
}

/// Parse a `/market_chart/range` body into one price per UTC day (the first of each day).
pub fn parse_prices(body: &[u8]) -> Result<Vec<DailyPrice>, String> {
    let result = dec_market_chart_range(&decode_body(body)?)?;

    // CoinGecko returns [timestamp_ms, price] pairs.
    let mut seen: HashSet<NaiveDate> = HashSet::new();
    let mut prices = Vec::new();
    for (i, p) in result.prices.iter().enumerate() {
        if p.len() < 2 {
            continue;
        }
        // Truncate the milliseconds toward zero, then divide (also truncating).
        let secs = (p[0].trunc() as i64) / 1000;
        let ts = DateTime::<Utc>::from_timestamp(secs, 0).ok_or_else(|| format!("timestamp {secs} is out of range"))?;
        let date = ts.date_naive();
        if !seen.insert(date) {
            continue; // one price per day
        }
        let mut dp = DailyPrice { date, close_price: p[1], volume: 0.0 };
        if let Some(v) = result.total_volumes.get(i)
            && v.len() >= 2
        {
            dp.volume = v[1];
        }
        prices.push(dp);
    }
    Ok(prices)
}

fn unix(t: DateTime<Utc>) -> i64 {
    t.timestamp()
}

/// Fetch daily prices for a coin (e.g. "bitcoin", "ethereum") from CoinGecko's free API.
pub fn fetch_coin_gecko_prices(coin_id: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<DailyPrice>> {
    let url = format!(
        "https://api.coingecko.com/api/v3/coins/{coin_id}/market_chart/range?vs_currency=usd&from={}&to={}",
        unix(from),
        unix(to)
    );
    let (status, body) =
        http_get(&url, Duration::from_secs(30)).with_context(|| format!("fetching {coin_id} prices"))?;
    if status != 200 {
        bail!("coingecko returned {status}: {}", body_text(&body));
    }
    parse_prices(&body).map_err(|e| anyhow!("decoding {coin_id} prices: {e}"))
}

/// The value Postgres stores when a float8 parameter is assigned to a NUMERIC column: the
/// float rounded to 15 significant digits (`DBL_DIG`).
fn float8_as_numeric(v: f64) -> f64 {
    if !v.is_finite() {
        return v;
    }
    format!("{v:.14e}").parse().unwrap_or(v)
}

/// Store prices in the `crypto_prices` table; returns how many were written.
pub fn upsert_crypto_prices(db: &Db, symbol: &str, prices: &[DailyPrice]) -> Result<usize> {
    let mut inserted = 0;
    for p in prices {
        db.exec(
            "
            INSERT INTO crypto_prices (symbol, price_date, close_price, volume)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (symbol, price_date) DO UPDATE SET
                close_price = EXCLUDED.close_price,
                volume = EXCLUDED.volume",
            &[&symbol, &p.date, &Num(float8_as_numeric(p.close_price)), &Num(float8_as_numeric(p.volume))],
        )
        .map_err(pg)
        .with_context(|| format!("upserting {symbol} price for {}", p.date.format("%Y-%m-%d")))?;
        inserted += 1;
    }
    Ok(inserted)
}

/// Retrieve cached prices from the database.
pub fn get_crypto_prices(db: &Db, symbol: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<DailyPrice>> {
    let rows = db
        .query(
            "
        SELECT price_date, close_price, COALESCE(volume, 0)
        FROM crypto_prices
        WHERE symbol = $1 AND price_date >= $2 AND price_date <= $3
        ORDER BY price_date ASC",
            &[&symbol, &from.date_naive(), &to.date_naive()],
        )
        .map_err(pg)?;
    Ok(rows.iter().map(|r| DailyPrice { date: r.get(0), close_price: r.num(1), volume: r.num(2) }).collect())
}

/// Fetch and cache prices unless the cache already covers more than 80% of the range.
pub fn ensure_crypto_prices(
    db: &Db,
    symbol: &str,
    coin_gecko_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    verbose: bool,
) -> Result<Vec<DailyPrice>> {
    let prices = get_crypto_prices(db, symbol, from, to)?;

    let expected_days = ((to - from).num_milliseconds() as f64 / 1000.0 / 3600.0 / 24.0) as i64;
    let threshold = (expected_days * 8) / 10;
    if prices.len() as i64 > threshold {
        if verbose {
            let pct = if expected_days == 0 {
                "+Inf".to_string()
            } else {
                fixed(prices.len() as f64 / expected_days as f64 * 100.0, 0)
            };
            println!("  {symbol}: {} days cached ({pct}% coverage)", prices.len());
        }
        return Ok(prices);
    }

    if verbose {
        println!(
            "  fetching {symbol} prices from CoinGecko ({} to {})...",
            from.format("%Y-%m-%d"),
            to.format("%Y-%m-%d")
        );
    }
    let fetched = fetch_coin_gecko_prices(coin_gecko_id, from, to)?;
    if verbose {
        println!("  got {} daily prices, caching in DB...", fetched.len());
    }
    let n = upsert_crypto_prices(db, symbol, &fetched)?;
    if verbose {
        println!("  {n} prices cached");
    }
    Ok(fetched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_price_per_day() {
        let body = br#"{"prices": [[1577836800000, 7195.153895430571], [1577840400000.9, 7200.0],
            [1577923200000, 6985.47], [1], [1578009600000, 7344.88]],
            "Total_Volumes": [[1577836800000, 1.5e10], [0, 0], [0, 0], [0, 0]], "x": 1} trailing"#;
        let got = parse_prices(body).unwrap();
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(
            got,
            vec![
                DailyPrice { date: d("2020-01-01"), close_price: 7195.153895430571, volume: 1.5e10 },
                DailyPrice { date: d("2020-01-02"), close_price: 6985.47, volume: 0.0 },
                DailyPrice { date: d("2020-01-03"), close_price: 7344.88, volume: 0.0 },
            ]
        );
    }

    #[test]
    fn decode_errors_name_type_and_field() {
        assert_eq!(parse_prices(b"  ").unwrap_err(), "EOF");
        assert_eq!(parse_prices(b"[1]").unwrap_err(), "invalid value: expected MarketChartRange, got array");
        assert_eq!(
            parse_prices(br#"{"prices": {"a": 1}}"#).unwrap_err(),
            "invalid value for field \"MarketChartRange.prices\": expected list of list of number, got object"
        );
        assert_eq!(
            parse_prices(br#"{"prices": [["1", 2]]}"#).unwrap_err(),
            "invalid value for field \"MarketChartRange.prices\": expected number, got string"
        );
        assert_eq!(
            parse_prices(br#"{"prices": [[1e999, 2]]}"#).unwrap_err(),
            "invalid value for field \"MarketChartRange.prices\": expected number, got number 1e999"
        );
        assert_eq!(
            parse_prices(b"{\"prices\": NaN}").unwrap_err(),
            "invalid character 'N' looking for beginning of value"
        );
        assert_eq!(parse_prices(b"null").unwrap(), vec![]);
    }

    #[test]
    fn numeric_assignment_rounds_to_15_digits() {
        assert_eq!(float8_as_numeric(7195.153895430571), 7195.15389543057);
        assert_eq!(float8_as_numeric(43912345678.12345), 43912345678.1235);
        assert_eq!(float8_as_numeric(0.1), 0.1);
    }
}
