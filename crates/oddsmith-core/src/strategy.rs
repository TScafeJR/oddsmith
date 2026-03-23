//! Strategy parameters, strategy JSON files, date windows and the fee model.

use std::fs;
use std::io;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Datelike, NaiveDate, TimeDelta, Utc};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::textfmt;

/// Parameters for a backtesting strategy.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Strategy {
    pub name: String,
    pub description: String,
    /// "YES" or "NO": which token to buy.
    pub side: String,
    /// Min YES implied probability to enter (e.g. 0.50).
    pub min_price: f64,
    /// Max YES implied probability (e.g. 0.95).
    pub max_price: f64,
    pub avoid_below: f64,
    /// "MAKER" or "TAKER": selects which fee rate applies.
    pub role: String,
    pub initial_capital: f64,
    pub position_size: f64,
    pub maker_fee_rate: f64,
    pub taker_fee_rate: f64,
    /// Bucket width in percentage points (e.g. 5 = 5%).
    pub bucket_width: f64,
    /// Custom bucket boundaries as percentages.
    pub buckets: Option<Vec<f64>>,
    /// YYYY-MM-DD, inclusive; empty = no lower bound.
    pub start_date: String,
    /// YYYY-MM-DD, inclusive; empty = no upper bound.
    pub end_date: String,
    /// "traded" (default) or "resolved".
    pub window_by: String,
    pub enforce_capital: bool,
}

impl Strategy {
    /// Bucket label for a price given the strategy's custom buckets (default 10-cent buckets).
    pub fn price_bucket(&self, price: f64) -> String {
        let buckets = self.buckets.as_deref().unwrap_or(&[]);
        match bucket_slot(buckets, is_sorted(buckets), price * 100.0) {
            Slot::Tens(b) => tens_label(b),
            Slot::Custom(i) => custom_label(buckets, i),
        }
    }

    /// The date window this strategy is restricted to (validated).
    pub fn window(&self) -> Result<Window> {
        parse_window(&self.start_date, &self.end_date, &self.window_by)
    }

    /// Fee coefficient for the strategy's role.
    pub fn fee_rate(&self) -> f64 {
        if self.role.to_lowercase() == "taker" { self.taker_fee_rate } else { self.maker_fee_rate }
    }
}

/// Where a price falls: a default 10-cent bucket (its lower bound) or a custom bucket index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    Tens(i64),
    Custom(usize),
}

pub(crate) fn is_sorted(buckets: &[f64]) -> bool {
    buckets.windows(2).all(|w| w[0] <= w[1])
}

/// The bucket for a percentage: the first matching bucket in order.
/// Sorted boundaries use a binary search; anything else is scanned in order.
pub(crate) fn bucket_slot(buckets: &[f64], sorted: bool, pct: f64) -> Slot {
    if buckets.len() < 2 {
        let mut b = (pct / 10.0).trunc() as i64 * 10;
        if b >= 100 {
            b = 90;
        }
        return Slot::Tens(b);
    }
    let n = buckets.len();
    if sorted {
        let k = buckets.partition_point(|&b| b <= pct);
        return if k == 0 {
            Slot::Custom(0)
        } else if k == n {
            Slot::Custom(n - 2)
        } else {
            Slot::Custom(k - 1)
        };
    }
    for i in 0..n - 1 {
        if buckets[i] <= pct && pct < buckets[i + 1] {
            return Slot::Custom(i);
        }
    }
    if pct >= buckets[n - 1] { Slot::Custom(n - 2) } else { Slot::Custom(0) }
}

pub(crate) fn tens_label(b: i64) -> String {
    format!("{b}-{}", b + 10)
}

pub(crate) fn custom_label(buckets: &[f64], i: usize) -> String {
    format!("{}-{}", textfmt::g(buckets[i], 4), textfmt::g(buckets[i + 1], 4))
}

/// Which timestamp of a trade a [`Window`] filters on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowBy {
    #[default]
    Traded,
    Resolved,
}

impl WindowBy {
    pub fn as_str(self) -> &'static str {
        match self {
            WindowBy::Traded => "traded",
            WindowBy::Resolved => "resolved",
        }
    }
}

/// A half-open UTC time range `[start, end)` applied to one timestamp of each trade.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Window {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub by: WindowBy,
}

impl Window {
    pub fn is_set(&self) -> bool {
        self.start.is_some() || self.end.is_some()
    }

    pub fn contains(&self, ts: DateTime<Utc>) -> bool {
        if self.start.is_some_and(|s| ts < s) {
            return false;
        }
        !self.end.is_some_and(|e| ts >= e)
    }

    pub fn describe(&self) -> String {
        if !self.is_set() {
            return "all time".into();
        }
        let lo = self.start.map_or("start".to_string(), |s| s.date_naive().format("%Y-%m-%d").to_string());
        let hi = self
            .end
            .map_or("now".to_string(), |e| (e - TimeDelta::days(1)).date_naive().format("%Y-%m-%d").to_string());
        format!("{lo} to {hi} (by {})", self.by.as_str())
    }
}

/// Parse inclusive YYYY-MM-DD bounds into a half-open UTC window.
pub fn parse_window(start_date: &str, end_date: &str, window_by: &str) -> Result<Window> {
    let by = match (if window_by.is_empty() { "traded" } else { window_by }).to_lowercase().as_str() {
        "traded" => WindowBy::Traded,
        "resolved" => WindowBy::Resolved,
        _ => bail!("invalid window_by {}: must be traded or resolved", textfmt::json_str(window_by)),
    };
    let start = parse_day(start_date, "start_date")?;
    let mut end = parse_day(end_date, "end_date")?;
    if let Some(e) = end {
        if e.year() >= 9999 && e.month() == 12 && e.day() == 31 {
            bail!("date value out of range");
        }
        end = Some(e + TimeDelta::days(1)); // make the end date inclusive
    }
    if let (Some(s), Some(e)) = (start, end)
        && s >= e
    {
        bail!("start_date {start_date} is after end_date {end_date}");
    }
    Ok(Window { start, end, by })
}

fn parse_day(value: &str, name: &str) -> Result<Option<DateTime<Utc>>> {
    if value.is_empty() {
        return Ok(None);
    }
    match strptime_ymd(value) {
        Some(d) => Ok(Some(d.and_hms_opt(0, 0, 0).expect("midnight").and_utc())),
        None => bail!("invalid {name} {}: expected YYYY-MM-DD", textfmt::json_str(value)),
    }
}

/// Parse `%Y-%m-%d`: four-digit year, one- or two-digit month and
/// day (a day may also be a space and a digit), nothing left over.
fn strptime_ymd(value: &str) -> Option<NaiveDate> {
    let b = value.as_bytes();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    if !(0..4).all(digit) || b.get(4) != Some(&b'-') {
        return None;
    }
    let year: i32 = value[..4].parse().ok()?;
    let rest = &b[5..];
    // %m: 1[0-2] | 0[1-9] | [1-9], followed by "-"
    let (month, used) = match rest {
        [b'1', c @ b'0'..=b'2', b'-', ..] => (10 + (c - b'0') as u32, 2),
        [b'0', c @ b'1'..=b'9', b'-', ..] => ((c - b'0') as u32, 2),
        [c @ b'1'..=b'9', b'-', ..] => ((c - b'0') as u32, 1),
        _ => return None,
    };
    let day_part = &rest[used + 1..];
    // %d: 3[01] | [12]\d | 0[1-9] | [1-9] | " "[1-9]; the first alternative that matches wins.
    let (day, used) = match day_part {
        [b'3', c @ b'0'..=b'1', ..] => (30 + (c - b'0') as u32, 2),
        [a @ b'1'..=b'2', c @ b'0'..=b'9', ..] => ((a - b'0') as u32 * 10 + (c - b'0') as u32, 2),
        [b'0', c @ b'1'..=b'9', ..] => ((c - b'0') as u32, 2),
        [c @ b'1'..=b'9', ..] => ((c - b'0') as u32, 1),
        [b' ', c @ b'1'..=b'9', ..] => ((c - b'0') as u32, 2),
        _ => return None,
    };
    if used != day_part.len() || year < 1 {
        return None;
    }
    NaiveDate::from_ymd_opt(year, month, day)
}

/// The JSON type name reported for a value in error messages.
pub fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        _ => "object",
    }
}

/// Typed field access on a decoded JSON object: missing keys and nulls read as `None`,
/// unknown keys are ignored, and a value of the wrong JSON type is an error naming the
/// field. Integers are accepted where a number is expected; booleans are not.
#[derive(Debug, Clone, Copy)]
pub struct JsonObject<'a> {
    map: &'a Map<String, Value>,
}

impl<'a> JsonObject<'a> {
    /// The object, or `invalid {type_name}: expected object, got ...`.
    pub fn new(value: &'a Value, type_name: &str) -> Result<Self> {
        match value {
            Value::Object(map) => Ok(JsonObject { map }),
            other => bail!("invalid {type_name}: expected object, got {}", json_kind(other)),
        }
    }

    fn get(&self, name: &str) -> Option<&'a Value> {
        self.map.get(name).filter(|v| !v.is_null())
    }

    fn wrong(name: &str, expected: &str, v: &Value) -> anyhow::Error {
        anyhow!("invalid value for field \"{name}\": expected {expected}, got {}", json_kind(v))
    }

    pub fn string(&self, name: &str) -> Result<Option<String>> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(v) => Err(Self::wrong(name, "string", v)),
        }
    }

    pub fn number(&self, name: &str) -> Result<Option<f64>> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::Number(n)) => Ok(Some(n.as_f64().unwrap_or(f64::NAN))),
            Some(v) => Err(Self::wrong(name, "number", v)),
        }
    }

    pub fn boolean(&self, name: &str) -> Result<Option<bool>> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(v) => Err(Self::wrong(name, "boolean", v)),
        }
    }

    pub fn number_list(&self, name: &str) -> Result<Option<Vec<f64>>> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::Array(items)) if items.iter().all(Value::is_number) => {
                Ok(Some(items.iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect()))
            }
            Some(v) => Err(Self::wrong(name, "list of numbers", v)),
        }
    }
}

/// Build a strategy from a JSON object. Missing keys and nulls keep the zero value and
/// unknown keys are ignored; a value of the wrong JSON type is rejected with a message
/// naming the field. `type_name` is used in errors.
pub fn strategy_from_json(value: &Value, type_name: &str) -> Result<Strategy> {
    let o = JsonObject::new(value, type_name)?;
    let mut s = Strategy::default();
    // Fields are checked in declaration order, so the first bad one is reported.
    macro_rules! field {
        ($name:ident, $get:ident) => {
            if let Some(v) = o.$get(stringify!($name))? {
                s.$name = v;
            }
        };
    }
    field!(name, string);
    field!(description, string);
    field!(side, string);
    field!(min_price, number);
    field!(max_price, number);
    field!(avoid_below, number);
    field!(role, string);
    field!(initial_capital, number);
    field!(position_size, number);
    field!(maker_fee_rate, number);
    field!(taker_fee_rate, number);
    field!(bucket_width, number);
    if let Some(v) = o.number_list("buckets")? {
        s.buckets = Some(v);
    }
    field!(start_date, string);
    field!(end_date, string);
    field!(window_by, string);
    field!(enforce_capital, boolean);
    Ok(s)
}

/// Error text for an I/O failure on a path: `x: No such file or directory (os error 2)`.
pub fn os_error_text(e: &io::Error, path: &str) -> String {
    format!("{path}: {e}")
}

/// Load a strategy JSON file (fills defaults and buckets from `bucket_width`, validates
/// the window).
pub fn load_strategy(path: impl AsRef<Path>) -> Result<Strategy> {
    let path = path.as_ref();
    let data =
        fs::read(path).map_err(|e| anyhow!("reading strategy file: {}", os_error_text(&e, &path.to_string_lossy())))?;
    let text = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&data);
    let value: Value = serde_json::from_slice(text).map_err(|e| anyhow!("parsing strategy file: {e}"))?;
    let mut s = strategy_from_json(&value, "strategy").map_err(|e| anyhow!("parsing strategy file: {e}"))?;
    if s.side.is_empty() {
        s.side = "YES".into();
    }
    if s.role.is_empty() {
        s.role = "MAKER".into();
    }
    if s.initial_capital == 0.0 {
        s.initial_capital = 10000.0;
    }
    if s.position_size == 0.0 {
        s.position_size = 100.0;
    }
    // Generate bucket boundaries from bucket_width if set and no explicit buckets
    if s.bucket_width > 0.0 && s.buckets.as_ref().is_none_or(Vec::is_empty) {
        s.buckets = generate_buckets(s.min_price, s.max_price, s.bucket_width);
    }
    s.window().map_err(|e| anyhow!("parsing strategy file: {e}"))?;
    Ok(s)
}

/// Fee per contract bought at price p: `rate * p * (1 - p)`.
///
/// Fees peak at 50c in dollar terms, but as a fraction of cost (`rate * (1 - p)`) they
/// hit cheap contracts hardest.
pub fn contract_fee(price: f64, rate: f64) -> f64 {
    rate * price * (1.0 - price)
}

/// Bucket boundaries from min/max price and a width (all in percentage points).
pub fn generate_buckets(min_price: f64, max_price: f64, width: f64) -> Option<Vec<f64>> {
    let min_pct = (min_price * 100.0).floor();
    let max_pct = (max_price * 100.0).ceil();
    let mut buckets = Vec::new();
    let mut b = min_pct;
    while b <= max_pct {
        buckets.push(b);
        b += width;
    }
    // Ensure the last boundary covers max_pct
    if buckets.last().is_some_and(|&last| last < max_pct) {
        buckets.push(max_pct);
    }
    if buckets.is_empty() { None } else { Some(buckets) }
}

/// The leading float token of `s` (`[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?`).
fn float_prefix(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let digits = |mut i: usize| {
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        i
    };
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i = 1;
    }
    let int_end = digits(i);
    if int_end > i {
        i = int_end;
        if b.get(i) == Some(&b'.') {
            i = digits(i + 1);
        }
    } else if b.get(i) == Some(&b'.') && digits(i + 1) > i + 1 {
        i = digits(i + 1);
    } else {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let end = digits(j);
        if end > j {
            i = end;
        }
    }
    Some(&s[..i])
}

/// Parse custom bucket boundaries in percent, e.g. `"0,5,10,20,50,100"`. Spaces are
/// ignored, as is trailing junk after a number.
pub fn parse_buckets(s: &str) -> Result<Option<Vec<f64>>> {
    if s.is_empty() {
        return Ok(None);
    }
    let mut buckets = Vec::new();
    for part in s.split(',').map(|p| p.replace(' ', "")).filter(|p| !p.is_empty()) {
        let Some(tok) = float_prefix(&part) else {
            bail!("invalid bucket boundary {}: not a number", textfmt::json_str(&part));
        };
        buckets.push(textfmt::parse_float(tok).unwrap_or(f64::NAN));
    }
    buckets.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if buckets.len() < 2 {
        bail!("need at least 2 bucket boundaries, got {}", buckets.len());
    }
    Ok(Some(buckets))
}

/// The built-in maker-on-favorites strategy.
pub fn default_strategy() -> Strategy {
    Strategy {
        name: "maker_favorites".into(),
        description: "Buy YES tokens on favorites priced 50-95 cents".into(),
        side: "YES".into(),
        min_price: 0.50,
        max_price: 0.95,
        avoid_below: 0.10,
        role: "MAKER".into(),
        initial_capital: 10000.0,
        position_size: 100.0,
        ..Strategy::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strptime_rules() {
        let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d);
        assert_eq!(strptime_ymd("2024-01-31"), d(2024, 1, 31));
        assert_eq!(strptime_ymd("2024-1-5"), d(2024, 1, 5));
        assert_eq!(strptime_ymd("2024-12- 5"), d(2024, 12, 5));
        assert_eq!(strptime_ymd("2024-13-01"), None);
        assert_eq!(strptime_ymd("2024-02-30"), None);
        assert_eq!(strptime_ymd("2024-01-35"), None);
        assert_eq!(strptime_ymd("2024-01-01x"), None);
        assert_eq!(strptime_ymd("Jan 1"), None);
        assert_eq!(strptime_ymd("0000-01-01"), None);
    }

    #[test]
    fn bucket_labels() {
        let s = Strategy { buckets: Some(vec![0.0, 5.0, 10.0, 30.0, 40.0]), ..Strategy::default() };
        assert_eq!(s.price_bucket(0.07), "5-10");
        assert_eq!(s.price_bucket(0.35), "30-40");
        assert_eq!(s.price_bucket(1.0), "30-40");
        assert_eq!(s.price_bucket(-0.5), "0-5");
        assert_eq!(s.price_bucket(f64::NAN), "0-5");
        let unsorted = Strategy { buckets: Some(vec![50.0, 0.0, 10.0]), ..Strategy::default() };
        assert_eq!(unsorted.price_bucket(0.05), "0-10");
        assert_eq!(unsorted.price_bucket(0.2), "0-10");
        assert_eq!(unsorted.price_bucket(-0.01), "50-0");
        assert_eq!(Strategy::default().price_bucket(0.999), "90-100");
        assert_eq!(Strategy::default().price_bucket(1.0), "90-100");
        assert_eq!(Strategy::default().price_bucket(0.07), "0-10");
        let fine = Strategy { buckets: generate_buckets(0.0, 0.33, 0.01), ..Strategy::default() };
        assert_eq!(fine.buckets.as_ref().map(Vec::len), Some(3301));
        assert_eq!(fine.price_bucket(0.2), "19.99-20");
        assert_eq!(fine.price_bucket(0.33), "32.99-33");
        assert_eq!(fine.price_bucket(0.07), "7-7.01");
    }

    #[test]
    fn parse_buckets_leading_number() {
        assert_eq!(parse_buckets("").unwrap(), None);
        assert_eq!(parse_buckets("10, 0,5x,,1e1").unwrap(), Some(vec![0.0, 5.0, 10.0, 10.0]));
        assert_eq!(parse_buckets("1.,.5,1e").unwrap(), Some(vec![0.5, 1.0, 1.0]));
        let err = parse_buckets("0,abc").unwrap_err().to_string();
        assert_eq!(err, "invalid bucket boundary \"abc\": not a number");
        assert_eq!(parse_buckets("5").unwrap_err().to_string(), "need at least 2 bucket boundaries, got 1");
    }

    #[test]
    fn json_type_errors_name_the_field() {
        let err = |v: Value| strategy_from_json(&v, "strategy").unwrap_err().to_string();
        assert_eq!(
            err(serde_json::json!({"min_price": "0.5"})),
            "invalid value for field \"min_price\": expected number, got string"
        );
        assert_eq!(
            err(serde_json::json!({"enforce_capital": 1})),
            "invalid value for field \"enforce_capital\": expected boolean, got number"
        );
        assert_eq!(
            err(serde_json::json!({"max_price": true})),
            "invalid value for field \"max_price\": expected number, got bool"
        );
        assert_eq!(
            err(serde_json::json!({"buckets": [1, "2"]})),
            "invalid value for field \"buckets\": expected list of numbers, got array"
        );
        assert_eq!(err(serde_json::json!([1])), "invalid strategy: expected object, got array");
        assert_eq!(err(Value::Null), "invalid strategy: expected object, got object");
        // The first bad field in declaration order is reported.
        assert_eq!(
            err(serde_json::json!({"window_by": 1, "side": 2})),
            "invalid value for field \"side\": expected string, got number"
        );
        let s = strategy_from_json(
            &serde_json::json!({"min_price": 1, "unknown": "x", "role": null, "buckets": [5, 10.5]}),
            "strategy",
        )
        .unwrap();
        assert_eq!(s.min_price, 1.0);
        assert_eq!(s.buckets, Some(vec![5.0, 10.5]));
        assert_eq!(s.role, "");
    }
}
