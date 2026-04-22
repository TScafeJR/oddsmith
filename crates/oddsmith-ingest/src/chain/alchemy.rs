//! Alchemy client for `alchemy_getAssetTransfers` on Polygon.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, FixedOffset, NaiveDate};
use serde_json::json;

use super::blocklist::normalize_address;
use crate::polymarket::decode::{self, DResult, DecodeError, Fields, Value};
use crate::polymarket::ratelimit::RateLimiter;
use crate::polymarket::{HttpClient, body_text};
use crate::text::{hex_to_decimal, json_quote};

/// Which side of the transfer graph to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Out,
    In,
}

impl TransferDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            TransferDirection::Out => "out",
            TransferDirection::In => "in",
        }
    }
}

impl fmt::Display for TransferDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssetTransferRawContract {
    pub value: String,
    pub address: String,
    pub decimal: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssetTransferMetadata {
    pub block_timestamp: String,
}

/// One row from `alchemy_getAssetTransfers`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssetTransfer {
    pub block_num: String,
    pub hash: String,
    /// JSON `from`.
    pub from: String,
    pub to: String,
    /// A number or null.
    pub value: Option<serde_json::Value>,
    pub asset: String,
    pub category: String,
    pub unique_id: String,
    pub raw_contract: AssetTransferRawContract,
    pub metadata: AssetTransferMetadata,
}

fn dec_raw_contract(v: Option<&Value>, path: &str) -> DResult<AssetTransferRawContract> {
    let Some(f) = Fields::new(v, path, "AssetTransferRawContract")? else {
        return Ok(AssetTransferRawContract::default());
    };
    Ok(AssetTransferRawContract { value: f.str("value")?, address: f.str("address")?, decimal: f.str("decimal")? })
}

fn dec_metadata(v: Option<&Value>, path: &str) -> DResult<AssetTransferMetadata> {
    let Some(f) = Fields::new(v, path, "AssetTransferMetadata")? else {
        return Ok(AssetTransferMetadata::default());
    };
    Ok(AssetTransferMetadata { block_timestamp: f.str("blockTimestamp")? })
}

pub fn dec_asset_transfer(v: Option<&Value>, path: &str) -> DResult<AssetTransfer> {
    let Some(f) = Fields::new(v, path, "AssetTransfer")? else { return Ok(AssetTransfer::default()) };
    Ok(AssetTransfer {
        block_num: f.str("blockNum")?,
        hash: f.str("hash")?,
        from: f.str("from")?,
        to: f.str("to")?,
        value: f.get("value", decode::dec_any)?,
        asset: f.str("asset")?,
        category: f.str("category")?,
        unique_id: f.str("uniqueId")?,
        raw_contract: f.get("rawContract", dec_raw_contract)?,
        metadata: f.get("metadata", dec_metadata)?,
    })
}

/// Decode an `alchemy_getAssetTransfers` HTTP 200 body into `(transfers, page_key)`.
pub fn decode_transfers_response(raw: &[u8]) -> Result<(Vec<AssetTransfer>, String)> {
    let text = String::from_utf8_lossy(raw);
    let envelope = decode::parse_exact(&text).map_err(|e| anyhow!("decode rpc envelope: {e}"))?;
    let (result, rpc_err) = match &envelope {
        Value::Null => (None, None),
        Value::Object(entries) => {
            let folded = decode::fold_keys(entries);
            let get = |k: &str| folded.iter().find(|(key, _)| key == k).map(|(_, v)| *v);
            let err = match get("error") {
                None | Some(Value::Null) => None,
                Some(e) => Some(dec_rpc_error(e).map_err(|e| anyhow!("decode rpc envelope: {e}"))?),
            };
            (get("result"), err)
        }
        other => bail!("decode rpc envelope: {}", decode::type_error(other, "", "JSON-RPC response")),
    };
    if let Some((code, message)) = rpc_err {
        bail!("alchemy error {code}: {message}");
    }
    // A missing "result" is treated as truncated input.
    let Some(result) = result else { bail!("decode transfers: unexpected end of JSON input") };
    let decoded = (|| -> DResult<(Vec<AssetTransfer>, String)> {
        let Some(f) = Fields::new(Some(result), "", "GetTransfersResult")? else {
            return Ok((Vec::new(), String::new()));
        };
        Ok((f.list("transfers", "[]AssetTransfer", dec_asset_transfer)?, f.str("pageKey")?))
    })();
    decoded.map_err(|e| anyhow!("decode transfers: {e}"))
}

fn dec_rpc_error(v: &Value) -> DResult<(i64, String)> {
    let Some(f) = Fields::new(Some(v), "rpcResp.error", "RPCError")? else { return Ok((0, String::new())) };
    Ok((f.int("code")?, f.str("message")?))
}

/// The JSON-RPC request body: compact (no spaces after `,` or `:`), sorted keys, non-ASCII
/// escaped.
pub fn transfers_request_body(wallet: &str, dir: TransferDirection, page_key: &str, max_count: i64) -> String {
    let mut params: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
    params.insert("fromBlock", json!("0x0"));
    params.insert("toBlock", json!("latest"));
    params.insert("category", json!(["external", "erc20", "internal"]));
    params.insert("excludeZeroValue", json!(false));
    params.insert("withMetadata", json!(true));
    params.insert("order", json!("desc"));
    params.insert("maxCount", json!(format!("0x{max_count:x}")));
    match dir {
        TransferDirection::Out => params.insert("fromAddress", json!(wallet)),
        TransferDirection::In => params.insert("toAddress", json!(wallet)),
    };
    if !page_key.is_empty() {
        params.insert("pageKey", json!(page_key));
    }
    let mut body: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
    body.insert("id", json!(1));
    body.insert("jsonrpc", json!("2.0"));
    body.insert("method", json!("alchemy_getAssetTransfers"));
    body.insert("params", json!([params]));
    ascii_json(&serde_json::to_string(&body).unwrap_or_default())
}

/// Escape non-ASCII characters as `\uXXXX` (UTF-16 surrogate pairs above the BMP).
fn ascii_json(s: &str) -> String {
    if s.is_ascii() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let q = json_quote(&c.to_string());
            out.push_str(&q[1..q.len() - 1]);
        }
    }
    out
}

/// Calls `alchemy_getAssetTransfers` on Polygon.
pub struct AlchemyClient {
    pub rpc_url: String,
    http: HttpClient,
    rl: Arc<RateLimiter>,
}

impl AlchemyClient {
    pub fn new(rpc_url: &str, rl: Arc<RateLimiter>) -> AlchemyClient {
        AlchemyClient { rpc_url: rpc_url.trim().to_string(), http: HttpClient::new(60), rl }
    }

    /// One page of transfers for an address: `fromAddress` for [`TransferDirection::Out`],
    /// `toAddress` for [`TransferDirection::In`]. Returns `(transfers, next page key)`.
    pub fn fetch_asset_transfers(
        &self,
        wallet: &str,
        dir: TransferDirection,
        page_key: &str,
        max_count: i64,
    ) -> Result<(Vec<AssetTransfer>, String)> {
        if self.rpc_url.is_empty() {
            bail!("alchemy RPC URL is empty");
        }
        self.rl.wait_alchemy()?;
        let wallet = normalize_address(wallet);
        if wallet.is_empty() {
            bail!("invalid wallet address");
        }
        let max_count = if max_count <= 0 || max_count > 1000 { 1000 } else { max_count };
        let body = transfers_request_body(&wallet, dir, page_key, max_count);
        let (status, raw) =
            self.http.post_json(&self.rpc_url, body.as_bytes()).map_err(|e| anyhow!("alchemy request: {e}"))?;
        if status != 200 {
            bail!("alchemy HTTP {status}: {}", body_text(&raw));
        }
        decode_transfers_response(&raw)
    }
}

/// Follow `pageKey` for up to `max_pages` pages: `fetch` gets the current page key ("" for the
/// first page) and returns `(transfers, next key)`; `on_page` sees each page's transfers.
/// Stops early on an empty page or an empty next key.
pub fn paginate(
    max_pages: i64,
    mut fetch: impl FnMut(&str) -> Result<(Vec<AssetTransfer>, String)>,
    mut on_page: impl FnMut(&[AssetTransfer]) -> Result<()>,
) -> Result<()> {
    let mut page_key = String::new();
    for _ in 0..max_pages.max(0) {
        let (transfers, next_key) = fetch(&page_key)?;
        on_page(&transfers)?;
        if next_key.is_empty() || transfers.is_empty() {
            break;
        }
        page_key = next_key;
    }
    Ok(())
}

/// A stable id for `ON CONFLICT` in the database.
pub fn dedupe_id(t: &AssetTransfer) -> String {
    if !t.unique_id.is_empty() {
        return t.unique_id.clone();
    }
    let v = if t.raw_contract.value.is_empty() { "0" } else { t.raw_contract.value.as_str() };
    format!(
        "{}:{}:{}:{}:{}",
        t.hash,
        t.from.to_lowercase(),
        t.to.to_lowercase(),
        t.raw_contract.address.to_lowercase(),
        v
    )
}

/// `[+-]?[0-9a-fA-F]+`, the whole string.
fn is_hex_int(s: &str) -> bool {
    let d = s.strip_prefix(['+', '-']).unwrap_or(s);
    !d.is_empty() && d.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse an Alchemy hex block number.
pub fn parse_block_num(hex: &str) -> Result<i64, DecodeError> {
    let hex = hex.trim();
    if hex.is_empty() {
        return Err(DecodeError("empty block".into()));
    }
    let digits = hex.strip_prefix("0x").unwrap_or(hex);
    if !is_hex_int(digits) {
        return Err(DecodeError(format!("invalid block number \"{digits}\": not a hex integer")));
    }
    let (neg, body) = match digits.as_bytes()[0] {
        b'-' => (true, &digits[1..]),
        b'+' => (false, &digits[1..]),
        _ => (false, digits),
    };
    let out_of_range = || DecodeError(format!("invalid block number \"{digits}\": value out of range"));
    let body = body.trim_start_matches('0');
    if body.len() > 16 {
        return Err(out_of_range());
    }
    let mag = if body.is_empty() { 0 } else { i128::from_str_radix(body, 16).map_err(|_| out_of_range())? };
    let n = if neg { -mag } else { mag };
    i64::try_from(n).map_err(|_| out_of_range())
}

/// Parse an RFC 3339 metadata timestamp, keeping its offset; digits past microseconds are
/// dropped.
pub fn parse_block_time(s: &str) -> Result<DateTime<FixedOffset>, DecodeError> {
    if s.is_empty() {
        return Err(DecodeError("empty timestamp".into()));
    }
    let bad = || DecodeError(format!("parsing time \"{s}\": not an RFC 3339 timestamp"));
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<u32> {
        let part = b.get(from..from + len)?;
        part.iter().all(u8::is_ascii_digit).then(|| part.iter().fold(0u32, |a, d| a * 10 + (d - b'0') as u32))
    };
    let lit = |i: usize, c: u8| b.get(i) == Some(&c);
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) =
        (num(0, 4), num(5, 2), num(8, 2), num(11, 2), num(14, 2), num(17, 2))
    else {
        return Err(bad());
    };
    if !(lit(4, b'-') && lit(7, b'-') && lit(10, b'T') && lit(13, b':') && lit(16, b':')) {
        return Err(bad());
    }
    let mut i = 19;
    let mut micro = 0u32;
    if matches!(b.get(i), Some(b'.' | b',')) {
        let start = i + 1;
        let mut j = start;
        while b.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        if j == start {
            return Err(bad());
        }
        let frac = format!("{}000000", &s[start..j]);
        micro = frac[..6].parse().unwrap_or(0);
        i = j;
    }
    let offset_secs: i32 = match &s[i..] {
        "Z" => 0,
        z if z.len() == 6 && matches!(z.as_bytes()[0], b'+' | b'-') && z.as_bytes()[3] == b':' => {
            let (Some(hh), Some(mm)) = (num(i + 1, 2), num(i + 4, 2)) else { return Err(bad()) };
            if hh >= 24 || mm >= 60 {
                return Err(DecodeError(format!("parsing time \"{s}\": time zone offset out of range")));
            }
            let sign = if z.starts_with('-') { -1 } else { 1 };
            sign * (hh as i32 * 3600 + mm as i32 * 60)
        }
        _ => return Err(bad()),
    };
    let invalid = |what: &str| DecodeError(format!("parsing time \"{s}\": {what}"));
    if year == 0 {
        return Err(invalid(&format!("year {year} is out of range")));
    }
    if !(1..=12).contains(&month) {
        return Err(invalid("month must be in 1..12"));
    }
    let date =
        NaiveDate::from_ymd_opt(year as i32, month, day).ok_or_else(|| invalid("day is out of range for month"))?;
    if hour > 23 {
        return Err(invalid("hour must be in 0..23"));
    }
    if minute > 59 {
        return Err(invalid("minute must be in 0..59"));
    }
    if second > 59 {
        return Err(invalid("second must be in 0..59"));
    }
    let naive = date.and_hms_micro_opt(hour, minute, second, micro).ok_or_else(bad)?;
    let tz = FixedOffset::east_opt(offset_secs).ok_or_else(bad)?;
    naive.and_local_timezone(tz).single().ok_or_else(bad)
}

/// `rawContract.value` hex as a decimal string for NUMERIC storage ("0" if unparseable).
pub fn value_raw(t: &AssetTransfer) -> String {
    let hex = t.raw_contract.value.trim();
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    if hex.is_empty() || !is_hex_int(hex) {
        return "0".into();
    }
    hex_to_decimal(hex).unwrap_or_else(|| "0".into())
}

/// The ERC-20 contract address, or the asset name / "native" for other categories.
pub fn asset_label(t: &AssetTransfer) -> String {
    if t.category.to_lowercase() == "erc20" && !t.raw_contract.address.is_empty() {
        return normalize_address(&t.raw_contract.address);
    }
    if !t.asset.is_empty() {
        return t.asset.clone();
    }
    "native".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"transfers":[
        {"blockNum":"0x3a2f1b0","uniqueId":"0xabc:log:3","hash":"0xABC",
         "from":"0x1111111111111111111111111111111111111111","to":"0x2222222222222222222222222222222222222222",
         "value":12.5,"erc721TokenId":null,"asset":"USDC","category":"erc20",
         "rawContract":{"value":"0xbebc20","address":"0x2791BCA1F2DE4661ED88A30C99A7A9449AA84174","decimal":"0x6"},
         "metadata":{"blockTimestamp":"2024-10-01T12:34:56.789Z"}},
        {"blockNum":"0x10","hash":"0xdef","from":"0x3333333333333333333333333333333333333333","to":null,
         "value":null,"asset":"MATIC","category":"external","rawContract":{"value":null,"address":null,"decimal":null},
         "metadata":{}}
      ],"pageKey":"next-1"}}"#;

    #[test]
    fn decodes_a_page() {
        let (transfers, key) = decode_transfers_response(PAGE.as_bytes()).unwrap();
        assert_eq!(key, "next-1");
        assert_eq!(transfers.len(), 2);
        let t = &transfers[0];
        assert_eq!(parse_block_num(&t.block_num).unwrap(), 0x3a2f1b0);
        assert_eq!(t.value, Some(json!(12.5)));
        assert_eq!(dedupe_id(t), "0xabc:log:3");
        assert_eq!(value_raw(t), "12500000");
        assert_eq!(asset_label(t), "0x2791bca1f2de4661ed88a30c99a7a9449aa84174");
        assert_eq!(
            parse_block_time(&t.metadata.block_timestamp).unwrap().to_rfc3339(),
            "2024-10-01T12:34:56.789+00:00"
        );
        let t = &transfers[1];
        assert_eq!(t.to, "");
        assert_eq!(t.value, None);
        assert_eq!(dedupe_id(t), "0xdef:0x3333333333333333333333333333333333333333:::0");
        assert_eq!(value_raw(t), "0");
        assert_eq!(asset_label(t), "MATIC");
        assert!(parse_block_time(&t.metadata.block_timestamp).is_err());
    }

    #[test]
    fn envelope_errors() {
        let err = |s: &str| decode_transfers_response(s.as_bytes()).unwrap_err().to_string();
        assert_eq!(
            err(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"bad params"}}"#),
            "alchemy error -32602: bad params"
        );
        assert_eq!(err(r#"{"jsonrpc":"2.0","id":1}"#), "decode transfers: unexpected end of JSON input");
        assert_eq!(err("null"), "decode transfers: unexpected end of JSON input");
        assert_eq!(err(""), "decode rpc envelope: unexpected end of JSON input");
        assert_eq!(err("[1]"), "decode rpc envelope: invalid value: expected JSON-RPC response, got array");
        assert_eq!(err("{} x"), "decode rpc envelope: Extra data: line 1 column 4 (char 3)");
        assert_eq!(
            err(r#"{"error":{"code":"x"}}"#),
            r#"decode rpc envelope: invalid value for field "RPCError.code": expected integer, got string"#
        );
        assert_eq!(
            err(r#"{"error":5}"#),
            r#"decode rpc envelope: invalid value for field "rpcResp.error": expected RPCError, got number 5"#
        );
        assert_eq!(
            err(r#"{"result":{"transfers":{}}}"#),
            r#"decode transfers: invalid value for field "GetTransfersResult.transfers": expected list of AssetTransfer, got object"#
        );
        assert_eq!(decode_transfers_response(br#"{"result":null}"#).unwrap(), (vec![], String::new()));
    }

    #[test]
    fn request_body_is_sorted_and_compact() {
        let body = transfers_request_body("0xabc", TransferDirection::In, "pk", 1000);
        assert_eq!(
            body,
            r#"{"id":1,"jsonrpc":"2.0","method":"alchemy_getAssetTransfers","params":[{"category":["external","erc20","internal"],"excludeZeroValue":false,"fromBlock":"0x0","maxCount":"0x3e8","order":"desc","pageKey":"pk","toAddress":"0xabc","toBlock":"latest","withMetadata":true}]}"#
        );
        let body = transfers_request_body("0xabc", TransferDirection::Out, "é", 5);
        assert!(body.contains(r#""fromAddress":"0xabc""#) && body.contains(r#""maxCount":"0x5""#));
        assert!(body.contains("\"pageKey\":\"\\u00e9\""), "{body}");
    }

    #[test]
    fn block_numbers() {
        assert_eq!(parse_block_num(" 0x10 ").unwrap(), 16);
        assert_eq!(parse_block_num("ff").unwrap(), 255);
        assert_eq!(parse_block_num("").unwrap_err().0, "empty block");
        assert_eq!(parse_block_num("0xzz").unwrap_err().0, "invalid block number \"zz\": not a hex integer");
        assert_eq!(
            parse_block_num("0x8000000000000000").unwrap_err().0,
            "invalid block number \"8000000000000000\": value out of range"
        );
        assert_eq!(parse_block_num("0x7fffffffffffffff").unwrap(), i64::MAX);
        assert_eq!(parse_block_num("-0").unwrap(), 0);
    }

    #[test]
    fn block_times() {
        let t = parse_block_time("2024-10-01T12:34:56.123456789+05:30").unwrap();
        assert_eq!(t.to_rfc3339(), "2024-10-01T12:34:56.123456+05:30");
        assert_eq!(parse_block_time("2024-10-01T12:34:56,5Z").unwrap().timestamp_subsec_micros(), 500000);
        assert_eq!(parse_block_time("").unwrap_err().0, "empty timestamp");
        assert_eq!(
            parse_block_time("2024-10-01").unwrap_err().0,
            "parsing time \"2024-10-01\": not an RFC 3339 timestamp"
        );
        assert_eq!(
            parse_block_time("2024-10-01T00:00:00+24:00").unwrap_err().0,
            "parsing time \"2024-10-01T00:00:00+24:00\": time zone offset out of range"
        );
        assert_eq!(
            parse_block_time("2024-02-30T00:00:00Z").unwrap_err().0,
            "parsing time \"2024-02-30T00:00:00Z\": day is out of range for month"
        );
    }

    fn fixture_pages(key: &str) -> Result<(Vec<AssetTransfer>, String)> {
        let body = match key {
            "" => r#"{"result":{"transfers":[{"hash":"a"},{"hash":"b"}],"pageKey":"k1"}}"#,
            "k1" => r#"{"result":{"transfers":[{"hash":"c"}],"pageKey":"k2"}}"#,
            "k2" => r#"{"result":{"transfers":[{"hash":"d"}],"pageKey":"k3"}}"#,
            "k3" => r#"{"result":{"transfers":[],"pageKey":"k4"}}"#,
            _ => panic!("unexpected page key {key}"),
        };
        decode_transfers_response(body.as_bytes())
    }

    #[test]
    fn pagination_follows_page_keys() {
        let mut keys = Vec::new();
        let mut seen = Vec::new();
        paginate(
            100,
            |k| {
                keys.push(k.to_string());
                fixture_pages(k)
            },
            |ts| {
                seen.extend(ts.iter().map(|t| t.hash.clone()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(keys, vec!["", "k1", "k2", "k3"]);
        assert_eq!(seen, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn pagination_respects_max_pages_and_missing_keys() {
        let mut calls = 0;
        paginate(
            2,
            |k| {
                calls += 1;
                fixture_pages(k)
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(calls, 2);
        let mut calls = 0;
        paginate(
            10,
            |_| {
                calls += 1;
                decode_transfers_response(br#"{"result":{"transfers":[{"hash":"x"}]}}"#)
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(calls, 1);
        let err = paginate(5, |_| Err(anyhow!("boom")), |_| Ok(())).unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }
}
