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
