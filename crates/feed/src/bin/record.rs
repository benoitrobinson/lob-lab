use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use feed::gaps::{GapCheck, GapDetector};
use feed::messages::{BookMsg, Notification};
use feed::recorder::RawWriter;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

const WS_URL: &str = "wss://www.deribit.com/ws/api/v2";
const REST_BOOK: &str = "https://www.deribit.com/api/v2/public/get_order_book";

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "data")]
    out: PathBuf,
    #[arg(long, default_value = "BTC-PERPETUAL")]
    instrument: String,
    /// The public interval. `raw` needs an authenticated connection.
    #[arg(long, default_value = "100ms")]
    interval: String,
    /// How often to record a REST snapshot of the book. Each one carries the
    /// change_id it was taken at, which is what makes the rebuilt book
    /// checkable against the exchange at the same point in the sequence
    /// rather than at the same wall clock.
    #[arg(long, default_value_t = 30)]
    rest_check_secs: u64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as i64
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.out)?;
    let mut backoff = Duration::from_secs(1);
    loop {
        let started = SystemTime::now();
        match session(&args).await {
            Ok(Outcome::Interrupted) => {
                eprintln!("stopped by the operator");
                return Ok(());
            }
            Ok(Outcome::Closed) => eprintln!("session ended cleanly"),
            Err(e) => eprintln!("session error: {e}"),
        }
        if started.elapsed().unwrap_or_default() > Duration::from_secs(60) {
            backoff = Duration::from_secs(1);
        }
        eprintln!("reconnecting in {backoff:?}");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// Why a session ended: a closed socket is reconnected, an interrupt is not.
enum Outcome {
    Closed,
    Interrupted,
}

async fn session(args: &Args) -> anyhow::Result<Outcome> {
    let (mut ws, _) = tokio_tungstenite::connect_async(WS_URL).await?;
    let channels = vec![
        format!("book.{}.{}", args.instrument, args.interval),
        format!("trades.{}.{}", args.instrument, args.interval),
        format!("quote.{}", args.instrument),
    ];
    ws.send(Message::Text(
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "public/set_heartbeat",
            "params": {"interval": 10}
        })
        .to_string()
        .into(),
    ))
    .await?;
    ws.send(Message::Text(
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "public/subscribe",
            "params": {"channels": channels}
        })
        .to_string()
        .into(),
    ))
    .await?;

    let mut writer = RawWriter::new(args.out.clone());
    let mut gaps = GapDetector::default();
    let mut next_rest_check = std::time::Instant::now();
    let mut gap_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(args.out.join("gaps.jsonl"))?;

    loop {
        let msg = tokio::select! {
            m = ws.next() => match m {
                Some(m) => m,
                None => break,
            },
            // A recorder that is killed mid-frame leaves a file its own reader
            // cannot open. Catching the signal and finishing the frame is the
            // difference between losing a few seconds and losing the hour.
            _ = tokio::signal::ctrl_c() => {
                eprintln!("interrupted, closing the current file");
                writer.finish()?;
                return Ok(Outcome::Interrupted);
            }
        };
        let text = match msg? {
            Message::Text(t) => t.to_string(),
            Message::Ping(p) => {
                ws.send(Message::Pong(p)).await?;
                continue;
            }
            Message::Close(_) => break,
            _ => continue,
        };
        writer.write(now_ms(), &text)?;

        if args.rest_check_secs > 0 && std::time::Instant::now() >= next_rest_check {
            next_rest_check = std::time::Instant::now() + Duration::from_secs(args.rest_check_secs);
            let url = format!("{REST_BOOK}?instrument_name={}&depth=10", args.instrument);
            match reqwest::get(&url).await {
                Ok(r) => match r.text().await {
                    Ok(body) => {
                        let line = serde_json::json!({"source": "rest-check", "body": body});
                        writer.write(now_ms(), &line.to_string())?;
                    }
                    Err(e) => eprintln!("rest check body: {e}"),
                },
                Err(e) => eprintln!("rest check: {e}"),
            }
        }

        let Ok(note) = serde_json::from_str::<Notification>(&text) else {
            // Heartbeats and RPC replies are not notifications; they are still
            // written to the raw file above.
            if text.contains("test_request") {
                ws.send(Message::Text(
                    serde_json::json!({"jsonrpc":"2.0","id":3,"method":"public/test","params":{}})
                        .to_string()
                        .into(),
                ))
                .await?;
            }
            continue;
        };
        if !note.params.channel.starts_with("book.") {
            continue;
        }
        let book: BookMsg = serde_json::from_value(note.params.data)?;
        if let GapCheck::Missing { from, to } = gaps.check(&book) {
            let record = serde_json::json!({
                "timestamp": book.timestamp, "missing_from": from, "missing_to": to
            });
            use std::io::Write as _;
            writeln!(gap_log, "{record}")?;
            eprintln!("gap {from}..{to}, resyncing over REST");
            let url = format!("{REST_BOOK}?instrument_name={}&depth=1000", args.instrument);
            let body = reqwest::get(&url).await?.text().await?;
            let line = serde_json::json!({"source": "rest-resync", "body": body});
            writer.write(now_ms(), &line.to_string())?;
        }
    }
    writer.finish()?;
    Ok(Outcome::Closed)
}
