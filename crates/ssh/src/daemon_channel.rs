//! Non-PTY exec channel carrying the `rift-protocol` framing.
//!
//! Mirrors [`crate::pty`]'s channel-actor pattern, stripped to the bytes a
//! daemon connection needs: an async [`DaemonChannel::write`] and
//! [`DaemonChannel::read`], with no PTY allocation or window-resize handling.
//! [`DaemonClient`] layers the protocol framing on top and is the single
//! app-side emission seam the client `TmuxClient` path later swaps onto.

use rift_protocol::{encode_frame, ClientMessage, DaemonMessage, FrameDecoder};
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
