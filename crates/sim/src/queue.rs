use lob::{Qty, Side, Tick};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QueueModel {
    /// Filled as soon as a trade prints at our price. What most backtests assume.
    Naive,
    /// Only trades consume the queue ahead of us; cancels are assumed to be behind us.
    Pessimistic,
    /// Cancels remove queue ahead in proportion to the share of the level ahead of us.
    Proportional,
    /// A stated fraction of cancels, in percent, comes from ahead of us.
    ///
    /// Level-2 data cannot say where in the queue a cancel happened, so every
    /// fill model has to assume something about it, and the assumption is not
    /// falsifiable without the exchange's own fills. `Pessimistic` assumes
    /// none, an optimistic model assumes all, and the truth is in between.
    /// Rather than pick, the study sweeps this parameter and reports the
    /// result as a curve, so what the answer depends on is visible.
    FromAhead(u8),
}

#[derive(Debug, Clone)]
pub struct OwnOrder {
    pub side: Side,
    pub price: Tick,
    pub size: Qty,
    pub filled: Qty,
    /// Quantity ahead of us. Fractional because the proportional model removes
    /// a fraction of the queue at a time.
    pub queue_ahead: f64,
    /// Visible quantity at our price, our own order excluded, as of the last update.
    pub level_qty: f64,
    pub model: QueueModel,
}

impl OwnOrder {
    pub fn new(side: Side, price: Tick, size: Qty, level_qty: Qty, model: QueueModel) -> Self {
        Self {
            side,
            price,
            size,
            filled: 0,
            queue_ahead: level_qty as f64,
            level_qty: level_qty as f64,
            model,
        }
    }

    pub fn remaining(&self) -> Qty {
        self.size - self.filled
    }

    /// A trade printed at `price` for `size`, against the side of the book our
    /// order rests on. Returns the quantity it filled of our order.
    pub fn on_trade(&mut self, price: Tick, size: Qty) -> Qty {
        if self.remaining() == 0 {
            return 0;
        }
        let through = match self.side {
            Side::Bid => price < self.price,
            Side::Ask => price > self.price,
        };
        if through {
            let fill = self.remaining();
            self.filled += fill;
            return fill;
        }
        if price != self.price {
            return 0;
        }
        if self.model == QueueModel::Naive {
            let fill = self.remaining().min(size);
            self.filled += fill;
            return fill;
        }
        let consumed = (size as f64).min(self.queue_ahead);
        self.queue_ahead -= consumed;
        self.level_qty = (self.level_qty - size as f64).max(0.0);
        let leftover = size as f64 - consumed;
        if leftover <= 0.0 {
            return 0;
        }
        let fill = self.remaining().min(leftover.floor() as Qty);
        self.filled += fill;
        fill
    }

    /// The visible quantity at our price went from `old` to `new` without a trade,
    /// so the difference is cancels, or new orders joining behind us.
    pub fn on_level_change(&mut self, old: Qty, new: Qty) {
        let old = (old as f64 - self.remaining() as f64).max(0.0);
        let new = (new as f64 - self.remaining() as f64).max(0.0);
        if new >= old {
            self.level_qty = new;
            return;
        }
        let cancelled = old - new;
        match self.model {
            QueueModel::Naive | QueueModel::Pessimistic => {}
            QueueModel::FromAhead(pct) => {
                let share = f64::from(pct.min(100)) / 100.0;
                self.queue_ahead = (self.queue_ahead - (cancelled * share)).max(0.0);
            }
            QueueModel::Proportional => {
                let behind = (old - self.queue_ahead).max(0.0);
                let total = self.queue_ahead + behind;
                if total > 0.0 {
                    self.queue_ahead -= cancelled * (self.queue_ahead / total);
                    self.queue_ahead = self.queue_ahead.max(0.0);
                }
            }
        }
        self.level_qty = new;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lob::Side;

    #[test]
    fn pessimistic_waits_for_the_queue_to_trade_out() {
        let mut o = OwnOrder::new(Side::Bid, 100, 20, 50, QueueModel::Pessimistic);
        assert_eq!(o.on_trade(100, 30), 0);
        assert_eq!(o.queue_ahead, 20.0);
        assert_eq!(o.on_trade(100, 40), 20);
        assert_eq!(o.remaining(), 0);
    }

    #[test]
    fn naive_fills_on_the_first_print_at_our_price() {
        let mut o = OwnOrder::new(Side::Bid, 100, 20, 50, QueueModel::Naive);
        assert_eq!(o.on_trade(100, 30), 20);
    }

    #[test]
    fn a_trade_through_our_price_always_fills_us() {
        let mut o = OwnOrder::new(Side::Bid, 100, 20, 500, QueueModel::Pessimistic);
        assert_eq!(o.on_trade(99, 10), 20);
    }

    #[test]
    fn a_trade_on_the_far_side_does_not_touch_us() {
        let mut o = OwnOrder::new(Side::Bid, 100, 20, 50, QueueModel::Pessimistic);
        assert_eq!(o.on_trade(101, 10), 0);
        assert_eq!(o.queue_ahead, 50.0);
    }

    #[test]
    fn cancels_help_only_the_proportional_model() {
        let mut pess = OwnOrder::new(Side::Bid, 100, 10, 100, QueueModel::Pessimistic);
        let mut prop = OwnOrder::new(Side::Bid, 100, 10, 100, QueueModel::Proportional);
        pess.on_level_change(110, 70);
        prop.on_level_change(110, 70);
        assert_eq!(pess.queue_ahead, 100.0);
        assert!(prop.queue_ahead < 100.0 && prop.queue_ahead > 50.0);
    }

    #[test]
    fn the_cancel_fraction_brackets_the_other_models() {
        // 100 ahead of us, 40 lots cancel. Nothing from ahead is the
        // pessimistic case; everything from ahead is the optimistic one.
        let advance = |model| {
            let mut o = OwnOrder::new(Side::Bid, 100, 10, 100, model);
            o.on_level_change(110, 70);
            100.0 - o.queue_ahead
        };
        assert_eq!(advance(QueueModel::FromAhead(0)), 0.0);
        assert_eq!(advance(QueueModel::FromAhead(100)), 40.0);
        assert_eq!(advance(QueueModel::FromAhead(50)), 20.0);
        assert_eq!(
            advance(QueueModel::FromAhead(0)),
            advance(QueueModel::Pessimistic)
        );
    }

    #[test]
    fn a_larger_fraction_never_leaves_us_further_back() {
        let advance = |pct| {
            let mut o = OwnOrder::new(Side::Bid, 100, 10, 200, QueueModel::FromAhead(pct));
            o.on_level_change(210, 150);
            o.queue_ahead
        };
        let steps: Vec<f64> = (0..=100).step_by(10).map(|p| advance(p as u8)).collect();
        assert!(steps.windows(2).all(|w| w[1] <= w[0]));
    }

    #[test]
    fn the_queue_never_runs_past_zero() {
        let mut o = OwnOrder::new(Side::Bid, 100, 10, 20, QueueModel::FromAhead(100));
        o.on_level_change(30, 11);
        assert!(o.queue_ahead >= 0.0);
    }

    #[test]
    fn joins_behind_us_do_not_move_us_forward() {
        let mut o = OwnOrder::new(Side::Bid, 100, 10, 40, QueueModel::Proportional);
        o.on_level_change(50, 90);
        assert_eq!(o.queue_ahead, 40.0);
    }
}
