use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod spike;

use rift_explorer::{Change, Entry, Snapshot, Watcher};
use rift_protocol::{
    encode_frame, ClientMessage, DaemonMessage, EntryKind, FrameDecoder, WorktreeEntry,
    PROTOCOL_VERSION,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch};

/// Single source of truth for the daemon's observable state.
///
/// Mirrors `DaemonMessage::StateUpdate`. Held as the value of a
/// `tokio::sync::watch` channel so consumers observe the latest snapshot
/// without sharing a mutex.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub sessions: Vec<String>,
    /// Latest worktree snapshot, present once the initial scan completes.
    /// Kept current by applying the watcher's change batches in place.
    pub worktree: Option<Snapshot>,
}

/// Internal events from the worktree worker into the dispatch loop.
enum WorktreeEvent {
    /// The initial scan completed; this snapshot becomes the `State` worktree.
    Scanned(Snapshot),
    /// A coalesced batch of changes against the previously delivered state.
    Changed(Vec<Change>),
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
    core: Core,
}

/// The dispatch loop's owned half: the `State` writer and the event
/// broadcaster. Split from [`Daemon`] so `run` can poll the inbound channels
/// while a completed branch's handler mutates this.
struct Core {
    events: broadcast::Sender<DaemonMessage>,
    state: watch::Sender<State>,
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
        core: Core {
            events: events_tx.clone(),
            state: state_tx,
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

/// Queue depth for worktree events flowing from the blocking worker into the
/// dispatch loop. Bounds how far the worker may run ahead while the loop is busy.
const WORKTREE_EVENT_CAPACITY: usize = 64;

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
    // Arm the watcher BEFORE delivering the snapshot: `Watcher::new` registers
    // its watch set synchronously, so once a consumer has observed the snapshot,
    // any later write is guaranteed to produce an event. The reverse order races
    // — a write right after the snapshot lands would precede the watches and,
    // with no event, the rescan-on-event watcher would never surface it. The
    // clone is the two-owner boundary: the watcher keeps the diff baseline, the
    // dispatch loop's `State` gets its own copy.
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
    // `_watcher` stays alive for the loop's duration; dropping it (on return)
    // stops the watch thread and releases the OS watches.
    loop {
        match changes.recv_timeout(WORKTREE_IDLE_POLL) {
            Ok(batch) => {
                if events.blocking_send(WorktreeEvent::Changed(batch)).is_err() {
                    return;
                }
            }
            // Idle: only worth waking for if the dispatch loop has gone away.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if events.is_closed() {
                    return;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
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
/// Decodes [`ClientMessage`] frames from `reader` into the loop via `inbound`
/// and writes [`DaemonMessage`] frames from `events` to `writer`. One call
/// drives one connection; the dispatch loop and the `State` it owns live
/// outside it, so they persist across reconnects — the reattach contract.
///
/// `inbound` is a clone of the loop's inbound sender (dropped when the
/// connection ends); `events` is a fresh subscription to the outbound bus.
/// Returns once the reader reaches EOF, the dispatch loop is gone, or the event
/// bus closes.
async fn serve_connection<R, W>(
    reader: R,
    mut writer: W,
    inbound: mpsc::Sender<ClientMessage>,
    mut events: broadcast::Receiver<DaemonMessage>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = reader;
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0u8; SERVE_READ_BUFFER];

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
                    if inbound.send(msg).await.is_err() {
                        // Dispatch loop gone; nothing left to serve.
                        break 'serve;
                    }
                }
            }
            event = events.recv() => {
                match event {
                    Ok(msg) => {
                        let frame = encode_frame(&msg)?;
                        writer.write_all(&frame).await?;
                        writer.flush().await?;
                    }
                    // Lagged: the writer fell behind the broadcast backlog. Skip
                    // the dropped events and keep serving rather than tearing the
                    // connection down.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break 'serve,
                }
            }
        }
    }

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
        daemon.watch_worktree(root);
    }
    let events = handles.subscribe();
    let inbound = handles.inbound.clone();
    // Drop the spare handles so the dispatch loop ends once the connection's
    // `inbound` clone is dropped at EOF.
    drop(handles);

    let dispatch = tokio::spawn(daemon.run());
    let result = serve_connection(reader, writer, inbound, events).await;
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
        daemon.watch_worktree(root);
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
        tokio::spawn(async move {
            if let Err(e) = serve_connection(reader, writer, inbound, events).await {
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

    /// Run the flat dispatch loop until the inbound channel closes.
    ///
    /// Each `ClientMessage` and each worktree event is matched directly to a
    /// handler on [`Core`]. The loop owns the `State` writer and the event
    /// broadcaster; it ends when every inbound sender is dropped.
    pub async fn run(self) {
        let Daemon {
            mut inbound,
            mut worktree,
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
                let _ = self.events.send(DaemonMessage::Welcome {
                    version: PROTOCOL_VERSION,
                });
                // A (re)attaching client needs the full tree, not just future
                // deltas — re-broadcast the current snapshot after the
                // handshake. Other attached clients receive it too (single
                // shared event bus); a full snapshot replaces their model, so
                // the repeat is redundant but never inconsistent.
                self.broadcast_snapshot();
            }
            // Terminal/tmux handling is owned by a later Phase 3 sub-spec; this
            // scaffolding only carries the handshake.
            ClientMessage::Input { .. }
            | ClientMessage::ResizePane { .. }
            | ClientMessage::TmuxCommand { .. } => {}
        }
    }

    /// Fold a worktree event into the `State` and onto the event bus.
    fn apply_worktree(&mut self, event: WorktreeEvent) {
        match event {
            WorktreeEvent::Scanned(snapshot) => {
                self.state
                    .send_modify(|state| state.worktree = Some(snapshot));
                self.broadcast_snapshot();
            }
            WorktreeEvent::Changed(batch) => {
                self.state.send_modify(|state| {
                    if let Some(worktree) = &mut state.worktree {
                        worktree.apply(&batch);
                    }
                });
                let _ = self.events.send(update_message(&batch));
            }
        }
    }

    /// Emit the current worktree as chunked `WorktreeSnapshot` messages, or
    /// nothing while no scan has completed. The dispatch loop is the only
    /// emitter, so a snapshot's chunks are never interleaved with updates.
    fn broadcast_snapshot(&self) {
        let state = self.state.borrow();
        let Some(worktree) = &state.worktree else {
            return;
        };
        for msg in snapshot_messages(worktree.root(), worktree.entries()) {
            let _ = self.events.send(msg);
        }
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

    /// Collect one complete chunked snapshot: entries accumulated across
    /// `WorktreeSnapshot` messages until `final_chunk`.
    async fn recv_snapshot(
        events: &mut broadcast::Receiver<DaemonMessage>,
    ) -> Vec<rift_protocol::WorktreeEntry> {
        let mut collected = Vec::new();
        loop {
            let (mut entries, final_chunk) = recv_until(events, |msg| match msg {
                DaemonMessage::WorktreeSnapshot {
                    entries,
                    final_chunk,
                    ..
                } => Some((entries, final_chunk)),
                _ => None,
            })
            .await;
            collected.append(&mut entries);
            if final_chunk {
                return collected;
            }
        }
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
    async fn test_worktree_scan_broadcasts_snapshot_and_streams_update_on_change() {
        let tmp = TempDir::new("scan");
        write_file(&tmp.path.join("src/main.rs"), "fn main() {}");
        write_file(&tmp.path.join("README.md"), "# readme");

        let (mut daemon, handles) = channels(64, 8);
        daemon.watch_worktree(tmp.path.clone());
        let mut events = handles.subscribe();
        let state = handles.state.clone();
        let loop_handle = tokio::spawn(daemon.run());

        // The initial scan lands as a complete chunked snapshot on the bus.
        let entries = recv_snapshot(&mut events).await;
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"README.md"));
        assert!(paths.contains(&"src"));
        assert!(paths.contains(&"src/main.rs"));

        // The same tree is observable in the daemon's State.
        let held = state.borrow().worktree.clone().expect("worktree in State");
        assert!(held.get(Path::new("src/main.rs")).is_some());

        // A new file streams as an incremental UpdateWorktree, and the State
        // follows it.
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
    async fn test_hello_after_scan_rebroadcasts_snapshot_for_reattach() {
        let tmp = TempDir::new("reattach");
        write_file(&tmp.path.join("tracked.txt"), "x");

        let (mut daemon, handles) = channels(64, 8);
        daemon.watch_worktree(tmp.path.clone());
        let mut events = handles.subscribe();
        let loop_handle = tokio::spawn(daemon.run());

        // Wait for the initial scan to land so the Hello below races nothing.
        let _ = recv_snapshot(&mut events).await;

        // A late subscriber (a reattaching client) missed the initial
        // broadcast; its Hello must trigger a re-send.
        let mut late = handles.subscribe();
        handles
            .inbound
            .send(ClientMessage::Hello {
                version: PROTOCOL_VERSION,
            })
            .await
            .expect("send Hello");

        let welcome = recv_until(&mut late, |msg| match msg {
            DaemonMessage::Welcome { version } => Some(version),
            _ => None,
        })
        .await;
        assert_eq!(welcome, PROTOCOL_VERSION);
        let entries = recv_snapshot(&mut late).await;
        assert!(entries.iter().any(|e| e.path == "tracked.txt"));

        drop(handles);
        drop(events);
        drop(late);
        loop_handle.await.expect("dispatch loop joins cleanly");
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

        // Collect frames until one complete snapshot has arrived; the Welcome
        // and the snapshot may interleave depending on when the scan finishes
        // relative to the handshake, so assert content, not strict order.
        let mut welcome_seen = false;
        let mut entries = Vec::new();
        loop {
            match read_daemon_message(&mut client, &mut decoder).await {
                DaemonMessage::Welcome { version } => {
                    assert_eq!(version, PROTOCOL_VERSION);
                    welcome_seen = true;
                }
                DaemonMessage::WorktreeSnapshot {
                    entries: mut chunk,
                    final_chunk,
                    ..
                } => {
                    entries.append(&mut chunk);
                    if final_chunk && welcome_seen {
                        break;
                    }
                }
                other => panic!("unexpected message before snapshot: {other:?}"),
            }
        }
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
}
