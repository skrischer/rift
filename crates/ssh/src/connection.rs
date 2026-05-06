use std::path::Path;
use std::sync::Arc;

use russh::client::{self, Config, Handle};
use russh_keys::key::PublicKey;
use tracing::{debug, info};

use crate::error::SshError;
use crate::known_hosts::{verify_host_key, HostKeyVerification};
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
        debug!(
            path = %key_path.display(),
            exists = key_path.exists(),
            "loading SSH key"
        );
        let key_pair = russh_keys::load_secret_key(key_path, None)?;

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
}

struct ClientHandler {
    host: String,
    port: u16,
}

#[async_trait::async_trait]
impl client::Handler for ClientHandler {
    type Error = SshError;

    async fn check_server_key(&mut self, key: &PublicKey) -> Result<bool, Self::Error> {
        match verify_host_key(&self.host, self.port, key)? {
            HostKeyVerification::Matched | HostKeyVerification::TrustedOnFirstUse => Ok(true),
            HostKeyVerification::Mismatch { line } => Err(SshError::HostKeyMismatch {
                host: self.host.clone(),
                line,
            }),
        }
    }
}
