use russh::client;
use russh::Channel;

use crate::error::SshError;

pub struct PtyStream {
    channel: Channel<client::Msg>,
}

impl PtyStream {
    pub(crate) fn new(channel: Channel<client::Msg>) -> Self {
        Self { channel }
    }

    pub async fn read(&mut self) -> Result<Vec<u8>, SshError> {
        loop {
            let msg = self.channel.wait().await;
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
        self.channel
            .data(data)
            .await
            .map_err(|e| SshError::Pty(e.to_string()))
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshError> {
        self.channel
            .window_change(cols.into(), rows.into(), 0, 0)
            .await
            .map_err(|e| SshError::Pty(e.to_string()))
    }
}
