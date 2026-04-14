//! A JSON value and a serializer with a fixed output format: shortest round-trip floats
//! (`1.0`, `1e-05`), configurable indentation, separators, key sorting and ASCII-only
//! escaping, and an error for NaN or infinite floats.

use crate::textfmt;

/// A JSON value. Objects keep insertion order.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Build an object from `(key, value)` pairs.
    pub fn obj<K: Into<String>>(pairs: impl IntoIterator<Item = (K, Json)>) -> Json {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }
}

/// Serializer options.
#[derive(Debug, Clone, Copy)]
pub struct JsonStyle {
    /// Spaces per indentation level, one item per line; `None` writes everything on one line.
    pub indent: Option<usize>,
    /// Separate items with `,` and keys from values with `:` instead of `, ` and `: `.
    pub compact: bool,
    pub sort_keys: bool,
    /// Escape every non-ASCII character as `\uXXXX`.
    pub ascii_only: bool,
}

impl JsonStyle {
    /// Two-space indent, sorted keys.
    pub const SORTED_2: JsonStyle = JsonStyle { indent: Some(2), compact: false, sort_keys: true, ascii_only: false };
    /// Two-space indent.
    pub const INDENT_2: JsonStyle = JsonStyle { indent: Some(2), compact: false, sort_keys: false, ascii_only: false };
    /// One line, no spaces after separators.
    pub const COMPACT: JsonStyle = JsonStyle { indent: None, compact: true, sort_keys: false, ascii_only: false };
}

/// Serialize `v`. NaN or an infinity is an error
/// (`Out of range float values are not JSON compliant: nan`).
pub fn serialize(v: &Json, opts: JsonStyle) -> Result<String, String> {
    let mut out = String::new();
    write_value(&mut out, v, opts, 0)?;
    Ok(out)
}

fn write_value(out: &mut String, v: &Json, opts: JsonStyle, level: usize) -> Result<(), String> {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(i) => out.push_str(&i.to_string()),
        Json::Float(f) => {
            if !f.is_finite() {
                return Err(format!("Out of range float values are not JSON compliant: {}", textfmt::shortest(*f)));
            }
            out.push_str(&textfmt::shortest(*f));
        }
        Json::Str(s) => write_str(out, s, opts.ascii_only),
        Json::Arr(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return Ok(());
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    item_sep(out, opts);
                }
                newline(out, opts, level + 1);
                write_value(out, item, opts, level + 1)?;
            }
            newline(out, opts, level);
            out.push(']');
        }
        Json::Obj(pairs) => {
            if pairs.is_empty() {
                out.push_str("{}");
                return Ok(());
            }
            let mut refs: Vec<&(String, Json)> = pairs.iter().collect();
            if opts.sort_keys {
                refs.sort_by(|a, b| a.0.cmp(&b.0));
            }
            out.push('{');
            for (i, (k, val)) in refs.into_iter().enumerate() {
                if i > 0 {
                    item_sep(out, opts);
                }
                newline(out, opts, level + 1);
                write_str(out, k, opts.ascii_only);
                out.push_str(if opts.compact { ":" } else { ": " });
                write_value(out, val, opts, level + 1)?;
            }
            newline(out, opts, level);
            out.push('}');
        }
    }
    Ok(())
}

fn item_sep(out: &mut String, opts: JsonStyle) {
    // With an indent the item separator has no trailing space.
    if opts.compact || opts.indent.is_some() {
        out.push(',');
    } else {
        out.push_str(", ");
    }
}

fn newline(out: &mut String, opts: JsonStyle, level: usize) {
    if let Some(n) = opts.indent {
        out.push('\n');
        out.push_str(&" ".repeat(n * level));
    }
}

fn write_str(out: &mut String, s: &str, ascii_only: bool) {
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
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if ascii_only && (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Compact JSON with `<`, `>`, `&`, U+2028 and U+2029 escaped so the text
/// is safe to embed in an HTML `<script>`.
pub fn compact_html_json(v: &Json) -> Result<String, String> {
    let s = serialize(v, JsonStyle::COMPACT)?;
    Ok(s.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Json {
        Json::obj([
            ("b", Json::Arr(vec![])),
            ("a", Json::Arr(vec![Json::Int(1), Json::Obj(vec![])])),
            ("c", Json::str("x\u{2028}é\u{7f}\u{1}\"\\")),
        ])
    }

    #[test]
    fn indent_and_sorted_keys() {
        let want = "{\n  \"a\": [\n    1,\n    {}\n  ],\n  \"b\": [],\n  \"c\": \"x\u{2028}é\u{7f}\\u0001\\\"\\\\\"\n}";
        assert_eq!(serialize(&sample(), JsonStyle::SORTED_2).unwrap(), want);
    }

    #[test]
    fn ascii_only_escapes_non_ascii() {
        let v = Json::str("x\u{2028}é\u{7f}\u{1}\u{1F600}");
        let opts = JsonStyle { ascii_only: true, ..JsonStyle::SORTED_2 };
        assert_eq!(serialize(&v, opts).unwrap(), "\"x\\u2028\\u00e9\\u007f\\u0001\\ud83d\\ude00\"");
    }

    #[test]
    fn compact_and_nan() {
        let v = Json::obj([("p", Json::Float(1.0)), ("s", Json::str("<a&b>"))]);
        assert_eq!(compact_html_json(&v).unwrap(), "{\"p\":1.0,\"s\":\"\\u003ca\\u0026b\\u003e\"}");
        assert_eq!(
            serialize(&Json::Float(f64::NAN), JsonStyle::COMPACT).unwrap_err(),
            "Out of range float values are not JSON compliant: nan"
        );
        let plain = JsonStyle { indent: None, compact: false, sort_keys: false, ascii_only: false };
        assert_eq!(serialize(&Json::Arr(vec![Json::Int(1), Json::Null]), plain).unwrap(), "[1, null]");
    }
}
