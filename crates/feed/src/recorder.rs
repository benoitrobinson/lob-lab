use std::io::Write;
use std::path::PathBuf;

/// Files rotate on the UTC hour. Naming them by the hour number keeps the
/// recorder free of a calendar dependency; replay reads timestamps from the
/// messages themselves.
pub fn hour_of(timestamp_ms: i64) -> i64 {
    timestamp_ms.div_euclid(3_600_000)
}

/// Flush this often. A killed recorder loses everything written since the last
/// flush, so this is the size of the hole a crash leaves: a few seconds.
const FLUSH_EVERY: u64 = 200;

pub struct RawWriter {
    dir: PathBuf,
    current: Option<(i64, zstd::stream::write::Encoder<'static, std::fs::File>)>,
    since_flush: u64,
}

impl RawWriter {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            current: None,
            since_flush: 0,
        }
    }

    pub fn write(&mut self, timestamp_ms: i64, line: &str) -> anyhow::Result<()> {
        let hour = hour_of(timestamp_ms);
        let rotate = match &self.current {
            Some((h, _)) => *h != hour,
            None => true,
        };
        if rotate {
            self.finish()?;
            let path = self.dir.join(format!("raw-{hour}.jsonl.zst"));
            // Append, never create. A reconnect inside the same hour used to
            // truncate the hour's file and silently throw the recording away;
            // zstd reads concatenated frames, so appending a new frame after a
            // reconnect costs nothing and loses nothing.
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            self.current = Some((hour, zstd::stream::write::Encoder::new(file, 3)?));
        }
        let (_, writer) = self.current.as_mut().expect("writer exists after rotate");
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        self.since_flush += 1;
        if self.since_flush >= FLUSH_EVERY {
            writer.flush()?;
            self.since_flush = 0;
        }
        Ok(())
    }

    pub fn finish(&mut self) -> anyhow::Result<()> {
        if let Some((_, writer)) = self.current.take() {
            writer.finish()?.sync_all()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_once_per_utc_hour() {
        assert_eq!(hour_of(0), 0);
        assert_eq!(hour_of(3_599_999), 0);
        assert_eq!(hour_of(3_600_000), 1);
        assert_eq!(hour_of(1_590_484_156_350), 441_801);
    }

    #[test]
    fn writes_one_line_per_message() {
        let dir = tempdir();
        let mut w = RawWriter::new(dir.clone());
        w.write(0, "{\"a\":1}").unwrap();
        w.write(10, "{\"a\":2}").unwrap();
        w.finish().unwrap();
        let decoded = read_zst(&dir.join("raw-0.jsonl.zst"));
        assert_eq!(decoded.lines().count(), 2);
    }

    fn tempdir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("lob-lab-test-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn read_zst(path: &std::path::Path) -> String {
        let f = std::fs::File::open(path).unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(&mut zstd::stream::read::Decoder::new(f).unwrap(), &mut s)
            .unwrap();
        s
    }
}
