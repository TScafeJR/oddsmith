//! Number formatting and rounding with a fixed spelling for special values.

/// The shortest digits that round-trip, scientific notation when the decimal
/// exponent is below -4 or at least 16, and always a `.0` on integral fixed values.
pub fn shortest(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // `{:e}` yields the shortest round-trip digits, e.g. "-1.2345e-5".
    let s = format!("{v:e}");
    let (mant, exp) = s.split_once('e').expect("LowerExp always has an exponent");
    let exp: i32 = exp.parse().expect("LowerExp exponent is an integer");
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let mut out = String::with_capacity(digits.len() + 8);
    if neg {
        out.push('-');
    }
    if !(-4..16).contains(&exp) {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.abs()));
    } else if exp >= 0 {
        let int_len = exp as usize + 1;
        if digits.len() <= int_len {
            out.push_str(&digits);
            out.push_str(&"0".repeat(int_len - digits.len()));
            out.push_str(".0");
        } else {
            out.push_str(&digits[..int_len]);
            out.push('.');
            out.push_str(&digits[int_len..]);
        }
    } else {
        out.push_str("0.");
        out.push_str(&"0".repeat((-exp - 1) as usize));
        out.push_str(&digits);
    }
    out
}

/// Fixed-point with `prec` decimals. NaN and infinities are spelled `nan`, `inf`, `-inf`.
pub fn fixed(v: f64, prec: usize) -> String {
    if v.is_nan() {
        return if v.is_sign_negative() { "-nan".into() } else { "nan".into() };
    }
    format!("{v:.prec$}")
}

/// Like [`fixed`], always with a sign.
pub fn fixed_signed(v: f64, prec: usize) -> String {
    if v.is_nan() {
        return if v.is_sign_negative() { "-nan".into() } else { "+nan".into() };
    }
    format!("{v:+.prec$}")
}

/// Fixed-point with 6 decimals; NaN and infinities as `NaN`, `+Inf`, `-Inf`.
pub fn ff(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "+Inf".into() } else { "-Inf".into() };
    }
    format!("{v:.6}")
}

/// One decimal; NaN and infinities as `NaN`, `+Inf`, `-Inf`.
pub fn fmt_1f(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "+Inf".into() } else { "-Inf".into() };
    }
    format!("{v:.1}")
}

/// Half away from zero via truncation (used by the overconfidence report).
pub fn round_half_away_trunc(x: f64) -> f64 {
    if x.is_nan() || x.is_infinite() {
        return x;
    }
    let mut t = x.trunc();
    if (x - t).abs() >= 0.5 {
        t += 1.0f64.copysign(x);
    }
    if t != 0.0 { t } else { 0.0f64.copysign(x) }
}

/// Half away from zero via `floor(|x| + 0.5)` (used by the wallet reports).
pub fn round_half_away_floor(x: f64) -> f64 {
    if x.is_nan() || x.is_infinite() {
        return x;
    }
    (x.abs() + 0.5).floor().copysign(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortest_round_trips() {
        let cases = [
            (1.0, "1.0"),
            (0.1, "0.1"),
            (1e-5, "1e-05"),
            (1.5e16, "1.5e+16"),
            (1e16, "1e+16"),
            (9999999999999998.0, "9999999999999998.0"),
            (123456789012345.6, "123456789012345.6"),
            (0.0001, "0.0001"),
            (-0.0, "-0.0"),
            (1e22, "1e+22"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (100.0, "100.0"),
            (0.30000000000000004, "0.30000000000000004"),
            (1234.5e-7, "0.00012345"),
            (-1471.15, "-1471.15"),
            (f64::NAN, "nan"),
            (f64::NEG_INFINITY, "-inf"),
        ];
        for (v, want) in cases {
            assert_eq!(shortest(v), want, "{v}");
        }
    }

    #[test]
    fn fixed_formats() {
        assert_eq!(fixed(0.125, 2), "0.12");
        assert_eq!(fixed(2.5, 0), "2");
        assert_eq!(fixed(f64::NAN, 4), "nan");
        assert_eq!(fixed_signed(0.35, 4), "+0.3500");
        assert_eq!(fixed_signed(f64::NAN, 4), "+nan");
        assert_eq!(ff(-0.0), "-0.000000");
        assert_eq!(ff(f64::INFINITY), "+Inf");
        assert_eq!(fmt_1f(4.0), "4.0");
    }

    #[test]
    fn rounding_half_away() {
        assert_eq!(round_half_away_trunc(2.5), 3.0);
        assert_eq!(round_half_away_trunc(-2.5), -3.0);
        assert_eq!(round_half_away_trunc(-0.4), -0.0);
        assert!(round_half_away_trunc(-0.4).is_sign_negative());
        assert_eq!(round_half_away_floor(2.5), 3.0);
        assert_eq!(round_half_away_floor(-2.5), -3.0);
        assert_eq!(round_half_away_floor(1234.49), 1234.0);
    }
}
