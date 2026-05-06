use std::path::{Path, PathBuf};

use russh_keys::key::PublicKey;
use tracing::{debug, info, warn};

use crate::error::SshError;

/// Result of a successful server key verification.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HostKeyVerification {
    /// The key matches a known entry.
    Matched,
    /// The host was not found — key was trusted on first use and appended.
    TrustedOnFirstUse,
}

/// Path to the user's known_hosts file.
fn default_known_hosts_path() -> Result<PathBuf, SshError> {
    let home = home::home_dir()
        .ok_or_else(|| SshError::KnownHosts("could not determine home directory".into()))?;
    Ok(home.join(".ssh").join("known_hosts"))
}

/// Verify the server key for `host:port` against the known_hosts file at `path`.
///
/// Returns `Ok(Matched)` or `Ok(TrustedOnFirstUse)` on success.
/// Returns `Err(HostKeyMismatch)` if the host is known but the key differs.
pub(crate) fn verify_host_key_at_path(
    host: &str,
    port: u16,
    key: &PublicKey,
    path: &Path,
) -> Result<HostKeyVerification, SshError> {
    match russh_keys::check_known_hosts_path(host, port, key, path) {
        Ok(true) => {
            debug!(%host, port, "server key matches known_hosts");
            Ok(HostKeyVerification::Matched)
        }
        // No entry found for this host+key-algorithm combination — trust on first use.
        Ok(false) => {
            info!(%host, port, "unknown host, trusting key on first use");
            russh_keys::known_hosts::learn_known_hosts_path(host, port, key, path)
                .map_err(|e| SshError::KnownHosts(e.to_string()))?;
            Ok(HostKeyVerification::TrustedOnFirstUse)
        }
        Err(russh_keys::Error::KeyChanged { line }) => {
            warn!(%host, port, line, "server key mismatch detected");
            Err(SshError::HostKeyMismatch {
                host: host.to_owned(),
                line,
            })
        }
        Err(e) => Err(SshError::KnownHosts(e.to_string())),
    }
}

/// Verify the server key using the default known_hosts file (~/.ssh/known_hosts).
pub(crate) fn verify_host_key(
    host: &str,
    port: u16,
    key: &PublicKey,
) -> Result<HostKeyVerification, SshError> {
    let path = default_known_hosts_path()?;
    verify_host_key_at_path(host, port, key, &path)
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::Write;

    use russh_keys::PublicKeyBase64;

    use super::*;

    fn generate_ed25519_key() -> PublicKey {
        let kp = russh_keys::key::KeyPair::generate_ed25519();
        kp.clone_public_key()
            .expect("clone_public_key should not fail for ed25519")
    }

    fn write_known_hosts(dir: &Path, content: &[u8]) -> PathBuf {
        let path = dir.join("known_hosts");
        let mut f = File::create(&path).expect("create known_hosts");
        f.write_all(content).expect("write known_hosts");
        path
    }

    #[test]
    fn test_verify_host_key_matching_key_returns_matched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = generate_ed25519_key();
        let base64 = key.public_key_base64();

        let content = format!("[localhost]:2222 {} {}\n", key.name(), base64);
        let path = write_known_hosts(dir.path(), content.as_bytes());

        let result = verify_host_key_at_path("localhost", 2222, &key, &path);
        assert_eq!(
            result.expect("should succeed"),
            HostKeyVerification::Matched
        );
    }

    #[test]
    fn test_verify_host_key_standard_port_returns_matched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = generate_ed25519_key();
        let base64 = key.public_key_base64();

        let content = format!("example.com {} {}\n", key.name(), base64);
        let path = write_known_hosts(dir.path(), content.as_bytes());

        let result = verify_host_key_at_path("example.com", 22, &key, &path);
        assert_eq!(
            result.expect("should succeed"),
            HostKeyVerification::Matched
        );
    }

    #[test]
    fn test_verify_host_key_unknown_host_trusts_on_first_use() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = generate_ed25519_key();

        // Empty known_hosts
        let path = write_known_hosts(dir.path(), b"");

        let result = verify_host_key_at_path("newhost.example.com", 22, &key, &path);
        assert_eq!(
            result.expect("should succeed"),
            HostKeyVerification::TrustedOnFirstUse
        );

        // Verify the key was written
        let contents = fs::read_to_string(&path).expect("read known_hosts");
        assert!(contents.contains("newhost.example.com"));
        assert!(contents.contains(&key.public_key_base64()));
    }

    #[test]
    fn test_verify_host_key_unknown_host_no_file_trusts_on_first_use() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = generate_ed25519_key();

        // File does not exist yet
        let path = dir.path().join("known_hosts");

        let result = verify_host_key_at_path("brand-new.example.com", 22, &key, &path);
        assert_eq!(
            result.expect("should succeed"),
            HostKeyVerification::TrustedOnFirstUse
        );

        // Verify the file was created with the key
        let contents = fs::read_to_string(&path).expect("read known_hosts");
        assert!(contents.contains("brand-new.example.com"));
    }

    #[test]
    fn test_verify_host_key_mismatch_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stored_key = generate_ed25519_key();
        let different_key = generate_ed25519_key();

        let base64 = stored_key.public_key_base64();
        let content = format!("mismatch.example.com {} {}\n", stored_key.name(), base64);
        let path = write_known_hosts(dir.path(), content.as_bytes());

        let result = verify_host_key_at_path("mismatch.example.com", 22, &different_key, &path);
        assert!(
            matches!(
                result,
                Err(SshError::HostKeyMismatch { ref host, line: 1 }) if host == "mismatch.example.com"
            ),
            "expected HostKeyMismatch error at line 1, got: {result:?}"
        );
    }

    #[test]
    fn test_verify_host_key_mismatch_with_comment_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stored_key = generate_ed25519_key();
        let different_key = generate_ed25519_key();

        let base64 = stored_key.public_key_base64();
        let content = format!(
            "# this is a comment\n\
             other.host {} {}\n\
             target.host {} {}\n",
            stored_key.name(),
            base64,
            stored_key.name(),
            base64
        );
        let path = write_known_hosts(dir.path(), content.as_bytes());

        let result = verify_host_key_at_path("target.host", 22, &different_key, &path);
        assert!(
            matches!(
                result,
                Err(SshError::HostKeyMismatch { ref host, line: 2 }) if host == "target.host"
            ),
            "expected HostKeyMismatch error at line 2, got: {result:?}"
        );
    }

    #[test]
    fn test_verify_host_key_malformed_lines_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = generate_ed25519_key();
        let base64 = key.public_key_base64();

        // Mix of malformed lines with a valid entry
        let content = format!(
            "this-is-not-valid\n\
             also invalid line with no key type\n\
             [localhost]:9999 {} {}\n",
            key.name(),
            base64
        );
        let path = write_known_hosts(dir.path(), content.as_bytes());

        let result = verify_host_key_at_path("localhost", 9999, &key, &path);
        assert_eq!(
            result.expect("should succeed"),
            HostKeyVerification::Matched
        );
    }

    #[test]
    fn test_verify_host_key_multiple_hosts_per_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = generate_ed25519_key();
        let base64 = key.public_key_base64();

        let content = format!(
            "host1.com,host2.com,192.168.1.1 {} {}\n",
            key.name(),
            base64
        );
        let path = write_known_hosts(dir.path(), content.as_bytes());

        // Should match any of the comma-separated hosts
        let result = verify_host_key_at_path("host2.com", 22, &key, &path);
        assert_eq!(
            result.expect("should succeed"),
            HostKeyVerification::Matched
        );
    }

    #[test]
    fn test_verify_host_key_nonstandard_port_format() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = generate_ed25519_key();
        let base64 = key.public_key_base64();

        let content = format!("[myhost]:8022 {} {}\n", key.name(), base64);
        let path = write_known_hosts(dir.path(), content.as_bytes());

        // Correct port should match
        let result = verify_host_key_at_path("myhost", 8022, &key, &path);
        assert_eq!(
            result.expect("should succeed"),
            HostKeyVerification::Matched
        );

        // Wrong port should not match (unknown host → TOFU)
        let result = verify_host_key_at_path("myhost", 9999, &key, &path);
        assert_eq!(
            result.expect("should succeed"),
            HostKeyVerification::TrustedOnFirstUse
        );
    }
}
