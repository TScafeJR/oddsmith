//! Value formatting shared by the server's CSV routes and `oddsmith export csv`.

use chrono::{DateTime, Local, Offset, Utc};

/// Unicode whitespace plus the ASCII file/group/record/unit separators (`\x1c`..`\x1f`).
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Shortest round-trip digits; exponent form when the exponent is below -4 or at least 6.
pub fn float_v(v: f64) -> String {
    if v.is_infinite() {
        return if v > 0.0 { "+Inf".into() } else { "-Inf".into() };
    }
    if v.is_nan() {
        return "NaN".into();
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let neg = if v < 0.0 { "-" } else { "" };
    let sci = format!("{:e}", v.abs());
    let (mant, exp) = sci.split_once('e').expect("exponent form");
    let x: i32 = exp.parse().expect("exponent");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let digits = match digits.trim_end_matches('0') {
        "" => "0",
        d => d,
    };
    if !(-4..6).contains(&x) {
        let mant = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits.to_string() };
        let sign = if x >= 0 { '+' } else { '-' };
        return format!("{neg}{mant}e{sign}{:02}", x.abs());
    }
    let n = digits.len() as i32;
    let point = x + 1; // digits before the decimal point
    if point >= n {
        format!("{neg}{digits}{}", "0".repeat((point - n) as usize))
    } else if point > 0 {
        format!("{neg}{}.{}", &digits[..point as usize], &digits[point as usize..])
    } else {
        format!("{neg}0.{}{digits}", "0".repeat((-point) as usize))
    }
}

/// Fixed-point with `prec` decimals; NaN and infinities print as `NaN`, `+Inf`, `-Inf`.
pub fn fixed(v: f64, prec: usize) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "+Inf".into() } else { "-Inf".into() }
    } else {
        format!("{v:.prec$}")
    }
}

/// RFC 3339 with second precision in the local zone, `Z` when the offset is zero.
pub fn rfc3339(t: DateTime<Utc>) -> String {
    let local = t.with_timezone(&Local);
    let off_min = local.offset().fix().local_minus_utc().div_euclid(60);
    let zone = if off_min == 0 {
        "Z".to_string()
    } else {
        let sign = if off_min > 0 { '+' } else { '-' };
        format!("{sign}{:02}:{:02}", off_min.abs() / 60, off_min.abs() % 60)
    };
    format!("{}{zone}", local.format("%Y-%m-%dT%H:%M:%S"))
}

/// One CSV record terminated by a newline; fields are quoted only when needed.
pub fn csv_line<S: AsRef<str>>(fields: &[S]) -> String {
    let mut line = fields.iter().map(|f| csv_field(f.as_ref())).collect::<Vec<_>>().join(",");
    line.push('\n');
    line
}

fn csv_field(f: &str) -> String {
    let Some(first) = f.chars().next() else { return String::new() };
    if f == "\\." || f.contains([',', '"', '\r', '\n']) || is_space(first) {
        format!("\"{}\"", f.replace('"', "\"\""))
    } else {
        f.to_string()
    }
}

/// A nullable text column as a CSV field.
pub fn pstr(s: Option<String>) -> String {
    s.unwrap_or_default()
}

/// A nullable float column as a CSV field.
pub fn pfloat(v: Option<f64>) -> String {
    v.map(float_v).unwrap_or_default()
}

/// A nullable boolean column as a CSV field.
pub fn pbool(b: Option<bool>) -> String {
    match b {
        None => String::new(),
        Some(true) => "true".into(),
        Some(false) => "false".into(),
    }
}

/// A nullable timestamp column as a CSV field.
pub fn ptime(t: Option<DateTime<Utc>>) -> String {
    t.map(rfc3339).unwrap_or_default()
}

/// Space-separated floats in brackets, e.g. `[0 5 10.5]`.
pub fn float_slice(vals: Option<&[f64]>) -> String {
    let parts: Vec<String> = vals.unwrap_or_default().iter().map(|&v| float_v(v)).collect();
    format!("[{}]", parts.join(" "))
}

/// The markets query shared by `/api/markets.csv` and `oddsmith export csv`.
pub const MARKETS_SQL: &str = "
        SELECT id, event_id, question, slug, condition_id,
               active, closed, resolved, resolution, end_date,
               volume, liquidity, last_trade_price, best_bid, best_ask, spread,
               race_type, state, candidate, party, created_at
        FROM markets ORDER BY volume DESC NULLS LAST";

pub const MARKETS_HEADER: [&str; 21] = [
    "id",
    "event_id",
    "question",
    "slug",
    "condition_id",
    "active",
    "closed",
    "resolved",
    "resolution",
    "end_date",
    "volume",
    "liquidity",
    "last_trade_price",
    "best_bid",
    "best_ask",
    "spread",
    "race_type",
    "state",
    "candidate",
    "party",
    "created_at",
];

/// One page of trades after the `(traded_at, id)` cursor.
pub const TRADES_SQL: &str = "
            SELECT id, market_id, event_id, asset_id, side, price, size,
                   is_taker_trade, maker_address, taker_address,
                   outcome, transaction_hash, bucket_label, traded_at
            FROM trades
            WHERE (traded_at, id) > ($1::timestamptz, $2::text)
            ORDER BY traded_at, id
            LIMIT $3::int8";

pub const TRADES_HEADER: [&str; 14] = [
    "id",
    "market_id",
    "event_id",
    "asset_id",
    "side",
    "price",
    "size",
    "is_taker_trade",
    "maker_address",
    "taker_address",
    "outcome",
    "transaction_hash",
    "bucket_label",
    "traded_at",
];

/// Rows per trades page.
pub const TRADES_PAGE_SIZE: i64 = 100_000;

/// A markets row as CSV fields.
pub fn market_fields(r: &postgres::Row) -> Vec<String> {
    use oddsmith_store::RowExt;
    let s = |i: usize| pstr(r.get(i));
    let b = |i: usize| pbool(r.get(i));
    let f = |i: usize| pfloat(r.opt_num(i));
    let t = |i: usize| ptime(r.get(i));
    vec![
        s(0),
        s(1),
        s(2),
        s(3),
        s(4),
        b(5),
        b(6),
        b(7),
        s(8),
        t(9),
        f(10),
        f(11),
        f(12),
        f(13),
        f(14),
        f(15),
        s(16),
        s(17),
        s(18),
        s(19),
        t(20),
    ]
}

/// A trades row as CSV fields, plus its `traded_at` and `id` for the page cursor.
pub fn trade_fields(r: &postgres::Row) -> (Vec<String>, Option<DateTime<Utc>>, Option<String>) {
    use oddsmith_store::RowExt;
    let id: Option<String> = r.get(0);
    let traded_at: Option<DateTime<Utc>> = r.get(13);
    let s = |i: usize| pstr(r.get(i));
    let f = |i: usize| pfloat(r.opt_num(i));
    let fields = vec![
        pstr(id.clone()),
        s(1),
        s(2),
        s(3),
        s(4),
        f(5),
        f(6),
        pbool(r.get(7)),
        s(8),
        s(9),
        s(10),
        s(11),
        s(12),
        ptime(traded_at),
    ];
    (fields, traded_at, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_v_formatting() {
        let cases = [
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (0.5, "0.5"),
            (100.0, "100"),
            (123456.0, "123456"),
            (1234567.0, "1.234567e+06"),
            (1e6, "1e+06"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (-2.5e-7, "-2.5e-07"),
            (1e21, "1e+21"),
            (0.1 + 0.2, "0.30000000000000004"),
            (999999.5, "999999.5"),
            (f64::INFINITY, "+Inf"),
            (f64::NEG_INFINITY, "-Inf"),
            (f64::NAN, "NaN"),
        ];
        for (v, want) in cases {
            assert_eq!(float_v(v), want, "{v}");
        }
    }

    #[test]
    fn fixed_spells_special_values() {
        assert_eq!(fixed(0.125, 2), "0.12");
        assert_eq!(fixed(2.5, 0), "2");
        assert_eq!(fixed(f64::NAN, 6), "NaN");
        assert_eq!(fixed(f64::INFINITY, 1), "+Inf");
    }

    #[test]
    fn csv_quotes_only_when_needed() {
        assert_eq!(
            csv_line(&["a", "", "b,c", "say \"hi\"", " lead", "\\.", "x\ny", "\u{1c}z"]),
            "a,,\"b,c\",\"say \"\"hi\"\"\",\" lead\",\"\\.\",\"x\ny\",\"\u{1c}z\"\n"
        );
        assert_eq!(csv_line(&["plain"]), "plain\n");
        assert_eq!(csv_line(&[""]), "\n");
    }

    #[test]
    fn rfc3339_utc_offset() {
        let t = DateTime::parse_from_rfc3339("2024-11-05T12:34:56.789Z").unwrap().with_timezone(&Utc);
        let s = rfc3339(t);
        assert!(s.starts_with("2024-11-0"), "{s}");
        assert_eq!(s.len(), if s.ends_with('Z') { 20 } else { 25 });
    }

    #[test]
    fn float_slice_formats() {
        assert_eq!(float_slice(None), "[]");
        assert_eq!(float_slice(Some(&[0.0, 5.0, 10.5])), "[0 5 10.5]");
    }
}
