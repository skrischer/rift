//! Auto-deploy of the remote `rift-daemon` binary.
//!
//! Detects the remote platform via `uname -sm`, resolves the expected versioned
//! binary path, and uploads the locally-built musl binary when the deployed copy
//! is absent or its contents differ. The version is encoded in the filename
//! (`rift-daemon-<version>`), so a released version bump resolves to a fresh name
//! and always deploys. Within one version the filename is stable, so deployment
//! also compares a content fingerprint (a dependency-free FNV-1a of the binary,
//! stored beside it in a `.fnv` marker on upload): a rebuilt same-version binary
//! re-deploys instead of the launch silently keeping the stale copy (issue
//! #268, reinstated atop the atomic rename-into-place upload from #280 so the
//! re-upload no longer fails with `ETXTBSY` against a running daemon).
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

/// Whether the daemon binary must be (re-)uploaded: `true` unless the marker the
/// remote probe returned exactly equals the local binary's `fingerprint`. An
/// absent binary, an absent/stale marker, or any content change all read as
/// "upload"; only a byte-identical, already-fingerprinted deploy is skipped.
///
/// `probe_output` is the stdout of the presence probe (see
/// [`presence_probe_command`]) — the stored fingerprint, or empty.
pub fn needs_upload(probe_output: &str, fingerprint: &str) -> bool {
    probe_output.trim() != fingerprint
}

/// 64-bit FNV-1a fingerprint of the binary, hex-encoded. Hand-rolled rather than
/// pulling a hashing crate (deps rule: a simple custom implementation over a
/// dependency for one function) and deterministic across toolchains — unlike
/// `std`'s `DefaultHasher` — so the value a prior deploy stored on the remote can
/// be compared against a freshly built binary to detect a same-version content
/// change. Not cryptographic: it only has to notice a recompile, where the worst
/// case of an unrecognised change is a harmless extra upload.
pub fn binary_fingerprint(bytes: &[u8]) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// Sibling marker path holding the deployed binary's fingerprint.
fn fingerprint_path(remote_path: &str) -> String {
    format!("{remote_path}.fnv")
}

/// Build the remote presence-probe command for the already-resolved literal
/// `remote_path`: emit the stored fingerprint iff the binary exists and is
/// executable, and empty otherwise (absent binary, or a binary deployed before
/// fingerprinting existed — `cat` of the missing marker is silenced). Both paths
/// are single-quoted so they cannot be expanded or break out of quoting.
///
/// The whole command must exit zero regardless of presence: the outer
/// `if`/`then`/`fi` covers an absent *binary* (the `then` body never runs), but
/// `cat`'s own exit status still surfaces when the binary exists and only the
/// `.fnv` marker is missing (the pre-fingerprint-upgrade case) — redirecting its
/// stderr does not change that. `|| true` absorbs that failure so a missing
/// marker reads as empty output, not a failed remote command that would abort
/// `exec_capture`.
fn presence_probe_command(remote_path: &str) -> String {
    let bin = shell_single_quote(remote_path);
    let marker = shell_single_quote(&fingerprint_path(remote_path));
    format!("if test -x {bin}; then cat {marker} 2>/dev/null || true; fi")
}

/// Build the command that writes the fingerprint marker beside the uploaded
/// binary. Both the value and the path are single-quoted.
fn write_fingerprint_command(remote_path: &str, fingerprint: &str) -> String {
    let marker = shell_single_quote(&fingerprint_path(remote_path));
    let fp = shell_single_quote(fingerprint);
    format!("printf '%s' {fp} > {marker}")
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

/// Outcome of [`ensure_daemon_deployed`]: the resolved remote binary path, plus
/// whether this call (re)uploaded it. A caller that needs to react to a binary
/// change — e.g. restarting an already-running daemon so it picks up the new
/// code (#283) — checks `uploaded` instead of re-deriving it from the deploy
/// decision.
pub struct DeployOutcome {
    pub remote_path: String,
    pub uploaded: bool,
}

/// Detect the remote platform, resolve the versioned daemon path under
/// `remote_dir`, and upload `local_binary` when that path is absent or its
/// content fingerprint has changed. Does **not** launch the daemon (see module
/// docs).
///
/// `version` is the app's compiled-in daemon version; pass
/// `env!("CARGO_PKG_VERSION")` at the call site.
pub async fn ensure_daemon_deployed(
    conn: &mut SshConnection,
    local_binary: &[u8],
    remote_dir: &str,
    version: &str,
) -> Result<DeployOutcome, SshError> {
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

    let fingerprint = binary_fingerprint(local_binary);
    let probe = conn
        .exec_capture(&presence_probe_command(&remote_path))
        .await?;

    let uploaded = needs_upload(&probe, &fingerprint);
    if uploaded {
        info!(remote_path, "daemon binary absent or changed, uploading");
        conn.exec_capture(&mkdir_command(&remote_dir)).await?;
        conn.upload_executable(local_binary, &remote_path).await?;
        conn.exec_capture(&write_fingerprint_command(&remote_path, &fingerprint))
            .await?;
    } else {
        debug!(remote_path, "daemon binary up to date, skipping upload");
    }

    Ok(DeployOutcome {
        remote_path,
        uploaded,
    })
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
        // Absent binary or absent marker -> empty probe -> must upload.
        assert!(needs_upload("", "deadbeefdeadbeef"));
    }

    #[test]
    fn test_needs_upload_false_when_fingerprint_matches() {
        assert!(!needs_upload("deadbeefdeadbeef", "deadbeefdeadbeef"));
        assert!(!needs_upload("deadbeefdeadbeef\n", "deadbeefdeadbeef"));
    }

    #[test]
    fn test_needs_upload_true_when_fingerprint_differs() {
        // A same-version content change: stored marker no longer matches.
        assert!(needs_upload("0000000000000000", "deadbeefdeadbeef"));
    }

    #[test]
    fn test_binary_fingerprint_is_deterministic() {
        assert_eq!(
            binary_fingerprint(b"rift-daemon"),
            binary_fingerprint(b"rift-daemon")
        );
    }

    #[test]
    fn test_binary_fingerprint_differs_on_content_change() {
        assert_ne!(
            binary_fingerprint(b"rift-daemon-v1"),
            binary_fingerprint(b"rift-daemon-v2")
        );
    }

    #[test]
    fn test_binary_fingerprint_matches_known_fnv1a_vectors() {
        // Canonical 64-bit FNV-1a vectors: empty input is the offset basis.
        assert_eq!(binary_fingerprint(b""), "cbf29ce484222325");
        assert_eq!(binary_fingerprint(b"a"), "af63dc4c8601ec8c");
    }

    #[test]
    fn test_presence_probe_command_single_quotes_paths() {
        let cmd = presence_probe_command("/tmp/rift-daemon-0.1.0");
        assert_eq!(
            cmd,
            "if test -x '/tmp/rift-daemon-0.1.0'; then cat '/tmp/rift-daemon-0.1.0.fnv' 2>/dev/null || true; fi"
        );
    }

    #[test]
    fn test_presence_probe_command_neutralizes_injection() {
        // A crafted path must be inert in both the test and the cat occurrence.
        let cmd = presence_probe_command("/tmp/$(touch pwned)");
        assert_eq!(
            cmd,
            "if test -x '/tmp/$(touch pwned)'; then cat '/tmp/$(touch pwned).fnv' 2>/dev/null || true; fi"
        );
    }

    /// Runs the generated probe through a real `sh` against a temp directory,
    /// covering the case a string-only assertion cannot: a binary deployed
    /// before fingerprinting existed has no `.fnv` marker, so `cat` fails, and
    /// only executing the command reveals whether that failure leaks through
    /// the `if`/`fi` wrapper as a non-zero exit (which `exec_capture` — see
    /// `crate::connection::exec::drain_channel` — turns into `SshError::Exec`,
    /// aborting the whole deploy instead of correctly deciding "needs upload").
    #[test]
    fn test_presence_probe_command_exits_zero_when_binary_present_but_marker_absent() {
        let dir = std::env::temp_dir().join(format!(
            "rift-ssh-probe-test-{:?}",
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let bin = dir.join("rift-daemon-0.1.0");
        std::fs::write(&bin, b"stub").expect("write stub binary");
        let mut perms = std::fs::metadata(&bin)
            .expect("stat stub binary")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&bin, perms).expect("chmod stub binary");

        let cmd = presence_probe_command(bin.to_str().expect("utf8 temp path"));
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .output()
            .expect("run probe command");

        std::fs::remove_dir_all(&dir).expect("clean up temp dir");

        assert!(
            output.status.success(),
            "probe must exit zero even when the marker is missing, got {:?}",
            output.status
        );
        assert!(output.stdout.is_empty());
    }

    #[test]
    fn test_write_fingerprint_command_single_quotes_value_and_path() {
        assert_eq!(
            write_fingerprint_command("/tmp/rift-daemon-0.1.0", "af63dc4c8601ec8c"),
            "printf '%s' 'af63dc4c8601ec8c' > '/tmp/rift-daemon-0.1.0.fnv'"
        );
    }

    #[test]
    fn test_write_fingerprint_command_neutralizes_injection() {
        let cmd = write_fingerprint_command("/tmp/$(touch pwned)", "deadbeef");
        assert_eq!(cmd, "printf '%s' 'deadbeef' > '/tmp/$(touch pwned).fnv'");
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
