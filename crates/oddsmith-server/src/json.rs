//! A lenient JSON decoder (accepts NaN, Infinity, lone surrogates and trailing data; errors
//! carry a message and position), plus the compact JSON spelling of the values the server
//! sends back.

use std::fmt::Write as _;

use oddsmith_core::textfmt;

/// Deepest container nesting the decoder accepts; deeper input is a
/// [`DecodeError::Recursion`].
const MAX_DEPTH: usize = 9996;

/// Most digits allowed in an integer literal.
const MAX_INT_DIGITS: usize = 4300;

/// A decoded JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// An integer literal converted exactly to the nearest float; infinite when that overflows.
    Int(f64),
    Float(f64),
    Str(Text),
    Array(Vec<Value>),
    /// Members in document order; on duplicate keys the last one wins.
    Object(Vec<(Text, Value)>),
}

/// A decoded string. JSON `\u` escapes can encode lone surrogates, Rust strings can't: they
/// are replaced by U+FFFD in `text`, and `json` keeps the original as an ASCII-only JSON
/// string literal.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Text {
    pub text: String,
    pub json: Option<String>,
}

impl Text {
    /// Whether the original string holds a lone surrogate.
    pub fn has_surrogate(&self) -> bool {
        self.json.is_some()
    }
}

/// Why decoding failed.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodeError {
    /// Invalid JSON, with this message.
    Value(String),
    /// Nesting deeper than [`MAX_DEPTH`].
    Recursion,
}

impl Value {
    /// The value's JSON type name, as used in error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Bool(_) => "bool",
            Value::Int(_) | Value::Float(_) => "number",
            Value::Str(_) => "string",
            Value::Array(_) => "array",
            Value::Null | Value::Object(_) => "object",
        }
    }

    /// Look up `key` in an object (the last duplicate wins); `None` for other values.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(members) => {
                members.iter().rev().find(|(k, _)| k.json.is_none() && k.text == key).map(|(_, v)| v)
            }
            _ => None,
        }
    }
}

/// Decode the first JSON value in `doc`; anything after it is ignored.
pub fn decode_prefix(doc: &str) -> Result<Value, DecodeError> {
    let chars: Vec<char> = doc.chars().collect();
    let mut p = Parser { s: &chars, depth: 0 };
    p.value(0).map(|(v, _)| v).map_err(|e| match e {
        Fail::At(msg, pos) => DecodeError::Value(p.errmsg(msg, pos)),
        Fail::Stop(pos) => DecodeError::Value(p.errmsg("Expecting value", pos)),
        Fail::Plain(msg) => DecodeError::Value(msg),
        Fail::Recursion => DecodeError::Recursion,
    })
}

enum Fail {
    /// A syntax error at a position.
    At(&'static str, usize),
    /// No value starts here.
    Stop(usize),
    Plain(String),
    Recursion,
}

struct Parser<'a> {
    s: &'a [char],
    depth: usize,
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

impl Parser<'_> {
    fn errmsg(&self, msg: &str, pos: usize) -> String {
        let before = &self.s[..pos.min(self.s.len())];
        let lineno = before.iter().filter(|&&c| c == '\n').count() + 1;
        let colno = match before.iter().rposition(|&c| c == '\n') {
            Some(nl) => pos - nl,
            None => pos + 1,
        };
        format!("{msg}: line {lineno} column {colno} (char {pos})")
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

    fn matches(&self, i: usize, word: &str) -> bool {
        let n = word.chars().count();
        i + n <= self.s.len() && self.s[i..i + n].iter().copied().eq(word.chars())
    }

    fn value(&mut self, i: usize) -> Result<(Value, usize), Fail> {
        let Some(c) = self.at(i) else { return Err(Fail::Stop(i)) };
        match c {
            '"' => return self.string(i + 1).map(|(t, e)| (Value::Str(t), e)),
            '{' | '[' => {
                if self.depth >= MAX_DEPTH {
                    return Err(Fail::Recursion);
                }
                self.depth += 1;
                let r = if c == '{' { self.object(i + 1) } else { self.array(i + 1) };
                self.depth -= 1;
                return r;
            }
            'n' if self.matches(i, "null") => return Ok((Value::Null, i + 4)),
            't' if self.matches(i, "true") => return Ok((Value::Bool(true), i + 4)),
            'f' if self.matches(i, "false") => return Ok((Value::Bool(false), i + 5)),
            'N' if self.matches(i, "NaN") => return Ok((Value::Float(f64::NAN), i + 3)),
            'I' if self.matches(i, "Infinity") => return Ok((Value::Float(f64::INFINITY), i + 8)),
            '-' if self.matches(i, "-Infinity") => return Ok((Value::Float(f64::NEG_INFINITY), i + 9)),
            _ => {}
        }
        self.number(i)
    }

    fn object(&mut self, start: usize) -> Result<(Value, usize), Fail> {
        let mut members = Vec::new();
        let mut i = self.skip_ws(start);
        if self.at(i) == Some('}') {
            return Ok((Value::Object(members), i + 1));
        }
        loop {
            if self.at(i) != Some('"') {
                return Err(Fail::At("Expecting property name enclosed in double quotes", i));
            }
            let (key, next) = self.string(i + 1)?;
            i = self.skip_ws(next);
            if self.at(i) != Some(':') {
                return Err(Fail::At("Expecting ':' delimiter", i));
            }
            i = self.skip_ws(i + 1);
            let (val, next) = self.value(i)?;
            members.push((key, val));
            i = self.skip_ws(next);
            match self.at(i) {
                Some('}') => return Ok((Value::Object(members), i + 1)),
                Some(',') => {}
                _ => return Err(Fail::At("Expecting ',' delimiter", i)),
            }
            let comma = i;
            i = self.skip_ws(i + 1);
            if self.at(i) == Some('}') {
                return Err(Fail::At("Illegal trailing comma before end of object", comma));
            }
        }
    }

    fn array(&mut self, start: usize) -> Result<(Value, usize), Fail> {
        let mut items = Vec::new();
        let mut i = self.skip_ws(start);
        if self.at(i) == Some(']') {
            return Ok((Value::Array(items), i + 1));
        }
        loop {
            let (val, next) = self.value(i)?;
            items.push(val);
            i = self.skip_ws(next);
            match self.at(i) {
                Some(']') => return Ok((Value::Array(items), i + 1)),
                Some(',') => {}
                _ => return Err(Fail::At("Expecting ',' delimiter", i)),
            }
            let comma = i;
            i = self.skip_ws(i + 1);
            if self.at(i) == Some(']') {
                return Err(Fail::At("Illegal trailing comma before end of array", comma));
            }
        }
    }

    fn number(&self, start: usize) -> Result<(Value, usize), Fail> {
        let digit = |i: usize| self.at(i).is_some_and(|c| c.is_ascii_digit());
        let len = self.s.len();
        let mut i = start;
        if self.at(i) == Some('-') {
            i += 1;
        }
        match self.at(i) {
            Some('1'..='9') => {
                i += 1;
                while digit(i) {
                    i += 1;
                }
            }
            Some('0') => i += 1,
            _ => return Err(Fail::Stop(start)),
        }
        let mut is_float = false;
        if i + 1 < len && self.s[i] == '.' && digit(i + 1) {
            is_float = true;
            i += 2;
            while digit(i) {
                i += 1;
            }
        }
        if i + 1 < len && matches!(self.s[i], 'e' | 'E') {
            let e_start = i;
            i += 1;
            if i + 1 < len && matches!(self.s[i], '-' | '+') {
                i += 1;
            }
            while digit(i) {
                i += 1;
            }
            if digit(i - 1) {
                is_float = true;
            } else {
                i = e_start;
            }
        }
        let text: String = self.s[start..i].iter().collect();
        if is_float {
            return Ok((Value::Float(text.parse().unwrap_or(f64::NAN)), i));
        }
        let digits = text.trim_start_matches('-').len();
        if digits > MAX_INT_DIGITS {
            return Err(Fail::Plain(format!("integer literal too long: {digits} digits (max {MAX_INT_DIGITS})")));
        }
        // Correctly rounded, and an integer zero has no sign.
        let v: f64 = text.parse().unwrap_or(f64::NAN);
        Ok((Value::Int(if v == 0.0 { 0.0 } else { v }), i))
    }

    fn hex4(&self, from: usize) -> Option<u32> {
        let mut v = 0;
        for k in 0..4 {
            v = (v << 4) | self.at(from + k)?.to_digit(16)?;
        }
        Some(v)
    }

    /// Parse a string; `end` is just past the opening quote.
    fn string(&self, end: usize) -> Result<(Text, usize), Fail> {
        let len = self.s.len();
        let begin = end - 1;
        let mut units: Vec<u32> = Vec::new();
        let mut end = end;
        loop {
            let mut next = end;
            let mut c = '\0';
            while next < len {
                c = self.s[next];
                if c == '"' || c == '\\' {
                    break;
                }
                if (c as u32) <= 0x1f {
                    return Err(Fail::At("Invalid control character at", next));
                }
                next += 1;
            }
            if next >= len || (c != '"' && c != '\\') {
                return Err(Fail::At("Unterminated string starting at", begin));
            }
            units.extend(self.s[end..next].iter().map(|&ch| ch as u32));
            next += 1;
            if c == '"' {
                end = next;
                break;
            }
            if next == len {
                return Err(Fail::At("Unterminated string starting at", begin));
            }
            let esc = self.s[next];
            if esc != 'u' {
                end = next + 1;
                let ch = match esc {
                    '"' => '"',
                    '\\' => '\\',
                    '/' => '/',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return Err(Fail::At("Invalid \\escape", end - 2)),
                };
                units.push(ch as u32);
                continue;
            }
            next += 1;
            end = next + 4;
            if end >= len {
                return Err(Fail::At("Invalid \\uXXXX escape", next - 1));
            }
            let Some(mut u) = self.hex4(next) else { return Err(Fail::At("Invalid \\uXXXX escape", end - 5)) };
            if (0xd800..0xdc00).contains(&u) && end + 6 < len && self.s[end] == '\\' && self.s[end + 1] == 'u' {
                let Some(u2) = self.hex4(end + 2) else {
                    return Err(Fail::At("Invalid \\uXXXX escape", end + 1));
                };
                if (0xdc00..0xe000).contains(&u2) {
                    u = 0x10000 + ((u - 0xd800) << 10) + (u2 - 0xdc00);
                    end += 6;
                }
            }
            units.push(u);
        }
        Ok((text_from_units(&units), end))
    }
}

fn text_from_units(units: &[u32]) -> Text {
    let has_surrogate = units.iter().any(|u| (0xd800..0xe000).contains(u));
    let text = units.iter().map(|&u| char::from_u32(u).unwrap_or('\u{fffd}')).collect();
    if !has_surrogate {
        return Text { text, json: None };
    }
    let mut json = String::from("\"");
    for &u in units {
        match char::from_u32(u) {
            Some(c) => {
                let q = textfmt::json_str(c.encode_utf8(&mut [0; 4]));
                json.push_str(&q[1..q.len() - 1]);
            }
            None => {
                let _ = write!(json, "\\u{u:04x}");
            }
        }
    }
    json.push('"');
    Text { text, json: Some(json) }
}

/// A float as JSON in its shortest round-trip form; `None` when not finite.
pub fn float(x: f64) -> Option<String> {
    x.is_finite().then(|| textfmt::shortest(x))
}

/// A JSON string literal that keeps non-ASCII characters, used for quoting strings in log lines.
pub fn quote_unicode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(doc: &str) -> String {
        match decode_prefix(doc) {
            Err(DecodeError::Value(m)) => m,
            other => panic!("{doc:?}: {other:?}"),
        }
    }

    #[test]
    fn error_messages_report_position() {
        let cases = [
            ("{", "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"),
            ("{\"a\"", "Expecting ':' delimiter: line 1 column 5 (char 4)"),
            ("{\"a\":", "Expecting value: line 1 column 6 (char 5)"),
            ("{\"a\":1", "Expecting ',' delimiter: line 1 column 7 (char 6)"),
            ("{\"a\":1,}", "Illegal trailing comma before end of object: line 1 column 7 (char 6)"),
            ("[1,]", "Illegal trailing comma before end of array: line 1 column 3 (char 2)"),
            ("[", "Expecting value: line 1 column 2 (char 1)"),
            ("\"abc", "Unterminated string starting at: line 1 column 1 (char 0)"),
            ("\"a\\x\"", "Invalid \\escape: line 1 column 3 (char 2)"),
            ("\"\\u1234", "Invalid \\uXXXX escape: line 1 column 3 (char 2)"),
            ("\"\\ud800\\udc0\"", "Invalid \\uXXXX escape: line 1 column 9 (char 8)"),
            ("\"\\ud800\\uZZZZ\"", "Invalid \\uXXXX escape: line 1 column 9 (char 8)"),
            ("\"\u{1}\"", "Invalid control character at: line 1 column 2 (char 1)"),
            ("-Inf", "Expecting value: line 1 column 1 (char 0)"),
            ("{\n \"a\" 1}", "Expecting ':' delimiter: line 2 column 6 (char 7)"),
        ];
        for (doc, want) in cases {
            assert_eq!(err(doc), want, "{doc:?}");
        }
        assert_eq!(err(&"1".repeat(4301)), "integer literal too long: 4301 digits (max 4300)");
    }

    #[test]
    fn accepts_nan_infinity_and_lone_surrogates() {
        assert_eq!(decode_prefix("1.e5").unwrap(), Value::Int(1.0));
        assert_eq!(decode_prefix("1E+5x").unwrap(), Value::Float(1e5));
        assert_eq!(decode_prefix("-0").unwrap(), Value::Int(0.0));
        assert!(matches!(decode_prefix("NaN").unwrap(), Value::Float(x) if x.is_nan()));
        assert_eq!(decode_prefix("-Infinity").unwrap(), Value::Float(f64::NEG_INFINITY));
        assert_eq!(decode_prefix(&format!("1{}", "0".repeat(400))).unwrap(), Value::Int(f64::INFINITY));
        let Value::Str(t) = decode_prefix("\"\\ud800\\u0041\"").unwrap() else { panic!() };
        assert_eq!(t.text, "\u{fffd}A");
        assert_eq!(t.json.as_deref(), Some("\"\\ud800A\""));
        let Value::Str(t) = decode_prefix("\"\\ud83d\\ude00\"").unwrap() else { panic!() };
        assert_eq!((t.text.as_str(), t.json), ("\u{1f600}", None));
        let v = decode_prefix("{\"a\":1,\"a\":2}").unwrap();
        assert_eq!(v.get("a"), Some(&Value::Int(2.0)));
    }

    #[test]
    fn deep_nesting_is_a_recursion_error() {
        // Connection threads get a large stack for this; so does the test.
        std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(|| {
                let ok = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
                assert!(decode_prefix(&ok).is_ok());
                let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
                assert_eq!(decode_prefix(&deep), Err(DecodeError::Recursion));
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
