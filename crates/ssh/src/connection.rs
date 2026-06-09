use std::path::Path;
use std::sync::Arc;

use russh::client::{self, Config, Handle};
use russh_keys::key::PublicKey;
use tracing::{debug, info};

use crate::daemon_channel::DaemonChannel;
use crate::error::SshError;
use crate::known_hosts::verify_host_key;
use crate::pty::PtyStream;

pub struct SshConnection {
    handle: Handle<ClientHandler>,
}

impl SshConnection {
    pub async fn connect(
        host: &str,
        port: u16,
        user: &str,
        key_path: &Path,
    ) -> Result<Self, SshError> {
        let key_exists = key_path.exists();
        debug!(
            path = %key_path.display(),
            exists = key_exists,
            "loading SSH key"
        );
        let path = key_path.to_path_buf();
        let key_pair =
            tokio::task::spawn_blocking(move || russh_keys::load_secret_key(&path, None))
                .await
                .map_err(|e| SshError::Key(e.to_string()))??;

        let config = Arc::new(Config::default());
        let handler = ClientHandler {
            host: host.to_owned(),
            port,
        };

        debug!(%host, port, "establishing SSH connection");
        let mut handle = client::connect(config, (host, port), handler).await?;

        debug!(%user, "authenticating");
        let auth_result = handle
            .authenticate_publickey(user, Arc::new(key_pair))
            .await?;

        if !auth_result {
            return Err(SshError::Auth(format!(
                "public key authentication failed for user '{user}'"
            )));
        }

        info!(%host, port, %user, "SSH connection established");
        Ok(Self { handle })
    }

    pub async fn open_pty(&mut self, cols: u16, rows: u16) -> Result<PtyStream, SshError> {
        let channel = self.handle.channel_open_session().await?;

        channel
            .request_pty(false, "xterm-256color", cols.into(), rows.into(), 0, 0, &[])
            .await?;

        channel.request_shell(false).await?;

        Ok(PtyStream::new(channel))
    }

    pub async fn open_pty_exec(
        &mut self,
        cols: u16,
        rows: u16,
        command: &str,
    ) -> Result<PtyStream, SshError> {
        let channel = self.handle.channel_open_session().await?;

        channel
            .request_pty(false, "xterm-256color", cols.into(), rows.into(), 0, 0, &[])
            .await?;

        channel.exec(true, command).await?;

        Ok(PtyStream::new(channel))
    }

    /// Open a non-PTY exec channel that carries the `rift-protocol` framing.
    ///
    /// Runs `command` (the remote daemon) over a plain session channel — no PTY,
    /// no shell — so its stdin/stdout become the daemon transport. The returned
    /// [`DaemonChannel`] is the byte half of the client-side transport seam.
    pub async fn open_daemon_channel(&mut self, command: &str) -> Result<DaemonChannel, SshError> {
        let channel = self.handle.channel_open_session().await?;
        channel.exec(true, command).await?;
        Ok(DaemonChannel::new(channel))
    }
}

struct ClientHandler {
    host: String,
    port: u16,
}

#[async_trait::async_trait]
impl client::Handler for ClientHandler {
    type Error = SshError;

    async fn check_server_key(&mut self, key: &PublicKey) -> Result<bool, Self::Error> {
        verify_host_key(&self.host, self.port, key)?;
        Ok(true)
    }
}
