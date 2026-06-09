//! A minimal HTTP/1.1 server: request-line and header parsing, keep-alive rules, HTML
//! error pages, and a buffered response writer that falls back to chunked transfer
//! encoding once a handler flushes.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, MutexGuard};

/// Longest request or header line accepted, in bytes.
const MAX_LINE: usize = 65536;
/// Most header lines accepted in one request.
const MAX_HEADERS: usize = 100;
/// Methods passed to the route handler; others get a 501.
const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
const SERVER_NAME: &str = concat!("oddsmith/", env!("CARGO_PKG_VERSION"));

const ERROR_PAGE: &str = "<!DOCTYPE HTML>\n<html lang=\"en\">\n    <head>\n        <meta charset=\"utf-8\">\n        <title>Error response</title>\n    </head>\n    <body>\n        <h1>Error response</h1>\n        <p>Error code: {code}</p>\n        <p>Message: {message}.</p>\n        <p>Error code explanation: {code} - {explain}.</p>\n    </body>\n</html>\n";

/// Reason phrase and long description for a status code.
fn status_text(code: u16) -> (&'static str, &'static str) {
    match code {
        100 => ("Continue", "Request received, please continue"),
        200 => ("OK", "Request fulfilled, document follows"),
        400 => ("Bad Request", "Bad request syntax or unsupported method"),
        404 => ("Not Found", "Nothing matches the given URI"),
        405 => ("Method Not Allowed", "Specified method is invalid for this resource"),
        414 => ("URI Too Long", "URI is too long"),
        431 => (
            "Request Header Fields Too Large",
            "The server is unwilling to process the request because its header fields are too large",
        ),
        501 => ("Not Implemented", "Server does not support this operation"),
        505 => ("HTTP Version Not Supported", "Cannot fulfill request"),
        _ => ("???", "???"),
    }
}

/// A parsed request as handlers see it.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

/// The handler failed: the error is logged and the connection closed without a reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Abort(pub String);

/// Routes a request to a handler. The writer is shared so a handler can hand it to a
/// log callback that runs while the handler is still active.
pub type Handler = dyn Fn(&Shared, &Request) -> Result<(), Abort> + Send + Sync;

/// A response writer that a handler and its callbacks can both use.
pub type Shared = Arc<Mutex<ResponseWriter>>;

/// Lock a shared writer, ignoring poisoning.
pub fn lock(w: &Shared) -> MutexGuard<'_, ResponseWriter> {
    w.lock().unwrap_or_else(|e| e.into_inner())
}

/// Connection-level state the response writer also changes.
struct ConnState {
    out: Box<dyn Write + Send>,
    close: bool,
    /// The request line had no version: no status line or headers are sent.
    http09: bool,
}

impl ConnState {
    /// A `Connection: close` or `keep-alive` header also sets whether the connection closes.
    fn note_header(&mut self, name: &str, value: &str) {
        if name.eq_ignore_ascii_case("connection") {
            if value.eq_ignore_ascii_case("close") {
                self.close = true;
            } else if value.eq_ignore_ascii_case("keep-alive") {
                self.close = false;
            }
        }
    }

    fn status_line(&self, code: u16, reason: &str, buf: &mut Vec<u8>) {
        if !self.http09 {
            buf.extend_from_slice(format!("HTTP/1.1 {code} {reason}\r\n").as_bytes());
        }
    }

    fn header(&mut self, name: &str, value: &str, buf: &mut Vec<u8>) {
        if !self.http09 {
            buf.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        self.note_header(name, value);
    }

    fn std_headers(&mut self, buf: &mut Vec<u8>) {
        self.header("Server", SERVER_NAME, buf);
        let date = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        self.header("Date", &date, buf);
    }

    fn end_headers(&mut self, buf: &mut Vec<u8>) {
        if !self.http09 {
            buf.extend_from_slice(b"\r\n");
        }
    }

    /// Send an HTML error page and close the connection.
    fn send_error(&mut self, code: u16, message: Option<&str>, explain: Option<&str>) -> io::Result<()> {
        let (short, long) = status_text(code);
        let message = message.unwrap_or(short);
        let explain = explain.unwrap_or(long);
        let mut buf = Vec::new();
        self.status_line(code, message, &mut buf);
        self.std_headers(&mut buf);
        self.header("Connection", "close", &mut buf);
        let body = ERROR_PAGE
            .replace("{code}", &code.to_string())
            .replace("{message}", &escape_html(message))
            .replace("{explain}", &escape_html(explain));
        self.header("Content-Type", "text/html;charset=utf-8", &mut buf);
        self.header("Content-Length", &body.len().to_string(), &mut buf);
        self.end_headers(&mut buf);
        self.out.write_all(&buf)?;
        self.out.write_all(body.as_bytes())?;
        self.out.flush()
    }
}

/// Escape `&`, `<` and `>` for HTML (quotes are left alone).
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Buffered response writer. Output is held until `flush`
/// or the end of the handler; a response flushed early is sent chunked.
pub struct ResponseWriter {
    /// Response headers in insertion order; setting an existing name replaces its value.
    headers: Vec<(String, String)>,
    /// Called once when a write to the client fails.
    pub on_disconnect: Option<Box<dyn Fn() + Send>>,
    conn: ConnState,
    head_only: bool,
    status: u16,
    buf: Vec<u8>,
    wrote_header: bool,
    chunked: bool,
    broken: bool,
}

impl ResponseWriter {
    fn new(conn: ConnState, head_only: bool) -> Self {
        ResponseWriter {
            headers: Vec::new(),
            on_disconnect: None,
            conn,
            head_only,
            status: 0,
            buf: Vec::new(),
            wrote_header: false,
            chunked: false,
            broken: false,
        }
    }

    /// Set a response header, replacing any existing value for `name`.
    pub fn set_header(&mut self, name: &str, value: &str) {
        match self.headers.iter_mut().find(|(k, _)| k == name) {
            Some(slot) => slot.1 = value.to_string(),
            None => self.headers.push((name.to_string(), value.to_string())),
        }
    }

    pub fn write_header(&mut self, code: u16) {
        if self.status == 0 {
            self.status = code;
        }
    }

    pub fn write(&mut self, data: &[u8]) {
        if self.status == 0 {
            self.status = 200;
        }
        self.buf.extend_from_slice(data);
    }

    pub fn flush(&mut self) {
        if !self.wrote_header {
            self.chunked = true;
            self.send_header(None);
        }
        self.send_body();
    }

    fn finish(&mut self) {
        if !self.wrote_header {
            let n = self.buf.len();
            self.send_header(Some(n));
            self.send_body();
        } else {
            self.send_body();
            if self.chunked {
                self.raw(b"0\r\n\r\n");
            }
        }
    }

    fn send_header(&mut self, content_length: Option<usize>) {
        self.wrote_header = true;
        if self.status == 0 {
            self.status = 200;
        }
        if !self.headers.iter().any(|(k, _)| k == "Content-Type") && !self.buf.is_empty() {
            self.headers.push(("Content-Type".into(), "text/plain; charset=utf-8".into()));
        }
        let mut out = Vec::new();
        let c = &mut self.conn;
        c.status_line(self.status, status_text(self.status).0, &mut out);
        c.std_headers(&mut out);
        for (k, v) in &self.headers {
            c.header(k, v, &mut out);
        }
        match content_length {
            Some(n) => c.header("Content-Length", &n.to_string(), &mut out),
            None if !self.head_only => c.header("Transfer-Encoding", "chunked", &mut out),
            None => {}
        }
        c.end_headers(&mut out);
        if c.out.write_all(&out).and_then(|()| c.out.flush()).is_err() {
            self.disconnected();
        }
    }

    fn send_body(&mut self) {
        let data = std::mem::take(&mut self.buf);
        if data.is_empty() || self.head_only {
            return;
        }
        if self.chunked {
            let mut framed = format!("{:x}\r\n", data.len()).into_bytes();
            framed.extend_from_slice(&data);
            framed.extend_from_slice(b"\r\n");
            self.raw(&framed);
        } else {
            self.raw(&data);
        }
    }

    fn raw(&mut self, data: &[u8]) {
        if self.broken || self.head_only {
            return;
        }
        if self.conn.out.write_all(data).and_then(|()| self.conn.out.flush()).is_err() {
            self.disconnected();
        }
    }

    fn disconnected(&mut self) {
        self.broken = true;
        self.conn.close = true;
        if let Some(f) = &self.on_disconnect {
            f();
        }
    }
}

/// Split a latin-1 decoded line on runs of whitespace.
fn split_words(line: &str) -> Vec<&str> {
    line.split(crate::format::is_space).filter(|w| !w.is_empty()).collect()
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Quote a latin-1 string for an error message: single quotes unless it holds `'` but no
/// `"`, with control characters, DEL, U+0080..U+00A0 and the soft hyphen as `\xNN`.
fn quote_latin1(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        let n = c as u32;
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            _ if n < 0x20 || (0x7f..=0xa0).contains(&n) || n == 0xad => out.push_str(&format!("\\x{n:02x}")),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Parse a header value as a decimal integer: surrounding whitespace, a sign and single `_`
/// between digits are allowed. `None` when invalid or out of `i64` range.
pub fn parse_int(s: &str) -> Option<i64> {
    let t = s.trim_matches(crate::format::is_space);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let b = digits.as_bytes();
    if b.is_empty() || !b[0].is_ascii_digit() || !b[b.len() - 1].is_ascii_digit() {
        return None;
    }
    let mut v: i64 = 0;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'0'..=b'9' => v = v.checked_mul(10)?.checked_add(i64::from(c - b'0'))?,
            b'_' if b[i - 1] != b'_' => {}
            _ => return None,
        }
    }
    Some(if neg { -v } else { v })
}

/// Parsed header lines. Continuation lines are folded into the previous header and the
/// block ends at the first line that isn't a header.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    /// Parse the raw header block (latin-1 text, blank line included).
    pub fn parse(block: &str) -> Headers {
        // Split on \r\n, \r and \n, keeping the endings.
        let mut lines = Vec::new();
        let mut rest = block;
        while !rest.is_empty() {
            let end = match rest.find(['\r', '\n']) {
                Some(i) if rest[i..].starts_with("\r\n") => i + 2,
                Some(i) => i + 1,
                None => rest.len(),
            };
            lines.push(&rest[..end]);
            rest = &rest[end..];
        }
        let is_header = |l: &str| {
            l.starts_with("From ")
                || l.starts_with([' ', '\t'])
                || l.find(':').is_some_and(|i| l[..i].bytes().all(|b| (0x21..=0x7e).contains(&b)))
        };
        let mut out = Vec::new();
        let mut cur: Option<Vec<&str>> = None;
        let flush = |cur: &mut Option<Vec<&str>>, out: &mut Vec<(String, String)>| {
            if let Some(src) = cur.take() {
                let (name, value) = src[0].split_once(':').expect("header line has a colon");
                let mut v = value.trim_start_matches([' ', '\t']).to_string();
                v.extend(src[1..].iter().copied());
                out.push((name.to_string(), v.trim_end_matches(['\r', '\n']).to_string()));
            }
        };
        for line in lines {
            if !is_header(line) {
                break;
            }
            if line.starts_with([' ', '\t']) {
                if let Some(src) = cur.as_mut() {
                    src.push(line);
                }
                continue;
            }
            flush(&mut cur, &mut out);
            if line.starts_with("From ") || line.starts_with(':') {
                continue;
            }
            cur = Some(vec![line]);
        }
        flush(&mut cur, &mut out);
        Headers(out)
    }

    /// The first header with this name (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// The path part of a request target (scheme, authority, query and fragment removed), or an
/// error for a malformed bracketed host.
pub fn url_path(url: &str) -> Result<String, String> {
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let mut url: String = url.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    if let Some(i) = url.find(':') {
        let scheme = &url[..i];
        if i > 0
            && scheme.as_bytes()[0].is_ascii_alphabetic()
            && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
        {
            url = url[i + 1..].to_string();
        }
    }
    if url.starts_with("//") {
        let delim = url[2..].find(['/', '?', '#']).map_or(url.len(), |i| i + 2);
        let netloc = &url[2..delim];
        if netloc.contains('[') != netloc.contains(']') {
            return Err("Invalid IPv6 URL".into());
        }
        if netloc.contains('[') {
            let host = netloc.split_once('[').map_or("", |(_, r)| r.split_once(']').map_or(r, |(h, _)| h));
            check_bracketed_host(host)?;
        }
        url = url[delim..].to_string();
    }
    let path = url.split('#').next().unwrap_or("");
    Ok(path.split('?').next().unwrap_or("").to_string())
}

fn check_bracketed_host(host: &str) -> Result<(), String> {
    if let Some(rest) = host.strip_prefix('v') {
        let ok = rest.split_once('.').is_some_and(|(hex, tail)| {
            !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()) && !tail.is_empty() && !tail.contains('\n')
        });
        return if ok { Ok(()) } else { Err("IPvFuture address is invalid".into()) };
    }
    let addr = host.split_once('%').map_or(host, |(a, _)| a);
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err("An IPv4 address cannot be in brackets".into());
    }
    match addr.parse::<std::net::Ipv6Addr>() {
        Ok(_) => Ok(()),
        Err(_) => Err(format!("{} does not appear to be an IPv4 or IPv6 address", quote_str(host))),
    }
}

fn quote_str(s: &str) -> String {
    oddsmith_core::textfmt::quote_str(s)
}

/// Read up to `limit` bytes, stopping after a newline.
fn read_line(r: &mut impl BufRead, limit: usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    while line.len() < limit {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let want = (limit - line.len()).min(buf.len());
        let (n, done) = match buf[..want].iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (want, false),
        };
        line.extend_from_slice(&buf[..n]);
        r.consume(n);
        if done {
            break;
        }
    }
    Ok(line)
}

/// The outcome of parsing a request line and headers.
enum Parsed {
    Request {
        command: String,
        path: String,
        headers: Headers,
    },
    /// An error reply was sent, or there is nothing to answer.
    Done,
}

/// Serve requests on one connection until it should close.
pub fn handle_connection(stream: TcpStream, route: &Handler) {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let Ok(out) = stream.try_clone() else { return };
    let mut reader = BufReader::new(stream);
    let mut conn = ConnState { out: Box::new(out), close: true, http09: false };
    loop {
        conn = match handle_one_request(&mut reader, conn, route) {
            Ok(Some(c)) if !c.close => c,
            Ok(_) => break,
            Err(msg) => {
                crate::log_printf(&format!("exception while processing request from {peer}: {msg}"));
                break;
            }
        };
    }
    let _ = reader.get_ref().shutdown(std::net::Shutdown::Write);
}

/// One request. Returns the connection state to keep going, `None` to close quietly, or an
/// error message when the connection should be dropped without a reply.
fn handle_one_request(
    reader: &mut BufReader<TcpStream>,
    mut conn: ConnState,
    route: &Handler,
) -> Result<Option<ConnState>, String> {
    let raw = read_line(reader, MAX_LINE + 1).map_err(|e| e.to_string())?;
    if raw.len() > MAX_LINE {
        conn.http09 = false;
        conn.send_error(414, None, None).map_err(|e| e.to_string())?;
        return Ok(Some(conn));
    }
    if raw.is_empty() {
        return Ok(None);
    }
    let (command, path, headers) = match parse_request(reader, &mut conn, &raw).map_err(|e| e.to_string())? {
        Parsed::Done => return Ok(Some(conn)),
        Parsed::Request { command, path, headers } => (command, path, headers),
    };
    if !METHODS.contains(&command.as_str()) {
        let msg = format!("Unsupported method ({})", quote_latin1(&command));
        conn.send_error(501, Some(&msg), None).map_err(|e| e.to_string())?;
        return Ok(Some(conn));
    }

    // Read the body and dispatch.
    let length = match headers.get("Content-Length").filter(|v| !v.is_empty()) {
        None => 0,
        Some(v) => parse_int(v).ok_or_else(|| format!("invalid integer in Content-Length: {}", quote_str(v)))?,
    };
    let mut body = Vec::new();
    if length > 0 {
        reader.by_ref().take(length as u64).read_to_end(&mut body).map_err(|e| e.to_string())?;
    }
    if headers.get("Transfer-Encoding").is_some_and(|v| !v.is_empty()) {
        conn.close = true;
    }
    let path = url_path(&path)?;
    let req = Request { method: command.clone(), path, body };
    let w: Shared = Arc::new(Mutex::new(ResponseWriter::new(conn, command == "HEAD")));
    route(&w, &req).map_err(|a| a.0)?;
    let mut w = lock(&w);
    w.finish();
    w.on_disconnect = None;
    let mut conn = std::mem::replace(&mut w.conn, ConnState { out: Box::new(io::sink()), close: true, http09: false });
    let _ = conn.out.flush();
    Ok(Some(conn))
}

/// Parse the request line and headers, and apply keep-alive and `Expect` handling.
fn parse_request(reader: &mut BufReader<TcpStream>, conn: &mut ConnState, raw: &[u8]) -> io::Result<Parsed> {
    conn.close = true;
    conn.http09 = true;
    let mut version = "HTTP/0.9".to_string();
    let line = latin1(raw);
    let line = line.trim_end_matches(['\r', '\n']);
    let words = split_words(line);
    if words.is_empty() {
        return Ok(Parsed::Done);
    }
    if words.len() >= 3 {
        let v = words[words.len() - 1];
        let number = v.strip_prefix("HTTP/").and_then(|base| {
            let parts: Vec<&str> = base.split('.').collect();
            let ok = parts.len() == 2
                && parts.iter().all(|p| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit()));
            ok.then(|| (parts[0].parse::<u64>().unwrap_or(0), parts[1].parse::<u64>().unwrap_or(0), base))
        });
        let Some((major, minor, base)) = number else {
            conn.send_error(400, Some(&format!("Bad request version ({})", quote_latin1(v))), None)?;
            return Ok(Parsed::Done);
        };
        if (major, minor) >= (1, 1) {
            conn.close = false;
        }
        if major >= 2 {
            conn.send_error(505, Some(&format!("Invalid HTTP version ({base})")), None)?;
            return Ok(Parsed::Done);
        }
        version = v.to_string();
        conn.http09 = version == "HTTP/0.9";
    }
    if !(2..=3).contains(&words.len()) {
        conn.send_error(400, Some(&format!("Bad request syntax ({})", quote_latin1(line))), None)?;
        return Ok(Parsed::Done);
    }
    let command = words[0].to_string();
    let mut path = words[1].to_string();
    if words.len() == 2 {
        conn.close = true;
        if command != "GET" {
            conn.send_error(400, Some(&format!("Bad HTTP/0.9 request type ({})", quote_latin1(&command))), None)?;
            return Ok(Parsed::Done);
        }
    }
    if path.starts_with("//") {
        path = format!("/{}", path.trim_start_matches('/'));
    }

    let mut block = Vec::new();
    let mut count = 0;
    loop {
        let line = read_line(reader, MAX_LINE + 1)?;
        if line.len() > MAX_LINE {
            conn.send_error(
                431,
                Some("Line too long"),
                Some(&format!("got more than {MAX_LINE} bytes when reading header line")),
            )?;
            return Ok(Parsed::Done);
        }
        count += 1;
        if count > MAX_HEADERS {
            conn.send_error(431, Some("Too many headers"), Some(&format!("got more than {MAX_HEADERS} headers")))?;
            return Ok(Parsed::Done);
        }
        let end = matches!(line.as_slice(), b"\r\n" | b"\n" | b"");
        block.extend_from_slice(&line);
        if end {
            break;
        }
    }
    let headers = Headers::parse(&latin1(&block));

    let conntype = headers.get("Connection").unwrap_or("");
    if conntype.eq_ignore_ascii_case("close") {
        conn.close = true;
    } else if conntype.eq_ignore_ascii_case("keep-alive") {
        conn.close = false;
    }
    let expect = headers.get("Expect").unwrap_or("");
    if expect.eq_ignore_ascii_case("100-continue") && version.as_str() >= "HTTP/1.1" {
        conn.out.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        conn.out.flush()?;
    }
    Ok(Parsed::Request { command, path, headers })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A writer that records everything and can be told to start failing.
    #[derive(Clone, Default)]
    pub struct Sink {
        pub data: Arc<Mutex<Vec<u8>>>,
        pub fail: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            self.data.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        /// The output with the Date header removed.
        pub fn text(&self) -> String {
            let s = String::from_utf8_lossy(&self.data.lock().unwrap()).into_owned();
            s.split_inclusive("\r\n").filter(|l| !l.starts_with("Date: ")).collect()
        }
    }

    pub fn writer(head_only: bool) -> (ResponseWriter, Sink) {
        let sink = Sink::default();
        let conn = ConnState { out: Box::new(sink.clone()), close: false, http09: false };
        (ResponseWriter::new(conn, head_only), sink)
    }

    pub fn finish(w: &mut ResponseWriter) {
        w.finish();
    }

    #[test]
    fn buffered_response_has_content_length() {
        let (mut w, sink) = writer(false);
        w.set_header("Content-Type", "application/json");
        w.write(b"{\"cancelled\": true}");
        w.finish();
        assert_eq!(
            sink.text(),
            format!(
                "HTTP/1.1 200 OK\r\nServer: {SERVER_NAME}\r\nContent-Type: application/json\r\nContent-Length: 19\r\n\r\n{{\"cancelled\": true}}"
            )
        );
    }

    #[test]
    fn flushed_response_is_chunked() {
        let (mut w, sink) = writer(false);
        w.write(b"abc");
        w.flush();
        w.write(b"defghijklmnopqrstu");
        w.finish();
        assert_eq!(
            sink.text(),
            format!(
                "HTTP/1.1 200 OK\r\nServer: {SERVER_NAME}\r\nContent-Type: text/plain; charset=utf-8\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n12\r\ndefghijklmnopqrstu\r\n0\r\n\r\n"
            )
        );
    }

    #[test]
    fn head_sends_no_body() {
        let (mut w, sink) = writer(true);
        w.write(b"hello");
        w.finish();
        assert!(sink.text().ends_with("Content-Length: 5\r\n\r\n"));
    }

    #[test]
    fn failed_write_calls_on_disconnect() {
        let (mut w, sink) = writer(false);
        let hit = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let h = hit.clone();
        w.on_disconnect = Some(Box::new(move || h.store(true, std::sync::atomic::Ordering::SeqCst)));
        w.write(b"x");
        w.flush();
        sink.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        w.write(b"y");
        w.flush();
        assert!(hit.load(std::sync::atomic::Ordering::SeqCst));
        assert!(w.conn.close);
    }

    #[test]
    fn error_page_html() {
        let sink = Sink::default();
        let mut c = ConnState { out: Box::new(sink.clone()), close: false, http09: false };
        c.send_error(501, Some("Unsupported method ('FOO')"), None).unwrap();
        let text = sink.text();
        assert!(text.starts_with("HTTP/1.1 501 Unsupported method ('FOO')\r\n"));
        assert!(
            text.contains("Connection: close\r\nContent-Type: text/html;charset=utf-8\r\nContent-Length: 356\r\n\r\n")
        );
        assert!(text.contains("<p>Message: Unsupported method ('FOO').</p>"));
        assert!(c.close);
    }

    #[test]
    fn headers_parse_like_feedparser() {
        let h = Headers::parse("Host: x\r\nContent-Length:  12 \r\nX-Long: a\r\n b\r\nContent-Length: 3\r\n\r\n");
        assert_eq!(h.get("content-length"), Some("12 "));
        assert_eq!(h.get("X-Long"), Some("a\r\n b"));
        // A line that isn't a header ends the block.
        let h = Headers::parse("Host: x\r\nnot a header\r\nContent-Length: 5\r\n\r\n");
        assert_eq!(h.get("Content-Length"), None);
        // A bare \r also ends a line.
        let h = Headers::parse("A: b\rContent-Length: 5\r\n\r\n");
        assert_eq!(h.get("Content-Length"), Some("5"));
    }

    #[test]
    fn parse_int_rules() {
        assert_eq!(parse_int(" 12 "), Some(12));
        assert_eq!(parse_int("+1_000"), Some(1000));
        assert_eq!(parse_int("-3"), Some(-3));
        for bad in ["", "1__0", "_1", "1_", "0x10", "1.0", "12a"] {
            assert_eq!(parse_int(bad), None, "{bad}");
        }
    }

    #[test]
    fn url_path_matches_urlsplit() {
        assert_eq!(url_path("/api/backtest?x=1#f").unwrap(), "/api/backtest");
        assert_eq!(url_path("http://host:80/api/trades.csv").unwrap(), "/api/trades.csv");
        assert_eq!(url_path("a:b").unwrap(), "b");
        assert_eq!(url_path("1a:b").unwrap(), "1a:b");
        assert_eq!(url_path("http://[::1]/x").unwrap(), "/x");
        assert!(url_path("http://[::1/x").is_err());
    }

    #[test]
    fn request_line_words_and_quoting() {
        assert_eq!(split_words("GET  /\u{1c}x HTTP/1.1"), ["GET", "/", "x", "HTTP/1.1"]);
        assert_eq!(quote_latin1("a'b\u{80}\u{e9}"), "\"a'b\\x80\u{e9}\"");
    }
}
