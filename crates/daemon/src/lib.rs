use std::path::Path;

use rift_protocol::{encode_frame, ClientMessage, DaemonMessage, FrameDecoder, PROTOCOL_VERSION};
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
}

/// Wiring for the daemon's flat dispatch loop.
///
/// Inbound `ClientMessage`s arrive on `inbound`; outbound `DaemonMessage`
/// events are published on `events`; the latest `State` snapshot is observable
/// via the `watch` receiver returned alongside this struct by [`channels`].
pub struct Daemon {
    inbound: mpsc::Receiver<ClientMessage>,
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
        events: events_tx.clone(),
        state: state_tx,
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
/// `writer`, then tears the loop down. Used by the daemon binary's stdio mode
/// and the duplex round-trip test. For a long-lived, reattachable daemon that
/// survives client disconnects, see [`serve_uds`].
///
/// Returns once the reader reaches EOF or the writer/event bus closes.
pub async fn serve<R, W>(reader: R, writer: W) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (daemon, handles) = channels(SERVE_EVENT_CAPACITY, SERVE_INBOUND_CAPACITY);
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
/// If a live daemon already owns `socket_path` this returns an error — the
/// caller must attach via [`connect_relay`], not spawn. A stale socket left by a
/// crashed daemon is removed and rebound. Transient per-accept errors are logged
/// and retried (the daemon must not die on FD pressure and leave nothing to
/// reattach to); the function returns only on a bind failure or process signal.
pub async fn serve_uds(socket_path: &Path) -> anyhow::Result<()> {
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

    let (daemon, handles) = channels(SERVE_EVENT_CAPACITY, SERVE_INBOUND_CAPACITY);
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
    /// Run the flat dispatch loop until the inbound channel closes.
    ///
    /// Each `ClientMessage` is matched directly to a handler. The loop owns the
    /// `State` writer and the event broadcaster; it ends when every inbound
    /// sender is dropped.
    pub async fn run(mut self) {
        while let Some(msg) = self.inbound.recv().await {
            self.dispatch(msg);
        }
    }

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
            }
            // Terminal/tmux handling is owned by a later Phase 3 sub-spec; this
            // scaffolding only carries the handshake.
            ClientMessage::Input { .. }
            | ClientMessage::ResizePane { .. }
            | ClientMessage::TmuxCommand { .. } => {}
        }
    }

    /// Return the latest `State` snapshot (the `watch` sender's view).
    ///
    /// Stays `State::default()` until handlers begin mutating state; it exists
    /// for future consumers and keeps the held `watch::Sender` from tripping
    /// `dead_code` under `-D warnings`.
    pub fn state(&self) -> State {
        self.state.borrow().clone()
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
    async fn read_daemon_message<R: AsyncRead + Unpin>(reader: &mut R) -> DaemonMessage {
        let mut decoder = FrameDecoder::new();
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
            async move { serve_uds(&sock).await }
        });
        wait_for_socket(&sock).await;

        let mut client = UnixStream::connect(&sock).await.expect("connect");
        client.write_all(&hello_frame()).await.expect("send Hello");
        client.flush().await.expect("flush Hello");

        assert_eq!(
            read_daemon_message(&mut client).await,
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
            async move { serve_uds(&sock).await }
        });
        wait_for_socket(&sock).await;

        // First client: handshake, then disconnect by dropping the stream.
        {
            let mut c1 = UnixStream::connect(&sock).await.expect("connect 1");
            c1.write_all(&hello_frame()).await.expect("send Hello 1");
            c1.flush().await.expect("flush 1");
            assert_eq!(
                read_daemon_message(&mut c1).await,
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
        c2.write_all(&hello_frame()).await.expect("send Hello 2");
        c2.flush().await.expect("flush 2");
        assert_eq!(
            read_daemon_message(&mut c2).await,
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
            async move { serve_uds(&sock).await }
        });
        wait_for_socket(&sock).await;

        let err = serve_uds(&sock).await.expect_err("second bind must fail");
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
            async move { serve_uds(&sock).await }
        });
        wait_for_socket(&sock).await;

        let mut client = UnixStream::connect(&sock)
            .await
            .expect("connect after rebind");
        client.write_all(&hello_frame()).await.expect("send Hello");
        client.flush().await.expect("flush Hello");
        assert_eq!(
            read_daemon_message(&mut client).await,
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
            async move { serve_uds(&sock).await }
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

        assert_eq!(
            read_daemon_message(&mut client_reader).await,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            }
        );

        relay_task.abort();
        server.abort();
        let _ = tokio::fs::remove_file(&sock).await;
    }

    #[tokio::test]
    async fn test_ping_false_when_absent_true_when_listening() {
        let sock = unique_socket_path();
        assert!(!ping(&sock).await, "ping must be false before any bind");

        let server = tokio::spawn({
            let sock = sock.clone();
            async move { serve_uds(&sock).await }
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
