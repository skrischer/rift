//! Auto-deploy of the remote `rift-daemon` binary.
//!
//! Detects the remote platform via `uname -sm`, resolves the expected versioned
//! binary path, and uploads the locally-built musl binary only when that path is
//! absent. Because the daemon version is encoded in the filename
//! (`rift-daemon-<version>`), "missing or outdated" collapses to "the versioned
//! path is absent" — a stale version lives under a different name and is simply
//! never selected, so "no re-upload when present" collapses to "the path
//! exists".
//!
//! Deployment does **not** launch the daemon. A detached background spawn over
//! an exec channel is a no-op: the daemon would inherit that channel's
//! stdin/stdout, and as soon as the probe channel drains and `sshd` closes the
//! FDs the daemon hits EOF on stdin and exits (see the daemon's `serve`
//! contract). The real launch is opening the protocol channel on the returned
//! path via [`crate::SshConnection::open_daemon_channel`], done later when a
//! consumer actually wires the daemon protocol (the future `TmuxClient` swap).
//! Until then there is no persistent daemon to keep alive.

use tracing::{debug, info};

use crate::connection::exec::shell_single_quote;
use crate::error::SshError;
use crate::SshConnection;

/// Map the `uname -sm` output (kernel name + machine, e.g. `"Linux x86_64"`) to
/// the Rust target triple of the daemon binary built for that platform. Returns
/// `None` for platforms rift does not ship a daemon for.
///
/// Only `Linux x86_64` is mapped: the daemon target is
/// `x86_64-unknown-linux-musl` (statically linked, headless) per the spec, and a
/// single `RIFT_DAEMON_BINARY` is uploaded with no per-arch selection. Mapping
/// any other arch would silently upload the x86_64 binary to a host that cannot
/// run it, so everything else returns `None`.
pub fn target_triple_from_uname(uname_sm: &str) -> Option<&'static str> {
    match uname_sm.trim() {
        "Linux x86_64" => Some("x86_64-unknown-linux-musl"),
        _ => None,
    }
}

/// Versioned remote binary file name for a given daemon version. The version is
/// encoded in the name so a mismatched build resolves to a different path and is
/// re-uploaded rather than overwriting a still-running daemon.
pub fn remote_binary_name(version: &str) -> String {
    format!("rift-daemon-{version}")
}

/// Whether the daemon binary must be uploaded, given the result of probing the
/// versioned remote path for an executable file. `true` means absent (or not
/// executable) and therefore upload is required; `false` means the correct
/// version is already present and no re-upload is needed.
///
/// `probe_output` is the stdout of the presence probe (see
/// [`presence_probe_command`]) — the marker is emitted only when the path
/// exists and is executable.
pub fn needs_upload(probe_output: &str) -> bool {
    probe_output.trim() != PRESENT_MARKER
}

/// Marker echoed by the remote presence probe when the versioned binary is
/// already in place. Kept distinct from any path so a stray substring cannot be
/// mistaken for a positive result.
const PRESENT_MARKER: &str = "rift-present";

/// Build the remote presence-probe command for the already-resolved literal
/// `remote_path`: emit [`PRESENT_MARKER`] iff the path exists and is executable.
/// The path is single-quoted so it cannot be expanded or break out of quoting.
fn presence_probe_command(remote_path: &str) -> String {
    let quoted = shell_single_quote(remote_path);
    format!("test -x {quoted} && printf '%s' '{PRESENT_MARKER}'")
}

/// Build the remote `mkdir -p` command for the already-resolved literal
/// `remote_dir`, single-quoted.
fn mkdir_command(remote_dir: &str) -> String {
    let quoted = shell_single_quote(remote_dir);
    format!("mkdir -p {quoted}")
}

/// Resolve `remote_dir` to a literal absolute path. If it is `~`/`~/…` or
/// `$HOME`/`$HOME/…`, the home directory is fetched from the remote with a
/// fixed, data-free command (`printf '%s' "$HOME"`) and joined client-side —
/// so the raw configured value never enters a variable-expanding shell context.
/// Any other value is treated as an already-absolute literal path.
fn join_home(home: &str, remote_dir: &str) -> String {
    let home = home.trim_end_matches('/');
    let rest = remote_dir
        .strip_prefix("$HOME")
        .or_else(|| remote_dir.strip_prefix('~'))
        .map(|r| r.trim_start_matches('/'));
    match rest {
        Some("") => home.to_string(),
        Some(rest) => format!("{home}/{rest}"),
        None => remote_dir.to_string(),
    }
}

/// Detect the remote platform, resolve the versioned daemon path under
/// `remote_dir`, and upload `local_binary` only if that path is absent. Returns
/// the resolved remote path. Does **not** launch the daemon (see module docs).
///
/// `version` is the app's compiled-in daemon version; pass
/// `env!("CARGO_PKG_VERSION")` at the call site.
pub async fn ensure_daemon_deployed(
    conn: &mut SshConnection,
    local_binary: &[u8],
    remote_dir: &str,
    version: &str,
) -> Result<String, SshError> {
    let uname = conn.exec_capture("uname -sm").await?;
    let triple = target_triple_from_uname(&uname)
        .ok_or_else(|| SshError::UnsupportedPlatform(uname.trim().to_string()))?;
    debug!(platform = %uname.trim(), triple, "resolved remote daemon target");

    // Resolve `$HOME`/`~` with a fixed command that carries no user input, then
    // build the target directory client-side. The configured `remote_dir` value
    // is never placed in a variable-expanding shell context.
    let remote_dir = if remote_dir.starts_with("$HOME") || remote_dir.starts_with('~') {
        let home = conn.exec_capture("printf '%s' \"$HOME\"").await?;
        join_home(home.trim(), remote_dir)
    } else {
        remote_dir.to_string()
    };
    let remote_dir = remote_dir.trim_end_matches('/').to_string();
    let remote_path = format!("{remote_dir}/{}", remote_binary_name(version));

    let probe = conn
        .exec_capture(&presence_probe_command(&remote_path))
        .await?;

    if needs_upload(&probe) {
        info!(remote_path, "daemon binary absent, uploading");
        conn.exec_capture(&mkdir_command(&remote_dir)).await?;
        conn.upload_executable(local_binary, &remote_path).await?;
    } else {
        debug!(
            remote_path,
            "daemon binary already present, skipping upload"
        );
    }

    Ok(remote_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_triple_from_uname_linux_x86_64_maps_to_musl() {
        assert_eq!(
            target_triple_from_uname("Linux x86_64"),
            Some("x86_64-unknown-linux-musl")
        );
    }

    #[test]
    fn test_target_triple_from_uname_trims_trailing_newline() {
        assert_eq!(
            target_triple_from_uname("Linux x86_64\n"),
            Some("x86_64-unknown-linux-musl")
        );
    }

    #[test]
    fn test_target_triple_from_uname_aarch64_returns_none() {
        // Only x86_64-musl is built; aarch64 must not silently get the x86_64
        // binary.
        assert_eq!(target_triple_from_uname("Linux aarch64"), None);
    }

    #[test]
    fn test_target_triple_from_uname_unknown_returns_none() {
        assert_eq!(target_triple_from_uname("Darwin arm64"), None);
        assert_eq!(target_triple_from_uname("Linux riscv64"), None);
        assert_eq!(target_triple_from_uname(""), None);
    }

    #[test]
    fn test_remote_binary_name_encodes_version() {
        assert_eq!(remote_binary_name("0.1.0"), "rift-daemon-0.1.0");
    }

    #[test]
    fn test_needs_upload_true_when_probe_empty() {
        assert!(needs_upload(""));
    }

    #[test]
    fn test_needs_upload_false_when_present_marker() {
        assert!(!needs_upload(PRESENT_MARKER));
        assert!(!needs_upload("rift-present\n"));
    }

    #[test]
    fn test_needs_upload_true_when_unexpected_output() {
        assert!(needs_upload("no such file"));
    }

    #[test]
    fn test_presence_probe_command_single_quotes_path() {
        let cmd = presence_probe_command("/tmp/rift-daemon-0.1.0");
        assert_eq!(
            cmd,
            "test -x '/tmp/rift-daemon-0.1.0' && printf '%s' 'rift-present'"
        );
    }

    #[test]
    fn test_presence_probe_command_neutralizes_injection() {
        // A crafted path must be inert inside the probe command.
        let cmd = presence_probe_command("/tmp/$(touch pwned)");
        assert_eq!(
            cmd,
            "test -x '/tmp/$(touch pwned)' && printf '%s' 'rift-present'"
        );
    }

    #[test]
    fn test_mkdir_command_single_quotes_dir() {
        assert_eq!(
            mkdir_command("/home/u/.rift/bin"),
            "mkdir -p '/home/u/.rift/bin'"
        );
    }

    #[test]
    fn test_join_home_expands_dollar_home_prefix() {
        assert_eq!(join_home("/home/u", "$HOME/.rift/bin"), "/home/u/.rift/bin");
    }

    #[test]
    fn test_join_home_expands_tilde_prefix() {
        assert_eq!(join_home("/home/u", "~/.rift/bin"), "/home/u/.rift/bin");
    }

    #[test]
    fn test_join_home_bare_home_yields_home() {
        assert_eq!(join_home("/home/u/", "$HOME"), "/home/u");
        assert_eq!(join_home("/home/u", "~"), "/home/u");
    }

    #[test]
    fn test_join_home_absolute_path_is_left_literal() {
        assert_eq!(join_home("/home/u", "/opt/rift/bin"), "/opt/rift/bin");
    }
}
