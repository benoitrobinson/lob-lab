use lob::{Book, Qty, Side, Tick};
use proptest::prelude::*;

/// A book nobody can get wrong: a flat vector scanned end to end.
#[derive(Default)]
struct RefBook {
    levels: Vec<(Side, Tick, Qty)>,
}

impl RefBook {
    fn set(&mut self, side: Side, price: Tick, amount: Qty) {
        self.levels.retain(|(s, p, _)| !(*s == side && *p == price));
        if amount > 0 {
            self.levels.push((side, price, amount));
        }
    }

    fn best(&self, side: Side) -> Option<(Tick, Qty)> {
        self.levels
            .iter()
            .filter(|(s, _, _)| *s == side)
            .map(|(_, p, q)| (*p, *q))
            .reduce(|a, b| match side {
                Side::Bid => {
                    if b.0 > a.0 {
                        b
                    } else {
                        a
                    }
                }
                Side::Ask => {
                    if b.0 < a.0 {
                        b
                    } else {
                        a
                    }
                }
            })
    }
}

proptest! {
    #[test]
    fn book_matches_reference(
        ops in prop::collection::vec(
            (any::<bool>(), 1i64..20, 0i64..5).prop_map(|(is_bid, price, amount)| {
                let side = if is_bid { Side::Bid } else { Side::Ask };
                (side, price, amount * 10)
            }),
            1..200,
        )
    ) {
        let mut book = Book::new();
        let mut reference = RefBook::default();
        for (side, price, amount) in ops {
            book.set(side, price, amount);
            reference.set(side, price, amount);
            prop_assert_eq!(book.best_bid(), reference.best(Side::Bid));
            prop_assert_eq!(book.best_ask(), reference.best(Side::Ask));
            prop_assert_eq!(book.qty_at(side, price), if amount > 0 { amount } else { 0 });
        }
    }
}
