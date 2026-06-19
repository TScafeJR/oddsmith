//! Round-trips against a real Postgres. Set ODDSMITH_TEST_DB_URL to run them; skipped otherwise.

use oddsmith_store::{Db, Num, RowExt};

fn db() -> Option<Db> {
    let url = std::env::var("ODDSMITH_TEST_DB_URL").ok()?;
    Some(Db::connect(&url).expect("connect"))
}

#[test]
fn numeric_values_round_trip_through_postgres() {
    let Some(db) = db() else { return };
    for v in [0.0, 0.5, 0.07, -12345.6, 0.1 + 0.2, 1e-10, 98765.4321] {
        let row = db.query_one("SELECT $1::numeric, $1::numeric::text", &[&Num(v)]).unwrap().unwrap();
        assert_eq!(row.num(0), v, "numeric param {v}");
        assert_eq!(row.get::<_, String>(1).parse::<f64>().unwrap(), v, "server sees {v}");
    }
    let row = db.query_one("SELECT NULL::numeric, 7::int4, 2.5::float4", &[]).unwrap().unwrap();
    assert_eq!(row.opt_num(0), None);
    assert_eq!(row.num(1), 7.0);
    assert_eq!(row.num(2), 2.5);
}

#[test]
fn pool_serves_many_threads() {
    let Some(db) = db() else { return };
    let handles: Vec<_> = (0..25)
        .map(|i| {
            let db = db.clone();
            std::thread::spawn(move || {
                let row = db.query_one("SELECT $1::int8 + 1, pg_sleep(0.05)::text", &[&(i as i64)]).unwrap().unwrap();
                assert_eq!(row.get::<_, i64>(0), i as i64 + 1);
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let total = db.tx(|tx| Ok(tx.query_one("SELECT count(*) FROM markets", &[])?.get::<_, i64>(0))).unwrap();
    assert!(total >= 0);
}
