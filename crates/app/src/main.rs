// Console-free stable launcher: GUI subsystem instead of console, so a desktop
// shortcut launch opens no console window. Gated by the `windowed` feature (not
// `not(debug_assertions)` — the `stable` profile keeps debug-assertions on for the
// GPUI runtime-shader path); off by default so dev keeps its RUST_LOG console.
#![cfg_attr(feature = "windowed", windows_subsystem = "windows")]

use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::thread;

use anyhow::{Context as _, Result};
use gpui::*;
use gpui_component::Root;
use rift_app::{apply_theme, window_state, workspace};
use rift_logging::{
    LogTarget, RotatingMakeWriter, SizedWriter, DEFAULT_MAX_BYTES, FORCE_CONSOLE_ENV,
};
use rift_terminal::{
    CaptureRequest, CaptureResult, ConnectionStatus, KeyTableQueryResult, PaneInput, PaneOutput,
    SelectWindow, SessionView, SubscriptionUpdate, TermSize, TERMINAL_KEY_CONTEXT,
};
use tracing::{debug, error, info, warn};

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
    /// An explicit key-table refresh request from the render layer's statusbar
    /// button; forwarded onto the protocol as `ClientMessage::QueryKeyTable` in
    /// daemon mode. (A binding-mutating dispatch's refresh is issued
    /// server-side by `spawn_command_bridge`, not carried on this channel.)
    /// Unused in the legacy tmux path (`docs/spec-tmux-keytable-mirroring.md`
    /// scopes the live refresh to the daemon seam) — a request there is a
    /// harmless no-op once its receiver drops.
    key_table_request_rx: flume::Receiver<()>,
    /// The parsed-ready reply to a key-table refresh, routed to `SessionView`.
    key_table_result_tx: flume::Sender<KeyTableQueryResult>,
}

/// The daemon-side endpoints of the editor surface's buffer-channel and worktree
/// wiring (#187, #188). The tokio session reader (`consume_daemon_messages`)
/// forwards worktree-family messages and the buffer-channel replies
/// (`FileContent` / `SaveResult` / `SaveConflict`) onto these senders; an
/// open-file bridge drains `open_file_rx` into `ClientMessage::OpenFile` reads and
/// a save-file bridge drains `save_file_rx` into `SaveFile` writes. The matching
/// GPUI-side endpoints live on [`rift_app::workspace::WorkspaceChannels`].
struct EditorChannels {
    /// Worktree-family daemon messages routed to the file tree's model.
    worktree_tx: flume::Sender<rift_protocol::DaemonMessage>,
    /// Buffer-channel replies routed to the editor: `FileContent` (load),
    /// `SaveResult` (save landed), `SaveConflict` (save refused).
    buffer_tx: flume::Sender<rift_protocol::DaemonMessage>,
    /// Nav replies routed to the editor: `DefinitionResponse` (#196).
    nav_tx: flume::Sender<rift_protocol::DaemonMessage>,
    /// `StatusLineReply` routed to the workspace's mirrored-status-line render
    /// model (#221, `docs/spec-tmux-statusline-mirroring.md`).
    status_line_tx: flume::Sender<rift_protocol::DaemonMessage>,
    /// Root-relative paths to open, emitted by the tree (or the editor's
    /// auto-reload); each becomes an `OpenFile` request.
    open_file_rx: flume::Receiver<String>,
    /// `SaveFile` write requests the editor built from the open buffer; forwarded
    /// onto the protocol verbatim by the save-file bridge.
    save_file_rx: flume::Receiver<rift_protocol::ClientMessage>,
    /// Live-buffer feed (#189): `BufferChanged` / `BufferClosed` the editor emits
    /// so the daemon feeds the LSP the live buffer; forwarded onto the protocol
    /// verbatim by the buffer-change bridge.
    buffer_change_rx: flume::Receiver<rift_protocol::ClientMessage>,
    /// Navigation requests: `DefinitionRequest` (#196).
    nav_request_rx: flume::Receiver<rift_protocol::ClientMessage>,
    /// `FileDiff` replies routed to the diff view (#338).
    diff_tx: flume::Sender<rift_protocol::DaemonMessage>,
    /// Root-relative paths to diff, emitted by the source-control panel on
    /// selection; each becomes a `RequestDiff` request.
    request_diff_rx: flume::Receiver<String>,
}

/// Per-channel log-pair basename, keyed off the same `windowed` feature that
/// selects the stable build (`docs/spec-dogfooding-channels.md`) — so the
/// side-by-side stable and dev dogfooding instances never share a rotation pair.
fn log_channel() -> &'static str {
    if cfg!(feature = "windowed") {
        "rift-stable"
    } else {
        "rift-dev"
    }
}

/// `%LOCALAPPDATA%\rift\<channel>.log` — the file sink's target path. `None` when
/// `LOCALAPPDATA` is unset (off Windows), in which case the file sink falls back
/// to console.
fn log_file_path() -> Option<PathBuf> {
    let base = env::var_os("LOCALAPPDATA")?;
    Some(
        PathBuf::from(base)
            .join("rift")
            .join(format!("{}.log", log_channel())),
    )
}

// Sink selection is a runtime TTY check (rift_logging::log_target_from), not the
// old compile-time `windowed` gate — one mechanism covers the dev console, the
// windowed stable build, and a redirected/piped launch (e.g. dev-windows over the
// WSL binfmt pipe relay), with RIFT_LOG_CONSOLE forcing either direction. The
// app's console is stdout (the writer below), so the TTY check keys off stdout —
// `log_target()`'s default checks stderr, which fits the daemon's sink instead.
// A console launch logs to stdout (the dev loop's RUST_LOG console); everything
// else logs to a rotated `.log`/`.log.old` append pair keyed by channel, so a
// restart's previous run survives instead of being truncated. The panic hook
// installs in every profile, since panics bypass tracing's normal call sites.
fn init_logging() {
    let filter = rift_logging::build_filter();
    let target = rift_logging::log_target_from(
        env::var(FORCE_CONSOLE_ENV).ok().as_deref(),
        std::io::stdout().is_terminal(),
    );

    let file_writer = match target {
        LogTarget::Console => None,
        LogTarget::File => log_file_path()
            .and_then(|path| SizedWriter::new(path, DEFAULT_MAX_BYTES).ok())
            .map(RotatingMakeWriter::new),
    };

    match file_writer {
        Some(writer) => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(true)
                .with_ansi(false)
                .with_writer(writer)
                .init();
        }
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(true)
                .init();
        }
    }

    rift_logging::install_panic_hook();
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
        // Command palette (#359, `docs/spec-command-palette.md`): Ctrl+Shift+P /
        // Cmd+Shift+P opens the palette. Unscoped (`None`), like `SelectWindow`
        // above, so it reaches the shortcut regardless of which surface is
        // focused, including the terminal.
        cx.bind_keys([
            KeyBinding::new(
                "ctrl-shift-p",
                rift_app::command_palette::OpenCommandPalette,
                None,
            ),
            KeyBinding::new(
                "cmd-shift-p",
                rift_app::command_palette::OpenCommandPalette,
                None,
            ),
        ]);
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
        // Save the open buffer over the buffer channel (#188). Scoped to the
        // editor's key context so it fires only when focus is in the editor, never
        // for an unrelated input. Both chords are bound so the binding matches the
        // host's muscle memory (Ctrl+S on Windows/Linux, Cmd+S on macOS) without a
        // per-OS cfg — the inactive chord simply never arrives.
        cx.bind_keys([
            KeyBinding::new(
                "ctrl-s",
                rift_app::editor::Save,
                Some(rift_app::editor::EDITOR_KEY_CONTEXT),
            ),
            KeyBinding::new(
                "cmd-s",
                rift_app::editor::Save,
                Some(rift_app::editor::EDITOR_KEY_CONTEXT),
            ),
            // Go-to-definition (#196): F12 mirrors VS Code / JetBrains muscle memory.
            // Ctrl+click fires the action programmatically (not via this binding).
            KeyBinding::new(
                "f12",
                rift_app::editor::GoToDefinition,
                Some(rift_app::editor::EDITOR_KEY_CONTEXT),
            ),
            // Back-navigation (#196): Alt+Left mirrors VS Code / JetBrains muscle memory.
            KeyBinding::new(
                "alt-left",
                rift_app::editor::GoBack,
                Some(rift_app::editor::EDITOR_KEY_CONTEXT),
            ),
            // Hover popover (#197): Shift+K mirrors VS Code / Neovim (`K` in Normal mode)
            // muscle memory. Fires `ShowHover` at the cursor position; the result
            // renders as a markdown popover anchored to the bottom of the editor area.
            // Mouse-rest (500 ms debounce) also triggers hover automatically.
            KeyBinding::new(
                "shift-k",
                rift_app::editor::ShowHover,
                Some(rift_app::editor::EDITOR_KEY_CONTEXT),
            ),
            // Find references (#198): Shift+F12 mirrors VS Code muscle memory.
            // Also available via the context-menu "Find References" entry.
            KeyBinding::new(
                "shift-f12",
                rift_app::editor::FindReferences,
                Some(rift_app::editor::EDITOR_KEY_CONTEXT),
            ),
        ]);
        // Explorer keyboard navigation (#332): up/down move the selection,
        // left/right collapse/expand (stepping to parent/first-child at the
        // edges), Enter opens/toggles, Home/End jump to the first/last visible
        // row. Scoped to the tree's own key context, so a focused terminal
        // pane's keystrokes are never intercepted (agent-first).
        cx.bind_keys([
            KeyBinding::new(
                "up",
                rift_app::file_tree::SelectUp,
                Some(rift_app::file_tree::FILE_TREE_KEY_CONTEXT),
            ),
            KeyBinding::new(
                "down",
                rift_app::file_tree::SelectDown,
                Some(rift_app::file_tree::FILE_TREE_KEY_CONTEXT),
            ),
            KeyBinding::new(
                "left",
                rift_app::file_tree::CollapseOrSelectParent,
                Some(rift_app::file_tree::FILE_TREE_KEY_CONTEXT),
            ),
            KeyBinding::new(
                "right",
                rift_app::file_tree::ExpandOrSelectChild,
                Some(rift_app::file_tree::FILE_TREE_KEY_CONTEXT),
            ),
            KeyBinding::new(
                "enter",
                rift_app::file_tree::OpenSelected,
                Some(rift_app::file_tree::FILE_TREE_KEY_CONTEXT),
            ),
            KeyBinding::new(
                "home",
                rift_app::file_tree::SelectFirst,
                Some(rift_app::file_tree::FILE_TREE_KEY_CONTEXT),
            ),
            KeyBinding::new(
                "end",
                rift_app::file_tree::SelectLast,
                Some(rift_app::file_tree::FILE_TREE_KEY_CONTEXT),
            ),
        ]);
        // Window-state restore (#225, docs/spec-window-state-persistence.md):
        // resolve this instance's channel-keyed state file, load it (defaulting
        // on any read/parse failure per `window_state::load`'s own tolerant
        // contract), and clamp its bounds against the live display topology —
        // all before the window is ever created, so the restore lands before
        // first paint. `state_path` is `None` only when no platform state
        // directory could be resolved at all (`LOCALAPPDATA`/`XDG_STATE_HOME`/
        // `HOME` all unset); the window then opens at today's default and every
        // save site below no-ops instead of crashing.
        let state_path = match window_state::state_path() {
            Ok(path) => Some(path),
            Err(e) => {
                warn!(%e, "window-state persistence disabled");
                None
            }
        };
        let restored = state_path
            .as_deref()
            .map(window_state::load)
            .unwrap_or_default();
        let window_bounds =
            workspace::initial_window_bounds(&restored, &workspace::display_rects(cx));
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
                window_bounds: Some(window_bounds),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                // Editor surface (#187) wiring: the daemon stream reader forwards
                // worktree structure and `FileContent` replies onto these, and the
                // tree's open requests come back on `open_file`. The daemon-side
                // ends thread into the SSH session below; the GPUI-side ends into
                // the `WorkspaceView`.
                let (worktree_tx, worktree_rx) = flume::unbounded();
                let (buffer_tx, buffer_rx) = flume::unbounded();
                let (nav_daemon_tx, nav_rx) = flume::unbounded::<rift_protocol::DaemonMessage>();
                let (status_line_tx, status_line_rx) =
                    flume::unbounded::<rift_protocol::DaemonMessage>();
                let (open_file_tx, open_file_rx) = flume::unbounded::<String>();
                let (save_file_tx, save_file_rx) =
                    flume::unbounded::<rift_protocol::ClientMessage>();
                let (buffer_change_tx, buffer_change_rx) =
                    flume::unbounded::<rift_protocol::ClientMessage>();
                let (nav_request_tx, nav_request_rx) =
                    flume::unbounded::<rift_protocol::ClientMessage>();
                let (diff_tx, diff_rx) = flume::unbounded::<rift_protocol::DaemonMessage>();
                let (request_diff_tx, request_diff_rx) = flume::unbounded::<String>();

                let session_view = cx.new(|cx| {
                    let (mut view, handle) = SessionView::new(cx);
                    // Font-size restore (#225): seed before the first tmux
                    // snapshot creates any panes, so every pane picks up the
                    // restored size from the start rather than flashing the
                    // default and then resizing.
                    view.seed_font_size(restored.font_size_px);

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
                        key_table_request_rx: handle.key_table_request_rx,
                        key_table_result_tx: handle.key_table_result_tx,
                    };

                    let editor_channels = EditorChannels {
                        worktree_tx,
                        buffer_tx,
                        nav_tx: nav_daemon_tx,
                        status_line_tx,
                        open_file_rx,
                        save_file_rx,
                        buffer_change_rx,
                        nav_request_rx,
                        diff_tx,
                        request_diff_rx,
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
                            if let Err(e) = run_ssh_session(&ssh, channels, editor_channels).await {
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

                // The app root: the file-tree explorer + code editor mounted beside
                // the terminal (#187). `SessionView` lives in `rift-terminal`, which
                // cannot reach `rift-app`'s explorer/editor, so the composition lives
                // here. Focus still delegates to the terminal so keystrokes reach the
                // active pane.
                let workspace = cx.new(|cx| {
                    workspace::WorkspaceView::new(
                        session_view,
                        workspace::WorkspaceChannels {
                            worktree_rx,
                            buffer_rx,
                            nav_rx,
                            status_line_rx,
                            diff_rx,
                            open_file_tx,
                            save_file_tx,
                            buffer_change_tx,
                            nav_tx: nav_request_tx,
                            request_diff_tx,
                        },
                        state_path.clone(),
                        window,
                        cx,
                    )
                });

                let focus_handle = workspace.focus_handle(cx);
                window.defer(cx, move |window, cx| {
                    focus_handle.focus(window, cx);
                });

                cx.new(|cx| Root::new(workspace, window, cx))
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

async fn run_ssh_session(ssh: &SshConfig, ch: PtyChannels, editor: EditorChannels) -> Result<()> {
    use rift_ssh::SshConnection;

    let mut conn = SshConnection::connect(&ssh.host, ssh.port, &ssh.user, &ssh.key)
        .await
        .context("SSH connection failed")?;

    // Provision the daemon ahead of the terminal: detect the platform, upload the
    // versioned binary when absent, then attach — spawning it detached if none is
    // running — and confirm the transport with a handshake. The detached daemon
    // survives SSH drops, so a reconnect reattaches instead of spawning a second
    // one (#62). Returns the live client on success; `None` when no daemon binary
    // is configured (or a step fails), in which case the legacy tmux path still
    // runs without daemon-backed features.
    let daemon_client = provision_daemon(&mut conn).await;

    // Tmux session name, overridable so a second rift instance can mirror the
    // same live session (default `rift`) or attach to an isolated one for
    // destructive tests (`RIFT_SESSION=rift-dev`). Matches the SshConfig env
    // pattern above. See docs/spec-dogfooding-channels.md.
    let session = env::var("RIFT_SESSION").unwrap_or_else(|_| "rift".to_string());

    // Terminal byte source (Phase 6 swap, #205): the daemon protocol is the
    // default; the legacy direct `tmux -CC` over an SSH PTY stays as an
    // env-selected escape hatch until the milestone QA gate (gate decision in
    // docs/spec-terminal-streaming.md). The render stack is identical either way —
    // only where the bytes come from changes.
    if use_daemon_terminal() {
        match daemon_client {
            Some(client) => {
                info!("terminal source: daemon protocol");
                return run_daemon_terminal(client, session, ch, editor).await;
            }
            None => warn!(
                "daemon terminal selected but no daemon available; \
                 falling back to the legacy tmux path"
            ),
        }
    } else if let Some(client) = daemon_client {
        // Legacy terminal, but keep the daemon's worktree/git/diagnostics +
        // buffer-channel stream alive on its own task (today's behavior) while
        // tmux drives the terminal.
        info!("terminal source: legacy tmux (daemon worktree stream active)");
        let client = std::sync::Arc::new(client);
        spawn_open_file_bridge(client.clone(), editor.open_file_rx.clone());
        spawn_save_file_bridge(client.clone(), editor.save_file_rx.clone());
        spawn_buffer_change_bridge(client.clone(), editor.buffer_change_rx.clone());
        spawn_nav_bridge(client.clone(), editor.nav_request_rx.clone());
        spawn_request_diff_bridge(client.clone(), editor.request_diff_rx.clone());
        tokio::spawn(consume_daemon_messages(client, None, editor));
    } else {
        info!("terminal source: legacy tmux (no daemon configured)");
    }

    run_legacy_terminal(conn, session, ch).await
}

/// Whether the terminal sources its bytes from the daemon protocol (the default)
/// rather than the legacy direct `tmux -CC` path. Any non-empty
/// `RIFT_TERMINAL_LEGACY` selects the legacy escape hatch; the dev recipes
/// forward the var so the fallback is operable end-to-end.
fn use_daemon_terminal() -> bool {
    // True (daemon) unless RIFT_TERMINAL_LEGACY is set non-empty: none-or-empty
    // selects the daemon path, a non-empty value the legacy escape hatch.
    env::var_os("RIFT_TERMINAL_LEGACY").is_none_or(|v| v.is_empty())
}

/// Drive the terminal entirely over the daemon protocol: open this client's tmux
/// attach, bridge the reverse path (input, resize, raw commands, capture) onto
/// the protocol, and fold the daemon's pane-output / layout / worktree stream
/// into the existing render channels. Blocks until the daemon channel closes or
/// the tmux attach reports `TerminalExit`; returning ends the session
/// (`Disconnected` → quit), matching the legacy `tmux` exit path. The SSH
/// connection (`conn`) stays alive for the session because it outlives this await
/// in [`run_ssh_session`]'s frame.
async fn run_daemon_terminal(
    client: rift_ssh::DaemonClient,
    session: String,
    ch: PtyChannels,
    editor: EditorChannels,
) -> Result<()> {
    use rift_protocol::ClientMessage;
    use std::sync::Arc;

    let client = Arc::new(client);

    // Open this client's own tmux attach, carrying `RIFT_SESSION` end-to-end. The
    // daemon answers with a LayoutSnapshot baseline, then the live stream.
    if let Err(e) = client
        .send(ClientMessage::Attach {
            session: session.clone(),
        })
        .await
    {
        warn!(%e, "failed to open daemon terminal attach");
        return Ok(());
    }
    let _ = ch.connection_status_tx.send(ConnectionStatus::Connected);

    // Reverse-path bridges: each forwards a render-side flume stream onto the
    // protocol. They live as long as the daemon client; a closed channel ends the
    // bridge. Pane ids cross the seam as tmux's native `%N` form.
    spawn_input_bridge(client.clone(), ch.input_rx);
    spawn_resize_bridge(client.clone(), ch.size_changed_rx);
    spawn_command_bridge(client.clone(), ch.tmux_command_rx);
    spawn_capture_bridge(client.clone(), ch.capture_request_rx);
    // Key-table refresh reverse path (tmux key-table mirroring, #212): each
    // request becomes a `QueryKeyTable`; the daemon also issues one unprompted
    // on attach, so this bridge only carries the statusbar's explicit-refresh
    // trigger (a binding-mutating dispatch's refresh is issued inline by
    // `spawn_command_bridge` instead, ordered after the mutation on the same
    // task). The reply returns via `consume_daemon_messages` on
    // `sinks.key_table_result_tx`.
    spawn_key_table_bridge(client.clone(), ch.key_table_request_rx);
    // Buffer channel reverse path: the editor's open requests become `OpenFile`
    // reads (#187) and its save requests `SaveFile` writes (#188). The forward
    // replies (`FileContent` / `SaveResult` / `SaveConflict`) return via
    // `consume_daemon_messages` on `editor.buffer_tx`.
    spawn_open_file_bridge(client.clone(), editor.open_file_rx.clone());
    spawn_save_file_bridge(client.clone(), editor.save_file_rx.clone());
    // Live-buffer feed reverse path (#189): the editor's `BufferChanged` /
    // `BufferClosed` forward verbatim so the daemon feeds the LSP the live buffer.
    // Push-only — diagnostics return on the worktree stream as `Diagnostics`.
    spawn_buffer_change_bridge(client.clone(), editor.buffer_change_rx.clone());
    // Navigation request reverse path (#196): `DefinitionRequest` forwards verbatim.
    // The `DefinitionResponse` returns via `consume_daemon_messages` on `editor.nav_tx`.
    spawn_nav_bridge(client.clone(), editor.nav_request_rx.clone());
    // Diff pull reverse path (#338): the source-control panel's selection becomes
    // a `RequestDiff`. The `FileDiff` reply returns via `consume_daemon_messages`
    // on `editor.diff_tx`.
    spawn_request_diff_bridge(client.clone(), editor.request_diff_rx.clone());

    // Forward path: fold the daemon stream into the render channels (pane output,
    // layout snapshots, capture replies), the file tree, and the editor. Blocks
    // until the stream ends.
    let sinks = TerminalSinks {
        pane_output_tx: ch.pane_output_tx,
        snapshot_tx: ch.snapshot_tx,
        capture_result_tx: ch.capture_result_tx,
        key_table_result_tx: ch.key_table_result_tx,
    };
    consume_daemon_messages(client, Some(sinks), editor).await;
    Ok(())
}

/// The legacy terminal path: open a `tmux -CC` control-mode session over an SSH
/// PTY and stream it through termy's [`TmuxClient`]. Identical to the pre-#205
/// behavior; retained as the env-selected fallback until the milestone QA gate.
async fn run_legacy_terminal(
    mut conn: rift_ssh::SshConnection,
    session: String,
    ch: PtyChannels,
) -> Result<()> {
    use termy_terminal_ui::{TmuxClient, TmuxNotification, TmuxSocketTarget};

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

/// Best-effort daemon provisioning, run before the terminal is opened.
///
/// Reads the locally-built musl binary from `RIFT_DAEMON_BINARY` (or the
/// `just promote` compile-time bake `RIFT_DEFAULT_DAEMON_BINARY`; remote target
/// dir `RIFT_DAEMON_REMOTE_DIR`, default `$HOME/.rift/bin`), deploys the
/// versioned binary via [`rift_ssh::ensure_daemon_deployed`]. When that
/// re-uploaded a changed binary, [`rift_ssh::stop_daemon`] stops the running
/// daemon via its pidfile so the redeploy actually takes effect (#283) — an
/// unchanged deploy skips this and never bounces a healthy daemon. Then
/// attaches to the remote daemon — spawning it detached if none is running —
/// via [`rift_ssh::connect_or_spawn_daemon`] and confirms the transport with a
/// `Hello`/`Welcome` handshake. The detached daemon outlives the SSH connection,
/// so a later reconnect reattaches to it instead of spawning a second one (#62).
///
/// Returns the live [`rift_ssh::DaemonClient`] on a clean handshake; the caller
/// decides how to drive it (the terminal byte stream in daemon mode, or just the
/// worktree/git/diagnostics consumer in legacy mode). Every step is best-effort:
/// an unconfigured binary or any error logs and returns `None`, so the legacy
/// tmux flow keeps working without the daemon. The socket and log sit beside the
/// versioned binary (`<binary>.sock` / `<binary>.log`), inheriting its path.
async fn provision_daemon(conn: &mut rift_ssh::SshConnection) -> Option<rift_ssh::DaemonClient> {
    // RIFT_DAEMON_BINARY (runtime) wins over the `just promote` compile-time bake
    // RIFT_DEFAULT_DAEMON_BINARY (mirroring the RIFT_SSH_KEY / RIFT_DEFAULT_SSH_KEY
    // split), so a bare desktop-shortcut launch of the pinned stable exe resolves a
    // working daemon without any user env. Both unset/empty skips the daemon: the
    // terminal then needs the legacy path (the daemon is load-bearing under #205).
    let binary_path = match env::var_os("RIFT_DAEMON_BINARY") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => match option_env!("RIFT_DEFAULT_DAEMON_BINARY").filter(|s| !s.is_empty()) {
            Some(baked) => PathBuf::from(baked),
            None => {
                debug!("no daemon binary configured (RIFT_DAEMON_BINARY / baked default), skipping daemon");
                return None;
            }
        },
    };

    let bytes = match tokio::fs::read(&binary_path).await {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!(%e, path = %binary_path.display(), "failed to read local daemon binary, skipping daemon");
            return None;
        }
    };

    let remote_dir =
        env::var("RIFT_DAEMON_REMOTE_DIR").unwrap_or_else(|_| "$HOME/.rift/bin".to_string());

    let outcome = match rift_ssh::ensure_daemon_deployed(
        conn,
        &bytes,
        &remote_dir,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    {
        Ok(outcome) => {
            info!(
                remote_path = outcome.remote_path,
                uploaded = outcome.uploaded,
                "daemon auto-deploy complete"
            );
            outcome
        }
        Err(e) => {
            warn!(%e, "daemon auto-deploy failed, continuing with tmux only");
            return None;
        }
    };
    let remote_path = outcome.remote_path;

    // Socket and log sit beside the versioned binary, inheriting its already
    // resolved absolute path and version (no second $HOME resolution needed).
    let socket_path = format!("{remote_path}.sock");
    let log_path = format!("{remote_path}.log");

    // The binary changed under a still-running daemon (#282's signal): stop it
    // via its pidfile (#281) so the spawn below starts the fresh binary
    // instead of `connect_or_spawn_daemon` reattaching the stale one (#283).
    // An unchanged deploy skips this so a healthy daemon is never bounced.
    // Best-effort like every other step here: a failed stop just means the
    // spawn below reattaches to the still-running old binary instead.
    if outcome.uploaded {
        if let Err(e) = rift_ssh::stop_daemon(conn, &socket_path).await {
            warn!(%e, "daemon stop failed, continuing with existing daemon");
        }
    }

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
            return None;
        }
    };

    // Confirm the reattach transport with a protocol round-trip. The daemon
    // re-broadcasts the full worktree snapshot on every Hello, so the stream
    // following the Welcome starts with the complete tree.
    let client = rift_ssh::DaemonClient::new(channel);
    if let Err(e) = client
        .send(rift_protocol::ClientMessage::Hello {
            version: rift_protocol::PROTOCOL_VERSION,
        })
        .await
    {
        warn!(%e, "daemon handshake send failed");
        return None;
    }
    match client.recv().await {
        Some(rift_protocol::DaemonMessage::Welcome { version }) => {
            info!(version, "daemon transport ready (Hello/Welcome ok)");
            Some(client)
        }
        Some(other) => {
            warn!(?other, "unexpected daemon handshake reply");
            None
        }
        None => {
            warn!("daemon closed before Welcome");
            None
        }
    }
}

/// Drive the daemon message stream into the render channels, the file tree, and
/// the editor.
///
/// The single reader of the shared [`rift_ssh::DaemonClient`]: it forwards every
/// worktree-structure message (snapshot / update / git / repo / diagnostics) to
/// the file tree's model on `editor.worktree_tx`, the buffer-channel replies
/// (`FileContent` / `SaveResult` / `SaveConflict`) to the editor on
/// `editor.buffer_tx`, and — when `terminal` is `Some` —
/// the per-pane byte stream and layout snapshots into the terminal render
/// channels (#205). With `terminal` `None` (legacy mode) the terminal arms are
/// inert — the app never sent `Attach`, so the daemon streams no terminal events,
/// but the worktree + buffer channel still flow. Ends when the channel closes or
/// a `TerminalExit` ends the active attach; the detached daemon keeps running for
/// the next attach. The structure/buffer sends are best-effort: a closed
/// GPUI-side receiver (window gone) drops the message rather than fail the loop.
async fn consume_daemon_messages(
    client: std::sync::Arc<rift_ssh::DaemonClient>,
    terminal: Option<TerminalSinks>,
    editor: EditorChannels,
) {
    use rift_protocol::DaemonMessage;

    while let Some(msg) = client.recv().await {
        match msg {
            // --- terminal byte stream (daemon terminal mode only) ---
            DaemonMessage::PaneOutput { pane_id, bytes } => {
                if let Some(sinks) = &terminal {
                    // Pane ids cross the render seam in tmux's native `%N` form,
                    // matching the synthesized snapshot below and the command
                    // targets the session view builds.
                    let _ = sinks.pane_output_tx.send(PaneOutput {
                        pane_id: format!("%{pane_id}"),
                        bytes,
                    });
                }
            }
            // The reply to a capture request: route the captured scrollback back
            // to the originating pane (empty bytes on a capture error clear its
            // in-flight flag without wedging the scroll).
            DaemonMessage::PaneCapture { pane_id, bytes } => {
                if let Some(sinks) = &terminal {
                    let _ = sinks.capture_result_tx.send(CaptureResult {
                        pane_id: format!("%{pane_id}"),
                        bytes,
                    });
                }
            }
            // The reply to a key-table refresh (the daemon's own unprompted
            // attach-time query, or one this client requested): route the raw
            // `list-keys`/`show-options` text to `SessionView` to re-parse.
            DaemonMessage::KeyTableReply { list_keys, options } => {
                if let Some(sinks) = &terminal {
                    let _ = sinks
                        .key_table_result_tx
                        .send(KeyTableQueryResult { list_keys, options });
                }
            }
            // Snapshot and update both carry the full latest layout (replace
            // semantics), which is exactly what the render layer's `apply_snapshot`
            // expects — so both fold into one synthesized `TmuxSnapshot`.
            DaemonMessage::LayoutSnapshot { session, windows }
            | DaemonMessage::LayoutUpdate { session, windows } => {
                if let Some(sinks) = &terminal {
                    let _ = sinks.snapshot_tx.send(layout_to_snapshot(session, windows));
                }
            }
            DaemonMessage::TerminalExit { session, reason } => {
                info!(%session, ?reason, "daemon terminal path down");
                if terminal.is_some() {
                    // The tmux attach ended; end the session so the app surfaces
                    // it the same way the legacy `tmux` exit does (Disconnected →
                    // quit). Reconnect is #206.
                    break;
                }
            }
            // --- worktree structure -> file tree (every mode) ---
            // The structure-path messages fold into the file tree's model on the
            // GPUI side; forward each unchanged. A send failure means the window
            // closed — drop it, the recv loop ends on the next channel close.
            msg @ (DaemonMessage::WorktreeSnapshot { .. }
            | DaemonMessage::UpdateWorktree { .. }
            | DaemonMessage::UpdateGitStatus { .. }
            | DaemonMessage::RepoState { .. }
            | DaemonMessage::Diagnostics { .. }) => {
                let _ = editor.worktree_tx.send(msg);
            }
            // --- buffer channel replies -> editor (every mode) ---
            // The request/response replies on the buffer channel: the `OpenFile`
            // read reply (the only message carrying file content) and the
            // `SaveFile` write replies (`SaveResult` / `SaveConflict`). Forward
            // each to the editor, which routes it by path against the open buffer.
            msg @ (DaemonMessage::FileContent { .. }
            | DaemonMessage::SaveResult { .. }
            | DaemonMessage::SaveConflict { .. }) => {
                let _ = editor.buffer_tx.send(msg);
            }
            // --- nav replies -> editor (every mode) ---
            // Definition, hover, and references responses route to the editor's
            // nav reply channel; the workspace's `nav_rx` loop dispatches each
            // to the correct `apply_*` method on the GPUI side (#196, #197, #198).
            msg @ (DaemonMessage::DefinitionResponse { .. }
            | DaemonMessage::HoverResponse { .. }
            | DaemonMessage::ReferencesResponse { .. }) => {
                let _ = editor.nav_tx.send(msg);
            }
            // --- mirrored status line -> workspace render model (every mode) ---
            // Sent unprompted on attach, again on the daemon's own
            // `status-interval` cadence, and after a mirrored-option-mutating
            // dispatch (#219); forwarded regardless of whether the app's
            // `RIFT_STATUSLINE_MIRROR` toggle is on — the render layer decides
            // whether to use it (#221).
            msg @ DaemonMessage::StatusLineReply { .. } => {
                let _ = editor.status_line_tx.send(msg);
            }
            // --- diff reply -> diff view (every mode) ---
            // The reply to a `RequestDiff`: forward to the diff view, which
            // routes it by path against the currently open selection (#338).
            msg @ DaemonMessage::FileDiff { .. } => {
                let _ = editor.diff_tx.send(msg);
            }
            other => debug!(?other, "daemon message without a consumer yet"),
        }
    }
    info!("daemon message stream ended");
}

/// Forward the editor's file-open requests onto the protocol as
/// [`rift_protocol::ClientMessage::OpenFile`] reads (#187). Each path the file
/// tree emitted becomes one read request; the daemon answers with a
/// `FileContent` reply that returns through [`consume_daemon_messages`] on
/// `editor.buffer_tx`. A *refused* request (binary / path escape) draws no
/// reply by protocol, so the editor's own timeout recovers it. Ends when either
/// channel closes.
fn spawn_open_file_bridge(
    client: std::sync::Arc<rift_ssh::DaemonClient>,
    open_file_rx: flume::Receiver<String>,
) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while let Ok(path) = open_file_rx.recv_async().await {
            debug!(%path, "sending open-file request");
            if client.send(ClientMessage::OpenFile { path }).await.is_err() {
                break;
            }
        }
    });
}

/// Forward the source-control panel's selections onto the protocol as
/// [`rift_protocol::ClientMessage::RequestDiff`] pulls (#338). Each selected
/// path becomes one diff request; the daemon answers with a `FileDiff` reply
/// that returns through [`consume_daemon_messages`] on `editor.diff_tx`. Ends
/// when either channel closes.
fn spawn_request_diff_bridge(
    client: std::sync::Arc<rift_ssh::DaemonClient>,
    request_diff_rx: flume::Receiver<String>,
) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while let Ok(path) = request_diff_rx.recv_async().await {
            debug!(%path, "sending diff request");
            if client
                .send(ClientMessage::RequestDiff { path })
                .await
                .is_err()
            {
                break;
            }
        }
    });
}

/// Forward the editor's save requests onto the protocol as
/// [`rift_protocol::ClientMessage::SaveFile`] writes (#188). Each is the whole
/// open buffer plus its base `mtime`; the daemon answers with a `SaveResult` or a
/// `SaveConflict` that returns through [`consume_daemon_messages`] on
/// `editor.buffer_tx`. A *refused* write (a path escape, non-UTF-8) draws no reply
/// by protocol, so the editor's own save timeout recovers it. Ends when either
/// channel closes. The editor builds the full `SaveFile` (path, content,
/// base_mtime), so the bridge forwards it unchanged.
fn spawn_save_file_bridge(
    client: std::sync::Arc<rift_ssh::DaemonClient>,
    save_file_rx: flume::Receiver<rift_protocol::ClientMessage>,
) {
    tokio::spawn(async move {
        while let Ok(msg) = save_file_rx.recv_async().await {
            if let rift_protocol::ClientMessage::SaveFile { path, .. } = &msg {
                debug!(%path, "sending save-file request");
            }
            if client.send(msg).await.is_err() {
                break;
            }
        }
    });
}

/// Forward the editor's live-buffer feed onto the protocol (#189): each
/// `BufferChanged` (debounced edit) or `BufferClosed` (close / switch / save) is
/// sent verbatim so the daemon feeds the LSP the live buffer (the disk→buffer
/// source-of-truth shift). Push-only — there is no reply; diagnostics return on
/// the worktree stream as `Diagnostics`. Ends when either channel closes.
fn spawn_buffer_change_bridge(
    client: std::sync::Arc<rift_ssh::DaemonClient>,
    buffer_change_rx: flume::Receiver<rift_protocol::ClientMessage>,
) {
    tokio::spawn(async move {
        while let Ok(msg) = buffer_change_rx.recv_async().await {
            match &msg {
                rift_protocol::ClientMessage::BufferChanged { path, .. } => {
                    debug!(%path, "sending live-buffer change")
                }
                rift_protocol::ClientMessage::BufferClosed { path } => {
                    debug!(%path, "sending live-buffer close")
                }
                _ => {}
            }
            if client.send(msg).await.is_err() {
                break;
            }
        }
    });
}

/// Forward the editor's navigation requests onto the protocol (#196, #197, #198):
/// `DefinitionRequest` (ctrl+click / context-menu / F12), `HoverRequest`
/// (Shift+K / context-menu "Show Hover" / mouse-rest debounce), and
/// `ReferencesRequest` (Shift+F12 / context-menu "Find References") are sent
/// verbatim; the daemon answers with `DefinitionResponse` / `HoverResponse` /
/// `ReferencesResponse` that return through [`consume_daemon_messages`] on
/// `editor.nav_tx`. Ends when either channel closes.
fn spawn_nav_bridge(
    client: std::sync::Arc<rift_ssh::DaemonClient>,
    nav_request_rx: flume::Receiver<rift_protocol::ClientMessage>,
) {
    tokio::spawn(async move {
        while let Ok(msg) = nav_request_rx.recv_async().await {
            match &msg {
                rift_protocol::ClientMessage::DefinitionRequest { id, path, .. } => {
                    debug!(?id, %path, "sending definition request");
                }
                rift_protocol::ClientMessage::HoverRequest { id, path, .. } => {
                    debug!(?id, %path, "sending hover request");
                }
                rift_protocol::ClientMessage::ReferencesRequest { id, path, .. } => {
                    debug!(?id, %path, "sending references request");
                }
                _ => {}
            }
            if client.send(msg).await.is_err() {
                break;
            }
        }
    });
}

/// The render-side sinks the daemon terminal stream feeds: per-pane output and
/// full-layout snapshots. Held by [`consume_daemon_messages`] in daemon mode; the
/// reverse path (input, resize, commands, capture) runs through the bridge tasks.
struct TerminalSinks {
    pane_output_tx: flume::Sender<PaneOutput>,
    snapshot_tx: flume::Sender<termy_terminal_ui::TmuxSnapshot>,
    capture_result_tx: flume::Sender<CaptureResult>,
    key_table_result_tx: flume::Sender<KeyTableQueryResult>,
}

/// Forward typed input from the render layer onto the protocol as
/// [`rift_protocol::ClientMessage::Input`]; the daemon replays it to the pane via
/// `send-keys -H` (opaque bytes, agent-agnostic). Ends when either channel closes.
fn spawn_input_bridge(
    client: std::sync::Arc<rift_ssh::DaemonClient>,
    input_rx: flume::Receiver<PaneInput>,
) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while let Ok(input) = input_rx.recv_async().await {
            let Some(pane_id) = parse_pane_id(&input.pane_id) else {
                continue;
            };
            let msg = ClientMessage::Input {
                pane_id,
                data: bytes_to_string(input.bytes),
            };
            if client.send(msg).await.is_err() {
                break;
            }
        }
    });
}

/// Forward client viewport resizes onto the protocol as
/// [`rift_protocol::ClientMessage::ResizePane`]; the daemon applies them with
/// `refresh-client -C <cols>x<rows>` (the control client's single viewport, so
/// `pane_id` is unused there — any value carries).
fn spawn_resize_bridge(
    client: std::sync::Arc<rift_ssh::DaemonClient>,
    size_rx: flume::Receiver<TermSize>,
) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while let Ok(size) = size_rx.recv_async().await {
            let msg = ClientMessage::ResizePane {
                pane_id: 0,
                cols: size.cols as u16,
                rows: size.rows as u16,
            };
            if client.send(msg).await.is_err() {
                break;
            }
        }
    });
}

/// Forward raw tmux commands (the session view's window/pane affordances) onto
/// the protocol as [`rift_protocol::ClientMessage::TmuxCommand`]; the daemon runs
/// them verbatim. A command that could mutate the mirrored key table or the
/// prefix/repeat options (`keytable::mutates_bindings`), or a mirrored
/// status-line option (`statusline::mutates_status_options`), is followed, on
/// this same task, by the matching refresh request(s) — sequential `send`s on
/// one task land in program order on the shared write queue, so each refresh
/// is guaranteed to reach the daemon after the mutation it is refreshing for.
/// Issuing a refresh from a separate channel/task (as the render layer used
/// to) gave no such ordering guarantee.
fn spawn_command_bridge(
    client: std::sync::Arc<rift_ssh::DaemonClient>,
    cmd_rx: flume::Receiver<String>,
) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while let Ok(cmd) = cmd_rx.recv_async().await {
            debug!(cmd = %cmd, "sending tmux command (daemon)");
            let refresh_key_table = rift_terminal::keytable::mutates_bindings(&cmd);
            let refresh_status_line = rift_terminal::statusline::mutates_status_options(&cmd);
            if client
                .send(ClientMessage::TmuxCommand { cmd })
                .await
                .is_err()
            {
                break;
            }
            if refresh_key_table && client.send(ClientMessage::QueryKeyTable).await.is_err() {
                break;
            }
            if refresh_status_line && client.send(ClientMessage::QueryStatusLine).await.is_err() {
                break;
            }
        }
    });
}

/// Forward pre-attach scrollback (`capture-pane`) requests onto the protocol as
/// [`rift_protocol::ClientMessage::CapturePane`]; the daemon issues `capture-pane
/// -p -e` and replies with a `PaneCapture` that the consumer routes back to the
/// originating pane as a [`CaptureResult`]. The render-side `start_row`/`end_row`
/// tmux line addresses and the `-J` flag cross the seam unchanged.
fn spawn_capture_bridge(
    client: std::sync::Arc<rift_ssh::DaemonClient>,
    capture_rx: flume::Receiver<CaptureRequest>,
) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while let Ok(req) = capture_rx.recv_async().await {
            let Some(pane_id) = parse_pane_id(&req.pane_id) else {
                continue;
            };
            let msg = ClientMessage::CapturePane {
                pane_id,
                start: req.start_row,
                end: req.end_row,
                join: req.join_wraps,
            };
            if client.send(msg).await.is_err() {
                break;
            }
        }
    });
}

/// Forward key-table refresh requests (tmux key-table mirroring, #212) onto
/// the protocol as [`rift_protocol::ClientMessage::QueryKeyTable`]; the daemon
/// answers with a `KeyTableReply` that returns through
/// [`consume_daemon_messages`] on `sinks.key_table_result_tx`. The daemon also
/// issues this query unprompted on attach, so this bridge only carries the
/// statusbar's explicit-refresh trigger — a binding-mutating dispatch's
/// refresh is issued inline by `spawn_command_bridge` instead, so it lands
/// strictly after the mutating command on the same task.
fn spawn_key_table_bridge(
    client: std::sync::Arc<rift_ssh::DaemonClient>,
    key_table_request_rx: flume::Receiver<()>,
) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while key_table_request_rx.recv_async().await.is_ok() {
            if client.send(ClientMessage::QueryKeyTable).await.is_err() {
                break;
            }
        }
    });
}

/// Parse tmux's `%N` pane id into the protocol's integer pane id. A render-side
/// id that does not match the synthesized `%N` form is dropped by the caller.
fn parse_pane_id(id: &str) -> Option<u32> {
    id.strip_prefix('%')?.parse().ok()
}

/// Render keyboard/paste input bytes as the protocol's `String` payload. Terminal
/// input is UTF-8 (typed text) or ASCII (control sequences from the keystroke
/// encoder), so this is lossless in practice; a malformed run degrades to a lossy
/// decode rather than dropping the keystroke.
fn bytes_to_string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Build the render layer's [`termy_terminal_ui::TmuxSnapshot`] from the daemon's
/// protocol layout. The render `apply_snapshot` replaces its whole model from
/// this, so a `LayoutSnapshot` and a `LayoutUpdate` map identically. Window and
/// pane ids take tmux's native `@N` / `%N` form, matching the command targets the
/// session view embeds and the `%N` pane ids on `PaneOutput`. Per-pane
/// CWD/command are subscription-driven on the legacy path and absent from the
/// daemon layout query, so they start empty here.
fn layout_to_snapshot(
    session: String,
    windows: Vec<rift_protocol::WindowLayout>,
) -> termy_terminal_ui::TmuxSnapshot {
    use termy_terminal_ui::{TmuxPaneState, TmuxSnapshot, TmuxWindowState};

    let windows = windows
        .into_iter()
        .enumerate()
        .map(|(index, window)| {
            let window_id = format!("@{}", window.window_id);
            let active_pane_id = window
                .panes
                .iter()
                .find(|p| p.active)
                .map(|p| format!("%{}", p.pane_id));
            let panes = window
                .panes
                .into_iter()
                .map(|pane| TmuxPaneState {
                    id: format!("%{}", pane.pane_id),
                    window_id: window_id.clone(),
                    session_id: String::new(),
                    is_active: pane.active,
                    left: pane.left,
                    top: pane.top,
                    width: pane.width,
                    height: pane.height,
                    cursor_x: 0,
                    cursor_y: 0,
                    current_path: String::new(),
                    current_command: String::new(),
                })
                .collect();
            TmuxWindowState {
                id: window_id,
                // The daemon layout query carries no tmux window index; the layout
                // order is a stable, monotonic stand-in for the tab number (display
                // only — window selection targets the `@N` id, not this).
                index: index as i32,
                name: window.name,
                layout: String::new(),
                is_active: window.active,
                automatic_rename: false,
                active_pane_id,
                panes,
            }
        })
        .collect();

    TmuxSnapshot {
        session_name: session,
        session_id: None,
        windows,
    }
}
