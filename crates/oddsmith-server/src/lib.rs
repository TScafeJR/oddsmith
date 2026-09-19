//! The backtest web UI: a small threaded HTTP server that serves the page, runs backtests
//! with progress streamed as server-sent events, and offers CSV downloads.

pub mod csv_export;
mod format;
mod http;
mod json;

use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use oddsmith_backtest::{Engine, is_cancelled};
use oddsmith_core::strategy::parse_window;
use oddsmith_core::textfmt::{self, fixed};
use oddsmith_core::{PortfolioMetrics, Position, Strategy};
use oddsmith_store::Db;

use crate::http::{Abort, Request, ResponseWriter, Shared, lock};
use crate::json::{Text, Value};

/// The page served for every path without its own route.
const INDEX_HTML: &str = include_str!("index.html");

/// Stack size for connection threads; decoding deeply nested JSON recurses.
const THREAD_STACK: usize = 64 << 20;

/// Serve the UI on `port` until the process exits.
pub fn serve(db: Db, port: u16) -> Result<()> {
    let server = Arc::new(Server { db, state: Mutex::new(State::default()) });
    log_printf(&format!("starting server on :{port}"));
    let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|e| anyhow!("{e}"))?;
    let route: Arc<http::Handler> = Arc::new(move |w: &Shared, r: &Request| server.route(w, r));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let route = route.clone();
        let spawned = std::thread::Builder::new()
            .stack_size(THREAD_STACK)
            .spawn(move || http::handle_connection(stream, route.as_ref()));
        if let Err(e) = spawned {
            log_printf(&format!("spawning connection thread: {e}"));
        }
    }
    Ok(())
}

/// Print a timestamped line to stderr.
pub(crate) fn log_printf(msg: &str) {
    eprintln!("{} {msg}", chrono::Local::now().format("%Y/%m/%d %H:%M:%S"));
}

#[derive(Default)]
struct State {
    /// Cancels the most recent backtest run.
    cancel: Option<Arc<AtomicBool>>,
    /// Positions of the last successful run, for the CSV download.
    last_positions: Option<Arc<Vec<Position>>>,
}

struct Server {
    db: Db,
    state: Mutex<State>,
}

impl Server {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn route(&self, w: &Shared, r: &Request) -> Result<(), Abort> {
        match r.path.as_str() {
            "/api/backtest" => return self.handle_backtest(w, r),
            "/api/backtest/cancel" => self.handle_cancel(&mut lock(w), r),
            "/api/backtest/trades.csv" => self.handle_trades_csv(&mut lock(w)),
            "/api/trades.csv" => self.handle_all_trades_csv(w),
            "/api/markets.csv" => self.handle_all_markets_csv(&mut lock(w)),
            _ => {
                let mut w = lock(w);
                w.set_header("Content-Type", "text/html; charset=utf-8");
                w.write(INDEX_HTML.as_bytes());
            }
        }
        Ok(())
    }

    fn handle_cancel(&self, w: &mut ResponseWriter, r: &Request) {
        if r.method != "POST" {
            http_error(w, "POST only", 405);
            return;
        }
        let cancel = self.state().cancel.clone();
        if let Some(c) = cancel {
            c.store(true, Ordering::SeqCst);
            log_printf("backtest cancelled by user");
        }
        w.set_header("Content-Type", "application/json");
        w.write(b"{\"cancelled\": true}");
    }

    fn handle_backtest(&self, shared: &Shared, r: &Request) -> Result<(), Abort> {
        let mut w = lock(shared);
        if r.method != "POST" {
            http_error(&mut w, "POST only", 405);
            return Ok(());
        }
        let mut req = match decode_backtest_request(&r.body) {
            Ok(req) => req,
            Err(RequestError::Invalid(msg)) => {
                send_sse(&mut w, "error", &format!("invalid JSON: {msg}"));
                return Ok(());
            }
            Err(RequestError::Abort(msg)) => return Err(Abort(msg)),
        };
        req.apply_defaults();

        w.set_header("Content-Type", "text/event-stream");
        w.set_header("Cache-Control", "no-cache");
        w.set_header("Connection", "keep-alive");

        let strat = req.strategy();
        if let Err(msg) = req.check_window() {
            send_sse(&mut w, "error", &msg);
            return Ok(());
        }

        log_printf(&format!(
            "[backtest] starting: strategy={} side={} price=[{},{}] race={}",
            strat.name,
            strat.side,
            fixed(strat.min_price, 2),
            fixed(strat.max_price, 2),
            json::quote_unicode(&req.race_filter.text)
        ));
        // A lone surrogate can't be logged; drop the connection.
        let shown = [&req.name, &req.side, &req.description, &req.race_filter];
        if shown.iter().any(|t| t.has_surrogate()) {
            return Err(Abort("request text contains a lone surrogate and cannot be logged".into()));
        }

        send_sse(
            &mut w,
            "log",
            &format!(
                "starting backtest: {} (side={}, price=[{}, {}])",
                strat.name,
                strat.side,
                fixed(strat.min_price, 2),
                fixed(strat.max_price, 2)
            ),
        );
        if !strat.description.is_empty() {
            send_sse(&mut w, "log", &format!("description: {}", strat.description));
        }
        send_sse(
            &mut w,
            "log",
            &format!(
                "config: initial_capital=${}, position_size=${}, role={}, fee_rate={}, buckets={}",
                fixed(strat.initial_capital, 0),
                fixed(strat.position_size, 0),
                strat.role,
                fixed(strat.fee_rate(), 4),
                format::float_slice(strat.buckets.as_deref())
            ),
        );
        if !req.race_filter.text.is_empty() {
            send_sse(&mut w, "log", &format!("race filter: {}", req.race_filter.text));
        }
        send_sse(&mut w, "log", "querying resolved markets from database...");
        w.flush();

        let log_writer = shared.clone();
        let mut engine = Engine::new(self.db.clone(), strat.clone());
        engine.verbose = true;
        engine.skip_persist = true;
        engine.log = Some(Arc::new(move |msg: &str| {
            let mut w = lock(&log_writer);
            send_sse(&mut w, "log", msg.trim_end_matches('\n'));
            w.flush();
        }));

        // The flag is also set when the client disconnects.
        let cancel = Arc::new(AtomicBool::new(false));
        let on_cancel = cancel.clone();
        w.on_disconnect = Some(Box::new(move || on_cancel.store(true, Ordering::SeqCst)));
        self.state().cancel = Some(cancel.clone());
        drop(w);

        let outcome = engine.run(&req.race_filter.text, Some(&cancel));
        let mut w = lock(shared);
        let result = match outcome {
            Ok(result) => result,
            Err(err) => {
                if cancel.load(Ordering::SeqCst) || is_cancelled(&err) {
                    send_sse(&mut w, "log", "backtest cancelled");
                    log_printf(&format!("[backtest] cancelled: {}", strat.name));
                } else {
                    send_sse(&mut w, "error", &format!("{err:#}"));
                    log_printf(&format!("[backtest] error: {}: {err:#}", strat.name));
                }
                cancel.store(true, Ordering::SeqCst);
                w.flush();
                return Ok(());
            }
        };
        cancel.store(true, Ordering::SeqCst);

        let metrics = &result.metrics;
        self.state().last_positions = Some(Arc::new(result.positions));

        let result_json = result_json(&req, metrics).unwrap_or_default();
        send_sse(&mut w, "result", &result_json);
        send_sse(
            &mut w,
            "log",
            &format!(
                "backtest complete! {} trades, PnL=${}, win_rate={}%",
                metrics.total_trades,
                fixed(metrics.total_pnl, 2),
                fixed(metrics.win_rate * 100.0, 1)
            ),
        );
        w.flush();

        log_printf(&format!(
            "[backtest] complete: strategy={} trades={} pnl=${} win_rate={}%",
            strat.name,
            metrics.total_trades,
            fixed(metrics.total_pnl, 2),
            fixed(metrics.win_rate * 100.0, 1)
        ));
        Ok(())
    }

    fn handle_trades_csv(&self, w: &mut ResponseWriter) {
        let positions = self.state().last_positions.clone();
        let Some(positions) = positions.filter(|p| !p.is_empty()) else {
            http_error(w, "no backtest results available; run a backtest first", 404);
            return;
        };
        w.set_header("Content-Type", "text/csv");
        w.set_header("Content-Disposition", "attachment; filename=backtest_trades.csv");
        w.write(b"market_id,entry_price,size,side,role,resolution,pnl,fee,return_pct,bucket,hold_days\n");
        for p in positions.iter() {
            w.write(position_line(p).as_bytes());
        }
    }

    fn handle_all_trades_csv(&self, shared: &Shared) {
        let disconnected = Arc::new(AtomicBool::new(false));
        {
            let mut w = lock(shared);
            w.set_header("Content-Type", "text/csv");
            w.set_header("Content-Disposition", "attachment; filename=all_trades.csv");
            w.set_header("X-Content-Type-Options", "nosniff");
            // Set when the client goes away, so the export stops early.
            let d = disconnected.clone();
            w.on_disconnect = Some(Box::new(move || d.store(true, Ordering::SeqCst)));
            w.write(format::csv_line(&format::TRADES_HEADER).as_bytes());
            w.flush();
        }

        let mut cursor = DateTime::<Utc>::UNIX_EPOCH;
        let mut last_id = String::new();
        let mut total = 0usize;
        loop {
            if disconnected.load(Ordering::SeqCst) {
                log_printf(&format!("[trades.csv] query error at {total} rows: client disconnected"));
                return;
            }
            let rows = match self.db.query(format::TRADES_SQL, &[&cursor, &last_id, &format::TRADES_PAGE_SIZE]) {
                Ok(rows) => rows,
                Err(e) => {
                    log_printf(&format!("[trades.csv] query error at {total} rows: {e:#}"));
                    return;
                }
            };
            let mut w = lock(shared);
            let page_count = rows.len();
            for row in &rows {
                let (fields, traded_at, id) = format::trade_fields(row);
                w.write(format::csv_line(&fields).as_bytes());
                if let Some(t) = traded_at {
                    cursor = t;
                }
                if let Some(id) = id {
                    last_id = id;
                }
            }
            total += page_count;
            w.flush();
            drop(w);
            log_printf(&format!("[trades.csv] streamed {total} rows..."));
            if (page_count as i64) < format::TRADES_PAGE_SIZE {
                break;
            }
        }
        log_printf(&format!("[trades.csv] done: {total} rows"));
    }

    fn handle_all_markets_csv(&self, w: &mut ResponseWriter) {
        w.set_header("Content-Type", "text/csv");
        w.set_header("Content-Disposition", "attachment; filename=all_markets.csv");
        w.set_header("X-Content-Type-Options", "nosniff");
        w.write(format::csv_line(&format::MARKETS_HEADER).as_bytes());
        let rows = match self.db.query(format::MARKETS_SQL, &[]) {
            Ok(rows) => rows,
            Err(e) => {
                log_printf(&format!("[markets.csv] query error: {e:#}"));
                return;
            }
        };
        for row in &rows {
            w.write(format::csv_line(&format::market_fields(row)).as_bytes());
        }
        log_printf(&format!("[markets.csv] done: {} rows", rows.len()));
    }
}

/// One row of the backtest trades CSV.
fn position_line(p: &Position) -> String {
    use format::fixed as f;
    format!(
        "{},{},{},{},{},{},{},{},{},{},{}\n",
        p.market_id,
        f(p.entry_price, 6),
        f(p.size, 2),
        p.side,
        p.role,
        p.resolution,
        f(p.pnl, 6),
        f(p.fee, 6),
        f(p.return_pct, 6),
        p.bucket,
        f(p.hold_days, 1)
    )
}

/// Send `data` as one event per line.
fn send_sse(w: &mut ResponseWriter, event: &str, data: &str) {
    for line in data.split('\n') {
        w.write(format!("event: {event}\ndata: {line}\n\n").as_bytes());
    }
}

/// A plain-text error response with the given status code.
fn http_error(w: &mut ResponseWriter, error: &str, code: u16) {
    w.set_header("Content-Type", "text/plain; charset=utf-8");
    w.set_header("X-Content-Type-Options", "nosniff");
    w.write_header(code);
    w.write(format!("{error}\n").as_bytes());
}

/// The JSON body of a backtest request. Strings keep their decoded form so a lone
/// surrogate can be reported in error messages.
#[derive(Debug, Clone, Default, PartialEq)]
struct BacktestRequest {
    name: Text,
    description: Text,
    side: Text,
    min_price: f64,
    max_price: f64,
    race_filter: Text,
    initial_capital: f64,
    position_size: f64,
    role: Text,
    maker_fee_rate: f64,
    taker_fee_rate: f64,
    buckets: Option<Vec<f64>>,
    start_date: Text,
    end_date: Text,
    window_by: Text,
    enforce_capital: bool,
    /// These defaults are integers, so the response writes them without `.0`.
    default_capital: bool,
    default_size: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum RequestError {
    /// Reported to the client as `invalid JSON: ...`.
    Invalid(String),
    /// Not reported to the client: the error is logged and the connection closed.
    Abort(String),
}

fn plain(s: &str) -> Text {
    Text { text: s.to_string(), json: None }
}

impl BacktestRequest {
    fn apply_defaults(&mut self) {
        if self.side.text.is_empty() {
            self.side = plain("YES");
        }
        if self.initial_capital == 0.0 {
            self.initial_capital = 10000.0;
            self.default_capital = true;
        }
        if self.position_size == 0.0 {
            self.position_size = 100.0;
            self.default_size = true;
        }
        let role =
            if self.role.text.to_uppercase() == "TAKER" && !self.role.has_surrogate() { "TAKER" } else { "MAKER" };
        self.role = plain(role);
        if self.name.text.is_empty() {
            self.name = plain(if self.side.text == "NO" && !self.side.has_surrogate() {
                "no_strategy"
            } else {
                "yes_strategy"
            });
        }
    }

    fn strategy(&self) -> Strategy {
        Strategy {
            name: self.name.text.clone(),
            description: self.description.text.clone(),
            side: self.side.text.clone(),
            min_price: self.min_price,
            max_price: self.max_price,
            role: self.role.text.clone(),
            initial_capital: self.initial_capital,
            position_size: self.position_size,
            maker_fee_rate: self.maker_fee_rate,
            taker_fee_rate: self.taker_fee_rate,
            buckets: self.buckets.clone(),
            start_date: self.start_date.text.clone(),
            end_date: self.end_date.text.clone(),
            window_by: self.window_by.text.clone(),
            enforce_capital: self.enforce_capital,
            ..Strategy::default()
        }
    }

    /// Validate the date window, reporting values that hold lone surrogates (which can
    /// never be valid) by their JSON spelling.
    fn check_window(&self) -> Result<(), String> {
        let msg = |e: anyhow::Error| format!("{e:#}");
        if let Some(j) = &self.window_by.json {
            return Err(format!("invalid window_by {j}: must be traded or resolved"));
        }
        let by = &self.window_by.text;
        if let Some(j) = &self.start_date.json {
            parse_window("", "", by).map_err(msg)?;
            return Err(format!("invalid start_date {j}: expected YYYY-MM-DD"));
        }
        if let Some(j) = &self.end_date.json {
            parse_window(&self.start_date.text, "", by).map_err(msg)?;
            return Err(format!("invalid end_date {j}: expected YYYY-MM-DD"));
        }
        parse_window(&self.start_date.text, &self.end_date.text, by).map(|_| ()).map_err(msg)
    }
}

/// Decode the first JSON value in the body (trailing data is ignored) into a request.
/// Missing keys and nulls keep defaults, unknown keys are ignored, and a value of the
/// wrong JSON type is an error naming the field.
fn decode_backtest_request(body: &[u8]) -> Result<BacktestRequest, RequestError> {
    let text = String::from_utf8_lossy(body);
    let text = text.trim_start_matches(format::is_space);
    if text.is_empty() {
        return Err(RequestError::Invalid("EOF".into()));
    }
    let obj = json::decode_prefix(text).map_err(|e| match e {
        json::DecodeError::Value(m) => RequestError::Invalid(m),
        json::DecodeError::Recursion => RequestError::Abort("JSON nested too deeply".into()),
    })?;
    if !matches!(obj, Value::Object(_)) {
        return Err(RequestError::Invalid(format!("invalid backtest request: expected object, got {}", obj.kind())));
    }

    let wrong = |name: &str, expected: &str, v: &Value| {
        RequestError::Invalid(format!("invalid value for field \"{name}\": expected {expected}, got {}", v.kind()))
    };
    let overflow = || RequestError::Abort("integer too large to convert to a number".into());
    let get = |name: &str| obj.get(name).filter(|v| !matches!(v, Value::Null));
    let string = |name: &str| -> Result<Option<Text>, RequestError> {
        match get(name) {
            None => Ok(None),
            Some(Value::Str(t)) => Ok(Some(t.clone())),
            Some(v) => Err(wrong(name, "string", v)),
        }
    };
    let number = |name: &str| -> Result<Option<f64>, RequestError> {
        match get(name) {
            None => Ok(None),
            Some(Value::Int(x)) if x.is_infinite() => Err(overflow()),
            Some(Value::Int(x) | Value::Float(x)) => Ok(Some(*x)),
            Some(v) => Err(wrong(name, "number", v)),
        }
    };

    let mut req = BacktestRequest::default();
    macro_rules! field {
        ($name:ident, $get:ident) => {
            if let Some(v) = $get(stringify!($name))? {
                req.$name = v;
            }
        };
    }
    field!(name, string);
    field!(description, string);
    field!(side, string);
    field!(min_price, number);
    field!(max_price, number);
    field!(race_filter, string);
    field!(initial_capital, number);
    field!(position_size, number);
    field!(role, string);
    field!(maker_fee_rate, number);
    field!(taker_fee_rate, number);
    match get("buckets") {
        None => {}
        Some(Value::Array(items)) if items.iter().all(|x| matches!(x, Value::Int(_) | Value::Float(_))) => {
            let mut out = Vec::with_capacity(items.len());
            for x in items {
                match x {
                    Value::Int(v) if v.is_infinite() => return Err(overflow()),
                    Value::Int(v) | Value::Float(v) => out.push(*v),
                    _ => unreachable!("checked above"),
                }
            }
            req.buckets = Some(out);
        }
        Some(v) => return Err(wrong("buckets", "list of numbers", v)),
    }
    field!(start_date, string);
    field!(end_date, string);
    field!(window_by, string);
    match get("enforce_capital") {
        None => {}
        Some(Value::Bool(b)) => req.enforce_capital = *b,
        Some(v) => return Err(wrong("enforce_capital", "boolean", v)),
    }
    Ok(req)
}

/// The response as compact JSON (no spaces after `,` or `:`); `None` when a float isn't
/// finite.
fn result_json(req: &BacktestRequest, m: &PortfolioMetrics) -> Option<String> {
    let s = |t: &Text| textfmt::json_str(&t.text);
    let f = json::float;
    let int_or = |is_int: bool, v: f64| if is_int { Some(format!("{v:.0}")) } else { f(v) };
    let buckets = match &req.buckets {
        None => "null".to_string(),
        Some(b) => format!("[{}]", b.iter().map(|&v| f(v)).collect::<Option<Vec<_>>>()?.join(",")),
    };
    let strategy = format!(
        "{{\"name\":{},\"description\":{},\"side\":{},\"min_price\":{},\"max_price\":{},\"race_filter\":{},\"initial_capital\":{},\"position_size\":{},\"role\":{},\"maker_fee_rate\":{},\"taker_fee_rate\":{},\"buckets\":{},\"start_date\":{},\"end_date\":{},\"window_by\":{},\"enforce_capital\":{}}}",
        s(&req.name),
        s(&req.description),
        s(&req.side),
        f(req.min_price)?,
        f(req.max_price)?,
        s(&req.race_filter),
        int_or(req.default_capital, req.initial_capital)?,
        int_or(req.default_size, req.position_size)?,
        s(&req.role),
        f(req.maker_fee_rate)?,
        f(req.taker_fee_rate)?,
        buckets,
        s(&req.start_date),
        s(&req.end_date),
        s(&req.window_by),
        req.enforce_capital,
    );
    let mut breakdown = Vec::with_capacity(m.bucket_breakdown.len());
    for b in &m.bucket_breakdown {
        breakdown.push(format!(
            "{{\"Bucket\":{},\"TradeCount\":{},\"TotalPnL\":{},\"AvgReturn\":{},\"MedianReturn\":{},\"WinRate\":{},\"ExpectedWinPct\":{},\"BreakevenWinPct\":{},\"TotalFees\":{},\"StdDev\":{}}}",
            textfmt::json_str(&b.bucket),
            b.trade_count,
            f(b.total_pnl)?,
            f(b.avg_return)?,
            f(b.median_return)?,
            f(b.win_rate)?,
            f(b.expected_win_pct)?,
            f(b.breakeven_win_pct)?,
            f(b.total_fees)?,
            f(b.std_dev)?,
        ));
    }
    let mut out = format!(
        "{{\"strategy\":{strategy},\"total_trades\":{},\"total_pnl\":{},\"total_fees\":{},\"avg_return_pct\":{},\"win_rate\":{},\"sharpe_ratio\":{},\"max_drawdown\":{},\"avg_hold_days\":{},\"bucket_breakdown\":[{}]",
        m.total_trades,
        f(m.total_pnl)?,
        f(m.total_fees)?,
        f(m.avg_return_pct)?,
        f(m.win_rate)?,
        f(m.sharpe_ratio)?,
        f(m.max_drawdown)?,
        f(m.avg_hold_days)?,
        breakdown.join(","),
    );
    if req.enforce_capital {
        out.push_str(&format!(
            ",\"final_equity\":{},\"peak_exposure\":{},\"skipped_trades\":{}",
            f(m.final_equity)?,
            f(m.peak_exposure)?,
            m.skipped_trades
        ));
    }
    out.push('}');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invalid(body: &str) -> String {
        match decode_backtest_request(body.as_bytes()) {
            Err(RequestError::Invalid(m)) => m,
            other => panic!("{body:?}: {other:?}"),
        }
    }

    #[test]
    fn request_decoding_errors() {
        assert_eq!(invalid(""), "EOF");
        assert_eq!(invalid(" \n\u{1c}"), "EOF");
        assert_eq!(invalid("[1]"), "invalid backtest request: expected object, got array");
        assert_eq!(invalid("null"), "invalid backtest request: expected object, got object");
        assert_eq!(
            invalid("{\"min_price\":\"x\"}"),
            "invalid value for field \"min_price\": expected number, got string"
        );
        assert_eq!(invalid("{\"name\":true}"), "invalid value for field \"name\": expected string, got bool");
        assert_eq!(
            invalid("{\"buckets\":[1,true]}"),
            "invalid value for field \"buckets\": expected list of numbers, got array"
        );
        assert_eq!(
            invalid("{\"enforce_capital\":1}"),
            "invalid value for field \"enforce_capital\": expected boolean, got number"
        );
        // Fields are checked in declaration order.
        assert_eq!(
            invalid("{\"window_by\":1,\"side\":2}"),
            "invalid value for field \"side\": expected string, got number"
        );
        assert_eq!(invalid("  {\"a\" 1}"), "Expecting ':' delimiter: line 1 column 6 (char 5)");
        let big = format!("{{\"min_price\":1{}}}", "0".repeat(400));
        assert!(matches!(decode_backtest_request(big.as_bytes()), Err(RequestError::Abort(_))));
    }

    #[test]
    fn request_decoding_values_and_defaults() {
        let mut req = decode_backtest_request(
            b"{\"min_price\":1,\"role\":\"taker\",\"buckets\":[0,50.5],\"name\":null,\"x\":[}]} trailing",
        );
        assert!(req.is_err());
        req = decode_backtest_request(b"{\"min_price\":1,\"role\":\"taker\",\"buckets\":[0,50.5],\"name\":null} junk");
        let mut req = req.unwrap();
        req.apply_defaults();
        assert_eq!(req.min_price, 1.0);
        assert_eq!(req.role.text, "TAKER");
        assert_eq!(req.side.text, "YES");
        assert_eq!(req.name.text, "yes_strategy");
        assert_eq!(req.initial_capital, 10000.0);
        assert_eq!(req.buckets, Some(vec![0.0, 50.5]));
    }

    #[test]
    fn window_errors_quote_surrogates() {
        let req = decode_backtest_request(b"{\"start_date\":\"2024\\udc00\"}").unwrap();
        assert_eq!(req.check_window().unwrap_err(), "invalid start_date \"2024\\udc00\": expected YYYY-MM-DD");
        let req = decode_backtest_request(b"{\"start_date\":\"2024-02-01\",\"end_date\":\"2024-01-01\"}").unwrap();
        assert_eq!(req.check_window().unwrap_err(), "start_date 2024-02-01 is after end_date 2024-01-01");
    }

    #[test]
    fn sse_framing_splits_lines() {
        let (mut w, sink) = http::tests::writer(false);
        send_sse(&mut w, "log", "a\nb");
        send_sse(&mut w, "result", "");
        http::tests::finish(&mut w);
        assert!(
            sink.text().ends_with("\r\n\r\nevent: log\ndata: a\n\nevent: log\ndata: b\n\nevent: result\ndata: \n\n")
        );
    }

    #[test]
    fn result_json_layout() {
        let mut req = decode_backtest_request(b"{\"name\":\"t\\u00e9\",\"enforce_capital\":true}").unwrap();
        req.apply_defaults();
        let m = PortfolioMetrics {
            total_trades: 2,
            total_pnl: 1.5,
            win_rate: 0.5,
            bucket_breakdown: vec![oddsmith_core::BucketMetrics {
                bucket: "50-60%".into(),
                trade_count: 2,
                ..Default::default()
            }],
            skipped_trades: 3,
            ..Default::default()
        };
        let j = result_json(&req, &m).unwrap();
        assert!(j.starts_with(
            "{\"strategy\":{\"name\":\"t\\u00e9\",\"description\":\"\",\"side\":\"YES\",\"min_price\":0.0,"
        ));
        assert!(j.contains("\"initial_capital\":10000,\"position_size\":100,"));
        assert!(j.contains("\"buckets\":null,"));
        assert!(j.contains("\"total_trades\":2,\"total_pnl\":1.5,"));
        assert!(j.contains("\"bucket_breakdown\":[{\"Bucket\":\"50-60%\",\"TradeCount\":2,\"TotalPnL\":0.0,"));
        assert!(j.ends_with(",\"final_equity\":0.0,\"peak_exposure\":0.0,\"skipped_trades\":3}"));
        let m = PortfolioMetrics { sharpe_ratio: f64::NAN, ..m };
        assert_eq!(result_json(&req, &m), None);
    }

    #[test]
    fn position_csv_line() {
        let p = Position {
            market_id: "m1".into(),
            entry_price: 0.625,
            size: 100.0,
            side: "YES".into(),
            role: "MAKER".into(),
            resolution: "YES".into(),
            pnl: f64::NAN,
            fee: 0.0,
            return_pct: 0.6,
            bucket: "60-70%".into(),
            hold_days: 12.25,
            ..Default::default()
        };
        assert_eq!(position_line(&p), "m1,0.625000,100.00,YES,MAKER,YES,NaN,0.000000,0.600000,60-70%,12.2\n");
    }
}
