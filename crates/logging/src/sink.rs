use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

/// Default rotation threshold — Zed's 1 MiB `.log` / `.log.old` pair.
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;

/// A size-rotating append writer over a `.log` / `.log.old` pair.
///
/// Opens the active file with `append(true)` and never truncates it on open, so
/// the previous run's tail survives a restart. When a write would push the active
/// file past `max_bytes`, the active file is copied to the `.old` sibling
/// (replacing any prior `.old`) and then truncated in place — the active path
/// stays stable, so an external reader (`tail -f`) keeps its handle.
pub struct SizedWriter {
    path: PathBuf,
    old_path: PathBuf,
    file: File,
    written: u64,
    max_bytes: u64,
}

impl SizedWriter {
    /// Open the rotating writer at `path` with the given rotation threshold,
    /// creating parent directories and the file as needed. The active file is
    /// opened in append mode; its current length seeds the rotation counter so a
    /// reopened file rotates relative to what is already on disk.
    pub fn new(path: impl Into<PathBuf>, max_bytes: u64) -> io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let old_path = rotated_path(&path);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            path,
            old_path,
            file,
            written,
            max_bytes,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        std::fs::copy(&self.path, &self.old_path)?;
        self.file.set_len(0)?;
        self.written = 0;
        Ok(())
    }
}

impl Write for SizedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written > 0 && self.written.saturating_add(buf.len() as u64) > self.max_bytes {
            self.rotate()?;
        }
        let written = self.file.write(buf)?;
        self.written = self.written.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Sibling `.old` path for `path` (`foo.log` -> `foo.log.old`), built on the raw
/// `OsString` so non-UTF-8 paths round-trip.
fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// A [`MakeWriter`] over a shared [`SizedWriter`], so the `tracing` fmt layer can
/// drive the rotating sink. The mutex serializes concurrent log events into the
/// single active file.
#[derive(Clone)]
pub struct RotatingMakeWriter {
    inner: Arc<Mutex<SizedWriter>>,
}

impl RotatingMakeWriter {
    pub fn new(writer: SizedWriter) -> Self {
        Self {
            inner: Arc::new(Mutex::new(writer)),
        }
    }
}

/// Write guard returned by [`RotatingMakeWriter::make_writer`]; holds the lock for
/// the duration of one event's write.
pub struct LockedWriter<'a> {
    guard: std::sync::MutexGuard<'a, SizedWriter>,
}

impl Write for LockedWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.guard.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.guard.flush()
    }
}

impl<'a> MakeWriter<'a> for RotatingMakeWriter {
    type Writer = LockedWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        LockedWriter { guard }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique scratch directory under the OS temp dir — no `tempfile` dependency
    /// (the crate is std-plus-tracing only).
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut dir = std::env::temp_dir();
            dir.push(format!("rift-logging-{}-{tag}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("read log file")
    }

    #[test]
    fn test_rotated_path_appends_old_suffix() {
        assert_eq!(
            rotated_path(Path::new("/tmp/rift-stable.log")),
            PathBuf::from("/tmp/rift-stable.log.old")
        );
    }

    #[test]
    fn test_appends_across_reopens_without_truncating() {
        let scratch = Scratch::new("reopen");
        let log = scratch.path("app.log");

        {
            let mut writer = SizedWriter::new(&log, DEFAULT_MAX_BYTES).expect("open");
            writer.write_all(b"first\n").expect("write");
        }
        {
            let mut writer = SizedWriter::new(&log, DEFAULT_MAX_BYTES).expect("reopen");
            writer.write_all(b"second\n").expect("write");
        }

        assert_eq!(read(&log), "first\nsecond\n");
    }

    #[test]
    fn test_rotates_at_threshold_into_old() {
        let scratch = Scratch::new("rotate");
        let log = scratch.path("app.log");
        let old = rotated_path(&log);

        let mut writer = SizedWriter::new(&log, 4).expect("open");
        writer.write_all(b"aaaa").expect("write fills threshold");
        // No `.old` yet — the active file is exactly at the threshold.
        assert!(!old.exists());

        writer.write_all(b"b").expect("write triggers rotation");
        assert_eq!(read(&old), "aaaa");
        assert_eq!(read(&log), "b");
    }

    #[test]
    fn test_rotation_replaces_previous_old() {
        let scratch = Scratch::new("replace");
        let log = scratch.path("app.log");
        let old = rotated_path(&log);

        let mut writer = SizedWriter::new(&log, 2).expect("open");
        writer.write_all(b"11").expect("write");
        writer.write_all(b"22").expect("rotate, old=11");
        assert_eq!(read(&old), "11");
        writer.write_all(b"33").expect("rotate, old=22");

        // The previous `.old` ("11") was replaced by the most recent active body.
        assert_eq!(read(&old), "22");
        assert_eq!(read(&log), "33");
    }

    #[test]
    fn test_first_write_larger_than_threshold_does_not_rotate_empty() {
        let scratch = Scratch::new("bigfirst");
        let log = scratch.path("app.log");
        let old = rotated_path(&log);

        let mut writer = SizedWriter::new(&log, 2).expect("open");
        writer.write_all(b"oversized").expect("write");

        // An empty active file must never be rotated into `.old`.
        assert!(!old.exists());
        assert_eq!(read(&log), "oversized");
    }

    #[test]
    fn test_survives_simulated_interruption_with_pair_intact() {
        let scratch = Scratch::new("interrupt");
        let log = scratch.path("app.log");
        let old = rotated_path(&log);

        {
            // Write, rotate, then drop the writer mid-run (the "crash").
            let mut writer = SizedWriter::new(&log, 3).expect("open");
            writer.write_all(b"old").expect("write");
            writer.write_all(b"new").expect("rotate");
        }

        // Both halves of the pair survive and are readable.
        assert_eq!(read(&old), "old");
        assert_eq!(read(&log), "new");

        // Reopening keeps appending to the active file — no truncation on open.
        {
            let mut writer = SizedWriter::new(&log, 1024).expect("reopen");
            writer.write_all(b"-more").expect("write");
        }
        assert_eq!(read(&log), "new-more");
    }

    #[test]
    fn test_make_writer_routes_through_shared_writer() {
        let scratch = Scratch::new("makewriter");
        let log = scratch.path("app.log");

        let make =
            RotatingMakeWriter::new(SizedWriter::new(&log, DEFAULT_MAX_BYTES).expect("open"));
        make.make_writer().write_all(b"event-1\n").expect("write");
        make.make_writer().write_all(b"event-2\n").expect("write");

        assert_eq!(read(&log), "event-1\nevent-2\n");
    }
}
