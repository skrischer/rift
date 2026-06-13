/// Errors surfaced by the LSP client layer.
///
/// Library code returns these via [`Result`](crate::Result); the daemon binary
/// adapts them into `anyhow` at its boundary (constitution: `thiserror` in
/// libs, `anyhow` in binaries).
#[derive(Debug, thiserror::Error)]
pub enum LspError {
    /// The language-server binary could not be spawned (e.g. not on `$PATH`).
    #[error("failed to spawn language server `{server}`: {source}")]
    Spawn {
        server: String,
        source: std::io::Error,
    },

    /// A document's on-disk content could not be read to feed the server.
    #[error("failed to read `{path}`: {source}")]
    ReadDocument {
        path: String,
        source: std::io::Error,
    },

    /// A worktree-root path could not be turned into a `file://` URI, which
    /// every LSP request needs. `lsp_types::Url::from_file_path` reports this
    /// only as `()`, so the offending path is carried for diagnosis.
    #[error("path is not a valid file URI: {0}")]
    InvalidUri(String),

    /// The JSON-RPC main loop or a request/notification on the server socket
    /// failed.
    #[error("language-server protocol error: {0}")]
    Protocol(#[from] async_lsp::Error),

    /// The server's notification stream ended before publishing the awaited
    /// diagnostics (the main loop stopped or the server exited).
    #[error("language server closed before publishing diagnostics")]
    ServerClosed,

    /// The server did not publish the awaited diagnostics within the timeout.
    #[error("timed out waiting for diagnostics after {0:?}")]
    Timeout(std::time::Duration),
}
