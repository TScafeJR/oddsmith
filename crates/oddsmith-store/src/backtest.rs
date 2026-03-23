//! The bulk backtest trade query.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use oddsmith_core::research::TradeSource;
use oddsmith_core::{BacktestTrade, Window, WindowBy};

use crate::db::{Db, Num, Param};

impl Db {
    /// All matching trades across resolved markets in one JOIN query, oldest first.
    ///
    /// The band is on YES probability: YES-token trades priced in `[min_price, max_price]`,
    /// or for `side == "NO"` NO-token trades priced in `[1 - max_price, 1 - min_price]`.
    /// `window` bounds a half-open range on the entry time or the market's resolution time.
    pub fn get_all_backtest_trades(
        &self,
        min_price: f64,
        max_price: f64,
        side: &str,
        race_filter: &str,
        window: &Window,
    ) -> Result<Vec<BacktestTrade>> {
        let (outcome_filter, q_min, q_max) =
            if side == "NO" { ("= 'No'", 1.0 - max_price, 1.0 - min_price) } else { ("!= 'No'", min_price, max_price) };

        // Prices are bound as float8, so the comparison with the NUMERIC column happens in
        // float8.
        let mut sql = format!(
            "
            SELECT t.market_id, t.price, m.resolution,
                COALESCE(t.traded_at, m.end_date) AS traded_at,
                COALESCE(m.end_date, t.traded_at) AS end_date
            FROM trades t
            JOIN markets m ON m.id = t.market_id
            WHERE m.resolved = true AND m.resolution IS NOT NULL
                AND t.price >= $1::float8 AND t.price <= $2::float8
                AND t.outcome {outcome_filter}"
        );
        let mut params: Vec<Param<'_>> = vec![&q_min, &q_max];
        if !race_filter.is_empty() {
            sql += &format!(" AND m.race_type = ${}", params.len() + 1);
            params.push(&race_filter);
        }
        let ts_column = match window.by {
            WindowBy::Traded => "COALESCE(t.traded_at, m.end_date)",
            WindowBy::Resolved => "COALESCE(m.end_date, t.traded_at)",
        };
        if let Some(start) = &window.start {
            sql += &format!(" AND {ts_column} >= ${}", params.len() + 1);
            params.push(start);
        }
        if let Some(end) = &window.end {
            sql += &format!(" AND {ts_column} < ${}", params.len() + 1);
            params.push(end);
        }
        // Chronological replay: drawdown, the equity curve and capital limits depend on order.
        sql += " ORDER BY 4, t.market_id";

        let rows = self.query(&sql, &params).map_err(|e| anyhow!("bulk querying backtest trades: {e:#}"))?;
        rows.iter()
            .map(|r| {
                Ok(BacktestTrade {
                    market_id: r.try_get(0)?,
                    price: r.try_get::<_, Num>(1)?.0,
                    resolution: r.try_get(2)?,
                    traded_at: r.try_get::<_, DateTime<Utc>>(3)?,
                    end_date: r.try_get::<_, DateTime<Utc>>(4)?,
                })
            })
            .collect::<Result<_, postgres::Error>>()
            .map_err(|e| anyhow!("bulk querying backtest trades: {e}"))
    }
}

impl TradeSource for Db {
    fn backtest_trades(
        &self,
        min_price: f64,
        max_price: f64,
        side: &str,
        race_filter: &str,
        window: &Window,
    ) -> Result<Vec<BacktestTrade>> {
        self.get_all_backtest_trades(min_price, max_price, side, race_filter, window)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    /// Runs against a real database when ODDSMITH_TEST_DB_URL is set; skipped otherwise.
    #[test]
    fn backtest_trades_respect_band_window_and_order() {
        let Ok(url) = std::env::var("ODDSMITH_TEST_DB_URL") else { return };
        let db = Db::connect(&url).unwrap();
        let all = db.get_all_backtest_trades(0.0, 1.0, "YES", "", &Window::default()).unwrap();
        assert!(!all.is_empty());
        assert!(all.windows(2).all(|w| (w[0].traded_at, &w[0].market_id) <= (w[1].traded_at, &w[1].market_id)));

        let start = Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 9, 1, 0, 0, 0).unwrap();
        let window = Window { start: Some(start), end: Some(end), by: WindowBy::Resolved };
        for t in db.get_all_backtest_trades(0.0, 0.3, "NO", "", &window).unwrap() {
            assert!((0.7..=1.0).contains(&t.price), "{}", t.price);
            assert!(start <= t.end_date && t.end_date < end);
        }
        assert!(db.get_all_backtest_trades(0.0, 1.0, "YES", "no-such-race", &Window::default()).unwrap().is_empty());
    }
}
