/// Fitted arrival intensity: lambda(delta) = a * exp(-kappa * delta), with
/// delta the distance in USD from the mid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Intensity {
    pub a: f64,
    pub kappa: f64,
}

impl Intensity {
    pub fn is_degenerate(&self) -> bool {
        !(self.a.is_finite() && self.kappa.is_finite() && self.a > 0.0 && self.kappa > 0.0)
    }

    pub fn lambda(&self, delta: f64) -> f64 {
        self.a * (-self.kappa * delta).exp()
    }
}

/// Least squares of `ln lambda` on `delta`, where `lambda(delta)` is the count
/// of trades at distance `delta` or more, divided by the elapsed time in
/// seconds.
///
/// `distances` are per-trade distances from the mid in USD, `elapsed_secs` the
/// window they were observed over, `bucket` the bucket width in USD, and
/// `buckets` how many to use.
pub fn fit(distances: &[f64], elapsed_secs: f64, bucket: f64, buckets: usize) -> Intensity {
    let mut xs = Vec::with_capacity(buckets);
    let mut ys = Vec::with_capacity(buckets);
    for i in 0..buckets {
        let edge = i as f64 * bucket;
        let count = distances.iter().filter(|d| **d >= edge).count();
        if count == 0 {
            continue;
        }
        xs.push(edge);
        ys.push((count as f64 / elapsed_secs).ln());
    }
    if xs.len() < 2 {
        return Intensity {
            a: f64::NAN,
            kappa: f64::NAN,
        };
    }
    let n = xs.len() as f64;
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let mut sxy = 0.0;
    let mut sxx = 0.0;
    for (x, y) in xs.iter().zip(ys.iter()) {
        sxy += (x - mean_x) * (y - mean_y);
        sxx += (x - mean_x) * (x - mean_x);
    }
    let slope = if sxx == 0.0 { f64::NAN } else { sxy / sxx };
    Intensity {
        a: (mean_y - slope * mean_x).exp(),
        kappa: -slope,
    }
}

/// Distances from the mid of every trade in a replay, and the window they were
/// observed over, for the fit above.
pub fn trade_distances(events: &[feed::event::Event], tick_size: f64) -> (Vec<f64>, f64) {
    use feed::event::Event;
    use lob::{Book, Side};
    let mut book = Book::new();
    let mut out = Vec::new();
    let mut first = None;
    let mut last = 0i64;
    for e in events {
        last = e.ts();
        first.get_or_insert(last);
        match e {
            Event::Snapshot { bids, asks, .. } => {
                book.clear();
                for (p, q) in bids {
                    book.set(Side::Bid, *p, *q);
                }
                for (p, q) in asks {
                    book.set(Side::Ask, *p, *q);
                }
            }
            Event::Change { bids, asks, .. } => {
                for (p, q) in bids {
                    book.set(Side::Bid, *p, *q);
                }
                for (p, q) in asks {
                    book.set(Side::Ask, *p, *q);
                }
            }
            Event::Trade { price, .. } => {
                if let Some(mid2) = book.mid2() {
                    let mid = mid2 as f64 * tick_size / 2.0;
                    out.push((*price as f64 * tick_size - mid).abs());
                }
            }
            _ => {}
        }
    }
    let elapsed = (last - first.unwrap_or(last)) as f64 / 1_000.0;
    (out, elapsed.max(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic exponential distances: the inverse CDF of Exp(kappa)
    /// evaluated on a regular grid, which is what a perfect sample looks like.
    fn distances(n: usize, kappa: f64) -> Vec<f64> {
        (1..=n)
            .map(|i| {
                let u = i as f64 / (n as f64 + 1.0);
                -(1.0 - u).ln() / kappa
            })
            .collect()
    }

    #[test]
    fn recovers_the_decay_of_a_known_sample() {
        let kappa = 2.0;
        let d = distances(5_000, kappa);
        let fit = fit(&d, 10_000.0, 0.5, 8);
        assert!(
            (fit.kappa - kappa).abs() / kappa < 0.15,
            "kappa was {}",
            fit.kappa
        );
        assert!(fit.a > 0.0);
    }

    #[test]
    fn a_denser_flow_raises_a_but_not_kappa() {
        let d = distances(5_000, 2.0);
        let slow = fit(&d, 20_000.0, 0.5, 8);
        let fast = fit(&d, 10_000.0, 0.5, 8);
        assert!(fast.a > slow.a);
        assert!((fast.kappa - slow.kappa).abs() < 1e-9);
    }

    #[test]
    fn an_empty_sample_is_not_a_fit() {
        assert!(fit(&[], 1_000.0, 0.5, 8).is_degenerate());
    }
}
