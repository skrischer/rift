use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

mod browse;
mod buffer;
mod clone;
mod diff;
mod file_ops;
mod git_write;
pub mod lsp;
mod terminal;

use lsp::{document_changes, BufferEvent, LspDiagnostics, LspStatusEvent, LspWorker, NavRequest};
use rift_explorer::{Change, Entry, GitStatus, Snapshot, Watcher};
use rift_lsp::{DocumentChange, DocumentSelector};
use rift_protocol::{
    encode_frame, BufferErrorReason, ClientMessage, DaemonMessage, Diagnostic, EntryKind,
    FrameDecoder, LspServerState, NavRequestId, WorktreeEntry, PROTOCOL_VERSION,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tracing::{error, info, warn};

/// Single source of truth for the daemon's observable worktree/git state.
///
/// Held as the value of a `tokio::sync::watch` channel so consumers observe the
/// latest snapshot without sharing a mutex. The terminal path is not part of
/// this shared state — each connection drives its own per-client tmux attach
/// (see [`terminal`]).
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Latest worktree snapshot, present once the initial scan completes.
    /// Kept current by applying the watcher's change batches in place.
    pub worktree: Option<Snapshot>,
    /// Latest git status for the worktree, present when the root is a git
    /// repository and the first recompute has landed. Replaced wholesale by
    /// each recompute; the dispatch loop diffs consecutive values to stream
    /// incremental updates.
    pub git: Option<GitStatus>,
    /// Latest diagnostics per `(worktree-relative path, server id)`, mirroring
    /// LSP's full-set-per-`(file, server)` replace semantics. Each language
    /// server's published set replaces only its own entry for the file, so a
    /// linter and a type-checker aggregate without clobbering one another. An
    /// empty published set clears that server's entry entirely (the key is
    /// removed) so the map only ever holds live diagnostics. Replayed per
    /// connection alongside the worktree and git snapshots.
    pub diagnostics: BTreeMap<DiagnosticKey, Vec<Diagnostic>>,
    /// Latest lifecycle state per language server name (issue #520), e.g.
    /// `"rust-analyzer" -> Running`. Unlike `diagnostics`, an entry is never
    /// removed once a server has been observed — a server that has ever
    /// started is always exactly one of `starting`/`running`/`crashed`, never
    /// absent. Replayed per connection alongside the worktree, git, and
    /// diagnostics snapshots.
    pub lsp_status: BTreeMap<String, LspServerState>,
}

/// Map key for [`State::diagnostics`]: a worktree-relative path paired with the
/// daemon-assigned id of the publishing server, as a string. The per-server
/// component is what lets two servers' sets for one file coexist.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DiagnosticKey {
    pub path: String,
    pub server: String,
}

/// Internal events from the worktree worker into the dispatch loop.
enum WorktreeEvent {
    /// The initial scan completed; this snapshot becomes the `State` worktree.
    Scanned(Snapshot),
    /// A coalesced batch of changes against the previously delivered state.
    Changed(Vec<Change>),
    /// A fresh full git status (recomputed on a worktree or `.git/` change).
    /// The dispatch loop diffs it against the held one to emit incrementals.
    GitRecomputed(GitStatus),
}

/// Wiring for the daemon's flat dispatch loop.
///
/// Inbound `ClientMessage`s arrive on `inbound`; worktree scan/watch events
/// arrive on `worktree` (when [`Daemon::watch_worktree`] armed one); outbound
/// `DaemonMessage` events are published on the event bus; the latest `State`
/// snapshot is observable via the `watch` receiver returned alongside this
/// struct by [`channels`].
pub struct Daemon {
    inbound: mpsc::Receiver<ClientMessage>,
    worktree: Option<mpsc::Receiver<WorktreeEvent>>,
    /// Diagnostics translated by the off-loop LSP worker, polled as a dispatch
    /// branch. `None` until [`Daemon::watch_lsp`] arms the worker; the branch
    /// then pends forever (never fires) so the loop is unaffected when LSP is
    /// off.
    lsp_diagnostics: Option<mpsc::Receiver<LspDiagnostics>>,
    /// Lifecycle transitions from the off-loop LSP worker (issue #520), polled
    /// as a dispatch branch alongside `lsp_diagnostics`. Same `None`-until-armed
    /// discipline.
    lsp_status: Option<mpsc::Receiver<LspStatusEvent>>,
    core: Core,
}

/// The dispatch loop's owned half: the `State` writer and the event
/// broadcaster. Split from [`Daemon`] so `run` can poll the inbound channels
/// while a completed branch's handler mutates this.
struct Core {
    events: broadcast::Sender<DaemonMessage>,
    state: watch::Sender<State>,
    /// Forwards document changes (mapped from worktree `Changed` batches) to the
    /// off-loop LSP worker. `None` when LSP is not armed; the dispatch loop then
    /// derives no document changes and the branch is inert.
    doc_changes: Option<mpsc::Sender<Vec<DocumentChange>>>,
    /// Forwards the editor's live-buffer events (`BufferChanged` / `BufferClosed`,
    /// #189) to the off-loop LSP worker — the disk→buffer source-of-truth shift.
    /// `None` when LSP is not armed; the dispatch loop then drops buffer events.
    buffer_events: Option<mpsc::Sender<BufferEvent>>,
    /// The canonical navigation-request sender into the off-loop LSP worker
    /// (#195, #482). `None` when LSP is not armed. Unlike the disk/buffer feeds
    /// above, the dispatch loop does not forward nav itself: each connection
    /// holds its own clone (see [`serve_connection`]) so it can attach its
    /// private `reply` channel and receive the answer on its own socket alone.
    /// The dispatch loop keeps this original alive for the daemon's lifetime so
    /// the worker's nav channel does not close as clients come and go.
    nav_requests: Option<mpsc::Sender<NavRequest>>,
}

/// Sender handles for driving a [`Daemon`].
///
/// `Clone`: every field is itself a channel handle (`mpsc`/`broadcast`/
/// `watch`), so cloning `Handles` never creates a second dispatch loop — it
/// hands out another set of handles onto the SAME one. [`ContextMap`] relies
/// on this to share one [`Context`] across every acquirer of a root.
#[derive(Clone)]
pub struct Handles {
    /// Send `ClientMessage`s into the dispatch loop.
    pub inbound: mpsc::Sender<ClientMessage>,
    /// Internal publisher handle for the outbound `DaemonMessage` event bus.
    /// Not public: external callers subscribe via [`Handles::subscribe`] and
    /// cannot inject events.
    events: broadcast::Sender<DaemonMessage>,
    /// Observe the latest `State` snapshot.
    pub state: watch::Receiver<State>,
}

impl Handles {
    /// Subscribe to the daemon's outbound `DaemonMessage` event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<DaemonMessage> {
        self.events.subscribe()
    }
}

/// Construct a [`Daemon`] and its [`Handles`] over freshly created channels.
///
/// `event_capacity` bounds the broadcast backlog; `inbound_capacity` bounds the
/// inbound mpsc queue.
pub fn channels(event_capacity: usize, inbound_capacity: usize) -> (Daemon, Handles) {
    let (inbound_tx, inbound_rx) = mpsc::channel(inbound_capacity);
    let (events_tx, _events_rx) = broadcast::channel(event_capacity);
    let (state_tx, state_rx) = watch::channel(State::default());

    let daemon = Daemon {
        inbound: inbound_rx,
        worktree: None,
        lsp_diagnostics: None,
        lsp_status: None,
        core: Core {
            events: events_tx.clone(),
            state: state_tx,
            doc_changes: None,
            buffer_events: None,
            nav_requests: None,
        },
    };
    let handles = Handles {
        inbound: inbound_tx,
        events: events_tx,
        state: state_rx,
    };
    (daemon, handles)
}

/// Capacities for the channels backing a daemon dispatch loop.
///
/// The inbound queue absorbs bursts of `ClientMessage`s while the dispatch loop
/// drains them; the broadcast backlog bounds how far an outbound writer may lag
/// before lagged events are dropped. [`serve`] drives a single connection;
/// [`serve_uds`] shares one dispatch loop across all attached clients, so these
/// bound the combined queue depth there.
const SERVE_INBOUND_CAPACITY: usize = 256;
const SERVE_EVENT_CAPACITY: usize = 256;

/// Read buffer for a single transport read. The transport delivers arbitrary
/// chunk sizes; the [`FrameDecoder`] reassembles frames regardless of this size.
const SERVE_READ_BUFFER: usize = 8 * 1024;

/// Per-connection terminal channel bounds. `INBOUND` queues the connection's
/// terminal `ClientMessage`s for its tmux attach; `OUTBOUND` bounds the attach's
/// event backlog — the daemon→client flow-control leg, so a flooding pane can
/// never grow this without bound (its backpressure pauses the pane tmux-side).
const TERMINAL_INBOUND_CAPACITY: usize = 256;
const TERMINAL_OUTBOUND_CAPACITY: usize = 256;

/// Per-connection resolved-root channel bound (#737, the Attach seam): at
/// most one value in flight per attach, so this only needs to absorb a rapid
/// re-attach without blocking the tmux read loop — see `terminal::RootResolved`.
const ROOT_RESOLVED_CAPACITY: usize = 4;

/// How long [`ContextMap::release`]'s last-reference path waits for the torn-
/// down context's dispatch loop to join, while holding the registry-wide
/// lock (the atomicity fix, #737). Generous — LSP shutdown can take a
/// moment — but bounded: a FUTURE violation of the "caller drops its own
/// clones before the final release" contract (documented on `release`) would
/// otherwise wedge EVERY root's acquire/release behind a join that never
/// completes; past this, the join is abandoned (the task keeps running
/// detached — a logged leak, not a forced abort) rather than the whole
/// registry hanging forever.
const CONTEXT_RELEASE_JOIN_TIMEOUT: Duration = Duration::from_secs(30);

/// How long [`keep_warm_supervisor`] waits, after the primary/`--root`
/// context's last client disconnects, before releasing it via
/// [`ContextMap::release`] — tearing down its `rust-analyzer` (#551, the
/// observed multi-GB orphan). Re-acquired (fresh scan + LSP spawn) on the
/// next connection, so a returning client still gets a working reactive
/// layer.
///
/// Must clear the mid-session daemon-stream recovery window
/// (`reconnect_daemon` in `crates/app/src/main.rs`, #475): 10 attempts under
/// `rift_ssh::ReconnectBackoff`'s capped schedule sum to ~190s worst case with
/// jitter (`docs/spec-connection-robustness.md` calls the unjittered sum
/// "~2 min") before that layer gives up — well inside this grace, so a
/// transient SSH drop always reattaches to the still-warm context. A longer
/// outage falls through to the SSH-level reconnect engine (#476, unlimited
/// retries); intentionally not covered here — the context is simply
/// re-acquired on the eventual reconnect, per the issue's "or at minimum stop
/// its spawned language servers" acceptance.
const KEEP_WARM_RELEASE_GRACE: Duration = Duration::from_secs(240);

/// Bound on outstanding `serve_uds` accept-loop <-> [`keep_warm_supervisor`]
/// traffic — a handful of connections at a time
/// (`docs/spec-dogfooding-channels.md`), so a small fixed capacity never
/// backs up.
const KEEP_WARM_EVENT_CAPACITY: usize = 16;

/// Per-connection navigation-reply channel bound (#482): the private inbox the
/// LSP worker's spawned nav tasks send this connection's hover/definition/
/// references answers to. Nav responses are user-paced (one per hover or click),
/// so a small buffer absorbs a burst without ever backing up the worker.
const NAV_REPLY_CAPACITY: usize = 32;

/// Per-connection clone-reply channel bound (#828, `docs/spec-clone-repo.md`):
/// the private inbox each connection's detached `clone::run` tasks post their
/// single `CloneResult` to. A clone is an operator-paced, rare action (unlike
/// terminal or nav traffic), so a small buffer is ample.
const CLONE_REPLY_CAPACITY: usize = 8;

/// Queue depth for worktree events flowing from the blocking worker into the
/// dispatch loop. Bounds how far the worker may run ahead while the loop is busy.
const WORKTREE_EVENT_CAPACITY: usize = 64;

/// Queue depth for document-change batches the dispatch loop forwards to the
/// off-loop LSP worker, and for the diagnostics the worker hands back. Bounds
/// the in-flight backlog without coupling either side to the other's pace.
const LSP_CHANNEL_CAPACITY: usize = 64;

/// Entries per `WorktreeSnapshot` chunk. Bounds a single frame's size so a
/// large tree streams as several frames instead of one giant allocation.
const SNAPSHOT_CHUNK: usize = 1024;

/// How often the worktree worker, while idle, checks whether the dispatch loop
/// is gone so it can release the watcher instead of blocking forever.
const WORKTREE_IDLE_POLL: Duration = Duration::from_millis(500);

/// Scan-then-watch worker. Runs on a blocking thread (`spawn_blocking`): the
/// initial scan, the watcher's lifetime, and the blocking relay of its change
/// batches all live here, so the dispatch loop never blocks on filesystem work.
/// A scan or watch failure is logged and degrades to "no worktree" — the daemon
/// keeps serving (stderr is the daemon's log sink).
///
/// Git watching is not fixed at startup. A root that is not a repository yet is
/// watched worktree-only while `GitStatus::compute` is re-probed on every tick;
/// the moment a repository appears (`git init`, or a transient boot-time
/// unreadability clearing) the worker upgrades in place to git watching against
/// a fresh baseline (#483). Git mode itself is terminal — a repo that loses its
/// `.git` just fails recomputes, which the git relay already tolerates (#430).
fn worktree_worker(root: PathBuf, events: mpsc::Sender<WorktreeEvent>) {
    loop {
        let snapshot = match Snapshot::scan(&root) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                error!(root = %root.display(), %err, "worktree scan failed");
                return;
            }
        };

        // Probe for a repository at the root. A successful compute means it is
        // one, so arm git watching (`with_git_status`) — the `.git/` whitelist
        // plus a recompute per flush. An error (not a repo yet, or git
        // unreadable) watches worktree-only (`Watcher::new`) and re-probes until
        // a repo appears, so a non-repo root still streams its file tree without
        // spamming per-flush git errors.
        //
        // The watcher is armed BEFORE the snapshot/status is delivered: it
        // registers its watch set synchronously, so once a consumer has observed
        // the initial state, any later change is guaranteed to produce an event.
        // The reverse order races — a write right after delivery would precede
        // the watches and, with no event, the rescan-on-event watcher would never
        // surface it. The clone is the two-owner boundary: the watcher keeps the
        // diff baseline, the dispatch loop's `State` gets its own copy.
        match GitStatus::compute(&root) {
            Ok(initial_git) => {
                watch_git_mode(snapshot, initial_git, &events);
                return;
            }
            Err(err) => {
                info!(root = %root.display(), %err, "no git status; watching worktree only");
                let (_watcher, changes) = match Watcher::new(snapshot.clone()) {
                    Ok(pair) => pair,
                    Err(err) => {
                        error!(%err, "worktree watch failed");
                        return;
                    }
                };
                if events
                    .blocking_send(WorktreeEvent::Scanned(snapshot))
                    .is_err()
                {
                    return;
                }
                match relay_worktree_until_git(&root, &changes, &events) {
                    // A repo appeared: drop this worktree-only watcher (end of
                    // its scope) and loop to arm a git-mode watch on a fresh scan.
                    RelayOutcome::Upgrade => continue,
                    // The dispatch loop is gone; the worker is done.
                    RelayOutcome::Stop => return,
                }
            }
        }
    }
}

/// Arm a git-mode watch on the snapshot's root, deliver the initial scan and git
/// status, then relay change batches and git recomputes until a channel closes
/// or the dispatch loop is gone. `initial_git` is the already-computed status
/// delivered right behind the scan — the watcher is armed before either is sent,
/// the ordering the worker relies on (see [`worktree_worker`]).
fn watch_git_mode(
    snapshot: Snapshot,
    initial_git: GitStatus,
    events: &mpsc::Sender<WorktreeEvent>,
) {
    let (_watcher, changes, git_rx) = match Watcher::with_git_status(snapshot.clone()) {
        Ok(triple) => triple,
        Err(err) => {
            error!(%err, "worktree watch failed");
            return;
        }
    };
    if events
        .blocking_send(WorktreeEvent::Scanned(snapshot))
        .is_err()
        || events
            .blocking_send(WorktreeEvent::GitRecomputed(initial_git))
            .is_err()
    {
        return;
    }
    relay_events(&changes, &git_rx, events);
}

/// Relay the watcher's change batches and git-status recomputes into the
/// dispatch loop until a channel closes or the loop is gone.
///
/// The git channel is the primary wait: `with_git_status` emits a recompute on
/// *every* successful flush, while worktree changes are emitted only on a
/// non-empty diff and always *before* the flush's git tick. So blocking on the
/// git tick and then draining the worktree changes already queued preserves the
/// order (tree update before its git decoration). A flush whose git recompute
/// fails (e.g. a transient `index.lock`) emits no tick, so the idle-poll timeout
/// drains the queued worktree changes too — the tree keeps updating while git
/// status recovers on a later tick (#430). The pre-repository phase has its own
/// relay ([`relay_worktree_until_git`]); this runs only once a repo exists.
fn relay_events(
    changes: &std::sync::mpsc::Receiver<Vec<Change>>,
    git: &std::sync::mpsc::Receiver<GitStatus>,
    events: &mpsc::Sender<WorktreeEvent>,
) {
    use std::sync::mpsc::RecvTimeoutError;
    loop {
        match git.recv_timeout(WORKTREE_IDLE_POLL) {
            Ok(status) => {
                // Drain the worktree changes this flush queued before its git
                // tick, so the tree update precedes the git decoration.
                if !drain_changes(changes, events) {
                    return;
                }
                if events
                    .blocking_send(WorktreeEvent::GitRecomputed(status))
                    .is_err()
                {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // No git tick — either idle, or a flush whose git recompute
                // failed and emitted none. Its worktree changes are already
                // queued; relay them so the client tree never stalls on a
                // failing recompute (#430).
                if !drain_changes(changes, events) {
                    return;
                }
                if events.is_closed() {
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Drain every queued worktree change batch into the dispatch loop. Returns
/// `false` when the loop is gone (the relay should stop).
fn drain_changes(
    changes: &std::sync::mpsc::Receiver<Vec<Change>>,
    events: &mpsc::Sender<WorktreeEvent>,
) -> bool {
    while let Ok(batch) = changes.try_recv() {
        if events.blocking_send(WorktreeEvent::Changed(batch)).is_err() {
            return false;
        }
    }
    true
}

/// Why the worktree-only relay ([`relay_worktree_until_git`]) returned: a
/// repository appeared (upgrade to git watching) or the dispatch loop is gone
/// (stop the worker).
enum RelayOutcome {
    Upgrade,
    Stop,
}

/// Relay worktree change batches while the root is not (yet) a git repository,
/// re-probing `GitStatus::compute` on every tick — each change flush and each
/// idle poll.
///
/// A bare `git init` mutates only `.git/`, which the worktree scan excludes, so
/// it yields no change batch; the idle-poll re-probe is what catches it (#483).
/// The probe is a cheap `gix::open` that fails fast on a non-repo, so polling it
/// each tick is inexpensive. Returns [`RelayOutcome::Upgrade`] the moment a repo
/// is detected — the caller then arms a git-mode watch against a fresh baseline
/// — or [`RelayOutcome::Stop`] when the dispatch loop has dropped the channel.
fn relay_worktree_until_git(
    root: &Path,
    changes: &std::sync::mpsc::Receiver<Vec<Change>>,
    events: &mpsc::Sender<WorktreeEvent>,
) -> RelayOutcome {
    use std::sync::mpsc::RecvTimeoutError;
    loop {
        match changes.recv_timeout(WORKTREE_IDLE_POLL) {
            Ok(batch) => {
                if events.blocking_send(WorktreeEvent::Changed(batch)).is_err() {
                    return RelayOutcome::Stop;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if events.is_closed() {
                    return RelayOutcome::Stop;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return RelayOutcome::Stop,
        }
        if GitStatus::compute(root).is_ok() {
            return RelayOutcome::Upgrade;
        }
    }
}

/// Receive the next worktree event, or pend forever when no worktree is armed
/// (so the `select!` branch never fires).
async fn next_worktree_event(
    rx: &mut Option<mpsc::Receiver<WorktreeEvent>>,
) -> Option<WorktreeEvent> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Receive the next translated diagnostics update from the off-loop LSP worker,
/// or pend forever when LSP is not armed (so the `select!` branch never fires).
async fn next_lsp_diagnostics(
    rx: &mut Option<mpsc::Receiver<LspDiagnostics>>,
) -> Option<LspDiagnostics> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Receive the next lifecycle transition from the off-loop LSP worker (issue
/// #520), or pend forever when LSP is not armed (so the `select!` branch
/// never fires).
async fn next_lsp_status(
    rx: &mut Option<mpsc::Receiver<LspStatusEvent>>,
) -> Option<LspStatusEvent> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Map an explorer entry onto its wire representation. Paths cross the
/// boundary lossily as UTF-8 (`to_string_lossy`) — the protocol is JSON today,
/// so a non-UTF-8 path cannot round-trip regardless.
fn wire_entry(path: &Path, entry: &Entry) -> WorktreeEntry {
    WorktreeEntry {
        path: path.to_string_lossy().into_owned(),
        kind: match entry.kind {
            rift_explorer::EntryKind::File => EntryKind::File,
            rift_explorer::EntryKind::Dir => EntryKind::Dir,
        },
        ignored: entry.ignored,
        mtime: entry.mtime,
    }
}

/// Fold a change batch into one `UpdateWorktree` message.
fn update_message(batch: &[Change]) -> DaemonMessage {
    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut removed = Vec::new();
    for change in batch {
        match change {
            Change::Added { path, entry } => added.push(wire_entry(path, entry)),
            Change::Changed { path, entry } => changed.push(wire_entry(path, entry)),
            Change::Removed { path } => removed.push(path.to_string_lossy().into_owned()),
        }
    }
    DaemonMessage::UpdateWorktree {
        added,
        changed,
        removed,
    }
}

/// Map an explorer porcelain code onto its protocol wire code (1:1).
fn wire_git_code(code: rift_explorer::GitStatusCode) -> rift_protocol::GitStatusCode {
    use rift_explorer::GitStatusCode as E;
    use rift_protocol::GitStatusCode as P;
    match code {
        E::Unmodified => P::Unmodified,
        E::Modified => P::Modified,
        E::TypeChange => P::TypeChange,
        E::Added => P::Added,
        E::Deleted => P::Deleted,
        E::Renamed => P::Renamed,
        E::Copied => P::Copied,
        E::Unmerged => P::Unmerged,
        E::Untracked => P::Untracked,
    }
}

/// Map an explorer git entry status + its path onto a wire `GitStatusEntry`.
fn wire_git_entry(
    path: &Path,
    status: &rift_explorer::GitEntryStatus,
) -> rift_protocol::GitStatusEntry {
    rift_protocol::GitStatusEntry {
        path: path.to_string_lossy().into_owned(),
        status: rift_protocol::GitEntryStatus {
            index: wire_git_code(status.index),
            worktree: wire_git_code(status.worktree),
        },
    }
}

/// Map an explorer diff outcome onto its wire payload.
fn wire_diff(diff: rift_explorer::FileDiff) -> rift_protocol::FileDiffPayload {
    match diff {
        rift_explorer::FileDiff::Hunks(hunks) => rift_protocol::FileDiffPayload::Hunks {
            hunks: hunks.into_iter().map(wire_diff_hunk).collect(),
        },
        rift_explorer::FileDiff::Binary => rift_protocol::FileDiffPayload::Binary,
        rift_explorer::FileDiff::TooLarge => rift_protocol::FileDiffPayload::TooLarge,
    }
}

/// Map one explorer diff hunk onto its wire representation.
fn wire_diff_hunk(hunk: rift_explorer::DiffHunk) -> rift_protocol::DiffHunk {
    rift_protocol::DiffHunk {
        old_start: hunk.old_start,
        old_len: hunk.old_len,
        new_start: hunk.new_start,
        new_len: hunk.new_len,
        lines: hunk
            .lines
            .into_iter()
            .map(|line| rift_protocol::DiffLine {
                kind: wire_diff_line_kind(line.kind),
                content: line.content,
            })
            .collect(),
    }
}

/// The protocol [`rift_protocol::hunk_fingerprint`] of an explorer hunk — the
/// single source of truth for a hunk's `hunk_id`, shared with the client (which
/// computes it over the wire [`rift_protocol::DiffHunk`] it received). The
/// `StageHunk` handler recomputes it over the file's fresh worktree-vs-HEAD
/// hunks to resolve a request's `hunk_id` back to a concrete hunk before
/// applying; a stale or content-changed id matches nothing and is rejected.
pub(crate) fn hunk_fingerprint(hunk: &rift_explorer::DiffHunk) -> u64 {
    rift_protocol::hunk_fingerprint(&wire_diff_hunk(hunk.clone()))
}

/// Map an explorer diff line role onto its wire code (1:1).
fn wire_diff_line_kind(kind: rift_explorer::DiffLineKind) -> rift_protocol::DiffLineKind {
    match kind {
        rift_explorer::DiffLineKind::Context => rift_protocol::DiffLineKind::Context,
        rift_explorer::DiffLineKind::Add => rift_protocol::DiffLineKind::Add,
        rift_explorer::DiffLineKind::Remove => rift_protocol::DiffLineKind::Remove,
    }
}

/// Build the `RepoState` message from an explorer repo state.
fn repo_state_message(repo: &rift_explorer::RepoState) -> DaemonMessage {
    DaemonMessage::RepoState {
        branch: repo.branch.clone(),
        ahead_behind: repo.ahead_behind.map(|ab| rift_protocol::AheadBehind {
            ahead: ab.ahead,
            behind: ab.behind,
        }),
        lines_added: repo.lines_added,
        lines_removed: repo.lines_removed,
    }
}

/// Build the `LspStatus` message from a translated lifecycle event.
fn lsp_status_message(event: &LspStatusEvent) -> DaemonMessage {
    DaemonMessage::LspStatus {
        server: event.server.clone(),
        state: event.state,
    }
}

/// Replay the full held LSP health map as one `LspStatus` message per server
/// name — the full state a freshly attached connection needs, the LSP-health
/// analogue of `diagnostics_snapshot_messages`.
fn lsp_status_snapshot_messages(
    lsp_status: &BTreeMap<String, LspServerState>,
) -> Vec<DaemonMessage> {
    lsp_status
        .iter()
        .map(|(server, state)| DaemonMessage::LspStatus {
            server: server.clone(),
            state: *state,
        })
        .collect()
}

/// Diff `old` git status against `new`, producing the incremental messages that
/// carry `old` to `new`: an `UpdateGitStatus` (entries whose status changed or
/// appeared in `changed`, paths that went clean in `cleared`) emitted only when
/// non-empty, plus a `RepoState` emitted only when the repo-level state changed.
///
/// With `old = None` this yields the full state — every entry as `changed`, no
/// `cleared`, and the `RepoState` — which is exactly what a freshly attached
/// connection needs replayed.
fn git_delta_messages(old: Option<&GitStatus>, new: &GitStatus) -> Vec<DaemonMessage> {
    let mut messages = Vec::new();
    let old_entries = old.map(|g| g.entries());

    let mut changed = Vec::new();
    for (path, status) in new.entries() {
        if old_entries.and_then(|entries| entries.get(path)) != Some(status) {
            changed.push(wire_git_entry(path, status));
        }
    }
    let mut cleared = Vec::new();
    if let Some(entries) = old_entries {
        for path in entries.keys() {
            if !new.entries().contains_key(path) {
                cleared.push(path.to_string_lossy().into_owned());
            }
        }
    }
    if !changed.is_empty() || !cleared.is_empty() {
        messages.push(DaemonMessage::UpdateGitStatus { changed, cleared });
    }

    if old.map(|g| g.repo()) != Some(new.repo()) {
        messages.push(repo_state_message(new.repo()));
    }
    messages
}

/// Build the `Diagnostics` message carrying one server's full current set for
/// one file — the wire form of an [`LspDiagnostics`] update. An empty `items`
/// clears that server's set for the file, matching LSP's full-set replace.
fn diagnostics_message(diag: &LspDiagnostics) -> DaemonMessage {
    DaemonMessage::Diagnostics {
        path: diag.path.clone(),
        server: diag.server.clone(),
        items: diag.items.clone(),
    }
}

/// Replay the full held diagnostics set as one `Diagnostics` message per
/// `(path, server)` — the full state a freshly attached connection needs, the
/// diagnostics analogue of `git_delta_messages(None, …)`. Only live sets are
/// held (an empty publish removes its key), so every replayed message is
/// non-empty.
fn diagnostics_snapshot_messages(
    diagnostics: &BTreeMap<DiagnosticKey, Vec<Diagnostic>>,
) -> Vec<DaemonMessage> {
    diagnostics
        .iter()
        .map(|(key, items)| DaemonMessage::Diagnostics {
            path: key.path.clone(),
            server: key.server.clone(),
            items: items.clone(),
        })
        .collect()
}

/// Split a worktree into chunked `WorktreeSnapshot` messages: every chunk
/// carries up to [`SNAPSHOT_CHUNK`] entries and only the last one sets
/// `final_chunk`. An empty tree still yields one (final) message so the client
/// learns the snapshot is complete.
fn snapshot_messages(root: &Path, entries: &BTreeMap<PathBuf, Entry>) -> Vec<DaemonMessage> {
    let root = root.to_string_lossy().into_owned();
    if entries.is_empty() {
        return vec![DaemonMessage::WorktreeSnapshot {
            root,
            entries: Vec::new(),
            final_chunk: true,
        }];
    }

    let mut messages = Vec::with_capacity(entries.len().div_ceil(SNAPSHOT_CHUNK));
    let mut iter = entries.iter().peekable();
    while iter.peek().is_some() {
        let chunk: Vec<WorktreeEntry> = iter
            .by_ref()
            .take(SNAPSHOT_CHUNK)
            .map(|(path, entry)| wire_entry(path, entry))
            .collect();
        let final_chunk = iter.peek().is_none();
        messages.push(DaemonMessage::WorktreeSnapshot {
            root: root.clone(),
            entries: chunk,
            final_chunk,
        });
    }
    messages
}

/// Answer a per-connection request/response message (`OpenFile` / `SaveFile`
/// / `RequestDiff`) against the watched worktree root, producing the reply
/// `DaemonMessage` for the requesting connection.
///
/// The root is the canonicalized [`Snapshot::root`] held in `State` — the same
/// root the worktree watcher uses, so a relative request path keys the same
/// space as a worktree entry, and the buffer service confines **writes** to it.
/// A relative read is confined too; an **absolute** read is the out-of-root
/// carve-out (a navigation target outside the root, opened read-only) and is
/// served by [`buffer::read_file`] — writes of an absolute path stay refused.
/// `RequestDiff` has no such carve-out: it is confined exactly like a write
/// ([`diff::compute`]). A refused `OpenFile` / `SaveFile` is logged and answered
/// with a typed [`DaemonMessage::OpenError`] / [`DaemonMessage::SaveError`]
/// carrying a [`BufferErrorReason`] (mapped from the internal
/// [`buffer::BufferError`] by [`buffer_error_reason`]), so the editor renders the
/// specific failure at once instead of waiting out its own open/save timeout. A
/// `RequestDiff` failure has no error reply in the protocol — it is logged and
/// dropped. The success/conflict outcomes map directly onto the protocol replies.
async fn request_reply(
    state: &watch::Receiver<State>,
    msg: ClientMessage,
) -> Option<DaemonMessage> {
    // The borrow is released before any `await`: the root is cloned out up front
    // (the snapshot's canonical root), then the file I/O runs unborrowed.
    let root = {
        let guard = state.borrow();
        match guard.worktree.as_ref() {
            Some(snapshot) => snapshot.root().to_path_buf(),
            // No worktree scanned yet: there is no root to confine to, so the
            // request cannot be served. Drop it.
            None => {
                warn!("buffer request before the worktree is ready, dropping");
                return None;
            }
        }
    };

    match msg {
        ClientMessage::OpenFile { path } => match buffer::read_file(&root, &path).await {
            Ok((content, mtime)) => Some(DaemonMessage::FileContent {
                path,
                content,
                mtime,
            }),
            Err(err) => {
                warn!(?path, %err, "open_file refused");
                let reason = buffer_error_reason(&err);
                Some(DaemonMessage::OpenError { path, reason })
            }
        },
        ClientMessage::SaveFile {
            path,
            content,
            base_mtime,
        } => match buffer::write_file(&root, &path, &content, base_mtime).await {
            Ok(buffer::SaveOutcome::Saved(mtime)) => {
                Some(DaemonMessage::SaveResult { path, mtime })
            }
            Ok(buffer::SaveOutcome::Conflict(disk_mtime)) => {
                Some(DaemonMessage::SaveConflict { path, disk_mtime })
            }
            Err(err) => {
                warn!(?path, %err, "save_file refused");
                let reason = buffer_error_reason(&err);
                Some(DaemonMessage::SaveError { path, reason })
            }
        },
        ClientMessage::RequestDiff { path } => match diff::compute(&root, &path).await {
            Ok(file_diff) => Some(DaemonMessage::FileDiff {
                path,
                diff: wire_diff(file_diff),
            }),
            Err(err) => {
                warn!(?path, %err, "request_diff refused");
                None
            }
        },
        // `request_reply` is only ever called with one of the messages matched
        // above; any other variant is a caller bug, handled as a no-reply
        // rather than a panic so a stray message can never take the
        // connection down.
        _ => None,
    }
}

/// Map an internal [`buffer::BufferError`] onto the wire [`BufferErrorReason`] the
/// editor renders.
///
/// `NotUtf8` and `TooLarge` map directly. A `PathEscape` is a client-side
/// impossibility (the editor only ever sends worktree-relative or out-of-root
/// navigation paths), so it collapses to the generic `Io` rather than earning a
/// distinct wire variant — the spec's decision. An `Io` failure is refined by its
/// [`std::io::ErrorKind`] into `NotFound` / `PermissionDenied`, falling back to
/// the generic `Io` for any other kind.
fn buffer_error_reason(err: &buffer::BufferError) -> BufferErrorReason {
    match err {
        buffer::BufferError::NotUtf8(_) => BufferErrorReason::NotUtf8,
        buffer::BufferError::TooLarge(_) => BufferErrorReason::TooLarge,
        buffer::BufferError::PathEscape(_) => BufferErrorReason::Io,
        buffer::BufferError::Io { source, .. } => match source.kind() {
            std::io::ErrorKind::NotFound => BufferErrorReason::NotFound,
            std::io::ErrorKind::PermissionDenied => BufferErrorReason::PermissionDenied,
            _ => BufferErrorReason::Io,
        },
    }
}

/// Convert a navigation `ClientMessage` into the internal [`NavRequest`] the LSP
/// worker consumes, attaching `reply` — the requesting connection's private
/// response channel (#482). Returns `None` for any non-navigation message; the
/// caller only ever passes the four navigation variants, so `None` is a
/// caller bug handled as a silent drop rather than a panic.
fn nav_request(msg: ClientMessage, reply: mpsc::Sender<DaemonMessage>) -> Option<NavRequest> {
    match msg {
        ClientMessage::HoverRequest { id, path, position } => Some(NavRequest::Hover {
            id,
            path,
            position,
            reply,
        }),
        ClientMessage::DefinitionRequest { id, path, position } => Some(NavRequest::Definition {
            id,
            path,
            position,
            reply,
        }),
        ClientMessage::ReferencesRequest { id, path, position } => Some(NavRequest::References {
            id,
            path,
            position,
            reply,
        }),
        ClientMessage::DocumentSymbolRequest { id, path } => {
            Some(NavRequest::DocumentSymbol { id, path, reply })
        }
        _ => None,
    }
}

/// Per-connection drop-stale bookkeeping for the navigation reply path (#482).
///
/// A connection issues hover / definition / references / document-symbol
/// requests with a client-assigned [`NavRequestId`] and receives their
/// answers on its own reply channel. A slow server can deliver an answer
/// after the user has moved on and issued a newer request of the same kind;
/// this gate records the latest id per operation on send
/// ([`record`](NavStaleGate::record)) and, on receipt, reports whether the
/// answer still matches ([`is_current`](NavStaleGate::is_current)) — a
/// superseded answer is dropped before it reaches the socket. Keeping the
/// state per connection is the fix's core: one client's newer request can no
/// longer cancel another client's in-flight answer.
#[derive(Default)]
struct NavStaleGate {
    latest_hover: Option<NavRequestId>,
    latest_definition: Option<NavRequestId>,
    latest_references: Option<NavRequestId>,
    latest_document_symbol: Option<NavRequestId>,
}

impl NavStaleGate {
    /// Record `msg` (an outbound navigation `ClientMessage`) as this connection's
    /// latest request of its kind. Non-navigation messages are ignored.
    fn record(&mut self, msg: &ClientMessage) {
        match msg {
            ClientMessage::HoverRequest { id, .. } => self.latest_hover = Some(*id),
            ClientMessage::DefinitionRequest { id, .. } => self.latest_definition = Some(*id),
            ClientMessage::ReferencesRequest { id, .. } => self.latest_references = Some(*id),
            ClientMessage::DocumentSymbolRequest { id, .. } => {
                self.latest_document_symbol = Some(*id)
            }
            _ => {}
        }
    }

    /// Report whether `msg` (a navigation response from the worker) still matches
    /// the latest request of its kind this connection issued. An answer with no
    /// matching request, or one a newer request has superseded, is stale. Only
    /// navigation responses ever reach the reply channel, so any other variant is
    /// treated as current (written through) rather than dropped.
    fn is_current(&self, msg: &DaemonMessage) -> bool {
        match msg {
            DaemonMessage::HoverResponse { id, .. } => self.latest_hover == Some(*id),
            DaemonMessage::DefinitionResponse { id, .. } => self.latest_definition == Some(*id),
            DaemonMessage::ReferencesResponse { id, .. } => self.latest_references == Some(*id),
            DaemonMessage::DocumentSymbolResponse { id, .. } => {
                self.latest_document_symbol == Some(*id)
            }
            _ => true,
        }
    }
}

/// Serve one client connection against an already-running dispatch loop.
///
/// Decodes [`ClientMessage`] frames from `reader` into the loop via `inbound`,
/// writes [`DaemonMessage`] frames from `events` to `writer`, and replays the
/// current worktree snapshot from `state` straight to this connection right
/// after the handshake. One call drives one connection; the dispatch loop and
/// the `State` it owns live outside it, so they persist across reconnects — the
/// reattach contract.
///
/// The handshake is answered per connection (issue #473): every `Hello` gets
/// `Welcome { version: PROTOCOL_VERSION }` written straight to this socket —
/// never onto the shared bus, so one client's handshake cannot reach another
/// connection's stream. Version equality is strict: a matching `Hello`
/// completes the handshake and is followed by the snapshot replay; a
/// mismatched one gets the same `Welcome` — the orderly early version signal
/// for a stale client — and then a clean close, with no state or stream
/// frames.
///
/// The snapshot is delivered per connection — backpressured by the socket —
/// rather than over the shared `events` bus, whose bounded backlog silently
/// drops chunks once a large snapshot exceeds its capacity (issue #227). The
/// bus carries only incremental `UpdateWorktree`s; a lagging writer may still
/// drop those — the loss is logged and the full snapshot is replayed off-bus
/// so the client converges rather than staying stale (issue #426). Bus events
/// are forwarded only once this connection's `Welcome` has been written
/// (issue #425) — the client requires `Welcome` as its first frame.
///
/// Navigation requests (hover/definition/references/document-symbol) are
/// answered per connection too (#482): each is forwarded to the off-loop LSP
/// worker over `nav_requests`
/// carrying this connection's own private `reply` channel, and the worker sends
/// the answer straight back on it. The answer is written to *this* socket alone,
/// never onto the shared bus — so with two clients attached (stable + dev share
/// one daemon) one client's request can neither leak into nor cancel the other's
/// navigation UI. Drop-stale is enforced per connection by [`NavStaleGate`]: a
/// slow answer the user has already superseded with a newer request of the same
/// kind is dropped before it reaches the socket.
///
/// `inbound` is a clone of the loop's inbound sender (dropped when the
/// connection ends); `events` is a fresh subscription to the outbound bus;
/// `state` observes the latest worktree snapshot; `nav_requests` is this
/// connection's clone of the LSP worker's request sender (`None` when LSP is not
/// armed — nav requests are then silently dropped). `root` is the daemon's
/// watched project root, passed into this connection's `terminal_task` so a
/// freshly created tmux session defaults to it instead of `$HOME`
/// (`docs/spec-session-start-directory.md`); `None` in the rootless test call
/// sites.
///
/// `context_map` is the shared, per-root [`ContextMap`] (#737, the Attach
/// seam): `None` opts a caller out of re-rooting entirely (every test above
/// that only exercises unrelated behavior); `Some` lets this connection
/// follow the tmux session it attaches to. On each `Attach` whose session
/// root resolves (`terminal::terminal_task`'s `RootResolved` signal, read off
/// a dedicated internal channel this function owns), `inbound`/`events`/
/// `state`/`nav_requests` above are REBOUND to the resolved root's acquired
/// context, in the defined resolve -> acquire -> release-old -> snapshot
/// order (`reroot_connection`), so the first `WorktreeSnapshot` after an
/// `Attach` always carries the new root. `root` above (the connect-time
/// default) and its context are never touched by this — they are this
/// function's caller's own reference, acquired and released outside it,
/// exactly as before #737; only a root a resolved `Attach` actually names is
/// tracked and released by this connection itself.
///
/// Returns once the reader reaches EOF, the dispatch loop is gone, or the
/// event bus closes.
#[allow(clippy::too_many_arguments)]
async fn serve_connection<R, W>(
    reader: R,
    mut writer: W,
    mut inbound: mpsc::Sender<ClientMessage>,
    mut events: broadcast::Receiver<DaemonMessage>,
    mut state: watch::Receiver<State>,
    mut nav_requests: Option<mpsc::Sender<NavRequest>>,
    tmux_server: Option<String>,
    root: Option<PathBuf>,
    context_map: Option<Arc<ContextMap>>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = reader;
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0u8; SERVE_READ_BUFFER];
    // Per-connection replay bookkeeping: the snapshot is sent once, right behind
    // the `Welcome`. `handshaken` is set by the `Hello` arm below only when the
    // versions match; it gates the `state` branch so a snapshot landing mid-scan
    // is never written ahead of the handshake the client waits for, and the
    // `events` branch so shared bus traffic at connect time can never reach the
    // socket before the `Welcome` (#425) — the client hard-fails on any
    // non-`Welcome` first frame.
    let mut handshaken = false;
    let mut snapshot_sent = false;

    // This connection's private navigation reply path (#482): the LSP worker's
    // spawned nav tasks send hover/definition/references/document-symbol
    // answers here, and only here — never onto the shared bus — so another
    // attached client can neither see nor cancel them. `nav_gate` tracks the
    // latest request id per operation for this connection so a superseded
    // answer is dropped before the socket.
    let (nav_reply_tx, mut nav_reply_rx) = mpsc::channel::<DaemonMessage>(NAV_REPLY_CAPACITY);
    let mut nav_gate = NavStaleGate::default();

    // This connection's private clone reply path (#828, `docs/spec-clone-repo.md`):
    // each `CloneRepo` is answered by a DETACHED task (`clone::run`, spawned
    // below, never awaited inline) that posts its single `CloneResult` here
    // and only here — mirroring the nav reply path above, minus the
    // staleness gate (a clone reply always answers exactly the request that
    // spawned it, no superseding). `clone_interrupts` tracks each in-flight
    // clone's cooperative interrupt flag so every one still running is
    // flipped when this connection ends, instead of running unbounded after
    // nothing is left to hear its answer.
    let (clone_reply_tx, mut clone_reply_rx) = mpsc::channel::<DaemonMessage>(CLONE_REPLY_CAPACITY);
    let mut clone_interrupts: Vec<Arc<AtomicBool>> = Vec::new();

    // This connection's own tmux attach: terminal `ClientMessage`s are routed to
    // a dedicated `terminal_task` (each client gets its own `tmux -C` child), and
    // its outbound events are multiplexed onto this socket alongside the shared
    // worktree/git stream. Keeping the terminal path per connection is what gives
    // each rift client an independent attach (per-client size, flow control).
    let (terminal_in_tx, terminal_in_rx) = mpsc::channel(TERMINAL_INBOUND_CAPACITY);
    let (terminal_out_tx, mut terminal_out_rx) = mpsc::channel(TERMINAL_OUTBOUND_CAPACITY);
    // The Attach seam (#737): `terminal_task` reports the resolved session
    // root here once per attach; the branch below drives the re-root.
    // `terminal_done` (below) guards both this and `terminal_out_rx` since
    // their senders are dropped together when the terminal task ends.
    let (root_resolved_tx, mut root_resolved_rx) = mpsc::channel(ROOT_RESOLVED_CAPACITY);
    let terminal = tokio::spawn(terminal::terminal_task(
        terminal_in_rx,
        terminal_out_tx,
        tmux_server,
        root,
        root_resolved_tx,
    ));
    let mut terminal_done = false;
    // The root this connection has itself acquired via a resolved `Attach`
    // (as opposed to `root`/its context above, which the CALLER acquired and
    // releases). `None` until the first `Attach` resolves a root.
    let mut self_root: Option<PathBuf> = None;
    // This connection's own view of which paths currently have a live buffer
    // open against `self_root`'s context (#738, "Detach open buffers on
    // re-root"): every `BufferChanged` inserts, every `BufferClosed` removes.
    // `reroot_connection` drains this and synthesizes a `BufferClosed` for
    // each remaining path on the OLD root before rebinding, so a buffer left
    // open across a project switch can never resolve a later save against
    // the wrong root.
    let mut open_buffers: HashSet<String> = HashSet::new();

    'serve: loop {
        tokio::select! {
            read = reader.read(&mut buf) => {
                let n = read?;
                if n == 0 {
                    // Reader EOF: the client disconnected. End this connection;
                    // the daemon and its state stay alive for the next attach.
                    break 'serve;
                }
                decoder.push(&buf[..n]);
                while let Some(msg) = decoder.next_frame::<ClientMessage>()? {
                    // Terminal messages drive this connection's own tmux attach;
                    // the buffer-channel requests are answered per connection
                    // (request/response, replying to this socket); the handshake
                    // goes to the shared loop.
                    match msg {
                        ClientMessage::Attach { .. }
                        | ClientMessage::Input { .. }
                        | ClientMessage::ResizePane { .. }
                        | ClientMessage::TmuxCommand { .. }
                        | ClientMessage::CapturePane { .. }
                        | ClientMessage::QueryKeyTable
                        | ClientMessage::QuerySessionList => {
                            if terminal_in_tx.send(msg).await.is_err() {
                                // Terminal task gone; the terminal path is dead,
                                // but the worktree path can keep serving.
                                terminal_done = true;
                            }
                        }
                        // The buffer and diff channels are the protocol's
                        // request/response paths: the reply goes back to *this*
                        // connection's writer, never onto the shared broadcast
                        // bus. The worktree root both confine to is the
                        // canonicalized `Snapshot::root()` held in `State` — the
                        // same root the worktree watcher uses, so a request path
                        // keys the same space as a worktree entry.
                        ClientMessage::OpenFile { .. }
                        | ClientMessage::SaveFile { .. }
                        | ClientMessage::RequestDiff { .. } => {
                            // A refused request (escape, non-UTF-8, I/O error,
                            // diff compute error) yields no reply — logged in
                            // `request_reply`.
                            if let Some(reply) = request_reply(&state, msg).await {
                                let frame = encode_frame(&reply)?;
                                writer.write_all(&frame).await?;
                                writer.flush().await?;
                            }
                        }
                        // The handshake is answered per connection (#473): the
                        // `Welcome` goes to exactly this socket, never onto the
                        // shared bus, so one client's handshake — matched or
                        // not — cannot disturb another connection's stream.
                        // Version equality is strict: on mismatch the `Welcome`
                        // carrying the daemon's OWN version is the orderly
                        // early signal a stale client can act on, and the
                        // connection closes cleanly without streaming — no
                        // state, no terminal frames, no mid-stream codec death.
                        ClientMessage::Hello { version } => {
                            let welcome = encode_frame(&DaemonMessage::Welcome {
                                version: PROTOCOL_VERSION,
                            })?;
                            writer.write_all(&welcome).await?;
                            writer.flush().await?;
                            if version != PROTOCOL_VERSION {
                                warn!(
                                    client = version,
                                    daemon = PROTOCOL_VERSION,
                                    "protocol version mismatch, closing connection"
                                );
                                break 'serve;
                            }
                            handshaken = true;
                            // Everything queued on this connection's bus
                            // subscription up to here predates the handshake:
                            // the dispatch loop mutates `State` before each
                            // broadcast, so it is all contained in the snapshot
                            // written below. Resubscribe at the bus tail so the
                            // stale backlog is dropped instead of leaking to
                            // the socket after the `Welcome` (#425).
                            events = events.resubscribe();
                            // Replay the snapshot immediately behind the
                            // handshake so the client sees `Welcome` then a
                            // complete tree. If the scan has not finished, the
                            // `state` branch delivers it once it lands.
                            if !snapshot_sent {
                                snapshot_sent = write_snapshot(&mut writer, &state).await?;
                            }
                        }
                        // The source-control write ops (#544, hunk staging #545)
                        // are per-connection request/response, exactly like the
                        // buffer/diff channels above: apply the op against the
                        // confined worktree root and write the single
                        // `GitOpResult` straight back to this socket. The
                        // resulting state change is never in the reply — it
                        // arrives through the existing push git recompute the
                        // `.git/index` watcher triggers.
                        ClientMessage::StageFile { .. }
                        | ClientMessage::UnstageFile { .. }
                        | ClientMessage::DiscardFile { .. }
                        | ClientMessage::StageHunk { .. }
                        | ClientMessage::Commit { .. } => {
                            let reply = git_write::reply(&state, msg).await;
                            let frame = encode_frame(&reply)?;
                            writer.write_all(&frame).await?;
                            writer.flush().await?;
                        }
                        // The file-operation requests (`docs/spec-explorer-file-ops.md`,
                        // #674) are per-connection request/response, exactly like
                        // the source-control write ops above: apply the op
                        // against the confined worktree root with `std::fs` and
                        // write the single `FileOpResult` straight back to this
                        // socket. The resulting tree change is never in the
                        // reply — it arrives through the existing push-only
                        // `UpdateWorktree` recompute the worktree watcher
                        // triggers, the same self-inflicted-op contract the
                        // git-write channel established.
                        ClientMessage::CreateFile { .. }
                        | ClientMessage::CreateDir { .. }
                        | ClientMessage::RenamePath { .. }
                        | ClientMessage::DeletePath { .. } => {
                            let reply = file_ops::reply(&state, msg).await;
                            let frame = encode_frame(&reply)?;
                            writer.write_all(&frame).await?;
                            writer.flush().await?;
                        }
                        // The directory-browse request
                        // (`docs/spec-session-root-picker.md`, #766) is
                        // per-connection request/response too, but — unlike the
                        // file-op/git-write ops above — it is deliberately
                        // ROOTLESS: it accepts an absolute host path and reads
                        // wherever the daemon's SSH user can, with no
                        // `buffer::resolve` confinement and no `State` borrow
                        // (the browse's purpose is to pick a *new* root).
                        ClientMessage::QueryDirEntries { .. } => {
                            let reply = browse::reply(msg).await;
                            let frame = encode_frame(&reply)?;
                            writer.write_all(&frame).await?;
                            writer.flush().await?;
                        }
                        // The clone request (`docs/spec-clone-repo.md`, #828)
                        // is a request/reply pair too, but — unlike every
                        // other arm above — it must NOT be awaited inline
                        // here: a clone is unbounded (seconds to minutes), so
                        // awaiting it in this loop would stall this
                        // connection's terminal output and every other
                        // inbound message for the clone's duration. Spawn a
                        // DETACHED task instead: dispatch returns immediately,
                        // the task runs the clone (`clone::run`, itself
                        // `spawn_blocking` internally) and posts the single
                        // `CloneResult` on `clone_reply_tx` — this
                        // connection's own private inbox, drained by the
                        // `clone_reply_rx` branch below, mirroring the nav
                        // reply path. `should_interrupt` is tracked in
                        // `clone_interrupts` so it can be flipped when this
                        // connection ends (below); `clone::run` watches it
                        // alongside the `git` child's exit and kills the
                        // child on interrupt.
                        ClientMessage::CloneRepo { .. } => {
                            let should_interrupt = Arc::new(AtomicBool::new(false));
                            clone_interrupts.push(should_interrupt.clone());
                            let reply_tx = clone_reply_tx.clone();
                            tokio::spawn(async move {
                                let reply = clone::run(msg, should_interrupt).await;
                                let _ = reply_tx.send(reply).await;
                            });
                        }
                        // The live-buffer feed goes to the shared loop: the LSP
                        // worker that consumes the buffer events lives off that
                        // single loop (one document model + servers for the
                        // daemon), not per connection. Push-only — no reply here;
                        // diagnostics return on the shared broadcast bus.
                        //
                        // Also track which paths THIS connection currently has
                        // open (#738): `reroot_connection` drains this set and
                        // synthesizes a `BufferClosed` for each remaining path
                        // on the OLD root before rebinding to a new one, so a
                        // buffer left open across a project switch is detached
                        // rather than silently carried into the new context.
                        ClientMessage::BufferChanged { ref path, .. } => {
                            open_buffers.insert(path.clone());
                            if inbound.send(msg).await.is_err() {
                                // Dispatch loop gone; nothing left to serve.
                                break 'serve;
                            }
                        }
                        ClientMessage::BufferClosed { ref path } => {
                            open_buffers.remove(path);
                            if inbound.send(msg).await.is_err() {
                                // Dispatch loop gone; nothing left to serve.
                                break 'serve;
                            }
                        }
                        // Navigation requests (hover/definition/references/
                        // document-symbol) are answered per connection (#482):
                        // forward to the off-loop LSP worker carrying this
                        // connection's private `reply` channel, and record the
                        // id as the latest of its kind so a superseded answer is
                        // drop-stale-gated below. The worker sends the answer
                        // back on the reply channel (the `nav_reply_rx` branch),
                        // which writes it to this socket alone — never onto the
                        // shared bus. When LSP is not armed (`nav_requests` is
                        // `None`) or the worker's queue is full, the request is
                        // dropped; "no answer" is a valid nav result and a later
                        // request re-drives it.
                        ClientMessage::HoverRequest { .. }
                        | ClientMessage::DefinitionRequest { .. }
                        | ClientMessage::ReferencesRequest { .. }
                        | ClientMessage::DocumentSymbolRequest { .. } => {
                            nav_gate.record(&msg);
                            if let Some(nav_requests) = &nav_requests {
                                if let Some(req) = nav_request(msg, nav_reply_tx.clone()) {
                                    if let Err(err) = nav_requests.try_send(req) {
                                        warn!(%err, "dropped navigation request");
                                    }
                                }
                            }
                        }
                    }
                }
            }
            event = events.recv() => {
                match event {
                    Ok(msg) => {
                        // Pre-handshake bus traffic (events broadcast before this
                        // connection's handshake) is dropped, never written.
                        // Nothing is lost: the dispatch loop mutates `State`
                        // before each broadcast, so everything suppressed here is
                        // already in the snapshot replayed behind the `Welcome`
                        // (which the `Hello` arm writes per connection, #473 —
                        // the bus never carries a `Welcome`).
                        if !handshaken {
                            continue;
                        }
                        let frame = encode_frame(&msg)?;
                        writer.write_all(&frame).await?;
                        writer.flush().await?;
                    }
                    // The bus carries only incremental updates now; a lagging
                    // writer drops some. Log the count — never silently — then
                    // replay the full snapshot (tree + git + diagnostics)
                    // off-bus so this client converges instead of staying
                    // permanently stale (#426): a completed snapshot atomically
                    // replaces the client model, and the retained bus tail
                    // written after it re-applies idempotently. Pre-handshake
                    // there is nothing to resync — bus traffic is suppressed
                    // above and the snapshot follows the `Welcome` anyway.
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "connection lagged, resyncing via snapshot replay");
                        if handshaken {
                            snapshot_sent = write_snapshot(&mut writer, &state).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break 'serve,
                }
            }
            changed = state.changed() => {
                if changed.is_err() {
                    // The dispatch loop dropped the `State` sender; it is gone.
                    break 'serve;
                }
                if handshaken && !snapshot_sent {
                    snapshot_sent = write_snapshot(&mut writer, &state).await?;
                }
            }
            terminal_event = terminal_out_rx.recv(), if !terminal_done => {
                match terminal_event {
                    // This connection's tmux attach produced an event (pane bytes,
                    // a layout change, or terminal-path-down): write it to the
                    // socket alongside the shared worktree/git stream.
                    Some(msg) => {
                        let frame = encode_frame(&msg)?;
                        writer.write_all(&frame).await?;
                        writer.flush().await?;
                    }
                    // The terminal task ended; stop polling its channel so this
                    // branch cannot busy-loop. The worktree path keeps serving.
                    None => terminal_done = true,
                }
            }
            // The Attach seam (#737): the terminal task resolved the attached
            // session's root. Re-root this connection's reactive bindings to
            // it, then replay a fresh snapshot so the FIRST one after Attach
            // always carries the new root (guarded the same way and for the
            // same reason as `terminal_out_rx` above — both channels' senders
            // close together when the terminal task ends).
            resolved = root_resolved_rx.recv(), if !terminal_done => {
                match resolved {
                    Some(Some(resolved_root)) => {
                        if let Some(map) = context_map.as_ref() {
                            let rerooted = reroot_connection(
                                map,
                                resolved_root,
                                &mut self_root,
                                &mut inbound,
                                &mut events,
                                &mut state,
                                &mut nav_requests,
                                &mut open_buffers,
                            )
                            .await;
                            // Same root as already active (a same-session
                            // reconnect, or a redundant Attach): nothing
                            // changed, so no re-snapshot either.
                            if rerooted && handshaken {
                                snapshot_sent = write_snapshot(&mut writer, &state).await?;
                            }
                        }
                    }
                    // Resolution yielded no root (an empty `session_path` too
                    // — should not happen for a live session); nothing to
                    // re-root to, so the current context stays as is.
                    Some(None) => {}
                    None => terminal_done = true,
                }
            }
            // This connection's private navigation answers (#482), off the shared
            // bus: the LSP worker's nav task sends hover/definition/references/
            // document-symbol results here. Drop a superseded answer (the user
            // issued a newer request of the same kind since) and, once
            // handshaken, write the rest to this socket alone. `nav_reply_tx` is
            // held by this task for the connection's lifetime, so this branch
            // never yields `None` — no guard is needed and it cannot busy-loop.
            nav_reply = nav_reply_rx.recv() => {
                if let Some(msg) = nav_reply {
                    if handshaken && nav_gate.is_current(&msg) {
                        let frame = encode_frame(&msg)?;
                        writer.write_all(&frame).await?;
                        writer.flush().await?;
                    }
                }
            }
            // This connection's private clone answers (#828), off the shared
            // bus: each detached `clone::run` task (spawned by the
            // `CloneRepo` arm above) sends its single `CloneResult` here.
            // Unlike nav, no staleness gate — a clone reply always answers
            // exactly the request that spawned it. `clone_reply_tx` is held
            // by this task for the connection's lifetime (and by every
            // still-running clone task), so this branch never yields `None`
            // — no guard is needed and it cannot busy-loop.
            clone_reply = clone_reply_rx.recv() => {
                if let Some(msg) = clone_reply {
                    if handshaken {
                        let frame = encode_frame(&msg)?;
                        writer.write_all(&frame).await?;
                        writer.flush().await?;
                    }
                }
            }
        }
    }

    // Interrupt every clone this connection started that is still running
    // (#828/#841, `docs/spec-clone-repo.md`): nothing is left to hear its
    // `CloneResult` once this connection is gone, so flipping the flag here
    // makes `clone::run`'s `select!` kill the `git` child instead of letting
    // it run unbounded. A clone that already finished holds no other
    // reference to its flag, so this is a no-op for it.
    for should_interrupt in &clone_interrupts {
        should_interrupt.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    // End this connection's tmux attach. The task then detaches the control
    // child (the tmux session persists) and exits.
    shutdown_terminal(terminal_in_tx, terminal_out_rx, terminal).await;

    // Release the root THIS connection acquired for itself via a resolved
    // `Attach` (#737), if any — `root`/its context above is the caller's own
    // reference, untouched here. Drop every clone of it first (the same
    // "drop before the final release" discipline `reroot_connection` and
    // `ContextMap::release` document), so a self-root that happens to be the
    // last reference tears down cleanly instead of hanging on its own
    // still-held `inbound`.
    if let (Some(map), Some(root)) = (context_map, self_root) {
        drop(inbound);
        drop(events);
        drop(state);
        drop(nav_requests);
        map.release(&root).await;
    }

    Ok(())
}

/// Tear down a connection's terminal task: drop BOTH channel ends, then join.
///
/// Dropping `in_tx` wakes a task parked at `inbound.recv()`; dropping `out_rx`
/// wakes one parked in `process()` on a full OUTBOUND send (a flooding pane
/// while the connection stopped draining the channel). Both drops must happen
/// BEFORE the await — a task parked on a full send is never woken by dropping
/// the sender alone, so awaiting first would hang the connection forever.
async fn shutdown_terminal(
    in_tx: mpsc::Sender<ClientMessage>,
    out_rx: mpsc::Receiver<DaemonMessage>,
    handle: tokio::task::JoinHandle<()>,
) {
    drop(in_tx);
    drop(out_rx);
    let _ = handle.await;
}

/// Replay the current worktree snapshot to one connection, backpressured by the
/// socket so no chunk is dropped regardless of tree size. Returns `true` when a
/// snapshot was written, `false` when none is ready yet. The `watch` borrow is
/// released before any `await`: the chunked messages are built up front, then
/// written.
async fn write_snapshot<W: AsyncWrite + Unpin>(
    writer: &mut W,
    state: &watch::Receiver<State>,
) -> anyhow::Result<bool> {
    let messages = {
        let guard = state.borrow();
        let Some(snapshot) = guard.worktree.as_ref() else {
            return Ok(false);
        };
        let mut messages = snapshot_messages(snapshot.root(), snapshot.entries());
        // Replay the full git status (if any) right behind the tree, so a
        // (re)attaching client gets the complete git decoration loss-free —
        // `git_delta_messages(None, …)` is the full set. Incremental updates
        // then ride the bus.
        if let Some(git) = guard.git.as_ref() {
            messages.extend(git_delta_messages(None, git));
        }
        // Replay the held diagnostics too, so a (re)attaching client receives
        // the full live error set — one `Diagnostics` message per
        // `(path, server)` — alongside the tree and git decoration. Incremental
        // updates then ride the bus.
        messages.extend(diagnostics_snapshot_messages(&guard.diagnostics));
        // Replay the held LSP health map (issue #520) too, so a (re)attaching
        // client sees current server health without waiting for the next
        // transition. Incremental updates then ride the bus.
        messages.extend(lsp_status_snapshot_messages(&guard.lsp_status));
        messages
    };
    write_messages(writer, &messages).await?;
    Ok(true)
}

/// Write a sequence of `DaemonMessage` frames to a connection, flushing once at
/// the end. Each `write_all` is backpressured by the transport, so the whole
/// sequence arrives intact however many frames it is — the property the
/// per-connection snapshot relies on.
async fn write_messages<W: AsyncWrite + Unpin>(
    writer: &mut W,
    messages: &[DaemonMessage],
) -> anyhow::Result<()> {
    for msg in messages {
        let frame = encode_frame(msg)?;
        writer.write_all(&frame).await?;
    }
    writer.flush().await?;
    Ok(())
}

/// One root's live reactive context: its `State`, worktree/git worker, LSP
/// worker, and update bus — wired exactly like [`serve`]/[`serve_uds`]'s
/// single-root setup always has been: [`channels`] plus
/// [`Daemon::watch_worktree`] and [`Daemon::watch_lsp`], run on its own
/// dispatch-loop task. Acquired and released through [`ContextMap`] rather
/// than constructed directly.
#[derive(Clone)]
pub struct Context {
    /// Handles into this context's dispatch loop.
    pub handles: Handles,
    /// This context's LSP nav-request sender (#482), cloned per acquirer.
    /// `None` when no root is armed (see [`standalone_context`]).
    pub nav_requests: Option<mpsc::Sender<NavRequest>>,
}

/// A bare dispatch loop over [`channels`] with no worktree/git watcher and no
/// LSP worker — `State` stays at its `Default` forever. Mirrors what
/// [`serve`]/[`serve_uds`] have always done when no `--root` is given; kept
/// outside [`ContextMap`] because there is no root to key it by.
fn standalone_context() -> Context {
    let (daemon, handles) = channels(SERVE_EVENT_CAPACITY, SERVE_INBOUND_CAPACITY);
    let nav_requests = daemon.nav_request_sender();
    tokio::spawn(daemon.run());
    Context {
        handles,
        nav_requests,
    }
}

/// A root's registry bookkeeping: its [`Context`], how many acquirers
/// currently hold it, and the dispatch loop's task handle — joined by
/// [`ContextMap::release`] on the last release.
struct ContextEntry {
    context: Context,
    refcount: usize,
    task: tokio::task::JoinHandle<()>,
}

/// Per-root, reference-counted registry of reactive [`Context`]s
/// (`docs/spec-per-session-project-root.md`, "Per-root, reference-counted
/// context map"). Replaces the daemon's old single process-global `State` +
/// worker set: [`ContextMap::acquire`] creates a root's context on first
/// acquire — a fresh dispatch loop, worktree/git worker, and LSP worker,
/// exactly [`serve`]/[`serve_uds`]'s single-root wiring — and shares the SAME
/// context with every later acquirer of that root, so two connections on one
/// root never run a second language server (the decisive RAM constraint; see
/// the spec's "Prior decisions"). [`ContextMap::release`] drops the ref; the
/// last release removes the entry, dropping the registry's own `Context`
/// clone — closing the dispatch loop's inbound channel and cascading into
/// [`lsp::LspWorker::run`]'s own clean server shutdown.
///
/// Keyed by the raw `PathBuf` an acquirer passes in — no canonicalization at
/// this layer, matching [`Daemon::watch_worktree`]'s existing raw-root
/// contract; a caller that wants two spellings of one directory (a
/// resolved-but-uncanonical `@root`/`session_path`, a trailing slash, a
/// relative path) to share a context normalizes it BEFORE calling `acquire`
/// (`reroot_connection` does this for the Attach seam, #737).
///
/// Guarded by a [`tokio::sync::Mutex`] — deliberately, not a
/// [`std::sync::Mutex`] — because of a concurrency gap #736's review
/// surfaced and #737 closes: the old `std::sync::Mutex` version dropped the
/// lock BEFORE awaiting the last release's dispatch-loop join, so a
/// concurrent `acquire` for the SAME root landing in that window built a
/// SECOND live context (a second rust-analyzer) while the first was still
/// tearing down — exactly the RAM duplication this registry exists to
/// prevent. `release`'s last-reference path now tears its context down
/// (drops it, then awaits the join) WHILE STILL HOLDING this lock, so any
/// concurrent `acquire`/`release` — for this root or any other, since the
/// lock is registry-wide, not per-root — blocks until that teardown fully
/// completes; by the time it can look at the map, the old entry is gone AND
/// its dispatch loop has already exited, so a re-acquire always builds a
/// fresh context, never a duplicate. This trades finer-grained per-root
/// locking for a much simpler, provably-correct scheme; at the connection
/// counts this daemon serves (a handful, per `docs/spec-dogfooding-channels.md`)
/// briefly serializing an unrelated root's acquire behind another root's
/// teardown is not a real cost.
#[derive(Default)]
pub struct ContextMap {
    entries: Mutex<HashMap<PathBuf, ContextEntry>>,
}

impl ContextMap {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire `root`'s context: create it (scan + watchers + LSP) on first
    /// acquire, or bump the refcount and hand back a clone of the already-live
    /// context on a later acquire for the same root. Never fails: a bad or
    /// missing root degrades to a worktree-only or empty context and logs, via
    /// the same [`worktree_worker`] degradation path a directly-armed root
    /// already goes through. `async` (unlike #736's original sync version) so
    /// this can wait out a concurrent last-release's in-flight teardown
    /// rather than race it — see the struct doc comment.
    pub async fn acquire(&self, root: PathBuf) -> Context {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get_mut(&root) {
            entry.refcount += 1;
            return entry.context.clone();
        }

        let (mut daemon, handles) = channels(SERVE_EVENT_CAPACITY, SERVE_INBOUND_CAPACITY);
        daemon.watch_worktree(root.clone());
        daemon.watch_lsp(root.clone(), DocumentSelector::builtin());
        let nav_requests = daemon.nav_request_sender();
        let task = tokio::spawn(daemon.run());
        let context = Context {
            handles,
            nav_requests,
        };
        entries.insert(
            root,
            ContextEntry {
                context: context.clone(),
                refcount: 1,
                task,
            },
        );
        context
    }

    /// Release one reference to `root`'s context. A root with no live entry
    /// (already fully released, or never acquired) is a no-op. On the last
    /// release the entry is removed from the map and torn down — its
    /// registry-owned `Context` clone dropped, then its dispatch loop's task
    /// joined — WITHOUT releasing the registry lock in between (the
    /// atomicity fix; see the struct doc comment), so a caller that just
    /// dropped its own last handle can rely on the context being fully torn
    /// down, with no concurrent duplicate possible, once this returns. A
    /// dispatch task that panicked is logged, not propagated — an internal
    /// teardown detail degrades rather than failing the releasing caller.
    ///
    /// The caller MUST have already dropped every clone of this root's
    /// `Context`/handles it was holding before calling this on what it knows
    /// to be the last reference: the dispatch loop only ends once ALL
    /// `inbound` sender clones — not just the registry's own — have dropped,
    /// so a lingering one held past this call hangs the join forever
    /// (`reroot_connection` follows this discipline for the Attach seam,
    /// #737).
    pub async fn release(&self, root: &Path) {
        let mut entries = self.entries.lock().await;
        let Some(entry) = entries.get_mut(root) else {
            return;
        };
        entry.refcount = entry.refcount.saturating_sub(1);
        if entry.refcount > 0 {
            return;
        }
        let Some(entry) = entries.remove(root) else {
            return;
        };
        // Still holding `entries`: the atomicity fix. Drop the registry's own
        // `Context` clone (closing its `inbound` sender), then await the
        // dispatch loop's join, all before releasing the lock. Bounded (N1,
        // #737 review): see `CONTEXT_RELEASE_JOIN_TIMEOUT`.
        drop(entry.context);
        match tokio::time::timeout(CONTEXT_RELEASE_JOIN_TIMEOUT, entry.task).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                warn!(root = %root.display(), %err, "context dispatch loop ended with an error");
            }
            Err(_) => {
                warn!(
                    root = %root.display(),
                    "context dispatch loop did not exit within the teardown timeout; \
                     leaking it rather than wedging the registry"
                );
            }
        }
    }
}

/// Canonicalize a filesystem path before it keys the [`ContextMap`] registry
/// (#737, closing a gap the #736 review AND the #737 review both surfaced):
/// EVERY acquire/release call that keys the map from a resolved or configured
/// root — `reroot_connection`, `serve`'s and `serve_uds`'s keep-warm
/// acquire (and `serve`'s matching release) — MUST go through this one
/// helper, so two spellings of one directory (a raw `--root`/`@root`, a
/// trailing slash, a relative path, a symlink) can never key two DIFFERENT
/// entries. Two entries for one project means two independent `LspWorker`s —
/// two rust-analyzers for one project — the exact RAM-duplication failure
/// [`ContextMap`] exists to prevent (`docs/spec-per-session-project-root.md`,
/// "Prior decisions", option (b) rejected for precisely this reason).
///
/// [`ContextMap`] itself stays keyed by the raw path an acquirer passes in
/// (its own contract, #736) — normalizing before every `acquire`/`release`
/// call is entirely this helper's (and its callers') responsibility.
/// Canonicalize failure (a since-removed directory, a permissions error)
/// degrades to the raw path and logs, never aborts — `ContextMap::acquire`
/// already degrades a bad root to an empty/worktree-only context on its own.
///
/// A `@root` tmux STAMP (`terminal::stamp_root_command`) and the root handed
/// to `terminal_task` for a freshly created session's default directory stay
/// RAW — only the map KEY goes through this helper; a later `Attach`
/// resolving that same stamped value canonicalizes it again on read
/// (`reroot_connection`), so the two always converge on one key regardless.
async fn canonicalize_root(root: PathBuf) -> PathBuf {
    match tokio::fs::canonicalize(&root).await {
        Ok(canonical) => canonical,
        Err(err) => {
            warn!(
                root = %root.display(),
                %err,
                "failed to canonicalize root; keying the context map by the raw path"
            );
            root
        }
    }
}

/// Detach a connection's live-buffer feed (#738,
/// `docs/spec-per-session-project-root.md`, "Detach open buffers on
/// re-root"): send a synthetic [`ClientMessage::BufferClosed`] over
/// `inbound` for every path in `open_buffers`, then clear the set.
///
/// Reuses the EXISTING `BufferClosed` -> [`BufferEvent::Closed`] machinery
/// (`Core::dispatch`/`forward_buffer_event`) rather than inventing a new
/// event: from the dispatch loop's point of view this is indistinguishable
/// from the editor itself closing the tab, so the per-root document model
/// reverts the path to its disk-backed baseline. [`reroot_connection`] calls
/// this against the OLD root's `inbound` BEFORE rebinding to the new root's
/// context, so a buffer left open across a project switch is closed on the
/// root being left behind — never silently carried into the new one, and
/// never able to resolve a later save against the wrong root (the existing
/// `mtime`-conflict check on [`buffer::write_file`] is the backstop against
/// a save that DOES land, e.g. a client that has not yet processed the
/// switch).
///
/// Best-effort and non-blocking (`try_send`), matching
/// `forward_buffer_event`'s discipline: a full or already-gone inbound just
/// drops the synthetic close and logs — this is safety-net cleanup on a
/// context this connection is leaving, never a request worth stalling the
/// re-root over.
async fn detach_open_buffers(
    inbound: &mpsc::Sender<ClientMessage>,
    open_buffers: &mut HashSet<String>,
) {
    for path in open_buffers.drain() {
        if let Err(err) = inbound.try_send(ClientMessage::BufferClosed { path }) {
            warn!(%err, "dropped synthetic buffer-close on re-root");
        }
    }
}

/// Re-root one connection's reactive bindings to `resolved_root`'s per-root
/// [`Context`] — the Attach seam (#737, `docs/spec-per-session-project-root.md`).
/// Called from `serve_connection`'s dispatch loop once `terminal::terminal_task`
/// resolves an attached session's root: resolve (already done by the caller) ->
/// acquire -> release-old, in that defined order, so `self_root`/`inbound`/
/// `events`/`state`/`nav_requests` always land on a fully live context before
/// this returns; the caller streams the fresh snapshot immediately after.
///
/// `resolved_root` is canonicalized first, via [`canonicalize_root`], so two
/// spellings of one directory (a raw `@root`/`session_path`, a trailing
/// slash, a relative path, a symlink) key the SAME context rather than a
/// second one — see that function's doc comment.
///
/// The new context is acquired BEFORE the previous one (if any) is released,
/// so re-attaching the SAME root (a plain reconnect) never drops its refcount
/// to zero and pays a teardown + rebuild for a root nothing else stopped
/// referencing. If the canonical root is UNCHANGED from `self_root`, this
/// returns `false` immediately without touching the map at all — a
/// same-session reconnect or a redundant `Attach` is then a no-op, not a
/// spurious acquire(+1)/release(-1) churn; the caller uses the return value
/// to skip the redundant re-snapshot too. Otherwise `inbound`/`events`/
/// `state`/`nav_requests` are fully replaced (and the new `Context` handle
/// dropped) BEFORE the old root is released, satisfying
/// [`ContextMap::release`]'s "caller must have already dropped its own
/// clones" contract — the exact ordering #736's review flagged as otherwise
/// hanging the last release's dispatch-join — and this returns `true`.
///
/// On an ACTUAL re-root (never on the same-root no-op above), also detaches
/// this connection's live-buffer feed via [`detach_open_buffers`] — #738's
/// cross-project write-safety rule — against the OLD `inbound`, before it is
/// rebound to the new root's context.
#[allow(clippy::too_many_arguments)]
async fn reroot_connection(
    context_map: &ContextMap,
    resolved_root: PathBuf,
    self_root: &mut Option<PathBuf>,
    inbound: &mut mpsc::Sender<ClientMessage>,
    events: &mut broadcast::Receiver<DaemonMessage>,
    state: &mut watch::Receiver<State>,
    nav_requests: &mut Option<mpsc::Sender<NavRequest>>,
    open_buffers: &mut HashSet<String>,
) -> bool {
    let canonical = canonicalize_root(resolved_root).await;
    if self_root.as_ref() == Some(&canonical) {
        // Already on this root (a same-session reconnect, or a redundant
        // Attach): nothing to acquire, release, or re-snapshot — and no
        // buffers to detach either, since the root has not actually changed.
        return false;
    }

    detach_open_buffers(inbound, open_buffers).await;

    let context = context_map.acquire(canonical.clone()).await;
    *events = context.handles.subscribe();
    *inbound = context.handles.inbound.clone();
    *state = context.handles.state.clone();
    *nav_requests = context.nav_requests.clone();
    drop(context);

    if let Some(previous) = self_root.replace(canonical) {
        context_map.release(&previous).await;
    }
    true
}

/// Run a daemon over a single byte-stream transport until either side closes.
///
/// Spins up a dispatch loop, serves exactly one connection over `reader`/
/// `writer`, then tears the loop down. With a `worktree_root`, the root is
/// scanned and watched for the daemon's lifetime (see
/// [`Daemon::watch_worktree`]) and handed to the connection's terminal attach
/// too (`docs/spec-session-start-directory.md`), so a freshly created tmux
/// session's default directory is the project root, not `$HOME`. Used by the
/// daemon binary's stdio mode and the duplex round-trip test. For a long-lived,
/// reattachable daemon that survives client disconnects, see [`serve_uds`].
///
/// Returns once the reader reaches EOF or the writer/event bus closes.
pub async fn serve<R, W>(reader: R, writer: W, worktree_root: Option<PathBuf>) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // A local, single-use registry: `serve` drives exactly one connection.
    // Shared with `serve_connection` (#737) even though it drives only that
    // one connection, because that connection's OWN `Attach` can still
    // resolve a DIFFERENT session root mid-connection (a reconnect that picks
    // another session) and re-root against this same registry.
    let context_map = Arc::new(ContextMap::new());
    match worktree_root {
        Some(root) => {
            // Acquires the one root's context up front, exactly like the
            // pre-context-map code did, and releases it below once that
            // connection ends. Keyed by the CANONICAL root (#737 review,
            // B1): a later `Attach` re-rooting to this SAME directory keys
            // through `canonicalize_root` too (`reroot_connection`), so an
            // uncanonical `--root` (a trailing slash, a symlinked path) must
            // key identically here or the two acquire TWO live contexts —
            // two `LspWorker`s, two rust-analyzers — for one project. `root`
            // itself stays RAW below: it is handed to `serve_connection`'s
            // terminal attach unchanged (the `@root` stamp / session
            // start-directory, which a later resolve-on-attach canonicalizes
            // again on read, converging on the same key regardless).
            let canonical_root = canonicalize_root(root.clone()).await;
            let context = context_map.acquire(canonical_root.clone()).await;
            let events = context.handles.subscribe();
            let inbound = context.handles.inbound.clone();
            let state = context.handles.state.clone();
            let nav_requests = context.nav_requests.clone();
            // Drop this function's own handle: the clone passed into
            // `serve_connection` below is then the only inbound sender left
            // outstanding besides the registry's own — `release` below drops
            // that one too, so the dispatch loop ends exactly when this
            // connection does, unchanged from the pre-context-map behavior.
            drop(context);

            // The `None`: production uses the default tmux server for attaches.
            let result = serve_connection(
                reader,
                writer,
                inbound,
                events,
                state,
                nav_requests,
                None,
                Some(root),
                Some(context_map.clone()),
            )
            .await;
            // `serve_connection` dropped its `inbound` clone on return, making
            // this the context's last reference; `release` awaits the
            // dispatch loop's own join before this function returns —
            // matching the pre-context-map `dispatch.await?`. Same canonical
            // key as the acquire above, or this would release a DIFFERENT
            // (nonexistent) entry and leak the real one.
            context_map.release(&canonical_root).await;
            result
        }
        None => {
            let (daemon, handles) = channels(SERVE_EVENT_CAPACITY, SERVE_INBOUND_CAPACITY);
            let events = handles.subscribe();
            let inbound = handles.inbound.clone();
            let state = handles.state.clone();
            let nav_requests = daemon.nav_request_sender();
            // Drop the spare handles so the dispatch loop ends once the
            // connection's `inbound` clone is dropped at EOF.
            drop(handles);

            let dispatch = tokio::spawn(daemon.run());
            let result = serve_connection(
                reader,
                writer,
                inbound,
                events,
                state,
                nav_requests,
                None,
                None,
                Some(context_map.clone()),
            )
            .await;
            // `serve_connection` dropped its `inbound` clone on return, so the
            // dispatch loop has observed channel closure and will join.
            dispatch.await?;
            result
        }
    }
}

/// Derive the pidfile path for a daemon socket: the socket path with a `.pid`
/// suffix appended (`/run/rift.sock` -> `/run/rift.sock.pid`).
///
/// The suffix is appended, not substituted — `Path::with_extension` would turn
/// `rift.sock` into `rift.pid` and collide across sockets that differ only by
/// extension. Appending keeps the pidfile uniquely paired with its socket so the
/// app can stop the running daemon by PID when redeploying a changed binary (spec
/// `docs/spec-daemon-redeploy.md`, Family A restart).
fn pidfile_path(socket_path: &Path) -> PathBuf {
    let mut raw = socket_path.as_os_str().to_owned();
    raw.push(".pid");
    PathBuf::from(raw)
}

/// Removes the daemon's pidfile when the serve loop ends.
///
/// Best-effort: a failed unlink is logged, never propagated — the daemon's exit
/// must not hinge on cleanup, and a leftover pidfile is harmless (the next start
/// overwrites it). A kill by signal bypasses `Drop`; the stale pidfile is then
/// reclaimed on the next start, so cleanup here covers only the clean-return path.
struct PidfileGuard {
    path: PathBuf,
}

impl Drop for PidfileGuard {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            warn!(path = %self.path.display(), err = %e, "failed to remove pidfile");
        }
    }
}

/// Run a long-lived daemon that listens on a Unix-domain socket and survives
/// client disconnects — the reattach contract behind issue #62.
///
/// Binds `socket_path`, then accepts connections in a loop, handing each to
/// [`serve_connection`] against a single shared dispatch loop. Because the loop
/// and its `State` outlive any one connection, killing the SSH transport ends
/// only that connection; a reconnect attaches to the same running daemon rather
/// than spawning a second one.
///
/// With a `worktree_root`, the root's [`Context`] is acquired up front (see
/// [`ContextMap::acquire`]) and handed to [`keep_warm_supervisor`], which owns
/// it for the daemon's lifetime: scanned/watched while at least one client is
/// connected, released [`KEEP_WARM_RELEASE_GRACE`] after the last one
/// disconnects, and re-acquired (fresh scan + LSP spawn) on the next
/// connection (#551, releasing the memory an indefinitely-orphaned language
/// server would otherwise hold). Every attaching client receives the current
/// snapshot on its handshake, and each accepted connection's terminal attach
/// gets a clone of the root too (`docs/spec-session-start-directory.md`), so a
/// freshly created tmux session's default directory is the project root, not
/// `$HOME`. Per-connection re-rooting on `Attach` (#737) is independent of
/// this: it tracks its own root reference and releases it on that
/// connection's own disconnect, untouched by the keep-warm grace timer.
///
/// If a live daemon already owns `socket_path` this returns an error — the
/// caller must attach via [`connect_relay`], not spawn. A stale socket left by a
/// crashed daemon is removed and rebound. Transient per-accept errors are logged
/// and retried (the daemon must not die on FD pressure and leave nothing to
/// reattach to); the function returns only on a bind failure or process signal.
///
/// Once bound, the daemon writes its PID to `<socket_path>.pid` so the app can
/// stop it by PID when redeploying a changed binary (spec
/// `docs/spec-daemon-redeploy.md`, Family A restart). The pidfile is best-effort:
/// a write failure is logged and serving continues; it is removed on clean exit.
pub async fn serve_uds(socket_path: &Path, worktree_root: Option<PathBuf>) -> anyhow::Result<()> {
    if socket_path.exists() {
        // Distinguish a live daemon from a stale socket: a successful connect
        // means another instance owns this path, so refuse to bind a second.
        match UnixStream::connect(socket_path).await {
            Ok(_) => {
                anyhow::bail!("daemon already listening on {}", socket_path.display());
            }
            // Connect refused/failed: the socket is stale. Remove it and rebind.
            Err(_) => {
                let _ = tokio::fs::remove_file(socket_path).await;
            }
        }
    }

    let listener = UnixListener::bind(socket_path)?;

    // Write the pidfile only after a successful bind, so a refused second start
    // never overwrites the live daemon's pidfile. Best-effort: log and carry on
    // if the write fails. The guard removes it when this function returns.
    let pidfile = pidfile_path(socket_path);
    let _pidfile_guard = match tokio::fs::write(&pidfile, std::process::id().to_string()).await {
        Ok(()) => Some(PidfileGuard { path: pidfile }),
        Err(e) => {
            warn!(path = %pidfile.display(), err = %e, "failed to write pidfile");
            None
        }
    };

    // `context_map` is shared with every accepted connection (#737): each
    // one's own `Attach` can resolve a DIFFERENT session root and re-root
    // against the SAME registry, without disturbing the primary reference
    // below.
    let context_map = Arc::new(ContextMap::new());
    let connection_root = worktree_root.clone();
    let primary = match worktree_root {
        // Canonicalized for the SAME reason as `serve`'s keep-warm acquire
        // above (#737 review, B1): a later `Attach` re-rooting to this same
        // directory keys through `canonicalize_root` too
        // (`reroot_connection`), so an uncanonical `--root` must key
        // identically here or the two build TWO live contexts — two
        // `LspWorker`s — for one project. `connection_root` below (handed to
        // each connection's terminal attach) stays RAW.
        Some(root) => {
            let canonical_root = canonicalize_root(root).await;
            let context = context_map.acquire(canonical_root.clone()).await;
            let (keep_warm_tx, keep_warm_rx) = mpsc::channel(KEEP_WARM_EVENT_CAPACITY);
            tokio::spawn(keep_warm_supervisor(
                Arc::clone(&context_map),
                canonical_root,
                context,
                KEEP_WARM_RELEASE_GRACE,
                keep_warm_rx,
            ));
            PrimaryContext::KeepWarm(keep_warm_tx)
        }
        // Rootless daemons keep the pre-#551 behavior unchanged: one
        // standalone context (no `ContextMap` entry, no LSP), held for the
        // process's whole lifetime — nothing to release.
        None => PrimaryContext::Standalone(standalone_context()),
    };

    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                // A long-lived daemon must not die on a transient accept error
                // (ECONNABORTED, or EMFILE/ENFILE under FD pressure) — that would
                // leave nothing to reattach to. Log, back off briefly so a
                // persistent failure cannot hot-spin, and keep accepting.
                warn!(err = %e, "accept error");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };

        // Resolve this connection's context. `KeepWarm` asks the supervisor
        // (arming/disarming the release grace timer, re-acquiring first if
        // the previous last client's grace already released it); `Standalone`
        // just clones the one process-lifetime context.
        let (context, keep_warm_tx) = match &primary {
            PrimaryContext::KeepWarm(keep_warm_tx) => {
                let (reply_tx, mut reply_rx) = mpsc::channel(1);
                if keep_warm_tx
                    .send(KeepWarmEvent::Connected(reply_tx))
                    .await
                    .is_err()
                {
                    warn!("keep-warm supervisor gone; dropping connection");
                    continue;
                }
                let Some(context) = reply_rx.recv().await else {
                    warn!("keep-warm supervisor dropped without a reply; dropping connection");
                    continue;
                };
                (context, Some(keep_warm_tx.clone()))
            }
            PrimaryContext::Standalone(context) => (context.clone(), None),
        };

        let (reader, writer) = stream.into_split();
        let inbound = context.handles.inbound.clone();
        let events = context.handles.subscribe();
        let state = context.handles.state.clone();
        let nav_requests = context.nav_requests.clone();
        let root = connection_root.clone();
        let context_map = Arc::clone(&context_map);
        tokio::spawn(async move {
            if let Err(e) = serve_connection(
                reader,
                writer,
                inbound,
                events,
                state,
                nav_requests,
                None,
                root,
                Some(context_map),
            )
            .await
            {
                // The active sink (stderr or the rotated file) is the daemon's
                // log; one failed connection must not stop the daemon.
                warn!(err = %e, "connection ended with error");
            }
            // Tell the supervisor this connection is gone, so it can notice
            // the last client leaving and arm the release grace timer (#551).
            // `serve_connection` above has already returned by this point, so
            // every clone of `context` it owned is already dropped — the
            // "drop before the disconnect signal" discipline
            // `ContextMap::release` and `reroot_connection` document.
            if let Some(keep_warm_tx) = keep_warm_tx {
                let _ = keep_warm_tx.send(KeepWarmEvent::Disconnected).await;
            }
        });
    }
}

/// [`serve_uds`]'s primary/`--root` context: either routed through
/// [`keep_warm_supervisor`] (a rooted daemon, releasable and re-acquirable,
/// #551) or a single process-lifetime standalone context (a rootless daemon,
/// unchanged pre-#551 behavior — no root to key a [`ContextMap`] entry by).
enum PrimaryContext {
    KeepWarm(mpsc::Sender<KeepWarmEvent>),
    Standalone(Context),
}

/// One event [`serve_uds`]'s accept loop reports to [`keep_warm_supervisor`]
/// about the primary/`--root` context's connection lifecycle (#551).
enum KeepWarmEvent {
    /// A new connection needs the current context. The supervisor sends
    /// exactly one [`Context`] back on `reply` — re-acquiring first (fresh
    /// scan + LSP spawn) if the previous last client's grace already
    /// released it — before dropping it.
    Connected(mpsc::Sender<Context>),
    /// A connection this supervisor previously handed a `Context` to (via a
    /// `Connected` reply) has ended.
    Disconnected,
}

/// Own the primary/`--root` [`Context`] for [`serve_uds`]'s whole process
/// lifetime: hand it out on every `Connected` event (re-acquiring via
/// [`ContextMap::acquire`] first if it was released), and release it (see
/// [`ContextMap::release`]) `grace` after the connection count implied by
/// `Connected`/`Disconnected` events drops to zero (#551) — unless a
/// `Connected` arrives first, which cancels the pending release.
///
/// Runs as its own task, off `serve_uds`'s accept loop: this single-tasked
/// event loop serializes every `Connected`/`Disconnected` it receives, so a
/// `Connected` racing an in-flight re-acquire simply queues behind it and
/// observes the same freshly built context — never a duplicate. The accept
/// loop awaits each connection's `Connected` reply before accepting the next,
/// so a re-acquire's worktree scan briefly delays only the connection that
/// triggered it — the same one-time cost the original up-front acquire always
/// paid at daemon startup, just possibly repeated.
///
/// `events` closing (every sender dropped) means the process is going down;
/// this task exits without releasing — exit already frees everything.
async fn keep_warm_supervisor(
    context_map: Arc<ContextMap>,
    root: PathBuf,
    initial: Context,
    grace: Duration,
    mut events: mpsc::Receiver<KeepWarmEvent>,
) {
    let mut context = Some(initial);
    let mut active: usize = 0;
    let sleep = tokio::time::sleep(grace);
    tokio::pin!(sleep);
    let mut grace_armed = false;

    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Some(KeepWarmEvent::Connected(reply)) => {
                        active += 1;
                        grace_armed = false;
                        if context.is_none() {
                            context = Some(context_map.acquire(root.clone()).await);
                        }
                        if let Some(ctx) = &context {
                            let _ = reply.send(ctx.clone()).await;
                        }
                    }
                    Some(KeepWarmEvent::Disconnected) => {
                        active = active.saturating_sub(1);
                        if active == 0 {
                            sleep.as_mut().reset(tokio::time::Instant::now() + grace);
                            grace_armed = true;
                        }
                    }
                    None => return,
                }
            }
            () = &mut sleep, if grace_armed => {
                grace_armed = false;
                // Drop this supervisor's own reference BEFORE releasing (the
                // same discipline `ContextMap::release`'s doc comment and
                // `reroot_connection` follow): every per-connection clone is
                // already gone by the time `active` reached zero above, so
                // this is the last one, and the release below tears the
                // context down instead of blocking on a lingering clone.
                if let Some(ctx) = context.take() {
                    drop(ctx);
                    context_map.release(&root).await;
                }
            }
        }
    }
}

/// Relay raw bytes between `reader`/`writer` and the daemon socket at
/// `socket_path`, in both directions, until either side closes.
///
/// Byte-transparent — the daemon owns the framing, this only shuttles bytes. It
/// is the remote endpoint the SSH host wires its channel to: the channel carries
/// `reader`/`writer`, this connects them to the persistent daemon.
async fn relay<R, W>(mut reader: R, mut writer: W, socket_path: &Path) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let stream = UnixStream::connect(socket_path).await?;
    let (mut sock_reader, mut sock_writer) = stream.into_split();

    let upstream = async {
        tokio::io::copy(&mut reader, &mut sock_writer).await?;
        sock_writer.shutdown().await?;
        Ok::<(), anyhow::Error>(())
    };
    let downstream = async {
        tokio::io::copy(&mut sock_reader, &mut writer).await?;
        // Mirror upstream's half-close so no buffered bytes are lost if `writer`
        // is ever a buffering wrapper; on bare stdout/channel halves it is a flush.
        writer.shutdown().await?;
        Ok::<(), anyhow::Error>(())
    };

    // Whichever direction closes first ends the relay: a closed SSH channel
    // (upstream EOF) or a daemon that dropped the connection (downstream EOF).
    tokio::select! {
        result = upstream => result,
        result = downstream => result,
    }
}

/// Connect the process's stdio to the daemon socket at `socket_path` and relay
/// between them — the daemon binary's `--connect` mode. Thin [`relay`] wrapper
/// supplying real stdin/stdout; the SSH host runs this remotely so its channel
/// reaches the persistent daemon.
pub async fn connect_relay(socket_path: &Path) -> anyhow::Result<()> {
    relay(tokio::io::stdin(), tokio::io::stdout(), socket_path).await
}

/// Return whether a daemon is currently listening on `socket_path`.
///
/// A connect probe (no framing, no relay): the SSH host keys its reattach-vs-
/// spawn decision on this so it never starts a second daemon when one already
/// owns the socket.
pub async fn ping(socket_path: &Path) -> bool {
    UnixStream::connect(socket_path).await.is_ok()
}

impl Daemon {
    /// Start owning a worktree: scan `root` and watch it for changes on a
    /// blocking worker ([`worktree_worker`]), feeding the dispatch loop through
    /// an internal channel — the scan and watch never run on the loop itself.
    /// Must be called before [`Daemon::run`], from within a tokio runtime.
    pub fn watch_worktree(&mut self, root: PathBuf) {
        let (tx, rx) = mpsc::channel(WORKTREE_EVENT_CAPACITY);
        tokio::task::spawn_blocking(move || worktree_worker(root, tx));
        self.worktree = Some(rx);
    }

    /// Start LSP diagnostics: run a [`LspWorker`] for `root` on its own task,
    /// off the dispatch loop, wired so document changes (mapped from worktree
    /// `Changed` batches) flow to it and translated diagnostics flow back into
    /// the loop. `selector` chooses the language → server table — the built-in
    /// one in production, a stub-server table in tests. Must be called before
    /// [`Daemon::run`], from within a tokio runtime.
    ///
    /// Language servers are external child processes the worker spawns and
    /// drives; the dispatch loop only forwards changes and folds the resulting
    /// diagnostics, never blocking on server I/O.
    ///
    /// `root` is canonicalized first so the worker keys diagnostics in the same
    /// path space as the worktree snapshot: [`Snapshot::scan`] canonicalizes its
    /// root, so every worktree entry path (and thus the editor's open path) is
    /// relative to the canonical root. The worker derives a diagnostic's relative
    /// path by stripping its own root prefix from the server's publish URI; if
    /// that root is the raw (non-canonical) one, a symlinked or `..`-containing
    /// path yields a key the client never matches (`push_open_file_diagnostics`
    /// looks up by the canonical-relative open path → no inline marker, #308).
    /// Canonicalizing here also makes the daemon's own `didOpen`/`didChange` URIs
    /// canonical, so they round-trip cleanly with a server (rust-analyzer) that
    /// canonicalizes paths internally before publishing. A canonicalize failure
    /// (root gone) falls back to the raw root — the worktree worker would have
    /// failed the same way and stopped, so no diagnostics flow regardless.
    pub fn watch_lsp(&mut self, root: PathBuf, selector: DocumentSelector) {
        let root = root.canonicalize().unwrap_or(root);
        let (doc_tx, doc_rx) = mpsc::channel(LSP_CHANNEL_CAPACITY);
        let (buffer_tx, buffer_rx) = mpsc::channel(LSP_CHANNEL_CAPACITY);
        let (diag_tx, diag_rx) = mpsc::channel(LSP_CHANNEL_CAPACITY);
        let (status_tx, status_rx) = mpsc::channel(LSP_CHANNEL_CAPACITY);
        let (nav_req_tx, nav_req_rx) = mpsc::channel(LSP_CHANNEL_CAPACITY);
        let worker = LspWorker::new(
            root, selector, doc_rx, buffer_rx, diag_tx, status_tx, nav_req_rx,
        );
        tokio::spawn(worker.run());
        self.core.doc_changes = Some(doc_tx);
        self.core.buffer_events = Some(buffer_tx);
        // Kept as the daemon-lifetime keeper for the worker's nav channel; each
        // connection receives its own clone via [`Daemon::nav_request_sender`]
        // (#482). The dispatch loop no longer forwards nav itself.
        self.core.nav_requests = Some(nav_req_tx);
        self.lsp_diagnostics = Some(diag_rx);
        self.lsp_status = Some(status_rx);
    }

    /// A clone of the LSP worker's navigation-request sender, or `None` when LSP
    /// is not armed (#482). Each connection takes one so its hover/definition/
    /// references/document-symbol answers return to that socket alone; the
    /// dispatch loop keeps the original ([`Core::nav_requests`]) alive for the
    /// daemon's lifetime so the worker's nav channel does not close as clients
    /// come and go. Must be read before [`Daemon::run`] consumes the daemon.
    fn nav_request_sender(&self) -> Option<mpsc::Sender<NavRequest>> {
        self.core.nav_requests.clone()
    }

    /// Run the flat dispatch loop until the inbound channel closes.
    ///
    /// Each `ClientMessage` and each worktree event is matched directly to a
    /// handler on [`Core`]. The loop owns the `State` writer and the event
    /// broadcaster; it ends when every inbound sender is dropped.
    pub async fn run(self) {
        let Daemon {
            mut inbound,
            mut worktree,
            mut lsp_diagnostics,
            mut lsp_status,
            mut core,
        } = self;
        loop {
            tokio::select! {
                msg = inbound.recv() => match msg {
                    Some(msg) => core.dispatch(msg),
                    None => break,
                },
                event = next_worktree_event(&mut worktree) => match event {
                    Some(event) => core.apply_worktree(event),
                    // The worker ended (scan failure or shutdown); stop polling
                    // the closed channel.
                    None => worktree = None,
                },
                diagnostics = next_lsp_diagnostics(&mut lsp_diagnostics) => match diagnostics {
                    Some(diagnostics) => core.apply_diagnostics(diagnostics),
                    // The LSP worker ended; stop polling the closed channel.
                    None => lsp_diagnostics = None,
                },
                status = next_lsp_status(&mut lsp_status) => match status {
                    Some(status) => core.apply_lsp_status(status),
                    // The LSP worker ended; stop polling the closed channel.
                    None => lsp_status = None,
                },
            }
        }
    }

    /// Return the latest `State` snapshot (the `watch` sender's view).
    pub fn state(&self) -> State {
        self.core.state.borrow().clone()
    }
}

impl Core {
    /// Route a single `ClientMessage` to its handler.
    fn dispatch(&mut self, msg: ClientMessage) {
        match msg {
            // Live-buffer feed (#189): the editor's `BufferChanged` / `BufferClosed`
            // reach the shared dispatch loop (routed here by `serve_connection`)
            // because the LSP worker — the single owner of the document model
            // and servers — lives off this loop, not per connection.
            // Forward each to it as a `BufferEvent`; a full queue drops the event
            // (the next `BufferChanged` re-feeds, so a dropped one only delays
            // diagnostics, never corrupts the model), and an unarmed LSP drops it.
            ClientMessage::BufferChanged { path, content } => {
                self.forward_buffer_event(BufferEvent::Changed { path, content });
            }
            ClientMessage::BufferClosed { path } => {
                self.forward_buffer_event(BufferEvent::Closed { path });
            }
            // Terminal/tmux messages never reach the shared dispatch loop:
            // `serve_connection` routes them to this connection's own
            // `terminal_task` (per-client attach). The request/response
            // messages (`OpenFile`/`SaveFile`/`RequestDiff`) likewise never
            // reach it — they are answered per connection by `request_reply`,
            // request/response back to that socket, not on the broadcast bus.
            // Navigation requests (hover/definition/references/document-symbol)
            // are also answered per connection (#482): `serve_connection`
            // forwards them straight to the LSP worker with the connection's
            // private reply channel, so they never pass through here either.
            // Neither does the handshake:
            // `serve_connection` answers `Hello` per connection (#473) — the
            // `Welcome` must reach exactly one socket, and a version mismatch
            // closes only that connection. These arms are a defensive no-op
            // should one arrive here anyway.
            //
            // The source-control write ops (#544, hunk staging #545) are
            // answered per connection by `git_write::reply` (request/response
            // back to that socket), so they never reach this loop; their arms
            // below are a defensive no-op.
            //
            // The file-operation requests (#674) are answered per connection
            // by `file_ops::reply` the same way, so they never reach this loop
            // either; their arms below are a defensive no-op too.
            //
            // The directory-browse request (#766) is answered per connection
            // by `browse::reply`, same pattern; its arm below is a defensive
            // no-op too.
            //
            // The clone request (#827) will likewise be answered per
            // connection, as a detached task (`docs/spec-clone-repo.md`); the
            // daemon-side execution lands in a follow-on issue (#828) — its
            // arm below is a defensive no-op until then.
            ClientMessage::Hello { .. }
            | ClientMessage::Attach { .. }
            | ClientMessage::Input { .. }
            | ClientMessage::ResizePane { .. }
            | ClientMessage::TmuxCommand { .. }
            | ClientMessage::CapturePane { .. }
            | ClientMessage::QueryKeyTable
            | ClientMessage::QuerySessionList
            | ClientMessage::OpenFile { .. }
            | ClientMessage::SaveFile { .. }
            | ClientMessage::RequestDiff { .. }
            | ClientMessage::HoverRequest { .. }
            | ClientMessage::DefinitionRequest { .. }
            | ClientMessage::ReferencesRequest { .. }
            | ClientMessage::DocumentSymbolRequest { .. }
            | ClientMessage::StageFile { .. }
            | ClientMessage::UnstageFile { .. }
            | ClientMessage::StageHunk { .. }
            | ClientMessage::DiscardFile { .. }
            | ClientMessage::Commit { .. }
            | ClientMessage::CreateFile { .. }
            | ClientMessage::CreateDir { .. }
            | ClientMessage::RenamePath { .. }
            | ClientMessage::DeletePath { .. }
            | ClientMessage::QueryDirEntries { .. }
            | ClientMessage::CloneRepo { .. } => {}
        }
    }

    /// Forward a live-buffer event to the off-loop LSP worker, dropping it (with a
    /// log) when the worker's queue is full or LSP is not armed — the same
    /// non-blocking discipline the disk document-change forward uses.
    fn forward_buffer_event(&self, event: BufferEvent) {
        if let Some(buffer_events) = &self.buffer_events {
            if let Err(err) = buffer_events.try_send(event) {
                warn!(%err, "dropped live-buffer event");
            }
        }
    }

    /// Fold a worktree event into the `State` and onto the event bus.
    fn apply_worktree(&mut self, event: WorktreeEvent) {
        match event {
            WorktreeEvent::Scanned(snapshot) => {
                // Store the snapshot; each connection replays it from `State`
                // per connection (see `serve_connection`), so there is nothing
                // to broadcast here. The `State` change wakes already-attached
                // connections that handshook before the scan finished, so they
                // replay it too.
                self.state
                    .send_modify(|state| state.worktree = Some(snapshot));
            }
            WorktreeEvent::Changed(batch) => {
                // Forward the file-level changes to the off-loop LSP worker for
                // document sync *before* mutating the held snapshot — the send
                // never blocks the loop on server I/O (the worker owns that).
                // Only the observed / changed set drives `didOpen` / `didChange`
                // / `didClose` (spec: no eager whole-tree open), so the initial
                // `Scanned` snapshot is deliberately not forwarded.
                if let Some(doc_changes) = &self.doc_changes {
                    let changes = document_changes(&batch);
                    if !changes.is_empty() {
                        // A full queue means the worker is lagging; drop this
                        // batch rather than block the dispatch loop. The next
                        // change re-syncs from disk, so a dropped batch only
                        // delays diagnostics, never corrupts the model.
                        if let Err(err) = doc_changes.try_send(changes) {
                            warn!(%err, "dropped document-sync batch");
                        }
                    }
                }
                self.state.send_modify(|state| {
                    if let Some(worktree) = &mut state.worktree {
                        worktree.apply(&batch);
                    }
                });
                let _ = self.events.send(update_message(&batch));
            }
            WorktreeEvent::GitRecomputed(new_git) => {
                // Diff against the held status (built before storing the new one,
                // so the comparison sees the previous state), then store the new
                // full status. Each attaching connection replays the full status
                // from `State`, so even if these incrementals are missed (lagged
                // bus), a reattach reconciles.
                let messages = git_delta_messages(self.state.borrow().git.as_ref(), &new_git);
                self.state.send_modify(|state| state.git = Some(new_git));
                for msg in messages {
                    let _ = self.events.send(msg);
                }
            }
        }
    }

    /// Fold one server's full diagnostic set for a file into the `State` and
    /// onto the event bus.
    ///
    /// Full-set-replace per `(path, server)`: the published set replaces only
    /// this server's entry for the file, leaving every other server's entry
    /// intact (the aggregation the spec mandates). An empty set removes the key
    /// so the held map carries only live diagnostics — which keeps the
    /// per-connection replay free of stale empty sets. Either way a `Diagnostics`
    /// message is broadcast (the empty one is how the client clears its set),
    /// and each attaching connection replays the live map from `State`, so a
    /// lagged bus reconciles on reattach.
    fn apply_diagnostics(&mut self, diagnostics: LspDiagnostics) {
        let message = diagnostics_message(&diagnostics);
        let key = DiagnosticKey {
            path: diagnostics.path,
            server: diagnostics.server,
        };
        self.state.send_modify(|state| {
            if diagnostics.items.is_empty() {
                state.diagnostics.remove(&key);
            } else {
                state.diagnostics.insert(key, diagnostics.items);
            }
        });
        let _ = self.events.send(message);
    }

    /// Fold one language server's lifecycle transition into the `State` and
    /// onto the event bus (issue #520). Unlike `apply_diagnostics`, the entry
    /// is never removed — a server that has ever transitioned is always
    /// exactly one of `starting`/`running`/`crashed`, so the map only ever
    /// grows (by distinct server name) or overwrites its current state.
    fn apply_lsp_status(&mut self, event: LspStatusEvent) {
        let message = lsp_status_message(&event);
        self.state.send_modify(|state| {
            state.lsp_status.insert(event.server, event.state);
        });
        let _ = self.events.send(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique per-test Unix-socket path under the system temp dir — avoids a
    /// `tempfile` dependency. The pid plus an atomic counter keep concurrent
    /// tests from colliding; the short name stays under the ~108-byte
    /// `sockaddr_un` path limit.
    fn unique_socket_path() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("rift-uds-{}-{}.sock", std::process::id(), n))
    }

    /// Poll until the daemon socket accepts a connection, so tests don't race the
    /// `serve_uds` bind. Panics if it never comes up.
    async fn wait_for_socket(path: &Path) {
        for _ in 0..100 {
            if UnixStream::connect(path).await.is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("daemon socket {} never became connectable", path.display());
    }

    /// Read one framed `DaemonMessage` from `reader`, reassembling across reads.
    ///
    /// The decoder is caller-owned and must live for the whole connection: one
    /// read may deliver several back-to-back frames, and a per-call decoder
    /// would silently discard the buffered tail along with itself.
    async fn read_daemon_message<R: AsyncRead + Unpin>(
        reader: &mut R,
        decoder: &mut FrameDecoder,
    ) -> DaemonMessage {
        let mut buf = vec![0u8; 4096];
        loop {
            if let Some(msg) = decoder.next_frame::<DaemonMessage>().expect("decode frame") {
                return msg;
            }
            let n = reader.read(&mut buf).await.expect("read reply");
            assert!(n > 0, "stream closed before a full frame arrived");
            decoder.push(&buf[..n]);
        }
    }

    /// A framed `Hello` at the current protocol version.
    fn hello_frame() -> Vec<u8> {
        encode_frame(&ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        })
        .expect("encode Hello")
    }

    /// A framed `Hello` one version ahead of the daemon's — a mismatched client.
    fn mismatched_hello_frame() -> Vec<u8> {
        encode_frame(&ClientMessage::Hello {
            version: PROTOCOL_VERSION + 1,
        })
        .expect("encode mismatched Hello")
    }

    /// Read `reader` to EOF, panicking if any complete `DaemonMessage` frame
    /// arrives — the mismatch-close contract: nothing follows the `Welcome`.
    async fn assert_eof_without_frames<R: AsyncRead + Unpin>(
        reader: &mut R,
        decoder: &mut FrameDecoder,
    ) {
        let mut buf = vec![0u8; 4096];
        loop {
            if let Some(msg) = decoder.next_frame::<DaemonMessage>().expect("decode frame") {
                panic!("unexpected frame after the mismatch Welcome: {msg:?}");
            }
            let n = reader.read(&mut buf).await.expect("read until EOF");
            if n == 0 {
                return;
            }
            decoder.push(&buf[..n]);
        }
    }

    #[tokio::test]
    async fn test_serve_connection_hello_mismatch_replies_welcome_then_closes_without_streaming() {
        // The version gate (#473): a mismatched `Hello` is answered with the
        // daemon's OWN version and a clean close — no snapshot, no stream
        // frames. The worktree scan is awaited in `State` BEFORE the handshake,
        // so the absent snapshot below proves the gate, not a slow scan.
        let tmp = TempDir::new("mismatch");
        write_file(&tmp.path.join("tracked.txt"), "x");

        let (mut daemon, handles) = channels(8, 8);
        daemon.watch_worktree(tmp.path.clone());
        let inbound = handles.inbound.clone();
        let events = handles.subscribe();
        let mut state = handles.state.clone();
        let loop_handle = tokio::spawn(daemon.run());

        loop {
            if state.borrow_and_update().worktree.is_some() {
                break;
            }
            state.changed().await.expect("state sender alive");
        }

        let (client, server) = tokio::io::duplex(64 * 1024);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        let (server_reader, server_writer) = tokio::io::split(server);
        let conn = tokio::spawn(async move {
            serve_connection(
                server_reader,
                server_writer,
                inbound,
                events,
                state,
                None,
                None,
                None,
                None,
            )
            .await
        });

        client_writer
            .write_all(&mismatched_hello_frame())
            .await
            .expect("send mismatched Hello");
        client_writer.flush().await.expect("flush Hello");

        let mut decoder = FrameDecoder::new();
        assert_eq!(
            read_daemon_message(&mut client_reader, &mut decoder).await,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            },
            "the mismatch reply must carry the daemon's own version"
        );

        tokio::time::timeout(
            Duration::from_secs(5),
            assert_eof_without_frames(&mut client_reader, &mut decoder),
        )
        .await
        .expect("clean close within the timeout");

        let result = tokio::time::timeout(Duration::from_secs(5), conn)
            .await
            .expect("serve_connection returns after the mismatch close")
            .expect("connection task joins");
        result.expect("the mismatch close is clean, not an error");

        loop_handle.abort();
    }

    #[tokio::test]
    async fn test_serve_connection_hello_mismatch_does_not_disturb_concurrent_connection() {
        // The `Welcome` is per connection (#473): a mismatched client's
        // handshake must never surface on a healthy concurrent connection.
        // Under the old shared-bus `Welcome`, the mismatched handshake below
        // would inject a stray `Welcome` into the healthy client's stream.
        let (daemon, handles) = channels(64, 8);
        let loop_handle = tokio::spawn(daemon.run());

        // Healthy connection: handshakes at the current version.
        let (healthy, healthy_srv) = tokio::io::duplex(64 * 1024);
        let (mut healthy_reader, mut healthy_writer) = tokio::io::split(healthy);
        let (healthy_srv_reader, healthy_srv_writer) = tokio::io::split(healthy_srv);
        let healthy_conn = tokio::spawn({
            let inbound = handles.inbound.clone();
            let events = handles.subscribe();
            let state = handles.state.clone();
            async move {
                serve_connection(
                    healthy_srv_reader,
                    healthy_srv_writer,
                    inbound,
                    events,
                    state,
                    None,
                    None,
                    None,
                    None,
                )
                .await
            }
        });
        healthy_writer
            .write_all(&hello_frame())
            .await
            .expect("send healthy Hello");
        healthy_writer.flush().await.expect("flush healthy Hello");
        let mut healthy_decoder = FrameDecoder::new();
        assert_eq!(
            read_daemon_message(&mut healthy_reader, &mut healthy_decoder).await,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            }
        );

        // Mismatched connection: drive the full reject cycle (Welcome{own} +
        // clean close) to completion FIRST, so its handshake has provably been
        // processed before the healthy stream is asserted below.
        let (bad, bad_srv) = tokio::io::duplex(64 * 1024);
        let (mut bad_reader, mut bad_writer) = tokio::io::split(bad);
        let (bad_srv_reader, bad_srv_writer) = tokio::io::split(bad_srv);
        let bad_conn = tokio::spawn({
            let inbound = handles.inbound.clone();
            let events = handles.subscribe();
            let state = handles.state.clone();
            async move {
                serve_connection(
                    bad_srv_reader,
                    bad_srv_writer,
                    inbound,
                    events,
                    state,
                    None,
                    None,
                    None,
                    None,
                )
                .await
            }
        });
        bad_writer
            .write_all(&mismatched_hello_frame())
            .await
            .expect("send mismatched Hello");
        bad_writer.flush().await.expect("flush mismatched Hello");
        let mut bad_decoder = FrameDecoder::new();
        assert_eq!(
            read_daemon_message(&mut bad_reader, &mut bad_decoder).await,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            }
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            assert_eof_without_frames(&mut bad_reader, &mut bad_decoder),
        )
        .await
        .expect("mismatched connection closes within the timeout");
        tokio::time::timeout(Duration::from_secs(5), bad_conn)
            .await
            .expect("mismatched serve_connection returns")
            .expect("mismatched connection task joins")
            .expect("the mismatch close is clean, not an error");

        // The healthy connection's next frame must be exactly the next bus
        // event — no stray `Welcome` from the mismatched handshake in between.
        let post = DaemonMessage::UpdateWorktree {
            added: Vec::new(),
            changed: Vec::new(),
            removed: vec!["after-mismatch".to_owned()],
        };
        handles
            .events
            .send(post.clone())
            .expect("bus subscriber alive");
        assert_eq!(
            read_daemon_message(&mut healthy_reader, &mut healthy_decoder).await,
            post,
            "healthy stream must continue undisturbed after a mismatched Hello"
        );

        drop(healthy_writer);
        drop(healthy_reader);
        healthy_conn.abort();
        loop_handle.abort();
    }

    #[test]
    fn test_pidfile_path_appends_pid_suffix_to_socket() {
        assert_eq!(
            pidfile_path(Path::new("/run/rift/rift.sock")),
            PathBuf::from("/run/rift/rift.sock.pid")
        );
    }

    #[test]
    fn test_pidfile_path_appends_rather_than_replaces_extension() {
        // `with_extension` would yield `/run/rift.pid`; appending keeps the
        // socket name intact so each socket maps to a distinct pidfile.
        assert_eq!(
            pidfile_path(Path::new("/run/rift.sock")),
            PathBuf::from("/run/rift.sock.pid")
        );
        assert_eq!(
            pidfile_path(Path::new("/run/rift")),
            PathBuf::from("/run/rift.pid")
        );
    }

    #[tokio::test]
    async fn test_serve_uds_writes_pidfile_with_daemon_pid() {
        let sock = unique_socket_path();
        let server = tokio::spawn({
            let sock = sock.clone();
            async move { serve_uds(&sock, None).await }
        });
        wait_for_socket(&sock).await;

        let pidfile = pidfile_path(&sock);
        let contents = tokio::fs::read_to_string(&pidfile)
            .await
            .expect("pidfile written");
        assert_eq!(contents, std::process::id().to_string());

        server.abort();
        let _ = tokio::fs::remove_file(&sock).await;
        let _ = tokio::fs::remove_file(&pidfile).await;
    }

    #[tokio::test]
    async fn test_serve_uds_hello_returns_welcome() {
        let sock = unique_socket_path();
        let server = tokio::spawn({
            let sock = sock.clone();
            async move { serve_uds(&sock, None).await }
        });
        wait_for_socket(&sock).await;

        let mut client = UnixStream::connect(&sock).await.expect("connect");
        let mut decoder = FrameDecoder::new();
        client.write_all(&hello_frame()).await.expect("send Hello");
        client.flush().await.expect("flush Hello");

        assert_eq!(
            read_daemon_message(&mut client, &mut decoder).await,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            }
        );

        server.abort();
        let _ = tokio::fs::remove_file(&sock).await;
    }

    #[tokio::test]
    async fn test_serve_uds_survives_client_disconnect_and_reattaches() {
        let sock = unique_socket_path();
        let server = tokio::spawn({
            let sock = sock.clone();
            async move { serve_uds(&sock, None).await }
        });
        wait_for_socket(&sock).await;

        // First client: handshake, then disconnect by dropping the stream.
        {
            let mut c1 = UnixStream::connect(&sock).await.expect("connect 1");
            let mut d1 = FrameDecoder::new();
            c1.write_all(&hello_frame()).await.expect("send Hello 1");
            c1.flush().await.expect("flush 1");
            assert_eq!(
                read_daemon_message(&mut c1, &mut d1).await,
                DaemonMessage::Welcome {
                    version: PROTOCOL_VERSION,
                }
            );
        }

        // The daemon must still be listening: a second client reattaches and
        // completes its own handshake against the same running process.
        let mut c2 = UnixStream::connect(&sock)
            .await
            .expect("reconnect after disconnect");
        let mut d2 = FrameDecoder::new();
        c2.write_all(&hello_frame()).await.expect("send Hello 2");
        c2.flush().await.expect("flush 2");
        assert_eq!(
            read_daemon_message(&mut c2, &mut d2).await,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            }
        );

        assert!(
            !server.is_finished(),
            "serve_uds exited on client disconnect"
        );
        server.abort();
        let _ = tokio::fs::remove_file(&sock).await;
    }

    #[tokio::test]
    async fn test_serve_uds_rejects_bind_when_already_running() {
        let sock = unique_socket_path();
        let server = tokio::spawn({
            let sock = sock.clone();
            async move { serve_uds(&sock, None).await }
        });
        wait_for_socket(&sock).await;

        let err = serve_uds(&sock, None)
            .await
            .expect_err("second bind must fail");
        assert!(
            err.to_string().contains("already listening"),
            "unexpected error: {err}"
        );

        server.abort();
        let _ = tokio::fs::remove_file(&sock).await;
    }

    #[tokio::test]
    async fn test_serve_uds_rebinds_over_stale_socket() {
        let sock = unique_socket_path();
        // A leftover regular file at the path simulates a stale socket left by a
        // crashed daemon: connect fails, so serve_uds must remove it and bind.
        tokio::fs::write(&sock, b"stale")
            .await
            .expect("create stale file");

        let server = tokio::spawn({
            let sock = sock.clone();
            async move { serve_uds(&sock, None).await }
        });
        wait_for_socket(&sock).await;

        let mut client = UnixStream::connect(&sock)
            .await
            .expect("connect after rebind");
        let mut decoder = FrameDecoder::new();
        client.write_all(&hello_frame()).await.expect("send Hello");
        client.flush().await.expect("flush Hello");
        assert_eq!(
            read_daemon_message(&mut client, &mut decoder).await,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            }
        );

        server.abort();
        let _ = tokio::fs::remove_file(&sock).await;
    }

    #[tokio::test]
    async fn test_relay_round_trips_hello_to_welcome_over_uds() {
        let sock = unique_socket_path();
        let server = tokio::spawn({
            let sock = sock.clone();
            async move { serve_uds(&sock, None).await }
        });
        wait_for_socket(&sock).await;

        // Drive the private `relay` with in-memory "stdio": one half is the
        // relay's reader/writer, the other is the test's client.
        let (client, stdio) = tokio::io::duplex(64 * 1024);
        let (stdio_reader, stdio_writer) = tokio::io::split(stdio);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        let relay_task = tokio::spawn({
            let sock = sock.clone();
            async move { relay(stdio_reader, stdio_writer, &sock).await }
        });

        client_writer
            .write_all(&hello_frame())
            .await
            .expect("send Hello via relay");
        client_writer.flush().await.expect("flush Hello");

        let mut decoder = FrameDecoder::new();
        assert_eq!(
            read_daemon_message(&mut client_reader, &mut decoder).await,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            }
        );

        relay_task.abort();
        server.abort();
        let _ = tokio::fs::remove_file(&sock).await;
    }

    /// A self-cleaning temporary directory for worktree fixtures, mirroring the
    /// explorer tests' helper so these stay self-contained without a `tempfile`
    /// dev-dependency.
    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("rift-daemon-{tag}-{}-{n}", std::process::id()));
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

    /// Receive broadcast events until the predicate yields, with a generous
    /// ceiling for a real filesystem event to cross notify, the debounce, the
    /// rescan, and the dispatch loop.
    async fn recv_until<T>(
        events: &mut broadcast::Receiver<DaemonMessage>,
        mut pick: impl FnMut(DaemonMessage) -> Option<T>,
    ) -> T {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = events.recv().await.expect("event bus open");
                if let Some(found) = pick(msg) {
                    return found;
                }
            }
        })
        .await
        .expect("expected event within the timeout")
    }

    /// Read framed messages off a live connection until one complete chunked
    /// `WorktreeSnapshot` has arrived (tolerating the `Welcome` that precedes
    /// it), returning its entries. Panics on any other message — the snapshot is
    /// the per-connection replay, never an update.
    async fn read_full_snapshot<R: AsyncRead + Unpin>(
        reader: &mut R,
        decoder: &mut FrameDecoder,
    ) -> Vec<rift_protocol::WorktreeEntry> {
        // Bounded so a regressed delivery path (a dropped chunk that never lets
        // `final_chunk` arrive) fails the test deterministically instead of
        // hanging it.
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut welcome_seen = false;
            let mut collected = Vec::new();
            loop {
                match read_daemon_message(reader, decoder).await {
                    DaemonMessage::Welcome { version } => {
                        assert_eq!(version, PROTOCOL_VERSION);
                        welcome_seen = true;
                    }
                    DaemonMessage::WorktreeSnapshot {
                        entries: mut chunk,
                        final_chunk,
                        ..
                    } => {
                        collected.append(&mut chunk);
                        if final_chunk && welcome_seen {
                            return collected;
                        }
                    }
                    // The per-connection replay appends the full git status after
                    // the tree (for a git-repo root); tolerate it so this helper
                    // works against a repo, not only a plain dir.
                    DaemonMessage::UpdateGitStatus { .. } | DaemonMessage::RepoState { .. } => {}
                    other => panic!("unexpected message before snapshot: {other:?}"),
                }
            }
        })
        .await
        .expect("a complete snapshot within the timeout")
    }

    fn synthetic_entries(count: usize) -> BTreeMap<PathBuf, Entry> {
        (0..count)
            .map(|i| {
                (
                    PathBuf::from(format!("file-{i:05}.txt")),
                    Entry {
                        kind: rift_explorer::EntryKind::File,
                        ignored: false,
                        mtime: std::time::SystemTime::UNIX_EPOCH,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn test_snapshot_messages_empty_tree_yields_single_final_chunk() {
        let messages = snapshot_messages(Path::new("/root"), &BTreeMap::new());
        assert_eq!(
            messages,
            vec![DaemonMessage::WorktreeSnapshot {
                root: "/root".to_owned(),
                entries: Vec::new(),
                final_chunk: true,
            }]
        );
    }

    #[test]
    fn test_snapshot_messages_large_tree_chunks_with_final_flag_on_last_only() {
        let entries = synthetic_entries(SNAPSHOT_CHUNK + 1);
        let messages = snapshot_messages(Path::new("/root"), &entries);

        assert_eq!(messages.len(), 2);
        match &messages[0] {
            DaemonMessage::WorktreeSnapshot {
                entries,
                final_chunk,
                ..
            } => {
                assert_eq!(entries.len(), SNAPSHOT_CHUNK);
                assert!(!final_chunk);
            }
            other => panic!("expected WorktreeSnapshot, got {other:?}"),
        }
        match &messages[1] {
            DaemonMessage::WorktreeSnapshot {
                entries,
                final_chunk,
                ..
            } => {
                assert_eq!(entries.len(), 1);
                assert!(final_chunk);
            }
            other => panic!("expected WorktreeSnapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_serve_connection_replays_multichunk_snapshot_off_a_tiny_bus() {
        // Regression for #227: a tree spanning several SNAPSHOT_CHUNK frames,
        // served over an event bus far smaller than the chunk count, must still
        // arrive complete — the snapshot is delivered per connection, off the
        // bus, backpressured by the socket. Under the old bus broadcast the
        // surplus chunks were silently dropped and `final_chunk` never arrived.
        let tmp = TempDir::new("multichunk");
        let file_count = SNAPSHOT_CHUNK * 2 + 1; // three chunks
        for i in 0..file_count {
            write_file(&tmp.path.join(format!("f{i:06}.txt")), "x");
        }

        // Bus capacity 2 is below the snapshot's chunk count (3): if the snapshot
        // still flowed over the bus, chunks would be dropped and the read below
        // would time out.
        let (mut daemon, handles) = channels(2, 8);
        daemon.watch_worktree(tmp.path.clone());
        let inbound = handles.inbound.clone();
        let events = handles.subscribe();
        let state = handles.state.clone();
        let loop_handle = tokio::spawn(daemon.run());

        let (client, server) = tokio::io::duplex(8 * 1024);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        let (server_reader, server_writer) = tokio::io::split(server);
        let conn = tokio::spawn(async move {
            let _ = serve_connection(
                server_reader,
                server_writer,
                inbound,
                events,
                state,
                None,
                None,
                None,
                None,
            )
            .await;
        });

        client_writer
            .write_all(&hello_frame())
            .await
            .expect("send Hello");
        client_writer.flush().await.expect("flush Hello");

        let mut decoder = FrameDecoder::new();
        let entries = read_full_snapshot(&mut client_reader, &mut decoder).await;
        assert_eq!(
            entries.len(),
            file_count,
            "every entry across all chunks must arrive despite the tiny bus"
        );

        drop(client_writer);
        drop(client_reader);
        conn.abort();
        loop_handle.abort();
    }

    #[tokio::test]
    async fn test_serve_connection_bus_traffic_at_connect_welcome_is_first_frame() {
        // Regression for #425: events broadcast on the shared bus before this
        // connection's handshake completes must never reach its socket — the
        // client hard-fails on any non-`Welcome` first frame. Flood the bus
        // before sending `Hello`, then assert `Welcome` arrives first and that
        // post-handshake bus events flow again.
        let (daemon, handles) = channels(64, 8);
        let inbound = handles.inbound.clone();
        let events = handles.subscribe();
        let state = handles.state.clone();
        let loop_handle = tokio::spawn(daemon.run());

        let (client, server) = tokio::io::duplex(64 * 1024);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        let (server_reader, server_writer) = tokio::io::split(server);
        let conn = tokio::spawn(async move {
            let _ = serve_connection(
                server_reader,
                server_writer,
                inbound,
                events,
                state,
                None,
                None,
                None,
                None,
            )
            .await;
        });

        // Sustained bus traffic ahead of the handshake: the connection's
        // subscription (created above, before the flood) queues these in send
        // order, so every one of them precedes the `Welcome` on the bus.
        for i in 0..32 {
            handles
                .events
                .send(DaemonMessage::UpdateWorktree {
                    added: Vec::new(),
                    changed: Vec::new(),
                    removed: vec![format!("pre-handshake-{i}")],
                })
                .expect("bus subscriber alive");
        }

        client_writer
            .write_all(&hello_frame())
            .await
            .expect("send Hello");
        client_writer.flush().await.expect("flush Hello");

        let mut decoder = FrameDecoder::new();
        assert_eq!(
            read_daemon_message(&mut client_reader, &mut decoder).await,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            },
            "Welcome must be the first frame despite pre-handshake bus traffic"
        );

        // Post-handshake bus events reach the socket again. No worktree is
        // armed, so no snapshot intervenes: the very next frame must be this
        // event — any leaked pre-handshake frame would arrive ahead of it.
        let post = DaemonMessage::UpdateWorktree {
            added: Vec::new(),
            changed: Vec::new(),
            removed: vec!["post-handshake".to_owned()],
        };
        handles
            .events
            .send(post.clone())
            .expect("bus subscriber alive");
        assert_eq!(
            read_daemon_message(&mut client_reader, &mut decoder).await,
            post,
            "bus forwarding must resume after the handshake"
        );

        drop(client_writer);
        drop(client_reader);
        conn.abort();
        loop_handle.abort();
    }

    #[tokio::test]
    async fn test_serve_connection_bus_lag_replays_snapshot_to_converge() {
        // Regression for #426: when this connection's bus subscription lags,
        // the dropped incremental updates are unrecoverable — the connection
        // must replay the full off-bus snapshot so the client model converges
        // instead of staying permanently stale.
        let tmp = TempDir::new("lag-resync");
        write_file(&tmp.path.join("a.txt"), "x");
        write_file(&tmp.path.join("b.txt"), "y");

        // Bus capacity 2 with a 64-event burst: while the client is not
        // reading, the connection can drain at most the tiny duplex buffer
        // plus the retained backlog before it must observe `Lagged` — the
        // overflow is guaranteed regardless of task scheduling.
        let (mut daemon, handles) = channels(2, 8);
        daemon.watch_worktree(tmp.path.clone());
        let inbound = handles.inbound.clone();
        let events = handles.subscribe();
        let state = handles.state.clone();
        let loop_handle = tokio::spawn(daemon.run());

        let (client, server) = tokio::io::duplex(256);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        let (server_reader, server_writer) = tokio::io::split(server);
        let conn = tokio::spawn(async move {
            let _ = serve_connection(
                server_reader,
                server_writer,
                inbound,
                events,
                state,
                None,
                None,
                None,
                None,
            )
            .await;
        });

        client_writer
            .write_all(&hello_frame())
            .await
            .expect("send Hello");
        client_writer.flush().await.expect("flush Hello");

        let mut decoder = FrameDecoder::new();
        let initial = read_full_snapshot(&mut client_reader, &mut decoder).await;
        assert_eq!(initial.len(), 2, "initial replay carries the full tree");

        // Burst past the bus capacity while the client reads nothing: the
        // connection parks on the full duplex and its subscription overflows.
        for i in 0..64 {
            handles
                .events
                .send(DaemonMessage::UpdateWorktree {
                    added: Vec::new(),
                    changed: Vec::new(),
                    removed: vec![format!("burst-{i}")],
                })
                .expect("bus subscriber alive");
        }

        // Resume reading: burst frames written before the writer parked (and
        // the retained bus tail re-delivered after the lag) are tolerated —
        // convergence requires a complete fresh snapshot to arrive.
        let fresh = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut collected = Vec::new();
            loop {
                match read_daemon_message(&mut client_reader, &mut decoder).await {
                    DaemonMessage::UpdateWorktree { .. } => {}
                    DaemonMessage::WorktreeSnapshot {
                        entries,
                        final_chunk,
                        ..
                    } => {
                        collected.extend(entries);
                        if final_chunk {
                            return collected;
                        }
                    }
                    other => panic!("unexpected message during lag resync: {other:?}"),
                }
            }
        })
        .await
        .expect("lagged connection must receive a fresh snapshot");

        let paths: Vec<&str> = fresh.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            ["a.txt", "b.txt"],
            "replayed snapshot must carry the full current tree"
        );

        drop(client_writer);
        drop(client_reader);
        conn.abort();
        loop_handle.abort();
    }

    /// Kills an isolated `-L` tmux server on drop, so a terminal test leaves no
    /// stray tmux behind and never touches the developer's server.
    struct IsolatedTmux(String);

    impl Drop for IsolatedTmux {
        fn drop(&mut self) {
            let _ = std::process::Command::new("tmux")
                .args(["-L", &self.0, "kill-server"])
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    #[tokio::test]
    async fn test_shutdown_terminal_joins_a_task_parked_on_full_outbound() {
        // Deterministic regression for the teardown deadlock (#263 review).
        // serve_connection's teardown is `shutdown_terminal`. Park a real
        // terminal task in `process()` on a full (cap-1) outbound channel, then
        // call shutdown_terminal: it must RETURN. It does so by dropping the
        // receiver before joining — if it awaited the handle first, the parked
        // task would never wake and this times out. (The full serve_connection
        // break-path that exposed this is racy to force; this tests the exact
        // teardown code it runs.)
        let server = IsolatedTmux(format!("rift204td-{}", std::process::id()));
        let (in_tx, in_rx) = mpsc::channel(64);
        // Capacity 1: the attach's snapshot plus the pane's initial draw overrun
        // it, so the task parks on a send with nobody reading.
        let (out_tx, out_rx) = mpsc::channel(1);
        let (root_resolved_tx, _root_resolved_rx) = mpsc::channel(4);
        let handle = tokio::spawn(terminal::terminal_task(
            in_rx,
            out_tx,
            Some(server.0.clone()),
            None,
            root_resolved_tx,
        ));

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
                root: None,
            })
            .await
            .expect("attach");
        // Let the attach produce >1 message and park on the full channel.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            !handle.is_finished(),
            "task must be parked on the full outbound channel"
        );

        let done = tokio::time::timeout(
            Duration::from_secs(10),
            shutdown_terminal(in_tx, out_rx, handle),
        )
        .await;
        assert!(
            done.is_ok(),
            "shutdown_terminal hung joining a task parked on a full outbound channel"
        );
    }

    #[tokio::test]
    async fn test_serve_connection_returns_on_disconnect_during_flood() {
        // Integration smoke: the whole serve_connection path against real tmux —
        // attach, flood a pane, stop reading, disconnect — must RETURN.
        let server = IsolatedTmux(format!("rift204conn-{}", std::process::id()));

        let (daemon, handles) = channels(64, 64);
        let inbound = handles.inbound.clone();
        let events = handles.subscribe();
        let state = handles.state.clone();
        let loop_handle = tokio::spawn(daemon.run());

        let (client, server_io) = tokio::io::duplex(64 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server_io);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        let tmux_name = server.0.clone();
        let conn = tokio::spawn(async move {
            serve_connection(
                server_reader,
                server_writer,
                inbound,
                events,
                state,
                None,
                Some(tmux_name),
                None,
                None,
            )
            .await
        });

        // Handshake, then attach and wait for the layout snapshot.
        client_writer
            .write_all(&hello_frame())
            .await
            .expect("hello");
        client_writer.flush().await.expect("flush hello");
        let mut decoder = FrameDecoder::new();
        loop {
            if matches!(
                read_daemon_message(&mut client_reader, &mut decoder).await,
                DaemonMessage::Welcome { .. }
            ) {
                break;
            }
        }
        client_writer
            .write_all(
                &encode_frame(&ClientMessage::Attach {
                    session: "rift".to_owned(),
                    root: None,
                })
                .expect("encode attach"),
            )
            .await
            .expect("attach");
        client_writer.flush().await.expect("flush attach");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if matches!(
                    read_daemon_message(&mut client_reader, &mut decoder).await,
                    DaemonMessage::LayoutSnapshot { .. }
                ) {
                    break;
                }
            }
        })
        .await
        .expect("layout snapshot");

        // Flood the pane, then stop reading and disconnect.
        client_writer
            .write_all(
                &encode_frame(&ClientMessage::Input {
                    pane_id: 0,
                    data: "yes RIFTFLOOD\n".to_owned(),
                })
                .expect("encode input"),
            )
            .await
            .expect("flood");
        client_writer.flush().await.expect("flush flood");
        // Give the flood a moment to fill buffers, then disconnect mid-stream.
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(client_reader);
        drop(client_writer);

        // serve_connection must return (the fix drops terminal_out_rx before
        // awaiting the possibly-parked terminal task).
        let result = tokio::time::timeout(Duration::from_secs(15), conn).await;
        assert!(
            result.is_ok(),
            "serve_connection hung on disconnect during a flood"
        );

        loop_handle.abort();
    }

    #[tokio::test]
    async fn test_serve_connection_attach_reroots_worktree_snapshot_to_the_resolved_session_root() {
        // #737 end-to-end (the Attach seam): two real tmux sessions, each
        // stamped with a DIFFERENT `@root`. Attaching to each in turn must
        // re-root this connection's reactive layer — resolve -> acquire ->
        // release-old -> snapshot — so the FIRST WorktreeSnapshot after each
        // Attach carries THAT session's own root and tree, never the other's
        // and never stale data left over from before the switch. `root: None`
        // below: this connection has no connect-time default to fall back
        // on, so the snapshots below can only have come from `Attach`.
        let server = IsolatedTmux(format!("rift737e2e-{}", std::process::id()));
        let tmp_a = TempDir::new("reroot-e2e-a");
        let tmp_b = TempDir::new("reroot-e2e-b");
        write_file(&tmp_a.path.join("a-only.txt"), "a");
        write_file(&tmp_b.path.join("b-only.txt"), "b");

        for (session, root) in [("sess-a", &tmp_a.path), ("sess-b", &tmp_b.path)] {
            let status = std::process::Command::new("tmux")
                .args(["-L", &server.0, "new-session", "-d", "-s", session])
                .status()
                .expect("tmux new-session");
            assert!(status.success(), "tmux new-session {session} failed");
            let status = std::process::Command::new("tmux")
                .args([
                    "-L",
                    &server.0,
                    "set-option",
                    "-t",
                    session,
                    "@root",
                    &root.to_string_lossy(),
                ])
                .status()
                .expect("tmux set-option @root");
            assert!(
                status.success(),
                "tmux set-option @root for {session} failed"
            );
        }

        let context_map = Arc::new(ContextMap::new());
        let (daemon, handles) = channels(64, 64);
        let inbound = handles.inbound.clone();
        let events = handles.subscribe();
        let state = handles.state.clone();
        let loop_handle = tokio::spawn(daemon.run());

        let (client, server_io) = tokio::io::duplex(64 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server_io);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        let tmux_name = server.0.clone();
        let conn = tokio::spawn(async move {
            serve_connection(
                server_reader,
                server_writer,
                inbound,
                events,
                state,
                None,
                Some(tmux_name),
                None,
                Some(context_map),
            )
            .await
        });

        client_writer
            .write_all(&hello_frame())
            .await
            .expect("hello");
        client_writer.flush().await.expect("flush hello");
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            read_daemon_message(&mut client_reader, &mut decoder).await,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            }
        );

        async fn attach_and_read_snapshot(
            client_writer: &mut (impl AsyncWrite + Unpin),
            client_reader: &mut (impl AsyncRead + Unpin),
            decoder: &mut FrameDecoder,
            session: &str,
        ) -> (String, Vec<String>) {
            client_writer
                .write_all(
                    &encode_frame(&ClientMessage::Attach {
                        session: session.to_owned(),
                        root: None,
                    })
                    .expect("encode attach"),
                )
                .await
                .expect("send attach");
            client_writer.flush().await.expect("flush attach");

            tokio::time::timeout(Duration::from_secs(10), async {
                let mut paths = Vec::new();
                loop {
                    // Terminal/layout traffic interleaves with the worktree
                    // snapshot; only the snapshot matters here.
                    if let DaemonMessage::WorktreeSnapshot {
                        root,
                        entries,
                        final_chunk,
                    } = read_daemon_message(client_reader, decoder).await
                    {
                        paths.extend(entries.into_iter().map(|e| e.path));
                        if final_chunk {
                            return (root, paths);
                        }
                    }
                }
            })
            .await
            .expect("worktree snapshot after attach")
        }

        let (root_a, paths_a) = attach_and_read_snapshot(
            &mut client_writer,
            &mut client_reader,
            &mut decoder,
            "sess-a",
        )
        .await;
        assert_eq!(
            root_a,
            std::fs::canonicalize(&tmp_a.path)
                .expect("canonicalize root a")
                .to_string_lossy()
        );
        assert_eq!(paths_a, ["a-only.txt"]);

        let (root_b, paths_b) = attach_and_read_snapshot(
            &mut client_writer,
            &mut client_reader,
            &mut decoder,
            "sess-b",
        )
        .await;
        assert_eq!(
            root_b,
            std::fs::canonicalize(&tmp_b.path)
                .expect("canonicalize root b")
                .to_string_lossy()
        );
        assert_eq!(
            paths_b,
            ["b-only.txt"],
            "the snapshot after switching to sess-b must carry root B's tree, not A's"
        );

        drop(client_reader);
        drop(client_writer);
        let _ = tokio::time::timeout(Duration::from_secs(10), conn).await;
        loop_handle.abort();
    }

    #[tokio::test]
    async fn test_worktree_scan_populates_state_and_streams_update_on_change() {
        let tmp = TempDir::new("scan");
        write_file(&tmp.path.join("src/main.rs"), "fn main() {}");
        write_file(&tmp.path.join("README.md"), "# readme");

        let (mut daemon, handles) = channels(64, 8);
        daemon.watch_worktree(tmp.path.clone());
        let mut events = handles.subscribe();
        let mut state = handles.state.clone();
        let loop_handle = tokio::spawn(daemon.run());

        // The initial scan lands in State (the snapshot is replayed per
        // connection now, not broadcast on the bus). `borrow_and_update` marks
        // each value seen so the `changed` await cannot miss the transition.
        loop {
            if state.borrow_and_update().worktree.is_some() {
                break;
            }
            state.changed().await.expect("state sender alive");
        }
        let held = state.borrow().worktree.clone().expect("worktree in State");
        assert!(held.get(Path::new("README.md")).is_some());
        assert!(held.get(Path::new("src")).is_some());
        assert!(held.get(Path::new("src/main.rs")).is_some());

        // A new file streams as an incremental UpdateWorktree on the bus, and
        // the State follows it.
        write_file(&tmp.path.join("src/lib.rs"), "pub fn lib() {}");
        let added = recv_until(&mut events, |msg| match msg {
            DaemonMessage::UpdateWorktree { added, .. } => Some(added),
            _ => None,
        })
        .await;
        assert!(added.iter().any(|e| e.path == "src/lib.rs"));
        let held = state.borrow().worktree.clone().expect("worktree in State");
        assert!(held.get(Path::new("src/lib.rs")).is_some());

        drop(handles);
        drop(events);
        loop_handle.await.expect("dispatch loop joins cleanly");
    }

    #[tokio::test]
    async fn test_serve_uds_replays_snapshot_to_each_attach() {
        let tmp = TempDir::new("reattach");
        write_file(&tmp.path.join("tracked.txt"), "x");

        let sock = unique_socket_path();
        let server = tokio::spawn({
            let sock = sock.clone();
            let root = tmp.path.clone();
            async move { serve_uds(&sock, Some(root)).await }
        });
        wait_for_socket(&sock).await;

        // First attach: handshake, receive the full snapshot, then disconnect.
        {
            let mut c1 = UnixStream::connect(&sock).await.expect("connect 1");
            let mut d1 = FrameDecoder::new();
            c1.write_all(&hello_frame()).await.expect("send Hello 1");
            c1.flush().await.expect("flush 1");
            let entries = read_full_snapshot(&mut c1, &mut d1).await;
            assert!(entries.iter().any(|e| e.path == "tracked.txt"));
        }

        // Reattach: a fresh connection replays the snapshot again. The replay is
        // per connection (the #62 reattach contract), with no shared bus
        // involved — so a large snapshot cannot be lost to a lagging subscriber.
        let mut c2 = UnixStream::connect(&sock).await.expect("reconnect");
        let mut d2 = FrameDecoder::new();
        c2.write_all(&hello_frame()).await.expect("send Hello 2");
        c2.flush().await.expect("flush 2");
        let entries = read_full_snapshot(&mut c2, &mut d2).await;
        assert!(entries.iter().any(|e| e.path == "tracked.txt"));

        server.abort();
        let _ = tokio::fs::remove_file(&sock).await;
    }

    #[tokio::test]
    async fn test_serve_uds_with_worktree_streams_snapshot_then_updates() {
        let tmp = TempDir::new("uds-worktree");
        write_file(&tmp.path.join("src/main.rs"), "fn main() {}");

        let sock = unique_socket_path();
        let server = tokio::spawn({
            let sock = sock.clone();
            let root = tmp.path.clone();
            async move { serve_uds(&sock, Some(root)).await }
        });
        wait_for_socket(&sock).await;

        let mut client = UnixStream::connect(&sock).await.expect("connect");
        let mut decoder = FrameDecoder::new();
        client.write_all(&hello_frame()).await.expect("send Hello");
        client.flush().await.expect("flush Hello");

        // The handshake is followed by the per-connection snapshot replay.
        let entries = read_full_snapshot(&mut client, &mut decoder).await;
        assert!(entries.iter().any(|e| e.path == "src/main.rs"));

        // A change on disk reaches the attached client as an UpdateWorktree.
        write_file(&tmp.path.join("src/lib.rs"), "pub fn lib() {}");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match read_daemon_message(&mut client, &mut decoder).await {
                    DaemonMessage::UpdateWorktree { added, .. }
                        if added.iter().any(|e| e.path == "src/lib.rs") =>
                    {
                        break;
                    }
                    // Tolerate unrelated traffic (e.g. a coalesced batch split).
                    _ => continue,
                }
            }
        })
        .await
        .expect("UpdateWorktree for the new file within the timeout");

        server.abort();
        let _ = tokio::fs::remove_file(&sock).await;
    }

    #[tokio::test]
    async fn test_ping_false_when_absent_true_when_listening() {
        let sock = unique_socket_path();
        assert!(!ping(&sock).await, "ping must be false before any bind");

        let server = tokio::spawn({
            let sock = sock.clone();
            async move { serve_uds(&sock, None).await }
        });
        wait_for_socket(&sock).await;
        assert!(
            ping(&sock).await,
            "ping must be true while the daemon listens"
        );

        server.abort();
        let _ = tokio::fs::remove_file(&sock).await;
    }

    // --- git status wiring (#134) ---

    use rift_explorer::{GitEntryStatus as ExGitEntryStatus, GitStatusCode as ExCode};

    /// Build an explorer `GitStatus` from `(path, index, worktree)` triples and
    /// an optional branch, via the public `compute` path is overkill for unit
    /// tests — instead exercise `git_delta_messages` against a real computed
    /// status from a git fixture (below). This helper builds the daemon-side
    /// fixture repos.
    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
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

    fn init_git_repo(tag: &str) -> TempDir {
        let tmp = TempDir::new(tag);
        git(&tmp.path, &["init", "-q", "-b", "main"]);
        write_file(&tmp.path.join("tracked.txt"), "v1\n");
        git(&tmp.path, &["add", "tracked.txt"]);
        git(&tmp.path, &["commit", "-q", "-m", "init"]);
        tmp
    }

    /// A flush whose git recompute fails emits worktree changes but no git tick
    /// (#430). The relay must still deliver those changes on the idle-poll
    /// timeout — not hold them until the next successful recompute — and a
    /// later successful recompute must follow as a normal git tick.
    #[tokio::test]
    async fn test_relay_events_git_tick_absent_still_relays_worktree_changes() {
        let (changes_tx, changes_rx) = std::sync::mpsc::channel::<Vec<Change>>();
        let (git_tx, git_rx) = std::sync::mpsc::channel::<GitStatus>();
        let (events_tx, mut events_rx) = mpsc::channel(WORKTREE_EVENT_CAPACITY);

        // `relay_events` blocks (`recv_timeout` / `blocking_send`), so it runs
        // on its own thread — the same shape as the production worker.
        let relay = std::thread::spawn(move || {
            relay_events(&changes_rx, &git_rx, &events_tx);
        });

        // Injected git failure: the flush queued a change batch, but the failed
        // recompute produced no git tick.
        changes_tx
            .send(vec![Change::Removed {
                path: PathBuf::from("gone.txt"),
            }])
            .expect("queue change batch");

        let event = tokio::time::timeout(Duration::from_secs(5), events_rx.recv())
            .await
            .expect("worktree change relayed despite the missing git tick")
            .expect("relay alive");
        assert!(
            matches!(
                &event,
                WorktreeEvent::Changed(batch)
                    if matches!(
                        batch.as_slice(),
                        [Change::Removed { path }] if path.as_path() == Path::new("gone.txt")
                    )
            ),
            "expected the queued change batch to be relayed"
        );

        // Git status recovers on a later tick: the recompute is relayed as a
        // normal GitRecomputed event.
        let repo = init_git_repo("relay-drain");
        let status = GitStatus::compute(&repo.path).expect("compute status");
        git_tx.send(status).expect("queue git tick");
        let event = tokio::time::timeout(Duration::from_secs(5), events_rx.recv())
            .await
            .expect("git tick relayed after recovery")
            .expect("relay alive");
        assert!(matches!(event, WorktreeEvent::GitRecomputed(_)));

        // Dropping the watcher-side senders disconnects the relay's channels;
        // it must return rather than spin.
        drop(changes_tx);
        drop(git_tx);
        relay.join().expect("relay thread joins cleanly");
    }

    #[test]
    fn test_git_delta_messages_full_when_old_is_none() {
        let repo = init_git_repo("delta-full");
        write_file(&repo.path.join("loose.txt"), "x\n");
        let status = GitStatus::compute(&repo.path).expect("compute");

        let messages = git_delta_messages(None, &status);
        // Full set: one UpdateGitStatus (all entries as changed, nothing cleared)
        // and a RepoState.
        let update = messages
            .iter()
            .find_map(|m| match m {
                DaemonMessage::UpdateGitStatus { changed, cleared } => Some((changed, cleared)),
                _ => None,
            })
            .expect("an UpdateGitStatus");
        assert!(update.1.is_empty(), "full set clears nothing");
        assert!(update.0.iter().any(|e| e.path == "loose.txt"
            && e.status.worktree == rift_protocol::GitStatusCode::Untracked));
        assert!(messages
            .iter()
            .any(|m| matches!(m, DaemonMessage::RepoState { branch, .. } if branch.as_deref() == Some("main"))));
    }

    #[test]
    fn test_git_delta_messages_incremental_changed_and_cleared() {
        let repo = init_git_repo("delta-incr");
        // old: one untracked file. new: that file gone, a different one modified.
        write_file(&repo.path.join("gone.txt"), "g\n");
        let old = GitStatus::compute(&repo.path).expect("old");
        std::fs::remove_file(repo.path.join("gone.txt")).expect("rm");
        write_file(&repo.path.join("tracked.txt"), "v2\n");
        let new = GitStatus::compute(&repo.path).expect("new");

        let messages = git_delta_messages(Some(&old), &new);
        let (changed, cleared) = messages
            .iter()
            .find_map(|m| match m {
                DaemonMessage::UpdateGitStatus { changed, cleared } => Some((changed, cleared)),
                _ => None,
            })
            .expect("an UpdateGitStatus");
        assert!(
            changed.iter().any(|e| e.path == "tracked.txt"
                && e.status.worktree == rift_protocol::GitStatusCode::Modified),
            "the newly-modified file is changed"
        );
        assert!(
            cleared.iter().any(|p| p == "gone.txt"),
            "the removed-from-status file is cleared"
        );
    }

    #[test]
    fn test_git_delta_messages_repo_only_change_emits_only_repo_state() {
        let repo = init_git_repo("delta-repo");
        let old = GitStatus::compute(&repo.path).expect("old");
        git(&repo.path, &["checkout", "-q", "-b", "feature"]);
        let new = GitStatus::compute(&repo.path).expect("new");

        let messages = git_delta_messages(Some(&old), &new);
        // Only the branch changed; no per-file status delta.
        assert!(
            !messages
                .iter()
                .any(|m| matches!(m, DaemonMessage::UpdateGitStatus { .. })),
            "no per-file delta for a branch-only change: {messages:?}"
        );
        assert!(messages.iter().any(
            |m| matches!(m, DaemonMessage::RepoState { branch, .. } if branch.as_deref() == Some("feature"))
        ));
    }

    #[test]
    fn test_git_delta_messages_repo_state_carries_line_totals() {
        let repo = init_git_repo("delta-totals");
        write_file(&repo.path.join("tracked.txt"), "v2\nextra\n");
        let status = GitStatus::compute(&repo.path).expect("compute");

        let messages = git_delta_messages(None, &status);
        let totals = messages
            .iter()
            .find_map(|m| match m {
                DaemonMessage::RepoState {
                    lines_added,
                    lines_removed,
                    ..
                } => Some((*lines_added, *lines_removed)),
                _ => None,
            })
            .expect("a RepoState");
        assert_eq!(totals, (2, 1));
    }

    #[test]
    fn test_git_delta_messages_totals_only_change_still_emits_repo_state() {
        // The per-file porcelain code stays `Modified` on both sides, so no
        // `UpdateGitStatus` delta fires — but the line totals differ, and
        // `RepoState`'s `PartialEq` covers them, so it must still re-emit
        // (the recompute-driven "+N -M updates on the recompute cadence"
        // acceptance behavior).
        let repo = init_git_repo("delta-totals-only");
        write_file(&repo.path.join("tracked.txt"), "v2\n");
        let old = GitStatus::compute(&repo.path).expect("old");
        write_file(&repo.path.join("tracked.txt"), "v2\nv3\nv4\n");
        let new = GitStatus::compute(&repo.path).expect("new");

        let messages = git_delta_messages(Some(&old), &new);
        assert!(
            !messages
                .iter()
                .any(|m| matches!(m, DaemonMessage::UpdateGitStatus { .. })),
            "the porcelain code is unchanged, so no per-file delta: {messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, DaemonMessage::RepoState { .. })),
            "the line totals changed, so RepoState must still re-emit: {messages:?}"
        );
    }

    #[test]
    fn test_git_delta_messages_no_change_is_empty() {
        let repo = init_git_repo("delta-none");
        let a = GitStatus::compute(&repo.path).expect("a");
        let b = GitStatus::compute(&repo.path).expect("b");
        assert!(git_delta_messages(Some(&a), &b).is_empty());
    }

    #[test]
    fn test_wire_git_code_maps_every_variant() {
        use rift_protocol::GitStatusCode as P;
        for (e, p) in [
            (ExCode::Unmodified, P::Unmodified),
            (ExCode::Modified, P::Modified),
            (ExCode::TypeChange, P::TypeChange),
            (ExCode::Added, P::Added),
            (ExCode::Deleted, P::Deleted),
            (ExCode::Renamed, P::Renamed),
            (ExCode::Copied, P::Copied),
            (ExCode::Unmerged, P::Unmerged),
            (ExCode::Untracked, P::Untracked),
        ] {
            assert_eq!(wire_git_code(e), p);
        }
    }

    #[test]
    fn test_wire_git_entry_maps_path_and_both_sides() {
        let entry = wire_git_entry(
            Path::new("src/main.rs"),
            &ExGitEntryStatus {
                index: ExCode::Added,
                worktree: ExCode::Modified,
            },
        );
        assert_eq!(entry.path, "src/main.rs");
        assert_eq!(entry.status.index, rift_protocol::GitStatusCode::Added);
        assert_eq!(
            entry.status.worktree,
            rift_protocol::GitStatusCode::Modified
        );
    }

    #[test]
    fn test_lsp_status_message_carries_server_and_state() {
        let event = LspStatusEvent {
            server: "rust-analyzer".to_string(),
            state: LspServerState::Crashed,
        };
        assert_eq!(
            lsp_status_message(&event),
            DaemonMessage::LspStatus {
                server: "rust-analyzer".to_string(),
                state: LspServerState::Crashed,
            }
        );
    }

    #[test]
    fn test_lsp_status_snapshot_messages_one_per_server() {
        let mut lsp_status = BTreeMap::new();
        lsp_status.insert("rust-analyzer".to_string(), LspServerState::Running);
        lsp_status.insert("some-linter".to_string(), LspServerState::Crashed);

        let messages = lsp_status_snapshot_messages(&lsp_status);
        assert_eq!(messages.len(), 2);
        assert!(messages.contains(&DaemonMessage::LspStatus {
            server: "rust-analyzer".to_string(),
            state: LspServerState::Running,
        }));
        assert!(messages.contains(&DaemonMessage::LspStatus {
            server: "some-linter".to_string(),
            state: LspServerState::Crashed,
        }));
    }

    #[test]
    fn test_lsp_status_snapshot_messages_empty_map_yields_no_messages() {
        assert!(lsp_status_snapshot_messages(&BTreeMap::new()).is_empty());
    }

    /// Drain framed messages off a connection until no message arrives within
    /// `settle`, returning all collected. Tolerates ordering/duplicates (the
    /// per-connection replay and the bus broadcast can both deliver git state).
    async fn drain_messages<R: AsyncRead + Unpin>(
        reader: &mut R,
        decoder: &mut FrameDecoder,
        settle: Duration,
    ) -> Vec<DaemonMessage> {
        let mut msgs = Vec::new();
        while let Ok(msg) = tokio::time::timeout(settle, read_daemon_message(reader, decoder)).await
        {
            msgs.push(msg);
        }
        msgs
    }

    fn untracked_in(msgs: &[DaemonMessage], path: &str) -> bool {
        msgs.iter().any(|m| {
            matches!(m, DaemonMessage::UpdateGitStatus { changed, .. }
                if changed.iter().any(|e| e.path == path
                    && e.status.worktree == rift_protocol::GitStatusCode::Untracked))
        })
    }

    #[tokio::test]
    async fn test_serve_uds_git_repo_streams_status_and_updates() {
        let repo = init_git_repo("uds-git");
        write_file(&repo.path.join("loose.txt"), "x\n");

        let sock = unique_socket_path();
        let server = tokio::spawn({
            let sock = sock.clone();
            let root = repo.path.clone();
            async move { serve_uds(&sock, Some(root)).await }
        });
        wait_for_socket(&sock).await;

        let mut client = UnixStream::connect(&sock).await.expect("connect");
        let mut decoder = FrameDecoder::new();
        client.write_all(&hello_frame()).await.expect("send Hello");
        client.flush().await.expect("flush Hello");

        // Initial: the replay (and/or bus) delivers the worktree snapshot plus
        // the full git status — the untracked file and the branch.
        let initial = drain_messages(&mut client, &mut decoder, Duration::from_secs(2)).await;
        assert!(
            untracked_in(&initial, "loose.txt"),
            "initial git status carries the untracked file: {initial:?}"
        );
        assert!(
            initial.iter().any(|m| matches!(m, DaemonMessage::RepoState { branch, .. } if branch.as_deref() == Some("main"))),
            "initial repo state carries the branch: {initial:?}"
        );

        // A `git add` mutates `.git/index`; the daemon recomputes and streams an
        // incremental moving the change to the index (staged) side.
        git(&repo.path, &["add", "loose.txt"]);
        let after = drain_messages(&mut client, &mut decoder, Duration::from_secs(3)).await;
        assert!(
            after.iter().any(|m| {
                matches!(m, DaemonMessage::UpdateGitStatus { changed, .. }
                    if changed.iter().any(|e| e.path == "loose.txt"
                        && e.status.index == rift_protocol::GitStatusCode::Added))
            }),
            "staging streams an index-side update: {after:?}"
        );

        server.abort();
        let _ = tokio::fs::remove_file(&sock).await;
    }

    #[tokio::test]
    async fn test_worktree_worker_upgrades_to_git_after_init() {
        // A root that becomes a git repository after startup must gain git status
        // without a daemon restart: the worktree-only phase re-probes each tick
        // and upgrades in place when a repo appears (#483).
        let tmp = TempDir::new("git-reprobe");
        write_file(&tmp.path.join("tracked.txt"), "v1\n");
        let (mut daemon, handles) = channels(64, 8);
        daemon.watch_worktree(tmp.path.clone());
        let mut state = handles.state.clone();
        let loop_handle = tokio::spawn(daemon.run());
        // The initial scan lands with no git status — the root is not a repo yet.
        loop {
            {
                let snap = state.borrow_and_update();
                if snap.worktree.is_some() {
                    assert!(snap.git.is_none(), "a non-repo root carries no git status");
                    break;
                }
            }
            state.changed().await.expect("state sender alive");
        }
        // Make the root a repository. `git add` (no commit) leaves tracked.txt
        // staged as an add, so the recomputed status carries an entry for it.
        git(&tmp.path, &["init", "-q", "-b", "main"]);
        git(&tmp.path, &["add", "tracked.txt"]);
        // The next re-probe tick upgrades in place: git status for the tracked
        // file arrives without a restart.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                state.changed().await.expect("state sender alive");
                let has_status = state
                    .borrow_and_update()
                    .git
                    .as_ref()
                    .is_some_and(|status| status.get(Path::new("tracked.txt")).is_some());
                if has_status {
                    break;
                }
            }
        })
        .await
        .expect("git status delivered after init without a daemon restart");
        loop_handle.abort();
    }

    // ── Navigation routing + per-connection drop-stale (#482) ────────────────

    /// A hover request `ClientMessage` at `id` for a fixed position.
    fn hover_request_frame(id: u64) -> Vec<u8> {
        encode_frame(&ClientMessage::HoverRequest {
            id: NavRequestId(id),
            path: "a.rs".to_string(),
            position: rift_protocol::Position {
                line: 0,
                character: 0,
            },
        })
        .expect("encode HoverRequest")
    }

    /// Read framed messages until a `HoverResponse` arrives, returning its id.
    /// Tolerates the leading `Welcome`; panics on any other message so a frame
    /// that leaked from another connection fails the test loudly. Bounded so a
    /// regression that never routes the answer fails fast instead of hanging CI.
    async fn read_hover_response_id<R: AsyncRead + Unpin>(
        reader: &mut R,
        decoder: &mut FrameDecoder,
    ) -> NavRequestId {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match read_daemon_message(reader, decoder).await {
                    DaemonMessage::Welcome { .. } => continue,
                    DaemonMessage::HoverResponse { id, .. } => return id,
                    other => panic!("unexpected frame before hover response: {other:?}"),
                }
            }
        })
        .await
        .expect("a hover response must arrive within the timeout")
    }

    /// A document-symbol request `ClientMessage` at `id`. No `Position` field,
    /// unlike the other navigation requests.
    fn document_symbol_request_frame(id: u64) -> Vec<u8> {
        encode_frame(&ClientMessage::DocumentSymbolRequest {
            id: NavRequestId(id),
            path: "a.rs".to_string(),
        })
        .expect("encode DocumentSymbolRequest")
    }

    /// Read framed messages until a `DocumentSymbolResponse` arrives, returning
    /// its id. Same tolerance/bound discipline as [`read_hover_response_id`].
    async fn read_document_symbol_response_id<R: AsyncRead + Unpin>(
        reader: &mut R,
        decoder: &mut FrameDecoder,
    ) -> NavRequestId {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match read_daemon_message(reader, decoder).await {
                    DaemonMessage::Welcome { .. } => continue,
                    DaemonMessage::DocumentSymbolResponse { id, .. } => return id,
                    other => panic!("unexpected frame before document_symbol response: {other:?}"),
                }
            }
        })
        .await
        .expect("a document_symbol response must arrive within the timeout")
    }

    /// Assert no further frame arrives within `dur` — the discriminator between
    /// per-connection routing and the old broadcast: a leaked response would show
    /// up here. The connection stays open (its writer is held), so a quiet socket
    /// times out rather than closing.
    async fn assert_quiet<R: AsyncRead + Unpin>(
        reader: &mut R,
        decoder: &mut FrameDecoder,
        dur: Duration,
    ) {
        if let Ok(msg) = tokio::time::timeout(dur, read_daemon_message(reader, decoder)).await {
            panic!("unexpected extra frame (nav response leaked across connections?): {msg:?}");
        }
    }

    #[test]
    fn test_nav_stale_gate_supersedes_older_request_of_same_kind() {
        let pos = rift_protocol::Position {
            line: 0,
            character: 0,
        };
        let mut gate = NavStaleGate::default();

        // Nothing recorded yet: any answer is stale — there is no request to match.
        assert!(!gate.is_current(&DaemonMessage::HoverResponse {
            id: NavRequestId(1),
            content: None,
        }));

        // The user hovers, then hovers again before the first answer returns.
        gate.record(&ClientMessage::HoverRequest {
            id: NavRequestId(1),
            path: "a.rs".to_string(),
            position: pos,
        });
        gate.record(&ClientMessage::HoverRequest {
            id: NavRequestId(2),
            path: "a.rs".to_string(),
            position: pos,
        });

        // The superseded id=1 answer is dropped; the latest id=2 answer passes.
        assert!(!gate.is_current(&DaemonMessage::HoverResponse {
            id: NavRequestId(1),
            content: None,
        }));
        assert!(gate.is_current(&DaemonMessage::HoverResponse {
            id: NavRequestId(2),
            content: None,
        }));
    }

    #[test]
    fn test_nav_stale_gate_keys_each_operation_independently() {
        let pos = rift_protocol::Position {
            line: 0,
            character: 0,
        };
        let mut gate = NavStaleGate::default();
        gate.record(&ClientMessage::HoverRequest {
            id: NavRequestId(1),
            path: "a.rs".to_string(),
            position: pos,
        });
        gate.record(&ClientMessage::DefinitionRequest {
            id: NavRequestId(2),
            path: "a.rs".to_string(),
            position: pos,
        });
        gate.record(&ClientMessage::ReferencesRequest {
            id: NavRequestId(3),
            path: "a.rs".to_string(),
            position: pos,
        });
        gate.record(&ClientMessage::DocumentSymbolRequest {
            id: NavRequestId(4),
            path: "a.rs".to_string(),
        });

        // Each operation matches only its own latest id — no cross-talk.
        assert!(gate.is_current(&DaemonMessage::HoverResponse {
            id: NavRequestId(1),
            content: None,
        }));
        assert!(gate.is_current(&DaemonMessage::DefinitionResponse {
            id: NavRequestId(2),
            targets: vec![],
        }));
        assert!(gate.is_current(&DaemonMessage::ReferencesResponse {
            id: NavRequestId(3),
            locations: vec![],
        }));
        assert!(gate.is_current(&DaemonMessage::DocumentSymbolResponse {
            id: NavRequestId(4),
            symbols: vec![],
        }));
        // A hover answer carrying the definition's id does not match hover.
        assert!(!gate.is_current(&DaemonMessage::HoverResponse {
            id: NavRequestId(2),
            content: None,
        }));
        // A document-symbol answer carrying the references' id does not match.
        assert!(!gate.is_current(&DaemonMessage::DocumentSymbolResponse {
            id: NavRequestId(3),
            symbols: vec![],
        }));
    }

    /// Two connections share one dispatch loop and one off-loop LSP worker (the
    /// stable + dev dogfooding case): each connection's hover answer must reach
    /// only its own socket. Before the fix the worker broadcast nav responses, so
    /// both sockets saw both answers — the leak `assert_quiet` catches.
    #[tokio::test]
    async fn test_nav_response_routes_to_requesting_connection_only() {
        let (daemon, handles) = channels(SERVE_EVENT_CAPACITY, SERVE_INBOUND_CAPACITY);
        let _dispatch = tokio::spawn(daemon.run());

        // Fake off-loop LSP worker: echo each hover request's id back on ITS OWN
        // reply channel — the routing the real worker performs, minus a server.
        let (nav_tx, mut nav_rx) = mpsc::channel::<NavRequest>(16);
        let _worker = tokio::spawn(async move {
            while let Some(req) = nav_rx.recv().await {
                if let NavRequest::Hover { id, reply, .. } = req {
                    let _ = reply
                        .send(DaemonMessage::HoverResponse { id, content: None })
                        .await;
                }
            }
        });

        let (client_a, server_a) = tokio::io::duplex(64 * 1024);
        let (mut ca_reader, mut ca_writer) = tokio::io::split(client_a);
        let (sa_reader, sa_writer) = tokio::io::split(server_a);
        let _conn_a = {
            let (inbound, events, state, nav) = (
                handles.inbound.clone(),
                handles.subscribe(),
                handles.state.clone(),
                nav_tx.clone(),
            );
            tokio::spawn(async move {
                serve_connection(
                    sa_reader,
                    sa_writer,
                    inbound,
                    events,
                    state,
                    Some(nav),
                    None,
                    None,
                    None,
                )
                .await
            })
        };

        let (client_b, server_b) = tokio::io::duplex(64 * 1024);
        let (mut cb_reader, mut cb_writer) = tokio::io::split(client_b);
        let (sb_reader, sb_writer) = tokio::io::split(server_b);
        let _conn_b = {
            let (inbound, events, state, nav) = (
                handles.inbound.clone(),
                handles.subscribe(),
                handles.state.clone(),
                nav_tx.clone(),
            );
            tokio::spawn(async move {
                serve_connection(
                    sb_reader,
                    sb_writer,
                    inbound,
                    events,
                    state,
                    Some(nav),
                    None,
                    None,
                    None,
                )
                .await
            })
        };

        let mut da = FrameDecoder::new();
        let mut db = FrameDecoder::new();

        // Handshake both connections.
        ca_writer.write_all(&hello_frame()).await.expect("A Hello");
        ca_writer.flush().await.expect("flush A Hello");
        assert!(matches!(
            read_daemon_message(&mut ca_reader, &mut da).await,
            DaemonMessage::Welcome { .. }
        ));
        cb_writer.write_all(&hello_frame()).await.expect("B Hello");
        cb_writer.flush().await.expect("flush B Hello");
        assert!(matches!(
            read_daemon_message(&mut cb_reader, &mut db).await,
            DaemonMessage::Welcome { .. }
        ));

        // A asks for hover id=100, B for hover id=200.
        ca_writer
            .write_all(&hover_request_frame(100))
            .await
            .expect("A sends hover");
        ca_writer.flush().await.expect("flush A hover");
        cb_writer
            .write_all(&hover_request_frame(200))
            .await
            .expect("B sends hover");
        cb_writer.flush().await.expect("flush B hover");

        // Each connection receives only its own answer, and nothing else.
        assert_eq!(
            read_hover_response_id(&mut ca_reader, &mut da).await,
            NavRequestId(100),
            "connection A must receive its own hover answer"
        );
        assert_eq!(
            read_hover_response_id(&mut cb_reader, &mut db).await,
            NavRequestId(200),
            "connection B must receive its own hover answer"
        );
        assert_quiet(&mut ca_reader, &mut da, Duration::from_millis(300)).await;
        assert_quiet(&mut cb_reader, &mut db, Duration::from_millis(300)).await;
    }

    /// A single connection issues two hovers before the first answer returns; the
    /// gate drops the superseded answer so only the latest reaches the socket —
    /// per-connection drop-stale over the real transport (#482).
    #[tokio::test]
    async fn test_nav_superseded_response_dropped_per_connection() {
        let (daemon, handles) = channels(SERVE_EVENT_CAPACITY, SERVE_INBOUND_CAPACITY);
        let _dispatch = tokio::spawn(daemon.run());

        // Fake worker: wait for BOTH requests before answering, so the connection
        // has recorded id=2 as its latest hover before the stale id=1 answer is
        // even sent. Then answer id=1 (stale) first, then id=2.
        let (nav_tx, mut nav_rx) = mpsc::channel::<NavRequest>(16);
        let _worker = tokio::spawn(async move {
            let first = nav_rx.recv().await.expect("first nav request");
            let second = nav_rx.recv().await.expect("second nav request");
            let (id1, reply1) = match first {
                NavRequest::Hover { id, reply, .. } => (id, reply),
                other => panic!("expected hover, got {other:?}"),
            };
            let (id2, reply2) = match second {
                NavRequest::Hover { id, reply, .. } => (id, reply),
                other => panic!("expected hover, got {other:?}"),
            };
            let _ = reply1
                .send(DaemonMessage::HoverResponse {
                    id: id1,
                    content: None,
                })
                .await;
            let _ = reply2
                .send(DaemonMessage::HoverResponse {
                    id: id2,
                    content: None,
                })
                .await;
        });

        let (client, server) = tokio::io::duplex(64 * 1024);
        let (mut c_reader, mut c_writer) = tokio::io::split(client);
        let (s_reader, s_writer) = tokio::io::split(server);
        let _conn = {
            let (inbound, events, state) = (
                handles.inbound.clone(),
                handles.subscribe(),
                handles.state.clone(),
            );
            tokio::spawn(async move {
                serve_connection(
                    s_reader,
                    s_writer,
                    inbound,
                    events,
                    state,
                    Some(nav_tx),
                    None,
                    None,
                    None,
                )
                .await
            })
        };

        let mut decoder = FrameDecoder::new();
        c_writer
            .write_all(&hello_frame())
            .await
            .expect("send Hello");
        c_writer.flush().await.expect("flush Hello");
        assert!(matches!(
            read_daemon_message(&mut c_reader, &mut decoder).await,
            DaemonMessage::Welcome { .. }
        ));

        c_writer
            .write_all(&hover_request_frame(1))
            .await
            .expect("hover 1");
        c_writer
            .write_all(&hover_request_frame(2))
            .await
            .expect("hover 2");
        c_writer.flush().await.expect("flush hovers");

        // The stale id=1 answer is dropped; only the latest id=2 reaches the wire.
        assert_eq!(
            read_hover_response_id(&mut c_reader, &mut decoder).await,
            NavRequestId(2),
            "only the latest hover answer is written; the superseded one is dropped"
        );
        assert_quiet(&mut c_reader, &mut decoder, Duration::from_millis(300)).await;
    }

    /// The document-symbol pair (#526) must not inherit the #482 broadcast bug:
    /// two connections sharing one dispatch loop and one off-loop LSP worker
    /// each receive only their own `DocumentSymbolResponse`.
    #[tokio::test]
    async fn test_document_symbol_response_routes_to_requesting_connection_only() {
        let (daemon, handles) = channels(SERVE_EVENT_CAPACITY, SERVE_INBOUND_CAPACITY);
        let _dispatch = tokio::spawn(daemon.run());

        // Fake off-loop LSP worker: echo each document-symbol request's id back
        // on ITS OWN reply channel — the routing the real worker performs,
        // minus a server.
        let (nav_tx, mut nav_rx) = mpsc::channel::<NavRequest>(16);
        let _worker = tokio::spawn(async move {
            while let Some(req) = nav_rx.recv().await {
                if let NavRequest::DocumentSymbol { id, reply, .. } = req {
                    let _ = reply
                        .send(DaemonMessage::DocumentSymbolResponse {
                            id,
                            symbols: vec![],
                        })
                        .await;
                }
            }
        });

        let (client_a, server_a) = tokio::io::duplex(64 * 1024);
        let (mut ca_reader, mut ca_writer) = tokio::io::split(client_a);
        let (sa_reader, sa_writer) = tokio::io::split(server_a);
        let _conn_a = {
            let (inbound, events, state, nav) = (
                handles.inbound.clone(),
                handles.subscribe(),
                handles.state.clone(),
                nav_tx.clone(),
            );
            tokio::spawn(async move {
                serve_connection(
                    sa_reader,
                    sa_writer,
                    inbound,
                    events,
                    state,
                    Some(nav),
                    None,
                    None,
                    None,
                )
                .await
            })
        };

        let (client_b, server_b) = tokio::io::duplex(64 * 1024);
        let (mut cb_reader, mut cb_writer) = tokio::io::split(client_b);
        let (sb_reader, sb_writer) = tokio::io::split(server_b);
        let _conn_b = {
            let (inbound, events, state, nav) = (
                handles.inbound.clone(),
                handles.subscribe(),
                handles.state.clone(),
                nav_tx.clone(),
            );
            tokio::spawn(async move {
                serve_connection(
                    sb_reader,
                    sb_writer,
                    inbound,
                    events,
                    state,
                    Some(nav),
                    None,
                    None,
                    None,
                )
                .await
            })
        };

        let mut da = FrameDecoder::new();
        let mut db = FrameDecoder::new();

        // Handshake both connections.
        ca_writer.write_all(&hello_frame()).await.expect("A Hello");
        ca_writer.flush().await.expect("flush A Hello");
        assert!(matches!(
            read_daemon_message(&mut ca_reader, &mut da).await,
            DaemonMessage::Welcome { .. }
        ));
        cb_writer.write_all(&hello_frame()).await.expect("B Hello");
        cb_writer.flush().await.expect("flush B Hello");
        assert!(matches!(
            read_daemon_message(&mut cb_reader, &mut db).await,
            DaemonMessage::Welcome { .. }
        ));

        // A asks for document symbols id=100, B for id=200.
        ca_writer
            .write_all(&document_symbol_request_frame(100))
            .await
            .expect("A sends document_symbol");
        ca_writer.flush().await.expect("flush A document_symbol");
        cb_writer
            .write_all(&document_symbol_request_frame(200))
            .await
            .expect("B sends document_symbol");
        cb_writer.flush().await.expect("flush B document_symbol");

        // Each connection receives only its own answer, and nothing else.
        assert_eq!(
            read_document_symbol_response_id(&mut ca_reader, &mut da).await,
            NavRequestId(100),
            "connection A must receive its own document_symbol answer"
        );
        assert_eq!(
            read_document_symbol_response_id(&mut cb_reader, &mut db).await,
            NavRequestId(200),
            "connection B must receive its own document_symbol answer"
        );
        assert_quiet(&mut ca_reader, &mut da, Duration::from_millis(300)).await;
        assert_quiet(&mut cb_reader, &mut db, Duration::from_millis(300)).await;
    }

    /// Every [`buffer::BufferError`] maps to the matching wire
    /// [`BufferErrorReason`]: `NotUtf8` / `TooLarge` directly, `PathEscape` to the
    /// generic `Io`, and an `Io` refined by its [`std::io::ErrorKind`].
    #[test]
    fn test_buffer_error_reason_maps_each_variant_to_matching_reason() {
        use std::io::{Error, ErrorKind};

        let cases: [(buffer::BufferError, BufferErrorReason); 6] = [
            (
                buffer::BufferError::NotUtf8("f".into()),
                BufferErrorReason::NotUtf8,
            ),
            (
                buffer::BufferError::TooLarge("f".into()),
                BufferErrorReason::TooLarge,
            ),
            (
                buffer::BufferError::PathEscape("f".into()),
                BufferErrorReason::Io,
            ),
            (
                buffer::BufferError::Io {
                    path: "f".into(),
                    source: Error::from(ErrorKind::NotFound),
                },
                BufferErrorReason::NotFound,
            ),
            (
                buffer::BufferError::Io {
                    path: "f".into(),
                    source: Error::from(ErrorKind::PermissionDenied),
                },
                BufferErrorReason::PermissionDenied,
            ),
            (
                buffer::BufferError::Io {
                    path: "f".into(),
                    source: Error::from(ErrorKind::Other),
                },
                BufferErrorReason::Io,
            ),
        ];

        for (err, expected) in cases {
            assert_eq!(buffer_error_reason(&err), expected, "for {err:?}");
        }
    }

    /// A worktree-backed `State` receiver keyed at `root`, for driving
    /// `request_reply` against a real on-disk worktree in tests. The sender is
    /// dropped: `request_reply` only `borrow`s the last value, which the receiver
    /// keeps holding after the sender is gone.
    fn state_rx_for(root: &Path) -> watch::Receiver<State> {
        let snapshot = Snapshot::scan(root).expect("scan worktree root");
        let (_tx, rx) = watch::channel(State {
            worktree: Some(snapshot),
            ..Default::default()
        });
        rx
    }

    /// `request_reply` answers a refused `OpenFile` (missing file) with an
    /// immediate `OpenError` carrying the specific `NotFound` reason, instead of
    /// dropping the request and leaving the editor to time out.
    #[tokio::test]
    async fn test_request_reply_open_missing_file_yields_open_error_not_found() {
        let tmp = TempDir::new("reply-open-missing");
        let rx = state_rx_for(&tmp.path);

        let reply = request_reply(
            &rx,
            ClientMessage::OpenFile {
                path: "does-not-exist.rs".to_string(),
            },
        )
        .await;
        match reply {
            Some(DaemonMessage::OpenError { path, reason }) => {
                assert_eq!(path, "does-not-exist.rs");
                assert_eq!(reason, BufferErrorReason::NotFound);
            }
            other => panic!("expected OpenError NotFound, got {other:?}"),
        }
    }

    /// `request_reply` answers a refused `OpenFile` (non-UTF-8 content) with an
    /// `OpenError { reason: NotUtf8 }`.
    #[tokio::test]
    async fn test_request_reply_open_binary_file_yields_open_error_not_utf8() {
        let tmp = TempDir::new("reply-open-binary");
        std::fs::write(tmp.path.join("blob.bin"), [0x00, 0xff, 0xfe, b'a']).expect("write binary");
        let rx = state_rx_for(&tmp.path);

        let reply = request_reply(
            &rx,
            ClientMessage::OpenFile {
                path: "blob.bin".to_string(),
            },
        )
        .await;
        match reply {
            Some(DaemonMessage::OpenError { path, reason }) => {
                assert_eq!(path, "blob.bin");
                assert_eq!(reason, BufferErrorReason::NotUtf8);
            }
            other => panic!("expected OpenError NotUtf8, got {other:?}"),
        }
    }

    /// `request_reply` answers a refused `SaveFile` (a path escaping the root)
    /// with an immediate `SaveError` rather than dropping it — a path escape
    /// collapses to the generic `Io` reason.
    #[tokio::test]
    async fn test_request_reply_save_path_escape_yields_save_error_io() {
        let tmp = TempDir::new("reply-save-escape");
        let rx = state_rx_for(&tmp.path);

        let reply = request_reply(
            &rx,
            ClientMessage::SaveFile {
                path: "../escape.txt".to_string(),
                content: "should not land".to_string(),
                base_mtime: std::time::SystemTime::UNIX_EPOCH,
            },
        )
        .await;
        match reply {
            Some(DaemonMessage::SaveError { path, reason }) => {
                assert_eq!(path, "../escape.txt");
                assert_eq!(reason, BufferErrorReason::Io);
            }
            other => panic!("expected SaveError Io, got {other:?}"),
        }
    }

    // ── ContextMap: per-root, reference-counted context registry (#736) ─────

    /// Two distinct roots acquire two independent contexts — neither their
    /// `inbound` channels nor their `State`s are the same underlying loop.
    #[tokio::test]
    async fn test_context_map_acquire_distinct_roots_yields_independent_contexts() {
        let tmp_a = TempDir::new("ctxmap-distinct-a");
        let tmp_b = TempDir::new("ctxmap-distinct-b");
        let map = ContextMap::new();

        let ctx_a = map.acquire(tmp_a.path.clone()).await;
        let ctx_b = map.acquire(tmp_b.path.clone()).await;
        assert!(
            !ctx_a.handles.inbound.same_channel(&ctx_b.handles.inbound),
            "distinct roots must not share a dispatch loop"
        );

        drop(ctx_a);
        drop(ctx_b);
        map.release(&tmp_a.path).await;
        map.release(&tmp_b.path).await;
    }

    /// A second acquire for an already-live root shares the SAME context (same
    /// `inbound` channel, and a state change driven through one acquirer's
    /// handle is observable through the other's), stays alive while a
    /// reference remains after one release, and only tears down at the last.
    #[tokio::test]
    async fn test_context_map_acquire_same_root_twice_shares_one_context() {
        let tmp = TempDir::new("ctxmap-shared");
        write_file(&tmp.path.join("tracked.txt"), "x");
        let map = ContextMap::new();

        let ctx_1 = map.acquire(tmp.path.clone()).await;
        let ctx_2 = map.acquire(tmp.path.clone()).await;
        assert!(
            ctx_1.handles.inbound.same_channel(&ctx_2.handles.inbound),
            "a second acquire for the same root must share the first's dispatch loop"
        );

        // Functional proof, not just channel identity: the initial scan driven
        // by `ctx_1`'s acquire lands in `ctx_2`'s own `State` view too.
        let mut state_2 = ctx_2.handles.state.clone();
        loop {
            if state_2.borrow_and_update().worktree.is_some() {
                break;
            }
            state_2.changed().await.expect("state sender alive");
        }

        drop(ctx_1);
        map.release(&tmp.path).await; // refcount 2 -> 1: `ctx_2` still holds it.
        assert!(
            state_2.has_changed().is_ok(),
            "the context must stay alive while a reference (ctx_2) remains"
        );

        drop(ctx_2);
        map.release(&tmp.path).await; // refcount 1 -> 0: torn down.
    }

    /// The last release tears the context down: its `State` sender (owned by
    /// the dispatch loop's `Core`) is dropped once every acquirer's handle and
    /// the registry's own are gone, which is exactly what the last `release`
    /// awaits before returning.
    #[tokio::test]
    async fn test_context_map_release_to_zero_tears_down_context() {
        let tmp = TempDir::new("ctxmap-teardown");
        let map = ContextMap::new();

        let ctx = map.acquire(tmp.path.clone()).await;
        let mut state = ctx.handles.state.clone();
        drop(ctx);

        map.release(&tmp.path).await;

        let result = state.changed().await;
        assert!(
            result.is_err(),
            "the context's State sender must be dropped once torn down"
        );
    }

    /// Releasing a root with no live entry (never acquired, or already fully
    /// released) is a no-op rather than a panic.
    #[tokio::test]
    async fn test_context_map_release_unknown_root_is_noop() {
        let tmp = TempDir::new("ctxmap-unknown");
        let map = ContextMap::new();
        map.release(&tmp.path).await;
    }

    // ── ContextMap: the #736-review atomicity fix (#737) ────────────────────

    /// Reacquiring a root whose sole reference was already fully released
    /// (torn down) must build a brand new dispatch loop from scratch, never
    /// resurrect the dead one — the "teardown-then-reacquire" case the #736
    /// review named directly.
    #[tokio::test]
    async fn test_context_map_reacquire_after_full_teardown_builds_a_fresh_working_context() {
        let tmp = TempDir::new("ctxmap-fresh");
        write_file(&tmp.path.join("tracked.txt"), "x");
        let map = ContextMap::new();

        let ctx_1 = map.acquire(tmp.path.clone()).await;
        let mut state_1 = ctx_1.handles.state.clone();
        drop(ctx_1);
        map.release(&tmp.path).await;
        assert!(
            state_1.changed().await.is_err(),
            "the first context must be fully torn down before the reacquire below"
        );

        let ctx_2 = map.acquire(tmp.path.clone()).await;
        let mut state_2 = ctx_2.handles.state.clone();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if state_2.borrow_and_update().worktree.is_some() {
                    break;
                }
                state_2
                    .changed()
                    .await
                    .expect("the fresh context's state sender is alive");
            }
        })
        .await
        .expect("the fresh context completes its own scan within the timeout");

        drop(ctx_2);
        map.release(&tmp.path).await;
    }

    /// The atomicity fix itself: the last release tears its context down
    /// WHILE HOLDING the registry lock, so a concurrent `acquire` — for this
    /// root or any other, since the lock is registry-wide — cannot observe
    /// the in-between state where the old entry is gone but its dispatch loop
    /// has not yet exited (the exact RAM-duplication race the #736 review
    /// surfaced). Proven directly by holding the lock ourselves — standing in
    /// for an in-flight last-release teardown — and asserting a concurrent
    /// `acquire` blocks until it is released.
    #[tokio::test]
    async fn test_context_map_acquire_blocks_while_the_registry_lock_is_held() {
        let map = Arc::new(ContextMap::new());
        let guard = map.entries.lock().await;

        let tmp = TempDir::new("ctxmap-lockwait");
        let root = tmp.path.clone();
        let map_task = Arc::clone(&map);
        let acquire_task = tokio::spawn(async move { map_task.acquire(root).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !acquire_task.is_finished(),
            "acquire must block while a release's in-flight teardown holds the registry lock"
        );

        drop(guard);
        let ctx = tokio::time::timeout(Duration::from_secs(5), acquire_task)
            .await
            .expect("acquire completes once the lock is released")
            .expect("acquire task joins");

        drop(ctx);
        map.release(&tmp.path).await;
    }

    /// The atomicity fix through REAL concurrent acquirers and a REAL
    /// `Daemon::run()` join (N3, #737 review) — not the lock-held stand-in
    /// above: many tasks race acquire/drop/release cycles on ONE root
    /// concurrently, repeatedly driving it to zero and back up. Every
    /// interleaving must converge without a hang or a panic, and must leave
    /// the registry USABLE afterward — a fresh acquire still completes its
    /// own scan — which a corrupted refcount or a resurrected/duplicated
    /// context (the RAM-duplication race the #736 review surfaced) would
    /// have broken.
    #[tokio::test]
    async fn test_context_map_concurrent_acquirers_racing_to_zero_converge_without_duplication() {
        let tmp = TempDir::new("ctxmap-race-real");
        write_file(&tmp.path.join("tracked.txt"), "x");
        let map = Arc::new(ContextMap::new());

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let map = Arc::clone(&map);
            let root = tmp.path.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..20 {
                    let ctx = map.acquire(root.clone()).await;
                    drop(ctx);
                    map.release(&root).await;
                }
            }));
        }
        for task in tasks {
            tokio::time::timeout(Duration::from_secs(30), task)
                .await
                .expect("concurrent acquire/release cycles must not hang")
                .expect("acquirer task joins without panicking");
        }

        // The registry must be left USABLE: a fresh acquire completes its own
        // scan, proving no corrupted refcount or wedged entry survived the race.
        let ctx = map.acquire(tmp.path.clone()).await;
        let mut state = ctx.handles.state.clone();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if state.borrow_and_update().worktree.is_some() {
                    break;
                }
                state.changed().await.expect("state sender alive");
            }
        })
        .await
        .expect("final acquire completes its own scan");

        drop(ctx);
        map.release(&tmp.path).await;
    }

    // ── keep_warm_supervisor: the release grace timer (#551) ─────────────────

    /// A short grace so these tests do not wait real seconds — the production
    /// [`KEEP_WARM_RELEASE_GRACE`] is minutes, `keep_warm_supervisor` takes it
    /// as a plain parameter for exactly this reason.
    const TEST_KEEP_WARM_GRACE: Duration = Duration::from_millis(30);

    /// Drive one `Connected` round-trip through a running supervisor and
    /// return the `Context` it handed back.
    async fn connect(events: &mpsc::Sender<KeepWarmEvent>) -> Context {
        let (reply_tx, mut reply_rx) = mpsc::channel(1);
        events
            .send(KeepWarmEvent::Connected(reply_tx))
            .await
            .expect("supervisor task alive");
        reply_rx
            .recv()
            .await
            .expect("supervisor replies with a context")
    }

    /// Loop `changed()` until it errors (the `State` sender dropped), so an
    /// unrelated intervening update — e.g. the context's own worktree scan
    /// completing while the test waits out the grace timer — is not mistaken
    /// for the teardown this waits for. Bounded by the caller's own
    /// `tokio::time::timeout`.
    async fn wait_for_state_closed(state: &mut watch::Receiver<State>) {
        loop {
            if state.changed().await.is_err() {
                return;
            }
        }
    }

    /// The last client disconnecting, followed by the grace window fully
    /// elapsing with no reconnect, releases the keep-warm context — its
    /// `State` sender is dropped, exactly [`ContextMap::release`]'s own
    /// teardown signal (mirroring `test_context_map_release_to_zero_tears_down_context`).
    #[tokio::test]
    async fn test_keep_warm_supervisor_last_disconnect_then_grace_expiry_releases_context() {
        let tmp = TempDir::new("keepwarm-release");
        let map = Arc::new(ContextMap::new());
        let initial = map.acquire(tmp.path.clone()).await;
        let (events_tx, events_rx) = mpsc::channel(KEEP_WARM_EVENT_CAPACITY);
        tokio::spawn(keep_warm_supervisor(
            Arc::clone(&map),
            tmp.path.clone(),
            initial,
            TEST_KEEP_WARM_GRACE,
            events_rx,
        ));

        let context = connect(&events_tx).await;
        let mut state = context.handles.state.clone();
        drop(context);
        events_tx
            .send(KeepWarmEvent::Disconnected)
            .await
            .expect("supervisor task alive");

        tokio::time::timeout(Duration::from_secs(5), wait_for_state_closed(&mut state))
            .await
            .expect(
                "the grace expiring with no reconnect must release the context \
                 (its State sender dropped) within the timeout",
            );
    }

    /// A disconnect immediately followed by a reconnect WITHIN the grace
    /// window keeps the context alive: the supervisor hands back the SAME
    /// context (no re-acquire), and it survives well past the original grace
    /// deadline.
    #[tokio::test]
    async fn test_keep_warm_supervisor_reconnect_within_grace_keeps_context_alive() {
        let tmp = TempDir::new("keepwarm-reconnect");
        let map = Arc::new(ContextMap::new());
        let initial = map.acquire(tmp.path.clone()).await;
        let (events_tx, events_rx) = mpsc::channel(KEEP_WARM_EVENT_CAPACITY);
        tokio::spawn(keep_warm_supervisor(
            Arc::clone(&map),
            tmp.path.clone(),
            initial,
            TEST_KEEP_WARM_GRACE,
            events_rx,
        ));

        let first = connect(&events_tx).await;
        let inbound_before = first.handles.inbound.clone();
        drop(first);
        events_tx
            .send(KeepWarmEvent::Disconnected)
            .await
            .expect("supervisor task alive");

        // Reconnect immediately — well inside `TEST_KEEP_WARM_GRACE`.
        let second = connect(&events_tx).await;
        assert!(
            second.handles.inbound.same_channel(&inbound_before),
            "a reconnect within the grace window must reuse the SAME context, \
             not re-acquire a fresh one"
        );
        let state = second.handles.state.clone();

        // Outlive the original grace deadline: the timer must have been
        // canceled by the reconnect above, so the context stays live.
        tokio::time::sleep(TEST_KEEP_WARM_GRACE * 4).await;
        assert!(
            state.has_changed().is_ok(),
            "the context must survive past the original grace deadline once \
             a reconnect canceled the pending release"
        );

        drop(second);
        events_tx
            .send(KeepWarmEvent::Disconnected)
            .await
            .expect("supervisor task alive");
    }

    /// A connection arriving after the grace already released the context
    /// gets a freshly re-acquired one (a working reactive layer again),
    /// distinct from the one that was released.
    #[tokio::test]
    async fn test_keep_warm_supervisor_connect_after_release_reacquires_a_fresh_context() {
        let tmp = TempDir::new("keepwarm-reacquire");
        let map = Arc::new(ContextMap::new());
        let initial = map.acquire(tmp.path.clone()).await;
        let (events_tx, events_rx) = mpsc::channel(KEEP_WARM_EVENT_CAPACITY);
        tokio::spawn(keep_warm_supervisor(
            Arc::clone(&map),
            tmp.path.clone(),
            initial,
            TEST_KEEP_WARM_GRACE,
            events_rx,
        ));

        let first = connect(&events_tx).await;
        // Only `state_before` (a `watch::Receiver`, which never keeps a
        // channel open the way an `mpsc::Sender` clone would) survives past
        // the drop below, so nothing here holds up the release this test
        // waits for next.
        let mut state_before = first.handles.state.clone();
        drop(first);
        events_tx
            .send(KeepWarmEvent::Disconnected)
            .await
            .expect("supervisor task alive");

        // Wait out the release, proven the same way as the release test above.
        tokio::time::timeout(
            Duration::from_secs(5),
            wait_for_state_closed(&mut state_before),
        )
        .await
        .expect("the first context must be released before the reconnect below");

        // A fresh, live context — not the released one (its own scan
        // completing is exercised generically by
        // `test_context_map_reacquire_after_full_teardown_builds_a_fresh_working_context`).
        let second = connect(&events_tx).await;
        assert!(
            !second.handles.state.same_channel(&state_before),
            "a connection after the release must get a freshly re-acquired \
             context, not the torn-down one"
        );
        assert!(
            second.handles.state.has_changed().is_ok(),
            "the re-acquired context must be alive (State sender not closed)"
        );

        drop(second);
        events_tx
            .send(KeepWarmEvent::Disconnected)
            .await
            .expect("supervisor task alive");
    }

    // ── reroot_connection: the Attach seam (#737) ────────────────────────────

    /// A placeholder reactive binding to seed `reroot_connection`'s `&mut`
    /// out-params with before any root has ever been resolved — mirrors
    /// `serve_connection`'s own pre-`Attach` state, no `ContextMap` entry.
    fn placeholder_bindings() -> (
        mpsc::Sender<ClientMessage>,
        broadcast::Receiver<DaemonMessage>,
        watch::Receiver<State>,
        Option<mpsc::Sender<NavRequest>>,
    ) {
        let placeholder = standalone_context();
        let inbound = placeholder.handles.inbound.clone();
        let events = placeholder.handles.subscribe();
        let state = placeholder.handles.state.clone();
        let nav_requests = placeholder.nav_requests.clone();
        drop(placeholder);
        (inbound, events, state, nav_requests)
    }

    /// The first `Attach` resolution in a connection's lifetime has no
    /// previous self-acquired root to release: it only acquires, and the
    /// rebound `state` observes the newly acquired context's own scan.
    #[tokio::test]
    async fn test_reroot_connection_first_attach_only_acquires() {
        let tmp = TempDir::new("reroot-first");
        let map = ContextMap::new();
        let (mut inbound, mut events, mut state, mut nav_requests) = placeholder_bindings();
        let mut self_root: Option<PathBuf> = None;
        let mut open_buffers: HashSet<String> = HashSet::new();

        let rerooted = tokio::time::timeout(
            Duration::from_secs(5),
            reroot_connection(
                &map,
                tmp.path.clone(),
                &mut self_root,
                &mut inbound,
                &mut events,
                &mut state,
                &mut nav_requests,
                &mut open_buffers,
            ),
        )
        .await
        .expect("reroot_connection completes within the timeout");
        assert!(
            rerooted,
            "the first attach resolution must perform a re-root"
        );

        let expected = std::fs::canonicalize(&tmp.path).expect("canonicalize temp root");
        assert_eq!(self_root, Some(expected.clone()));
        assert!(
            state.changed().await.is_ok(),
            "state must now be bound to the freshly acquired context, not the placeholder"
        );

        drop(inbound);
        drop(events);
        drop(state);
        drop(nav_requests);
        map.release(&expected).await;
    }

    /// A switch to a different root acquires the new one BEFORE releasing
    /// the previous self-acquired one (#736's "drop the old context before
    /// the final release" contract) — this connection's only reference to
    /// root A is torn down cleanly, without hanging on its own still-held
    /// handles, and `self_root` lands on root B.
    #[tokio::test]
    async fn test_reroot_connection_switch_tears_down_the_previous_sole_reference() {
        let tmp_a = TempDir::new("reroot-switch-a");
        let tmp_b = TempDir::new("reroot-switch-b");
        let map = ContextMap::new();
        let (mut inbound, mut events, mut state, mut nav_requests) = placeholder_bindings();
        let mut self_root: Option<PathBuf> = None;
        let mut open_buffers: HashSet<String> = HashSet::new();

        let first = reroot_connection(
            &map,
            tmp_a.path.clone(),
            &mut self_root,
            &mut inbound,
            &mut events,
            &mut state,
            &mut nav_requests,
            &mut open_buffers,
        )
        .await;
        assert!(first, "the first attach resolution must perform a re-root");
        let mut state_a = state.clone();

        let switched = tokio::time::timeout(
            Duration::from_secs(5),
            reroot_connection(
                &map,
                tmp_b.path.clone(),
                &mut self_root,
                &mut inbound,
                &mut events,
                &mut state,
                &mut nav_requests,
                &mut open_buffers,
            ),
        )
        .await
        .expect("switching roots must not hang releasing the previous sole reference");
        assert!(
            switched,
            "switching to a DIFFERENT root must perform a re-root"
        );

        let expected_b = std::fs::canonicalize(&tmp_b.path).expect("canonicalize root b");
        assert_eq!(self_root, Some(expected_b.clone()));

        let torn_down = tokio::time::timeout(Duration::from_secs(5), state_a.changed())
            .await
            .expect("root A's context must be torn down within the timeout");
        assert!(
            torn_down.is_err(),
            "switching away from the sole reference to root A must tear its context down"
        );

        drop(inbound);
        drop(events);
        drop(state);
        drop(nav_requests);
        map.release(&expected_b).await;
    }

    /// N2 (#737 review): re-resolving the SAME root already active for this
    /// connection (a same-session reconnect, or a redundant `Attach`) is a
    /// no-op — no acquire(+1)/release(-1) churn on the map, and `inbound`/
    /// `events`/`state`/`nav_requests` are left untouched (still the SAME
    /// bindings, not merely equal ones) so the caller can skip the
    /// re-snapshot too. #738: a same-root no-op is not a real re-root, so a
    /// buffer still tracked open must NOT be detached either.
    #[tokio::test]
    async fn test_reroot_connection_same_root_again_is_a_noop() {
        let tmp = TempDir::new("reroot-noop");
        let map = ContextMap::new();
        let (mut inbound, mut events, mut state, mut nav_requests) = placeholder_bindings();
        let mut self_root: Option<PathBuf> = None;
        let mut open_buffers: HashSet<String> = HashSet::new();

        let first = reroot_connection(
            &map,
            tmp.path.clone(),
            &mut self_root,
            &mut inbound,
            &mut events,
            &mut state,
            &mut nav_requests,
            &mut open_buffers,
        )
        .await;
        assert!(first);
        let bound_inbound = inbound.clone();
        open_buffers.insert("src/main.rs".to_string());

        // Re-resolve the identical root, spelled slightly differently (a
        // trailing slash) so a real bug could not hide behind pointer/string
        // equality on the raw path.
        let mut trailing_slash = tmp.path.clone().into_os_string();
        trailing_slash.push("/");
        let again = reroot_connection(
            &map,
            PathBuf::from(trailing_slash),
            &mut self_root,
            &mut inbound,
            &mut events,
            &mut state,
            &mut nav_requests,
            &mut open_buffers,
        )
        .await;

        assert!(!again, "re-resolving the same root must be a no-op");
        assert!(
            inbound.same_channel(&bound_inbound),
            "a no-op re-root must leave the bound handles untouched"
        );
        assert!(
            open_buffers.contains("src/main.rs"),
            "a same-root no-op must not detach buffers still open on the unchanged root"
        );

        drop(inbound);
        drop(events);
        drop(state);
        drop(nav_requests);
        drop(bound_inbound);
        map.release(&self_root.expect("self_root set")).await;
    }

    /// A non-canonical spelling of an already-live root (here, a trailing
    /// `/.`) must key the SAME context as the canonical spelling, never build
    /// a second one — the map-key canonicalization requirement the #736
    /// review raised.
    #[tokio::test]
    async fn test_reroot_connection_canonicalizes_the_key_so_two_spellings_share_one_context() {
        let tmp = TempDir::new("reroot-canon");
        let odd_spelling = tmp.path.join(".");
        let expected = std::fs::canonicalize(&tmp.path).expect("canonicalize temp root");

        let map = ContextMap::new();
        let witness = map.acquire(expected.clone()).await;
        let witness_inbound = witness.handles.inbound.clone();
        drop(witness);

        let (mut inbound, mut events, mut state, mut nav_requests) = placeholder_bindings();
        let mut self_root: Option<PathBuf> = None;
        let mut open_buffers: HashSet<String> = HashSet::new();
        reroot_connection(
            &map,
            odd_spelling,
            &mut self_root,
            &mut inbound,
            &mut events,
            &mut state,
            &mut nav_requests,
            &mut open_buffers,
        )
        .await;

        assert_eq!(self_root, Some(expected.clone()));
        assert!(
            inbound.same_channel(&witness_inbound),
            "a non-canonical spelling of an already-live root must share its context, never build a second one"
        );

        drop(inbound);
        drop(events);
        drop(state);
        drop(nav_requests);
        drop(witness_inbound);
        map.release(&expected).await; // the witness's reference
        map.release(&expected).await; // this connection's own reference
    }

    /// B1 (#737 review, BLOCKING): the keep-warm/default context —
    /// `serve`/`serve_uds`'s own up-front acquire, simulated here via the
    /// SAME `canonicalize_root` helper they now call before `acquire` — and
    /// a later `Attach`-driven re-root to the SAME directory must share ONE
    /// context even when the two are spelled differently (here: a
    /// trailing-slash `--root` vs. the plain path `Attach` resolves).
    /// Before this fix, the keep-warm acquire keyed the map with the RAW
    /// `--root` while `reroot_connection` keyed it with the canonicalized
    /// resolved root, so an uncanonical `--root` built TWO live contexts —
    /// two independent `LspWorker`s, two rust-analyzers — for one project;
    /// this is the OOM `docs/spec-per-session-project-root.md`'s per-root
    /// context map exists to prevent.
    #[tokio::test]
    async fn test_keep_warm_acquire_and_reroot_share_one_context_for_a_trailing_slash_root() {
        let tmp = TempDir::new("ctxmap-keepwarm-trailing-slash");
        let mut trailing_slash = tmp.path.clone().into_os_string();
        trailing_slash.push("/");
        let raw_root_with_trailing_slash = PathBuf::from(trailing_slash);

        let map = ContextMap::new();

        // The keep-warm acquire, exactly as `serve`/`serve_uds` now perform
        // it: canonicalize THEN acquire.
        let keep_warm_key = canonicalize_root(raw_root_with_trailing_slash).await;
        let keep_warm = map.acquire(keep_warm_key.clone()).await;
        let keep_warm_inbound = keep_warm.handles.inbound.clone();
        drop(keep_warm);

        // A later Attach resolves the SAME directory, spelled plainly.
        let (mut inbound, mut events, mut state, mut nav_requests) = placeholder_bindings();
        let mut self_root: Option<PathBuf> = None;
        let mut open_buffers: HashSet<String> = HashSet::new();
        let rerooted = reroot_connection(
            &map,
            tmp.path.clone(),
            &mut self_root,
            &mut inbound,
            &mut events,
            &mut state,
            &mut nav_requests,
            &mut open_buffers,
        )
        .await;
        assert!(
            rerooted,
            "the first Attach resolution must perform a re-root"
        );

        assert_eq!(self_root, Some(keep_warm_key.clone()));
        assert!(
            inbound.same_channel(&keep_warm_inbound),
            "the keep-warm context and a re-root to the same (differently spelled) \
             directory must share ONE context, never build a second one"
        );

        drop(inbound);
        drop(events);
        drop(state);
        drop(nav_requests);
        drop(keep_warm_inbound);
        map.release(&keep_warm_key).await; // this connection's own reference
        map.release(&keep_warm_key).await; // the keep-warm reference
    }

    // ── detach_open_buffers / reroot_connection buffer detach (#738) ─────────

    /// `detach_open_buffers` sends one `BufferClosed` per tracked path over
    /// the given `inbound` and drains the set — the unit-level behavior
    /// `reroot_connection` relies on for cross-project write safety.
    #[tokio::test]
    async fn test_detach_open_buffers_sends_buffer_closed_for_each_path_and_clears_the_set() {
        let (inbound, mut rx) = mpsc::channel::<ClientMessage>(4);
        let mut open_buffers: HashSet<String> =
            ["src/main.rs".to_string(), "Cargo.toml".to_string()]
                .into_iter()
                .collect();

        detach_open_buffers(&inbound, &mut open_buffers).await;

        assert!(
            open_buffers.is_empty(),
            "every tracked path must be drained from the set"
        );

        let mut closed: HashSet<String> = HashSet::new();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ClientMessage::BufferClosed { path } => {
                    closed.insert(path);
                }
                other => panic!("expected only BufferClosed, got {other:?}"),
            }
        }
        assert_eq!(
            closed,
            ["src/main.rs".to_string(), "Cargo.toml".to_string()]
                .into_iter()
                .collect::<HashSet<_>>()
        );
    }

    /// An empty tracking set sends nothing — the common case of a re-root
    /// with no editor buffers open.
    #[tokio::test]
    async fn test_detach_open_buffers_empty_set_sends_nothing() {
        let (inbound, mut rx) = mpsc::channel::<ClientMessage>(4);
        let mut open_buffers: HashSet<String> = HashSet::new();

        detach_open_buffers(&inbound, &mut open_buffers).await;

        assert!(
            rx.try_recv().is_err(),
            "no buffers were open, so nothing should be sent"
        );
    }

    /// End-to-end through `reroot_connection`: a connection with paths
    /// tracked open against root A must have a `BufferClosed` synthesized on
    /// root A's `inbound` for each of them, BEFORE `inbound` is rebound to
    /// root B — and `open_buffers` must come out empty, so a subsequent
    /// `BufferChanged`/`SaveFile` starts clean against the new root.
    ///
    /// Root A's `inbound` here is a bare, unconsumed `mpsc` channel (not a
    /// real `ContextMap`-acquired context) specifically so the synthetic
    /// closes are directly observable instead of being silently absorbed by
    /// a running dispatch loop.
    #[tokio::test]
    async fn test_reroot_connection_switch_detaches_previously_open_buffers_on_the_old_root() {
        let tmp_b = TempDir::new("reroot-detach-buffers-b");
        let map = ContextMap::new();

        let (mut inbound, mut old_inbound_rx) = mpsc::channel::<ClientMessage>(8);
        let (_events_tx, events_rx) = broadcast::channel::<DaemonMessage>(4);
        let mut events = events_rx;
        let (_state_tx, mut state) = watch::channel(State::default());
        let mut nav_requests: Option<mpsc::Sender<NavRequest>> = None;
        let mut self_root =
            Some(std::env::temp_dir().join("reroot-detach-buffers-root-a-stand-in"));
        let mut open_buffers: HashSet<String> =
            ["src/main.rs".to_string(), "Cargo.toml".to_string()]
                .into_iter()
                .collect();

        let switched = reroot_connection(
            &map,
            tmp_b.path.clone(),
            &mut self_root,
            &mut inbound,
            &mut events,
            &mut state,
            &mut nav_requests,
            &mut open_buffers,
        )
        .await;

        assert!(
            switched,
            "switching to a different root must perform a re-root"
        );
        assert!(
            open_buffers.is_empty(),
            "the previously-open buffers must be cleared on an actual re-root"
        );

        let mut closed: HashSet<String> = HashSet::new();
        while let Ok(msg) = old_inbound_rx.try_recv() {
            match msg {
                ClientMessage::BufferClosed { path } => {
                    closed.insert(path);
                }
                other => {
                    panic!("expected only BufferClosed on the OLD root's inbound, got {other:?}")
                }
            }
        }
        assert_eq!(
            closed,
            ["src/main.rs".to_string(), "Cargo.toml".to_string()]
                .into_iter()
                .collect::<HashSet<_>>(),
            "the OLD root's inbound must receive a BufferClosed for every previously-open path"
        );

        let expected_b = std::fs::canonicalize(&tmp_b.path).expect("canonicalize root b");
        assert_eq!(self_root, Some(expected_b.clone()));

        drop(inbound);
        drop(events);
        drop(state);
        drop(nav_requests);
        map.release(&expected_b).await;
    }
}
