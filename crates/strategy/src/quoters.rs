use lob::{Qty, Tick};
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
