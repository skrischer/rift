use std::path::{Path, PathBuf};

use anyhow::Context;
use rift_daemon::{connect_relay, ping, serve, serve_uds};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Stdout is reserved for protocol frames in stdio mode; nothing else may
    // write to it, or a stray banner would be decoded as a frame length prefix
    // and stall the client decoder. All logging goes to stderr.
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        // Long-lived reattachable mode: bind a Unix socket and serve connections
        // until the process is signalled. The SSH host launches this detached so
        // it survives connection drops (issue #62).
        Some("--serve-uds") => {
            let (path, root_flag) = parse_serve_uds_args(args)?;
            let root = watched_root(root_flag)?;
            eprintln!(
                "rift-daemon listening on {path}, worktree {}",
                root.display()
            );
            serve_uds(Path::new(&path), Some(root)).await
        }
        // Relay mode: connect the process's stdio to a running daemon's socket.
        // The SSH host wires its channel to this so the channel reaches the
        // persistent daemon without interpreting the protocol.
        Some("--connect") => {
            let path = args.next().context("--connect requires a socket path")?;
            connect_relay(Path::new(&path)).await
        }
        // Probe mode: exit 0 if a daemon is listening on the socket, 1 otherwise.
        // The SSH host keys its reattach-vs-spawn decision on this status; no
        // output is emitted either way.
        Some("--ping") => {
            let path = args.next().context("--ping requires a socket path")?;
            std::process::exit(if ping(Path::new(&path)).await { 0 } else { 1 });
        }
        Some(other) => anyhow::bail!("unknown argument: {other}"),
        // Default stdio mode: speak the protocol over stdin/stdout for a single
        // session. `serve` returns when stdin reaches EOF.
        None => {
            let root = watched_root(None)?;
            eprintln!("rift-daemon starting, worktree {}", root.display());
            serve(tokio::io::stdin(), tokio::io::stdout(), Some(root)).await
        }
    }
}

/// Parse the arguments following `--serve-uds`: a required socket path and an
/// optional trailing `--root <path>` naming the directory to watch. Split out
/// of `main` so the flag handling is unit-testable.
fn parse_serve_uds_args(
    mut args: impl Iterator<Item = String>,
) -> anyhow::Result<(String, Option<PathBuf>)> {
    let path = args.next().context("--serve-uds requires a socket path")?;
    let mut root = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                root = Some(PathBuf::from(
                    args.next().context("--root requires a path")?,
                ));
            }
            other => anyhow::bail!("unknown argument after --serve-uds: {other}"),
        }
    }
    Ok((path, root))
}

/// The directory the daemon watches: the `--root` flag when present, otherwise
/// the daemon's launch directory (current behavior). Both the socket and stdio
/// modes share this fallback.
fn watched_root(flag: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match flag {
        Some(root) => Ok(root),
        None => std::env::current_dir().context("cannot resolve the daemon launch directory"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> impl Iterator<Item = String> {
        parts
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn test_parse_serve_uds_args_with_root_sources_the_flag() {
        let (path, root) =
            parse_serve_uds_args(args(&["rift.sock", "--root", "/srv/project"])).expect("parse");
        assert_eq!(path, "rift.sock");
        assert_eq!(root, Some(PathBuf::from("/srv/project")));
    }

    #[test]
    fn test_parse_serve_uds_args_without_root_yields_none() {
        let (path, root) = parse_serve_uds_args(args(&["rift.sock"])).expect("parse");
        assert_eq!(path, "rift.sock");
        assert_eq!(root, None);
    }

    #[test]
    fn test_parse_serve_uds_args_root_without_value_errors() {
        assert!(parse_serve_uds_args(args(&["rift.sock", "--root"])).is_err());
    }

    #[test]
    fn test_watched_root_uses_flag_when_present() {
        let resolved = watched_root(Some(PathBuf::from("/srv/project"))).expect("resolve");
        assert_eq!(resolved, PathBuf::from("/srv/project"));
    }

    #[test]
    fn test_watched_root_falls_back_to_current_dir_when_absent() {
        let resolved = watched_root(None).expect("resolve");
        assert_eq!(resolved, std::env::current_dir().expect("cwd"));
    }
}
