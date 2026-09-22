use lob::{Qty, Side, Tick};

/// Instrument metadata, read at run time from `public/get_instrument` and
/// pinned in the run manifest so a replay is reproducible.
#[derive(Debug, Clone, Copy)]
pub struct Instrument {
    pub tick_size: f64,
    pub contract_size: f64,
    pub maker_fee: f64,
    pub taker_fee: f64,
}

impl Instrument {
    /// Values verified against public/get_instrument on 2026-09-22.
    pub fn btc_perpetual() -> Self {
        Self {
            tick_size: 0.5,
            contract_size: 10.0,
            maker_fee: 1.5e-4,
            taker_fee: 3.5e-4,
        }
    }
}

pub fn to_ticks(price: f64, inst: &Instrument) -> Tick {
    (price / inst.tick_size).round() as Tick
}

pub fn from_ticks(ticks: Tick, inst: &Instrument) -> f64 {
    ticks as f64 * inst.tick_size
}

pub fn to_qty(amount: f64, _inst: &Instrument) -> Qty {
    amount.round() as Qty
}

/// The book side a trade consumed. Deribit's `direction` is the taker's action.
pub fn taker_side(direction: &str) -> Side {
    match direction {
        "buy" => Side::Ask,
        _ => Side::Bid,
    }
}

/// One market event on the replay timeline.
#[derive(Debug, Clone)]
pub enum Event {
    Snapshot {
        ts: i64,
        bids: Vec<(Tick, Qty)>,
        asks: Vec<(Tick, Qty)>,
    },
    Change {
        ts: i64,
        bids: Vec<(Tick, Qty)>,
        asks: Vec<(Tick, Qty)>,
    },
    Trade {
        ts: i64,
        price: Tick,
        size: Qty,
        /// The book side consumed by the taker.
        side: Side,
    },
    Quote {
        ts: i64,
        bid: Option<(Tick, Qty)>,
        ask: Option<(Tick, Qty)>,
    },
    /// A detected feed gap. Everything between the previous event and this one
    /// is unknown, so the simulator flattens and stands down until the next
    /// snapshot.
    Gap {
        ts: i64,
        missing_from: u64,
        missing_to: u64,
    },
}

impl Event {
    pub fn ts(&self) -> i64 {
        match self {
            Event::Snapshot { ts, .. }
            | Event::Change { ts, .. }
            | Event::Trade { ts, .. }
            | Event::Quote { ts, .. }
            | Event::Gap { ts, .. } => *ts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lob::Side;

    #[test]
    fn converts_prices_to_ticks_and_back() {
        let inst = Instrument::btc_perpetual();
        // 3955.5 is on the 0.5 grid; the docs example 3955.75 is not.
        assert_eq!(to_ticks(3955.5, &inst), 7911);
        assert_eq!(to_ticks(0.5, &inst), 1);
        assert!((from_ticks(7911, &inst) - 3955.5).abs() < 1e-9);
    }

    #[test]
    fn a_taker_buy_consumes_the_ask_side() {
        assert_eq!(taker_side("buy"), Side::Ask);
        assert_eq!(taker_side("sell"), Side::Bid);
    }

    #[test]
    fn sizes_round_to_whole_usd() {
        let inst = Instrument::btc_perpetual();
        assert_eq!(to_qty(100.0, &inst), 100);
        assert_eq!(to_qty(0.0, &inst), 0);
    }
}
