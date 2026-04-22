//! The lenient JSON decoding toolkit shared by the Gamma, CLOB, Data API and Alchemy clients.
//!
//! Rules: keys match case-insensitively (later keys win, `null` is a no-op), `null` leaves a
//! field at its default, types are checked strictly (a JSON number in a string field is an
//! error), [`Number`] fields keep the literal text and also accept a quoted number, and
//! response bodies are decoded from their first JSON value only.
//!
//! Parse errors carry a message and position (`Expecting value: line 1 column 1 (char 0)`),
//! and number literals are kept as text.

use std::fmt;

use crate::text::{json_quote, strip_ws};

/// A parsed JSON value. Numbers keep their literal text; in objects a repeated key replaces
/// the earlier value in place.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

/// A decoding error and its message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodeError {}

pub type DResult<T> = Result<T, DecodeError>;

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

struct Parser {
    s: Vec<char>,
}

impl Parser {
    fn err(&self, msg: &str, pos: usize) -> DecodeError {
        let before = &self.s[..pos.min(self.s.len())];
        let lineno = before.iter().filter(|c| **c == '\n').count() + 1;
        let colno = match before.iter().rposition(|c| *c == '\n') {
            Some(nl) => pos - nl,
            None => pos + 1,
        };
        DecodeError(format!("{msg}: line {lineno} column {colno} (char {pos})"))
    }

    fn at(&self, i: usize) -> Option<char> {
        self.s.get(i).copied()
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while self.at(i).is_some_and(is_ws) {
            i += 1;
        }
        i
    }

    fn starts_with(&self, i: usize, lit: &str) -> bool {
        let n = lit.chars().count();
        i + n <= self.s.len() && self.s[i..i + n].iter().copied().eq(lit.chars())
    }

    /// Parse a value starting exactly at `i`.
    fn value(&self, i: usize) -> DResult<(Value, usize)> {
        let Some(c) = self.at(i) else { return Err(self.err("Expecting value", i)) };
        match c {
            '"' => self.string(i + 1).map(|(s, e)| (Value::Str(s), e)),
            '{' => self.object(i + 1),
            '[' => self.array(i + 1),
            'n' if self.starts_with(i, "null") => Ok((Value::Null, i + 4)),
            't' if self.starts_with(i, "true") => Ok((Value::Bool(true), i + 4)),
            'f' if self.starts_with(i, "false") => Ok((Value::Bool(false), i + 5)),
            'N' if self.starts_with(i, "NaN") => Err(reject_constant('N')),
            'I' if self.starts_with(i, "Infinity") => Err(reject_constant('I')),
            '-' if self.starts_with(i, "-Infinity") => Err(reject_constant('-')),
            _ => match self.number(i) {
                Some(end) => Ok((Value::Num(self.s[i..end].iter().collect()), end)),
                None => Err(self.err("Expecting value", i)),
            },
        }
    }

    /// End index of the number literal at `start`, or `None` if there is none.
    fn number(&self, start: usize) -> Option<usize> {
        let digit = |i: usize| self.at(i).is_some_and(|c| c.is_ascii_digit());
        let len = self.s.len();
        let mut i = start;
        if self.at(i) == Some('-') {
            i += 1;
            if i >= len {
                return None;
            }
        }
        match self.at(i) {
            Some('1'..='9') => {
                i += 1;
                while digit(i) {
                    i += 1;
                }
            }
            Some('0') => i += 1,
            _ => return None,
        }
        if i + 1 < len && self.at(i) == Some('.') && digit(i + 1) {
            i += 2;
            while digit(i) {
                i += 1;
            }
        }
        if i + 1 < len && matches!(self.at(i), Some('e' | 'E')) {
            let e_start = i;
            i += 1;
            if i + 1 < len && matches!(self.at(i), Some('-' | '+')) {
                i += 1;
            }
            while digit(i) {
                i += 1;
            }
            if !digit(i - 1) {
                i = e_start;
            }
        }
        Some(i)
    }

    /// Parse the string whose opening quote is at `end - 1`.
    fn string(&self, mut end: usize) -> DResult<(String, usize)> {
        let begin = end - 1;
        let len = self.s.len();
        let mut out = String::new();
        loop {
            let mut next = end;
            let mut c = '\0';
            while next < len {
                c = self.s[next];
                if c == '"' || c == '\\' {
                    break;
                }
                if (c as u32) <= 0x1f {
                    return Err(self.err("Invalid control character at", next));
                }
                next += 1;
            }
            if next >= len {
                return Err(self.err("Unterminated string starting at", begin));
            }
            out.extend(&self.s[end..next]);
            next += 1;
            if c == '"' {
                end = next;
                break;
            }
            if next == len {
                return Err(self.err("Unterminated string starting at", begin));
            }
            let c = self.s[next];
            if c != 'u' {
                end = next + 1;
                out.push(match c {
                    '"' => '"',
                    '\\' => '\\',
                    '/' => '/',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return Err(self.err("Invalid \\escape", end - 2)),
                });
                continue;
            }
            next += 1;
            end = next + 4;
            if end >= len {
                return Err(self.err("Invalid \\uXXXX escape", next - 1));
            }
            let Some(mut cp) = self.hex4(next) else { return Err(self.err("Invalid \\uXXXX escape", end - 5)) };
            if (0xd800..=0xdbff).contains(&cp) && end + 6 < len && self.s[end] == '\\' && self.s[end + 1] == 'u' {
                let Some(c2) = self.hex4(end + 2) else {
                    return Err(self.err("Invalid \\uXXXX escape", end + 1));
                };
                if (0xdc00..=0xdfff).contains(&c2) {
                    cp = 0x10000 + (((cp - 0xd800) << 10) | (c2 - 0xdc00));
                    end += 6;
                }
            }
            // Lone surrogates can't live in a Rust string.
            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
        }
        Ok((out, end))
    }

    fn hex4(&self, i: usize) -> Option<u32> {
        let mut v = 0u32;
        for k in 0..4 {
            v = v * 16 + self.at(i + k)?.to_digit(16)?;
        }
        Some(v)
    }

    fn object(&self, start: usize) -> DResult<(Value, usize)> {
        let mut entries: Vec<(String, Value)> = Vec::new();
        let mut i = self.skip_ws(start);
        if self.at(i) == Some('}') {
            return Ok((Value::Object(entries), i + 1));
        }
        loop {
            if self.at(i) != Some('"') {
                return Err(self.err("Expecting property name enclosed in double quotes", i));
            }
            let (key, e) = self.string(i + 1)?;
            i = self.skip_ws(e);
            if self.at(i) != Some(':') {
                return Err(self.err("Expecting ':' delimiter", i));
            }
            i = self.skip_ws(i + 1);
            let (val, e) = self.value(i)?;
            match entries.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = val,
                None => entries.push((key, val)),
            }
            i = self.skip_ws(e);
            if self.at(i) == Some('}') {
                return Ok((Value::Object(entries), i + 1));
            }
            if self.at(i) != Some(',') {
                return Err(self.err("Expecting ',' delimiter", i));
            }
            let comma = i;
            i = self.skip_ws(i + 1);
            if self.at(i) == Some('}') {
                return Err(self.err("Illegal trailing comma before end of object", comma));
            }
        }
    }

    fn array(&self, start: usize) -> DResult<(Value, usize)> {
        let mut items = Vec::new();
        let mut i = self.skip_ws(start);
        if self.at(i) == Some(']') {
            return Ok((Value::Array(items), i + 1));
        }
        loop {
            let (val, e) = self.value(i)?;
            items.push(val);
            i = self.skip_ws(e);
            if self.at(i) == Some(']') {
                return Ok((Value::Array(items), i + 1));
            }
            if self.at(i) != Some(',') {
                return Err(self.err("Expecting ',' delimiter", i));
            }
            let comma = i;
            i = self.skip_ws(i + 1);
            if self.at(i) == Some(']') {
                return Err(self.err("Illegal trailing comma before end of array", comma));
            }
        }
    }
}

fn reject_constant(first: char) -> DecodeError {
    DecodeError(format!("invalid character '{first}' looking for beginning of value"))
}

/// Parse the first JSON value in a response body and ignore anything after it.
pub fn parse_first(text: &str) -> DResult<Value> {
    let p = Parser { s: text.chars().collect() };
    let i = p.skip_ws(0);
    if i == p.s.len() {
        return Err(DecodeError("EOF".into()));
    }
    Ok(p.value(i)?.0)
}

/// Parse text that must be exactly one JSON value.
pub fn parse_exact(text: &str) -> DResult<Value> {
    if text.trim_matches(is_ws).is_empty() {
        return Err(DecodeError("unexpected end of JSON input".into()));
    }
    let p = Parser { s: text.chars().collect() };
    let (v, end) = p.value(p.skip_ws(0))?;
    let end = p.skip_ws(end);
    if end != p.s.len() {
        return Err(p.err("Extra data", end));
    }
    Ok(v)
}

/// Decode the first JSON value of a response body with `dec`.
pub fn decode_body<T>(body: &[u8], dec: impl Fn(Option<&Value>, &str) -> DResult<T>) -> DResult<T> {
    let v = parse_first(&String::from_utf8_lossy(body))?;
    dec(Some(&v), "")
}

/// Decode data that must be exactly one JSON value with `dec`.
pub fn unmarshal<T>(data: &str, dec: impl Fn(Option<&Value>, &str) -> DResult<T>) -> DResult<T> {
    let v = parse_exact(data)?;
    dec(Some(&v), "")
}

// ---------------------------------------------------------------------------
// Typed decoders
// ---------------------------------------------------------------------------

fn json_kind(v: &Value) -> String {
    match v {
        Value::Num(lit) => format!("number {lit}"),
        Value::Bool(_) => "bool".into(),
        Value::Str(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(_) => "object".into(),
        Value::Null => "null".into(),
    }
}

/// Readable type name for error messages, e.g. `"[]string"` -> `"list of string"`.
pub fn describe_type(t: &str) -> String {
    if let Some(rest) = t.strip_prefix("[]") {
        return format!("list of {}", describe_type(rest));
    }
    match t {
        "float64" => "number",
        "int64" => "integer",
        "json.Number" => "number",
        "slice" => "list",
        _ => t,
    }
    .to_string()
}

/// A JSON value that doesn't fit the type it was decoded into.
pub fn type_error(v: &Value, path: &str, expected: &str) -> DecodeError {
    let what =
        if path.is_empty() { "invalid value".to_string() } else { format!("invalid value for field \"{path}\"") };
    DecodeError(format!("{what}: expected {}, got {}", describe_type(expected), json_kind(v)))
}

fn present(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| !matches!(v, Value::Null))
}

pub fn dec_str(v: Option<&Value>, path: &str) -> DResult<String> {
    match present(v) {
        None => Ok(String::new()),
        Some(Value::Str(s)) => Ok(s.clone()),
        Some(v) => Err(type_error(v, path, "string")),
    }
}

pub fn dec_bool(v: Option<&Value>, path: &str) -> DResult<bool> {
    match present(v) {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(v) => Err(type_error(v, path, "bool")),
    }
}

pub fn dec_number(v: Option<&Value>, path: &str) -> DResult<Number> {
    match present(v) {
        None => Ok(Number::default()),
        Some(Value::Num(lit)) => Ok(Number(lit.clone())),
        Some(Value::Str(s)) if is_json_number(s) => Ok(Number(s.clone())),
        Some(Value::Str(s)) => {
            Err(DecodeError(format!("json: invalid number literal, trying to unmarshal {} into Number", json_quote(s))))
        }
        Some(v) => Err(type_error(v, path, "json.Number")),
    }
}

pub fn dec_float(v: Option<&Value>, path: &str) -> DResult<f64> {
    match present(v) {
        None => Ok(0.0),
        Some(val @ Value::Num(lit)) => match lit.parse::<f64>() {
            Ok(f) if !f.is_infinite() => Ok(f),
            _ => Err(type_error(val, path, "float64")),
        },
        Some(v) => Err(type_error(v, path, "float64")),
    }
}

pub fn dec_int(v: Option<&Value>, path: &str) -> DResult<i64> {
    match present(v) {
        None => Ok(0),
        Some(val @ Value::Num(lit)) => {
            let digits = lit.strip_prefix('-').unwrap_or(lit);
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                if let Ok(n) = lit.parse::<i64>() {
                    return Ok(n);
                }
            }
            Err(type_error(val, path, "int64"))
        }
        Some(v) => Err(type_error(v, path, "int64")),
    }
}

/// An untyped value: numbers become floats, objects maps, arrays lists. `None` if absent.
pub fn dec_any(v: Option<&Value>, path: &str) -> DResult<Option<serde_json::Value>> {
    fn conv(v: &Value, path: &str) -> DResult<serde_json::Value> {
        Ok(match v {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::Num(lit) => match lit.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
                Some(n) => serde_json::Value::Number(n),
                None => return Err(type_error(v, path, "float64")),
            },
            Value::Str(s) => serde_json::Value::String(s.clone()),
            Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(|x| conv(x, path)).collect::<DResult<_>>()?)
            }
            Value::Object(entries) => serde_json::Value::Object(
                entries.iter().map(|(k, x)| Ok((k.clone(), conv(x, path)?))).collect::<DResult<_>>()?,
            ),
        })
    }
    present(v).map(|v| conv(v, path)).transpose()
}

/// A list decoded item by item; items share the list's path.
pub fn dec_list<T>(
    v: Option<&Value>,
    path: &str,
    expected: &str,
    item: impl Fn(Option<&Value>, &str) -> DResult<T>,
) -> DResult<Vec<T>> {
    match present(v) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items.iter().map(|x| item(Some(x), path)).collect(),
        Some(v) => Err(type_error(v, path, expected)),
    }
}

/// `[]string`, as used for the JSON-encoded string arrays in Gamma market fields.
pub fn dec_str_list(v: Option<&Value>, path: &str) -> DResult<Vec<String>> {
    dec_list(v, path, "[]string", dec_str)
}

/// Case-folded object keys: later keys win and `null` never overwrites an earlier key.
pub fn fold_keys(entries: &[(String, Value)]) -> Vec<(String, &Value)> {
    let mut out: Vec<(String, &Value)> = Vec::with_capacity(entries.len());
    for (k, val) in entries {
        let lk = k.to_lowercase();
        match out.iter_mut().find(|(o, _)| *o == lk) {
            Some(_) if matches!(val, Value::Null) => {}
            Some(slot) => slot.1 = val,
            None => out.push((lk, val)),
        }
    }
    out
}

/// The folded fields of a JSON object being decoded into the struct `name`.
pub struct Fields<'a> {
    name: &'static str,
    folded: Vec<(String, &'a Value)>,
}

impl<'a> Fields<'a> {
    /// `Ok(None)` when the value is absent (the struct keeps its defaults).
    pub fn new(v: Option<&'a Value>, path: &str, name: &'static str) -> DResult<Option<Fields<'a>>> {
        match present(v) {
            None => Ok(None),
            Some(Value::Object(entries)) => Ok(Some(Fields { name, folded: fold_keys(entries) })),
            Some(v) => Err(type_error(v, path, name)),
        }
    }

    pub fn raw(&self, key: &str) -> Option<&'a Value> {
        let lk = key.to_lowercase();
        self.folded.iter().find(|(k, _)| *k == lk).map(|(_, v)| *v)
    }

    /// Decode one field with `dec`; errors name the field as `Struct.key`.
    pub fn get<T>(&self, key: &str, dec: impl Fn(Option<&Value>, &str) -> DResult<T>) -> DResult<T> {
        dec(self.raw(key), &format!("{}.{key}", self.name))
    }

    pub fn str(&self, key: &str) -> DResult<String> {
        self.get(key, dec_str)
    }

    pub fn bool(&self, key: &str) -> DResult<bool> {
        self.get(key, dec_bool)
    }

    pub fn number(&self, key: &str) -> DResult<Number> {
        self.get(key, dec_number)
    }

    pub fn float(&self, key: &str) -> DResult<f64> {
        self.get(key, dec_float)
    }

    pub fn int(&self, key: &str) -> DResult<i64> {
        self.get(key, dec_int)
    }

    pub fn list<T>(
        &self,
        key: &str,
        expected: &str,
        item: impl Fn(Option<&Value>, &str) -> DResult<T>,
    ) -> DResult<Vec<T>> {
        self.get(key, |v, p| dec_list(v, p, expected, &item))
    }
}

// ---------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------

/// The literal text of a JSON number, `""` when absent or null.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Number(pub String);

impl Number {
    /// Parse as a float; 0.0 when the text doesn't parse (empty included).
    pub fn float64(&self) -> f64 {
        parse_float_strict(&self.0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`, the whole string.
pub fn is_json_number(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize| {
        let st = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > st
    };
    if b.first() == Some(&b'-') {
        i += 1;
    }
    match b.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            digits(&mut i);
        }
        _ => return false,
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        if !digits(&mut i) {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digits(&mut i) {
            return false;
        }
    }
    i == b.len()
}

/// Parse a float from ASCII input: decimal (with optional sign, fraction and exponent)
/// or `inf` / `infinity` / `nan` in any case. `None` if the syntax is invalid.
pub fn parse_float(s: &str) -> Option<f64> {
    let (neg, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let lower = body.to_ascii_lowercase();
    let v = match lower.as_str() {
        "inf" | "infinity" => f64::INFINITY,
        "nan" => f64::NAN,
        _ => {
            let b = body.as_bytes();
            let mut i = 0;
            let int_start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let mut ndigits = i - int_start;
            if i < b.len() && b[i] == b'.' {
                i += 1;
                let f = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                ndigits += i - f;
            }
            if ndigits == 0 {
                return None;
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                i += 1;
                if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                    i += 1;
                }
                let e = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i == e {
                    return None;
                }
            }
            if i != b.len() {
                return None;
            }
            body.parse::<f64>().ok()?
        }
    };
    Some(if neg { -v } else { v })
}

/// Parse a hexadecimal float `[sign]0x<hex>[.<hex>]p[sign]<dec>`. `None` on syntax
/// error or overflow.
fn from_hex(s: &str) -> Option<f64> {
    let (neg, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let rest = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")).unwrap_or(rest);
    let (mant, exp) = match rest.find(['p', 'P']) {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    let (int_part, frac_part) = mant.split_once('.').unwrap_or((mant, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    let mut exp: i64 = match exp {
        None => 0,
        Some(e) => {
            let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            e.parse::<i64>().unwrap_or(if e.starts_with('-') { i64::MIN / 2 } else { i64::MAX / 2 })
        }
    };
    let mut mantissa: u64 = 0;
    let mut used = 0;
    for (k, c) in int_part.chars().chain(frac_part.chars()).enumerate() {
        let d = c.to_digit(16)? as u64;
        let is_frac = k >= int_part.len();
        if mantissa == 0 && d == 0 {
            if is_frac {
                exp -= 4;
            }
            continue;
        }
        if used < 15 {
            mantissa = mantissa * 16 + d;
            used += 1;
            if is_frac {
                exp -= 4;
            }
        } else if !is_frac {
            exp += 4;
        }
    }
    let mut v = mantissa as f64;
    let mut e = exp;
    while e > 0 && v.is_finite() {
        let step = e.min(1000);
        v *= 2f64.powi(step as i32);
        e -= step;
    }
    while e < 0 && v != 0.0 {
        let step = (-e).min(1000);
        v /= 2f64.powi(step as i32);
        e += step;
    }
    if v.is_infinite() {
        return None;
    }
    Some(if neg { -v } else { v })
}

/// Lenient float parse: 0 on syntax error, +-Inf on overflow, hex floats accepted.
pub fn parse_float_strict(s: &str) -> f64 {
    if s.is_empty() || s != strip_ws(s) || s.contains('_') {
        return 0.0;
    }
    if let Some(v) = parse_float(s) {
        return v;
    }
    let body = s.trim_start_matches(['+', '-']).to_lowercase();
    if body.starts_with("0x") && body.contains('p') {
        return from_hex(s).unwrap_or(0.0);
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_first(s: &str) -> String {
        parse_first(s).unwrap_err().0
    }

    #[test]
    fn parser_errors_report_position() {
        let cases = [
            ("", "EOF"),
            ("  ", "EOF"),
            ("x", "Expecting value: line 1 column 1 (char 0)"),
            ("[", "Expecting value: line 1 column 2 (char 1)"),
            ("[1,", "Expecting value: line 1 column 4 (char 3)"),
            ("[1,]", "Illegal trailing comma before end of array: line 1 column 3 (char 2)"),
            ("[1 2]", "Expecting ',' delimiter: line 1 column 4 (char 3)"),
            ("{", "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"),
            ("{\"a\"", "Expecting ':' delimiter: line 1 column 5 (char 4)"),
            ("{\"a\" 1}", "Expecting ':' delimiter: line 1 column 6 (char 5)"),
            ("{\"a\":}", "Expecting value: line 1 column 6 (char 5)"),
            ("{\"a\":1,}", "Illegal trailing comma before end of object: line 1 column 7 (char 6)"),
            ("{\"a\":1 \"b\":2}", "Expecting ',' delimiter: line 1 column 8 (char 7)"),
            ("{1:2}", "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"),
            ("\"abc", "Unterminated string starting at: line 1 column 1 (char 0)"),
            ("\"a\u{1}b\"", "Invalid control character at: line 1 column 3 (char 2)"),
            ("\"a\\qb\"", "Invalid \\escape: line 1 column 3 (char 2)"),
            ("\"\\u12\"", "Invalid \\uXXXX escape: line 1 column 3 (char 2)"),
            ("\"\\u12zz\"", "Invalid \\uXXXX escape: line 1 column 3 (char 2)"),
            ("-", "Expecting value: line 1 column 1 (char 0)"),
            ("-x", "Expecting value: line 1 column 1 (char 0)"),
            ("NaN", "invalid character 'N' looking for beginning of value"),
            ("-Infinity", "invalid character '-' looking for beginning of value"),
            ("Infinity", "invalid character 'I' looking for beginning of value"),
            ("nul", "Expecting value: line 1 column 1 (char 0)"),
            ("\n\n  [1,\n  ]", "Illegal trailing comma before end of array: line 3 column 5 (char 6)"),
            ("[\"\\x\"]", "Invalid \\escape: line 1 column 3 (char 2)"),
            ("[\"ab\\", "Unterminated string starting at: line 1 column 2 (char 1)"),
            ("{\"a\"\n:\n1,\n\"b\"}", "Expecting ':' delimiter: line 4 column 4 (char 13)"),
        ];
        for (input, want) in cases {
            assert_eq!(err_first(input), want, "input {input:?}");
        }
    }

    #[test]
    fn number_literal_scanning() {
        assert_eq!(parse_first("01").unwrap(), Value::Num("0".into()));
        assert_eq!(parse_first("1.").unwrap(), Value::Num("1".into()));
        assert_eq!(parse_first("1e").unwrap(), Value::Num("1".into()));
        assert_eq!(parse_first("1e+").unwrap(), Value::Num("1".into()));
        assert_eq!(parse_first("-1.5E+3x").unwrap(), Value::Num("-1.5E+3".into()));
        assert_eq!(parse_first("[1]\n  x").unwrap(), Value::Array(vec![Value::Num("1".into())]));
    }

    #[test]
    fn strings_unescape() {
        assert_eq!(parse_first(r#""\"q\u00e9\ud83d\ude00\/""#).unwrap(), Value::Str("\"qé😀/".into()));
        assert_eq!(parse_first(r#""\ud800x""#).unwrap(), Value::Str("\u{fffd}x".into()));
    }

    #[test]
    fn exact_parse_rejects_extra_data() {
        assert_eq!(parse_exact("").unwrap_err().0, "unexpected end of JSON input");
        assert_eq!(parse_exact("[1] x").unwrap_err().0, "Extra data: line 1 column 5 (char 4)");
        assert_eq!(parse_exact("{\"a\":1}{").unwrap_err().0, "Extra data: line 1 column 8 (char 7)");
        assert_eq!(unmarshal(" [\"a\",\"b\"] ", dec_str_list).unwrap(), vec!["a", "b"]);
        assert_eq!(unmarshal("null", dec_str_list).unwrap(), Vec::<String>::new());
        assert_eq!(unmarshal("[\"a\",1]", dec_str_list).unwrap_err().0, "invalid value: expected string, got number 1");
    }

    #[test]
    fn duplicate_and_case_folded_keys() {
        let v = parse_first(r#"{"id":"a","id":null}"#).unwrap();
        let f = Fields::new(Some(&v), "", "T").unwrap().unwrap();
        assert_eq!(f.str("id").unwrap(), "");
        let v = parse_first(r#"{"id":"a","ID":null}"#).unwrap();
        let f = Fields::new(Some(&v), "", "T").unwrap().unwrap();
        assert_eq!(f.str("id").unwrap(), "a");
        let v = parse_first(r#"{"ID":"x","id":null,"Volume":"12.50"}"#).unwrap();
        let f = Fields::new(Some(&v), "", "T").unwrap().unwrap();
        assert_eq!(f.str("id").unwrap(), "x");
        assert_eq!(f.number("volume").unwrap(), Number("12.50".into()));
    }

    #[test]
    fn typed_decoders() {
        let n = |s: &str| Value::Num(s.into());
        let st = |s: &str| Value::Str(s.into());
        assert_eq!(dec_number(Some(&st("1e5")), "").unwrap().0, "1e5");
        assert_eq!(
            dec_number(Some(&st("01")), "").unwrap_err().0,
            r#"json: invalid number literal, trying to unmarshal "01" into Number"#
        );
        assert_eq!(
            dec_number(Some(&st("é")), "").unwrap_err().0,
            r#"json: invalid number literal, trying to unmarshal "\u00e9" into Number"#
        );
        assert_eq!(dec_number(Some(&n("1e400")), "").unwrap().0, "1e400");
        assert_eq!(dec_float(Some(&n("0.53")), "T.price").unwrap(), 0.53);
        assert_eq!(
            dec_float(Some(&n("1e400")), "T.price").unwrap_err().0,
            r#"invalid value for field "T.price": expected number, got number 1e400"#
        );
        assert_eq!(
            dec_float(Some(&st("0.5")), "T.price").unwrap_err().0,
            r#"invalid value for field "T.price": expected number, got string"#
        );
        assert_eq!(dec_int(Some(&n("1700000000")), "").unwrap(), 1_700_000_000);
        assert_eq!(
            dec_int(Some(&n("1.0")), "T.t").unwrap_err().0,
            r#"invalid value for field "T.t": expected integer, got number 1.0"#
        );
        assert!(dec_int(Some(&n("9223372036854775808")), "").is_err());
        assert_eq!(
            dec_str(Some(&Value::Bool(true)), "T.id").unwrap_err().0,
            r#"invalid value for field "T.id": expected string, got bool"#
        );
        assert_eq!(
            dec_bool(Some(&st("true")), "T.a").unwrap_err().0,
            r#"invalid value for field "T.a": expected bool, got string"#
        );
        assert_eq!(dec_str(None, "").unwrap(), "");
        assert_eq!(dec_str(Some(&Value::Null), "").unwrap(), "");
        assert_eq!(
            dec_any(Some(&parse_first(r#"{"a":[1,"x",null,true]}"#).unwrap()), "").unwrap(),
            Some(serde_json::json!({"a": [1.0, "x", null, true]}))
        );
        assert_eq!(describe_type("[][]GammaTag"), "list of list of GammaTag");
    }

    #[test]
    fn json_number_grammar() {
        for ok in ["0", "-0", "12", "1.5", "1e5", "1E-5", "-0.0e+1"] {
            assert!(is_json_number(ok), "{ok}");
        }
        for bad in ["", "01", "1.", ".5", "+1", "1e", "abc", " 1", "1 "] {
            assert!(!is_json_number(bad), "{bad}");
        }
    }

    #[test]
    fn parse_float_strict_syntax() {
        let cases: [(&str, f64); 18] = [
            ("", 0.0),
            ("1", 1.0),
            (" 1", 0.0),
            ("1_0", 0.0),
            ("inf", f64::INFINITY),
            ("-Infinity", f64::NEG_INFINITY),
            ("1e400", f64::INFINITY),
            ("0x1p3", 8.0),
            ("0x1.8p1", 3.0),
            ("-0X10P-2", -4.0),
            ("0x1", 0.0),
            ("1e5", 100000.0),
            (".5", 0.5),
            ("5.", 5.0),
            ("+3", 3.0),
            ("abc", 0.0),
            ("0x1p99999", 0.0),
            ("0x.8p1", 1.0),
        ];
        for (s, want) in cases {
            assert_eq!(parse_float_strict(s), want, "{s:?}");
        }
        assert!(parse_float_strict("nan").is_nan());
        assert_eq!(parse_float_strict("INFINITY"), f64::INFINITY);
        assert_eq!(parse_float_strict("1e-400"), 0.0);
        assert_eq!(parse_float_strict("0xp1"), 0.0);
    }
}
