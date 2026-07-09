use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, Config, Handle};
use russh_keys::key::PublicKey;
use tracing::{debug, info};

use crate::daemon_channel::DaemonChannel;
use crate::error::SshError;
use crate::known_hosts::verify_host_key;
use crate::pty::PtyStream;

/// Send a keepalive probe after this long without receiving server data.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Close the connection after this many unanswered keepalives. Matches the
/// OpenSSH `ServerAliveCountMax` default; together with the interval a silent
/// network drop surfaces as a disconnect within roughly one minute instead of
/// leaving the session frozen in "connected" forever.
const KEEPALIVE_MAX: usize = 3;

/// Client config for all rift SSH connections: `Config::default()` with
/// keepalive enabled so dead transports are detected.
fn client_config() -> Config {
    Config {
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        keepalive_max: KEEPALIVE_MAX,
        ..Config::default()
    }
}

/// Whether the private key at `path` is passphrase-protected (issue #478,
/// `docs/spec-connection-robustness.md`): attempts to load it with no
/// password and reports `Ok(true)` only for the specific "needs a password"
/// failure (`russh_keys::Error::KeyIsEncrypted`), never for a missing file,
/// an unsupported format, or a corrupt key — those surface as `Err` so the
/// real connect attempt reports them properly instead of this probe
/// misreporting them as "show the passphrase field". A cheap, synchronous
/// parse (no KDF/decrypt runs when no password is supplied), safe to call
/// from a UI thread the same way [`Path::exists`] already is at the connect
/// call site.
pub fn key_requires_passphrase(path: &Path) -> Result<bool, SshError> {
    match russh_keys::load_secret_key(path, None) {
        Ok(_) => Ok(false),
        Err(russh_keys::Error::KeyIsEncrypted) => Ok(true),
        Err(e) => Err(SshError::from(e)),
    }
}

pub struct SshConnection {
    handle: Handle<ClientHandler>,
    remote_exec_wrapper: Option<String>,
}

impl SshConnection {
    pub async fn connect(
        host: &str,
        port: u16,
        user: &str,
        key_path: &Path,
        passphrase: Option<&str>,
    ) -> Result<Self, SshError> {
        let key_exists = key_path.exists();
        debug!(
            path = %key_path.display(),
            exists = key_exists,
            "loading SSH key"
        );
        let path = key_path.to_path_buf();
        let passphrase = passphrase.map(str::to_owned);
        let key_pair = tokio::task::spawn_blocking(move || {
            russh_keys::load_secret_key(&path, passphrase.as_deref())
        })
        .await
        .map_err(|e| SshError::Key(e.to_string()))??;

        let config = Arc::new(client_config());
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
        Ok(Self {
            handle,
            remote_exec_wrapper: None,
        })
    }

    /// Set the remote exec wrapper (`RIFT_REMOTE_EXEC_WRAPPER`), e.g.
    /// `docker exec -i devenv`, so every non-PTY exec command
    /// ([`Self::open_daemon_channel`], [`Self::exec_capture`],
    /// [`Self::upload_executable`]) runs one hop deeper — inside a container, a
    /// WSL distro, or under a jump user — instead of the host login shell. A
    /// setter/builder rather than a `connect()` parameter, so callers that don't
    /// need the wrapper are unaffected. `None` (the default) is byte-for-byte
    /// passthrough — see [`exec::wrap_command`].
    pub fn with_remote_exec_wrapper(mut self, wrapper: Option<String>) -> Self {
        self.remote_exec_wrapper = wrapper;
        self
    }

    /// Whether the underlying SSH transport has closed (dropped connection,
    /// exhausted keepalive window). Cheap and non-blocking — the daemon
    /// reconnect engine checks it to abort early and hand a dead transport to
    /// the SSH-level reconnect loop (#476,
    /// `docs/spec-connection-robustness.md`) instead of burning its bounded
    /// attempts against a connection that cannot carry a channel.
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
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
        let wrapped = exec::wrap_command(self.remote_exec_wrapper.as_deref(), command);
        let channel = self.handle.channel_open_session().await?;
        channel.exec(true, wrapped.as_str()).await?;
        Ok(DaemonChannel::new(channel))
    }

    /// Run `command` on the remote host over a non-PTY exec channel, collect its
    /// stdout to completion, and return it as a UTF-8 string. Used for short,
    /// one-shot probes such as `uname -sm` and `test -x <path>`.
    ///
    /// Returns [`SshError::Exec`] if the command exits with a non-zero status.
    pub async fn exec_capture(&mut self, command: &str) -> Result<String, SshError> {
        let wrapped = exec::wrap_command(self.remote_exec_wrapper.as_deref(), command);
        let mut channel = self.handle.channel_open_session().await?;
        channel.exec(true, wrapped.as_str()).await?;

        let stdout = exec::drain_channel(&mut channel, None).await?;
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }

    /// Upload `bytes` to `remote_path` and mark it executable. The payload is
    /// streamed over an exec channel into a temporary sibling path, made
    /// executable, then atomically renamed over `remote_path`. The rename
    /// succeeds even while a process is executing the old `remote_path` (it
    /// keeps its inode), so a re-deploy never fails with `ETXTBSY`. No SFTP/SCP
    /// dependency — the existing `russh` exec channel carries the bytes
    /// directly. Both paths are single-quote escaped.
    ///
    /// Returns [`SshError::Exec`] if the remote command exits with a non-zero
    /// status (e.g. unwritable target directory).
    pub async fn upload_executable(
        &mut self,
        bytes: &[u8],
        remote_path: &str,
    ) -> Result<(), SshError> {
        let command = exec::cat_to_executable_command(remote_path);
        let wrapped = exec::wrap_command(self.remote_exec_wrapper.as_deref(), &command);
        let mut channel = self.handle.channel_open_session().await?;
        channel.exec(true, wrapped.as_str()).await?;

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

    /// `sh` command body that writes stdin to a temporary sibling of `path`,
    /// makes it executable, then atomically renames it over `path`. Writing to
    /// `<path>.tmp` and `mv -f`-ing into place avoids `ETXTBSY`: a running
    /// process executing the old `path` keeps its inode while the new binary
    /// takes the name. Both the temp path and the target are single-quote
    /// escaped so spaces and shell metacharacters cannot break out of the
    /// quoting.
    pub(crate) fn cat_to_executable_command(path: &str) -> String {
        let quoted = shell_single_quote(path);
        let quoted_tmp = shell_single_quote(&format!("{path}.tmp"));
        format!("cat > {quoted_tmp} && chmod +x {quoted_tmp} && mv -f {quoted_tmp} {quoted}")
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

    /// Apply the remote exec wrapper (`RIFT_REMOTE_EXEC_WRAPPER`, e.g.
    /// `docker exec -i devenv`) to `command`, so daemon exec commands run one
    /// hop deeper — inside a container, a WSL distro, or under a jump user —
    /// instead of directly in the host login shell
    /// (`docs/spec-remote-exec-wrapper.md`).
    ///
    /// When `wrapper` is set (non-empty after trimming), `command` is relocated
    /// into the wrapper's target as an argument to its `sh -c`:
    /// `<wrapper> sh -c '<command>'`. The wrapper tokens are spliced **raw** so
    /// the host shell word-splits them into argv (`docker exec -i devenv` ->
    /// `docker`, `exec`, `-i`, `devenv`, `sh`, `-c`, `<command>`); only
    /// `command` is single-quoted via [`shell_single_quote`] — the same
    /// double-escape idiom [`crate::launch`] uses for `setsid sh -c` — so the
    /// composed command (`>`, `&&`, `if`/`fi`, `$HOME`) is parsed by the
    /// wrapper target's shell, not the host's.
    ///
    /// When `wrapper` is `None`, empty, or whitespace-only, `command` is
    /// returned unchanged — byte-for-byte passthrough, no `sh -c` added.
    pub(crate) fn wrap_command(wrapper: Option<&str>, command: &str) -> String {
        match wrapper {
            Some(w) if !w.trim().is_empty() => {
                format!("{w} sh -c {}", shell_single_quote(command))
            }
            _ => command.to_owned(),
        }
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
        fn test_cat_to_executable_command_writes_temp_then_renames_over_target() {
            let cmd = cat_to_executable_command("/tmp/rift daemon");
            assert_eq!(
                cmd,
                "cat > '/tmp/rift daemon.tmp' && chmod +x '/tmp/rift daemon.tmp' \
                 && mv -f '/tmp/rift daemon.tmp' '/tmp/rift daemon'"
            );
        }

        #[test]
        fn test_cat_to_executable_command_neutralizes_injection() {
            // A crafted path must be inert in both the temp and target positions.
            let cmd = cat_to_executable_command("/tmp/$(touch pwned)");
            assert_eq!(
                cmd,
                "cat > '/tmp/$(touch pwned).tmp' && chmod +x '/tmp/$(touch pwned).tmp' \
                 && mv -f '/tmp/$(touch pwned).tmp' '/tmp/$(touch pwned)'"
            );
        }

        // ── wrap_command ──────────────────────────────────────────────────

        #[test]
        fn test_wrap_command_no_wrapper_returns_command_unchanged() {
            assert_eq!(wrap_command(None, "echo hi"), "echo hi");
        }

        #[test]
        fn test_wrap_command_empty_or_whitespace_wrapper_returns_command_unchanged() {
            assert_eq!(wrap_command(Some(""), "echo hi"), "echo hi");
            assert_eq!(wrap_command(Some("   "), "echo hi"), "echo hi");
        }

        #[test]
        fn test_wrap_command_with_wrapper_relocates_command_into_sh_c() {
            let command = "cat > 'x' && mv 'x' 'y'";
            let wrapped = wrap_command(Some("docker exec -i c"), command);
            // Hardcoded doubly-escaped literal (not re-derived via
            // shell_single_quote) so a regression inside shell_single_quote
            // itself would be caught here.
            assert_eq!(
                wrapped,
                "docker exec -i c sh -c 'cat > '\\''x'\\'' && mv '\\''x'\\'' '\\''y'\\'''"
            );
            // Wrapper tokens are spliced raw (word-split by the host shell);
            // only the command is quoted.
            assert!(wrapped.starts_with("docker exec -i c sh -c "));
        }

        #[test]
        fn test_wrap_command_neutralizes_injection_inside_quoted_command() {
            let command = "echo $(touch pwned) `whoami`";
            let wrapped = wrap_command(Some("docker exec -i c"), command);
            // Hardcoded literal: the command has no embedded single quotes, so
            // shell_single_quote wraps it verbatim in one pair of quotes.
            assert_eq!(
                wrapped,
                "docker exec -i c sh -c 'echo $(touch pwned) `whoami`'"
            );
            // Prove the metacharacters sit INSIDE the single-quoted argument
            // (bounded by a literal `'` immediately before and after), not
            // merely present somewhere in the output — `.contains("$(touch
            // pwned)")` alone would also pass for an injectable unquoted
            // command, so it does not prove inertness by itself.
            assert!(wrapped.contains("'echo $(touch pwned) `whoami`'"));
        }

        #[test]
        fn test_wrap_command_wraps_cat_to_executable_command_upload_body() {
            // Shipping-depth: the actual upload_executable command body, wrapped.
            let upload_cmd = cat_to_executable_command("/h/rift-daemon-0.1.0");
            let wrapped = wrap_command(Some("docker exec -i devenv"), &upload_cmd);
            assert_eq!(
                wrapped,
                format!(
                    "docker exec -i devenv sh -c {}",
                    shell_single_quote(&upload_cmd)
                )
            );
            assert!(wrapped.starts_with("docker exec -i devenv sh -c "));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_config_keepalive_enabled_interval_and_max_set() {
        let config = client_config();
        assert_eq!(config.keepalive_interval, Some(KEEPALIVE_INTERVAL));
        assert_eq!(config.keepalive_max, KEEPALIVE_MAX);
    }

    #[test]
    fn test_client_config_keepalive_max_nonzero_enforces_bounded_window() {
        // russh treats `keepalive_max == 0` as "never give up"; the whole point
        // of this config is a bounded detection window, so guard against it.
        assert!(client_config().keepalive_max > 0);
    }

    // ── key_requires_passphrase ──────────────────────────────────────────
    //
    // Fixtures below are real `ssh-keygen -t ed25519` output (no live secret,
    // just a throwaway keypair generated for this test), exercising the exact
    // OpenSSH-format encrypted/plain shapes a developer's real `~/.ssh` key
    // would have — the same format russh_keys' own `KeyIsEncrypted` path
    // targets (`format/openssh.rs`).

    const PLAIN_ED25519_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACDD+Spx6Grs23TtvNlnEgT2ZvRWq6IGz3+w318y0vAe5wAAAIioE9c+qBPX
PgAAAAtzc2gtZWQyNTUxOQAAACDD+Spx6Grs23TtvNlnEgT2ZvRWq6IGz3+w318y0vAe5w
AAAECfLgpKaZM2WCQOK+K561MNE0reaXGkQxF+LfZm9eJrbsP5KnHoauzbdO282WcSBPZm
9FarogbPf7DfXzLS8B7nAAAAAAECAwQF
-----END OPENSSH PRIVATE KEY-----
";

    const ENCRYPTED_ED25519_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABA1nA1UjL
/BxU/vOLR2n8WtAAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIBTskMRnp64+FGOU
vHVkvR7+pmsv8ayZd9OzYo32D1vrAAAAkDJbty+E0n7yTQ5NEBbe2SIW0Izkk7aMc9mYgh
idatDXrAohqSsRJREqRkTEJJWxObn3AO1WA8j+KwIbI4842uVHjzmeXiT2F4c2RPcpEiVL
hTyMJu70Hu4ysIYhC+jhtY6kDNquv0P5q5/z0sy+DMB6tQl9uxrjc6HAD3n1ZiYVA8xSGa
2AAbPYqAztDXtY1w==
-----END OPENSSH PRIVATE KEY-----
";

    fn write_fixture(dir: &tempfile::TempDir, name: &str, contents: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).expect("failed to write key fixture");
        path
    }

    #[test]
    fn test_key_requires_passphrase_plain_key_returns_false() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        let path = write_fixture(&dir, "id_ed25519", PLAIN_ED25519_KEY);

        assert!(!key_requires_passphrase(&path).expect("plain key should load"));
    }

    #[test]
    fn test_key_requires_passphrase_encrypted_key_returns_true() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        let path = write_fixture(&dir, "id_ed25519_enc", ENCRYPTED_ED25519_KEY);

        assert!(key_requires_passphrase(&path).expect("encrypted key should probe cleanly"));
    }

    #[test]
    fn test_key_requires_passphrase_malformed_key_returns_err() {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        let path = write_fixture(&dir, "garbage", "not a key\n");

        assert!(key_requires_passphrase(&path).is_err());
    }

    #[test]
    fn test_key_requires_passphrase_missing_file_returns_err() {
        let path = std::path::Path::new("/nonexistent/rift-test-key");

        assert!(key_requires_passphrase(path).is_err());
    }
}
