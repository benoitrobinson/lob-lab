use lob::{Qty, Tick};
use signal::ofi::{Ofi, Touch};
use sim::engine::{Ctx, Quoter, Quotes};

/// Fixed distance from the mid, no inventory term. The control.
pub struct Symmetric {
    pub half_spread_ticks: i64,
    pub size: Qty,
}

impl Quoter for Symmetric {
    fn quote(&mut self, ctx: &Ctx) -> Option<Quotes> {
        let mid2 = ctx.book.mid2()?;
        // mid2 is twice the mid in ticks, so halve after adding the offset.
        let bid = (mid2 - 2 * self.half_spread_ticks).div_euclid(2);
        let ask = (mid2 + 2 * self.half_spread_ticks).div_euclid(2);
        Some(Quotes {
            bid: Some(bid),
            ask: Some(ask),
            size: self.size,
        })
    }
}

/// Joins the best bid and the best ask, which is where queue position decides
/// everything. A quoter that rests behind the touch is filled almost entirely
/// by trades sweeping through it, and a sweep fills you whatever your queue
/// position was, so the fill model stops mattering. That is measurable: with
/// `Symmetric { half_spread_ticks: 2 }` all three fill models produce byte
/// identical P&L.
pub struct JoinTouch {
    /// 0 joins the touch. 1 rests one tick behind it.
    pub offset_ticks: i64,
    pub size: Qty,
}

impl Quoter for JoinTouch {
    fn quote(&mut self, ctx: &Ctx) -> Option<Quotes> {
        let (bid, _) = ctx.book.best_bid()?;
        let (ask, _) = ctx.book.best_ask()?;
        Some(Quotes {
            bid: Some(bid - self.offset_ticks),
            ask: Some(ask + self.offset_ticks),
            size: self.size,
        })
    }
}

/// Gueant, Lehalle and Fernandez-Tapia (2013) steady-state quotes, ported from
/// `vol-lab`'s `glft_half_spreads` without changing the formula, so the two
/// experiments differ only in how fills happen.
pub struct Glft {
    pub gamma: f64,
    /// Volatility in USD per square root of a second.
    pub sigma: f64,
    pub kappa: f64,
    pub a: f64,
    pub tick_size: f64,
}

impl Glft {
    /// Returns (ask half-spread, bid half-spread) in USD for inventory `q`,
    /// expressed in units of the quote size.
    pub fn half_spreads(&self, q: f64) -> (f64, f64) {
        let base = (1.0 + self.gamma / self.kappa).ln() / self.gamma;
        let inner = self.sigma.powi(2) * self.gamma / (2.0 * self.kappa * self.a)
            * (1.0 + self.gamma / self.kappa).powf(1.0 + self.kappa / self.gamma);
        let scale = inner.max(0.0).sqrt();
        let ask = base - (2.0 * q - 1.0) / 2.0 * scale;
        let bid = base + (2.0 * q + 1.0) / 2.0 * scale;
        (ask, bid)
    }
}

pub struct GlftQuoter {
    pub model: Glft,
    pub size: Qty,
}

impl Quoter for GlftQuoter {
    fn quote(&mut self, ctx: &Ctx) -> Option<Quotes> {
        let mid2 = ctx.book.mid2()?;
        let mid = mid2 as f64 * self.model.tick_size / 2.0;
        let q = ctx.position_usd as f64 / self.size.max(1) as f64;
        let (ask_usd, bid_usd) = self.model.half_spreads(q);
        let to_tick = |price: f64| -> Tick { (price / self.model.tick_size).round() as Tick };
        Some(Quotes {
            bid: Some(to_tick(mid - bid_usd)),
            ask: Some(to_tick(mid + ask_usd)),
            size: self.size,
        })
    }
}

/// Joins the touch, but stands aside on the side the flow is about to run
/// over.
///
/// This is the answer to `vol-lab`'s finding that inventory skew does nothing
/// about adverse selection. Skew is a function of the position; being picked
/// off is a property of the next fill, and the only defence is a signal about
/// it. Order flow imbalance is the cheapest one, and it comes from the feed
/// the book is already rebuilt from.
///
/// The trade is explicit: quoting one side less often gives up fills, and the
/// question the study asks is whether the markout saved is worth more than the
/// spread given up.
pub struct SignalQuoter {
    pub ofi: Ofi,
    /// Imbalance beyond which the threatened side is pulled, in the same units
    /// as the imbalance itself. The study sets it from the day's own spread of
    /// imbalance rather than from a number chosen in advance.
    pub threshold: f64,
    pub size: Qty,
    /// Counts how often each side was pulled, so the cost of the defence is
    /// visible rather than inferred.
    pub pulled_bid: u64,
    pub pulled_ask: u64,
    /// The largest imbalance the quoter ever saw, so a threshold that never
    /// fires is visible as such rather than as a strategy that chose not to.
    pub max_abs: f64,
    pub observations: u64,
}

impl SignalQuoter {
    pub fn new(window_ms: i64, threshold: f64, size: Qty) -> Self {
        Self {
            ofi: Ofi::new(window_ms),
            threshold,
            size,
            pulled_bid: 0,
            pulled_ask: 0,
            max_abs: 0.0,
            observations: 0,
        }
    }
}

impl Quoter for SignalQuoter {
    fn observe(&mut self, ctx: &Ctx) {
        let (Some((bid, bid_qty)), Some((ask, ask_qty))) =
            (ctx.book.best_bid(), ctx.book.best_ask())
        else {
            return;
        };
        let value = self.ofi.update(
            ctx.ts,
            Touch {
                bid,
                bid_qty,
                ask,
                ask_qty,
            },
        );
        self.observations += 1;
        self.max_abs = self.max_abs.max(value.abs());
    }

    fn quote(&mut self, ctx: &Ctx) -> Option<Quotes> {
        let (bid, _) = ctx.book.best_bid()?;
        let (ask, _) = ctx.book.best_ask()?;
        let imbalance = self.ofi.value();
        let mut quotes = Quotes {
            bid: Some(bid),
            ask: Some(ask),
            size: self.size,
        };
        if imbalance > self.threshold {
            // Buyers are arriving. Selling to them at the touch is selling
            // into the move.
            quotes.ask = None;
            self.pulled_ask += 1;
        } else if imbalance < -self.threshold {
            quotes.bid = None;
            self.pulled_bid += 1;
        }
        Some(quotes)
    }

    fn report(&self) -> Vec<(&'static str, f64)> {
        vec![
            ("pulled_bid", self.pulled_bid as f64),
            ("pulled_ask", self.pulled_ask as f64),
            ("max_abs_imbalance", self.max_abs),
            ("observations", self.observations as f64),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glft() -> Glft {
        Glft {
            gamma: 0.1,
            sigma: 500.0,
            kappa: 1.5,
            a: 140.0,
            tick_size: 0.5,
        }
    }

    #[test]
    fn a_flat_dealer_quotes_almost_symmetrically() {
        let (ask, bid) = glft().half_spreads(0.0);
        // The steady-state form carries a (2q + 1)/2 asymmetry, so a flat
        // dealer is already skewed by half a unit of the inventory term.
        assert!((ask - bid).abs() < 0.5 * (ask + bid));
    }

    /// The test `vol-lab` keeps because it caught the sign twice: a long dealer
    /// quotes a closer ask and a further bid.
    #[test]
    fn every_quoter_leans_against_inventory() {
        let (ask_long, bid_long) = glft().half_spreads(1.0);
        let (ask_short, bid_short) = glft().half_spreads(-1.0);
        assert!(ask_long < bid_long, "a long dealer must quote a closer ask");
        assert!(
            bid_short < ask_short,
            "a short dealer must quote a closer bid"
        );
    }

    #[test]
    fn a_wider_gamma_widens_the_inventory_term() {
        let skew = |gamma: f64| {
            let g = Glft { gamma, ..glft() };
            let (a, b) = g.half_spreads(1.0);
            b - a
        };
        assert!(skew(0.5) > skew(0.05));
    }

    #[test]
    fn quotes_round_to_ticks_inside_the_spread() {
        let mut s = Symmetric {
            half_spread_ticks: 2,
            size: 10,
        };
        let mut book = lob::Book::new();
        book.set(lob::Side::Bid, 200, 100);
        book.set(lob::Side::Ask, 210, 100);
        let ctx = Ctx {
            ts: 0,
            book: &book,
            position_usd: 0,
        };
        let q = s.quote(&ctx).unwrap();
        assert_eq!(q.bid, Some(203));
        assert_eq!(q.ask, Some(207));
    }

    #[test]
    fn joining_the_touch_quotes_at_the_best_prices() {
        let mut book = lob::Book::new();
        book.set(lob::Side::Bid, 200, 100);
        book.set(lob::Side::Ask, 210, 100);
        let ctx = Ctx {
            ts: 0,
            book: &book,
            position_usd: 0,
        };
        let q = JoinTouch {
            offset_ticks: 0,
            size: 10,
        }
        .quote(&ctx)
        .unwrap();
        assert_eq!(q.bid, Some(200));
        assert_eq!(q.ask, Some(210));

        let behind = JoinTouch {
            offset_ticks: 1,
            size: 10,
        }
        .quote(&ctx)
        .unwrap();
        assert_eq!(behind.bid, Some(199));
        assert_eq!(behind.ask, Some(211));
    }

    fn book_at(bid_qty: lob::Qty, ask_qty: lob::Qty) -> lob::Book {
        let mut book = lob::Book::new();
        book.set(lob::Side::Bid, 200, bid_qty);
        book.set(lob::Side::Ask, 210, ask_qty);
        book
    }

    #[test]
    fn a_quiet_book_leaves_both_sides_quoted() {
        let mut q = SignalQuoter::new(1_000, 100.0, 10);
        let book = book_at(50, 50);
        let ctx = Ctx {
            ts: 0,
            book: &book,
            position_usd: 0,
        };
        q.observe(&ctx);
        let quotes = q.quote(&ctx).unwrap();
        assert_eq!(quotes.bid, Some(200));
        assert_eq!(quotes.ask, Some(210));
    }

    #[test]
    fn buyers_arriving_pull_the_ask() {
        let mut q = SignalQuoter::new(1_000, 100.0, 10);
        let first = book_at(50, 50);
        q.observe(&Ctx {
            ts: 0,
            book: &first,
            position_usd: 0,
        });
        // Size piles onto the bid and leaves the ask: demand.
        let second = book_at(300, 10);
        let ctx = Ctx {
            ts: 100,
            book: &second,
            position_usd: 0,
        };
        q.observe(&ctx);
        let quotes = q.quote(&ctx).unwrap();
        assert_eq!(quotes.bid, Some(200), "the safe side stays quoted");
        assert_eq!(quotes.ask, None, "the threatened side is pulled");
        assert_eq!(q.pulled_ask, 1);
        assert_eq!(q.pulled_bid, 0);
    }

    #[test]
    fn sellers_arriving_pull_the_bid() {
        let mut q = SignalQuoter::new(1_000, 100.0, 10);
        let first = book_at(50, 50);
        q.observe(&Ctx {
            ts: 0,
            book: &first,
            position_usd: 0,
        });
        let second = book_at(10, 300);
        let ctx = Ctx {
            ts: 100,
            book: &second,
            position_usd: 0,
        };
        q.observe(&ctx);
        let quotes = q.quote(&ctx).unwrap();
        assert_eq!(quotes.bid, None);
        assert_eq!(quotes.ask, Some(210));
        assert_eq!(q.pulled_bid, 1);
    }

    #[test]
    fn a_high_threshold_never_pulls() {
        let mut q = SignalQuoter::new(1_000, 1e9, 10);
        let first = book_at(50, 50);
        q.observe(&Ctx {
            ts: 0,
            book: &first,
            position_usd: 0,
        });
        let second = book_at(5_000, 1);
        let ctx = Ctx {
            ts: 100,
            book: &second,
            position_usd: 0,
        };
        q.observe(&ctx);
        let quotes = q.quote(&ctx).unwrap();
        assert!(quotes.bid.is_some() && quotes.ask.is_some());
        let pulled: f64 = q
            .report()
            .iter()
            .filter(|(k, _)| k.starts_with("pulled"))
            .map(|(_, v)| v)
            .sum();
        assert_eq!(pulled, 0.0);
        assert!(q.observations > 0, "it still watched the book");
    }

    #[test]
    fn a_long_position_moves_both_quotes_down() {
        let mut q = GlftQuoter {
            model: glft(),
            size: 100,
        };
        let mut book = lob::Book::new();
        book.set(lob::Side::Bid, 200_000, 100);
        book.set(lob::Side::Ask, 200_010, 100);
        let flat = q
            .quote(&Ctx {
                ts: 0,
                book: &book,
                position_usd: 0,
            })
            .unwrap();
        let long = q
            .quote(&Ctx {
                ts: 0,
                book: &book,
                position_usd: 500,
            })
            .unwrap();
        assert!(long.bid < flat.bid);
        assert!(long.ask < flat.ask);
    }
}
