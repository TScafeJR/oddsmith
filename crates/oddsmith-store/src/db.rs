//! A small blocking connection pool over `postgres::Client`, query helpers, and the
//! [`Num`] type that maps NUMERIC columns to and from `f64`.

use std::error::Error;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use bytes::{BufMut, BytesMut};
use postgres::row::RowIndex;
use postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};
use postgres::{Client, Row, Transaction};
use postgres_native_tls::MakeTlsConnector;

/// A query parameter.
pub type Param<'a> = &'a (dyn ToSql + Sync);

const MIN_CONNS: usize = 2;
const MAX_CONNS: usize = 10;
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

struct PoolState {
    idle: Vec<Client>,
    open: usize,
}

struct Pool {
    config: postgres::Config,
    tls: MakeTlsConnector,
    state: Mutex<PoolState>,
    available: Condvar,
}

impl Pool {
    fn new_client(&self) -> Result<Client> {
        self.config.connect(self.tls.clone()).map_err(Into::into)
    }

    fn release(&self, client: Client) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if client.is_closed() {
            st.open -= 1;
        } else {
            st.idle.push(client);
        }
        drop(st);
        self.available.notify_one();
    }
}

/// A shared handle to the connection pool. Cheap to clone and safe to use from many threads.
#[derive(Clone)]
pub struct Db {
    pool: Arc<Pool>,
}

impl fmt::Debug for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Db")
    }
}

/// A connection checked out of the pool; it goes back when dropped.
pub struct Conn {
    client: Option<Client>,
    pool: Arc<Pool>,
}

impl Deref for Conn {
    type Target = Client;
    fn deref(&self) -> &Client {
        self.client.as_ref().expect("connection already released")
    }
}

impl DerefMut for Conn {
    fn deref_mut(&mut self) -> &mut Client {
        self.client.as_mut().expect("connection already released")
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let Some(c) = self.client.take() {
            self.pool.release(c);
        }
    }
}

impl Db {
    /// Open a pool for a `postgresql://` URL (or key=value conninfo) and ping it.
    ///
    /// TLS follows libpq: `sslmode=prefer` (the default) and `require` encrypt without
    /// verifying the certificate, `verify-ca` / `verify-full` verify it, `disable` turns
    /// TLS off.
    pub fn connect(url: &str) -> Result<Db> {
        let (url, verify) = split_verify_mode(url);
        let mut config: postgres::Config =
            url.parse().map_err(|e| anyhow!("creating connection pool: invalid database URL: {e}"))?;
        if config.get_connect_timeout().is_none() {
            config.connect_timeout(ACQUIRE_TIMEOUT);
        }
        let connector = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(!verify)
            .danger_accept_invalid_hostnames(!verify)
            .build()
            .context("creating connection pool: TLS setup")?;
        let pool = Arc::new(Pool {
            config,
            tls: MakeTlsConnector::new(connector),
            state: Mutex::new(PoolState { idle: Vec::new(), open: 0 }),
            available: Condvar::new(),
        });
        for _ in 0..MIN_CONNS {
            let client = pool.new_client().context("creating connection pool")?;
            let mut st = pool.state.lock().unwrap_or_else(|e| e.into_inner());
            st.idle.push(client);
            st.open += 1;
        }
        let db = Db { pool };
        db.query_one("SELECT 1", &[]).context("pinging database")?;
        Ok(db)
    }

    /// Check a connection out of the pool, opening a new one if under the limit.
    pub fn conn(&self) -> Result<Conn> {
        let mut st = self.pool.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(client) = st.idle.pop() {
                return Ok(Conn { client: Some(client), pool: self.pool.clone() });
            }
            if st.open < MAX_CONNS {
                st.open += 1;
                drop(st);
                return match self.pool.new_client() {
                    Ok(client) => Ok(Conn { client: Some(client), pool: self.pool.clone() }),
                    Err(e) => {
                        self.pool.state.lock().unwrap_or_else(|e| e.into_inner()).open -= 1;
                        self.pool.available.notify_one();
                        Err(e.context("opening database connection"))
                    }
                };
            }
            let (guard, timeout) =
                self.pool.available.wait_timeout(st, ACQUIRE_TIMEOUT).unwrap_or_else(|e| e.into_inner());
            st = guard;
            if timeout.timed_out() && st.idle.is_empty() && st.open >= MAX_CONNS {
                return Err(anyhow!("timed out waiting for a database connection"));
            }
        }
    }

    pub fn query(&self, sql: &str, params: &[Param<'_>]) -> Result<Vec<Row>> {
        Ok(self.conn()?.query(sql, params)?)
    }

    pub fn query_one(&self, sql: &str, params: &[Param<'_>]) -> Result<Option<Row>> {
        Ok(self.conn()?.query_opt(sql, params)?)
    }

    /// Execute one statement and return the affected row count.
    pub fn exec(&self, sql: &str, params: &[Param<'_>]) -> Result<u64> {
        Ok(self.conn()?.execute(sql, params)?)
    }

    /// Run a multi-statement script with the simple query protocol (no parameters).
    pub fn batch_execute(&self, sql: &str) -> Result<()> {
        Ok(self.conn()?.batch_execute(sql)?)
    }

    /// Run `f` in one transaction: commits when it returns `Ok`, rolls back otherwise.
    pub fn tx<T>(&self, f: impl FnOnce(&mut Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn()?;
        let mut tx = conn.transaction()?;
        let out = f(&mut tx)?;
        tx.commit()?;
        Ok(out)
    }
}

/// libpq's `verify-ca` / `verify-full` aren't understood by the URL parser; map them to
/// `require` and remember to verify certificates.
fn split_verify_mode(url: &str) -> (String, bool) {
    let mut verify = false;
    let mut out = url.to_string();
    for mode in ["verify-full", "verify-ca"] {
        let needle = format!("sslmode={mode}");
        if out.contains(&needle) {
            out = out.replace(&needle, "sslmode=require");
            verify = true;
        }
    }
    (out, verify)
}

/// A float that reads from and writes to NUMERIC, FLOAT4/8 and (reading) integer columns.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Num(pub f64);

impl From<f64> for Num {
    fn from(v: f64) -> Self {
        Num(v)
    }
}

impl From<Num> for f64 {
    fn from(v: Num) -> Self {
        v.0
    }
}

type BoxError = Box<dyn Error + Sync + Send>;

impl<'a> FromSql<'a> for Num {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, BoxError> {
        let v = match *ty {
            Type::NUMERIC => decode_numeric(raw)?,
            Type::FLOAT8 => f64::from_sql(ty, raw)?,
            Type::FLOAT4 => f32::from_sql(ty, raw)? as f64,
            Type::INT2 => i16::from_sql(ty, raw)? as f64,
            Type::INT4 => i32::from_sql(ty, raw)? as f64,
            Type::INT8 => i64::from_sql(ty, raw)? as f64,
            _ => return Err(format!("cannot read {ty} as a number").into()),
        };
        Ok(Num(v))
    }

    fn accepts(ty: &Type) -> bool {
        matches!(*ty, Type::NUMERIC | Type::FLOAT8 | Type::FLOAT4 | Type::INT2 | Type::INT4 | Type::INT8)
    }
}

impl ToSql for Num {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, BoxError> {
        match *ty {
            Type::NUMERIC => {
                encode_numeric(self.0, out);
                Ok(IsNull::No)
            }
            Type::FLOAT4 => (self.0 as f32).to_sql(ty, out),
            _ => self.0.to_sql(ty, out),
        }
    }

    fn accepts(ty: &Type) -> bool {
        matches!(*ty, Type::NUMERIC | Type::FLOAT8 | Type::FLOAT4)
    }

    to_sql_checked!();
}

const NUMERIC_POS: u16 = 0x0000;
const NUMERIC_NEG: u16 = 0x4000;
const NUMERIC_NAN: u16 = 0xC000;
const NUMERIC_PINF: u16 = 0xD000;
const NUMERIC_NINF: u16 = 0xF000;

/// Decode Postgres' binary NUMERIC (base-10000 digits) through its decimal string, so the
/// result is the correctly rounded nearest `f64`.
fn decode_numeric(raw: &[u8]) -> Result<f64, BoxError> {
    if raw.len() < 8 {
        return Err("invalid numeric: short header".into());
    }
    let ndigits = i16::from_be_bytes([raw[0], raw[1]]);
    let weight = i16::from_be_bytes([raw[2], raw[3]]);
    let sign = u16::from_be_bytes([raw[4], raw[5]]);
    match sign {
        NUMERIC_NAN => return Ok(f64::NAN),
        NUMERIC_PINF => return Ok(f64::INFINITY),
        NUMERIC_NINF => return Ok(f64::NEG_INFINITY),
        _ => {}
    }
    if ndigits <= 0 {
        return Ok(0.0);
    }
    let body = &raw[8..];
    if body.len() < ndigits as usize * 2 {
        return Err("invalid numeric: truncated digits".into());
    }
    let mut s = String::with_capacity(ndigits as usize * 4 + 8);
    if sign == NUMERIC_NEG {
        s.push('-');
    }
    for i in 0..ndigits as usize {
        let d = u16::from_be_bytes([body[2 * i], body[2 * i + 1]]);
        s.push_str(&format!("{d:04}"));
    }
    let exp = 4 * (weight as i32 - ndigits as i32 + 1);
    s.push_str(&format!("e{exp}"));
    Ok(s.parse::<f64>()?)
}

fn encode_numeric(v: f64, out: &mut BytesMut) {
    let header = |out: &mut BytesMut, ndigits: i16, weight: i16, sign: u16, dscale: u16| {
        out.put_i16(ndigits);
        out.put_i16(weight);
        out.put_u16(sign);
        out.put_u16(dscale);
    };
    if v.is_nan() {
        return header(out, 0, 0, NUMERIC_NAN, 0);
    }
    if v.is_infinite() {
        return header(out, 0, 0, if v > 0.0 { NUMERIC_PINF } else { NUMERIC_NINF }, 0);
    }
    // Display for f64 is the shortest round-trip form and never uses an exponent.
    let s = format!("{}", v.abs());
    let (int_part, frac_part) = s.split_once('.').unwrap_or((&s, ""));
    let dscale = frac_part.len() as u16;
    let int_digits = format!("{}{}", "0".repeat((4 - int_part.len() % 4) % 4), int_part);
    let frac_digits = format!("{}{}", frac_part, "0".repeat((4 - frac_part.len() % 4) % 4));
    let group = |chunk: &[u8]| chunk.iter().fold(0u16, |acc, b| acc * 10 + (b - b'0') as u16);
    let mut digits: Vec<u16> = int_digits.as_bytes().chunks(4).map(group).collect();
    let mut weight = digits.len() as i16 - 1;
    digits.extend(frac_digits.as_bytes().chunks(4).map(group));
    let leading = digits.iter().take_while(|d| **d == 0).count();
    digits.drain(..leading);
    weight -= leading as i16;
    while digits.last() == Some(&0) {
        digits.pop();
    }
    let sign = if digits.is_empty() || v > 0.0 { NUMERIC_POS } else { NUMERIC_NEG };
    if digits.is_empty() {
        weight = 0;
    }
    header(out, digits.len() as i16, weight, sign, dscale);
    for d in digits {
        out.put_u16(d);
    }
}

/// Typed read helpers for rows with NUMERIC / float columns.
pub trait RowExt {
    /// A numeric column as `f64`; NULL reads as 0. Panics if the column isn't numeric.
    fn num<I: RowIndex + fmt::Display>(&self, idx: I) -> f64;
    /// A nullable numeric column.
    fn opt_num<I: RowIndex + fmt::Display>(&self, idx: I) -> Option<f64>;
}

impl RowExt for Row {
    fn num<I: RowIndex + fmt::Display>(&self, idx: I) -> f64 {
        self.opt_num(idx).unwrap_or(0.0)
    }

    fn opt_num<I: RowIndex + fmt::Display>(&self, idx: I) -> Option<f64> {
        self.get::<_, Option<Num>>(idx).map(|n| n.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(v: f64) -> f64 {
        let mut buf = BytesMut::new();
        encode_numeric(v, &mut buf);
        decode_numeric(&buf).unwrap()
    }

    #[test]
    fn numeric_roundtrips_floats() {
        for v in [0.0, 1.0, -1.0, 0.5, 0.07, 0.0175, 123456.789, -0.0001, 1e-10, 99.99, 10000.0, 1e21, 0.1 + 0.2] {
            assert_eq!(roundtrip(v), v, "{v}");
        }
        assert!(roundtrip(f64::NAN).is_nan());
        assert_eq!(roundtrip(f64::INFINITY), f64::INFINITY);
    }

    #[test]
    fn numeric_encoding_matches_postgres_layout() {
        let mut buf = BytesMut::new();
        encode_numeric(0.5, &mut buf);
        // ndigits=1, weight=-1, sign=+, dscale=1, digit 5000
        assert_eq!(&buf[..], &[0, 1, 0xff, 0xff, 0, 0, 0, 1, 0x13, 0x88]);
        let mut buf = BytesMut::new();
        encode_numeric(-12345.6, &mut buf);
        // 1 2345 . 6000 -> ndigits=3, weight=1, sign=-, dscale=1
        assert_eq!(&buf[..8], &[0, 3, 0, 1, 0x40, 0, 0, 1]);
        assert_eq!(decode_numeric(&buf).unwrap(), -12345.6);
    }

    #[test]
    fn verify_modes_map_to_require() {
        assert_eq!(
            split_verify_mode("postgres://h/db?sslmode=verify-full"),
            ("postgres://h/db?sslmode=require".into(), true)
        );
        assert_eq!(
            split_verify_mode("postgres://h/db?sslmode=require"),
            ("postgres://h/db?sslmode=require".into(), false)
        );
    }
}
