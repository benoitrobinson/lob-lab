//! Order flow imbalance, after Cont, Kukanov and Stoikov (2014).
//!
//! `vol-lab` measured, on simulated flow, that leaning quotes against inventory
//! does nothing at all about adverse selection: all three quoting rules marked
//! out identically, because a quoting rule is a function of the position and
//! information is a property of the next fill. If anything is to help, it has
//! to be a signal about that next fill.
//!
//! Order flow imbalance is the cheapest such signal, and it is computed from
//! the same feed the book is rebuilt from. Each book update contributes the
//! change in demand at the touch: size added to the bid and size pulled from
//! the ask push the price up, and the reverse pushes it down. Summed over a
//! window it is the imbalance, and it predicts the next move over horizons of
//! seconds.

use std::collections::VecDeque;

use lob::{Qty, Tick};

/// The top of the book at one instant.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Touch {
    pub bid: Tick,
    pub bid_qty: Qty,
    pub ask: Tick,
    pub ask_qty: Qty,
}

/// Rolling sum of the per-update contributions over a time window.
pub struct Ofi {
    window_ms: i64,
    prev: Option<Touch>,
    events: VecDeque<(i64, f64)>,
    sum: f64,
}

impl Ofi {
    pub fn new(window_ms: i64) -> Self {
        Self {
            window_ms,
            prev: None,
            events: VecDeque::new(),
            sum: 0.0,
        }
    }

    /// The contribution of a single transition, which is the whole model:
    ///
    /// - the bid improves, so every lot resting there is new demand;
    /// - the bid holds, so only the change in size is new demand;
    /// - the bid is taken out or cancelled, so all of the old size is gone.
    ///
    /// The ask is the mirror image, entering with the opposite sign.
    pub fn contribution(prev: Touch, now: Touch) -> f64 {
        let bid = if now.bid > prev.bid {
            now.bid_qty as f64
        } else if now.bid == prev.bid {
            (now.bid_qty - prev.bid_qty) as f64
        } else {
            -(prev.bid_qty as f64)
        };
        let ask = if now.ask < prev.ask {
            -(now.ask_qty as f64)
        } else if now.ask == prev.ask {
            -((now.ask_qty - prev.ask_qty) as f64)
        } else {
            prev.ask_qty as f64
        };
        bid + ask
    }

    /// Feeds one book update and returns the rolling imbalance.
    pub fn update(&mut self, ts: i64, now: Touch) -> f64 {
        let e = match self.prev {
            None => 0.0,
            Some(prev) => Self::contribution(prev, now),
        };
        self.prev = Some(now);
        if e != 0.0 {
            self.events.push_back((ts, e));
            self.sum += e;
        }
        while let Some((t0, e0)) = self.events.front().copied() {
            if ts - t0 > self.window_ms {
                self.events.pop_front();
                self.sum -= e0;
            } else {
                break;
            }
        }
        self.sum
    }

    pub fn value(&self) -> f64 {
        self.sum
    }

    pub fn reset(&mut self) {
        self.prev = None;
        self.events.clear();
        self.sum = 0.0;
    }
}

/// Ordinary least squares of `y` on `x` with an intercept, returning
/// (slope, r_squared, n). Written here rather than pulled in, because the
/// regression is four lines and a dependency would be four lines plus a
/// version.
pub fn regress(x: &[f64], y: &[f64]) -> (f64, f64, usize) {
    let n = x.len().min(y.len());
    if n < 3 {
        return (f64::NAN, f64::NAN, n);
    }
    let (xs, ys) = (&x[..n], &y[..n]);
    let mx = xs.iter().sum::<f64>() / n as f64;
    let my = ys.iter().sum::<f64>() / n as f64;
    let mut sxy = 0.0;
    let mut sxx = 0.0;
    let mut syy = 0.0;
    for i in 0..n {
        let dx = xs[i] - mx;
        let dy = ys[i] - my;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    if sxx == 0.0 || syy == 0.0 {
        return (f64::NAN, f64::NAN, n);
    }
    let slope = sxy / sxx;
    (slope, sxy * sxy / (sxx * syy), n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(bid: Tick, bid_qty: Qty, ask: Tick, ask_qty: Qty) -> Touch {
        Touch {
            bid,
            bid_qty,
            ask,
            ask_qty,
        }
    }

    #[test]
    fn size_joining_the_bid_is_demand() {
        let before = touch(100, 50, 101, 50);
        let after = touch(100, 80, 101, 50);
        assert_eq!(Ofi::contribution(before, after), 30.0);
    }

    #[test]
    fn size_leaving_the_bid_is_supply() {
        let before = touch(100, 50, 101, 50);
        let after = touch(100, 20, 101, 50);
        assert_eq!(Ofi::contribution(before, after), -30.0);
    }

    #[test]
    fn a_better_bid_counts_all_of_its_size() {
        let before = touch(100, 50, 101, 50);
        let after = touch(101, 10, 102, 50);
        // The whole new bid is demand; the ask moved up, so the old ask size
        // is demand too: nobody is selling there any more.
        assert_eq!(Ofi::contribution(before, after), 10.0 + 50.0);
    }

    #[test]
    fn the_book_is_antisymmetric() {
        // Mirror the book around the mid and the imbalance changes sign.
        let a = Ofi::contribution(touch(100, 50, 101, 50), touch(100, 80, 101, 50));
        let b = Ofi::contribution(touch(100, 50, 101, 50), touch(100, 50, 101, 80));
        assert_eq!(a, -b);
    }

    #[test]
    fn an_untouched_book_contributes_nothing() {
        let t = touch(100, 50, 101, 50);
        assert_eq!(Ofi::contribution(t, t), 0.0);
    }

    #[test]
    fn the_window_forgets() {
        let mut ofi = Ofi::new(1_000);
        ofi.update(0, touch(100, 50, 101, 50));
        assert_eq!(ofi.update(100, touch(100, 90, 101, 50)), 40.0);
        assert_eq!(ofi.update(500, touch(100, 110, 101, 50)), 60.0);
        // At t = 1200 the first contribution is older than the window.
        assert_eq!(ofi.update(1_200, touch(100, 110, 101, 50)), 20.0);
    }

    #[test]
    fn the_first_update_has_nothing_to_compare_against() {
        let mut ofi = Ofi::new(1_000);
        assert_eq!(ofi.update(0, touch(100, 50, 101, 50)), 0.0);
    }

    #[test]
    fn regression_recovers_a_known_slope() {
        let x: Vec<f64> = (0..100).map(|i| i as f64).collect();
        let y: Vec<f64> = x.iter().map(|v| 3.0 * v + 7.0).collect();
        let (slope, r2, n) = regress(&x, &y);
        assert!((slope - 3.0).abs() < 1e-9);
        assert!((r2 - 1.0).abs() < 1e-9);
        assert_eq!(n, 100);
    }

    #[test]
    fn regression_on_noise_explains_nothing() {
        let x: Vec<f64> = (0..200).map(|i| ((i * 37) % 101) as f64).collect();
        let y: Vec<f64> = (0..200).map(|i| ((i * 53) % 97) as f64).collect();
        let (_, r2, _) = regress(&x, &y);
        assert!(r2 < 0.05, "r2 was {r2}");
    }
}
