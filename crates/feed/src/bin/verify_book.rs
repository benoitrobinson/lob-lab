//! Two checks on the rebuilt book, one of them decisive.
//!
//! The quote channel publishes the exchange's own best bid and ask, but on its
//! own schedule. Comparing it against a book rebuilt from a 100ms aggregated
//! feed compares two different moments, so disagreement is the normal state of
//! affairs rather than a defect. It is reported here as drift, not as a gate.
//!
//! The REST snapshot is the decisive one. It carries the `change_id` it was
//! taken at, and that is the same sequence the book channel advances, so
//! replaying the feed to that exact id and comparing states compares like with
//! like. If those disagree, the reconstruction is wrong.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::Parser;
use feed::event::Instrument;
use feed::replay;
use serde::Deserialize;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "data")]
    dir: PathBuf,
    /// How many levels of each REST snapshot to compare.
    #[arg(long, default_value_t = 10)]
    depth: usize,
}

#[derive(Deserialize)]
struct RestLine {
    source: String,
    body: String,
}

#[derive(Deserialize)]
struct RestEnvelope {
    result: RestBook,
}

#[derive(Deserialize)]
struct RestBook {
    change_id: u64,
    bids: Vec<(f64, f64)>,
    asks: Vec<(f64, f64)>,
}

#[derive(Deserialize)]
struct Note {
    params: NoteParams,
}

#[derive(Deserialize)]
struct NoteParams {
    channel: String,
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct BookMsg {
    #[serde(rename = "type")]
    kind: String,
    change_id: u64,
    bids: Vec<(String, f64, f64)>,
    asks: Vec<(String, f64, f64)>,
}

#[derive(Default)]
struct Levels(BTreeMap<i64, f64>);

impl Levels {
    fn apply(&mut self, entries: &[(String, f64, f64)], inst: &Instrument) {
        for (action, price, amount) in entries {
            let tick = feed::event::to_ticks(*price, inst);
            if action == "delete" || *amount <= 0.0 {
                self.0.remove(&tick);
            } else {
                self.0.insert(tick, *amount);
            }
        }
    }

    fn top(&self, n: usize, descending: bool) -> Vec<(i64, f64)> {
        let mut levels: Vec<(i64, f64)> = self.0.iter().map(|(p, q)| (*p, *q)).collect();
        if descending {
            levels.reverse();
        }
        levels.truncate(n);
        levels
    }
}

fn rest_levels(levels: &[(f64, f64)], inst: &Instrument, n: usize) -> Vec<(i64, f64)> {
    levels
        .iter()
        .take(n)
        .map(|(p, q)| (feed::event::to_ticks(*p, inst), *q))
        .collect()
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let inst = Instrument::btc_perpetual();

    let events = replay::read_dir(&args.dir, &inst)?;
    let stats = replay::verify(&events);
    println!(
        "quote-channel drift: {} quotes, {} disagreed, rate {:.4}, gaps {}",
        stats.quotes_checked,
        stats.quotes_mismatched,
        stats.mismatch_rate(),
        stats.gaps
    );

    let mut bids = Levels::default();
    let mut asks = Levels::default();
    let mut pending: Option<RestBook> = None;
    let (mut checked, mut exact, mut straddled, mut wrong) = (0u32, 0u32, 0u32, 0u32);

    for line in replay::read_lines(&args.dir)? {
        if let Ok(rest) = serde_json::from_str::<RestLine>(&line) {
            if rest.source == "rest-check"
                && let Ok(env) = serde_json::from_str::<RestEnvelope>(&rest.body)
            {
                pending = Some(env.result);
            }
            continue;
        }
        let Ok(note) = serde_json::from_str::<Note>(&line) else {
            continue;
        };
        if !note.params.channel.starts_with("book.") {
            continue;
        }
        let Ok(msg) = serde_json::from_value::<BookMsg>(note.params.data) else {
            continue;
        };
        if msg.kind == "snapshot" {
            bids = Levels::default();
            asks = Levels::default();
        }
        bids.apply(&msg.bids, &inst);
        asks.apply(&msg.asks, &inst);
        if let Some(rest) = &pending {
            if msg.change_id < rest.change_id {
                continue;
            }
            checked += 1;
            if msg.change_id > rest.change_id {
                // The snapshot was taken between two of our updates, so there
                // is no moment at which the two states are comparable.
                straddled += 1;
            } else {
                let ours = (bids.top(args.depth, true), asks.top(args.depth, false));
                let theirs = (
                    rest_levels(&rest.bids, &inst, args.depth),
                    rest_levels(&rest.asks, &inst, args.depth),
                );
                if ours == theirs {
                    exact += 1;
                } else {
                    wrong += 1;
                    let ob = ours.0.first().map(|(p, _)| *p);
                    let tb = theirs.0.first().map(|(p, _)| *p);
                    println!("  change_id {}: ours {ob:?} theirs {tb:?}", rest.change_id);
                }
            }
            pending = None;
        }
    }

    println!(
        "rest snapshots: {checked} reached, {exact} identical at the same change_id, \
         {wrong} different, {straddled} straddled two updates"
    );
    if wrong > 0 {
        println!("the rebuilt book is wrong; nothing downstream is worth running");
    }
    Ok(())
}
