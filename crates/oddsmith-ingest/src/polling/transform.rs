//! Poll percentage to probability normalization.

use std::collections::HashMap;

use oddsmith_store::polls::Poll;

/// Convert raw poll percentages to probabilities, in place.
///
/// Polls are grouped by `(pollster, poll date, race type, state)`; each poll's probability is
/// its share of the group's total percentage. Groups whose total is not positive keep their
/// existing values (`pct / 100` from parsing).
pub fn compute_implied_probabilities(polls: &mut [Poll]) {
    let mut order: Vec<(String, String, String, String)> = Vec::new();
    let mut groups: HashMap<(String, String, String, String), Vec<usize>> = HashMap::new();
    for (i, p) in polls.iter().enumerate() {
        let date = match p.poll_date {
            Some(d) => d.format("%Y-%m-%d").to_string(),
            None => "0001-01-01".to_string(),
        };
        let key = (p.pollster.clone(), date, p.race_type.clone(), p.state.clone());
        groups
            .entry(key.clone())
            .or_insert_with(|| {
                order.push(key);
                Vec::new()
            })
            .push(i);
    }

    for key in &order {
        let indices = &groups[key];
        let mut total = 0.0;
        for &i in indices {
            total += polls[i].poll_pct;
        }
        if total <= 0.0 {
            continue;
        }
        for &i in indices {
            polls[i].implied_prob = polls[i].poll_pct / total;
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, TimeZone};

    use super::*;

    fn poll(pollster: &str, day: u32, state: &str, pct: f64) -> Poll {
        let tz = FixedOffset::east_opt(0).unwrap();
        Poll {
            pollster: pollster.into(),
            race_type: "senate".into(),
            state: state.into(),
            poll_pct: pct,
            implied_prob: pct / 100.0,
            poll_date: Some(tz.with_ymd_and_hms(2024, 10, day, 0, 0, 0).unwrap()),
            ..Default::default()
        }
    }

    #[test]
    fn normalizes_within_groups() {
        let mut polls = vec![
            poll("A", 1, "PA", 45.0),
            poll("A", 1, "PA", 50.0),
            poll("A", 2, "PA", 40.0),
            poll("B", 1, "PA", 30.0),
            poll("B", 1, "PA", 30.0),
            poll("A", 1, "OH", 0.0),
        ];
        compute_implied_probabilities(&mut polls);
        assert_eq!(polls[0].implied_prob, 45.0 / 95.0);
        assert_eq!(polls[1].implied_prob, 50.0 / 95.0);
        assert_eq!(polls[2].implied_prob, 1.0);
        assert_eq!(polls[3].implied_prob, 0.5);
        // A zero total leaves the pct/100 value alone.
        assert_eq!(polls[5].implied_prob, 0.0);
    }

    #[test]
    fn missing_dates_group_together() {
        let mut polls = vec![poll("A", 1, "", 60.0), poll("A", 1, "", 40.0)];
        polls[0].poll_date = None;
        polls[1].poll_date = None;
        compute_implied_probabilities(&mut polls);
        assert_eq!((polls[0].implied_prob, polls[1].implied_prob), (0.6, 0.4));
    }
}
