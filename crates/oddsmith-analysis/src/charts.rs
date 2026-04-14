//! Calibration chart data and the self-contained calibration chart HTML.

use std::fs;

use anyhow::{Context, Result, anyhow};
use oddsmith_store::{Db, Param, RowExt};

use crate::json::{Json, compact_html_json};
use crate::textfmt::fmt_1f;
use crate::util::{os_err, pg, render};

const TEMPLATE: &str = include_str!("../templates/calibration_chart.html");

/// Per-cent statistics for chart generation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PricePointData {
    pub price: f64,
    pub num_trades: i64,
    pub num_markets: i64,
    pub win_rate: f64,
    pub avg_return: f64,
    pub std_dev: f64,
}

impl PricePointData {
    fn to_json(&self) -> Json {
        Json::obj([
            ("price", Json::Float(self.price)),
            ("n", Json::Int(self.num_trades)),
            ("markets", Json::Int(self.num_markets)),
            ("win_rate", Json::Float(self.win_rate)),
            ("avg_return", Json::Float(self.avg_return)),
            ("std_dev", Json::Float(self.std_dev)),
        ])
    }
}

/// The data payload embedded in the HTML.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalibrationChartInput {
    /// `None` serializes as null.
    pub points: Option<Vec<PricePointData>>,
    pub total_trades: i64,
    pub total_markets: i64,
    pub race_filter: String,
    pub risk_free_rate: f64,
}

impl CalibrationChartInput {
    fn to_json(&self) -> Json {
        Json::obj([
            (
                "points",
                self.points.as_ref().map_or(Json::Null, |ps| Json::Arr(ps.iter().map(|p| p.to_json()).collect())),
            ),
            ("total_trades", Json::Int(self.total_trades)),
            ("total_markets", Json::Int(self.total_markets)),
            ("race_filter", Json::str(self.race_filter.clone())),
            ("risk_free_rate", Json::Float(self.risk_free_rate)),
        ])
    }
}

/// Query per-cent price point statistics from resolved markets.
pub fn compute_calibration_chart_data(
    db: &Db,
    race_filter: &str,
    risk_free_rate: f64,
) -> Result<CalibrationChartInput> {
    // NO-token trades at price p mean implied YES probability = 1-p.
    // We flip them so the calibration curve is correct.
    let mut query = String::from(
        "
		WITH trade_probs AS (
			SELECT
				t.market_id,
				CASE WHEN t.outcome = 'No' THEN 1.0 - t.price ELSE t.price END AS implied_prob,
				CASE WHEN m.resolution = 'YES' THEN 1.0 ELSE 0.0 END AS won
			FROM trades t
			JOIN markets m ON m.id = t.market_id
			WHERE m.resolved = true AND m.resolution IS NOT NULL
				AND t.price >= 0.005 AND t.price <= 1.0",
    );
    let mut args: Vec<Param<'_>> = Vec::new();
    if !race_filter.is_empty() {
        query.push_str(" AND m.race_type = $1");
        args.push(&race_filter);
    }
    query.push_str(
        "
		)
		SELECT
			ROUND(implied_prob::numeric, 2) AS price_point,
			COUNT(*) AS n,
			COUNT(DISTINCT market_id) AS n_markets,
			AVG(won) AS win_rate,
			AVG(CASE WHEN won = 1.0
				THEN (1.0 - implied_prob) / NULLIF(implied_prob, 0)
				ELSE -1.0 END) AS avg_return,
			COALESCE(STDDEV_SAMP(CASE WHEN won = 1.0
				THEN (1.0 - implied_prob) / NULLIF(implied_prob, 0)
				ELSE -1.0 END), 0) AS return_sd
		FROM trade_probs
		WHERE implied_prob >= 0.005 AND implied_prob <= 1.0
		GROUP BY price_point
		ORDER BY price_point",
    );

    let rows = db.query(&query, &args).map_err(pg).context("querying calibration chart data")?;

    let mut points: Option<Vec<PricePointData>> = None;
    let mut total_trades = 0;
    for r in &rows {
        let p = PricePointData {
            price: r.num(0),
            num_trades: r.get(1),
            num_markets: r.get(2),
            win_rate: r.num(3),
            avg_return: r.num(4),
            std_dev: r.num(5),
        };
        total_trades += p.num_trades;
        points.get_or_insert_with(Vec::new).push(p);
    }

    // Count total unique resolved markets.
    let mut count_query = String::from(
        "SELECT COUNT(DISTINCT m.id) FROM markets m
		JOIN trades t ON t.market_id = m.id
		WHERE m.resolved = true AND m.resolution IS NOT NULL
			AND t.price >= 0.005 AND t.price <= 1.0",
    );
    let mut count_args: Vec<Param<'_>> = Vec::new();
    if !race_filter.is_empty() {
        count_query.push_str(" AND m.race_type = $1");
        count_args.push(&race_filter);
    }
    let total_markets = match db.query_one(&count_query, &count_args) {
        Ok(Some(row)) => row.get::<_, Option<i64>>(0).unwrap_or(0),
        _ => 0,
    };

    Ok(CalibrationChartInput {
        points,
        total_trades,
        total_markets,
        race_filter: race_filter.to_string(),
        risk_free_rate,
    })
}

/// Render the chart page: calibration curve, price analysis table and sensitivity
/// calculator.
pub fn calibration_chart_html(data: &CalibrationChartInput) -> Result<String> {
    let data_json = compact_html_json(&data.to_json()).map_err(|e| anyhow!("marshaling chart data: {e}"))?;
    let rf = fmt_1f(data.risk_free_rate * 100.0);
    Ok(render(TEMPLATE, &[("RISK_FREE_PCT", &rf), ("DATA_JSON", &data_json)]))
}

/// Write the self-contained chart HTML to `path`.
pub fn write_calibration_chart_html(path: &str, data: &CalibrationChartInput) -> Result<()> {
    let html = calibration_chart_html(data)?;
    fs::write(path, html).map_err(|e| anyhow!(os_err(&e, path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_data_and_rate() {
        let data = CalibrationChartInput {
            points: Some(vec![PricePointData {
                price: 0.5,
                num_trades: 3,
                num_markets: 2,
                win_rate: 1.0,
                avg_return: 1.0,
                std_dev: 0.0,
            }]),
            total_trades: 3,
            total_markets: 2,
            race_filter: "<x>".into(),
            risk_free_rate: 0.04,
        };
        let html = calibration_chart_html(&data).unwrap();
        assert!(html.contains("value=\"4.0\""));
        assert!(html.contains(
            "const DATA = {\"points\":[{\"price\":0.5,\"n\":3,\"markets\":2,\"win_rate\":1.0,\"avg_return\":1.0,\"std_dev\":0.0}],\"total_trades\":3,\"total_markets\":2,\"race_filter\":\"\\u003cx\\u003e\",\"risk_free_rate\":0.04};"
        ));
        let empty = CalibrationChartInput { risk_free_rate: f64::NAN, ..Default::default() };
        assert_eq!(
            calibration_chart_html(&empty).unwrap_err().to_string(),
            "marshaling chart data: Out of range float values are not JSON compliant: nan"
        );
    }
}
