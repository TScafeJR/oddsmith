//! Poll CSV parsing (FiveThirtyEight and generic formats).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Result, anyhow};
use chrono::{DateTime, FixedOffset, NaiveDate};
use oddsmith_store::polls::Poll;

use super::csv::read_records;
use crate::text::{lower, parse_rfc3339, strip_ws};

type Columns = HashMap<String, usize>;

fn read_csv(path: &Path) -> Result<(Vec<String>, Vec<Vec<String>>)> {
    let bytes = fs::read(path).map_err(|e| anyhow!("opening {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let mut records = read_records(&text)?.into_iter().filter(|r| !r.is_empty());
    let header = records.next().ok_or_else(|| anyhow!("reading header: EOF"))?;
    Ok((header, records.collect()))
}

/// Read a FiveThirtyEight polling CSV: the wide format (`cand1_name`/`cand1_pct`,
/// `cand2_name`/`cand2_pct`) from raw_polls.csv, or the long format (`candidate_name`, `pct`).
pub fn parse_538_csv(path: &Path) -> Result<Vec<Poll>> {
    let (header, records) = read_csv(path)?;
    let cols = map_columns(&header);
    let is_wide = cols.contains_key("cand1_name");

    let mut polls = Vec::new();
    for record in &records {
        // Skip rows whose field count differs from the header.
        if record.len() != header.len() {
            continue;
        }
        let f = |names: &[&str]| get_field(record, &cols, names);

        if is_wide {
            let Some(poll_date) = parse_flex_date(&f(&["polldate", "end_date", "date"])) else { continue };
            let pollster = f(&["pollster"]);
            let methodology = f(&["methodology"]);
            let state = normalize_state(&f(&["location", "state"]));
            let race_type = parse_538_race_type(&f(&["type_simple", "race"]));
            let sample_size = atoi(&f(&["samplesize", "sample_size"]));

            for prefix in ["cand1", "cand2"] {
                let name = f(&[&format!("{prefix}_name")]);
                if name.is_empty() {
                    continue;
                }
                let Some(pct) = parse_float(&f(&[&format!("{prefix}_pct")])) else { continue };
                if pct <= 0.0 {
                    continue;
                }
                let party = normalize_party(&f(&[&format!("{prefix}_party")]));
                polls.push(Poll {
                    source: "538".into(),
                    race_type: race_type.into(),
                    state: state.clone(),
                    candidate: name,
                    party,
                    poll_pct: pct,
                    implied_prob: pct / 100.0,
                    pollster: pollster.clone(),
                    sample_size,
                    methodology: methodology.clone(),
                    poll_date: Some(poll_date),
                    ..Default::default()
                });
            }
        } else {
            let candidate = f(&["candidate_name", "candidate", "answer"]);
            if candidate.is_empty() {
                continue;
            }
            let Some(pct) = parse_float(&f(&["pct", "percentage", "value", "poll_pct"])) else { continue };
            if pct <= 0.0 {
                continue;
            }
            let Some(poll_date) = parse_flex_date(&f(&["end_date", "date", "poll_date", "modeldate"])) else {
                continue;
            };
            polls.push(Poll {
                source: "538".into(),
                race_type: parse_538_race_type(&f(&["office_type", "race_type", "type_simple", "type"])).into(),
                state: normalize_state(&f(&["state", "location"])),
                candidate,
                party: normalize_party(&f(&["party", "candidate_party"])),
                poll_pct: pct,
                implied_prob: pct / 100.0,
                pollster: f(&["pollster", "pollster_name"]),
                sample_size: atoi(&f(&["sample_size", "samplesize", "n"])),
                methodology: f(&["methodology", "method"]),
                poll_date: Some(poll_date),
                ..Default::default()
            });
        }
    }
    Ok(polls)
}

/// Normalize 538's race type codes.
pub fn parse_538_race_type(s: &str) -> &'static str {
    let t = lower(strip_ws(s));
    if t.contains("pres") {
        "presidential"
    } else if t.contains("sen") {
        "senate"
    } else if t.contains("house") || t.contains("rep") {
        "house"
    } else if t.contains("gov") {
        "governor"
    } else {
        "presidential"
    }
}

/// Read a generic polling CSV with columns
/// `source,race_type,state,candidate,party,poll_pct,pollster,sample_size,date`.
pub fn parse_generic_csv(path: &Path, source: &str) -> Result<Vec<Poll>> {
    let (header, records) = read_csv(path)?;
    let cols = map_columns(&header);

    let mut polls = Vec::new();
    for record in &records {
        if record.len() != header.len() {
            continue;
        }
        let f = |names: &[&str]| get_field(record, &cols, names);
        let candidate = f(&["candidate", "candidate_name"]);
        let pct = parse_float(&f(&["poll_pct", "pct", "percentage"])).unwrap_or(0.0);
        if candidate.is_empty() || pct <= 0.0 {
            continue;
        }
        let Some(poll_date) = parse_flex_date(&f(&["date", "poll_date", "end_date"])) else { continue };
        let mut src = f(&["source"]);
        if src.is_empty() {
            src = source.to_string();
        }
        polls.push(Poll {
            source: src,
            race_type: f(&["race_type"]),
            state: normalize_state(&f(&["state"])),
            candidate,
            party: normalize_party(&f(&["party"])),
            poll_pct: pct,
            implied_prob: pct / 100.0,
            pollster: f(&["pollster"]),
            sample_size: atoi(&f(&["sample_size"])),
            methodology: f(&["methodology"]),
            poll_date: Some(poll_date),
            ..Default::default()
        });
    }
    Ok(polls)
}

/// Read every `.csv` file in a directory (name order), printing a line per file. Files that
/// fail to parse are skipped with a warning.
pub fn parse_directory(dir: &Path, source: &str) -> Result<Vec<Poll>> {
    let read_err = |e: std::io::Error| anyhow!("reading directory {}: {e}", dir.display());
    let mut entries: Vec<_> = fs::read_dir(dir).map_err(read_err)?.collect::<std::io::Result<_>>().map_err(read_err)?;
    entries.sort_by_key(|e| e.file_name());

    let mut all = Vec::new();
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
        if is_dir || !lower(&name).ends_with(".csv") {
            continue;
        }
        let path = dir.join(&name);
        let parsed = if source == "538" { parse_538_csv(&path) } else { parse_generic_csv(&path, source) };
        match parsed {
            Ok(polls) => {
                println!("  parsed {name}: {} polls", polls.len());
                all.extend(polls);
            }
            Err(err) => println!("  warning: skipping {name}: {err:#}"),
        }
    }
    Ok(all)
}

fn map_columns(header: &[String]) -> Columns {
    let mut cols = HashMap::new();
    for (i, h) in header.iter().enumerate() {
        cols.insert(lower(strip_ws(h)), i);
    }
    cols
}

fn get_field(record: &[String], cols: &Columns, names: &[&str]) -> String {
    for name in names {
        if let Some(&i) = cols.get(*name) {
            if i < record.len() {
                return strip_ws(&record[i]).to_string();
            }
        }
    }
    String::new()
}

/// A decimal integer (`[+-]?[0-9]+`); `None` for empty, invalid or out-of-range input.
pub fn atoi(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// A decimal float (inf/nan spellings allowed); `None` on error or overflow.
pub fn parse_float(s: &str) -> Option<f64> {
    let v = crate::polymarket::decode::parse_float(s)?;
    if v.is_infinite() && !s.to_lowercase().contains('n') {
        return None; // out of range (overflowed to infinity)
    }
    Some(v)
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

fn utc_date(y: i32, m: u32, d: u32) -> Option<DateTime<FixedOffset>> {
    NaiveDate::from_ymd_opt(y, m, d)?.and_hms_opt(0, 0, 0)?.and_local_timezone(FixedOffset::east_opt(0)?).single()
}

fn all_digits(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

/// Try each supported date layout in order: `YYYY-MM-DD`, `M/D/YYYY`, `Jan 2, 2006`,
/// `January 2, 2006`, then RFC 3339. `None` if none match.
pub fn parse_flex_date(s: &str) -> Option<DateTime<FixedOffset>> {
    // YYYY-MM-DD
    let parts: Vec<&str> = s.split('-').collect();
    if let [y, m, d] = parts[..] {
        if all_digits(y, 4, 4) && all_digits(m, 2, 2) && all_digits(d, 2, 2) {
            if let Some(t) = utc_date(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?) {
                return Some(t);
            }
        }
    }
    // M/D/YYYY, with or without leading zeros
    let parts: Vec<&str> = s.split('/').collect();
    if let [m, d, y] = parts[..] {
        if all_digits(m, 1, 2) && all_digits(d, 1, 2) && all_digits(y, 4, 4) {
            if let Some(t) = utc_date(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?) {
                return Some(t);
            }
        }
    }
    // "Jan 2, 2006" or "January 2, 2006"
    if let Some((name, rest)) = s.split_once(' ') {
        if let Some((d, y)) = rest.split_once(", ") {
            if !name.is_empty()
                && name.bytes().all(|b| b.is_ascii_alphabetic())
                && all_digits(d, 1, 2)
                && all_digits(y, 4, 4)
            {
                let name = name.to_ascii_lowercase();
                let (y, d): (i32, u32) = (y.parse().ok()?, d.parse().ok()?);
                if let Some(i) = MONTHS.iter().position(|m| m[..3] == name) {
                    if let Some(t) = utc_date(y, i as u32 + 1, d) {
                        return Some(t);
                    }
                }
                if let Some(i) = MONTHS.iter().position(|m| *m == name) {
                    if let Some(t) = utc_date(y, i as u32 + 1, d) {
                        return Some(t);
                    }
                }
            }
        }
    }
    parse_rfc3339(s)
}

pub fn normalize_party(p: &str) -> String {
    match strip_ws(p).to_uppercase().as_str() {
        "DEM" | "DEMOCRAT" | "DEMOCRATIC" | "D" => "D".into(),
        "REP" | "REPUBLICAN" | "R" => "R".into(),
        "IND" | "INDEPENDENT" | "I" => "I".into(),
        "LIB" | "LIBERTARIAN" => "L".into(),
        "GRN" | "GREEN" => "G".into(),
        _ => p.to_string(),
    }
}

pub fn normalize_state(s: &str) -> String {
    let s = strip_ws(s);
    if s.len() == 2 {
        return s.to_uppercase();
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn write_csv(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        fs::File::create(&p).unwrap().write_all(body.as_bytes()).unwrap();
        p
    }

    fn date(p: &Poll) -> String {
        p.poll_date.unwrap().format("%Y-%m-%d").to_string()
    }

    #[test]
    fn wide_format() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_csv(
            dir.path(),
            "raw.csv",
            "poll_id,location,type_simple,pollster,methodology,polldate,samplesize,cand1_name,cand1_party,cand1_pct,cand2_name,cand2_party,cand2_pct\n\
             1,ne,Sen-G,YouGov,Online Panel,2014-09-26,721,Dave Domina,DEM,31,Ben Sasse,REP,58\n\
             2,US,Pres-G,Ipsos,Live Phone,10/3/2020,,Joe Biden,DEM,51.5,Donald Trump,REP,0\n\
             3,PA,Gov-G,X,,\"Nov 2, 2022\",abc,A,LIB,40,,GRN,30\n\
             4,PA,Gov-G,X,,bad date,1,A,DEM,40,B,REP,30\n\
             5,short,row\n\
             \n\
             6,TX,House-G,Y,IVR,2022-02-30,1,A,DEM,40,B,REP,30\n",
        );
        let polls = parse_538_csv(&p).unwrap();
        assert_eq!(polls.len(), 4, "{polls:#?}");
        let a = &polls[0];
        assert_eq!(
            (a.race_type.as_str(), a.state.as_str(), a.candidate.as_str(), a.party.as_str()),
            ("senate", "NE", "Dave Domina", "D")
        );
        assert_eq!((a.poll_pct, a.implied_prob, a.sample_size), (31.0, 0.31, Some(721)));
        assert_eq!(date(a), "2014-09-26");
        assert_eq!(polls[1].candidate, "Ben Sasse");
        assert_eq!(polls[1].party, "R");
        let b = &polls[2];
        assert_eq!((b.race_type.as_str(), b.candidate.as_str(), b.sample_size), ("presidential", "Joe Biden", None));
        assert_eq!(date(b), "2020-10-03");
        assert_eq!(b.methodology, "Live Phone");
        let c = &polls[3];
        assert_eq!((c.party.as_str(), c.race_type.as_str(), c.sample_size), ("L", "governor", None));
        assert_eq!(date(c), "2022-11-02");
    }

    #[test]
    fn long_format() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_csv(
            dir.path(),
            "long.csv",
            "Candidate_Name , pct, end_date, party, pollster, state, office_type, sample_size\n\
             Kamala Harris,48.2,2024-07-30,Democrat,Ipsos,Pennsylvania,U.S. President,1200\n\
             Donald Trump,47,7/30/2024,rep,Ipsos,pa,U.S. President,1200\n\
             ,47,7/30/2024,rep,Ipsos,pa,U.S. President,1200\n\
             Nobody,-1,7/30/2024,rep,Ipsos,pa,U.S. President,1200\n\
             Jill Stein,nan,2024-07-30T12:00:00-04:00,Green,Ipsos,,U.S. Senate,\n\
             Big,1e999,2024-07-30,G,Ipsos,,Senate,\n",
        );
        let polls = parse_538_csv(&p).unwrap();
        assert_eq!(polls.len(), 3, "{polls:#?}");
        assert_eq!((polls[0].party.as_str(), polls[0].state.as_str()), ("D", "Pennsylvania"));
        assert_eq!((polls[1].party.as_str(), polls[1].state.as_str()), ("R", "PA"));
        assert_eq!(polls[0].race_type, "presidential");
        assert!(polls[2].poll_pct.is_nan());
        assert_eq!((polls[2].party.as_str(), polls[2].race_type.as_str()), ("G", "senate"));
        assert_eq!(polls[2].poll_date.unwrap().to_rfc3339(), "2024-07-30T12:00:00-04:00");
    }

    #[test]
    fn directory_skips_bad_files_and_non_csv() {
        let dir = tempfile::tempdir().unwrap();
        write_csv(dir.path(), "b.CSV", "candidate,pct,date\nA,50,2024-01-01\n");
        write_csv(dir.path(), "a.csv", "");
        write_csv(dir.path(), "notes.txt", "candidate,pct,date\nA,50,2024-01-01\n");
        fs::create_dir(dir.path().join("sub.csv")).unwrap();
        let polls = parse_directory(dir.path(), "538").unwrap();
        assert_eq!(polls.len(), 1);
        assert!(parse_directory(&dir.path().join("missing"), "538").is_err());
        let err = parse_538_csv(&dir.path().join("a.csv")).unwrap_err().to_string();
        assert_eq!(err, "reading header: EOF");
    }

    #[test]
    fn generic_format() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_csv(
            dir.path(),
            "g.csv",
            "source,race_type,state,candidate,party,poll_pct,pollster,sample_size,date\n\
             ,senate,oh,Sherrod Brown,dem,45,Marist,800,2024-10-01\n\
             rcp,senate,OH,Bernie Moreno,REP,46,Marist,x,\"October 1, 2024\"\n\
             rcp,senate,OH,Zero,REP,0,Marist,1,2024-10-01\n",
        );
        let polls = parse_generic_csv(&p, "manual").unwrap();
        assert_eq!(polls.len(), 2, "{polls:#?}");
        assert_eq!((polls[1].source.as_str(), polls[1].sample_size), ("rcp", None));
        assert_eq!((polls[0].source.as_str(), polls[0].state.as_str(), polls[0].party.as_str()), ("manual", "OH", "D"));
    }

    #[test]
    fn flex_dates() {
        let d = |s: &str| parse_flex_date(s).map(|t| t.to_rfc3339());
        assert_eq!(d("2024-01-05").as_deref(), Some("2024-01-05T00:00:00+00:00"));
        assert_eq!(d("1/5/2024").as_deref(), Some("2024-01-05T00:00:00+00:00"));
        assert_eq!(d("01/05/2024").as_deref(), Some("2024-01-05T00:00:00+00:00"));
        assert_eq!(d("Jan 5, 2024").as_deref(), Some("2024-01-05T00:00:00+00:00"));
        assert_eq!(d("may 5, 2024").as_deref(), Some("2024-05-05T00:00:00+00:00"));
        assert_eq!(d("September 30, 2024").as_deref(), Some("2024-09-30T00:00:00+00:00"));
        assert_eq!(d("2024-01-05T10:00:00.5+01:00").as_deref(), Some("2024-01-05T10:00:00.500+01:00"));
        assert_eq!(d("Sept 5, 2024"), None);
        assert_eq!(d("1/5/24"), None);
        assert_eq!(d("2024-13-01"), None);
        assert_eq!(d("2024-1-05"), None);
        assert_eq!(d(""), None);
    }

    #[test]
    fn numbers_and_normalizers() {
        assert_eq!(atoi("+12"), Some(12));
        assert_eq!(atoi("1.0"), None);
        assert_eq!(atoi(""), None);
        assert_eq!(parse_float("45.5"), Some(45.5));
        assert_eq!(parse_float("1e999"), None);
        assert_eq!(parse_float("-inf"), Some(f64::NEG_INFINITY));
        assert_eq!(parse_float("NA"), None);
        assert_eq!(parse_538_race_type(" Pres-P "), "presidential");
        assert_eq!(parse_538_race_type("House-G"), "house");
        assert_eq!(parse_538_race_type("Gov-G"), "governor");
        assert_eq!(parse_538_race_type("other"), "presidential");
        assert_eq!(normalize_party("Democratic"), "D");
        assert_eq!(normalize_party("Working Families"), "Working Families");
        assert_eq!(normalize_state(" ny "), "NY");
        assert_eq!(normalize_state("M-1"), "M-1");
    }
}
