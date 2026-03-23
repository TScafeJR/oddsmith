//! Small string helpers for whitespace, case folding, JSON quoting and timestamp parsing.

use chrono::{DateTime, FixedOffset, NaiveDate};

/// Whitespace: Unicode whitespace plus the ASCII file/group/record/unit
/// separators (`\x1c`..`\x1f`), which Rust's `char::is_whitespace` leaves out.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Trim leading and trailing whitespace (as [`is_space`] defines it).
pub fn strip_ws(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// Split on runs of whitespace (as [`is_space`] defines it), dropping empty pieces.
pub fn split_ws(s: &str) -> impl Iterator<Item = &str> {
    s.split(is_space).filter(|w| !w.is_empty())
}

/// Case folding for the cases that matter here: lowercase, with the common
/// full case foldings (`ß` to `ss`, final sigma to sigma).
pub fn fold_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'ß' | 'ẞ' => out.push_str("ss"),
            'ς' => out.push('σ'),
            _ => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// Case-insensitive equality via [`fold_case`].
pub fn equal_fold(a: &str, b: &str) -> bool {
    a == b || fold_case(a) == fold_case(b)
}

/// Unicode lowercase.
pub fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// A JSON string literal with every non-ASCII character escaped as `\uXXXX`.
pub fn json_quote(s: &str) -> String {
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
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

/// The decimal digits of a signed hex string of any length, used for
/// NUMERIC values too large for `i128`. Returns `None` if `hex` has a non-hex digit.
pub fn hex_to_decimal(hex: &str) -> Option<String> {
    let (neg, digits) = match hex.as_bytes().first() {
        Some(b'-') => (true, &hex[1..]),
        Some(b'+') => (false, &hex[1..]),
        _ => (false, hex),
    };
    if digits.is_empty() {
        return None;
    }
    // Little-endian base 1e9 limbs.
    let mut limbs: Vec<u64> = vec![0];
    for c in digits.chars() {
        let d = c.to_digit(16)? as u64;
        let mut carry = d;
        for limb in limbs.iter_mut() {
            let v = *limb * 16 + carry;
            *limb = v % 1_000_000_000;
            carry = v / 1_000_000_000;
        }
        while carry > 0 {
            limbs.push(carry % 1_000_000_000);
            carry /= 1_000_000_000;
        }
    }
    let mut out = String::new();
    let is_zero = limbs.iter().all(|l| *l == 0);
    if neg && !is_zero {
        out.push('-');
    }
    let mut iter = limbs.iter().rev();
    if let Some(first) = iter.next() {
        out.push_str(&first.to_string());
    }
    for l in iter {
        out.push_str(&format!("{l:09}"));
    }
    Some(out)
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.frac](Z|+HH:MM)`: `,` works as the decimal mark, digits past
/// microseconds are dropped, and the offset may be anything strictly within 24 hours. `None`
/// if invalid.
pub fn parse_rfc3339(s: &str) -> Option<DateTime<FixedOffset>> {
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<u32> {
        let part = b.get(from..from + len)?;
        part.iter().all(u8::is_ascii_digit).then(|| part.iter().fold(0u32, |a, d| a * 10 + (d - b'0') as u32))
    };
    let lit = |i: usize, c: u8| b.get(i) == Some(&c);
    if !(lit(4, b'-') && lit(7, b'-') && lit(10, b'T') && lit(13, b':') && lit(16, b':')) {
        return None;
    }
    let (year, month, day) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (hour, minute, second) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    let mut i = 19;
    let mut micro = 0;
    if matches!(b.get(i), Some(b'.' | b',')) {
        let start = i + 1;
        let mut j = start;
        while b.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        if j == start {
            return None;
        }
        micro = format!("{}000000", &s[start..j])[..6].parse().ok()?;
        i = j;
    }
    let offset = match &s[i..] {
        "Z" => 0,
        z if z.len() == 6 && matches!(z.as_bytes()[0], b'+' | b'-') && z.as_bytes()[3] == b':' => {
            let secs = (num(i + 1, 2)? * 3600 + num(i + 4, 2)? * 60) as i32;
            if secs >= 86_400 {
                return None;
            }
            if z.starts_with('-') { -secs } else { secs }
        }
        _ => return None,
    };
    if year == 0 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let naive = NaiveDate::from_ymd_opt(year as i32, month, day)?.and_hms_micro_opt(hour, minute, second, micro)?;
    naive.and_local_timezone(FixedOffset::east_opt(offset)?).single()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_follows_fromisoformat() {
        let p = |s: &str| parse_rfc3339(s).map(|t| t.to_rfc3339());
        assert_eq!(p("2020-01-01T00:00:00Z").as_deref(), Some("2020-01-01T00:00:00+00:00"));
        assert_eq!(p("2020-01-01T00:00:00.1234567Z").as_deref(), Some("2020-01-01T00:00:00.123456+00:00"));
        assert_eq!(p("2020-01-01T00:00:00,5-05:00").as_deref(), Some("2020-01-01T00:00:00.500-05:00"));
        assert_eq!(p("2020-01-01T23:00:00+12:60").as_deref(), Some("2020-01-01T23:00:00+13:00"));
        assert_eq!(p("2020-01-01T00:00:00+23:59").as_deref(), Some("2020-01-01T00:00:00+23:59"));
        for bad in [
            "2020-01-01T24:00:00Z",
            "2020-02-30T00:00:00Z",
            "2020-01-01T23:59:60Z",
            "2020-01-01T00:00:00+24:00",
            "2020-01-01T00:00:00",
            "2020-01-01 00:00:00Z",
            "2020-01-01T00:00:00.Z",
            "0000-01-01T00:00:00Z",
            "",
        ] {
            assert_eq!(p(bad), None, "{bad}");
        }
    }

    #[test]
    fn strip_and_split_on_unicode_whitespace() {
        assert_eq!(strip_ws("\x1c a b \u{a0}\n"), "a b");
        assert_eq!(split_ws("  a\tb\u{2003}c  ").collect::<Vec<_>>(), vec!["a", "b", "c"]);
    }

    #[test]
    fn fold_case_handles_sharp_s() {
        assert!(equal_fold("Straße", "STRASSE"));
        assert!(equal_fold("Senate", "senate"));
        assert!(!equal_fold("house", "senate"));
    }

    #[test]
    fn json_quote_escapes_non_ascii() {
        assert_eq!(json_quote("abc"), r#""abc""#);
        assert_eq!(json_quote("\u{e9}\"\u{1}\u{7f}"), "\"\\u00e9\\\"\\u0001\\u007f\"");
        assert_eq!(json_quote("\u{1f600}"), "\"\\ud83d\\ude00\"");
    }

    #[test]
    fn hex_to_decimal_handles_big_values() {
        assert_eq!(hex_to_decimal("ff").as_deref(), Some("255"));
        assert_eq!(hex_to_decimal("-10").as_deref(), Some("-16"));
        assert_eq!(hex_to_decimal("0").as_deref(), Some("0"));
        assert_eq!(
            hex_to_decimal("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff").as_deref(),
            Some("115792089237316195423570985008687907853269984665640564039457584007913129639935")
        );
        assert_eq!(hex_to_decimal("xyz"), None);
    }
}
