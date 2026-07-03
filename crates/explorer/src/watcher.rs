//! A [`Watcher`] that turns filesystem events under a worktree root into coalesced
//! [`Change`] batches against a held [`Snapshot`].
//!
//! Events are used only as a *trigger*: on each quiet point the watcher rescans the
//! tree and diffs the fresh [`Snapshot`] against the held one, so a move falls out as
//! remove + add. Gitignored entries appear in the snapshot (#309) but
//! [`WatchSet::reconcile`] excludes them from the OS-watched set, so only
//! non-ignored directories are watched — a large ignored tree like `target/`
//! (excluded from the scan entirely) or `dist/`/`.venv/` (scanned, but unwatched)
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
use crate::{ExplorerError, GitStatus, Result, Snapshot};

/// What [`Watcher::start`] returns: the watcher, the worktree change receiver,
/// and (in git mode) the git-status receiver.
type StartParts = (Watcher, Receiver<Vec<Change>>, Option<Receiver<GitStatus>>);

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
        let (watcher, changes, _git) = Self::start(initial, false)?;
        Ok((watcher, changes))
    }

    /// Like [`Watcher::new`], but additionally watches the repository's `.git/`
    /// control files and recomputes the git status on every flush, emitting it
    /// on a second receiver.
    ///
    /// This reuses the *same* `notify` backend and worker as the worktree watch
    /// — the `.git/` whitelist (`.git` non-recursively for `HEAD` / `index` /
    /// `packed-refs`, `.git/refs` recursively for branch refs) is a second
    /// watched set layered on it, not a separate watcher stack. So a worktree
    /// edit *and* a `.git/`-only change (commit, `git add`, branch switch) each
    /// trigger a debounced recompute. A recompute that fails (e.g. a transient
    /// `index.lock` mid-write) is logged and skipped; the next change recomputes.
    pub fn with_git_status(
        initial: Snapshot,
    ) -> Result<(Self, Receiver<Vec<Change>>, Receiver<GitStatus>)> {
        let (watcher, changes, git) = Self::start(initial, true)?;
        Ok((
            watcher,
            changes,
            git.expect("git receiver is present when git watching is requested"),
        ))
    }

    /// Shared setup for [`Watcher::new`] and [`Watcher::with_git_status`]. With
    /// `git`, registers the `.git/` whitelist and returns a git-status receiver.
    fn start(initial: Snapshot, git: bool) -> Result<StartParts> {
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
        if git {
            // The worktree scan excludes `.git/`, so add a bounded second watched
            // set for the control files git mutates: `.git` non-recursively
            // (HEAD, index, packed-refs are direct children) and `.git/refs`
            // recursively (branch refs). Non-recursive on `.git` deliberately
            // skips the heavy `.git/objects` churn during gc/rebase.
            let git_dir = initial.root().join(".git");
            watches.watch_external(&git_dir, RecursiveMode::NonRecursive);
            watches.watch_external(&git_dir.join("refs"), RecursiveMode::Recursive);
        }

        let (change_tx, change_rx) = mpsc::channel();
        let (git_tx, git_rx) = if git {
            let (tx, rx) = mpsc::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = std::thread::Builder::new()
            .name("rift-explorer-watch".to_owned())
            .spawn(move || {
                run(
                    watches,
                    initial,
                    event_rx,
                    change_tx,
                    git_tx,
                    worker_shutdown,
                )
            })
            .map_err(|err| ExplorerError::WatchError(err.to_string()))?;

        Ok((
            Self {
                shutdown,
                worker: Some(worker),
            },
            change_rx,
            git_rx,
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
    warned_git: bool,
}

impl WatchSet {
    fn new(watcher: RecommendedWatcher) -> Self {
        Self {
            watcher,
            dirs: BTreeSet::new(),
            warned: false,
            warned_git: false,
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

    /// Watch `path` once, outside the snapshot-reconciled `dirs` set, so
    /// [`WatchSet::reconcile`] never unwatches it. Used for the `.git/` control
    /// whitelist. A missing path or watch-limit failure is logged once and
    /// skipped, never fatal.
    fn watch_external(&mut self, path: &Path, mode: RecursiveMode) {
        if let Err(err) = self.watcher.watch(path, mode) {
            // Separate latch from `warned`: a `.git/` registration failure (a
            // missing path, or `.git` being a file for a linked worktree) is a
            // distinct condition from the worktree watch-limit warning, and must
            // not suppress the other's diagnostics.
            if !self.warned_git {
                self.warned_git = true;
                tracing::warn!(%err, path = %path.display(), "cannot register .git watch; some git-state changes may be missed");
            }
        }
    }

    /// Reconcile the watched set to exactly the non-ignored directories of `snapshot`
    /// (its root plus every directory entry), adding new watches and dropping stale
    /// ones.
    ///
    /// The `!entry.ignored` filter preserves this module's invariant now that
    /// the scan includes gitignored entries (#309): an ignored directory (e.g. a
    /// large `dist/` or `.venv/` outside the hardcoded perf set) is shown from
    /// the scan and refreshed on the next debounced full rescan, but never
    /// consumes a dedicated OS watch.
    fn reconcile(&mut self, snapshot: &Snapshot) {
        let mut desired = BTreeSet::new();
        desired.insert(snapshot.root().to_path_buf());
        for (relative, entry) in snapshot.entries() {
            if entry.kind == EntryKind::Dir && !entry.ignored {
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
    git: Option<mpsc::Sender<GitStatus>>,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Relaxed) {
        // Block (waking periodically to notice shutdown) until a burst begins.
        let mut saw_change = match events.recv_timeout(IDLE_POLL) {
            Ok(event) => {
                log_event_error(&event);
                is_change(&event)
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };

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
                    saw_change |= is_change(&event);
                    if burst_start.elapsed() >= MAX_COALESCE {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        // A burst of only `Access` (open/read) events is not a change. Our own
        // rescan and the git recompute open watched directories, which inotify
        // reports as `Access` — flushing on those would feed back into an
        // endless rescan loop. Wait for a real create/modify/remove instead.
        if !saw_change {
            continue;
        }

        // Reconcile against the current tree and emit any worktree deltas. A
        // rescan failure is logged but must not skip the git recompute below: a
        // `.git/`-only change leaves the worktree unchanged yet still alters git
        // status.
        match Snapshot::scan(snapshot.root()) {
            Ok(next) => {
                let batch = snapshot.diff(&next);
                if !batch.is_empty() {
                    snapshot = next;
                    watches.reconcile(&snapshot);
                    if changes.send(batch).is_err() {
                        // The consumer dropped the receiver; nothing more to do.
                        return;
                    }
                }
            }
            Err(err) => tracing::warn!(%err, "worktree rescan failed; keeping previous snapshot"),
        }

        // Git status recompute (git mode only). Runs on every flush — a worktree
        // edit or any `.git/` control change. A compute error (e.g. a transient
        // `index.lock` while git is mid-write) is logged and skipped; the next
        // flush recomputes, so a momentary lock never aborts watching.
        if let Some(git_tx) = &git {
            match GitStatus::compute(snapshot.root()) {
                Ok(status) => {
                    if git_tx.send(status).is_err() {
                        return;
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, "git status recompute failed; retrying on next change")
                }
            }
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

/// Whether an event represents an actual filesystem change worth a rescan.
///
/// `Access` (open/read/close) events are pure reads — and our own rescan and
/// git recompute generate them by opening watched directories — so they must
/// not trigger a flush, or the watcher feeds back into an endless loop. Errors
/// are not changes either (they are logged separately).
///
/// Excluding the whole `Access(_)` family — including `Access(Close(Write))` —
/// is safe: `notify`'s supported backends report an actual write as a
/// `Modify` event, not as a close-for-write Access, so no real mutation is
/// lost. `EventKind::Any`/`Other` pass through as changes (a spurious extra
/// rescan, never a missed change).
fn is_change(event: &notify::Result<Event>) -> bool {
    matches!(event, Ok(ev) if !matches!(ev.kind, notify::EventKind::Access(_)))
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

    /// Invariant guard (#309): once ignored entries appear in the snapshot, a
    /// gitignored directory outside the hardcoded perf set (e.g. `dist/`) must
    /// still never consume an OS watch — only the perf set is excluded from the
    /// scan outright; everything else ignored is scanned but unwatched. Tests
    /// `reconcile` directly against a seeded snapshot rather than through real
    /// filesystem events, since the watch *set*, not event delivery, is what's
    /// under test.
    #[test]
    fn test_reconcile_excludes_ignored_directories_from_watch_set() {
        let tmp = TempDir::new("reconcile-ignored");
        let root = &tmp.path;
        write_file(&root.join(".gitignore"), "dist/\n");
        write_file(&root.join("src/main.rs"), "fn main() {}");
        write_file(&root.join("dist/bundle.js"), "console.log(1)");

        let snapshot = Snapshot::scan(root).expect("scan");
        assert_eq!(
            snapshot.get(Path::new("dist")).map(|e| e.ignored),
            Some(true),
            "fixture assumption: dist/ is gitignored"
        );
        assert_eq!(
            snapshot.get(Path::new("src")).map(|e| e.ignored),
            Some(false)
        );

        let (event_tx, _event_rx) = mpsc::channel();
        let watcher = RecommendedWatcher::new(
            move |result: notify::Result<Event>| {
                let _ = event_tx.send(result);
            },
            Config::default(),
        )
        .expect("create watcher");
        let mut watches = WatchSet::new(watcher);
        watches.reconcile(&snapshot);

        let root = snapshot.root().to_path_buf();
        assert!(watches.dirs.contains(&root));
        assert!(watches.dirs.contains(&root.join("src")));
        assert!(
            !watches.dirs.contains(&root.join("dist")),
            "an ignored directory must never be OS-watched"
        );
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

    // --- git-status watching (#133) ---

    use crate::{GitStatus, GitStatusCode};
    use std::process::Command;

    /// Run a git command in `dir`, asserting success. Real `git` is the ground
    /// truth for the `.git/` mutations the watcher must observe.
    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// An initialized repo with one committed file on `main`, plus a started
    /// git-status watcher and its receivers.
    fn init_repo_with_watcher(
        tag: &str,
    ) -> (TempDir, Watcher, Receiver<Vec<Change>>, Receiver<GitStatus>) {
        let tmp = TempDir::new(tag);
        git(&tmp.path, &["init", "-q", "-b", "main"]);
        write_file(&tmp.path.join("tracked.txt"), "v1\n");
        git(&tmp.path, &["add", "tracked.txt"]);
        git(&tmp.path, &["commit", "-q", "-m", "init"]);

        let snapshot = Snapshot::scan(&tmp.path).expect("scan");
        let (watcher, changes, git_rx) = Watcher::with_git_status(snapshot).expect("git watcher");
        (tmp, watcher, changes, git_rx)
    }

    fn recv_git(rx: &Receiver<GitStatus>) -> GitStatus {
        rx.recv_timeout(RECV_TIMEOUT)
            .expect("a git status within the timeout")
    }

    /// Drain to the most recent git status received within a short settle window,
    /// so coalesced/late recomputes don't leave a stale value under inspection.
    fn recv_latest_git(rx: &Receiver<GitStatus>) -> GitStatus {
        let mut last = recv_git(rx);
        while let Ok(next) = rx.recv_timeout(DEBOUNCE * 4) {
            last = next;
        }
        last
    }

    #[test]
    fn test_git_watch_worktree_edit_triggers_recompute() {
        let (tmp, _w, _changes, git_rx) = init_repo_with_watcher("wt-edit");
        // An untracked file is a pure worktree change (no `.git/` mutation): it
        // must still trigger a git recompute.
        write_file(&tmp.path.join("loose.txt"), "x\n");

        let status = recv_latest_git(&git_rx);
        assert_eq!(
            status.get(Path::new("loose.txt")).map(|s| s.worktree),
            Some(GitStatusCode::Untracked)
        );
    }

    #[test]
    fn test_git_watch_staging_triggers_recompute_via_git_index() {
        let (tmp, _w, _changes, git_rx) = init_repo_with_watcher("stage");
        write_file(&tmp.path.join("tracked.txt"), "v2\n");
        // `git add` mutates `.git/index` — observed through the `.git` whitelist.
        git(&tmp.path, &["add", "tracked.txt"]);

        let status = recv_latest_git(&git_rx);
        assert_eq!(
            status.get(Path::new("tracked.txt")).map(|s| s.index),
            Some(GitStatusCode::Modified),
            "staging must surface on the index side after a recompute"
        );
    }

    #[test]
    fn test_git_watch_commit_triggers_recompute_to_clean() {
        let (tmp, _w, _changes, git_rx) = init_repo_with_watcher("commit");
        write_file(&tmp.path.join("tracked.txt"), "v2\n");
        git(&tmp.path, &["add", "tracked.txt"]);
        // Committing (a `.git/`-only change beyond the index) clears the status.
        git(&tmp.path, &["commit", "-q", "-m", "change"]);

        let status = recv_latest_git(&git_rx);
        assert!(
            status.get(Path::new("tracked.txt")).is_none(),
            "after commit the file is clean: {:?}",
            status.get(Path::new("tracked.txt"))
        );
    }

    #[test]
    fn test_git_watch_branch_switch_updates_repo_state() {
        let (tmp, _w, _changes, git_rx) = init_repo_with_watcher("branch");
        // A branch switch mutates `.git/HEAD` only — observed via the whitelist.
        git(&tmp.path, &["checkout", "-q", "-b", "feature"]);

        let status = recv_latest_git(&git_rx);
        assert_eq!(status.repo().branch.as_deref(), Some("feature"));
    }

    #[test]
    fn test_git_watch_coalesces_rapid_edits() {
        let (tmp, _w, _changes, git_rx) = init_repo_with_watcher("coalesce");
        const WRITES: usize = 6;
        for i in 0..WRITES {
            write_file(&tmp.path.join(format!("f{i}.txt")), "x\n");
        }

        let mut recomputes = vec![recv_git(&git_rx)];
        while let Ok(status) = git_rx.recv_timeout(DEBOUNCE * 4) {
            recomputes.push(status);
        }
        assert!(
            recomputes.len() < WRITES,
            "expected coalescing: {} recomputes for {WRITES} writes",
            recomputes.len()
        );
    }

    #[test]
    fn test_git_watch_tolerates_stale_index_lock() {
        let (tmp, _w, _changes, git_rx) = init_repo_with_watcher("lock");
        // A leftover (stale) index.lock must not abort watching: the watcher
        // keeps recomputing and a later change still yields a status. (gix reads
        // succeed past a stale lock file; the genuine mid-write torn-index error
        // can't be reproduced deterministically without a lock-holding process,
        // but the compute-error path is logged-and-skipped in `run`.)
        write_file(&tmp.path.join(".git/index.lock"), "");
        write_file(&tmp.path.join("after_lock.txt"), "x\n");

        let status = recv_latest_git(&git_rx);
        assert_eq!(
            status.get(Path::new("after_lock.txt")).map(|s| s.worktree),
            Some(GitStatusCode::Untracked),
            "watcher must keep recomputing despite a present index.lock"
        );
    }
}
