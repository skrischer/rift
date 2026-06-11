//! A [`Watcher`] that turns filesystem events under a worktree root into coalesced
//! [`Change`] batches against a held [`Snapshot`].
//!
//! Events are used only as a *trigger*: on each quiet point the watcher rescans the
//! tree and diffs the fresh [`Snapshot`] against the held one, so a move falls out as
//! remove + add and an ignored path never appears (it is absent from both scans).
//! Only non-ignored directories are watched, so a large ignored tree like `target/`
//! never consumes an OS watch; if the watch limit is still hit the watcher logs once
//! and degrades rather than panicking.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};

use crate::snapshot::{Change, EntryKind};
use crate::{ExplorerError, Result, Snapshot};

/// Quiet period after the last event before a burst is flushed into a diff.
const DEBOUNCE: Duration = Duration::from_millis(100);
/// Upper bound on how long a sustained event storm may delay a flush.
const MAX_COALESCE: Duration = Duration::from_secs(1);
/// Idle wake interval so the worker notices shutdown between bursts.
const IDLE_POLL: Duration = Duration::from_millis(500);

/// Watches a worktree root and emits coalesced [`Change`] batches as files are
/// created, modified, deleted, or moved. Dropping the watcher stops the background
/// worker and releases the underlying OS watches.
pub struct Watcher {
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Start watching the root of `initial`, using it as the baseline to diff
    /// against. Returns the watcher and a receiver of coalesced change batches; every
    /// batch is non-empty, and applying batches in order keeps a consumer's mirror in
    /// sync with the worktree.
    ///
    /// Establishing the OS watcher can fail (e.g. the inotify instance limit) — that
    /// is the only error returned. A *per-directory* watch failure later (the watch
    /// limit) is logged once and degraded, never fatal.
    pub fn new(initial: Snapshot) -> Result<(Self, Receiver<Vec<Change>>)> {
        let (event_tx, event_rx) = mpsc::channel();
        let watcher = RecommendedWatcher::new(
            move |result: notify::Result<Event>| {
                // A send error only means the worker has gone away; nothing to do.
                let _ = event_tx.send(result);
            },
            Config::default(),
        )
        .map_err(|err| ExplorerError::WatchError(err.to_string()))?;

        // Register the initial watch set synchronously, so a change made right after
        // this returns is already observed.
        let mut watches = WatchSet::new(watcher);
        watches.reconcile(&initial);

        let (change_tx, change_rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = std::thread::Builder::new()
            .name("rift-explorer-watch".to_owned())
            .spawn(move || run(watches, initial, event_rx, change_tx, worker_shutdown))
            .map_err(|err| ExplorerError::WatchError(err.to_string()))?;

        Ok((
            Self {
                shutdown,
                worker: Some(worker),
            },
            change_rx,
        ))
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The directories currently registered with the OS watcher, plus the owned watcher
/// handle. Kept in lock-step with the non-ignored directories of the latest snapshot.
struct WatchSet {
    watcher: RecommendedWatcher,
    dirs: BTreeSet<PathBuf>,
    warned: bool,
}

impl WatchSet {
    fn new(watcher: RecommendedWatcher) -> Self {
        Self {
            watcher,
            dirs: BTreeSet::new(),
            warned: false,
        }
    }

    /// Watch `dir` non-recursively unless it already is. A failure (e.g. the inotify
    /// watch limit) is logged once and then suppressed, leaving that subtree unwatched
    /// rather than aborting the watcher.
    fn watch(&mut self, dir: PathBuf) {
        if self.dirs.contains(&dir) {
            return;
        }
        match self.watcher.watch(&dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                self.dirs.insert(dir);
            }
            Err(err) => {
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(%err, "cannot register filesystem watch (OS watch limit?); some changes may be missed");
                }
            }
        }
    }

    fn unwatch(&mut self, dir: &Path) {
        if self.dirs.remove(dir) {
            // The directory is usually already gone, so an error here is expected.
            let _ = self.watcher.unwatch(dir);
        }
    }

    /// Reconcile the watched set to exactly the non-ignored directories of `snapshot`
    /// (its root plus every directory entry), adding new watches and dropping stale
    /// ones.
    fn reconcile(&mut self, snapshot: &Snapshot) {
        let mut desired = BTreeSet::new();
        desired.insert(snapshot.root().to_path_buf());
        for (relative, entry) in snapshot.entries() {
            if entry.kind == EntryKind::Dir {
                desired.insert(snapshot.root().join(relative));
            }
        }

        // Materialize stale paths before mutating, since unwatch borrows `self.dirs`.
        let stale: Vec<PathBuf> = self.dirs.difference(&desired).cloned().collect();
        for dir in stale {
            self.unwatch(&dir);
        }
        for dir in desired {
            self.watch(dir);
        }
    }
}

/// The background worker: coalesce event bursts and, at each quiet point, rescan and
/// diff against the held snapshot to emit a change batch. The notify watcher lives
/// here (inside `watches`), so the event channel stays open for the worker's life.
fn run(
    mut watches: WatchSet,
    mut snapshot: Snapshot,
    events: Receiver<notify::Result<Event>>,
    changes: mpsc::Sender<Vec<Change>>,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Relaxed) {
        // Block (waking periodically to notice shutdown) until a burst begins.
        match events.recv_timeout(IDLE_POLL) {
            Ok(event) => log_event_error(&event),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        }

        // Coalesce the rest of the burst: drain until the stream is quiet for
        // DEBOUNCE, or the burst has run for MAX_COALESCE.
        let burst_start = Instant::now();
        loop {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }
            match events.recv_timeout(DEBOUNCE) {
                Ok(event) => {
                    log_event_error(&event);
                    if burst_start.elapsed() >= MAX_COALESCE {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        // Reconcile against the current tree and emit any deltas.
        let next = match Snapshot::scan(snapshot.root()) {
            Ok(next) => next,
            Err(err) => {
                tracing::warn!(%err, "worktree rescan failed; keeping previous snapshot");
                continue;
            }
        };
        let batch = snapshot.diff(&next);
        if batch.is_empty() {
            continue;
        }
        snapshot = next;
        watches.reconcile(&snapshot);
        if changes.send(batch).is_err() {
            // The consumer dropped the receiver; nothing more to do.
            return;
        }
    }
}

/// A notify error surfaces through the same channel as events; the event payload is
/// otherwise unused (the flush rescans wholesale), so only errors are worth a line.
fn log_event_error(event: &notify::Result<Event>) {
    if let Err(err) = event {
        tracing::warn!(%err, "filesystem watch error");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    /// A self-cleaning temporary directory, mirroring the snapshot tests' helper so
    /// these stay self-contained without a `tempfile` dev-dependency.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("rift-watcher-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp root");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, contents).expect("write file");
    }

    /// Generous ceiling for a real filesystem event to cross notify, the debounce, the
    /// rescan, and the diff.
    const RECV_TIMEOUT: Duration = Duration::from_secs(5);

    fn recv_batch(rx: &Receiver<Vec<Change>>) -> Vec<Change> {
        rx.recv_timeout(RECV_TIMEOUT)
            .expect("a change batch within the timeout")
    }

    fn change_path(change: &Change) -> &Path {
        match change {
            Change::Added { path, .. }
            | Change::Changed { path, .. }
            | Change::Removed { path } => path,
        }
    }

    #[test]
    fn test_watcher_emits_added_on_file_create() {
        let tmp = TempDir::new("create");
        let root = &tmp.path;
        write_file(&root.join("existing.txt"), "x");
        let initial = Snapshot::scan(root).expect("scan");
        let (_watcher, rx) = Watcher::new(initial).expect("watcher");

        write_file(&root.join("new.txt"), "new");

        let batch = recv_batch(&rx);
        assert!(batch.iter().any(|c| {
            matches!(c, Change::Added { path, entry }
                if path == Path::new("new.txt") && entry.kind == EntryKind::File)
        }));
    }

    #[test]
    fn test_watcher_emits_removed_on_file_delete() {
        let tmp = TempDir::new("delete");
        let root = &tmp.path;
        write_file(&root.join("doomed.txt"), "x");
        let initial = Snapshot::scan(root).expect("scan");
        let (_watcher, rx) = Watcher::new(initial).expect("watcher");

        std::fs::remove_file(root.join("doomed.txt")).expect("remove");

        let batch = recv_batch(&rx);
        assert!(batch
            .iter()
            .any(|c| matches!(c, Change::Removed { path } if path == Path::new("doomed.txt"))));
    }

    #[test]
    fn test_watcher_emits_changed_on_file_modify() {
        let tmp = TempDir::new("modify");
        let root = &tmp.path;
        let file = root.join("watched.txt");
        write_file(&file, "v1");
        let initial = Snapshot::scan(root).expect("scan");
        let before_mtime = initial.get(Path::new("watched.txt")).expect("entry").mtime;
        let (_watcher, rx) = Watcher::new(initial).expect("watcher");

        // set_modified bumps the mtime to a known-later instant *and* triggers a
        // metadata event, so the rescan deterministically observes the change.
        let bumped = before_mtime + Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .expect("open file")
            .set_modified(bumped)
            .expect("set mtime");

        let batch = recv_batch(&rx);
        assert!(batch.iter().any(|c| {
            matches!(c, Change::Changed { path, entry }
                if path == Path::new("watched.txt") && entry.mtime > before_mtime)
        }));
    }

    #[test]
    fn test_watcher_excludes_writes_inside_ignored_dirs() {
        let tmp = TempDir::new("ignored");
        let root = &tmp.path;
        write_file(&root.join(".gitignore"), "target/\n");
        write_file(&root.join("src/main.rs"), "fn main() {}");
        let initial = Snapshot::scan(root).expect("scan");
        let (_watcher, rx) = Watcher::new(initial).expect("watcher");

        // A write inside an ignored dir must not surface; a tracked write right after
        // guarantees a flush we can inspect.
        write_file(&root.join("target/debug/app"), "binary");
        write_file(&root.join("src/lib.rs"), "pub fn lib() {}");

        let batch = recv_batch(&rx);
        assert!(batch
            .iter()
            .any(|c| matches!(c, Change::Added { path, .. } if path == Path::new("src/lib.rs"))));
        assert!(batch.iter().all(|c| !change_path(c).starts_with("target")));
    }

    #[test]
    fn test_watcher_coalesces_rapid_events_on_one_path() {
        let tmp = TempDir::new("coalesce");
        let root = &tmp.path;
        let file = root.join("hot.txt");
        write_file(&file, "0");
        let initial = Snapshot::scan(root).expect("scan");
        let mut mtime = initial.get(Path::new("hot.txt")).expect("entry").mtime;
        let (_watcher, rx) = Watcher::new(initial).expect("watcher");

        // Fire a burst of metadata changes well within one debounce window.
        const WRITES: usize = 6;
        for _ in 0..WRITES {
            mtime += Duration::from_secs(60);
            std::fs::File::options()
                .write(true)
                .open(&file)
                .expect("open file")
                .set_modified(mtime)
                .expect("set mtime");
        }

        // Collect every batch up to a quiet window comfortably past the debounce.
        let mut batches = vec![recv_batch(&rx)];
        while let Ok(batch) = rx.recv_timeout(DEBOUNCE * 4) {
            batches.push(batch);
        }
        assert!(
            batches.len() < WRITES,
            "expected coalescing: {} batches for {WRITES} writes",
            batches.len()
        );
        assert!(batches
            .iter()
            .flatten()
            .any(|c| matches!(c, Change::Changed { path, .. } if path == Path::new("hot.txt"))));
    }

    #[test]
    fn test_watcher_registers_watch_for_new_subdir() {
        let tmp = TempDir::new("new-subdir");
        let root = &tmp.path;
        write_file(&root.join("src/main.rs"), "fn main() {}");
        let initial = Snapshot::scan(root).expect("scan");
        let (_watcher, rx) = Watcher::new(initial).expect("watcher");

        // A brand-new directory plus a file inside it: the parent (root) watch fires,
        // the rescan picks the file up, and reconcile must register a watch on the new
        // dir. Collect across batches since the dir and file creations may split.
        write_file(&root.join("pkg/mod.rs"), "pub mod pkg;");
        let mut changes = recv_batch(&rx);
        while let Ok(more) = rx.recv_timeout(DEBOUNCE * 4) {
            changes.extend(more);
        }
        assert!(changes.iter().any(|c| {
            matches!(c, Change::Added { path, entry }
                if path == Path::new("pkg/mod.rs") && entry.kind == EntryKind::File)
        }));

        // Change a file *inside* the new dir. Root's non-recursive watch cannot see
        // this — only a watch registered on `pkg/` by the reconcile surfaces it, which
        // proves the dynamic watch is live rather than a one-shot rescan artifact.
        let bumped = Snapshot::scan(root)
            .expect("rescan")
            .get(Path::new("pkg/mod.rs"))
            .expect("entry")
            .mtime
            + Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(root.join("pkg/mod.rs"))
            .expect("open file")
            .set_modified(bumped)
            .expect("set mtime");

        let batch = recv_batch(&rx);
        assert!(batch
            .iter()
            .any(|c| matches!(c, Change::Changed { path, .. } if path == Path::new("pkg/mod.rs"))));
    }
}
