use std::path::PathBuf;

use clap::Parser;
use feed::event::Instrument;
use feed::replay;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "data")]
    dir: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let inst = Instrument::btc_perpetual();
    let events = replay::read_dir(&args.dir, &inst)?;
    let stats = replay::verify(&events);
    println!(
        "events {} quotes {} mismatched {} rate {:.6} gaps {}",
        events.len(),
        stats.quotes_checked,
        stats.quotes_mismatched,
        stats.mismatch_rate(),
        stats.gaps
    );
    Ok(())
}
