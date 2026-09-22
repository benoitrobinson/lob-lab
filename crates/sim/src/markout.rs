use lob::Side;

use crate::ledger::Fill;

/// Mid prices in USD, sorted by timestamp.
pub struct MidSeries {
    points: Vec<(i64, f64)>,
}

impl MidSeries {
    pub fn new(mut points: Vec<(i64, f64)>) -> Self {
        points.sort_by_key(|(ts, _)| *ts);
        Self { points }
    }

    /// The last mid at or before `ts`, or None if the series starts later.
    pub fn at(&self, ts: i64) -> Option<f64> {
        match self.points.binary_search_by_key(&ts, |(t, _)| *t) {
            Ok(i) => Some(self.points[i].1),
            Err(0) => None,
            Err(i) => Some(self.points[i - 1].1),
        }
    }

    pub fn last_ts(&self) -> Option<i64> {
        self.points.last().map(|(t, _)| *t)
    }
}

pub fn markouts_bps(fills: &[Fill], mids: &MidSeries, horizons_ms: &[i64]) -> Vec<f64> {
    let last = mids.last_ts().unwrap_or(i64::MIN);
    horizons_ms
        .iter()
        .map(|h| {
            let mut sum = 0.0;
            let mut n = 0u32;
            for f in fills {
                let target = f.ts + h;
                if target > last {
                    continue;
                }
                let Some(mid) = mids.at(target) else { continue };
                let sign = match f.side {
                    Side::Bid => 1.0,
                    Side::Ask => -1.0,
                };
                sum += sign * (mid - f.price_usd) / f.price_usd * 10_000.0;
                n += 1;
            }
            if n == 0 { f64::NAN } else { sum / n as f64 }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lob::Side;

    fn series() -> MidSeries {
        MidSeries::new(vec![(0, 100.0), (1_000, 101.0), (10_000, 90.0)])
    }

    #[test]
    fn a_buy_before_a_rise_has_a_positive_markout() {
        let fills = vec![Fill {
            ts: 0,
            side: Side::Bid,
            price_usd: 100.0,
            size_usd: 10,
            fee_btc: 0.0,
        }];
        let m = markouts_bps(&fills, &series(), &[1_000]);
        assert!((m[0] - 100.0).abs() < 1e-9);
    }

    #[test]
    fn a_sell_before_a_rise_has_a_negative_markout() {
        let fills = vec![Fill {
            ts: 0,
            side: Side::Ask,
            price_usd: 100.0,
            size_usd: 10,
            fee_btc: 0.0,
        }];
        let m = markouts_bps(&fills, &series(), &[1_000]);
        assert!((m[0] + 100.0).abs() < 1e-9);
    }

    #[test]
    fn the_mid_used_is_the_last_one_at_or_before_the_horizon() {
        let fills = vec![Fill {
            ts: 0,
            side: Side::Bid,
            price_usd: 100.0,
            size_usd: 10,
            fee_btc: 0.0,
        }];
        let m = markouts_bps(&fills, &series(), &[5_000]);
        assert!((m[0] - 100.0).abs() < 1e-9);
    }

    #[test]
    fn fills_without_a_future_mid_are_skipped() {
        let fills = vec![Fill {
            ts: 10_000,
            side: Side::Bid,
            price_usd: 100.0,
            size_usd: 10,
            fee_btc: 0.0,
        }];
        let m = markouts_bps(&fills, &series(), &[1_000]);
        assert!(m[0].is_nan());
    }
}
