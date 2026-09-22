use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};

use feed::event::Event;
use lob::{Book, Qty, Side, Tick};

use crate::ledger::Ledger;
use crate::markout::MidSeries;
use crate::queue::{OwnOrder, QueueModel};

#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Time between deciding a quote and it resting on the book.
    pub latency_ms: i64,
    pub model: QueueModel,
    pub maker_fee: f64,
    pub tick_size: f64,
    /// Minimum time between two requote decisions.
    pub requote_ms: i64,
    /// Absolute inventory limit in USD; we stop quoting the side that would breach it.
    pub max_position: Qty,
}

pub struct Ctx<'a> {
    pub ts: i64,
    pub book: &'a Book,
    pub position_usd: Qty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quotes {
    pub bid: Option<Tick>,
    pub ask: Option<Tick>,
    pub size: Qty,
}

pub trait Quoter {
    fn quote(&mut self, ctx: &Ctx) -> Option<Quotes>;
}

pub struct Report {
    pub ledger: Ledger,
    pub mids: MidSeries,
    pub requotes: u64,
    pub halted_ms: i64,
    pub fill_hash: u64,
}

pub struct Engine {
    cfg: Config,
    book: Book,
    bid: Option<OwnOrder>,
    ask: Option<OwnOrder>,
    pending: VecDeque<(i64, Quotes)>,
    ledger: Ledger,
    mids: Vec<(i64, f64)>,
    last_decision: i64,
    live: Option<Quotes>,
    halted: bool,
    halted_since: Option<i64>,
    halted_ms: i64,
    requotes: u64,
}

impl Engine {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            book: Book::new(),
            bid: None,
            ask: None,
            pending: VecDeque::new(),
            ledger: Ledger::default(),
            mids: Vec::new(),
            last_decision: i64::MIN,
            live: None,
            halted: true,
            halted_since: None,
            halted_ms: 0,
            requotes: 0,
        }
    }

    fn mid_usd(&self) -> Option<f64> {
        self.book
            .mid2()
            .map(|m| m as f64 * self.cfg.tick_size / 2.0)
    }

    fn apply_pending(&mut self, now: i64) {
        while matches!(self.pending.front(), Some((ts, _)) if *ts <= now) {
            let (_, quotes) = self.pending.pop_front().expect("front checked");
            self.bid = quotes.bid.map(|p| {
                OwnOrder::new(
                    Side::Bid,
                    p,
                    quotes.size,
                    self.book.qty_at(Side::Bid, p),
                    self.cfg.model,
                )
            });
            self.ask = quotes.ask.map(|p| {
                OwnOrder::new(
                    Side::Ask,
                    p,
                    quotes.size,
                    self.book.qty_at(Side::Ask, p),
                    self.cfg.model,
                )
            });
        }
    }

    fn cancel_all(&mut self) {
        self.bid = None;
        self.ask = None;
        self.pending.clear();
        self.live = None;
    }

    fn on_trade(&mut self, ts: i64, price: Tick, size: Qty, side: Side) {
        let order = match side {
            Side::Bid => self.bid.as_mut(),
            Side::Ask => self.ask.as_mut(),
        };
        let Some(order) = order else { return };
        let filled = order.on_trade(price, size);
        if filled > 0 {
            let price_usd = order.price as f64 * self.cfg.tick_size;
            let remaining = order.remaining();
            self.ledger
                .on_fill(ts, side, price_usd, filled, self.cfg.maker_fee);
            if remaining == 0 {
                match side {
                    Side::Bid => self.bid = None,
                    Side::Ask => self.ask = None,
                }
                self.live = None;
            }
        }
    }

    fn apply_levels(&mut self, side: Side, levels: &[(Tick, Qty)]) {
        for (price, qty) in levels {
            let old = self.book.qty_at(side, *price);
            let own = match side {
                Side::Bid => self.bid.as_mut(),
                Side::Ask => self.ask.as_mut(),
            };
            if let Some(order) = own
                && order.price == *price
            {
                order.on_level_change(old, *qty);
            }
            self.book.set(side, *price, *qty);
        }
    }

    fn decide(&mut self, ts: i64, quoter: &mut dyn Quoter) {
        // Saturating because `last_decision` starts at i64::MIN: the first
        // decision is always allowed, never an overflow.
        if self.halted || ts.saturating_sub(self.last_decision) < self.cfg.requote_ms {
            return;
        }
        self.last_decision = ts;
        let ctx = Ctx {
            ts,
            book: &self.book,
            position_usd: self.ledger.position_usd,
        };
        let Some(mut wanted) = quoter.quote(&ctx) else {
            return;
        };
        if self.ledger.position_usd >= self.cfg.max_position {
            wanted.bid = None;
        }
        if self.ledger.position_usd <= -self.cfg.max_position {
            wanted.ask = None;
        }
        if Some(wanted) == self.live {
            return;
        }
        self.live = Some(wanted);
        self.requotes += 1;
        self.pending.push_back((ts + self.cfg.latency_ms, wanted));
    }

    pub fn run(mut self, events: &[Event], quoter: &mut dyn Quoter) -> Report {
        for event in events {
            let ts = event.ts();
            self.apply_pending(ts);
            match event {
                Event::Gap { .. } => {
                    self.cancel_all();
                    self.book.clear();
                    if !self.halted {
                        self.halted = true;
                        self.halted_since = Some(ts);
                    }
                }
                Event::Snapshot { bids, asks, .. } => {
                    self.book.clear();
                    self.cancel_all();
                    self.apply_levels(Side::Bid, bids);
                    self.apply_levels(Side::Ask, asks);
                    if let Some(since) = self.halted_since.take() {
                        self.halted_ms += ts - since;
                    }
                    self.halted = false;
                }
                Event::Change { bids, asks, .. } => {
                    self.apply_levels(Side::Bid, bids);
                    self.apply_levels(Side::Ask, asks);
                }
                Event::Trade {
                    price, size, side, ..
                } => {
                    self.on_trade(ts, *price, *size, *side);
                }
                Event::Quote { .. } => {}
            }
            if let Some(mid) = self.mid_usd()
                && self.mids.last().map(|(t, _)| *t) != Some(ts)
            {
                self.mids.push((ts, mid));
            }
            self.decide(ts, quoter);
        }
        if let Some(since) = self.halted_since.take() {
            self.halted_ms += events.last().map(|e| e.ts()).unwrap_or(since) - since;
        }
        let mut hasher = DefaultHasher::new();
        for f in &self.ledger.fills {
            f.ts.hash(&mut hasher);
            f.size_usd.hash(&mut hasher);
            f.price_usd.to_bits().hash(&mut hasher);
            matches!(f.side, Side::Bid).hash(&mut hasher);
        }
        Report {
            fill_hash: hasher.finish(),
            ledger: self.ledger,
            mids: MidSeries::new(self.mids),
            requotes: self.requotes,
            halted_ms: self.halted_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use feed::event::Event;
    use lob::Side;

    struct Fixed {
        bid: Tick,
        ask: Tick,
        size: Qty,
    }

    impl Quoter for Fixed {
        fn quote(&mut self, _ctx: &Ctx) -> Option<Quotes> {
            Some(Quotes {
                bid: Some(self.bid),
                ask: Some(self.ask),
                size: self.size,
            })
        }
    }

    fn snapshot(ts: i64) -> Event {
        Event::Snapshot {
            ts,
            bids: vec![(200, 100)],
            asks: vec![(202, 100)],
        }
    }

    fn config(latency_ms: i64, model: QueueModel) -> Config {
        Config {
            latency_ms,
            model,
            maker_fee: 0.0,
            tick_size: 0.5,
            requote_ms: 0,
            max_position: 1_000,
        }
    }

    #[test]
    fn a_quote_is_not_live_before_its_latency_has_passed() {
        let events = vec![
            snapshot(0),
            Event::Trade {
                ts: 500,
                price: 200,
                size: 1_000,
                side: Side::Bid,
            },
        ];
        let mut q = Fixed {
            bid: 200,
            ask: 202,
            size: 10,
        };
        let report = Engine::new(config(1_000, QueueModel::Naive)).run(&events, &mut q);
        assert_eq!(report.ledger.position_usd, 0);
    }

    #[test]
    fn a_live_quote_fills_once_the_queue_ahead_is_gone() {
        let events = vec![
            snapshot(0),
            Event::Trade {
                ts: 2_000,
                price: 200,
                size: 150,
                side: Side::Bid,
            },
        ];
        let mut q = Fixed {
            bid: 200,
            ask: 202,
            size: 10,
        };
        let report = Engine::new(config(1_000, QueueModel::Pessimistic)).run(&events, &mut q);
        assert_eq!(report.ledger.position_usd, 10);
        assert_eq!(report.ledger.fills[0].price_usd, 100.0);
    }

    #[test]
    fn the_naive_model_fills_where_the_queue_model_does_not() {
        let events = vec![
            snapshot(0),
            Event::Trade {
                ts: 2_000,
                price: 200,
                size: 50,
                side: Side::Bid,
            },
        ];
        let mut a = Fixed {
            bid: 200,
            ask: 202,
            size: 10,
        };
        let mut b = Fixed {
            bid: 200,
            ask: 202,
            size: 10,
        };
        let naive = Engine::new(config(1_000, QueueModel::Naive)).run(&events, &mut a);
        let queue = Engine::new(config(1_000, QueueModel::Pessimistic)).run(&events, &mut b);
        assert_eq!(naive.ledger.position_usd, 10);
        assert_eq!(queue.ledger.position_usd, 0);
    }

    #[test]
    fn a_gap_stands_us_down_until_the_next_snapshot() {
        let events = vec![
            snapshot(0),
            Event::Gap {
                ts: 1_500,
                missing_from: 1,
                missing_to: 9,
            },
            Event::Trade {
                ts: 2_000,
                price: 200,
                size: 1_000,
                side: Side::Bid,
            },
            snapshot(3_000),
            Event::Trade {
                ts: 5_000,
                price: 200,
                size: 1_000,
                side: Side::Bid,
            },
        ];
        let mut q = Fixed {
            bid: 200,
            ask: 202,
            size: 10,
        };
        let report = Engine::new(config(1_000, QueueModel::Naive)).run(&events, &mut q);
        assert_eq!(report.ledger.fills.len(), 1);
        assert_eq!(report.ledger.fills[0].ts, 5_000);
        assert_eq!(report.halted_ms, 1_500);
    }

    #[test]
    fn the_same_input_gives_the_same_fills() {
        let events = vec![
            snapshot(0),
            Event::Trade {
                ts: 2_000,
                price: 200,
                size: 150,
                side: Side::Bid,
            },
            Event::Trade {
                ts: 3_000,
                price: 202,
                size: 150,
                side: Side::Ask,
            },
        ];
        let mut a = Fixed {
            bid: 200,
            ask: 202,
            size: 10,
        };
        let mut b = Fixed {
            bid: 200,
            ask: 202,
            size: 10,
        };
        let first = Engine::new(config(1_000, QueueModel::Pessimistic)).run(&events, &mut a);
        let second = Engine::new(config(1_000, QueueModel::Pessimistic)).run(&events, &mut b);
        assert_eq!(first.fill_hash, second.fill_hash);
    }
}
