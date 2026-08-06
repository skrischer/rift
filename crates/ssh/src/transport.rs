//! Transport seam over the four verbs the daemon lifecycle needs.
//!
//! [`Connection`] wraps a concrete transport (currently only [`SshConnection`])
//! behind one type so `deploy.rs`, `launch.rs`, and the app's
//! provisioning/reconnect code depend on the seam, not on SSH concretely
//! (`docs/spec-wsl-transport.md`). The four verbs — [`Connection::exec_capture`],
//! [`Connection::upload_executable`], [`Connection::open_daemon_channel`],
//! [`Connection::is_closed`] — are exactly what the daemon lifecycle calls; the
//! dead PTY path (`SshConnection::open_pty`/`open_pty_exec`) is not part of the
//! contract and stays reachable only through the concrete type.
//!
//! An `enum` rather than a trait: a fixed, small set of variants (SSH today, a
//! WSL arm in a following issue) sidesteps the `async_trait`/`&mut self`
//! object-safety friction a trait would need for no benefit at this size.
//!
//! No unit tests here: every method is a single match arm forwarding to the
//! already-existing [`SshConnection`] method of the same name, and
//! `SshConnection`'s `handle` field can only come from a live
//! `client::connect` — exactly the reason none of `SshConnection`'s own
//! transport methods (`connect`, `exec_capture`, `open_daemon_channel`,
//! `upload_executable`) carry a unit test in this crate either.

use crate::connection::SshConnection;
use crate::daemon_channel::DaemonChannel;
use crate::error::SshError;

/// A pluggable transport carrying the daemon lifecycle's four verbs. SSH is
/// the only variant today; behavior is identical to calling the wrapped
/// [`SshConnection`] directly.
pub enum Connection {
    Ssh(SshConnection),
}

impl Connection {
    /// Whether the underlying transport has closed. See
    /// [`SshConnection::is_closed`].
    pub fn is_closed(&self) -> bool {
        match self {
            Connection::Ssh(conn) => conn.is_closed(),
        }
    }

    /// Open a non-PTY exec channel carrying the `rift-protocol` framing. See
    /// [`SshConnection::open_daemon_channel`].
    pub async fn open_daemon_channel(&mut self, command: &str) -> Result<DaemonChannel, SshError> {
        match self {
            Connection::Ssh(conn) => conn.open_daemon_channel(command).await,
        }
    }

    /// Run `command` on the remote host and collect its stdout. See
    /// [`SshConnection::exec_capture`].
    pub async fn exec_capture(&mut self, command: &str) -> Result<String, SshError> {
        match self {
            Connection::Ssh(conn) => conn.exec_capture(command).await,
        }
    }

    /// Upload `bytes` to `remote_path` and mark it executable. See
    /// [`SshConnection::upload_executable`].
    pub async fn upload_executable(
        &mut self,
        bytes: &[u8],
        remote_path: &str,
    ) -> Result<(), SshError> {
        match self {
            Connection::Ssh(conn) => conn.upload_executable(bytes, remote_path).await,
        }
    }
}
