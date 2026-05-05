use std::sync::Arc;

use russh::client;
use russh::Channel;
use tokio::sync::Mutex;

use crate::error::SshError;

pub struct PtyStream {
    channel: Arc<Mutex<Channel<client::Msg>>>,
}

impl PtyStream {
    pub(crate) fn new(channel: Channel<client::Msg>) -> Self {
        Self {
            channel: Arc::new(Mutex::new(channel)),
        }
    }

    pub async fn read(&self) -> Result<Vec<u8>, SshError> {
        let mut channel = self.channel.lock().await;
        loop {
            let msg = channel.wait().await;
            match msg {
                Some(russh::ChannelMsg::Data { data }) => {
                    return Ok(data.to_vec());
                }
                Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                    return Ok(data.to_vec());
                }
                Some(russh::ChannelMsg::Eof | russh::ChannelMsg::Close) | None => {
                    return Err(SshError::Channel("channel closed".into()));
                }
                Some(_) => continue,
            }
        }
    }

    pub async fn write(&self, data: &[u8]) -> Result<(), SshError> {
        let channel = self.channel.lock().await;
        channel
            .data(data)
            .await
            .map_err(|e| SshError::Pty(e.to_string()))
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshError> {
        let channel = self.channel.lock().await;
        channel
            .window_change(cols.into(), rows.into(), 0, 0)
            .await
            .map_err(|e| SshError::Pty(e.to_string()))
    }

    pub fn clone_writer(&self) -> PtyWriter {
        PtyWriter {
            channel: self.channel.clone(),
        }
    }
}

#[derive(Clone)]
pub struct PtyWriter {
    channel: Arc<Mutex<Channel<client::Msg>>>,
}

impl PtyWriter {
    pub async fn write(&self, data: &[u8]) -> Result<(), SshError> {
        let channel = self.channel.lock().await;
        channel
            .data(data)
            .await
            .map_err(|e| SshError::Pty(e.to_string()))
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshError> {
        let channel = self.channel.lock().await;
        channel
            .window_change(cols.into(), rows.into(), 0, 0)
            .await
            .map_err(|e| SshError::Pty(e.to_string()))
    }
}
