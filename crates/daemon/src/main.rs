use std::path::{Path, PathBuf};

use anyhow::Context;
use rift_daemon::{connect_relay, ping, serve, serve_uds};
use rift_logging::{
    build_filter, install_panic_hook, log_target, LogTarget, RotatingMakeWriter, SizedWriter,
    DEFAULT_MAX_BYTES,
};

/// Env var overriding the rotated file sink's path, checked when `--serve-uds`'s
/// `--log-file` flag is absent. Falls back further to a socket-path-keyed
/// default (see [`default_log_path`]).
const LOG_FILE_ENV: &str = "RIFT_DAEMON_LOG_FILE";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Panics must land in whatever sink ends up active, in every mode, so this
    // is installed before mode dispatch decides what that sink is. In `--ping`
    // and `--connect`, where no subscriber is ever installed, the hook's
    // `tracing::error!` is a harmless no-op and the default hook's backtrace
    // still runs.
    install_panic_hook();

    // Stdout is reserved for protocol frames in stdio mode; nothing else may
    // write to it, or a stray banner would be decoded as a frame length prefix
    // and stall the client decoder. Logging goes to stderr (interactive) or the
    // rotated file sink (redirected) — never stdout.
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        // Long-lived reattachable mode: bind a Unix socket and serve connections
        // until the process is signalled. The SSH host launches this detached so
        // it survives connection drops (issue #62).
        Some("--serve-uds") => {
            let (path, root_flag, log_file_flag) = parse_serve_uds_args(args)?;
            let root = watched_root(root_flag)?;
            init_logging(default_log_path(&path), log_file_flag)?;
            tracing::info!(socket = %path, worktree = %root.display(), "rift-daemon listening");
            serve_uds(Path::new(&path), Some(root)).await
        }
        // Relay mode: connect the process's stdio to a running daemon's socket.
        // The SSH host wires its channel to this so the channel reaches the
        // persistent daemon without interpreting the protocol. No logging setup
        // here, matching `--ping`: `connect_relay`/`relay` have no call sites
        // worth logging, and every real launch (`crates/ssh/src/launch.rs`)
        // drives `--connect` against the *same* socket path as its `--serve-uds`
        // daemon — a default log path keyed on that socket would collide with
        // the daemon's own rotated file, and `SizedWriter`'s rotation is not
        // safe across independent OS processes sharing one file.
        Some("--connect") => {
            let path = args.next().context("--connect requires a socket path")?;
            connect_relay(Path::new(&path)).await
        }
        // Probe mode: exit 0 if a daemon is listening on the socket, 1 otherwise.
        // The SSH host keys its reattach-vs-spawn decision on this status; no
        // output is emitted either way. No logging setup here: a probe runs on
        // every reconnect attempt and has no call sites worth logging.
        Some("--ping") => {
            let path = args.next().context("--ping requires a socket path")?;
            std::process::exit(if ping(Path::new(&path)).await { 0 } else { 1 });
        }
        Some(other) => anyhow::bail!("unknown argument: {other}"),
        // Default stdio mode: speak the protocol over stdin/stdout for a single
        // session. `serve` returns when stdin reaches EOF.
        None => {
            let root = watched_root(None)?;
            init_logging(PathBuf::from("rift-daemon.log"), None)?;
            tracing::info!(worktree = %root.display(), "rift-daemon starting");
            serve(tokio::io::stdin(), tokio::io::stdout(), Some(root)).await
        }
    }
}

/// Install the shared filter chain over the TTY-ruled sink: the stderr fmt
/// layer when stderr is a terminal (or `RIFT_LOG_CONSOLE` forces it), otherwise
/// the daemon-managed rotated file sink — never stdout, which stays reserved for
/// protocol frames.
///
/// The file path is `log_file_flag` (the `--serve-uds` `--log-file` flag) when
/// set, else [`LOG_FILE_ENV`], else `default_path` — a socket-path-keyed default
/// distinct from the launch line's `<binary>.log` redirect target, so no line is
/// duplicated into that unrotatable file (`crates/ssh/src/launch.rs`).
fn init_logging(default_path: PathBuf, log_file_flag: Option<PathBuf>) -> anyhow::Result<()> {
    let filter = build_filter();
    match log_target() {
        LogTarget::Console => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init()
            .map_err(|err| anyhow::anyhow!("failed to install stderr tracing subscriber: {err}")),
        LogTarget::File => {
            let path = log_file_flag
                .or_else(|| std::env::var_os(LOG_FILE_ENV).map(PathBuf::from))
                .unwrap_or(default_path);
            let writer = SizedWriter::new(&path, DEFAULT_MAX_BYTES)
                .with_context(|| format!("failed to open daemon log file {}", path.display()))?;
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(RotatingMakeWriter::new(writer))
                .try_init()
                .map_err(|err| anyhow::anyhow!("failed to install file tracing subscriber: {err}"))
        }
    }
}

/// Default rotated-log path for the `--serve-uds` daemon: the socket path with
/// a `.log` suffix appended — the `pidfile_path` naming precedent
/// (`crates/daemon/src/lib.rs`), so the daemon's own log, its pidfile, and its
/// socket all key off one path without colliding. Not reused by `--connect`
/// (see its match arm): every real launch drives both modes against the same
/// socket path, so a socket-keyed default there would collide with this one.
fn default_log_path(socket_path: &str) -> PathBuf {
    let mut raw = std::ffi::OsString::from(socket_path);
    raw.push(".log");
    PathBuf::from(raw)
}

/// Parse the arguments following `--serve-uds`: a required socket path and two
/// optional trailing flags — `--root <path>` naming the directory to watch, and
/// `--log-file <path>` naming the rotated log sink's path (overriding
/// [`default_log_path`]). Split out of `main` so the flag handling is
/// unit-testable.
fn parse_serve_uds_args(
    mut args: impl Iterator<Item = String>,
) -> anyhow::Result<(String, Option<PathBuf>, Option<PathBuf>)> {
    let path = args.next().context("--serve-uds requires a socket path")?;
    let mut root = None;
    let mut log_file = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                root = Some(PathBuf::from(
                    args.next().context("--root requires a path")?,
                ));
            }
            "--log-file" => {
                log_file = Some(PathBuf::from(
                    args.next().context("--log-file requires a path")?,
                ));
            }
            other => anyhow::bail!("unknown argument after --serve-uds: {other}"),
        }
    }
    Ok((path, root, log_file))
}

/// The directory the daemon watches: the `--root` flag, required. There is no
/// launch-directory fallback (issue #502): over SSH the launch directory is
/// `$HOME`, so falling back to it silently pointed the file watcher and git
/// status at the whole home directory instead of the intended project. Every
/// sanctioned launch path (`crates/ssh/src/launch.rs`, `justfile`) already
/// resolves and passes an explicit root; a missing one is refused loudly
/// instead of scanning the wrong tree quietly.
fn watched_root(flag: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    flag.context(
        "no watch root given: pass --root <path> (the daemon refuses to fall back to its \
         launch directory, which is $HOME over SSH)",
    )
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
        let (path, root, log_file) =
            parse_serve_uds_args(args(&["rift.sock", "--root", "/srv/project"])).expect("parse");
        assert_eq!(path, "rift.sock");
        assert_eq!(root, Some(PathBuf::from("/srv/project")));
        assert_eq!(log_file, None);
    }

    #[test]
    fn test_parse_serve_uds_args_without_root_yields_none() {
        let (path, root, log_file) = parse_serve_uds_args(args(&["rift.sock"])).expect("parse");
        assert_eq!(path, "rift.sock");
        assert_eq!(root, None);
        assert_eq!(log_file, None);
    }

    #[test]
    fn test_parse_serve_uds_args_root_without_value_errors() {
        assert!(parse_serve_uds_args(args(&["rift.sock", "--root"])).is_err());
    }

    #[test]
    fn test_parse_serve_uds_args_with_log_file_sources_the_flag() {
        let (path, root, log_file) =
            parse_serve_uds_args(args(&["rift.sock", "--log-file", "/srv/rift-daemon.log"]))
                .expect("parse");
        assert_eq!(path, "rift.sock");
        assert_eq!(root, None);
        assert_eq!(log_file, Some(PathBuf::from("/srv/rift-daemon.log")));
    }

    #[test]
    fn test_parse_serve_uds_args_log_file_without_value_errors() {
        assert!(parse_serve_uds_args(args(&["rift.sock", "--log-file"])).is_err());
    }

    #[test]
    fn test_parse_serve_uds_args_both_flags_in_either_order() {
        let (_, root, log_file) = parse_serve_uds_args(args(&[
            "rift.sock",
            "--log-file",
            "/srv/d.log",
            "--root",
            "/srv/project",
        ]))
        .expect("parse");
        assert_eq!(root, Some(PathBuf::from("/srv/project")));
        assert_eq!(log_file, Some(PathBuf::from("/srv/d.log")));
    }

    #[test]
    fn test_default_log_path_appends_log_suffix_distinct_from_launch_redirect() {
        // The launch line's redirect target is `<binary>.log`
        // (`crates/ssh/src/launch.rs`); the socket path is `<binary>.sock`, so
        // this default (`<socket>.log`) never collides with it.
        assert_eq!(
            default_log_path("/h/rift-daemon-0.1.0.sock"),
            PathBuf::from("/h/rift-daemon-0.1.0.sock.log")
        );
    }

    #[test]
    fn test_watched_root_uses_flag_when_present() {
        let resolved = watched_root(Some(PathBuf::from("/srv/project"))).expect("resolve");
        assert_eq!(resolved, PathBuf::from("/srv/project"));
    }

    #[test]
    fn test_watched_root_errors_when_absent() {
        let err = watched_root(None).expect_err("missing root must be refused");
        assert!(err.to_string().contains("--root"));
    }
}
