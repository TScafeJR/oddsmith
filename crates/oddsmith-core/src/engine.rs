//! The pure simulation core and the capital ledger.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};

use crate::BacktestTrade;
use crate::metrics::{Position, float_div, max_first, sorted_by_time};
use crate::strategy::{Slot, Strategy, bucket_slot, contract_fee, custom_label, is_sorted, tens_label};

/// Returned when a run is stopped through its cancel flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("backtest cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Bucket labels for one strategy, formatted once per bucket instead of once per trade.
struct Labeler<'a> {
    buckets: &'a [f64],
    sorted: bool,
    custom: Vec<Option<String>>,
    tens: Vec<Option<String>>,
}

impl<'a> Labeler<'a> {
    fn new(strategy: &'a Strategy) -> Self {
        let buckets = strategy.buckets.as_deref().unwrap_or(&[]);
        Labeler { buckets, sorted: is_sorted(buckets), custom: vec![None; buckets.len()], tens: vec![None; 10] }
    }

    fn label(&mut self, price: f64) -> String {
        match bucket_slot(self.buckets, self.sorted, price * 100.0) {
            Slot::Custom(i) => self.custom[i].get_or_insert_with(|| custom_label(self.buckets, i)).clone(),
            Slot::Tens(b) if (0..100).contains(&b) => {
                self.tens[(b / 10) as usize].get_or_insert_with(|| tens_label(b)).clone()
            }
            Slot::Tens(b) => tens_label(b),
        }
    }
}

/// Turn qualifying trades into simulated positions held to resolution.
///
/// This is the pure core of the backtest: no database access, so it can be run
/// repeatedly in memory and tested with fixture data.
/// `cancel` is checked every 100k trades; `log` receives progress lines (each ending in `\n`).
pub fn simulate(
    trades: &[BacktestTrade],
    strategy: &Strategy,
    cancel: Option<&AtomicBool>,
    log: Option<&dyn Fn(&str)>,
) -> Result<Vec<Position>, Cancelled> {
    let side = if strategy.side.is_empty() { "YES" } else { strategy.side.as_str() };
    let role = match strategy.role.to_uppercase() {
        r if r.is_empty() => "MAKER".to_string(),
        r => r,
    };
    let fee_rate = strategy.fee_rate();
    let size = strategy.position_size;
    let mut labels = Labeler::new(strategy);

    let mut positions = Vec::with_capacity(trades.len());
    for (i, trade) in trades.iter().enumerate() {
        if i % 100_000 == 0 {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                if let Some(log) = log {
                    log(&format!("backtest cancelled after {i}/{} trades\n", trades.len()));
                }
                return Err(Cancelled);
            }
            if i > 0
                && let Some(log) = log
            {
                log(&format!("  processing trades: {i}/{} ({} positions)\n", trades.len(), positions.len()));
            }
        }

        let resolved_yes = matches!(trade.resolution.as_str(), "YES" | "Yes" | "1");
        let (bucket, won) = if side == "NO" {
            (labels.label(1.0 - trade.price), !resolved_yes)
        } else {
            (labels.label(trade.price), resolved_yes)
        };
        let mut pnl = if won { (1.0 - trade.price) * size } else { -trade.price * size };

        // trade.price is the price of the token we buy (the NO-token price when side=NO),
        // so the fee and the cost basis are both in terms of the contract we hold.
        let fee = contract_fee(trade.price, fee_rate) * size;
        pnl -= fee;
        let return_pct = float_div(pnl, trade.price * size + fee);

        // Hold time: traded_at -> market end_date, in days, never negative.
        let hold = trade.end_date - trade.traded_at;
        let hold_days = hold_hours(hold) / 24.0;
        let hold_days = if 0.0 > hold_days { 0.0 } else { hold_days };

        positions.push(Position {
            market_id: trade.market_id.clone(),
            entry_price: trade.price,
            size,
            side: side.to_string(),
            role: role.clone(),
            resolution: trade.resolution.clone(),
            pnl,
            fee,
            return_pct,
            bucket,
            hold_days,
            entered_at: Some(trade.traded_at),
            resolved_at: Some(trade.end_date),
        });
    }
    Ok(positions)
}

/// `timedelta / timedelta(hours=1)`: the exact microsecond ratio, correctly rounded.
fn hold_hours(d: chrono::TimeDelta) -> f64 {
    match d.num_microseconds() {
        Some(us) => us as f64 / 3_600_000_000.0,
        None => d.num_milliseconds() as f64 / 3_600_000.0,
    }
}

/// Result of replaying positions against a cash balance.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapitalLedger {
    pub accepted: Vec<Position>,
    pub skipped: usize,
    pub peak_exposure: f64,
    pub final_cash: f64,
}

/// An open position in the ledger, ordered by (resolves, seq).
#[derive(Debug, Clone, Copy)]
struct Open {
    resolves: DateTime<Utc>,
    seq: usize,
    cost: f64,
    payout: f64,
}

impl Open {
    fn lt(&self, other: &Open) -> bool {
        (self.resolves, self.seq) < (other.resolves, other.seq)
    }
}

/// An array-backed binary min-heap with a fixed sift order, so iterating the leftover
/// entries sums them in a deterministic order.
#[derive(Default)]
struct MinHeap(Vec<Open>);

impl MinHeap {
    fn push(&mut self, item: Open) {
        self.0.push(item);
        let n = self.0.len() - 1;
        self.sift_down(0, n);
    }

    fn pop(&mut self) -> Option<Open> {
        let last = self.0.pop()?;
        if self.0.is_empty() {
            return Some(last);
        }
        let top = std::mem::replace(&mut self.0[0], last);
        self.sift_up(0);
        Some(top)
    }

    fn peek(&self) -> Option<&Open> {
        self.0.first()
    }

    // Move the item at `pos` up towards `start` until its parent is not larger.
    fn sift_down(&mut self, start: usize, mut pos: usize) {
        let item = self.0[pos];
        while pos > start {
            let parent_pos = (pos - 1) >> 1;
            let parent = self.0[parent_pos];
            if item.lt(&parent) {
                self.0[pos] = parent;
                pos = parent_pos;
                continue;
            }
            break;
        }
        self.0[pos] = item;
    }

    // Walk the smaller child up from `pos` to a leaf, then sift the item into place.
    fn sift_up(&mut self, mut pos: usize) {
        let end = self.0.len();
        let start = pos;
        let item = self.0[pos];
        let mut child = 2 * pos + 1;
        while child < end {
            let right = child + 1;
            if right < end && !self.0[child].lt(&self.0[right]) {
                child = right;
            }
            self.0[pos] = self.0[child];
            pos = child;
            child = 2 * pos + 1;
        }
        self.0[pos] = item;
        self.sift_down(start, pos);
    }
}

/// Keep only the positions a fixed bankroll could have afforded.
///
/// Positions are taken in entry order. Each one ties up its cost (price x size + fee)
/// until its market resolves, when the payout (size if it won, 0 if it lost) returns to
/// cash. An entry is skipped when cash on hand can't cover its cost.
pub fn apply_capital(positions: &[Position], initial_capital: f64) -> CapitalLedger {
    let ordered = sorted_by_time(positions, |p| p.entered_at);
    let mut cash = initial_capital;
    let mut exposure = 0.0;
    let mut peak = 0.0;
    let mut open = MinHeap::default();
    let mut accepted = Vec::new();
    let mut skipped = 0;
    for (seq, p) in ordered.into_iter().enumerate() {
        let now = p.entered_at;
        if let Some(now) = now {
            while open.peek().is_some_and(|o| o.resolves <= now) {
                let o = open.pop().expect("peeked");
                cash += o.payout;
                exposure -= o.cost;
            }
        }
        let cost = p.entry_price * p.size + p.fee;
        if cost > cash + 1e-9 {
            skipped += 1;
            continue;
        }
        cash -= cost;
        exposure += cost;
        peak = max_first(peak, exposure);
        let resolves = p.resolved_at.or(now).unwrap_or(DateTime::<Utc>::MAX_UTC);
        open.push(Open { resolves, seq, cost, payout: cost + p.pnl });
        accepted.push(p.clone());
    }
    for o in &open.0 {
        cash += o.payout;
    }
    CapitalLedger { accepted, skipped, peak_exposure: peak, final_cash: cash }
}
