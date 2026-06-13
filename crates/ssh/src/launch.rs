//! Daemon launch + reattach over SSH — the lifecycle behind issue #62.
//!
//! [`connect_or_spawn_daemon`] probes whether a daemon already listens on the
//! remote socket; if so it attaches (a relay channel only, no new process), and
//! if not it spawns one **detached** — so the daemon outlives this and future
//! SSH connections — waits for the socket to come up, then attaches. The
//! single-instance guarantee is enforced on both sides: the host skips the spawn
//! when the probe reports "running", and the daemon itself refuses a second bind
//! on the same socket as a race backstop.
//!
//! All remote paths are single-quoted via [`shell_single_quote`] before they
//! enter a command line, so a hostile or awkward path cannot break out of the
//! quoting or be expanded by the shell.

use tracing::debug;

use crate::connection::exec::shell_single_quote;
use crate::daemon_channel::DaemonChannel;
use crate::error::SshError;
use crate::SshConnection;

/// Marker the probe prints when a daemon is already listening.
const MARKER_RUNNING: &str = "RIFT_DAEMON_RUNNING";
/// Marker the launch command prints once the socket accepts connections.
const MARKER_READY: &str = "RIFT_DAEMON_READY";
/// Marker the launch command prints if the socket never came up in time.
const MARKER_TIMEOUT: &str = "RIFT_DAEMON_TIMEOUT";

/// How long the remote launch command waits for the socket to appear: 50 polls
/// at 0.1s ~= 5s — enough for a cold daemon start without hanging the UI.
const READY_POLL_ITERATIONS: u32 = 50;

/// Probe command: run the daemon's `--ping` against `socket_path` and print
/// [`MARKER_RUNNING`] iff a daemon answers. The `if` wrapper makes the command
/// exit zero whether or not a daemon is running, so the absence of a daemon is
/// not mistaken for a failed remote command.
fn probe_command(binary_path: &str, socket_path: &str) -> String {
    let bin = shell_single_quote(binary_path);
    let sock = shell_single_quote(socket_path);
    format!("if {bin} --ping {sock}; then printf '%s' '{MARKER_RUNNING}'; fi")
}

/// Whether the probe output indicates a running daemon.
fn daemon_is_running(probe_output: &str) -> bool {
    probe_output.trim() == MARKER_RUNNING
}

/// Detached-launch command: start the daemon in a new session (`setsid`) with
/// its stdio redirected to a log so it outlives this exec channel, then poll for
/// the socket and print [`MARKER_READY`] (or [`MARKER_TIMEOUT`]). Always exits
/// zero; readiness is reported via the marker, not the exit status.
///
/// When `root` is set, a single-quoted `--root <path>` is appended to the daemon
/// invocation so it watches that directory; absent, the flag is omitted and the
/// daemon falls back to its launch directory.
fn launch_command(
    binary_path: &str,
    socket_path: &str,
    log_path: &str,
    root: Option<&str>,
) -> String {
    let bin = shell_single_quote(binary_path);
    let sock = shell_single_quote(socket_path);
    let log = shell_single_quote(log_path);
    // The watched-root flag sits before the redirections so the shell hands it to
    // the daemon as an argument instead of swallowing it into the redirect; it is
    // single-quoted like every other remote path. Omitted entirely when unset.
    let root_arg = match root {
        Some(root) => format!(" --root {}", shell_single_quote(root)),
        None => String::new(),
    };
    // Inner command handed to `setsid sh -c`: `exec` replaces the shell with the
    // daemon so the new session leader *is* the daemon; stdin from /dev/null and
    // stdout/stderr appended to the log detach it from this channel's FDs, so
    // the daemon keeps running once the channel closes.
    let inner = shell_single_quote(&format!(
        "exec {bin} --serve-uds {sock}{root_arg} </dev/null >> {log} 2>&1"
    ));
    format!(
        "setsid sh -c {inner} >/dev/null 2>&1 & \
         i=0; while [ $i -lt {READY_POLL_ITERATIONS} ]; do \
         [ -S {sock} ] && {{ printf '%s' '{MARKER_READY}'; exit 0; }}; \
         i=$((i+1)); sleep 0.1; done; printf '%s' '{MARKER_TIMEOUT}'"
    )
}

/// Map the launch command's output to success or a launch error.
fn launch_succeeded(launch_output: &str) -> Result<(), SshError> {
    if launch_output.trim() == MARKER_READY {
        Ok(())
    } else {
        Err(SshError::DaemonLaunch(format!(
            "socket did not come up within {READY_POLL_ITERATIONS} polls (output: {:?})",
            launch_output.trim()
        )))
    }
}

/// Relay command run over the daemon channel: connect the channel's stdio to the
/// running daemon's socket via the daemon binary's `--connect` mode.
fn relay_command(binary_path: &str, socket_path: &str) -> String {
    let bin = shell_single_quote(binary_path);
    let sock = shell_single_quote(socket_path);
    format!("{bin} --connect {sock}")
}

/// Attach to a running daemon at `socket_path`, or spawn one detached and then
/// attach — returning a [`DaemonChannel`] that carries the `rift-protocol`
/// framing to the persistent daemon.
///
/// Probe first: when a daemon already listens, no second process is started (the
/// reattach contract). Otherwise launch it detached so it survives this and
/// future SSH connections, wait for the socket, then open the relay channel.
/// `binary_path` is the resolved remote daemon path (see
/// [`crate::ensure_daemon_deployed`]); `log_path` receives the detached daemon's
/// stdio. `root`, when set, is the project directory the daemon watches; it only
/// takes effect on a fresh spawn — a reattach keeps the running daemon's root.
pub async fn connect_or_spawn_daemon(
    conn: &mut SshConnection,
    binary_path: &str,
    socket_path: &str,
    log_path: &str,
    root: Option<&str>,
) -> Result<DaemonChannel, SshError> {
    let probe = conn
        .exec_capture(&probe_command(binary_path, socket_path))
        .await?;
    if daemon_is_running(&probe) {
        debug!(socket_path, "daemon already running, reattaching");
    } else {
        debug!(socket_path, "no daemon running, spawning detached");
        let launched = conn
            .exec_capture(&launch_command(binary_path, socket_path, log_path, root))
            .await?;
        launch_succeeded(&launched)?;
    }

    conn.open_daemon_channel(&relay_command(binary_path, socket_path))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_probe_command_quotes_paths_and_exits_zero() {
        assert_eq!(
            probe_command("/h/rift-daemon-0.1.0", "/h/d.sock"),
            "if '/h/rift-daemon-0.1.0' --ping '/h/d.sock'; then printf '%s' 'RIFT_DAEMON_RUNNING'; fi"
        );
    }

    #[test]
    fn test_probe_command_neutralizes_injection() {
        let cmd = probe_command("/h/bin", "/h/$(touch pwned).sock");
        assert!(cmd.contains("'/h/$(touch pwned).sock'"));
        assert!(!cmd.contains("$(touch pwned)`"));
    }

    #[test]
    fn test_daemon_is_running_only_on_running_marker() {
        assert!(daemon_is_running("RIFT_DAEMON_RUNNING"));
        assert!(daemon_is_running("RIFT_DAEMON_RUNNING\n"));
        assert!(!daemon_is_running(""));
        assert!(!daemon_is_running("RIFT_DAEMON_ABSENT"));
    }

    #[test]
    fn test_launch_command_detaches_serves_and_polls() {
        let cmd = launch_command("/h/rift-daemon-0.1.0", "/h/d.sock", "/h/d.log", None);
        // Detached via setsid, daemon serves the socket with stdio redirected
        // away from this channel (the inner command is single-quoted for the
        // `sh -c`, so its own quoting is escaped — these are the surviving
        // structural pieces; exact quoting is pinned by the injection test).
        assert!(cmd.starts_with("setsid sh -c "));
        assert!(cmd.contains("--serve-uds"));
        assert!(cmd.contains("</dev/null"));
        assert!(cmd.contains("/h/d.log"));
        // Backgrounded, then a readiness poll on the socket emitting a marker.
        assert!(cmd.contains(" & "));
        assert!(cmd.contains("[ -S '/h/d.sock' ]"));
        assert!(cmd.contains("printf '%s' 'RIFT_DAEMON_READY'"));
        assert!(cmd.contains("RIFT_DAEMON_TIMEOUT"));
    }

    #[test]
    fn test_launch_command_neutralizes_injection() {
        // A path with shell metacharacters stays inside single quotes. This
        // checks the poll-loop occurrence (`[ -S '...' ]`); the same path inside
        // the `setsid sh -c '...'` argument is single-quoted again and therefore
        // doubly-escaped, so it cannot break out there either.
        let cmd = launch_command("/h/bin", "/h/`id`.sock", "/h/d.log", None);
        assert!(cmd.contains("'/h/`id`.sock'"));
    }

    #[test]
    fn test_launch_command_with_root_emits_flag_before_redirect() {
        let cmd = launch_command("/h/bin", "/h/d.sock", "/h/d.log", Some("/srv/project"));
        // The root path is single-quoted inside the inner command and therefore
        // doubly-escaped in the final `setsid sh -c '...'` string. The flag must
        // precede `</dev/null` or the shell would treat it as part of the redirect.
        assert!(cmd.contains("--root '\\''/srv/project'\\'' </dev/null"));
    }

    #[test]
    fn test_launch_command_without_root_omits_flag() {
        let cmd = launch_command("/h/bin", "/h/d.sock", "/h/d.log", None);
        assert!(!cmd.contains("--root"));
    }

    #[test]
    fn test_launch_command_root_neutralizes_injection() {
        // A root path with shell metacharacters stays single-quoted inside the
        // `setsid sh -c` inner command (doubly-escaped in the final string), so it
        // cannot break out — mirroring the socket-path injection guard above.
        let cmd = launch_command("/h/bin", "/h/d.sock", "/h/d.log", Some("/h/$(touch pwned)"));
        assert!(cmd.contains("--root '\\''/h/$(touch pwned)'\\''"));
    }

    #[test]
    fn test_launch_succeeded_maps_marker_to_result() {
        assert!(launch_succeeded("RIFT_DAEMON_READY").is_ok());
        assert!(launch_succeeded("RIFT_DAEMON_READY\n").is_ok());
        assert!(matches!(
            launch_succeeded("RIFT_DAEMON_TIMEOUT"),
            Err(SshError::DaemonLaunch(_))
        ));
        assert!(matches!(
            launch_succeeded(""),
            Err(SshError::DaemonLaunch(_))
        ));
    }

    #[test]
    fn test_relay_command_quotes_path() {
        assert_eq!(
            relay_command("/h/rift-daemon-0.1.0", "/h/d.sock"),
            "'/h/rift-daemon-0.1.0' --connect '/h/d.sock'"
        );
    }
}
