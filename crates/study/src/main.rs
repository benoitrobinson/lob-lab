mod bootstrap;

use std::collections::BTreeSet;
use std::path::PathBuf;

use clap::Parser;
use feed::event::{Event, Instrument};
use feed::replay;
use sim::engine::{Config, Engine, Quoter};
use sim::markout::markouts_bps;
use sim::queue::QueueModel;
use strategy::intensity;
use strategy::quoters::{Glft, GlftQuoter, JoinTouch, SignalQuoter, Symmetric};

#[derive(Parser)]
struct Args {
    /// A directory of per-day directories, each holding that day's raw files.
    #[arg(long, default_value = "data/days")]
    data: PathBuf,
    #[arg(long, default_value = "artifacts")]
    out: PathBuf,
    #[arg(long, default_value_t = 10)]
    latency_ms: i64,
    #[arg(long, default_value_t = 100)]
    quote_size: i64,
    #[arg(long, default_value_t = 1_000)]
    max_position: i64,
    #[arg(long, default_value_t = 10_000)]
    resamples: usize,
    #[arg(long, default_value_t = 20_260_922)]
    seed: u64,
    /// Window over which order flow imbalance is accumulated.
    #[arg(long, default_value_t = 1_000)]
    ofi_window_ms: i64,
    /// Fractions of cancels assumed to come from ahead of us, in percent.
    /// Level-2 data cannot say, so the study reports the whole curve.
    #[arg(long, value_delimiter = ',', default_value = "0,25,50,75,100")]
    cancel_sweep: Vec<u8>,
    /// How many standard deviations of the day's own imbalance the signal
    /// quoter waits for before standing aside. Setting the threshold from the
    /// day rather than from a number chosen in advance keeps it comparable
    /// across days that traded at different sizes.
    #[arg(long, default_value_t = 1.0)]
    ofi_sigmas: f64,
}

const HORIZONS_MS: [i64; 3] = [100, 1_000, 10_000];

struct Row {
    day: String,
    quoter: &'static str,
    model: &'static str,
    pnl_btc: f64,
    fills: usize,
    fees_btc: f64,
    markout_1s: f64,
    halted_ms: i64,
    /// How often the quoter declined to quote a side.
    pulled: f64,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.out)?;
    let inst = Instrument::btc_perpetual();

    if !args.data.is_dir() {
        eprintln!(
            "{} does not exist. Record data first: see the README, then organise the \
             recorded hours into data/days/<YYYY-MM-DD>/.",
            args.data.display()
        );
        return Ok(());
    }
    let mut days: Vec<PathBuf> = std::fs::read_dir(&args.data)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    days.sort();
    if days.is_empty() {
        eprintln!(
            "no day directories under {}: record data first, see the README",
            args.data.display()
        );
    }

    let mut rows = Vec::new();
    for day in &days {
        let name = day
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let events = replay::read_dir(day, &inst)?;
        // The preregistered exclusion is feed gaps, not quote drift. Drift
        // against the quote channel is expected: that feed publishes on change
        // and the book feed is aggregated to 100ms, so the two describe
        // different moments. The check that the book is right is
        // `verify_book`, which compares against a REST snapshot at the same
        // change_id, and it is a gate on the whole dataset rather than a
        // per-day filter.
        let gaps = events
            .iter()
            .filter(|e| matches!(e, feed::event::Event::Gap { .. }))
            .count();
        if gaps > 0 {
            eprintln!("{name}: {gaps} feed gaps, see docs/preregistration.md");
        }
        let (distances, elapsed) = intensity::trade_distances(&events, inst.tick_size);
        let fitted = intensity::fit(&distances, elapsed, inst.tick_size, 8);
        if fitted.is_degenerate() {
            eprintln!("{name}: intensity fit failed, excluded");
            continue;
        }
        eprintln!("{name}: A {:.2} kappa {:.4}", fitted.a, fitted.kappa);
        let sigma = realised_sigma(&events, inst.tick_size);
        let ofi_sd = ofi_spread(&events, args.ofi_window_ms);
        let threshold = args.ofi_sigmas * ofi_sd;
        eprintln!("{name}: imbalance sd {ofi_sd:.1}, threshold {threshold:.1}");

        let mut models: Vec<(String, QueueModel)> = vec![
            ("naive".to_string(), QueueModel::Naive),
            ("pessimistic".to_string(), QueueModel::Pessimistic),
            ("proportional".to_string(), QueueModel::Proportional),
        ];
        for pct in &args.cancel_sweep {
            models.push((format!("ahead{pct}"), QueueModel::FromAhead(*pct)));
        }
        for (model_name, model) in models {
            let model_name: &'static str = Box::leak(model_name.into_boxed_str());
            let cfg = Config {
                latency_ms: args.latency_ms,
                model,
                maker_fee: inst.maker_fee,
                tick_size: inst.tick_size,
                requote_ms: 100,
                max_position: args.max_position,
            };
            let mut touch = JoinTouch {
                offset_ticks: 0,
                size: args.quote_size,
            };
            let mut symmetric = Symmetric {
                half_spread_ticks: 2,
                size: args.quote_size,
            };
            let mut signal = SignalQuoter::new(args.ofi_window_ms, threshold, args.quote_size);
            let mut glft = GlftQuoter {
                model: Glft {
                    gamma: 0.1,
                    sigma,
                    kappa: fitted.kappa,
                    a: fitted.a,
                    tick_size: inst.tick_size,
                },
                size: args.quote_size,
            };
            for (quoter_name, quoter) in [
                ("touch", &mut touch as &mut dyn Quoter),
                ("signal", &mut signal as &mut dyn Quoter),
                ("symmetric", &mut symmetric as &mut dyn Quoter),
                ("glft", &mut glft as &mut dyn Quoter),
            ] {
                let report = Engine::new(cfg).run(&events, quoter);
                let last_ts = events.last().map(|e| e.ts()).unwrap_or(0);
                let pnl = report
                    .mids
                    .at(last_ts)
                    .map(|m| report.ledger.equity_btc(m))
                    .unwrap_or(f64::NAN);
                let marks = markouts_bps(&report.ledger.fills, &report.mids, &HORIZONS_MS);
                let pulled = quoter
                    .report()
                    .iter()
                    .filter(|(k, _)| k.starts_with("pulled"))
                    .map(|(_, v)| v)
                    .sum::<f64>();
                if quoter_name == "signal" && model_name == "naive" {
                    let r = quoter.report();
                    let get = |k: &str| {
                        r.iter()
                            .find(|(n, _)| *n == k)
                            .map(|(_, v)| *v)
                            .unwrap_or(0.0)
                    };
                    eprintln!(
                        "  signal/{model_name}: {} observations, largest imbalance {:.0}, threshold {:.0}, pulled {:.0}",
                        get("observations"),
                        get("max_abs_imbalance"),
                        threshold,
                        get("pulled_bid") + get("pulled_ask")
                    );
                }
                rows.push(Row {
                    day: name.clone(),
                    quoter: quoter_name,
                    model: model_name,
                    pnl_btc: pnl,
                    fills: report.ledger.fills.len(),
                    fees_btc: report.ledger.fees_btc,
                    markout_1s: marks[1],
                    halted_ms: report.halted_ms,
                    pulled,
                });
            }
        }
    }

    let mut csv =
        String::from("day,quoter,model,pnl_btc,fills,fees_btc,markout_1s_bps,halted_ms,pulled\n");
    for r in &rows {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            r.day,
            r.quoter,
            r.model,
            r.pnl_btc,
            r.fills,
            r.fees_btc,
            r.markout_1s,
            r.halted_ms,
            r.pulled
        ));
    }
    std::fs::write(args.out.join("runs.csv"), csv)?;

    // Primary statistic, fixed in docs/preregistration.md: the relative
    // overstatement of the naive fill model, one number per day.
    let mut overstatement = Vec::new();
    // Fills are the direction-independent measure. The P&L difference changes
    // sign with the strategy: on a losing quoter the naive model exaggerates
    // the loss rather than flattering the edge, because it hands out fills
    // that were never there. The ratio of fill counts says how many of them.
    let mut fill_inflation = Vec::new();
    let mut naive_pnl = Vec::new();
    for day in rows
        .iter()
        .map(|r| r.day.clone())
        .collect::<BTreeSet<String>>()
    {
        let pick = |quoter: &str, model: &str| {
            rows.iter()
                .find(|r| r.day == day && r.quoter == quoter && r.model == model)
                .map(|r| r.pnl_btc)
        };
        let fills = |quoter: &str, model: &str| {
            rows.iter()
                .find(|r| r.day == day && r.quoter == quoter && r.model == model)
                .map(|r| r.fills)
        };
        if let (Some(naive), Some(queue)) = (pick("touch", "naive"), pick("touch", "pessimistic"))
            && naive.abs() > 0.0
        {
            overstatement.push((naive - queue) / naive.abs());
            naive_pnl.push(naive);
        }
        if let (Some(n), Some(q)) = (fills("touch", "naive"), fills("touch", "pessimistic"))
            && q > 0
        {
            fill_inflation.push(n as f64 / q as f64);
        }
    }
    // The same statistic across the sweep, so the reader sees what the answer
    // depends on instead of one number resting on an assumption nobody can
    // check.
    let mut sensitivity = serde_json::Map::new();
    for pct in &args.cancel_sweep {
        let model = format!("ahead{pct}");
        let mut ratios = Vec::new();
        let mut pnl_gap = Vec::new();
        for day in rows
            .iter()
            .map(|r| r.day.clone())
            .collect::<BTreeSet<String>>()
        {
            let at = |m: &str| {
                rows.iter()
                    .find(|r| r.day == day && r.quoter == "touch" && r.model == m)
            };
            if let (Some(naive), Some(queue)) = (at("naive"), at(model.as_str())) {
                if queue.fills > 0 {
                    ratios.push(naive.fills as f64 / queue.fills as f64);
                }
                if naive.pnl_btc.abs() > 0.0 {
                    pnl_gap.push((naive.pnl_btc - queue.pnl_btc) / naive.pnl_btc.abs());
                }
            }
        }
        let mean = |v: &[f64]| {
            if v.is_empty() {
                f64::NAN
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            }
        };
        sensitivity.insert(
            model,
            serde_json::json!({
                "cancels_from_ahead_pct": pct,
                "fill_inflation": mean(&ratios),
                "pnl_gap": mean(&pnl_gap),
            }),
        );
    }

    let (lo, hi) = bootstrap::paired_bootstrap(&overstatement, args.resamples, args.seed);
    let (flo, fhi) = bootstrap::paired_bootstrap(&fill_inflation, args.resamples, args.seed);
    let mean_of = |v: &[f64]| {
        if v.is_empty() {
            f64::NAN
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    let headline = serde_json::json!({
        "days": overstatement.len(),
        "overstatement_mean": mean_of(&overstatement),
        "overstatement_ci95": [lo, hi],
        "excludes_zero": lo > 0.0 || hi < 0.0,
        "fill_inflation_mean": mean_of(&fill_inflation),
        "fill_inflation_ci95": [flo, fhi],
        // The sign of the P&L difference only means "the naive model flatters
        // the strategy" when the strategy makes money in the first place.
        "naive_pnl_btc_mean": mean_of(&naive_pnl),
        "naive_pnl_positive": mean_of(&naive_pnl) > 0.0,
        "latency_ms": args.latency_ms,
        "seed": args.seed,
        "cancel_position_sensitivity": sensitivity,
    });
    std::fs::write(
        args.out.join("headline.json"),
        serde_json::to_string_pretty(&headline)?,
    )?;
    println!("{headline:#}");
    Ok(())
}

/// The standard deviation of the day's own order flow imbalance, which is what
/// the signal quoter's threshold is stated in. Computed in one pass before the
/// grid runs, from the same feed, so no future information reaches the
/// threshold beyond the scale of the day.
fn ofi_spread(events: &[Event], window_ms: i64) -> f64 {
    use lob::{Book, Side};
    use signal::ofi::{Ofi, Touch};
    let mut book = Book::new();
    let mut ofi = Ofi::new(window_ms);
    let mut values = Vec::new();
    for e in events {
        match e {
            Event::Gap { .. } => {
                book.clear();
                ofi.reset();
                continue;
            }
            Event::Snapshot { bids, asks, .. } | Event::Change { bids, asks, .. } => {
                for (p, q) in bids {
                    book.set(Side::Bid, *p, *q);
                }
                for (p, q) in asks {
                    book.set(Side::Ask, *p, *q);
                }
            }
            _ => continue,
        }
        if let (Some((bid, bid_qty)), Some((ask, ask_qty))) = (book.best_bid(), book.best_ask()) {
            values.push(ofi.update(
                e.ts(),
                Touch {
                    bid,
                    bid_qty,
                    ask,
                    ask_qty,
                },
            ));
        }
    }
    if values.len() < 3 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64;
    var.sqrt()
}

/// Realised volatility of the mid in USD per square root second, from
/// one-second returns. GLFT needs a sigma, and taking it from the day being
/// traded is the honest version of the assumption `vol-lab` made by choosing
/// one.
fn realised_sigma(events: &[Event], tick_size: f64) -> f64 {
    use lob::{Book, Side};
    let mut book = Book::new();
    let mut last_second = i64::MIN;
    let mut mids = Vec::new();
    for e in events {
        match e {
            Event::Snapshot { bids, asks, .. } | Event::Change { bids, asks, .. } => {
                for (p, q) in bids {
                    book.set(Side::Bid, *p, *q);
                }
                for (p, q) in asks {
                    book.set(Side::Ask, *p, *q);
                }
            }
            _ => {}
        }
        let second = e.ts() / 1_000;
        if second != last_second
            && let Some(mid2) = book.mid2()
        {
            mids.push(mid2 as f64 * tick_size / 2.0);
            last_second = second;
        }
    }
    if mids.len() < 3 {
        return 1.0;
    }
    let diffs: Vec<f64> = mids.windows(2).map(|w| w[1] - w[0]).collect();
    let mean = diffs.iter().sum::<f64>() / diffs.len() as f64;
    let var = diffs.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (diffs.len() - 1) as f64;
    var.sqrt()
}
