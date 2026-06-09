use rift_protocol::{encode_frame, ClientMessage, DaemonMessage, FrameDecoder, PROTOCOL_VERSION};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
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

/// Capacities for the channels backing a [`serve`] session.
///
/// Sized for a single client connection: the inbound queue absorbs bursts of
/// `ClientMessage`s while the dispatch loop drains them, and the broadcast
/// backlog bounds how far an outbound writer may lag before lagged events are
/// dropped.
const SERVE_INBOUND_CAPACITY: usize = 256;
const SERVE_EVENT_CAPACITY: usize = 256;

/// Read buffer for a single transport read. The transport delivers arbitrary
/// chunk sizes; the [`FrameDecoder`] reassembles frames regardless of this size.
const SERVE_READ_BUFFER: usize = 8 * 1024;

/// Run a daemon over a byte-stream transport until either side closes.
///
/// Decodes [`ClientMessage`] frames from `reader` into the dispatch loop and
/// writes [`DaemonMessage`] frames from the event bus to `writer`. This is the
/// single transport seam: the daemon binary feeds it stdio, an integration test
/// feeds it a `tokio::io::duplex` pipe, and a future SSH-channel host feeds it
/// the channel's read/write halves — all without touching the dispatch loop.
///
/// Returns once the reader reaches EOF or the writer/event bus closes.
pub async fn serve<R, W>(reader: R, mut writer: W) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (daemon, handles) = channels(SERVE_EVENT_CAPACITY, SERVE_INBOUND_CAPACITY);
    let mut events = handles.subscribe();
    let inbound = handles.inbound.clone();
    // Drop the remaining handles so the dispatch loop ends once `inbound` (held
    // by the reader task) is dropped at EOF; keep only the subscription and the
    // inbound sender that this session drives.
    drop(handles);

    let dispatch = tokio::spawn(daemon.run());

    let mut reader = reader;
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0u8; SERVE_READ_BUFFER];

    'serve: loop {
        tokio::select! {
            read = reader.read(&mut buf) => {
                let n = read?;
                if n == 0 {
                    // Reader EOF: stop accepting input. Dropping `inbound` lets
                    // the dispatch loop drain and exit.
                    break 'serve;
                }
                decoder.push(&buf[..n]);
                while let Some(msg) = decoder.next_frame::<ClientMessage>()? {
                    if inbound.send(msg).await.is_err() {
                        // Dispatch loop gone; stop serving entirely rather than
                        // reading and discarding further frames.
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
                    // session down.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break 'serve,
                }
            }
        }
    }

    drop(inbound);
    dispatch.await?;
    Ok(())
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
}
