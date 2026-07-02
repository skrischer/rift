//! Integration tests for the daemon binary's tracing sink selection (#231).
//!
//! Drives the real `rift-daemon` binary (not the library functions directly) so
//! the TTY-ruled sink choice in `main.rs` is exercised end to end: a detached
//! `--serve-uds` launch (non-TTY stdio, as in the real `setsid ... >> log 2>&1`
//! launch line, `crates/ssh/src/launch.rs`) must write only to its own rotated
//! file, leaving the launch redirect empty; `RIFT_LOG_CONSOLE` (the runtime
//! override for "no real TTY available", needed since a test harness has none)
//! forces the stderr sink instead; `--connect` relay mode's stdout must carry
//! only protocol frames even while logging is demonstrably active elsewhere in
//! the system (the #60 framing invariant), and `--connect` must never install
//! its own file sink (it would collide with its daemon's, since every real
//! launch drives both against the same socket path); and `--log-file`/
//! `RIFT_DAEMON_LOG_FILE` resolve with the right precedence.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rift_protocol::{encode_frame, ClientMessage, DaemonMessage, FrameDecoder, PROTOCOL_VERSION};

fn daemon_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rift-daemon")
}

/// Unique scratch directory under the OS temp dir for one test's sockets/logs —
/// also handed to `--root` so the daemon watches a tiny directory instead of
/// scanning the whole repo.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "rift-daemon-logging-{}-{tag}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Kills and reaps the wrapped child on drop, so a failing assertion never
/// leaks a daemon process past the test.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn read_to_string(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Poll until `path` exists (the daemon's socket bind), panicking after ~4s.
fn wait_for_path(path: &Path) {
    let start = Instant::now();
    while !path.exists() {
        if start.elapsed() > Duration::from_secs(4) {
            panic!("{} never appeared", path.display());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Poll until `path`'s content contains `needle`, panicking after ~4s. Used
/// instead of a fixed sleep so the happy path is fast and the failure path is
/// still deterministic.
fn wait_for_content(path: &Path, needle: &str) -> String {
    let start = Instant::now();
    loop {
        let content = read_to_string(path);
        if content.contains(needle) {
            return content;
        }
        if start.elapsed() > Duration::from_secs(4) {
            panic!(
                "{} never contained {needle:?}; got: {content:?}",
                path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn test_serve_uds_detached_writes_only_rotated_file_leaves_redirect_empty() {
    let scratch = Scratch::new("detached");
    let sock = scratch.path("rift.sock");
    let log_file = scratch.path("rift-daemon.log");
    let redirect_out = scratch.path("redirect.out");
    let redirect_err = scratch.path("redirect.err");

    let child = Command::new(daemon_bin())
        .arg("--serve-uds")
        .arg(&sock)
        .arg("--root")
        .arg(&scratch.dir)
        .arg("--log-file")
        .arg(&log_file)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            File::create(&redirect_out).expect("create redirect stdout"),
        ))
        .stderr(Stdio::from(
            File::create(&redirect_err).expect("create redirect stderr"),
        ))
        .spawn()
        .expect("spawn daemon --serve-uds");
    let _child = ChildGuard(child);

    wait_for_path(&sock);
    let log = wait_for_content(&log_file, "rift-daemon listening");
    assert!(
        log.contains(&sock.display().to_string()),
        "expected the startup line to carry the socket path, got: {log:?}"
    );

    // Nothing must ever have reached the launch line's redirect: no subscriber
    // targets it in detached mode, so it stays the tiny pre-init/panic backstop.
    assert!(
        read_to_string(&redirect_out).is_empty(),
        "launch redirect stdout must stay empty in detached mode"
    );
    assert!(
        read_to_string(&redirect_err).is_empty(),
        "launch redirect stderr must stay empty in detached mode"
    );
}

#[test]
fn test_serve_uds_console_override_logs_to_stderr_not_file() {
    let scratch = Scratch::new("console");
    let sock = scratch.path("rift.sock");
    let stderr_file = scratch.path("stderr.log");

    let child = Command::new(daemon_bin())
        .arg("--serve-uds")
        .arg(&sock)
        .arg("--root")
        .arg(&scratch.dir)
        .env("RIFT_LOG_CONSOLE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            File::create(&stderr_file).expect("create stderr file"),
        ))
        .spawn()
        .expect("spawn daemon --serve-uds");
    let _child = ChildGuard(child);

    wait_for_path(&sock);
    wait_for_content(&stderr_file, "rift-daemon listening");

    // The console override must skip the file sink entirely, not merely defer
    // to it: the default rotated-file path is never created.
    let default_log = PathBuf::from(format!("{}.log", sock.display()));
    assert!(
        !default_log.exists(),
        "RIFT_LOG_CONSOLE=1 must not create the rotated file sink"
    );
}

#[test]
fn test_connect_relay_stdout_is_frame_only_with_logging_active() {
    let scratch = Scratch::new("connect");
    let sock = scratch.path("rift.sock");
    let daemon_log = scratch.path("daemon.log");

    // `--root` points at a path that is never created: the worktree scan then
    // fails immediately and permanently (logged, not fatal — see
    // `worktree_worker`), so no `WorktreeSnapshot` is ever queued behind the
    // handshake. Without this, whether the scan of a real (if empty) root wins
    // the race against the `Welcome` flush is timing-dependent, which would
    // make the exact-frame-count assertion below flaky.
    let daemon = Command::new(daemon_bin())
        .arg("--serve-uds")
        .arg(&sock)
        .arg("--root")
        .arg(scratch.path("missing-root"))
        .arg("--log-file")
        .arg(&daemon_log)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon --serve-uds");
    let _daemon = ChildGuard(daemon);
    wait_for_path(&sock);

    // Proof that logging is genuinely active in the system under test before
    // exercising the `--connect` framing invariant against it.
    wait_for_content(&daemon_log, "rift-daemon listening");

    let mut connect = Command::new(daemon_bin())
        .arg("--connect")
        .arg(&sock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon --connect");
    let mut stdin = connect.stdin.take().expect("connect stdin");
    let mut stdout = connect.stdout.take().expect("connect stdout");

    let hello = encode_frame(&ClientMessage::Hello {
        version: PROTOCOL_VERSION,
    })
    .expect("encode Hello");
    stdin.write_all(&hello).expect("send Hello");
    stdin.flush().expect("flush Hello");

    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 4096];
    let mut total_read = 0usize;
    let welcome = loop {
        let n = stdout.read(&mut buf).expect("read from --connect stdout");
        assert!(n > 0, "--connect closed stdout before sending Welcome");
        total_read += n;
        decoder.push(&buf[..n]);
        if let Some(msg) = decoder
            .next_frame::<DaemonMessage>()
            .expect("decode DaemonMessage")
        {
            break msg;
        }
    };
    assert_eq!(
        welcome,
        DaemonMessage::Welcome {
            version: PROTOCOL_VERSION,
        }
    );

    // Every byte `--connect` wrote to stdout is accounted for by exactly the
    // Welcome frame: no log line ever reached it, regardless of the active
    // logging elsewhere in the system.
    let welcome_frame_len = encode_frame(&DaemonMessage::Welcome {
        version: PROTOCOL_VERSION,
    })
    .expect("encode Welcome")
    .len();
    assert_eq!(
        total_read, welcome_frame_len,
        "stdout must carry exactly the Welcome frame"
    );

    // Regression guard: `--connect` must never install its own file sink. Every
    // real launch (`crates/ssh/src/launch.rs`) drives `--connect` against the
    // *same* socket path as its `--serve-uds` daemon, so a socket-keyed default
    // there would collide with the daemon's own rotated file — two independent
    // processes rotating one file is exactly what `SizedWriter` is not safe
    // against.
    let connect_default_log = PathBuf::from(format!("{}.log", sock.display()));
    assert!(
        !connect_default_log.exists(),
        "--connect must not create its own socket-keyed log file"
    );

    drop(stdin);
    let _ = connect.kill();
    let _ = connect.wait();
}

#[test]
fn test_serve_uds_env_var_sets_log_file_when_flag_absent() {
    let scratch = Scratch::new("env-log-file");
    let sock = scratch.path("rift.sock");
    let env_log_file = scratch.path("from-env.log");

    let child = Command::new(daemon_bin())
        .arg("--serve-uds")
        .arg(&sock)
        .arg("--root")
        .arg(scratch.path("missing-root"))
        .env("RIFT_DAEMON_LOG_FILE", &env_log_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon --serve-uds");
    let _child = ChildGuard(child);

    wait_for_path(&sock);
    wait_for_content(&env_log_file, "rift-daemon listening");

    // The env var is a fallback for the *default*, not an addition to it: the
    // socket-keyed default path must never also be created.
    let default_log = PathBuf::from(format!("{}.log", sock.display()));
    assert!(
        !default_log.exists(),
        "RIFT_DAEMON_LOG_FILE must replace the default path, not sit alongside it"
    );
}

#[test]
fn test_serve_uds_log_file_flag_beats_env_var() {
    let scratch = Scratch::new("flag-beats-env");
    let sock = scratch.path("rift.sock");
    let flag_log_file = scratch.path("from-flag.log");
    let env_log_file = scratch.path("from-env.log");

    let child = Command::new(daemon_bin())
        .arg("--serve-uds")
        .arg(&sock)
        .arg("--root")
        .arg(scratch.path("missing-root"))
        .arg("--log-file")
        .arg(&flag_log_file)
        .env("RIFT_DAEMON_LOG_FILE", &env_log_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon --serve-uds");
    let _child = ChildGuard(child);

    wait_for_path(&sock);
    wait_for_content(&flag_log_file, "rift-daemon listening");

    assert!(
        read_to_string(&env_log_file).is_empty(),
        "the --log-file flag must win over RIFT_DAEMON_LOG_FILE, not merely also write"
    );
}
