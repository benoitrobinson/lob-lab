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
use strategy::quoters::{Glft, GlftQuoter, Symmetric};

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
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.out)?;
    let inst = Instrument::btc_perpetual();

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
        let stats = replay::verify(&events);
        if stats.mismatch_rate() > 0.001 {
            eprintln!(
                "{name}: book mismatch rate {:.4}, excluded",
                stats.mismatch_rate()
            );
            continue;
        }
        let (distances, elapsed) = intensity::trade_distances(&events, inst.tick_size);
        let fitted = intensity::fit(&distances, elapsed, inst.tick_size, 8);
        if fitted.is_degenerate() {
            eprintln!("{name}: intensity fit failed, excluded");
            continue;
        }
        eprintln!("{name}: A {:.2} kappa {:.4}", fitted.a, fitted.kappa);
        let sigma = realised_sigma(&events, inst.tick_size);

        for (model_name, model) in [
            ("naive", QueueModel::Naive),
            ("pessimistic", QueueModel::Pessimistic),
            ("proportional", QueueModel::Proportional),
        ] {
            let cfg = Config {
                latency_ms: args.latency_ms,
                model,
                maker_fee: inst.maker_fee,
                tick_size: inst.tick_size,
                requote_ms: 100,
                max_position: args.max_position,
            };
            let mut symmetric = Symmetric {
                half_spread_ticks: 2,
                size: args.quote_size,
            };
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
                rows.push(Row {
                    day: name.clone(),
                    quoter: quoter_name,
                    model: model_name,
                    pnl_btc: pnl,
                    fills: report.ledger.fills.len(),
                    fees_btc: report.ledger.fees_btc,
                    markout_1s: marks[1],
                    halted_ms: report.halted_ms,
                });
            }
        }
    }

    let mut csv =
        String::from("day,quoter,model,pnl_btc,fills,fees_btc,markout_1s_bps,halted_ms\n");
    for r in &rows {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{}\n",
            r.day, r.quoter, r.model, r.pnl_btc, r.fills, r.fees_btc, r.markout_1s, r.halted_ms
        ));
    }
    std::fs::write(args.out.join("runs.csv"), csv)?;

    // Primary statistic, fixed in docs/preregistration.md: the relative
    // overstatement of the naive fill model, one number per day.
    let mut overstatement = Vec::new();
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
        if let (Some(naive), Some(queue)) = (pick("glft", "naive"), pick("glft", "pessimistic"))
            && naive.abs() > 0.0
        {
            overstatement.push((naive - queue) / naive.abs());
        }
    }
    let (lo, hi) = bootstrap::paired_bootstrap(&overstatement, args.resamples, args.seed);
    let mean = if overstatement.is_empty() {
        f64::NAN
    } else {
        overstatement.iter().sum::<f64>() / overstatement.len() as f64
    };
    let headline = serde_json::json!({
        "days": overstatement.len(),
        "overstatement_mean": mean,
        "overstatement_ci95": [lo, hi],
        "excludes_zero": lo > 0.0 || hi < 0.0,
        "latency_ms": args.latency_ms,
        "seed": args.seed,
    });
    std::fs::write(
        args.out.join("headline.json"),
        serde_json::to_string_pretty(&headline)?,
    )?;
    println!("{headline:#}");
    Ok(())
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
