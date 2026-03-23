//! Polymarket API clients: Gamma (events/markets), CLOB (order books, price history) and the
//! public Data API (trades), plus the shared rate limiter and JSON decoding toolkit.

pub mod clob;
pub mod decode;
pub mod gamma;
pub mod ratelimit;

use std::time::Duration;

use anyhow::Result;
use ureq::Agent;

/// A blocking HTTP client. Non-2xx responses come back as `(status, body)`, not errors.
#[derive(Clone)]
pub struct HttpClient {
    agent: Agent,
}

impl HttpClient {
    pub fn new(timeout_secs: u64) -> HttpClient {
        HttpClient { agent: oddsmith_store::http::agent(Duration::from_secs(timeout_secs)) }
    }

    pub fn get(&self, url: &str) -> Result<(u16, Vec<u8>)> {
        let resp = self.agent.get(url).call()?;
        read(resp)
    }

    pub fn post_json(&self, url: &str, body: &[u8]) -> Result<(u16, Vec<u8>)> {
        let resp = self.agent.post(url).header("Content-Type", "application/json").send(body)?;
        read(resp)
    }
}

fn read(mut resp: ureq::http::Response<ureq::Body>) -> Result<(u16, Vec<u8>)> {
    let status = resp.status().as_u16();
    let body = resp.body_mut().with_config().limit(u64::MAX).read_to_vec()?;
    Ok((status, body))
}

/// Response body as text, with invalid UTF-8 replaced.
pub fn body_text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).into_owned()
}

/// Query string with keys sorted and keys and values form-encoded with [`quote_plus`].
pub fn encode_params(params: &[(&str, String)]) -> String {
    let mut sorted: Vec<_> = params.iter().collect();
    sorted.sort();
    sorted.iter().map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v))).collect::<Vec<_>>().join("&")
}

/// Form-encode a string: ASCII letters, digits and `_.-~` as is, space as `+`, every other
/// byte as `%XX`.
pub fn quote_plus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_are_sorted_and_escaped() {
        let p = [("tag", "politics".to_string()), ("limit", "100".into()), ("ascending", "false".into())];
        assert_eq!(encode_params(&p), "ascending=false&limit=100&tag=politics");
        assert_eq!(quote_plus("a b/c&d=é~"), "a+b%2Fc%26d%3D%C3%A9~");
    }
}
