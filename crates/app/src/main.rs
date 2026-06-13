// Console-free stable launcher: GUI subsystem instead of console, so a desktop
// shortcut launch opens no console window. Gated by the `windowed` feature (not
// `not(debug_assertions)` — the `stable` profile keeps debug-assertions on for the
// GPUI runtime-shader path); off by default so dev keeps its RUST_LOG console.
#![cfg_attr(feature = "windowed", windows_subsystem = "windows")]

use std::env;
use std::path::PathBuf;
use std::thread;

use anyhow::{Context as _, Result};
use gpui::*;
use gpui_component::Root;
use rift_app::apply_theme;
use rift_terminal::{
    CaptureRequest, CaptureResult, ConnectionStatus, PaneInput, PaneOutput, SelectWindow,
    SessionView, SubscriptionUpdate, TermSize, TERMINAL_KEY_CONTEXT,
};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

struct SshConfig {
    host: String,
    port: u16,
    user: String,
    key: PathBuf,
}

struct PtyChannels {
    pane_output_tx: flume::Sender<PaneOutput>,
    input_rx: flume::Receiver<PaneInput>,
    size_changed_rx: flume::Receiver<TermSize>,
    snapshot_tx: flume::Sender<termy_terminal_ui::TmuxSnapshot>,
    tmux_command_rx: flume::Receiver<String>,
    subscription_tx: flume::Sender<SubscriptionUpdate>,
    capture_request_rx: flume::Receiver<CaptureRequest>,
    capture_result_tx: flume::Sender<CaptureResult>,
    connection_status_tx: flume::Sender<ConnectionStatus>,
}

// Console builds log to stdout (the dev loop's RUST_LOG console). Windowed builds
// have no console, so a failed launch dies invisibly — they write a per-run log
// file next to the pinned exe instead (`%LOCALAPPDATA%\rift\rift-stable.log`,
// truncated each start) and route panics there too, since panics bypass tracing.
// Without RUST_LOG the dev-loop filter applies, so the file is useful by default.
fn init_logging() {
    let filter = || {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("rift=debug,rift_ssh=debug"))
    };

    #[cfg(feature = "windowed")]
    {
        let log_file = env::var_os("LOCALAPPDATA").and_then(|base| {
            let dir = PathBuf::from(base).join("rift");
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::File::create(dir.join("rift-stable.log")).ok()
        });
        if let Some(file) = log_file {
            tracing_subscriber::fmt()
                .with_env_filter(filter())
                .with_target(true)
                .with_ansi(false)
                .with_writer(std::sync::Arc::new(file))
                .init();
            let default_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                error!("panic: {info}");
                default_hook(info);
            }));
            return;
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(filter())
        .with_target(true)
        .init();
}

fn main() {
    // The stable profile keeps debug-assertions on, so GPUI resolves its
    // compile-time CARGO_MANIFEST_DIR paths at runtime (shader sources,
    // DirectWrite setup). Those are WSL paths — root-relative on Windows — and
    // only resolve while the current drive is the WSL distro root. Recipe
    // launches start inside WSL; an Explorer double-click starts on C:\ and
    // panics before any window appears. `just promote` bakes the WSL root
    // (RIFT_DEFAULT_WORKDIR) so the pinned shortcut launches from anywhere.
    // Best-effort: WSL-side launches already run on the right drive.
    if let Some(dir) = option_env!("RIFT_DEFAULT_WORKDIR") {
        let _ = env::set_current_dir(dir);
    }

    init_logging();

    info!(
        os = env::consts::OS,
        arch = env::consts::ARCH,
        "rift starting"
    );

    Application::with_platform(gpui_platform::current_platform(false)).run(|cx: &mut App| {
        gpui_component::init(cx);
        apply_theme(cx);
        // Alt+1..9 -> switch to window N. Unshifted modifier+digit needs no
        // keyboard-layout mapping, so it matches identically on Windows and
        // Linux/X11 (where GPUI's keyboard mapper is a no-op).
        cx.bind_keys(
            (1..=9usize).map(|n| KeyBinding::new(&format!("alt-{n}"), SelectWindow(n), None)),
        );
        // gpui-component's `Root` view binds `tab`/`shift-tab` to focus navigation
        // in the "Root" context. Root is an ancestor of every pane, so that action
        // consumes the keystroke before it reaches the pane's `on_key_down`, and the
        // terminal never receives Tab (shell completion, agent prompt suggestions).
        // Shadow it with `NoAction` in the deeper "Terminal" context: deepest context
        // wins, NoAction yields no binding, so the keystroke falls through to the
        // existing `encode_keystroke` path (`\t` / `\x1b[Z`). Scoped to "Terminal", so
        // Tab still navigates focus in dialogs and forms.
        cx.bind_keys([
            KeyBinding::new("tab", NoAction, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("shift-tab", NoAction, Some(TERMINAL_KEY_CONTEXT)),
        ]);
        let bounds = Bounds::centered(None, size(px(1200.0), px(800.0)), cx);
        // Per-channel window title (matching the per-channel taskbar icons), so the
        // mirrored stable and dev instances are distinguishable in alt-tab and
        // taskbar hover. Lowercase `rift` per brand rules.
        let title = if cfg!(feature = "windowed") {
            "rift"
        } else {
            "rift (dev)"
        };
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let session_view = cx.new(|cx| {
                    let (view, handle) = SessionView::new(cx);

                    let ssh = SshConfig {
                        host: env::var("RIFT_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
                        user: env::var("RIFT_SSH_USER").unwrap_or_else(|_| "developer".into()),
                        port: env::var("RIFT_SSH_PORT")
                            .ok()
                            .and_then(|p| p.parse().ok())
                            .unwrap_or(22),
                        key: env::var("RIFT_SSH_KEY")
                            .ok()
                            // Compile-time default baked by `just promote`
                            // (RIFT_DEFAULT_SSH_KEY), so the pinned stable exe
                            // launches from a bare desktop shortcut without any
                            // user env; runtime RIFT_SSH_KEY still wins.
                            .or_else(|| option_env!("RIFT_DEFAULT_SSH_KEY").map(String::from))
                            .map(PathBuf::from)
                            .unwrap_or_else(|| {
                                let home = env::var("USERPROFILE")
                                    .or_else(|_| env::var("HOME"))
                                    .unwrap_or_else(|_| {
                                        if cfg!(target_os = "windows") {
                                            "C:\\Users\\Default".into()
                                        } else {
                                            "/home/developer".into()
                                        }
                                    });
                                PathBuf::from(home).join(".ssh").join("id_ed25519")
                            }),
                    };

                    // Kept outside `channels` so the session thread can flip the
                    // indicator to Disconnected once `run_ssh_session` returns
                    // (the in-session clone reports Connected).
                    let status_tx = handle.connection_status_tx.clone();

                    let channels = PtyChannels {
                        pane_output_tx: handle.pane_output_tx,
                        input_rx: handle.input_rx,
                        size_changed_rx: handle.size_changed_rx,
                        snapshot_tx: handle.snapshot_tx,
                        tmux_command_rx: handle.tmux_command_rx,
                        subscription_tx: handle.subscription_tx,
                        capture_request_rx: handle.capture_request_rx,
                        capture_result_tx: handle.capture_result_tx,
                        connection_status_tx: handle.connection_status_tx,
                    };

                    let key_exists = ssh.key.exists();
                    debug!(
                        host = %ssh.host,
                        port = ssh.port,
                        user = %ssh.user,
                        key = %ssh.key.display(),
                        key_exists,
                        "connecting via SSH"
                    );

                    thread::spawn(move || {
                        let rt =
                            tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                        rt.block_on(async move {
                            if let Err(e) = run_ssh_session(&ssh, channels).await {
                                error!(
                                    %e,
                                    host = %ssh.host,
                                    port = ssh.port,
                                    key = %ssh.key.display(),
                                    key_exists,
                                    "SSH session failed"
                                );
                            }
                            let _ = status_tx.send(ConnectionStatus::Disconnected);
                        });
                    });

                    view
                });

                let focus_handle = session_view.focus_handle(cx);
                window.defer(cx, move |window, cx| {
                    focus_handle.focus(window, cx);
                });

                cx.new(|cx| Root::new(session_view, window, cx))
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

async fn run_ssh_session(ssh: &SshConfig, ch: PtyChannels) -> Result<()> {
    use rift_ssh::SshConnection;
    use termy_terminal_ui::{TmuxClient, TmuxNotification, TmuxSocketTarget};

    let mut conn = SshConnection::connect(&ssh.host, ssh.port, &ssh.user, &ssh.key)
        .await
        .context("SSH connection failed")?;

    // Provision the daemon ahead of the tmux session: detect the platform,
    // upload the versioned binary when absent, then attach — spawning it
    // detached if none is running — and confirm the transport with a handshake.
    // The detached daemon survives SSH drops, so a reconnect reattaches instead
    // of spawning a second one (#62). Gated on RIFT_DAEMON_BINARY; without it,
    // skip and fall through to the existing tmux flow so the app still runs.
    provision_daemon(&mut conn).await;

    // Tmux session name, overridable so a second rift instance can mirror the
    // same live session (default `rift`) or attach to an isolated one for
    // destructive tests (`RIFT_SESSION=rift-dev`). Matches the SshConfig env
    // pattern above. See docs/spec-dogfooding-channels.md.
    let session = env::var("RIFT_SESSION").unwrap_or_else(|_| "rift".to_string());

    let pty = conn
        .open_pty_exec(80, 24, &format!("tmux -CC new-session -A -s {session}"))
        .await
        .context("failed to start tmux control mode")?;

    let reader = pty.sync_reader();
    let writer = pty.sync_writer();

    let (wakeup_tx, wakeup_rx) = flume::bounded::<()>(1);

    let tmux_client = TmuxClient::from_streams(
        writer,
        reader,
        session,
        "tmux".to_string(),
        TmuxSocketTarget::Default,
        Some(wakeup_tx),
    )
    .context("failed to create tmux control client")?;

    tmux_client
        .set_client_size(80, 24)
        .context("failed to set initial tmux client size")?;

    tmux_client
        .send_command_async("refresh-client -f pause-after=5")
        .context("failed to activate flow control")?;

    // Register format subscriptions so pane/window state changes (cd, command,
    // window rename) stream in within ~1s instead of waiting for a structural
    // refresh. Requires tmux 3.4+; on older servers each call returns an error
    // and we degrade to snapshot-only rather than failing the session.
    for (name, scope, format) in [
        ("rift_pane_path", "%*", "#{pane_current_path}"),
        ("rift_pane_command", "%*", "#{pane_current_command}"),
        ("rift_window_name", "@*", "#{window_name}"),
    ] {
        if let Err(e) = tmux_client.subscribe(name, scope, format) {
            warn!(%e, name, "failed to register tmux subscription; continuing snapshot-only");
        }
    }

    info!("tmux control mode connected");
    let _ = ch.connection_status_tx.send(ConnectionStatus::Connected);

    let pane_output_tx = ch.pane_output_tx;
    let input_rx = ch.input_rx;
    let size_changed_rx = ch.size_changed_rx;
    let snapshot_tx = ch.snapshot_tx;
    let tmux_command_rx = ch.tmux_command_rx;
    let subscription_tx = ch.subscription_tx;
    let capture_request_rx = ch.capture_request_rx;
    let capture_result_tx = ch.capture_result_tx;

    let initial_snapshot = tmux_client
        .refresh_snapshot()
        .context("failed to get initial tmux snapshot")?;
    let _ = snapshot_tx.send(initial_snapshot);

    let tmux_for_input = std::sync::Arc::new(tmux_client);
    let tmux_for_resize = tmux_for_input.clone();
    let tmux_for_poll = tmux_for_input.clone();
    let tmux_for_cmd = tmux_for_input.clone();
    let tmux_for_capture = tmux_for_input.clone();

    let input_handle = std::thread::spawn(move || {
        while let Ok(input) = input_rx.recv() {
            if tmux_for_input
                .send_input(&input.pane_id, &input.bytes)
                .is_err()
            {
                break;
            }
        }
    });

    let resize_handle = std::thread::spawn(move || {
        while let Ok(new_size) = size_changed_rx.recv() {
            if tmux_for_resize
                .set_client_size(new_size.cols as u16, new_size.rows as u16)
                .is_err()
            {
                break;
            }
        }
    });

    let cmd_handle = std::thread::spawn(move || {
        while let Ok(cmd) = tmux_command_rx.recv() {
            debug!(cmd = %cmd, "sending tmux command");
            if tmux_for_cmd.send_command_async(&cmd).is_err() {
                break;
            }
        }
    });

    // Pre-attach scrollback capture. `capture_pane_range` goes through termy's
    // internal control-channel worker (10s timeout), so a blocking capture here
    // is demultiplexed against the poll loop's `%output` stream. An empty payload
    // on error lets the pane clear its in-flight flag and retry.
    let capture_handle = std::thread::spawn(move || {
        while let Ok(req) = capture_request_rx.recv() {
            let bytes = tmux_for_capture
                .capture_pane_range(&req.pane_id, &req.start_row, &req.end_row, req.join_wraps)
                .unwrap_or_default();
            if capture_result_tx
                .send(CaptureResult {
                    pane_id: req.pane_id,
                    bytes,
                })
                .is_err()
            {
                break;
            }
        }
    });

    let poll_handle = std::thread::spawn(move || loop {
        if wakeup_rx.recv().is_err() {
            break;
        }
        let notifications = tmux_for_poll.poll_notifications();
        let mut should_exit = false;
        for notification in notifications {
            match notification {
                TmuxNotification::Output { pane_id, bytes } => {
                    if pane_output_tx.send(PaneOutput { pane_id, bytes }).is_err() {
                        should_exit = true;
                        break;
                    }
                }
                TmuxNotification::NeedsRefresh => {
                    if let Ok(snapshot) = tmux_for_poll.refresh_snapshot() {
                        let _ = snapshot_tx.send(snapshot);
                    }
                }
                TmuxNotification::SubscriptionChanged {
                    name,
                    session,
                    window,
                    pane,
                    value,
                } => {
                    if subscription_tx
                        .send(SubscriptionUpdate {
                            name,
                            session,
                            window,
                            pane,
                            value,
                        })
                        .is_err()
                    {
                        should_exit = true;
                        break;
                    }
                }
                TmuxNotification::Exit(reason) => {
                    info!(?reason, "tmux control mode exited");
                    should_exit = true;
                    break;
                }
                TmuxNotification::Warning(msg) => {
                    tracing::warn!(%msg, "tmux control warning");
                }
            }
        }
        if should_exit {
            break;
        }
    });

    let _ = poll_handle.join();
    let _ = input_handle.join();
    let _ = resize_handle.join();
    let _ = cmd_handle.join();
    let _ = capture_handle.join();
    Ok(())
}

/// Best-effort daemon provisioning, run before the tmux session is opened.
///
/// Reads the locally-built musl binary from `RIFT_DAEMON_BINARY` (target dir
/// `RIFT_DAEMON_REMOTE_DIR`, default `$HOME/.rift/bin`), deploys the versioned
/// binary via [`rift_ssh::ensure_daemon_deployed`], then attaches to the remote
/// daemon — spawning it detached if none is running — via
/// [`rift_ssh::connect_or_spawn_daemon`] and confirms the transport with a
/// `Hello`/`Welcome` handshake. The detached daemon outlives the SSH connection,
/// so a later reconnect reattaches to it instead of spawning a second one (#62).
///
/// After the handshake the client stays alive: a spawned consumer task applies
/// the daemon's worktree stream (`WorktreeSnapshot` / `UpdateWorktree`) to the
/// client-side [`rift_app::worktree::WorktreeModel`] and logs the resulting
/// tree state — the protocol's first consumer (#111); rendering is a later
/// sub-spec.
///
/// Every step is best-effort: a missing `RIFT_DAEMON_BINARY` or any error is
/// logged and swallowed so the existing tmux flow keeps working without the
/// daemon. The socket and log sit beside the versioned binary
/// (`<binary>.sock` / `<binary>.log`), inheriting its resolved path.
async fn provision_daemon(conn: &mut rift_ssh::SshConnection) {
    // An unset or empty `RIFT_DAEMON_BINARY` skips provisioning: the dev recipes
    // forward the var unconditionally and default it to empty, so empty must read
    // as "not configured" rather than a path to read.
    let binary_path = match env::var_os("RIFT_DAEMON_BINARY") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            debug!("RIFT_DAEMON_BINARY not set, skipping daemon provisioning");
            return;
        }
    };

    let bytes = match tokio::fs::read(&binary_path).await {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!(%e, path = %binary_path.display(), "failed to read local daemon binary, skipping daemon");
            return;
        }
    };

    let remote_dir =
        env::var("RIFT_DAEMON_REMOTE_DIR").unwrap_or_else(|_| "$HOME/.rift/bin".to_string());

    let remote_path = match rift_ssh::ensure_daemon_deployed(
        conn,
        &bytes,
        &remote_dir,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    {
        Ok(remote_path) => {
            info!(remote_path, "daemon auto-deploy complete");
            remote_path
        }
        Err(e) => {
            warn!(%e, "daemon auto-deploy failed, continuing with tmux only");
            return;
        }
    };

    // Socket and log sit beside the versioned binary, inheriting its already
    // resolved absolute path and version (no second $HOME resolution needed).
    let socket_path = format!("{remote_path}.sock");
    let log_path = format!("{remote_path}.log");

    // Project root the daemon should watch: RIFT_PROJECT_ROOT (runtime) wins over
    // a `just promote` compile-time bake (RIFT_DEFAULT_PROJECT_ROOT), mirroring the
    // RIFT_SSH_KEY / RIFT_DEFAULT_SSH_KEY split. None leaves the daemon on its
    // launch directory; the root is only honored on a fresh spawn, so a reattach
    // keeps the already-running daemon's root.
    let project_root = env::var("RIFT_PROJECT_ROOT")
        .ok()
        .or_else(|| option_env!("RIFT_DEFAULT_PROJECT_ROOT").map(String::from));

    let channel = match rift_ssh::connect_or_spawn_daemon(
        conn,
        &remote_path,
        &socket_path,
        &log_path,
        project_root.as_deref(),
    )
    .await
    {
        Ok(channel) => channel,
        Err(e) => {
            warn!(%e, "daemon attach failed, continuing with tmux only");
            return;
        }
    };

    // Confirm the reattach transport with a protocol round-trip, then hand the
    // live client to the consumer task. The daemon re-broadcasts the full
    // worktree snapshot on every Hello, so the stream following the Welcome
    // starts with the complete tree.
    let client = rift_ssh::DaemonClient::new(channel);
    if let Err(e) = client
        .send(rift_protocol::ClientMessage::Hello {
            version: rift_protocol::PROTOCOL_VERSION,
        })
        .await
    {
        warn!(%e, "daemon handshake send failed");
        return;
    }
    match client.recv().await {
        Some(rift_protocol::DaemonMessage::Welcome { version }) => {
            info!(version, "daemon transport ready (Hello/Welcome ok)");
            // Spawned on the session thread's runtime: the task lives as long
            // as the SSH session (`block_on` in main) and is cancelled silently
            // when the runtime drops — the "stream ended" log only fires on a
            // clean channel close. `DaemonClient` owns its channel actor, so
            // cancellation is a plain drop, never a dangling connection.
            tokio::spawn(consume_daemon_messages(client));
        }
        Some(other) => warn!(?other, "unexpected daemon handshake reply"),
        None => warn!("daemon closed before Welcome"),
    }
}

/// Drive the daemon message stream into the client-side worktree model.
///
/// Owns the [`rift_ssh::DaemonClient`] for the session's lifetime and folds
/// every worktree message into a [`rift_app::worktree::WorktreeModel`],
/// logging the tree state for headless verification — no rendered consumer
/// yet (the explorer panel is its own sub-spec). Ends when the channel closes;
/// the detached daemon keeps running for the next attach.
async fn consume_daemon_messages(client: rift_ssh::DaemonClient) {
    use rift_app::worktree::WorktreeModel;
    use rift_protocol::DaemonMessage;

    let mut model = WorktreeModel::default();
    while let Some(msg) = client.recv().await {
        match msg {
            DaemonMessage::WorktreeSnapshot {
                root,
                entries,
                final_chunk,
            } => {
                let chunk_len = entries.len();
                if model.apply_snapshot_chunk(root, entries, final_chunk) {
                    info!(
                        root = model.root().unwrap_or(""),
                        entries = model.len(),
                        "worktree snapshot applied"
                    );
                } else {
                    debug!(chunk_len, "worktree snapshot chunk buffered");
                }
            }
            DaemonMessage::UpdateWorktree {
                added,
                changed,
                removed,
            } => {
                let (a, c, r) = (added.len(), changed.len(), removed.len());
                if model.apply_update(added, changed, removed) {
                    debug!(
                        added = a,
                        changed = c,
                        removed = r,
                        entries = model.len(),
                        "worktree update applied"
                    );
                } else {
                    debug!(
                        added = a,
                        changed = c,
                        removed = r,
                        "worktree update before first snapshot dropped"
                    );
                }
            }
            DaemonMessage::UpdateGitStatus { changed, cleared } => {
                let (c, r) = (changed.len(), cleared.len());
                model.apply_git_update(changed, cleared);
                debug!(
                    changed = c,
                    cleared = r,
                    decorated = model.git_statuses().len(),
                    "git status update applied"
                );
            }
            DaemonMessage::RepoState {
                branch,
                ahead_behind,
            } => {
                debug!(branch = ?branch, ahead_behind = ?ahead_behind, "repo state applied");
                model.apply_repo_state(branch, ahead_behind);
            }
            DaemonMessage::Diagnostics {
                path,
                server,
                items,
            } => {
                let count = items.len();
                debug!(%path, %server, items = count, "diagnostics received");
                model.apply_diagnostics(path, server, items);
                debug!(total = model.diagnostic_count(), "diagnostics applied");
            }
            other => debug!(?other, "daemon message without a consumer yet"),
        }
    }
    info!("daemon message stream ended");
}
