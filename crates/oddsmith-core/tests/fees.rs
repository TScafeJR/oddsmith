//! Contract fees and fee-adjusted metrics.

use oddsmith_core::strategy::contract_fee;
use oddsmith_core::{Position, Strategy, compute_portfolio_metrics};

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn contract_fee_cases() {
    let cases = [
        (0.50, 0.07, 0.0175), // peak dollar fee at 50c
        (0.10, 0.07, 0.0063),
        (0.90, 0.07, 0.0063), // symmetric in p
        (0.85, 0.00, 0.0),
    ];
    for (price, rate, want) in cases {
        let got = contract_fee(price, rate);
        assert!(approx(got, want), "contract_fee({price:.2}, {rate:.2}) = {got:.6}, want {want:.6}");
    }
}

#[test]
fn contract_fee_hits_cheap_contracts_hardest() {
    // As a fraction of cost, cheap contracts pay more.
    assert!(contract_fee(0.10, 0.07) / 0.10 > contract_fee(0.90, 0.07) / 0.90);
}

#[test]
fn fee_rate_by_role() {
    let mut s = Strategy { maker_fee_rate: 0.0175, taker_fee_rate: 0.07, ..Strategy::default() };
    s.role = "MAKER".into();
    assert_eq!(s.fee_rate(), 0.0175);
    s.role = "taker".into();
    assert_eq!(s.fee_rate(), 0.07);
    s.role = String::new();
    assert_eq!(s.fee_rate(), 0.0175, "default role should use the maker rate");
}

#[test]
fn bucket_breakeven_includes_fees() {
    // 100 NO contracts at 85c with a 0.07 taker fee: fee = 0.07 * 0.85 * 0.15 = 0.008925/contract.
    let fee = contract_fee(0.85, 0.07) * 100.0;
    let pos =
        |pnl: f64| Position { entry_price: 0.85, size: 100.0, fee, pnl, bucket: "10-15".into(), ..Position::default() };
    let pm = compute_portfolio_metrics(&[pos(15.0 - fee), pos(-85.0 - fee)], 0.0);
    assert!(approx(pm.total_fees, 2.0 * fee), "total_fees = {}", pm.total_fees);
    let b = &pm.bucket_breakdown[0];
    assert!(approx(b.expected_win_pct, 0.85), "expected_win_pct = {}", b.expected_win_pct);
    assert!(approx(b.breakeven_win_pct, 0.85 + 0.008925), "breakeven_win_pct = {}", b.breakeven_win_pct);
}
