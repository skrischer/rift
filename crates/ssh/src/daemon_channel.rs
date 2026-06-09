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

use crate::error::SshError;

/// Raw byte transport over a single SSH exec channel.
///
/// Owns a channel actor that forwards inbound `Data`/`ExtendedData` to a reader
/// queue and writes outbound bytes to the channel. No PTY, no resize.
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
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        if data_tx.send(data.to_vec()).is_err() {
                            break;
                        }
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
/// Owns a byte transport plus the `rift-protocol` framing, exposing exactly one
/// emission point ([`DaemonClient::send`]) and one reception point
/// ([`DaemonClient::recv`]). All app-side daemon emission flows through here, so
/// the later `TmuxClient` migration is a one-seam change.
pub struct DaemonClient<T> {
    transport: T,
    decoder: Mutex<FrameDecoder>,
}

/// Byte transport a [`DaemonClient`] frames over.
///
/// Abstracts the concrete channel so the seam can be unit-tested over an
/// in-memory pipe without a live SSH session.
#[async_trait::async_trait]
pub trait DaemonTransport {
    /// Send raw bytes to the daemon.
    async fn write(&self, data: &[u8]) -> Result<(), SshError>;
    /// Receive the next chunk of raw bytes from the daemon.
    async fn read(&self) -> Result<Vec<u8>, SshError>;
}

#[async_trait::async_trait]
impl DaemonTransport for DaemonChannel {
    async fn write(&self, data: &[u8]) -> Result<(), SshError> {
        DaemonChannel::write(self, data).await
    }

    async fn read(&self) -> Result<Vec<u8>, SshError> {
        DaemonChannel::read(self).await
    }
}

impl<T: DaemonTransport> DaemonClient<T> {
    /// Wrap a byte transport with protocol framing.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            decoder: Mutex::new(FrameDecoder::new()),
        }
    }

    /// Emit a `ClientMessage` to the daemon as a single length-delimited frame.
    pub async fn send(&self, msg: ClientMessage) -> Result<(), SshError> {
        let frame = encode_frame(&msg).map_err(|e| SshError::Channel(e.to_string()))?;
        self.transport.write(&frame).await
    }

    /// Receive the next `DaemonMessage`, reassembling frames across reads.
    ///
    /// Returns `None` once the transport closes before a full frame arrives.
    pub async fn recv(&self) -> Option<DaemonMessage> {
        let mut decoder = self.decoder.lock().await;
        loop {
            match decoder.next_frame::<DaemonMessage>() {
                Ok(Some(msg)) => return Some(msg),
                // A malformed frame on the wire is unrecoverable for this seam;
                // surface it as a closed stream rather than spinning.
                Err(_) => return None,
                Ok(None) => match self.transport.read().await {
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

    /// In-memory transport pairing a `DaemonClient`'s writes with a scripted set
    /// of reads, so the seam's framing is exercised without a live SSH channel.
    struct LoopbackTransport {
        outbound: flume::Sender<Vec<u8>>,
        inbound: flume::Receiver<Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl DaemonTransport for LoopbackTransport {
        async fn write(&self, data: &[u8]) -> Result<(), SshError> {
            self.outbound
                .send_async(data.to_vec())
                .await
                .map_err(|_| SshError::Channel("loopback closed".into()))
        }

        async fn read(&self) -> Result<Vec<u8>, SshError> {
            self.inbound
                .recv_async()
                .await
                .map_err(|_| SshError::Channel("loopback closed".into()))
        }
    }

    #[tokio::test]
    async fn test_send_emits_a_single_length_delimited_frame() {
        let (out_tx, out_rx) = flume::unbounded();
        let (_in_tx, in_rx) = flume::unbounded();
        let client = DaemonClient::new(LoopbackTransport {
            outbound: out_tx,
            inbound: in_rx,
        });

        client
            .send(ClientMessage::Hello {
                version: PROTOCOL_VERSION,
            })
            .await
            .expect("send Hello");

        let frame = out_rx.recv_async().await.expect("receive frame");
        let expected = encode_frame(&ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        })
        .expect("encode expected");
        assert_eq!(frame, expected);
    }

    #[tokio::test]
    async fn test_recv_reassembles_a_frame_split_across_reads() {
        let (out_tx, _out_rx) = flume::unbounded();
        let (in_tx, in_rx) = flume::unbounded();
        let client = DaemonClient::new(LoopbackTransport {
            outbound: out_tx,
            inbound: in_rx,
        });

        let frame = encode_frame(&DaemonMessage::Welcome {
            version: PROTOCOL_VERSION,
        })
        .expect("encode Welcome");
        let split = frame.len() / 2;
        in_tx.send(frame[..split].to_vec()).expect("push head");
        in_tx.send(frame[split..].to_vec()).expect("push tail");

        let msg = client.recv().await.expect("recv Welcome");
        assert_eq!(
            msg,
            DaemonMessage::Welcome {
                version: PROTOCOL_VERSION,
            }
        );
    }
}
