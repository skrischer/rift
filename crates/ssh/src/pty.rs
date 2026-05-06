use russh::client;
use russh::{Channel, ChannelMsg};

use crate::error::SshError;

enum PtyCmd {
    Write(Vec<u8>),
    Resize(u16, u16),
}

pub struct PtyStream {
    data_rx: flume::Receiver<Vec<u8>>,
    cmd_tx: flume::Sender<PtyCmd>,
}

impl PtyStream {
    pub(crate) fn new(channel: Channel<client::Msg>) -> Self {
        let (data_tx, data_rx) = flume::unbounded();
        let (cmd_tx, cmd_rx) = flume::unbounded();

        tokio::spawn(pty_actor(channel, cmd_rx, data_tx));

        Self { data_rx, cmd_tx }
    }

    pub async fn read(&self) -> Result<Vec<u8>, SshError> {
        self.data_rx
            .recv_async()
            .await
            .map_err(|_| SshError::Channel("channel closed".into()))
    }

    pub fn clone_writer(&self) -> PtyWriter {
        PtyWriter {
            cmd_tx: self.cmd_tx.clone(),
        }
    }
}

#[derive(Clone)]
pub struct PtyWriter {
    cmd_tx: flume::Sender<PtyCmd>,
}

impl PtyWriter {
    pub async fn write(&self, data: &[u8]) -> Result<(), SshError> {
        self.cmd_tx
            .send_async(PtyCmd::Write(data.to_vec()))
            .await
            .map_err(|_| SshError::Channel("channel closed".into()))
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshError> {
        self.cmd_tx
            .send_async(PtyCmd::Resize(cols, rows))
            .await
            .map_err(|_| SshError::Channel("channel closed".into()))
    }
}

async fn pty_actor(
    mut channel: Channel<client::Msg>,
    cmd_rx: flume::Receiver<PtyCmd>,
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
            cmd = cmd_rx.recv_async() => {
                match cmd {
                    Ok(PtyCmd::Write(data)) => {
                        if channel.data(&*data).await.is_err() {
                            break;
                        }
                    }
                    Ok(PtyCmd::Resize(cols, rows)) => {
                        let _ = channel
                            .window_change(cols.into(), rows.into(), 0, 0)
                            .await;
                    }
                    Err(_) => break,
                }
            }
        }
    }
}
