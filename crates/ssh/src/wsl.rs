//! WSL transport — the [`Connection::Wsl`](crate::transport::Connection::Wsl)
//! arm's implementation, backing the same four verbs the daemon lifecycle
//! calls on [`crate::SshConnection`] (`docs/spec-wsl-transport.md`).
//!
//! There is no persistent connection object the way SSH has one: a WSL
//! distro is already a running VM, so every verb shells out to a fresh
//! `wsl.exe -d <distro> -- <command>` child process with plain stdio pipes
//! (never a PTY, so binary framing is never corrupted). `wsl.exe`, run
//! without `-e`, hands the whole trailing argument to the distro's default
//! login shell (`<shell> -c "<command>"`) — the same "run this string via the
//! login shell" semantics an SSH exec channel already has, so the
//! transport-neutral `sh` command strings `deploy.rs`/`launch.rs` build run
//! here unchanged.
//!
//! [`WslConnection`] compiles and is exported on every target (Linux CI must
//! build it); only the actual `wsl.exe` spawn is a Windows-only runtime
//! concern — on a host without `wsl.exe` every verb simply fails with
//! [`SshError::Wsl`] or [`SshError::Io`], exactly like any other missing
//! binary. Restricting the WSL choice to the Windows build lives in the app's
//! connect-card layer (a later issue), not here.

use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::connection::exec::cat_to_executable_command;
use crate::daemon_channel::DaemonChannel;
use crate::error::SshError;

/// The Windows `wsl.exe` binary every verb shells out to.
const WSL_EXECUTABLE: &str = "wsl.exe";

/// Bytes read from the daemon child's stdout per read call.
const READ_CHUNK_SIZE: usize = 8192;

/// A WSL distro reached via local `wsl.exe` child processes — the transport
/// peer of [`crate::SshConnection`].
pub struct WslConnection {
    distro: String,
    /// Flips to `true` once the most recently opened daemon channel's
    /// `wsl.exe` child has exited (see [`WslConnection::is_closed`]).
    /// `false` before any channel is opened — mirrors
    /// [`crate::SshConnection`], whose freshly established handle is not
    /// closed either.
    closed: Arc<AtomicBool>,
}

impl WslConnection {
    /// Verify `wsl.exe` is on `PATH` and `distro` is reachable, with a cheap
    /// no-op probe (`wsl.exe -d <distro> -- true`), before handing back a
    /// connection bound to it. A missing `wsl.exe` binary or a stopped/
    /// mistyped distro both surface here as [`SshError::Wsl`], which
    /// [`SshError::is_retryable`] marks non-retryable — retrying cannot
    /// start a distro or install `wsl.exe`.
    pub async fn connect(distro: &str) -> Result<Self, SshError> {
        let output = wsl_command(distro, "true")
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| SshError::Wsl(format!("wsl.exe is not available: {e}")))?;

        if !output.status.success() {
            return Err(SshError::Wsl(format!(
                "WSL distro '{distro}' is not reachable: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        Ok(Self {
            distro: distro.to_owned(),
            closed: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Whether the `wsl.exe` child backing the most recently opened daemon
    /// channel has exited. See [`crate::SshConnection::is_closed`] for the
    /// SSH-side counterpart this mirrors.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Open a non-PTY channel carrying the `rift-protocol` framing: spawn
    /// `command` as a `wsl.exe` child with piped stdin/stdout (stderr is
    /// inherited, kept out of the frame stream) and drive its bytes through a
    /// [`DaemonChannel`], exactly like [`crate::SshConnection::open_daemon_channel`].
    pub async fn open_daemon_channel(&mut self, command: &str) -> Result<DaemonChannel, SshError> {
        let mut child = wsl_command(&self.distro, command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().expect("child spawned with piped stdin");
        let stdout = child
            .stdout
            .take()
            .expect("child spawned with piped stdout");

        let closed = Arc::new(AtomicBool::new(false));
        self.closed = closed.clone();

        let (data_tx, data_rx) = flume::unbounded();
        let (write_tx, write_rx) = flume::unbounded();
        tokio::spawn(child_actor(child, stdin, stdout, write_rx, data_tx, closed));

        Ok(DaemonChannel::from_parts(data_rx, write_tx))
    }

    /// Run `command` inside the distro and collect its stdout, mirroring
    /// [`crate::SshConnection::exec_capture`]. Errors with [`SshError::Exec`]
    /// on a non-zero exit.
    pub async fn exec_capture(&mut self, command: &str) -> Result<String, SshError> {
        let output = wsl_command(&self.distro, command)
            .stdin(Stdio::null())
            .output()
            .await?;

        if !output.status.success() {
            return Err(SshError::Exec {
                code: exit_code(output.status),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Upload `bytes` to `remote_path` inside the distro and mark it
    /// executable — the same `cat`-stream-into-a-temp-then-rename shape
    /// [`crate::SshConnection::upload_executable`] uses
    /// ([`cat_to_executable_command`]), run through a `wsl.exe` child instead
    /// of an SSH exec channel.
    pub async fn upload_executable(
        &mut self,
        bytes: &[u8],
        remote_path: &str,
    ) -> Result<(), SshError> {
        let command = cat_to_executable_command(remote_path);
        let mut child = wsl_command(&self.distro, &command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("child spawned with piped stdin");
        stdin.write_all(bytes).await?;
        drop(stdin); // close stdin so `cat` sees EOF

        let output = child.wait_with_output().await?;
        if !output.status.success() {
            return Err(SshError::Exec {
                code: exit_code(output.status),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(())
    }
}

/// Build the `wsl.exe -d <distro> -- <command>` child command. `command`
/// travels as a single trailing argument — see the module docs for why that
/// reaches the distro's login shell unsplit.
fn wsl_command(distro: &str, command: &str) -> Command {
    let mut cmd = Command::new(WSL_EXECUTABLE);
    cmd.args(wsl_argv(distro, command));
    cmd
}

/// The argv `wsl_command` passes to `wsl.exe`, split out as a pure function
/// so the exact shape is unit-tested without spawning a process.
fn wsl_argv<'a>(distro: &'a str, command: &'a str) -> Vec<&'a str> {
    vec!["-d", distro, "--", command]
}

/// Map a child's exit status to the `u32` [`SshError::Exec`] expects. A
/// missing code (killed by signal, Unix-only) or one that does not fit `u32`
/// falls back to `u32::MAX` rather than panicking — this only feeds an error
/// message, never a control-flow decision.
fn exit_code(status: ExitStatus) -> u32 {
    status
        .code()
        .and_then(|c| u32::try_from(c).ok())
        .unwrap_or(u32::MAX)
}

/// Drive one daemon channel's `wsl.exe` child to completion: relay its
/// stdout to `data_tx` and outbound writes from `write_rx` to its stdin,
/// exactly like [`crate::daemon_channel`]'s SSH channel actor. `closed` flips
/// to `true` once the loop ends for any reason (stdout EOF/error, or the
/// write side closing) — for a piped child, stdout EOF is the OS's own
/// signal that the process has exited (no one else holds the write end), and
/// `child` — spawned with `kill_on_drop`, dropped at the end of this
/// function — is force-terminated if it is somehow still alive, so `closed`
/// becoming `true` always means the `wsl.exe` child has exited.
async fn child_actor(
    child: Child,
    mut stdin: ChildStdin,
    mut stdout: ChildStdout,
    write_rx: flume::Receiver<Vec<u8>>,
    data_tx: flume::Sender<Vec<u8>>,
    closed: Arc<AtomicBool>,
) {
    let mut buf = [0u8; READ_CHUNK_SIZE];
    loop {
        tokio::select! {
            read = stdout.read(&mut buf) => {
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if data_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
            write = write_rx.recv_async() => {
                match write {
                    Ok(data) => {
                        if stdin.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
    closed.store(true, Ordering::Relaxed);
    drop(child);
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── wsl_argv ───────────────────────────────────────────────────────

    #[test]
    fn test_wsl_argv_builds_dash_d_distro_dash_dash_single_command_arg() {
        assert_eq!(
            wsl_argv("Ubuntu", "uname -sm"),
            vec!["-d", "Ubuntu", "--", "uname -sm"]
        );
    }

    #[test]
    fn test_wsl_argv_command_with_shell_metacharacters_stays_one_argv_element() {
        // The whole `sh` command string must land as ONE trailing argument
        // (so `wsl.exe` hands it to the distro's login shell as a single
        // command line), not shell-split into several argv elements.
        let argv = wsl_argv("Ubuntu-22.04", "cat > 'x' && mv 'x' 'y'");
        assert_eq!(
            argv,
            vec!["-d", "Ubuntu-22.04", "--", "cat > 'x' && mv 'x' 'y'"]
        );
        assert_eq!(argv.len(), 4);
    }

    // ── exit_code ──────────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn test_exit_code_success_status_returns_zero() {
        let status = std::process::Command::new("true")
            .status()
            .expect("run true");
        assert_eq!(exit_code(status), 0);
    }

    #[cfg(unix)]
    #[test]
    fn test_exit_code_failure_status_returns_code() {
        let status = std::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .status()
            .expect("run sh -c exit 7");
        assert_eq!(exit_code(status), 7);
    }

    // ── connect ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_connect_unreachable_distro_returns_non_retryable_wsl_error() {
        // Exercises the real `WslConnection::connect` path end-to-end: on
        // this runner `wsl.exe` is either absent (spawn fails) or present but
        // has no distro named this (non-zero exit) — both branches map to
        // `SshError::Wsl`, matching the issue's "distro-not-running /
        // wsl.exe-missing map to a NON-retryable connect error" contract.
        let err = match WslConnection::connect("rift-test-nonexistent-distro").await {
            Ok(_) => panic!("no such distro must not connect"),
            Err(err) => err,
        };
        assert!(matches!(err, SshError::Wsl(_)));
        assert!(!err.is_retryable());
    }
}
