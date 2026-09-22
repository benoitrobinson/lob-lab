use lob::{Qty, Side};

#[derive(Debug, Clone, Copy)]
pub struct Fill {
    pub ts: i64,
    pub side: Side,
    pub price_usd: f64,
    pub size_usd: Qty,
    pub fee_btc: f64,
}

/// Inverse-contract book-keeping. `cash_btc` is not a cash balance: it is the
/// accumulated `size / price` of every fill, signed by direction. Equity falls
/// out as `cash_btc - position_usd / mark`.
#[derive(Debug, Default, Clone)]
pub struct Ledger {
    pub cash_btc: f64,
    pub position_usd: Qty,
    pub fees_btc: f64,
    pub fills: Vec<Fill>,
}

impl Ledger {
    pub fn on_fill(&mut self, ts: i64, side: Side, price_usd: f64, size_usd: Qty, fee_rate: f64) {
        let signed = match side {
            Side::Bid => size_usd,
            Side::Ask => -size_usd,
        };
        self.position_usd += signed;
        self.cash_btc += signed as f64 / price_usd;
        let fee_btc = fee_rate * size_usd as f64 / price_usd;
        self.cash_btc -= fee_btc;
        self.fees_btc += fee_btc;
        self.fills.push(Fill {
            ts,
            side,
            price_usd,
            size_usd,
            fee_btc,
        });
    }

    pub fn equity_btc(&self, mark_usd: f64) -> f64 {
        self.cash_btc - self.position_usd as f64 / mark_usd
    }

    pub fn equity_usd(&self, mark_usd: f64) -> f64 {
        self.equity_btc(mark_usd) * mark_usd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lob::Side;

    #[test]
    fn a_long_gains_btc_when_the_price_rises() {
        let mut l = Ledger::default();
        l.on_fill(0, Side::Bid, 100_000.0, 100, 0.0);
        let expected = 100.0 / 100_000.0 - 100.0 / 101_000.0;
        assert!((l.equity_btc(101_000.0) - expected).abs() < 1e-15);
        assert!(l.equity_btc(99_000.0) < 0.0);
    }

    #[test]
    fn a_short_gains_btc_when_the_price_falls() {
        let mut l = Ledger::default();
        l.on_fill(0, Side::Ask, 100_000.0, 100, 0.0);
        assert!(l.equity_btc(99_000.0) > 0.0);
        assert!(l.equity_btc(101_000.0) < 0.0);
    }

    #[test]
    fn a_round_trip_at_the_same_price_costs_exactly_the_fees() {
        let mut l = Ledger::default();
        l.on_fill(0, Side::Bid, 100_000.0, 1_000, 1.5e-4);
        l.on_fill(1, Side::Ask, 100_000.0, 1_000, 1.5e-4);
        assert_eq!(l.position_usd, 0);
        let expected_fees = 2.0 * 1.5e-4 * 1_000.0 / 100_000.0;
        assert!((l.fees_btc - expected_fees).abs() < 1e-15);
        assert!((l.equity_btc(100_000.0) + expected_fees).abs() < 1e-15);
    }

    #[test]
    fn equity_in_usd_is_equity_in_btc_at_the_mark() {
        let mut l = Ledger::default();
        l.on_fill(0, Side::Bid, 100_000.0, 100, 0.0);
        let mark = 101_000.0;
        assert!((l.equity_usd(mark) - l.equity_btc(mark) * mark).abs() < 1e-9);
    }
}
