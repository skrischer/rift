//! Non-PTY exec channel carrying the `rift-protocol` framing.
//!
//! Mirrors [`crate::pty`]'s channel-actor pattern, stripped to the bytes a
//! daemon connection needs: an async [`DaemonChannel::write`] and
//! [`DaemonChannel::read`], with no PTY allocation or window-resize handling.
//! [`DaemonClient`] layers the protocol framing on top and is the single
//! app-side emission seam the client `TmuxClient` path later swaps onto.

use std::time::Duration;

use rift_protocol::{encode_frame, ClientMessage, DaemonMessage, FrameDecoder, PROTOCOL_VERSION};
use russh::client;
use russh::{Channel, ChannelMsg};
use tokio::sync::Mutex;
use tracing::warn;

use crate::error::SshError;

/// Raw byte transport over a single SSH exec channel.
///
/// Owns a channel actor that forwards the remote daemon's stdout
/// (`ChannelMsg::Data`) to a reader queue and writes outbound bytes to the
/// channel. No PTY, no resize. Remote stderr (`ChannelMsg::ExtendedData`) is
/// kept out of the frame stream — it would corrupt the protocol framing.
pub struct DaemonChannel {
    data_rx: flume::Receiver<Vec<u8>>,
    write_tx: flume::Sender<Vec<u8>>,
}

impl DaemonChannel {
    pub(crate) fn new(channel: Channel<client::Msg>) -> Self {
        let (data_tx, data_rx) = flume::unbounded();
        let (write_tx, write_rx) = flume::unbounded();

        tokio::spawn(channel_actor(channel, write_rx, data_tx));

        Self { data_rx, write_tx }
    }

    /// Wire a channel directly to in-memory queues, bypassing SSH — the test
    /// seam for exercising [`DaemonClient`] against a stubbed daemon side.
    #[cfg(test)]
    fn from_parts(data_rx: flume::Receiver<Vec<u8>>, write_tx: flume::Sender<Vec<u8>>) -> Self {
        Self { data_rx, write_tx }
    }

    /// Send raw bytes to the remote daemon.
    pub async fn write(&self, data: &[u8]) -> Result<(), SshError> {
        self.write_tx
            .send_async(data.to_vec())
            .await
            .map_err(|_| SshError::Channel("daemon channel closed".into()))
    }

    /// Receive the next chunk of raw bytes from the remote daemon.
    ///
    /// Resolves once the channel delivers data; errors when the channel closes.
    pub async fn read(&self) -> Result<Vec<u8>, SshError> {
        self.data_rx
            .recv_async()
            .await
            .map_err(|_| SshError::Channel("daemon channel closed".into()))
    }
}

async fn channel_actor(
    mut channel: Channel<client::Msg>,
    write_rx: flume::Receiver<Vec<u8>>,
    data_tx: flume::Sender<Vec<u8>>,
) {
    loop {
        tokio::select! {
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        if data_tx.send(data.to_vec()).is_err() {
                            break;
                        }
                    }
                    // Remote stderr. Must not enter the frame stream — feeding it
                    // to the decoder would corrupt protocol framing. Surface it
                    // for debugging and drop it.
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        warn!(bytes = data.len(), "daemon channel stderr (dropped, not framed)");
                    }
                    Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
                    Some(_) => {}
                }
            }
            write = write_rx.recv_async() => {
                match write {
                    Ok(data) => {
                        if channel.data(&*data).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

/// Outcome of a completed `Hello`/`Welcome` round-trip: the daemon answered
/// orderly, and its protocol version either matches this client's or
/// identifies a stale daemon to replace (strict-equality policy,
/// `docs/protocol.md` — Versioning policy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handshake {
    /// The daemon runs this client's own [`PROTOCOL_VERSION`]; the transport
    /// is confirmed and the stream may be consumed.
    Ready,
    /// The daemon answered with a different protocol version — the orderly
    /// early skew signal (#473). The caller owns the resolution: stop the
    /// stale daemon via its pidfile, redeploy, respawn, re-handshake.
    VersionMismatch {
        /// The daemon's own protocol version, as carried by its `Welcome`.
        daemon: u32,
    },
}

/// The single app-side seam for client/daemon protocol traffic.
///
/// Owns a [`DaemonChannel`] plus the `rift-protocol` framing, exposing exactly
/// one emission point ([`DaemonClient::send`]) and one reception point
/// ([`DaemonClient::recv`]). All app-side daemon emission flows through here, so
/// the later `TmuxClient` migration is a one-seam change.
pub struct DaemonClient {
    channel: DaemonChannel,
    decoder: Mutex<FrameDecoder>,
}

impl DaemonClient {
    /// Wrap a [`DaemonChannel`] with protocol framing.
    pub fn new(channel: DaemonChannel) -> Self {
        Self {
            channel,
            decoder: Mutex::new(FrameDecoder::new()),
        }
    }

    /// Emit a `ClientMessage` to the daemon as a single length-delimited frame.
    pub async fn send(&self, msg: ClientMessage) -> Result<(), SshError> {
        let frame = encode_frame(&msg).map_err(|e| SshError::Channel(e.to_string()))?;
        self.channel.write(&frame).await
    }

    /// Receive the next `DaemonMessage`, bounded by `timeout`.
    ///
    /// Same closed-stream semantics as [`DaemonClient::recv`] (`Ok(None)` once
    /// the channel closes); a daemon that stays silent past `timeout` yields
    /// [`SshError::RecvTimeout`] instead of blocking forever.
    pub async fn recv_timeout(&self, timeout: Duration) -> Result<Option<DaemonMessage>, SshError> {
        tokio::time::timeout(timeout, self.recv())
            .await
            .map_err(|_| SshError::RecvTimeout(timeout))
    }

    /// Run the `Hello`/`Welcome` handshake, bounded by `timeout`, enforcing
    /// the strict version-equality policy (`docs/protocol.md`).
    ///
    /// Sends `Hello { PROTOCOL_VERSION }` and awaits the daemon's `Welcome`,
    /// which the daemon writes per connection as the first frame (#473, #425).
    /// `Ok(Handshake::Ready)` confirms the transport at matching versions;
    /// `Ok(Handshake::VersionMismatch { .. })` is the daemon's orderly skew
    /// signal — it closes its side after that `Welcome`, and the caller owns
    /// the replacement. Everything else is an `Err`: a send failure or a
    /// non-`Welcome` first frame ([`SshError::Handshake`]), a channel closed
    /// before any `Welcome` ([`SshError::Handshake`]), or a daemon silent past
    /// `timeout` ([`SshError::RecvTimeout`]).
    pub async fn handshake(&self, timeout: Duration) -> Result<Handshake, SshError> {
        self.send(ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        })
        .await?;
        match self.recv_timeout(timeout).await? {
            Some(DaemonMessage::Welcome { version }) if version == PROTOCOL_VERSION => {
                Ok(Handshake::Ready)
            }
            Some(DaemonMessage::Welcome { version }) => {
                Ok(Handshake::VersionMismatch { daemon: version })
            }
            Some(other) => Err(SshError::Handshake(format!(
                "unexpected first frame from daemon: {other:?}"
            ))),
            None => Err(SshError::Handshake(
                "daemon closed the channel before a Welcome".into(),
            )),
        }
    }

    /// Receive the next `DaemonMessage`, reassembling frames across reads.
    ///
    /// Returns `None` once the channel closes before a full frame arrives.
    pub async fn recv(&self) -> Option<DaemonMessage> {
        let mut decoder = self.decoder.lock().await;
        loop {
            match decoder.next_frame::<DaemonMessage>() {
                Ok(Some(msg)) => return Some(msg),
                // A malformed frame on the wire is unrecoverable for this seam;
                // surface it as a closed stream rather than spinning.
                Err(e) => {
                    warn!(error = %e, "malformed daemon frame, closing stream");
                    return None;
                }
                Ok(None) => match self.channel.read().await {
                    Ok(bytes) => decoder.push(&bytes),
                    Err(_) => return None,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_protocol::PROTOCOL_VERSION;

    /// A [`DaemonClient`] over in-memory queues plus both daemon-side ends:
    /// the byte sender (holding it open without sending models a wedged
    /// daemon, dropping it a closed channel) and the byte receiver (what the
    /// client wrote, for asserting the emitted frames).
    fn wired_client() -> (
        DaemonClient,
        flume::Sender<Vec<u8>>,
        flume::Receiver<Vec<u8>>,
    ) {
        let (data_tx, data_rx) = flume::unbounded();
        let (write_tx, write_rx) = flume::unbounded();
        let client = DaemonClient::new(DaemonChannel::from_parts(data_rx, write_tx));
        (client, data_tx, write_rx)
    }

    /// Queue a daemon-side `Welcome` carrying `version`.
    fn queue_welcome(data_tx: &flume::Sender<Vec<u8>>, version: u32) {
        let frame = encode_frame(&DaemonMessage::Welcome { version }).expect("encode welcome");
        data_tx.send(frame).expect("queue welcome");
    }

    #[tokio::test]
    async fn test_recv_timeout_wedged_daemon_returns_timeout_error() {
        // Sender stays alive but never answers: the wait must end in an error.
        let (client, _data_tx, _write_rx) = wired_client();
        let result = client.recv_timeout(Duration::from_millis(50)).await;
        assert!(matches!(result, Err(SshError::RecvTimeout(_))));
    }

    #[tokio::test]
    async fn test_recv_timeout_frame_arrives_returns_message() {
        let (client, data_tx, _write_rx) = wired_client();
        queue_welcome(&data_tx, PROTOCOL_VERSION);
        let result = client.recv_timeout(Duration::from_secs(5)).await;
        assert!(matches!(
            result,
            Ok(Some(DaemonMessage::Welcome { version })) if version == PROTOCOL_VERSION
        ));
    }

    #[tokio::test]
    async fn test_recv_timeout_closed_channel_returns_none() {
        let (client, data_tx, _write_rx) = wired_client();
        drop(data_tx);
        let result = client.recv_timeout(Duration::from_secs(5)).await;
        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn test_handshake_matching_version_returns_ready() {
        let (client, data_tx, _write_rx) = wired_client();
        queue_welcome(&data_tx, PROTOCOL_VERSION);
        let result = client.handshake(Duration::from_secs(5)).await;
        assert!(matches!(result, Ok(Handshake::Ready)));
    }

    #[tokio::test]
    async fn test_handshake_mismatched_version_returns_daemon_version() {
        let (client, data_tx, _write_rx) = wired_client();
        queue_welcome(&data_tx, PROTOCOL_VERSION + 1);
        let result = client.handshake(Duration::from_secs(5)).await;
        assert!(matches!(
            result,
            Ok(Handshake::VersionMismatch { daemon }) if daemon == PROTOCOL_VERSION + 1
        ));
    }

    #[tokio::test]
    async fn test_handshake_non_welcome_first_frame_returns_error() {
        // A daemon that streams before greeting violates the Welcome-first
        // contract (#425); the handshake must fail, not consume the frame as
        // if the transport were confirmed.
        let (client, data_tx, _write_rx) = wired_client();
        let frame = encode_frame(&DaemonMessage::PaneOutput {
            pane_id: 1,
            bytes: b"stray".to_vec(),
        })
        .expect("encode stray frame");
        data_tx.send(frame).expect("queue stray frame");
        let result = client.handshake(Duration::from_secs(5)).await;
        assert!(matches!(result, Err(SshError::Handshake(_))));
    }

    #[tokio::test]
    async fn test_handshake_closed_before_welcome_returns_error() {
        let (client, data_tx, _write_rx) = wired_client();
        drop(data_tx);
        let result = client.handshake(Duration::from_secs(5)).await;
        assert!(matches!(result, Err(SshError::Handshake(_))));
    }

    #[tokio::test]
    async fn test_handshake_silent_daemon_returns_timeout_error() {
        let (client, _data_tx, _write_rx) = wired_client();
        let result = client.handshake(Duration::from_millis(50)).await;
        assert!(matches!(result, Err(SshError::RecvTimeout(_))));
    }

    #[tokio::test]
    async fn test_handshake_sends_hello_at_current_version() {
        let (client, data_tx, write_rx) = wired_client();
        queue_welcome(&data_tx, PROTOCOL_VERSION);
        client
            .handshake(Duration::from_secs(5))
            .await
            .expect("handshake");

        let mut decoder = FrameDecoder::new();
        decoder.push(&write_rx.recv().expect("client wrote the hello frame"));
        let hello = decoder
            .next_frame::<ClientMessage>()
            .expect("decode hello")
            .expect("complete hello frame");
        assert_eq!(
            hello,
            ClientMessage::Hello {
                version: PROTOCOL_VERSION
            }
        );
    }
}
