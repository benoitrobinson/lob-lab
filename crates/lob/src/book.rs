use std::collections::BTreeMap;

/// Price in ticks. BTC-PERPETUAL has a tick size of 0.5 USD, so a tick of
/// 200_000 is a price of 100_000.0 USD.
pub type Tick = i64;
/// Size in USD notional. Deribit reports perpetual amounts in USD.
pub type Qty = i64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Bid,
    Ask,
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct Book {
    bids: BTreeMap<Tick, Qty>,
    asks: BTreeMap<Tick, Qty>,
}

impl Book {
    pub fn new() -> Self {
        Self::default()
    }

    /// Deribit sends absolute amounts per level; a delete carries amount 0.
    pub fn set(&mut self, side: Side, price: Tick, amount: Qty) {
        let level = match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };
        if amount <= 0 {
            level.remove(&price);
        } else {
            level.insert(price, amount);
        }
    }

    pub fn qty_at(&self, side: Side, price: Tick) -> Qty {
        let level = match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        };
        level.get(&price).copied().unwrap_or(0)
    }

    pub fn best_bid(&self) -> Option<(Tick, Qty)> {
        self.bids.iter().next_back().map(|(p, q)| (*p, *q))
    }

    pub fn best_ask(&self) -> Option<(Tick, Qty)> {
        self.asks.iter().next().map(|(p, q)| (*p, *q))
    }

    /// Mid in ticks, doubled so it stays an integer.
    pub fn mid2(&self) -> Option<Tick> {
        match (self.best_bid(), self.best_ask()) {
            (Some((b, _)), Some((a, _))) => Some(b + a),
            _ => None,
        }
    }

    pub fn crossed(&self) -> bool {
        match (self.best_bid(), self.best_ask()) {
            (Some((b, _)), Some((a, _))) => b >= a,
            _ => false,
        }
    }

    pub fn levels(&self, side: Side) -> impl Iterator<Item = (Tick, Qty)> + '_ {
        let level = match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        };
        level.iter().map(|(p, q)| (*p, *q))
    }

    pub fn clear(&mut self) {
        self.bids.clear();
        self.asks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_absolute_amounts_and_deletes_on_zero() {
        let mut b = Book::new();
        b.set(Side::Bid, 100, 5);
        b.set(Side::Bid, 99, 7);
        b.set(Side::Ask, 101, 3);
        assert_eq!(b.best_bid(), Some((100, 5)));
        assert_eq!(b.best_ask(), Some((101, 3)));
        b.set(Side::Bid, 100, 0);
        assert_eq!(b.best_bid(), Some((99, 7)));
        assert!(!b.crossed());
    }

    #[test]
    fn detects_a_crossed_book() {
        let mut b = Book::new();
        b.set(Side::Bid, 101, 5);
        b.set(Side::Ask, 100, 5);
        assert!(b.crossed());
    }
}
