use crate::messages::BookMsg;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum GapCheck {
    /// The message follows the previous one.
    Ok,
    /// A snapshot: the book starts again from here.
    Restart,
    /// Updates between `from` and `to` were never delivered.
    Missing { from: u64, to: u64 },
}

#[derive(Debug, Default)]
pub struct GapDetector {
    last: Option<u64>,
}

impl GapDetector {
    pub fn check(&mut self, msg: &BookMsg) -> GapCheck {
        let result = match (self.last, msg.prev_change_id) {
            (_, None) => GapCheck::Restart,
            (None, Some(_)) => GapCheck::Restart,
            (Some(last), Some(prev)) if prev == last => GapCheck::Ok,
            (Some(last), Some(prev)) => GapCheck::Missing {
                from: last,
                to: prev,
            },
        };
        self.last = Some(msg.change_id);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(change_id: u64, prev: Option<u64>) -> BookMsg {
        BookMsg {
            kind: if prev.is_some() {
                "change".into()
            } else {
                "snapshot".into()
            },
            timestamp: 0,
            change_id,
            prev_change_id: prev,
            bids: vec![],
            asks: vec![],
        }
    }

    #[test]
    fn a_contiguous_stream_has_no_gaps() {
        let mut d = GapDetector::default();
        assert_eq!(d.check(&change(1, None)), GapCheck::Restart);
        assert_eq!(d.check(&change(2, Some(1))), GapCheck::Ok);
        assert_eq!(d.check(&change(3, Some(2))), GapCheck::Ok);
    }

    #[test]
    fn a_skipped_change_id_is_reported() {
        let mut d = GapDetector::default();
        d.check(&change(1, None));
        assert_eq!(
            d.check(&change(9, Some(7))),
            GapCheck::Missing { from: 1, to: 7 }
        );
    }

    #[test]
    fn a_snapshot_resets_the_detector() {
        let mut d = GapDetector::default();
        d.check(&change(1, None));
        d.check(&change(9, Some(7)));
        assert_eq!(d.check(&change(20, None)), GapCheck::Restart);
        assert_eq!(d.check(&change(21, Some(20))), GapCheck::Ok);
    }
}
