use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod lsp;
mod terminal;

use lsp::{document_changes, LspDiagnostics, LspWorker};
use rift_explorer::{Change, Entry, GitStatus, Snapshot, Watcher};
use rift_lsp::{DocumentChange, DocumentSelector};
use rift_protocol::{
    encode_frame, ClientMessage, DaemonMessage, Diagnostic, EntryKind, FrameDecoder, WorktreeEntry,
    PROTOCOL_VERSION,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch};

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
}

/// Sender handles for driving a [`Daemon`].
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
        core: Core {
            events: events_tx.clone(),
            state: state_tx,
            doc_changes: None,
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
fn worktree_worker(root: PathBuf, events: mpsc::Sender<WorktreeEvent>) {
    let snapshot = match Snapshot::scan(&root) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            eprintln!(
                "rift-daemon worktree scan of {} failed: {err}",
                root.display()
            );
            return;
        }
    };

    // Compute the initial git status before arming the watcher. A successful
    // compute means the root is a git repository, so enable git watching
    // (`with_git_status`) — the `.git/` whitelist plus a recompute per flush. An
    // error (not a repo, or git unreadable) degrades to worktree-only watching
    // (`Watcher::new`), so a non-repo root still streams its file tree without
    // spamming per-flush git errors.
    //
    // The watcher is armed BEFORE the snapshot/status is delivered: it registers
    // its watch set synchronously, so once a consumer has observed the initial
    // state, any later change is guaranteed to produce an event. The reverse
    // order races — a write right after delivery would precede the watches and,
    // with no event, the rescan-on-event watcher would never surface it. The
    // clone is the two-owner boundary: the watcher keeps the diff baseline, the
    // dispatch loop's `State` gets its own copy.
    match GitStatus::compute(&root) {
        Ok(initial_git) => {
            let (_watcher, changes, git_rx) = match Watcher::with_git_status(snapshot.clone()) {
                Ok(triple) => triple,
                Err(err) => {
                    eprintln!("rift-daemon worktree watch failed: {err}");
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
            relay_events(&changes, Some(&git_rx), &events);
        }
        Err(err) => {
            eprintln!(
                "rift-daemon: no git status for {} ({err}); worktree-only",
                root.display()
            );
            let (_watcher, changes) = match Watcher::new(snapshot.clone()) {
                Ok(pair) => pair,
                Err(err) => {
                    eprintln!("rift-daemon worktree watch failed: {err}");
                    return;
                }
            };
            if events
                .blocking_send(WorktreeEvent::Scanned(snapshot))
                .is_err()
            {
                return;
            }
            relay_events(&changes, None, &events);
        }
    }
}

/// Relay the watcher's change batches (and, in git mode, git-status recomputes)
/// into the dispatch loop until a channel closes or the loop is gone.
///
/// In git mode the git channel is the primary wait: `with_git_status` emits a
/// recompute on *every* flush, while worktree changes are emitted only on a
/// non-empty diff and always *before* the flush's git tick. So blocking on the
/// git tick and then draining the worktree changes already queued preserves the
/// order (tree update before its git decoration). Without git, this is the
/// original worktree-only relay.
fn relay_events(
    changes: &std::sync::mpsc::Receiver<Vec<Change>>,
    git: Option<&std::sync::mpsc::Receiver<GitStatus>>,
    events: &mpsc::Sender<WorktreeEvent>,
) {
    use std::sync::mpsc::RecvTimeoutError;
    loop {
        match git {
            Some(git_rx) => match git_rx.recv_timeout(WORKTREE_IDLE_POLL) {
                Ok(status) => {
                    // Drain the worktree changes this flush queued before its
                    // git tick, so the tree update precedes the git decoration.
                    while let Ok(batch) = changes.try_recv() {
                        if events.blocking_send(WorktreeEvent::Changed(batch)).is_err() {
                            return;
                        }
                    }
                    if events
                        .blocking_send(WorktreeEvent::GitRecomputed(status))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if events.is_closed() {
                        return;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match changes.recv_timeout(WORKTREE_IDLE_POLL) {
                Ok(batch) => {
                    if events.blocking_send(WorktreeEvent::Changed(batch)).is_err() {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if events.is_closed() {
                        return;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            },
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

/// Build the `RepoState` message from an explorer repo state.
fn repo_state_message(repo: &rift_explorer::RepoState) -> DaemonMessage {
    DaemonMessage::RepoState {
        branch: repo.branch.clone(),
        ahead_behind: repo.ahead_behind.map(|ab| rift_protocol::AheadBehind {
            ahead: ab.ahead,
            behind: ab.behind,
        }),
    }
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

/// Serve one client connection against an already-running dispatch loop.
///
/// Decodes [`ClientMessage`] frames from `reader` into the loop via `inbound`,
/// writes [`DaemonMessage`] frames from `events` to `writer`, and replays the
/// current worktree snapshot from `state` straight to this connection right
/// after the handshake. One call drives one connection; the dispatch loop and
/// the `State` it owns live outside it, so they persist across reconnects — the
/// reattach contract.
///
/// The snapshot is delivered per connection — backpressured by the socket —
/// rather than over the shared `events` bus, whose bounded backlog silently
/// drops chunks once a large snapshot exceeds its capacity (issue #227). The
/// bus carries only incremental `UpdateWorktree`s; a lagging writer may still
/// drop those, but that loss is logged, never silent.
///
/// `inbound` is a clone of the loop's inbound sender (dropped when the
/// connection ends); `events` is a fresh subscription to the outbound bus;
/// `state` observes the latest worktree snapshot. Returns once the reader
/// reaches EOF, the dispatch loop is gone, or the event bus closes.
async fn serve_connection<R, W>(
    reader: R,
    mut writer: W,
    inbound: mpsc::Sender<ClientMessage>,
    mut events: broadcast::Receiver<DaemonMessage>,
    mut state: watch::Receiver<State>,
    tmux_server: Option<String>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = reader;
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0u8; SERVE_READ_BUFFER];
    // Per-connection replay bookkeeping: the snapshot is sent once, right behind
    // the `Welcome`. `handshaken` gates the `state` branch so a snapshot landing
    // mid-scan is never written ahead of the handshake the client waits for.
    let mut handshaken = false;
    let mut snapshot_sent = false;

    // This connection's own tmux attach: terminal `ClientMessage`s are routed to
    // a dedicated `terminal_task` (each client gets its own `tmux -C` child), and
    // its outbound events are multiplexed onto this socket alongside the shared
    // worktree/git stream. Keeping the terminal path per connection is what gives
    // each rift client an independent attach (per-client size, flow control).
    let (terminal_in_tx, terminal_in_rx) = mpsc::channel(TERMINAL_INBOUND_CAPACITY);
    let (terminal_out_tx, mut terminal_out_rx) = mpsc::channel(TERMINAL_OUTBOUND_CAPACITY);
    let terminal = tokio::spawn(terminal::terminal_task(
        terminal_in_rx,
        terminal_out_tx,
        tmux_server,
    ));
    let mut terminal_done = false;

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
                    // everything else (the handshake) goes to the shared loop.
                    match msg {
                        ClientMessage::Attach { .. }
                        | ClientMessage::Input { .. }
                        | ClientMessage::ResizePane { .. }
                        | ClientMessage::TmuxCommand { .. }
                        | ClientMessage::CapturePane { .. } => {
                            if terminal_in_tx.send(msg).await.is_err() {
                                // Terminal task gone; the terminal path is dead,
                                // but the worktree path can keep serving.
                                terminal_done = true;
                            }
                        }
                        // The handshake and the buffer-channel requests
                        // (`OpenFile`/`SaveFile`) go to the shared loop; the
                        // buffer service that answers them is wired in a later
                        // editor step (#185), where `dispatch` currently no-ops.
                        ClientMessage::Hello { .. }
                        | ClientMessage::OpenFile { .. }
                        | ClientMessage::SaveFile { .. } => {
                            if inbound.send(msg).await.is_err() {
                                // Dispatch loop gone; nothing left to serve.
                                break 'serve;
                            }
                        }
                    }
                }
            }
            event = events.recv() => {
                match event {
                    Ok(msg) => {
                        let is_welcome = matches!(msg, DaemonMessage::Welcome { .. });
                        let frame = encode_frame(&msg)?;
                        writer.write_all(&frame).await?;
                        writer.flush().await?;
                        if is_welcome {
                            // Replay the snapshot immediately behind the
                            // handshake so the client sees `Welcome` then a
                            // complete tree. If the scan has not finished, the
                            // `state` branch delivers it once it lands.
                            handshaken = true;
                            if !snapshot_sent {
                                snapshot_sent = write_snapshot(&mut writer, &state).await?;
                            }
                        }
                    }
                    // The bus carries only incremental updates now; a lagging
                    // writer drops some. Log the count — never silently — so the
                    // divergence is observable. The snapshot itself is loss-free,
                    // delivered off-bus above.
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        eprintln!(
                            "rift-daemon connection lagged: dropped {skipped} worktree update(s)"
                        );
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
        }
    }

    // End this connection's tmux attach. The task then detaches the control
    // child (the tmux session persists) and exits.
    shutdown_terminal(terminal_in_tx, terminal_out_rx, terminal).await;

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

/// Run a daemon over a single byte-stream transport until either side closes.
///
/// Spins up a dispatch loop, serves exactly one connection over `reader`/
/// `writer`, then tears the loop down. With a `worktree_root`, the root is
/// scanned and watched for the daemon's lifetime (see
/// [`Daemon::watch_worktree`]). Used by the daemon binary's stdio mode and the
/// duplex round-trip test. For a long-lived, reattachable daemon that survives
/// client disconnects, see [`serve_uds`].
///
/// Returns once the reader reaches EOF or the writer/event bus closes.
pub async fn serve<R, W>(reader: R, writer: W, worktree_root: Option<PathBuf>) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (mut daemon, handles) = channels(SERVE_EVENT_CAPACITY, SERVE_INBOUND_CAPACITY);
    if let Some(root) = worktree_root {
        daemon.watch_worktree(root.clone());
        daemon.watch_lsp(root, DocumentSelector::builtin());
    }
    let events = handles.subscribe();
    let inbound = handles.inbound.clone();
    let state = handles.state.clone();
    // Drop the spare handles so the dispatch loop ends once the connection's
    // `inbound` clone is dropped at EOF.
    drop(handles);

    let dispatch = tokio::spawn(daemon.run());
    // `None`: production uses the default tmux server for terminal attaches.
    let result = serve_connection(reader, writer, inbound, events, state, None).await;
    // `serve_connection` dropped its `inbound` clone on return, so the dispatch
    // loop has observed channel closure and will join.
    dispatch.await?;
    result
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
/// With a `worktree_root`, the root is scanned and watched for the daemon's
/// lifetime; every attaching client receives the current snapshot on its
/// handshake (see [`Daemon::watch_worktree`]).
///
/// If a live daemon already owns `socket_path` this returns an error — the
/// caller must attach via [`connect_relay`], not spawn. A stale socket left by a
/// crashed daemon is removed and rebound. Transient per-accept errors are logged
/// and retried (the daemon must not die on FD pressure and leave nothing to
/// reattach to); the function returns only on a bind failure or process signal.
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

    let (mut daemon, handles) = channels(SERVE_EVENT_CAPACITY, SERVE_INBOUND_CAPACITY);
    if let Some(root) = worktree_root {
        daemon.watch_worktree(root.clone());
        daemon.watch_lsp(root, DocumentSelector::builtin());
    }
    // Held for the lifetime of the accept loop, so the dispatch loop and its
    // `State` stay alive even while no client is attached.
    let _dispatch = tokio::spawn(daemon.run());

    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                // A long-lived daemon must not die on a transient accept error
                // (ECONNABORTED, or EMFILE/ENFILE under FD pressure) — that would
                // leave nothing to reattach to. Log, back off briefly so a
                // persistent failure cannot hot-spin, and keep accepting.
                eprintln!("rift-daemon accept error: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let (reader, writer) = stream.into_split();
        let inbound = handles.inbound.clone();
        let events = handles.subscribe();
        let state = handles.state.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_connection(reader, writer, inbound, events, state, None).await {
                // Stderr is the daemon's log sink (stdout carries protocol frames
                // in stdio mode); one failed connection must not stop the daemon.
                eprintln!("rift-daemon connection ended with error: {e}");
            }
        });
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
    pub fn watch_lsp(&mut self, root: PathBuf, selector: DocumentSelector) {
        let (doc_tx, doc_rx) = mpsc::channel(LSP_CHANNEL_CAPACITY);
        let (diag_tx, diag_rx) = mpsc::channel(LSP_CHANNEL_CAPACITY);
        let worker = LspWorker::new(root, selector, doc_rx, diag_tx);
        tokio::spawn(worker.run());
        self.core.doc_changes = Some(doc_tx);
        self.lsp_diagnostics = Some(diag_rx);
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
            ClientMessage::Hello { version: _ } => {
                // Version negotiation / mismatch handling is deferred to the
                // transport sub-spec (a spec Risk); for now any version is
                // accepted and a `Welcome` carrying the daemon's
                // `PROTOCOL_VERSION` is emitted.
                //
                // A receiverless broadcast send is not an error here: events are
                // fire-and-forget, so a `Welcome` with no subscriber is dropped.
                //
                // The worktree snapshot is not broadcast here: each connection
                // replays it from `State` straight to its own socket right after
                // the `Welcome` (see `serve_connection`), off the bounded event
                // bus whose backlog would silently drop a large snapshot's
                // chunks (issue #227).
                let _ = self.events.send(DaemonMessage::Welcome {
                    version: PROTOCOL_VERSION,
                });
            }
            // Terminal/tmux messages never reach the shared dispatch loop:
            // `serve_connection` routes them to this connection's own
            // `terminal_task` (per-client attach). The arm stays as a defensive
            // no-op in case a caller drives the loop directly. The buffer-channel
            // requests (`OpenFile`/`SaveFile`) are likewise a no-op here — their
            // daemon service is wired in a later editor step (#185).
            ClientMessage::Attach { .. }
            | ClientMessage::Input { .. }
            | ClientMessage::ResizePane { .. }
            | ClientMessage::TmuxCommand { .. }
            | ClientMessage::CapturePane { .. }
            | ClientMessage::OpenFile { .. }
            | ClientMessage::SaveFile { .. } => {}
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
                            eprintln!("rift-daemon: dropped document-sync batch: {err}");
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

    #[tokio::test]
    async fn test_dispatch_hello_current_version_emits_welcome() {
        let (daemon, handles) = channels(8, 8);
        let mut events = handles.subscribe();
        let loop_handle = tokio::spawn(daemon.run());

        handles
            .inbound
            .send(ClientMessage::Hello {
                version: PROTOCOL_VERSION,
            })
            .await
            .expect("send Hello");

        let reply = events.recv().await.expect("receive Welcome");
        assert_eq!(
            reply,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            }
        );

        // drop all senders so run() observes channel closure and returns
        drop(handles);
        loop_handle.await.expect("dispatch loop joins cleanly");
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
            let _ =
                serve_connection(server_reader, server_writer, inbound, events, state, None).await;
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
        let handle = tokio::spawn(terminal::terminal_task(
            in_rx,
            out_tx,
            Some(server.0.clone()),
        ));

        in_tx
            .send(ClientMessage::Attach {
                session: "rift".to_owned(),
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
                Some(tmux_name),
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
}
