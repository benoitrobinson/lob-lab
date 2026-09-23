use std::path::Path;

use lob::{Side, Tick};

use crate::event::{Event, Instrument, taker_side, to_qty, to_ticks};
use crate::gaps::{GapCheck, GapDetector};
use crate::messages::{BookMsg, Notification, QuoteMsg, TradeMsg};

fn rank(e: &Event) -> u8 {
    match e {
        Event::Trade { .. } => 0,
        Event::Snapshot { .. } | Event::Change { .. } | Event::Gap { .. } => 1,
        Event::Quote { .. } => 2,
    }
}

fn entries(raw: &[(String, f64, f64)], inst: &Instrument) -> Vec<(Tick, i64)> {
    raw.iter()
        .map(|(action, price, amount)| {
            let amount = if action == "delete" { 0.0 } else { *amount };
            (to_ticks(*price, inst), to_qty(amount, inst))
        })
        .collect()
}

pub fn parse_lines<I: Iterator<Item = String>>(lines: I, inst: &Instrument) -> Vec<Event> {
    let mut events = Vec::new();
    let mut gaps = GapDetector::default();
    for line in lines {
        let Ok(note) = serde_json::from_str::<Notification>(&line) else {
            continue;
        };
        let channel = note.params.channel.clone();
        if channel.starts_with("book.") {
            let Ok(msg) = serde_json::from_value::<BookMsg>(note.params.data) else {
                continue;
            };
            if let GapCheck::Missing { from, to } = gaps.check(&msg) {
                events.push(Event::Gap {
                    ts: msg.timestamp,
                    missing_from: from,
                    missing_to: to,
                });
            }
            let bids = entries(&msg.bids, inst);
            let asks = entries(&msg.asks, inst);
            events.push(if msg.kind == "snapshot" {
                Event::Snapshot {
                    ts: msg.timestamp,
                    bids,
                    asks,
                }
            } else {
                Event::Change {
                    ts: msg.timestamp,
                    bids,
                    asks,
                }
            });
        } else if channel.starts_with("trades.") {
            let Ok(trades) = serde_json::from_value::<Vec<TradeMsg>>(note.params.data) else {
                continue;
            };
            for t in trades {
                events.push(Event::Trade {
                    ts: t.timestamp,
                    price: to_ticks(t.price, inst),
                    size: to_qty(t.amount, inst),
                    side: taker_side(&t.direction),
                });
            }
        } else if channel.starts_with("quote.") {
            let Ok(q) = serde_json::from_value::<QuoteMsg>(note.params.data) else {
                continue;
            };
            events.push(Event::Quote {
                ts: q.timestamp,
                bid: q
                    .best_bid_price
                    .map(|p| (to_ticks(p, inst), to_qty(q.best_bid_amount, inst))),
                ask: q
                    .best_ask_price
                    .map(|p| (to_ticks(p, inst), to_qty(q.best_ask_amount, inst))),
            });
        }
    }
    events.sort_by_key(|e| (e.ts(), rank(e)));
    events
}

/// Decompresses one recorded file, keeping whatever is readable.
///
/// A recorder that is killed leaves its last zstd frame unterminated, and a
/// strict reader throws away the whole hour rather than the last few seconds.
/// The recorder flushes as it goes so that everything up to the last flush is
/// recoverable, and this reads up to the break and says how much it kept. The
/// trailing partial line is dropped: half a JSON object is not data.
pub fn read_file(path: &Path) -> anyhow::Result<Vec<String>> {
    let file = std::fs::File::open(path)?;
    let mut decoder = zstd::stream::read::Decoder::new(file)?;
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match std::io::Read::read(&mut decoder, &mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(e) => {
                eprintln!(
                    "{}: {e}; keeping the {} bytes read before the break",
                    path.display(),
                    bytes.len()
                );
                break;
            }
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    let complete = match text.rfind('\n') {
        Some(end) => &text[..end],
        None => return Ok(Vec::new()),
    };
    Ok(complete.lines().map(|s| s.to_string()).collect())
}

/// Every recorded line in a directory, in hour order.
pub fn read_lines(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".jsonl.zst"))
        .collect();
    files.sort();
    let mut lines = Vec::new();
    for path in files {
        lines.extend(read_file(&path)?);
    }
    Ok(lines)
}

/// Reads every `raw-*.jsonl.zst` in a directory, in hour order.
pub fn read_dir(dir: &Path, inst: &Instrument) -> anyhow::Result<Vec<Event>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".jsonl.zst"))
        .collect();
    files.sort();
    let mut lines = Vec::new();
    for path in files {
        lines.extend(read_file(&path)?);
    }
    Ok(parse_lines(lines.into_iter(), inst))
}

/// The silent-failure detector: the book we rebuild must equal the book the
/// exchange publishes on the quote channel.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReplayStats {
    pub quotes_checked: u64,
    pub quotes_mismatched: u64,
    pub gaps: u64,
}

impl ReplayStats {
    pub fn mismatch_rate(&self) -> f64 {
        if self.quotes_checked == 0 {
            return 0.0;
        }
        self.quotes_mismatched as f64 / self.quotes_checked as f64
    }
}

pub fn verify(events: &[Event]) -> ReplayStats {
    use lob::Book;
    let mut book = Book::new();
    let mut stats = ReplayStats::default();
    let mut seen_snapshot = false;
    for event in events {
        match event {
            Event::Gap { .. } => {
                stats.gaps += 1;
                seen_snapshot = false;
                book.clear();
            }
            Event::Snapshot { bids, asks, .. } => {
                book.clear();
                for (p, q) in bids {
                    book.set(Side::Bid, *p, *q);
                }
                for (p, q) in asks {
                    book.set(Side::Ask, *p, *q);
                }
                seen_snapshot = true;
            }
            Event::Change { bids, asks, .. } => {
                for (p, q) in bids {
                    book.set(Side::Bid, *p, *q);
                }
                for (p, q) in asks {
                    book.set(Side::Ask, *p, *q);
                }
            }
            Event::Quote { bid, ask, .. } => {
                if !seen_snapshot {
                    continue;
                }
                stats.quotes_checked += 1;
                let ours = (
                    book.best_bid().map(|(p, _)| p),
                    book.best_ask().map(|(p, _)| p),
                );
                let theirs = (bid.map(|(p, _)| p), ask.map(|(p, _)| p));
                if ours != theirs {
                    stats.quotes_mismatched += 1;
                }
            }
            Event::Trade { .. } => {}
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use lob::Side;

    const LINES: &[&str] = &[
        r#"{"params":{"channel":"book.BTC-PERPETUAL.100ms","data":{"type":"snapshot","timestamp":100,"change_id":1,"bids":[["new",100.0,50.0]],"asks":[["new",101.0,40.0]]}}}"#,
        r#"{"params":{"channel":"quote.BTC-PERPETUAL","data":{"timestamp":100,"best_bid_price":100.0,"best_bid_amount":50.0,"best_ask_price":101.0,"best_ask_amount":40.0}}}"#,
        r#"{"params":{"channel":"trades.BTC-PERPETUAL.100ms","data":[{"trade_seq":1,"timestamp":100,"price":101.0,"amount":10.0,"direction":"buy"}]}}"#,
    ];

    #[test]
    fn orders_trades_before_book_changes_at_equal_timestamps() {
        let events = parse_lines(
            LINES.iter().map(|s| s.to_string()),
            &Instrument::btc_perpetual(),
        );
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| match e {
                Event::Trade { .. } => "trade",
                Event::Snapshot { .. } => "snapshot",
                Event::Change { .. } => "change",
                Event::Quote { .. } => "quote",
                Event::Gap { .. } => "gap",
            })
            .collect();
        assert_eq!(kinds, vec!["trade", "snapshot", "quote"]);
    }

    #[test]
    fn a_trade_carries_the_side_it_consumed() {
        let events = parse_lines(
            LINES.iter().map(|s| s.to_string()),
            &Instrument::btc_perpetual(),
        );
        match events
            .iter()
            .find(|e| matches!(e, Event::Trade { .. }))
            .unwrap()
        {
            Event::Trade {
                side, size, price, ..
            } => {
                assert_eq!(*side, Side::Ask);
                assert_eq!(*size, 10);
                assert_eq!(*price, 202);
            }
            _ => unreachable!(),
        }
    }
}
