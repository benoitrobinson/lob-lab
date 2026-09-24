//! Does order flow imbalance predict the next move on this data?
//!
//! Regresses the mid's change over a horizon on the imbalance observed up to
//! that moment. Samples are taken on a fixed grid rather than on every update,
//! because consecutive updates share most of their window and overlapping
//! samples make any relationship look more certain than it is.

use std::path::PathBuf;

use clap::Parser;
use feed::event::{Event, Instrument};
use feed::replay;
use lob::{Book, Side};
use signal::ofi::{Ofi, Touch, regress};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "data")]
    dir: PathBuf,
    /// Window over which imbalance is accumulated.
    #[arg(long, default_value_t = 1_000)]
    window_ms: i64,
    /// Spacing between samples. Below the window, samples overlap.
    #[arg(long, default_value_t = 1_000)]
    sample_ms: i64,
    /// Horizons to predict, in milliseconds.
    #[arg(long, value_delimiter = ',', default_value = "500,1000,5000,30000")]
    horizons_ms: Vec<i64>,
    /// Print JSON instead of the table, for vol-lab's panel.
    #[arg(long)]
    json: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let inst = Instrument::btc_perpetual();
    let events = replay::read_dir(&args.dir, &inst)?;

    let mut book = Book::new();
    let mut ofi = Ofi::new(args.window_ms);
    let mut mids: Vec<(i64, f64)> = Vec::new();
    let mut samples: Vec<(i64, f64)> = Vec::new();
    let mut next_sample = i64::MIN;

    for event in &events {
        match event {
            Event::Gap { .. } => {
                book.clear();
                ofi.reset();
                continue;
            }
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
            _ => continue,
        }
        let (Some((bid, bid_qty)), Some((ask, ask_qty))) = (book.best_bid(), book.best_ask())
        else {
            continue;
        };
        let ts = event.ts();
        let value = ofi.update(
            ts,
            Touch {
                bid,
                bid_qty,
                ask,
                ask_qty,
            },
        );
        let mid = (bid + ask) as f64 * inst.tick_size / 2.0;
        mids.push((ts, mid));
        if ts >= next_sample {
            samples.push((ts, value));
            next_sample = ts + args.sample_ms;
        }
    }

    let mid_at = |ts: i64| -> Option<f64> {
        match mids.binary_search_by_key(&ts, |(t, _)| *t) {
            Ok(i) => Some(mids[i].1),
            Err(0) => None,
            Err(i) => Some(mids[i - 1].1),
        }
    };
    let last_ts = mids.last().map(|(t, _)| *t).unwrap_or(0);

    if !args.json {
        println!(
            "{} book updates, {} samples every {} ms, imbalance over {} ms",
            mids.len(),
            samples.len(),
            args.sample_ms,
            args.window_ms
        );
        println!(
            "{:>9} {:>10} {:>9} {:>9} {:>7} {:>8}",
            "horizon", "slope", "r2", "sign", "moved", "n"
        );
    }
    let mut rows = Vec::new();
    for h in &args.horizons_ms {
        let mut x = Vec::new();
        let mut y = Vec::new();
        let mut agree = 0u32;
        let mut moved = 0u32;
        for (ts, value) in &samples {
            if ts + h > last_ts {
                continue;
            }
            let (Some(now), Some(later)) = (mid_at(*ts), mid_at(ts + h)) else {
                continue;
            };
            let move_usd = later - now;
            // Most samples of a 100ms feed have no move at all over half a
            // second, and counting those as disagreements would bury the
            // question. The rate below is over the samples that moved.
            if *value != 0.0 && move_usd != 0.0 {
                moved += 1;
                if (*value > 0.0) == (move_usd > 0.0) {
                    agree += 1;
                }
            }
            x.push(*value);
            y.push(move_usd);
        }
        let (slope, r2, n) = regress(&x, &y);
        let sign_pct = if moved > 0 {
            100.0 * agree as f64 / moved as f64
        } else {
            f64::NAN
        };
        if args.json {
            rows.push(serde_json::json!({
                "horizon_ms": h, "slope": slope, "r2": r2,
                "sign_pct": sign_pct, "moved": moved, "n": n,
            }));
        } else {
            println!("{h:>7} ms {slope:>10.3e} {r2:>9.4} {sign_pct:>7.1}% {moved:>7} {n:>8}");
        }
    }
    if args.json {
        let out = serde_json::json!({
            "book_updates": mids.len(),
            "samples": samples.len(),
            "sample_ms": args.sample_ms,
            "window_ms": args.window_ms,
            "horizons": rows,
        });
        println!("{out}");
        return Ok(());
    }
    println!(
        "\nslope is USD of mid move per unit of imbalance; sign is how often the \
         imbalance pointed the right way"
    );
    Ok(())
}
