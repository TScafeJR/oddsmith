//! Gamma API client (events and markets metadata).

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};

use super::decode::{self, DResult, Fields, Number, Value};
use super::ratelimit::RateLimiter;
use super::{HttpClient, body_text, encode_params};
use crate::text::{lower, split_ws};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GammaTag {
    pub id: String,
    pub label: String,
    pub slug: String,
}

/// Response shape from GET /markets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GammaMarket {
    pub id: String,
    pub question: String,
    pub slug: String,
    pub description: String,
    pub condition_id: String,
    /// JSON string: `["id1","id2"]`.
    pub clob_token_ids: String,
    pub active: bool,
    pub closed: bool,
    pub volume: Number,
    pub liquidity: Number,
    pub end_date: String,
    /// JSON string: `["0.55","0.45"]`.
    pub outcome_prices: String,
    /// JSON string: `["Yes","No"]`.
    pub outcomes: String,
    pub best_bid: Number,
    pub best_ask: Number,
    pub last_trade_price: Number,
    pub spread: Number,
    pub event_id: String,
    pub tags: Vec<GammaTag>,
}

impl GammaMarket {
    /// Whether a closed market has resolved, from its outcome prices: `(resolved, "YES"/"NO")`.
    pub fn infer_resolution(&self) -> (bool, &'static str) {
        if !self.closed || self.outcome_prices.is_empty() {
            return (false, "");
        }
        match self.outcome_prices.as_str() {
            r#"["1", "0"]"# | r#"["1","0"]"# => return (true, "YES"),
            r#"["0", "1"]"# | r#"["0","1"]"# => return (true, "NO"),
            _ => {}
        }
        // Near-1/near-0 values (some markets have tiny float residuals).
        if let Ok(prices) = decode::unmarshal(&self.outcome_prices, decode::dec_str_list) {
            if prices.len() == 2 {
                let p0 = decode::parse_float_strict(&prices[0]);
                let p1 = decode::parse_float_strict(&prices[1]);
                if p0 > 0.99 && p1 < 0.01 {
                    return (true, "YES");
                }
                if p1 > 0.99 && p0 < 0.01 {
                    return (true, "NO");
                }
            }
        }
        (false, "")
    }

    /// Parse the JSON string of token IDs into `(yes_token_id, no_token_id)`.
    pub fn parse_clob_token_ids(&self) -> Result<(String, String)> {
        let ids = decode::unmarshal(&self.clob_token_ids, decode::dec_str_list)
            .map_err(|e| anyhow!("parsing clobTokenIds: {e}"))?;
        if ids.len() < 2 {
            bail!("expected 2 CLOB token IDs, got {}", ids.len());
        }
        let mut it = ids.into_iter();
        Ok((it.next().unwrap_or_default(), it.next().unwrap_or_default()))
    }
}

/// Response shape from GET /events.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GammaEvent {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub description: String,
    pub start_date: String,
    pub end_date: String,
    pub active: bool,
    pub closed: bool,
    pub volume: Number,
    pub liquidity: Number,
    pub tags: Vec<GammaTag>,
    pub markets: Vec<GammaMarket>,
}

pub fn dec_gamma_tag(v: Option<&Value>, path: &str) -> DResult<GammaTag> {
    let Some(f) = Fields::new(v, path, "GammaTag")? else { return Ok(GammaTag::default()) };
    Ok(GammaTag { id: f.str("id")?, label: f.str("label")?, slug: f.str("slug")? })
}

pub fn dec_gamma_market(v: Option<&Value>, path: &str) -> DResult<GammaMarket> {
    let Some(f) = Fields::new(v, path, "GammaMarket")? else { return Ok(GammaMarket::default()) };
    Ok(GammaMarket {
        id: f.str("id")?,
        question: f.str("question")?,
        slug: f.str("slug")?,
        description: f.str("description")?,
        condition_id: f.str("conditionId")?,
        clob_token_ids: f.str("clobTokenIds")?,
        active: f.bool("active")?,
        closed: f.bool("closed")?,
        volume: f.number("volume")?,
        liquidity: f.number("liquidity")?,
        end_date: f.str("endDate")?,
        outcome_prices: f.str("outcomePrices")?,
        outcomes: f.str("outcomes")?,
        best_bid: f.number("bestBid")?,
        best_ask: f.number("bestAsk")?,
        last_trade_price: f.number("lastTradePrice")?,
        spread: f.number("spread")?,
        event_id: f.str("event_id")?,
        tags: f.list("tags", "[]GammaTag", dec_gamma_tag)?,
    })
}

pub fn dec_gamma_event(v: Option<&Value>, path: &str) -> DResult<GammaEvent> {
    let Some(f) = Fields::new(v, path, "GammaEvent")? else { return Ok(GammaEvent::default()) };
    Ok(GammaEvent {
        id: f.str("id")?,
        title: f.str("title")?,
        slug: f.str("slug")?,
        description: f.str("description")?,
        start_date: f.str("startDate")?,
        end_date: f.str("endDate")?,
        active: f.bool("active")?,
        closed: f.bool("closed")?,
        volume: f.number("volume")?,
        liquidity: f.number("liquidity")?,
        tags: f.list("tags", "[]GammaTag", dec_gamma_tag)?,
        markets: f.list("markets", "[]GammaMarket", dec_gamma_market)?,
    })
}

/// Decode a GET /events body.
pub fn decode_events(body: &[u8]) -> DResult<Vec<GammaEvent>> {
    decode::decode_body(body, |v, p| decode::dec_list(v, p, "[]GammaEvent", dec_gamma_event))
}

/// Decode a GET /markets body.
pub fn decode_markets(body: &[u8]) -> DResult<Vec<GammaMarket>> {
    decode::decode_body(body, |v, p| decode::dec_list(v, p, "[]GammaMarket", dec_gamma_market))
}

pub struct GammaClient {
    pub host: String,
    http: HttpClient,
    rl: Arc<RateLimiter>,
}

impl GammaClient {
    pub fn new(host: &str, rl: Arc<RateLimiter>) -> GammaClient {
        GammaClient { host: host.to_string(), http: HttpClient::new(30), rl }
    }

    /// Fetch events filtered by tag (e.g. "politics", "sports"), highest volume first.
    pub fn fetch_events(&self, tag: &str, limit: i64, offset: i64) -> Result<Vec<GammaEvent>> {
        self.rl.wait_gamma()?;
        let params = [
            ("tag", tag.to_string()),
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
            ("order", "volume".into()),
            ("ascending", "false".into()),
        ];
        let url = format!("{}/events?{}", self.host, encode_params(&params));
        let (status, body) = self.http.get(&url).map_err(|e| anyhow!("fetching events: {e}"))?;
        if status != 200 {
            bail!("events API returned {status}: {}", body_text(&body));
        }
        decode_events(&body).map_err(|e| anyhow!("decoding events: {e}"))
    }

    /// Fetch markets, optionally for one event.
    pub fn fetch_markets(&self, event_id: &str, limit: i64, offset: i64) -> Result<Vec<GammaMarket>> {
        self.rl.wait_gamma()?;
        let mut params = Vec::new();
        if !event_id.is_empty() {
            params.push(("event_id", event_id.to_string()));
        }
        params.push(("limit", limit.to_string()));
        params.push(("offset", offset.to_string()));
        let url = format!("{}/markets?{}", self.host, encode_params(&params));
        let (status, body) = self.http.get(&url).map_err(|e| anyhow!("fetching markets: {e}"))?;
        if status != 200 {
            bail!("markets API returned {status}: {}", body_text(&body));
        }
        decode_markets(&body).map_err(|e| anyhow!("decoding markets: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Metadata extraction
// ---------------------------------------------------------------------------

/// Infer the market type from event/market metadata, for politics (presidential, senate,
/// house, governor, policy) and sports (nba, nfl, mlb, nhl, soccer, mma, tennis, other_sport).
pub fn extract_market_type(domain: &str, question: &str, tags: &[GammaTag]) -> &'static str {
    let q = lower(question);
    let has = |s: &str, kws: &[&str]| kws.iter().any(|k| s.contains(k));

    // Tags first (most reliable).
    for tag in tags {
        let t = lower(&tag.slug);
        if domain == "politics" {
            if has(&t, &["presidential", "president"]) {
                return "presidential";
            }
            if t.contains("senate") {
                return "senate";
            }
            if has(&t, &["house", "congress"]) {
                return "house";
            }
            if t.contains("governor") {
                return "governor";
            }
        }
        if domain == "sports" {
            if has(&t, &["nba", "basketball"]) {
                return "nba";
            }
            if has(&t, &["nfl", "football"]) {
                return "nfl";
            }
            if has(&t, &["mlb", "baseball"]) {
                return "mlb";
            }
            if has(&t, &["nhl", "hockey"]) {
                return "nhl";
            }
            if has(&t, &["soccer", "epl", "mls"]) {
                return "soccer";
            }
            if has(&t, &["ufc", "mma"]) {
                return "mma";
            }
            if t.contains("tennis") {
                return "tennis";
            }
        }
    }

    // Fall back to the question text.
    if domain == "sports" {
        if has(&q, &["nba", "basketball"]) {
            return "nba";
        }
        if has(&q, &["nfl", "super bowl", "football"]) {
            return "nfl";
        }
        if has(&q, &["mlb", "world series", "baseball"]) {
            return "mlb";
        }
        if has(&q, &["nhl", "stanley cup", "hockey"]) {
            return "nhl";
        }
        if has(&q, &["ufc", "mma"]) {
            return "mma";
        }
        return "other_sport";
    }

    if has(&q, &["president", "presidential", "white house"]) {
        return "presidential";
    }
    if has(&q, &["senate", "senator"]) {
        return "senate";
    }
    if has(&q, &["house", "congress", "representative"]) {
        return "house";
    }
    if q.contains("governor") {
        return "governor";
    }
    "policy"
}

/// US state names and abbreviations. The order breaks ties between equally long names in
/// [`states_longest_first`].
pub const STATE_ABBREVS: &[(&str, &str)] = &[
    ("alabama", "AL"),
    ("alaska", "AK"),
    ("arizona", "AZ"),
    ("arkansas", "AR"),
    ("california", "CA"),
    ("colorado", "CO"),
    ("connecticut", "CT"),
    ("delaware", "DE"),
    ("florida", "FL"),
    ("georgia", "GA"),
    ("hawaii", "HI"),
    ("idaho", "ID"),
    ("illinois", "IL"),
    ("indiana", "IN"),
    ("iowa", "IA"),
    ("kansas", "KS"),
    ("kentucky", "KY"),
    ("louisiana", "LA"),
    ("maine", "ME"),
    ("maryland", "MD"),
    ("massachusetts", "MA"),
    ("michigan", "MI"),
    ("minnesota", "MN"),
    ("mississippi", "MS"),
    ("missouri", "MO"),
    ("montana", "MT"),
    ("nebraska", "NE"),
    ("nevada", "NV"),
    ("new hampshire", "NH"),
    ("new jersey", "NJ"),
    ("new mexico", "NM"),
    ("new york", "NY"),
    ("north carolina", "NC"),
    ("north dakota", "ND"),
    ("ohio", "OH"),
    ("oklahoma", "OK"),
    ("oregon", "OR"),
    ("pennsylvania", "PA"),
    ("rhode island", "RI"),
    ("south carolina", "SC"),
    ("south dakota", "SD"),
    ("tennessee", "TN"),
    ("texas", "TX"),
    ("utah", "UT"),
    ("vermont", "VT"),
    ("virginia", "VA"),
    ("washington", "WA"),
    ("west virginia", "WV"),
    ("wisconsin", "WI"),
    ("wyoming", "WY"),
];

/// State names, longest first (stable), so "west virginia" wins over "virginia" and
/// "arkansas" over "kansas".
pub fn states_longest_first() -> Vec<(&'static str, &'static str)> {
    let mut v = STATE_ABBREVS.to_vec();
    v.sort_by_key(|(name, _)| std::cmp::Reverse(name.chars().count()));
    v
}

/// Find a US state mentioned in the question (full name, or a 2-letter uppercase abbreviation).
pub fn extract_state(question: &str) -> String {
    let q = lower(question);
    for (name, abbrev) in states_longest_first() {
        if q.contains(name) {
            return abbrev.to_string();
        }
    }
    for w in split_ws(question) {
        let w = w.trim_matches(['?', ',', '.']);
        if w.len() == 2 && w == w.to_uppercase() && STATE_ABBREVS.iter().any(|(_, a)| *a == w) {
            return w.to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENTS: &str = r#"[
      {"id": "903", "title": "Presidential Election Winner 2024", "slug": "pres-2024",
       "description": "d", "startDate": "2024-01-04T22:58:00Z", "endDate": "2024-11-05T12:00:00Z",
       "active": true, "closed": true, "volume": 3686335059.29, "liquidity": "1234.5",
       "tags": [{"id": "2", "label": "Politics", "slug": "politics"}, null],
       "markets": [
         {"id": "253591", "question": "Will Donald Trump win the 2024 US Presidential Election?",
          "conditionId": "0xdd22", "clobTokenIds": "[\"111\", \"222\"]", "active": true, "closed": true,
          "volume": "1531479285.12", "outcomePrices": "[\"1\", \"0\"]", "outcomes": "[\"Yes\", \"No\"]",
          "bestBid": null, "lastTradePrice": 0.999, "spread": "0.001", "endDate": "2024-11-05T12:00:00Z",
          "extra": {"nested": [1, 2]}}
       ]}
    ] trailing garbage"#;

    #[test]
    fn decodes_events_and_markets() {
        let events = decode_events(EVENTS.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.id, "903");
        assert_eq!(e.volume.as_str(), "3686335059.29");
        assert_eq!(e.liquidity.as_str(), "1234.5");
        assert_eq!(e.tags.len(), 2);
        assert_eq!(e.tags[1], GammaTag::default());
        let m = &e.markets[0];
        assert_eq!(m.condition_id, "0xdd22");
        assert_eq!(m.best_bid, Number::default());
        assert_eq!(m.last_trade_price.float64(), 0.999);
        assert_eq!(m.parse_clob_token_ids().unwrap(), ("111".into(), "222".into()));
        assert_eq!(m.infer_resolution(), (true, "YES"));
    }

    #[test]
    fn decode_errors_name_the_field() {
        let err = |s: &str| decode_events(s.as_bytes()).unwrap_err().0;
        assert_eq!(err(r#"[{"id": 1}]"#), r#"invalid value for field "GammaEvent.id": expected string, got number 1"#);
        assert_eq!(err(r#"{"a":1}"#), "invalid value: expected list of GammaEvent, got object");
        assert_eq!(
            err(r#"[{"tags": {}}]"#),
            r#"invalid value for field "GammaEvent.tags": expected list of GammaTag, got object"#
        );
        assert_eq!(
            err(r#"[{"tags": [1]}]"#),
            r#"invalid value for field "GammaEvent.tags": expected GammaTag, got number 1"#
        );
        assert_eq!(
            err(r#"[{"markets": [{"conditionId": 5}]}]"#),
            r#"invalid value for field "GammaMarket.conditionId": expected string, got number 5"#
        );
        assert_eq!(
            err(r#"[{"id": "1", "volume": "abc"}]"#),
            r#"json: invalid number literal, trying to unmarshal "abc" into Number"#
        );
        assert_eq!(err("[1]\n  x"), "invalid value: expected GammaEvent, got number 1");
        assert_eq!(err("true false"), "invalid value: expected list of GammaEvent, got bool");
        let markets = decode_markets(br#"[{"id":"1","tags":null}]"#).unwrap();
        assert_eq!(markets[0].id, "1");
    }

    #[test]
    fn infers_resolution() {
        let m = |closed: bool, p: &str| GammaMarket { closed, outcome_prices: p.into(), ..Default::default() };
        assert_eq!(m(false, r#"["1","0"]"#).infer_resolution(), (false, ""));
        assert_eq!(m(true, "").infer_resolution(), (false, ""));
        assert_eq!(m(true, r#"["0", "1"]"#).infer_resolution(), (true, "NO"));
        assert_eq!(m(true, r#"["0.995", "0.005"]"#).infer_resolution(), (true, "YES"));
        assert_eq!(m(true, r#"["0.0001","0.9999"]"#).infer_resolution(), (true, "NO"));
        assert_eq!(m(true, r#"["0.5","0.5"]"#).infer_resolution(), (false, ""));
        assert_eq!(m(true, r#"[1, 0]"#).infer_resolution(), (false, ""));
        assert_eq!(m(true, "garbage").infer_resolution(), (false, ""));
    }

    #[test]
    fn token_id_errors() {
        let m = |s: &str| GammaMarket { clob_token_ids: s.into(), ..Default::default() };
        assert_eq!(m(r#"["a"]"#).parse_clob_token_ids().unwrap_err().to_string(), "expected 2 CLOB token IDs, got 1");
        assert_eq!(
            m("").parse_clob_token_ids().unwrap_err().to_string(),
            "parsing clobTokenIds: unexpected end of JSON input"
        );
        assert_eq!(m(r#"["a","b","c"]"#).parse_clob_token_ids().unwrap(), ("a".into(), "b".into()));
    }

    #[test]
    fn market_types() {
        let tag = |s: &str| GammaTag { slug: s.into(), ..Default::default() };
        assert_eq!(extract_market_type("politics", "x", &[tag("us-presidential-election")]), "presidential");
        assert_eq!(extract_market_type("politics", "Who wins the Senate?", &[tag("politics")]), "senate");
        assert_eq!(extract_market_type("politics", "Will the House flip?", &[]), "house");
        assert_eq!(extract_market_type("politics", "Fed rate cut?", &[]), "policy");
        assert_eq!(extract_market_type("sports", "x", &[tag("NBA")]), "nba");
        assert_eq!(extract_market_type("sports", "Stanley Cup winner?", &[]), "nhl");
        assert_eq!(extract_market_type("sports", "Chess?", &[]), "other_sport");
    }

    #[test]
    fn states_prefer_longest_names() {
        assert_eq!(extract_state("Will the GOP win West Virginia?"), "WV");
        assert_eq!(extract_state("Virginia senate race"), "VA");
        assert_eq!(extract_state("Who wins Arkansas?"), "AR");
        assert_eq!(extract_state("Kansas governor"), "KS");
        assert_eq!(extract_state("Will Trump win PA?"), "PA");
        assert_eq!(extract_state("Will Trump win pa?"), "");
        assert_eq!(extract_state("Who will be the next president?"), "");
        let order = states_longest_first();
        let pos = |n: &str| order.iter().position(|(s, _)| *s == n).unwrap();
        assert!(pos("west virginia") < pos("virginia"));
        assert!(pos("arkansas") < pos("kansas"));
    }
}
