mod connection;
mod daemon_channel;
mod deploy;
mod error;
mod known_hosts;
mod launch;
mod pty;

pub use connection::SshConnection;
pub use daemon_channel::{DaemonChannel, DaemonClient, Handshake};
pub use deploy::{
    ensure_daemon_deployed, needs_upload, remote_binary_name, target_triple_from_uname,
    DeployOutcome,
};
pub use error::SshError;
pub use launch::{connect_or_spawn_daemon, stop_daemon};
pub use pty::{PtyStream, PtySyncReader, PtySyncWriter, PtyWriter};
