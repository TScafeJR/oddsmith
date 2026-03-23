//! Number and string formatting with a fixed, stable spelling, so reports, CSV files and
//! CLI tables are byte-for-byte reproducible.
//!
//! Rust's fixed-precision float formatting already rounds the exact binary value half to
//! even; these helpers add the `nan` / `inf` spelling of special values, the shortest
//! round-trip float form, `%g`-style general formatting, rounding, float parsing and
//! string quoting.

use std::fmt::Write as _;

/// Fixed-point with `prec` decimals; NaN and infinities print as `nan`, `inf`, `-inf`.
pub fn fixed(x: f64, prec: usize) -> String {
    if x.is_nan() {
        "nan".into()
    } else if x.is_infinite() {
        if x > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        format!("{x:.prec$}")
    }
}

/// Like [`fixed`], always with a sign.
pub fn signed(x: f64, prec: usize) -> String {
    if x.is_nan() {
        "+nan".into()
    } else if x.is_infinite() {
        if x > 0.0 { "+inf".into() } else { "-inf".into() }
    } else {
        format!("{x:+.prec$}")
    }
}

/// No decimals, thousands separated by commas.
pub fn grouped0(x: f64) -> String {
    let s = fixed(x, 0);
    let (sign, digits) = match s.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", s.as_str()),
    };
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return s;
    }
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    out.push_str(sign);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Split Rust's `{:e}` output ("1.25e-3") into mantissa and exponent.
fn split_exp(s: &str) -> (&str, i32) {
    let (m, e) = s.split_once('e').expect("exponent form");
    (m, e.parse().expect("exponent"))
}

/// Exponent with an explicit sign and at least two digits (`e+05`, `e-07`).
fn exp_suffix(e: i32) -> String {
    let sign = if e < 0 { '-' } else { '+' };
    format!("{sign}{:02}", e.abs())
}

fn strip_zeros(s: &str) -> &str {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { s }
}

/// `%g`-style general format with `prec` significant digits: fixed notation when the
/// exponent is in `-4..prec`, scientific otherwise, trailing zeros removed.
pub fn g(x: f64, prec: usize) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let p = prec.max(1);
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{x:.*e}", p - 1);
    let (mant, exp) = split_exp(&sci);
    if exp >= -4 && exp < p as i32 {
        let decimals = (p as i32 - 1 - exp) as usize;
        strip_zeros(&format!("{x:.decimals$}")).to_string()
    } else {
        format!("{}e{}", strip_zeros(mant), exp_suffix(exp))
    }
}

/// The shortest representation that round-trips, `1.0` not `1`, scientific notation
/// outside `1e-4 <= |x| < 1e16`.
pub fn shortest(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{x:e}");
    let (mant, exp) = split_exp(&sci);
    if (-4..16).contains(&exp) {
        let s = format!("{x}");
        if s.contains('.') { s } else { s + ".0" }
    } else {
        format!("{mant}e{}", exp_suffix(exp))
    }
}

/// Round to `ndigits` decimals (correctly rounded, half to even).
pub fn round(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.ndigits$}").parse().unwrap_or(x)
}

/// Parse a float, accepting surrounding whitespace, `_` between digits, `inf`, `nan`.
pub fn parse_float(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let unsigned = lower.trim_start_matches(['+', '-']);
    if lower.len() - unsigned.len() > 1 {
        return None;
    }
    let negative = lower.starts_with('-');
    match unsigned {
        "inf" | "infinity" => return Some(if negative { f64::NEG_INFINITY } else { f64::INFINITY }),
        "nan" => return Some(f64::NAN),
        _ => {}
    }
    if !unsigned.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'+' | b'-' | b'_')) {
        return None;
    }
    let cleaned = if t.contains('_') {
        let b = t.as_bytes();
        for (i, &c) in b.iter().enumerate() {
            if c == b'_' && !(i > 0 && b[i - 1].is_ascii_digit() && i + 1 < b.len() && b[i + 1].is_ascii_digit()) {
                return None;
            }
        }
        t.replace('_', "")
    } else {
        t.to_string()
    };
    cleaned.parse().ok()
}

/// A JSON string literal with every non-ASCII character escaped as `\uXXXX`.
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Quote a string: single quotes unless the text holds `'` but no `"`, with backslash
/// escapes for the quote, backslash and control characters.
pub fn quote_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Escape `&`, `<`, `>`, `"` and `'` for HTML.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

/// One CSV field, quoted only when it holds a comma, quote or line break.
pub fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

/// A CSV row ending in `\n`; a single empty field is written as `""`.
pub fn csv_row<S: AsRef<str>>(fields: &[S]) -> String {
    let mut line = fields.iter().map(|f| csv_field(f.as_ref())).collect::<Vec<_>>().join(",");
    if fields.len() == 1 && fields[0].as_ref().is_empty() {
        line = "\"\"".into();
    }
    line.push('\n');
    line
}

/// Join a directory and a relative path with `/` unless `a` is empty or already ends in one.
pub fn path_join(a: &str, b: &str) -> String {
    if a.is_empty() || a.ends_with('/') { format!("{a}{b}") } else { format!("{a}/{b}") }
}

/// Exactly rounded sum (Shewchuk's algorithm).
pub fn fsum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut partials: Vec<f64> = Vec::with_capacity(8);
    let mut special = 0.0f64;
    let mut inf_sum = 0.0f64;
    for v in values {
        let mut x = v;
        if !x.is_finite() {
            if x.is_infinite() {
                inf_sum += x;
            }
            special += x;
            continue;
        }
        let mut i = 0;
        for j in 0..partials.len() {
            let mut y = partials[j];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            let lo = y - (hi - x);
            if lo != 0.0 {
                partials[i] = lo;
                i += 1;
            }
            x = hi;
        }
        partials.truncate(i);
        partials.push(x);
    }
    if special != 0.0 || special.is_nan() {
        // -inf + inf has no answer; return NaN.
        return if inf_sum.is_nan() { f64::NAN } else { special };
    }
    let mut n = partials.len();
    let mut hi = 0.0;
    if n > 0 {
        n -= 1;
        hi = partials[n];
        let mut lo = 0.0;
        while n > 0 {
            let x = hi;
            n -= 1;
            let y = partials[n];
            hi = x + y;
            let yr = hi - x;
            lo = y - yr;
            if lo != 0.0 {
                break;
            }
        }
        if n > 0 && ((lo < 0.0 && partials[n - 1] < 0.0) || (lo > 0.0 && partials[n - 1] > 0.0)) {
            let y = lo * 2.0;
            let x = hi + y;
            let yr = x - hi;
            if y == yr {
                hi = x;
            }
        }
    }
    hi
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_format() {
        let cases = [
            (50.0, 4, "50"),
            (0.01, 4, "0.01"),
            (12.345678, 4, "12.35"),
            (1234.5, 4, "1234"),
            (12345.0, 4, "1.234e+04"),
            (0.00001, 4, "1e-05"),
            (0.0001, 4, "0.0001"),
            (100.0, 4, "100"),
            (0.5, 6, "0.5"),
            (0.07, 6, "0.07"),
            (1e-7, 6, "1e-07"),
            (123456789.0, 6, "1.23457e+08"),
            (-2.5, 6, "-2.5"),
            (0.0, 4, "0"),
            (-0.0, 4, "-0"),
        ];
        for (x, p, want) in cases {
            assert_eq!(g(x, p), want, "{x} .{p}g");
        }
    }

    #[test]
    fn shortest_round_trips() {
        let cases = [
            (1.0, "1.0"),
            (0.5, "0.5"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (-1.5e-7, "-1.5e-07"),
            (123.456, "123.456"),
            (-0.0, "-0.0"),
            (f64::INFINITY, "inf"),
            (2.5e300, "2.5e+300"),
        ];
        for (x, want) in cases {
            assert_eq!(shortest(x), want);
        }
    }

    #[test]
    fn fixed_and_signed() {
        assert_eq!(fixed(2.5, 0), "2");
        assert_eq!(fixed(0.125, 2), "0.12");
        assert_eq!(fixed(f64::NAN, 2), "nan");
        assert_eq!(signed(0.0, 2), "+0.00");
        assert_eq!(signed(-0.004, 2), "-0.00");
        assert_eq!(signed(f64::NAN, 1), "+nan");
        assert_eq!(grouped0(-1234567.5), "-1,234,568");
        assert_eq!(grouped0(999.4), "999");
        assert_eq!(grouped0(-0.4), "-0");
        assert_eq!(grouped0(1000.0), "1,000");
    }

    #[test]
    fn round_is_half_even_on_the_exact_value() {
        assert_eq!(round(0.5 + 2.0 * 0.05, 10), 0.6);
        assert_eq!(round(0.00048828125, 10), 0.0004882812);
        assert_eq!(round(0.1 + 0.2, 10), 0.3);
    }

    #[test]
    fn parse_float_accepts_underscores_and_specials() {
        assert_eq!(parse_float(" 0.5 "), Some(0.5));
        assert_eq!(parse_float("1_000.5"), Some(1000.5));
        assert_eq!(parse_float("1__0"), None);
        assert_eq!(parse_float("_1"), None);
        assert_eq!(parse_float("-inf"), Some(f64::NEG_INFINITY));
        assert!(parse_float("nan").unwrap().is_nan());
        assert_eq!(parse_float("low"), None);
        assert_eq!(parse_float(""), None);
        assert_eq!(parse_float("1e5"), Some(1e5));
        assert_eq!(parse_float(".5"), Some(0.5));
        assert_eq!(parse_float("5."), Some(5.0));
        assert_eq!(parse_float("--1"), None);
    }

    #[test]
    fn quoting() {
        assert_eq!(json_str("a\"b\\\u{e9}\n"), "\"a\\\"b\\\\\\u00e9\\n\"");
        assert_eq!(quote_str("min_price"), "'min_price'");
        assert_eq!(quote_str("it's"), "\"it's\"");
        assert_eq!(quote_str("a'\"b"), "'a\\'\"b'");
        assert_eq!(html_escape("<t> & 'x'"), "&lt;t&gt; &amp; &#x27;x&#x27;");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(path_join("./output", "x.csv"), "./output/x.csv");
        assert_eq!(path_join("out/", "x.csv"), "out/x.csv");
    }

    #[test]
    fn fsum_is_exact() {
        assert_eq!(fsum([0.1; 10]), 1.0);
        assert_eq!(fsum([1e100, 1.0, -1e100, 1e-100, 1e50, -1.0, -1e50]), 1e-100);
        assert_eq!(fsum([]), 0.0);
        assert_eq!(fsum([f64::INFINITY, 1.0]), f64::INFINITY);
        assert!(fsum([f64::INFINITY, f64::NEG_INFINITY]).is_nan());
    }
}
