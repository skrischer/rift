#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error("connection failed: {0}")]
    Connection(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("key loading failed: {0}")]
    Key(String),
    #[error("channel error: {0}")]
    Channel(String),
    #[error("pty error: {0}")]
    Pty(String),
    #[error("remote command exited with status {code}: {stderr}")]
    Exec { code: u32, stderr: String },
    #[error("unsupported remote platform '{0}' (no daemon binary)")]
    UnsupportedPlatform(String),
    #[error("daemon launch did not become ready: {0}")]
    DaemonLaunch(String),
    #[error("timed out after {0:?} waiting for a daemon message")]
    RecvTimeout(std::time::Duration),
    #[error("daemon handshake failed: {0}")]
    Handshake(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "host key mismatch for {host}: the server key has changed since it was last recorded \
         (known_hosts line {line}). This could indicate a man-in-the-middle attack."
    )]
    HostKeyMismatch { host: String, line: usize },
    #[error("known_hosts error: {0}")]
    KnownHosts(String),
}

impl SshError {
    /// Whether the SSH-level reconnect loop may retry after this failure
    /// (`docs/spec-connection-robustness.md`, gate decision 2026-07-05).
    ///
    /// Transport-shaped deaths (dropped connection, dead channel, remote I/O,
    /// timeouts) are worth retrying — the outage may heal. Deterministic
    /// auth/config failures (bad key, refused auth, host-key/known_hosts
    /// problems) are not: retrying cannot fix them, and hiding them behind a
    /// retry banner would mask real misconfiguration.
    pub fn is_retryable(&self) -> bool {
        !matches!(
            self,
            SshError::Auth(_)
                | SshError::Key(_)
                | SshError::HostKeyMismatch { .. }
                | SshError::KnownHosts(_)
        )
    }
}

impl From<russh::Error> for SshError {
    fn from(e: russh::Error) -> Self {
        SshError::Connection(e.to_string())
    }
}

impl From<russh_keys::Error> for SshError {
    fn from(e: russh_keys::Error) -> Self {
        SshError::Key(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_retryable_transport_failures_return_true() {
        assert!(SshError::Connection("connection reset by peer".into()).is_retryable());
        assert!(SshError::Channel("channel closed".into()).is_retryable());
        assert!(SshError::Io(std::io::Error::other("broken pipe")).is_retryable());
        assert!(SshError::RecvTimeout(std::time::Duration::from_secs(10)).is_retryable());
        assert!(SshError::DaemonLaunch("socket never appeared".into()).is_retryable());
    }

    #[test]
    fn test_is_retryable_auth_and_config_failures_return_false() {
        assert!(!SshError::Auth("public key authentication failed".into()).is_retryable());
        assert!(!SshError::Key("bad passphrase".into()).is_retryable());
        assert!(!SshError::HostKeyMismatch {
            host: "vps".into(),
            line: 3,
        }
        .is_retryable());
        assert!(!SshError::KnownHosts("unreadable known_hosts".into()).is_retryable());
    }
}
