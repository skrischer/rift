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

    /// Run `command` on the remote host over a non-PTY exec channel, collect its
    /// stdout to completion, and return it as a UTF-8 string. Used for short,
    /// one-shot probes such as `uname -sm` and `test -x <path>`.
    ///
    /// Returns [`SshError::Exec`] if the command exits with a non-zero status.
    pub async fn exec_capture(&mut self, command: &str) -> Result<String, SshError> {
        let mut channel = self.handle.channel_open_session().await?;
        channel.exec(true, command).await?;

        let stdout = exec::drain_channel(&mut channel, None).await?;
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }

    /// Upload `bytes` to `remote_path` and mark it executable, streaming the
    /// payload over an exec channel into `cat > '<path>' && chmod +x '<path>'`.
    /// No SFTP/SCP dependency — the existing `russh` exec channel carries the
    /// bytes directly. The path is single-quote escaped.
    ///
    /// Returns [`SshError::Exec`] if the remote command exits with a non-zero
    /// status (e.g. unwritable target directory).
    pub async fn upload_executable(
        &mut self,
        bytes: &[u8],
        remote_path: &str,
    ) -> Result<(), SshError> {
        let command = exec::cat_to_executable_command(remote_path);
        let mut channel = self.handle.channel_open_session().await?;
        channel.exec(true, command.as_str()).await?;

        exec::drain_channel(&mut channel, Some(bytes)).await?;
        Ok(())
    }
}

/// Exec-channel plumbing shared by [`SshConnection::exec_capture`],
/// [`SshConnection::upload_executable`] and the [`crate::deploy`] commands:
/// optionally stream a stdin payload, then collect stdout while enforcing a
/// zero exit status. Stderr is collected separately for error reporting.
pub(crate) mod exec {
    use russh::client;
    use russh::{Channel, ChannelMsg};
    use tracing::warn;

    use crate::error::SshError;

    /// `sh` command body that writes stdin to `path` and makes it executable.
    /// The path is single-quote escaped so spaces and shell metacharacters in
    /// the remote path cannot break out of the quoting.
    pub(crate) fn cat_to_executable_command(path: &str) -> String {
        let quoted = shell_single_quote(path);
        format!("cat > {quoted} && chmod +x {quoted}")
    }

    /// Single-quote a string for safe embedding in a POSIX `sh` command line,
    /// escaping any embedded single quotes via the `'\''` idiom. The result is
    /// inert: nothing inside it is expanded or interpreted by the shell.
    pub(crate) fn shell_single_quote(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('\'');
        for c in s.chars() {
            if c == '\'' {
                out.push_str("'\\''");
            } else {
                out.push(c);
            }
        }
        out.push('\'');
        out
    }

    /// Drive an exec channel to completion: write `stdin` (if any) and send EOF,
    /// collect stdout and stderr, and verify the remote exit status is zero.
    /// Returns the collected stdout bytes on success.
    pub(crate) async fn drain_channel(
        channel: &mut Channel<client::Msg>,
        stdin: Option<&[u8]>,
    ) -> Result<Vec<u8>, SshError> {
        if let Some(data) = stdin {
            channel.data(data).await?;
        }
        channel.eof().await?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_status: Option<u32> = None;

        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                // `ext == 1` is the SSH stderr stream; capture it for diagnostics.
                ChannelMsg::ExtendedData { data, .. } => stderr.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status: code } => exit_status = Some(code),
                // `exit-status` is conventionally sent before `close`; keep
                // reading past `eof` so a status arriving in either order is
                // captured, and stop only when the channel actually closes.
                ChannelMsg::Close => break,
                _ => {}
            }
        }

        match exit_status {
            Some(0) => Ok(stdout),
            None => {
                warn!("exec channel closed without an exit status; assuming success");
                Ok(stdout)
            }
            Some(code) => Err(SshError::Exec {
                code,
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            }),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_shell_single_quote_plain_path_wraps_in_quotes() {
            assert_eq!(
                shell_single_quote("/tmp/rift-daemon-0.1.0"),
                "'/tmp/rift-daemon-0.1.0'"
            );
        }

        #[test]
        fn test_shell_single_quote_path_with_space_stays_single_argument() {
            assert_eq!(
                shell_single_quote("/home/my user/bin/rift"),
                "'/home/my user/bin/rift'"
            );
        }

        #[test]
        fn test_shell_single_quote_embedded_quote_is_escaped() {
            assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
        }

        #[test]
        fn test_shell_single_quote_neutralizes_expansion_and_injection() {
            // A crafted path with shell metacharacters must come back inert.
            assert_eq!(shell_single_quote("$HOME/`id`/\"x\""), "'$HOME/`id`/\"x\"'");
        }

        #[test]
        fn test_cat_to_executable_command_quotes_path_in_both_places() {
            let cmd = cat_to_executable_command("/tmp/rift daemon");
            assert_eq!(
                cmd,
                "cat > '/tmp/rift daemon' && chmod +x '/tmp/rift daemon'"
            );
        }
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
