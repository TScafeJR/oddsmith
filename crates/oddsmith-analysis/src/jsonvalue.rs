//! A JSON reader that keeps number literals as text and objects in key order with
//! duplicates; syntax errors give a message and position
//! ("Expecting value: line 1 column 1 (char 0)").

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// A number literal, kept as its source text.
    Num(String),
    /// `NaN`, `Infinity` or `-Infinity` (only when constants are allowed).
    Const(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// Whether the literal is an integer (no fraction or exponent).
    pub fn is_int_literal(s: &str) -> bool {
        !s.contains(['.', 'e', 'E'])
    }
}

/// Parse the first JSON value starting at `start` and return it with the end offset (in
/// chars). `allow_constants` accepts `NaN` / `Infinity` / `-Infinity`; when false they
/// are rejected with the error Go-style decoders give.
pub fn decode_prefix(text: &str, start: usize, allow_constants: bool) -> Result<(Value, usize), String> {
    let chars: Vec<char> = text.chars().collect();
    let mut p = Parser { s: &chars, allow_constants };
    let (v, end) = p.value(start)?;
    Ok((v, end))
}

/// Parse exactly one value surrounded by optional whitespace.
pub fn parse(text: &str) -> Result<Value, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut p = Parser { s: &chars, allow_constants: true };
    let start = p.skip_ws(0);
    let (v, end) = p.value(start)?;
    let end = p.skip_ws(end);
    if end != chars.len() {
        return Err(p.err("Extra data", end));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [char],
    allow_constants: bool,
}

impl Parser<'_> {
    fn err(&self, msg: &str, pos: usize) -> String {
        let before = &self.s[..pos.min(self.s.len())];
        let lineno = before.iter().filter(|c| **c == '\n').count() + 1;
        let colno = match before.iter().rposition(|c| *c == '\n') {
            Some(nl) => pos - nl,
            None => pos + 1,
        };
        format!("{msg}: line {lineno} column {colno} (char {pos})")
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while i < self.s.len() && matches!(self.s[i], ' ' | '\t' | '\n' | '\r') {
            i += 1;
        }
        i
    }

    fn starts_with(&self, i: usize, word: &str) -> bool {
        let w: Vec<char> = word.chars().collect();
        self.s.len() >= i + w.len() && self.s[i..i + w.len()] == w[..]
    }

    fn constant(&self, name: &str, v: f64, i: usize) -> Result<(Value, usize), String> {
        if !self.allow_constants {
            let first = name.chars().next().unwrap_or('N');
            return Err(format!("invalid character '{first}' looking for beginning of value"));
        }
        Ok((Value::Const(v), i + name.chars().count()))
    }

    fn value(&mut self, i: usize) -> Result<(Value, usize), String> {
        let Some(&c) = self.s.get(i) else {
            return Err(self.err("Expecting value", i));
        };
        match c {
            '"' => {
                let (s, end) = self.string(i + 1)?;
                Ok((Value::Str(s), end))
            }
            '{' => self.object(i + 1),
            '[' => self.array(i + 1),
            'n' if self.starts_with(i, "null") => Ok((Value::Null, i + 4)),
            't' if self.starts_with(i, "true") => Ok((Value::Bool(true), i + 4)),
            'f' if self.starts_with(i, "false") => Ok((Value::Bool(false), i + 5)),
            'N' if self.starts_with(i, "NaN") => self.constant("NaN", f64::NAN, i),
            'I' if self.starts_with(i, "Infinity") => self.constant("Infinity", f64::INFINITY, i),
            '-' if self.starts_with(i, "-Infinity") => self.constant("-Infinity", f64::NEG_INFINITY, i),
            _ => match self.number(i) {
                Some(end) => Ok((Value::Num(self.s[i..end].iter().collect()), end)),
                None => Err(self.err("Expecting value", i)),
            },
        }
    }

    /// Match `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?` at `i`.
    fn number(&self, i: usize) -> Option<usize> {
        let s = self.s;
        let digit = |j: usize| s.get(j).is_some_and(char::is_ascii_digit);
        let mut j = i;
        if s.get(j) == Some(&'-') {
            j += 1;
        }
        match s.get(j) {
            Some('0') => j += 1,
            Some(c) if ('1'..='9').contains(c) => {
                while digit(j) {
                    j += 1;
                }
            }
            _ => return None,
        }
        if s.get(j) == Some(&'.') && digit(j + 1) {
            j += 1;
            while digit(j) {
                j += 1;
            }
        }
        if matches!(s.get(j), Some('e' | 'E')) {
            let mut k = j + 1;
            if matches!(s.get(k), Some('+' | '-')) {
                k += 1;
            }
            if digit(k) {
                while digit(k) {
                    k += 1;
                }
                j = k;
            }
        }
        Some(j)
    }

    /// Scan a string body starting after the opening quote at `begin - 1`.
    fn string(&self, begin: usize) -> Result<(String, usize), String> {
        let s = self.s;
        let mut out = String::new();
        let mut i = begin;
        loop {
            let Some(&c) = s.get(i) else {
                return Err(self.err("Unterminated string starting at", begin - 1));
            };
            match c {
                '"' => return Ok((out, i + 1)),
                '\\' => {
                    let Some(&e) = s.get(i + 1) else {
                        return Err(self.err("Unterminated string starting at", begin - 1));
                    };
                    i += 2;
                    match e {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let hi = self.hex4(i).ok_or_else(|| self.err("Invalid \\uXXXX escape", i - 1))?;
                            i += 4;
                            let mut cp = hi;
                            if (0xd800..0xdc00).contains(&hi)
                                && s.get(i) == Some(&'\\')
                                && s.get(i + 1) == Some(&'u')
                                && let Some(lo) = self.hex4(i + 2)
                                && (0xdc00..0xe000).contains(&lo)
                            {
                                cp = 0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00);
                                i += 6;
                            }
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(self.err("Invalid \\escape", i - 2)),
                    }
                }
                c if (c as u32) < 0x20 => {
                    return Err(self.err("Invalid control character at", i));
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
    }

    fn hex4(&self, i: usize) -> Option<u32> {
        let h: String = self.s.get(i..i + 4)?.iter().collect();
        if h.chars().all(|c| c.is_ascii_hexdigit()) { u32::from_str_radix(&h, 16).ok() } else { None }
    }

    fn object(&mut self, i: usize) -> Result<(Value, usize), String> {
        let mut pairs = Vec::new();
        let mut i = self.skip_ws(i);
        if self.s.get(i) == Some(&'}') {
            return Ok((Value::Obj(pairs), i + 1));
        }
        loop {
            if self.s.get(i) != Some(&'"') {
                return Err(self.err("Expecting property name enclosed in double quotes", i));
            }
            let (key, end) = self.string(i + 1)?;
            i = self.skip_ws(end);
            if self.s.get(i) != Some(&':') {
                return Err(self.err("Expecting ':' delimiter", i));
            }
            i = self.skip_ws(i + 1);
            let (v, end) = self.value(i)?;
            pairs.push((key, v));
            i = self.skip_ws(end);
            match self.s.get(i) {
                Some('}') => return Ok((Value::Obj(pairs), i + 1)),
                Some(',') => {
                    let comma = i;
                    i = self.skip_ws(i + 1);
                    if self.s.get(i) == Some(&'}') {
                        return Err(self.err("Illegal trailing comma before end of object", comma));
                    }
                }
                _ => return Err(self.err("Expecting ',' delimiter", i)),
            }
        }
    }

    fn array(&mut self, i: usize) -> Result<(Value, usize), String> {
        let mut items = Vec::new();
        let mut i = self.skip_ws(i);
        if self.s.get(i) == Some(&']') {
            return Ok((Value::Arr(items), i + 1));
        }
        loop {
            let (v, end) = self.value(i)?;
            items.push(v);
            i = self.skip_ws(end);
            match self.s.get(i) {
                Some(']') => return Ok((Value::Arr(items), i + 1)),
                Some(',') => {
                    let comma = i;
                    i = self.skip_ws(i + 1);
                    if self.s.get(i) == Some(&']') {
                        return Err(self.err("Illegal trailing comma before end of array", comma));
                    }
                }
                _ => return Err(self.err("Expecting ',' delimiter", i)),
            }
        }
    }
}

/// Parse a float: surrounding whitespace is ignored, underscores are allowed between
/// digits, and `inf` / `infinity` / `nan` are accepted in any case.
pub fn parse_float(s: &str) -> Option<f64> {
    let t = s.trim_matches(|c: char| c.is_whitespace());
    if t.is_empty() {
        return None;
    }
    let cleaned = if t.contains('_') {
        let cs: Vec<char> = t.chars().collect();
        for (k, c) in cs.iter().enumerate() {
            if *c == '_' {
                let ok = k > 0 && cs[k - 1].is_ascii_digit() && cs.get(k + 1).is_some_and(char::is_ascii_digit);
                if !ok {
                    return None;
                }
            }
        }
        t.replace('_', "")
    } else {
        t.to_string()
    };
    let lower = cleaned.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    if matches!(body, "inf" | "infinity" | "nan") {
        // Only one sign is allowed.
        if lower.len() - body.len() > 1 {
            return None;
        }
    } else if !body.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// Display text for a decoded value in JSON style: `null`, `true` / `false`, numbers via
/// [`num_str`], `NaN` / `Infinity` / `-Infinity`, double-quoted strings, `[a, b]` lists and
/// `{"k": v}` objects.
pub fn value_text(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Num(lit) => num_str(lit),
        Value::Const(f) if f.is_nan() => "NaN".into(),
        Value::Const(f) => if *f > 0.0 { "Infinity" } else { "-Infinity" }.into(),
        Value::Str(s) => quote_str(s),
        Value::Arr(items) => format!("[{}]", items.iter().map(value_text).collect::<Vec<_>>().join(", ")),
        Value::Obj(pairs) => {
            // A duplicated key keeps its last value at the first key's position.
            let mut merged: Vec<(&String, &Value)> = Vec::new();
            for (k, v) in pairs {
                match merged.iter_mut().find(|(mk, _)| *mk == k) {
                    Some(slot) => slot.1 = v,
                    None => merged.push((k, v)),
                }
            }
            let inner: Vec<String> =
                merged.iter().map(|(k, v)| format!("{}: {}", quote_str(k), value_text(v))).collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Display text for a JSON number: integers without leading zeros (`-0` is `0`), others in
/// the shortest round-trip float form.
pub fn num_str(lit: &str) -> String {
    if Value::is_int_literal(lit) {
        let neg = lit.starts_with('-');
        let digits = lit.trim_start_matches('-').trim_start_matches('0');
        if digits.is_empty() {
            return "0".into();
        }
        return if neg { format!("-{digits}") } else { digits.to_string() };
    }
    crate::textfmt::shortest(lit.parse::<f64>().unwrap_or(f64::NAN))
}

/// A JSON string literal (non-ASCII kept as is).
fn quote_str(s: &str) -> String {
    crate::json::serialize(&crate::json::Json::str(s), crate::json::JsonStyle::COMPACT).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_values_and_keeps_literals() {
        let v = parse(r#" {"a": [1, 2.50, -0, 1e400], "b": "x\u00e9\ud83d\ude00", "a": null} "#).unwrap();
        let Value::Obj(pairs) = v else { panic!() };
        assert_eq!(pairs.len(), 3);
        assert_eq!(
            pairs[0].1,
            Value::Arr(vec![
                Value::Num("1".into()),
                Value::Num("2.50".into()),
                Value::Num("-0".into()),
                Value::Num("1e400".into())
            ])
        );
        assert_eq!(pairs[1].1, Value::Str("xé\u{1F600}".into()));
    }

    #[test]
    fn syntax_errors_report_position() {
        assert_eq!(parse("").unwrap_err(), "Expecting value: line 1 column 1 (char 0)");
        assert_eq!(parse("[1,]").unwrap_err(), "Illegal trailing comma before end of array: line 1 column 3 (char 2)");
        assert_eq!(parse("[1 2]").unwrap_err(), "Expecting ',' delimiter: line 1 column 4 (char 3)");
        assert_eq!(
            parse("{\n1}").unwrap_err(),
            "Expecting property name enclosed in double quotes: line 2 column 1 (char 2)"
        );
        assert_eq!(parse("[1] x").unwrap_err(), "Extra data: line 1 column 5 (char 4)");
        assert_eq!(parse("\"abc").unwrap_err(), "Unterminated string starting at: line 1 column 1 (char 0)");
        assert_eq!(parse("\"a\nb\"").unwrap_err(), "Invalid control character at: line 1 column 3 (char 2)");
        assert_eq!(parse("\"a\\qb\"").unwrap_err(), "Invalid \\escape: line 1 column 3 (char 2)");
        assert_eq!(parse("\"\\u12\"").unwrap_err(), "Invalid \\uXXXX escape: line 1 column 3 (char 2)");
        assert_eq!(
            parse("{\"a\":1,}").unwrap_err(),
            "Illegal trailing comma before end of object: line 1 column 7 (char 6)"
        );
        let (v, end) = decode_prefix("[1] trailing", 0, false).unwrap();
        assert_eq!((v, end), (Value::Arr(vec![Value::Num("1".into())]), 3));
        assert!(decode_prefix("NaN", 0, false).is_err());
    }

    #[test]
    fn parse_float_and_value_text() {
        assert_eq!(parse_float(" 1_000.5 "), Some(1000.5));
        assert_eq!(parse_float("1__0"), None);
        assert_eq!(parse_float("-inf"), Some(f64::NEG_INFINITY));
        assert_eq!(parse_float("abc"), None);
        assert_eq!(parse_float("0x10"), None);
        assert_eq!(parse_float(".5"), Some(0.5));
        assert_eq!(num_str("-0"), "0");
        assert_eq!(num_str("2.50"), "2.5");
        assert_eq!(value_text(&parse(r#"["a", {"k": true}, null]"#).unwrap()), r#"["a", {"k": true}, null]"#);
    }
}
