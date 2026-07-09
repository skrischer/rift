// Console-free stable launcher: GUI subsystem instead of console, so a desktop
// shortcut launch opens no console window. Gated by the `windowed` feature (not
// `not(debug_assertions)` — the `stable` profile keeps debug-assertions on for the
// GPUI runtime-shader path); off by default so dev keeps its RUST_LOG console.
#![cfg_attr(feature = "windowed", windows_subsystem = "windows")]

use std::borrow::Cow;
use std::env;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context as _, Result};
use gpui::*;
use gpui_component::{Root, TitleBar};
use gpui_component_assets::Assets;
use rift_app::connection_screen::{
    ConnectError, ConnectRequest, ConnectionScreen, ConnectionScreenEvent,
};
use rift_app::recents::RecentConnection;
use rift_app::{apply_persisted_theme, recents, window_state, workspace};
use rift_logging::{
    LogTarget, RotatingMakeWriter, SizedWriter, DEFAULT_MAX_BYTES, FORCE_CONSOLE_ENV,
};
use rift_terminal::{
    CaptureRequest, CaptureResult, ConnectionStatus, KeyTableQueryResult, PaneInput, PaneOutput,
    SelectWindow, SessionListItem, SessionSnapshot, SessionSwitchRequest, SessionView,
    SubscriptionUpdate, TermSize, TERMINAL_KEY_CONTEXT,
};
use tracing::{debug, error, info, warn};

struct SshConfig {
    host: String,
    port: u16,
    user: String,
    key: PathBuf,
    /// The Connection screen's passphrase field value for an encrypted key
    /// (#478, `docs/spec-connection-robustness.md`) — `None` for a plain key.
    /// Deliberately not `Debug`-derived on this struct: never format this
    /// field into a log line (constitution: no secrets in logs).
    passphrase: Option<String>,
}

// Cloned per connect attempt by the reconnect engine: flume handles are cheap
// `Arc` clones, and the dead session's clones drop with its runtime, so only
// the live session consumes the render-side channels.
#[derive(Clone)]
struct PtyChannels {
    pane_output_tx: flume::Sender<PaneOutput>,
    input_rx: flume::Receiver<PaneInput>,
    size_changed_rx: flume::Receiver<TermSize>,
    snapshot_tx: flume::Sender<SessionSnapshot>,
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
    /// The host's session list (`docs/spec-session-switch.md`), routed to the
    /// session switcher: every `SessionListReply` (explicit refresh or the
    /// daemon's unprompted churn-driven push) replaces the whole list.
    session_list_tx: flume::Sender<Vec<SessionListItem>>,
    /// An explicit session-list refresh (opening the switcher); forwarded onto
    /// the protocol as `ClientMessage::QuerySessionList` in daemon mode.
    /// Unused in the legacy tmux path — the receiver drops there, so the
    /// session switcher is inert on `RIFT_TERMINAL_LEGACY` (documented
    /// limitation; the legacy path is slated for removal, #285).
    session_list_request_rx: flume::Receiver<()>,
    /// A cockpit switch from the session switcher; forwarded onto the protocol
    /// as `ClientMessage::Attach { session }` plus a viewport re-assert in
    /// daemon mode. Same legacy-path caveat as `session_list_request_rx`.
    session_switch_rx: flume::Receiver<SessionSwitchRequest>,
}

/// The daemon-side endpoints of the editor surface's buffer-channel and worktree
/// wiring (#187, #188). The tokio session reader (`consume_daemon_messages`)
/// forwards worktree-family messages and the buffer-channel replies
/// (`FileContent` / `SaveResult` / `SaveConflict`) onto these senders; an
/// open-file bridge drains `open_file_rx` into `ClientMessage::OpenFile` reads and
/// a save-file bridge drains `save_file_rx` into `SaveFile` writes. The matching
/// GPUI-side endpoints live on [`rift_app::workspace::WorkspaceChannels`].
/// Cloned per connect attempt by the reconnect engine (see [`PtyChannels`]).
#[derive(Clone)]
struct EditorChannels {
    /// Worktree-family daemon messages routed to the file tree's model.
    worktree_tx: flume::Sender<rift_protocol::DaemonMessage>,
    /// Buffer-channel replies routed to the editor: `FileContent` (load),
    /// `SaveResult` (save landed), `SaveConflict` (save refused).
    buffer_tx: flume::Sender<rift_protocol::DaemonMessage>,
    /// Nav replies routed to the editor: `DefinitionResponse` (#196).
    nav_tx: flume::Sender<rift_protocol::DaemonMessage>,
    /// `LspStatus` pushes routed to the workspace's composite status line
    /// language-server health segment (`docs/spec-status-line.md`).
    lsp_status_tx: flume::Sender<rift_protocol::DaemonMessage>,
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
    /// Git write ops the source-control panel emits (#546) — `StageFile`,
    /// `UnstageFile`, `DiscardFile`, `Commit`; forwarded onto the protocol
    /// verbatim by the git-op bridge. Push-only from the panel's side: the
    /// daemon's `GitOpResult` reply is not routed back, the resulting state
    /// arrives on the worktree stream as `UpdateGitStatus` / `RepoState`.
    git_op_rx: flume::Receiver<rift_protocol::ClientMessage>,
    /// File ops the file tree emits (`docs/spec-explorer-file-ops.md`, #675) —
    /// `CreateFile`, `CreateDir`, `RenamePath`, `DeletePath`; forwarded onto
    /// the protocol verbatim by the file-op bridge, exactly like `git_op_rx`.
    /// Unlike the git-write channel, the daemon's `FileOpResult` reply IS
    /// routed back (on `file_op_result_tx` below) — the tree needs it for UX
    /// transitions (close the rename editor, surface an error), never for the
    /// tree mutation itself, which stays push-only via `UpdateWorktree`.
    file_op_rx: flume::Receiver<rift_protocol::ClientMessage>,
    /// `FileOpResult` replies routed to the file tree for UX transitions only
    /// (`docs/spec-explorer-file-ops.md`): `WorktreeModel` is never mutated
    /// from a file op — the push-only `UpdateWorktree` is the single writer.
    file_op_result_tx: flume::Sender<rift_protocol::DaemonMessage>,
    /// Fires once whenever `run_ssh_session` selects the daemon terminal but
    /// `provision_daemon` came back empty (#619): no daemon binary configured,
    /// or a provisioning step failed. The session still runs — the legacy tmux
    /// path takes over — but every daemon-backed IDE feature (file explorer,
    /// diagnostics, git status, LSP) is dead for the session, so this is the
    /// only render-side signal that anything is missing. Sent once per attempt
    /// (a reconnect resends it if the daemon is still unavailable); the
    /// workspace's notification dedups by id so this never stacks duplicates.
    daemon_unavailable_tx: flume::Sender<()>,
}

/// The reconnect engine's cross-attempt state (#476): `watch` channels whose
/// values must survive the per-attempt tokio runtimes. `run_session_with_reconnect`
/// owns the struct; each attempt receives it by reference and hands clones of
/// the senders to the bridges. Without engine scope, an SSH-level reconnect
/// would silently re-attach the startup session after a cockpit switch (#509)
/// and leave the fresh tmux child on the attach-default grid.
struct EngineWatches {
    /// The session the client is currently attached to: seeded once from the
    /// Connection screen's connect request (its Session field, itself
    /// prefilled from `RIFT_SESSION` — #477), updated by
    /// `spawn_session_switch_bridge` per switch the daemon actually saw.
    session: tokio::sync::watch::Sender<String>,
    /// The render layer's last known client grid: cached by
    /// `spawn_resize_bridge` per resize (and by the engine's backlog drain),
    /// re-asserted as a `ResizePane` after every fresh attach — the render
    /// layer only re-sends on a size change, which a reconnect is not.
    viewport: tokio::sync::watch::Sender<Option<TermSize>>,
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

/// Rift's own file-type glyph SVGs (a curated MIT Seti UI subset,
/// `crates/app/assets/file_icons/`), embedded via `rust-embed`. Kept as a
/// separate embed target from `gpui_component_assets::Assets` — GPUI's
/// `AssetSource` is disjoint per path prefix, not merged automatically.
#[derive(rust_embed::RustEmbed)]
#[folder = "assets"]
#[include = "file_icons/**/*.svg"]
struct RiftFileIcons;

/// Delegating `AssetSource`: serves rift's own `file_icons/*.svg` glyphs from
/// [`RiftFileIcons`] and hands every other path straight through to
/// `gpui_component_assets::Assets` unchanged. `with_assets` accepts exactly one
/// source, so this is the single source registered for the product build;
/// gpui-component's `IconName` glyphs (activity rail, window controls) keep
/// resolving through the delegate exactly as before (#597). Routing is by the
/// `file_icons/` prefix, not trial-and-error fallthrough: gpui-component's own
/// `Assets::load` returns `Err`, not `Ok(None)`, on a miss, so a fallthrough
/// attempt would surface the wrong asset source's error on a genuine miss.
struct RiftAssets;

impl AssetSource for RiftAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.starts_with("file_icons/") {
            return Ok(RiftFileIcons::get(path).map(|file| file.data));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        if path.starts_with("file_icons/") {
            return Ok(RiftFileIcons::iter()
                .filter(|asset_path| asset_path.starts_with(path))
                .map(SharedString::from)
                .collect());
        }
        Assets.list(path)
    }
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

    Application::with_platform(gpui_platform::current_platform(false))
        // Register the delegating RiftAssets source: gpui-component's icon SVGs
        // keep resolving through it exactly as `Assets` alone did (activity
        // rail / window controls — blank otherwise, #597), and it additionally
        // serves rift's own vendored file-type glyphs (`file_icons/*.svg`,
        // #668) so the explorer's icon slot can render real glyphs in the
        // shipping binary, not only under the dev-only `gallery` feature.
        .with_assets(RiftAssets)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            // Restore the persisted theme choice (`docs/spec-theme-settings.md`); a
            // missing platform state directory or a missing/corrupt state file both
            // degrade to `WindowState::default`'s dark Catppuccin default.
            let persisted_state = match window_state::state_path() {
                Ok(path) => window_state::load(&path),
                Err(e) => {
                    warn!(%e, "no platform state directory, using default preferences");
                    window_state::WindowState::default()
                }
            };
            apply_persisted_theme(&persisted_state, cx);
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
            // Jump-to-file quick-open (`docs/spec-explorer-search.md`, Phase 31,
            // #681): Ctrl+Shift+O / Cmd+Shift+O opens it, mirroring Xcode's
            // "Open Quickly" muscle memory and the command palette's
            // Ctrl+Shift+P pattern above — a terminal-safe `Ctrl/Cmd+Shift`
            // chord, never the bare `Ctrl+P` the terminal's readline claims.
            // Unscoped (`None`), so the shortcut reaches quick-open regardless
            // of which surface is focused, including the terminal.
            cx.bind_keys([
                KeyBinding::new("ctrl-shift-o", rift_app::quick_open::OpenQuickOpen, None),
                KeyBinding::new("cmd-shift-o", rift_app::quick_open::OpenQuickOpen, None),
            ]);
            // Settings surface (#366, `docs/spec-theme-settings.md`): Ctrl+, /
            // Cmd+, opens it, mirroring the editor-convention shortcut. Unscoped
            // (`None`), like the command palette above, so it reaches the
            // shortcut regardless of which surface is focused.
            cx.bind_keys([
                KeyBinding::new("ctrl-,", rift_app::settings::OpenSettings, None),
                KeyBinding::new("cmd-,", rift_app::settings::OpenSettings, None),
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
                // Hover popover (#197, #435): Ctrl+K Ctrl+I mirrors VS Code muscle
                // memory. A plain Shift+K binding would shadow typing a capital 'K'
                // into the buffer, so hover uses a non-typing chord instead. Fires
                // `ShowHover` at the cursor position; the result renders as a
                // markdown popover anchored to the bottom of the editor area.
                // Mouse-rest (500 ms debounce) also triggers hover automatically.
                KeyBinding::new(
                    "ctrl-k ctrl-i",
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
                // Results-panel close (#529): Escape closes the references/
                // definitions results panel in the right dock. The input's own
                // `escape` binding (deeper "Input" context) is tried first and
                // propagates when it has nothing to do (no context menu, inline
                // completion, or IME composition), so the keystroke falls through
                // to this binding; `CloseResultsPanel` itself propagates when no
                // panel is open, keeping Escape's other meanings intact.
                KeyBinding::new(
                    "escape",
                    rift_app::editor::CloseResultsPanel,
                    Some(rift_app::editor::EDITOR_KEY_CONTEXT),
                ),
                // Redo, Linux muscle memory (#599): `gpui-component`'s own input
                // keymap (`crates/ui/src/input/state.rs`, non-macOS branch) already
                // binds Ctrl+Z -> Undo, Ctrl+Y -> Redo, and Ctrl+A/C/X/V ->
                // SelectAll/Copy/Cut/Paste for Windows/Linux — audited against the
                // pinned rev and confirmed present, so none of those are re-bound
                // here. The one real gap: Ctrl+Shift+Z (the GTK/GNOME-convention
                // redo chord) has no non-macOS binding, only the macOS `cmd-shift-z`
                // and the Windows-convention `ctrl-y`. Add it as a second redo
                // chord, mirroring the "bind every chord unconditionally" pattern
                // above — harmless on macOS alongside `cmd-shift-z`.
                KeyBinding::new(
                    "ctrl-shift-z",
                    gpui_component::input::Redo,
                    Some(rift_app::editor::EDITOR_KEY_CONTEXT),
                ),
                // Go to Line (`docs/spec-v1-hardening.md`, #620): Ctrl+G mirrors
                // VS Code/JetBrains muscle memory. Find/replace needs no binding
                // here — `gpui-component`'s own `Ctrl+F`/`Cmd+F` (bound in its
                // "Input" context, `crates/ui/src/input/state.rs`) already opens
                // it, since the code editor's `InputState` is built with
                // `.code_editor(...)`, which turns on `searchable` by default.
                KeyBinding::new(
                    "ctrl-g",
                    rift_app::editor::GoToLine,
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
                KeyBinding::new(
                    "f2",
                    rift_app::file_tree::StartRename,
                    Some(rift_app::file_tree::FILE_TREE_KEY_CONTEXT),
                ),
                // Discrete multi-select keyboard extension
                // (`docs/spec-explorer-search.md`, Phase 31, #680): `Shift+Up`/
                // `Shift+Down` grow the multi-select set from the cursor, the
                // keyboard counterpart of `Ctrl/Cmd+Click`/`Shift+Click`.
                KeyBinding::new(
                    "shift-up",
                    rift_app::file_tree::ExtendSelectionUp,
                    Some(rift_app::file_tree::FILE_TREE_KEY_CONTEXT),
                ),
                KeyBinding::new(
                    "shift-down",
                    rift_app::file_tree::ExtendSelectionDown,
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
            // Recent-connections store (#477, `docs/spec-connection-robustness.md`):
            // beside the window-state file, same per-channel keying, same
            // tolerant-degrade-on-error contract.
            let recents_path = match recents::recents_path() {
                Ok(path) => Some(path),
                Err(e) => {
                    warn!(%e, "recent-connections persistence disabled");
                    None
                }
            };
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
            let font_size_px = restored.font_size_px;
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(window_bounds),
                    // Custom 38px title bar (#511, `docs/spec-cockpit-chrome.md`):
                    // `TitleBar::title_bar_options()` hides the native OS chrome
                    // (`appears_transparent: true`) and leaves `title: None` — the
                    // window's taskbar/alt-tab name is set explicitly below
                    // instead, since the OS title text no longer renders anywhere.
                    titlebar: Some(TitleBar::title_bar_options()),
                    // Client-side decorations (Wayland only; a no-op under X11 and
                    // Windows, where `appears_transparent` above already governs
                    // the chrome) — mirrors gpui-component's own story reference
                    // so a compositor never draws a redundant server-side bar
                    // behind the custom one.
                    #[cfg(target_os = "linux")]
                    window_background: WindowBackgroundAppearance::Transparent,
                    #[cfg(target_os = "linux")]
                    window_decorations: Some(WindowDecorations::Client),
                    ..Default::default()
                },
                |window, cx| {
                    // The OS-level window title (taskbar/alt-tab) — no longer
                    // carried by `titlebar.title` now that the native bar is
                    // hidden; the custom bar shows the wordmark instead.
                    window.set_window_title(title);
                    // The Connection screen (#477) is the startup state on every
                    // launch (no auto-connect, per the spec's gate decision): the
                    // Shell renders it first, and only builds the session
                    // pipeline below once the user submits the connect card.
                    let shell =
                        cx.new(|cx| Shell::new(state_path, recents_path, font_size_px, window, cx));
                    cx.new(|cx| Root::new(shell, window, cx))
                },
            )
            .unwrap();
            cx.activate(true);
        });
}

/// The window's root content (#477, `docs/spec-connection-robustness.md`):
/// the Connection screen until a connect attempt reaches the cockpit, then
/// the [`workspace::WorkspaceView`] — and back to a fresh Connection screen
/// (carrying the failure reason, if any) once that session ends, whether
/// from an orderly exit, a canceled reconnect, or a non-retryable connect
/// failure. Auto-connect-on-launch is deliberately not implemented (spec
/// gate decision): every launch starts on the screen, prefilled, one
/// explicit Connect (or Enter) away from the cockpit.
enum ScreenState {
    Connection(Entity<ConnectionScreen>),
    Workspace(Entity<workspace::WorkspaceView>),
}

struct Shell {
    screen: ScreenState,
    state_path: Option<PathBuf>,
    recents_path: Option<PathBuf>,
    font_size_px: f32,
}

impl Shell {
    fn new(
        state_path: Option<PathBuf>,
        recents_path: Option<PathBuf>,
        font_size_px: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let screen = Self::build_connection_screen(recents_path.as_deref(), None, window, cx);
        Self::watch_connection_screen(&screen, window, cx);
        Self::focus_connection_screen(&screen, window, cx);
        Self {
            screen: ScreenState::Connection(screen),
            state_path,
            recents_path,
            font_size_px,
        }
    }

    /// Build a fresh Connection screen prefilled from the live environment
    /// (`connection_screen::live_defaults`) and the on-disk RECENT list,
    /// optionally carrying `error` — a previous connect attempt's failure,
    /// surfaced on the card itself (field/banner) rather than only logged.
    fn build_connection_screen(
        recents_path: Option<&Path>,
        error: Option<ConnectError>,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> Entity<ConnectionScreen> {
        let defaults = rift_app::connection_screen::live_defaults();
        let recents = recents_path.map(recents::load).unwrap_or_default();
        cx.new(|cx| ConnectionScreen::new(&defaults, recents, error, window, cx))
    }

    /// Subscribe to the screen's `Connect` event so a submitted card drives
    /// [`Shell::connect`].
    fn watch_connection_screen(
        screen: &Entity<ConnectionScreen>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(
            screen,
            window,
            |this, _screen, event: &ConnectionScreenEvent, window, cx| {
                let ConnectionScreenEvent::Connect(request) = event;
                this.connect(request.clone(), window, cx);
            },
        )
        .detach();
    }

    /// Move keyboard focus onto the screen's Host field, mirroring the
    /// pre-#477 startup path's deferred focus of the workspace.
    fn focus_connection_screen(
        screen: &Entity<ConnectionScreen>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus_handle = screen.focus_handle(cx);
        window.defer(cx, move |window, cx| {
            focus_handle.focus(window, cx);
        });
    }

    /// Drive a submitted connect attempt: record it in the RECENT store, then
    /// build the full session pipeline (channels, `SessionView`,
    /// `WorkspaceView`, the SSH thread) exactly like the pre-#477 startup path
    /// did unconditionally at launch — now gated behind an explicit Connect
    /// instead. A watcher task routes back to a fresh Connection screen once
    /// the session ends ([`Shell::return_to_connection_screen`]).
    fn connect(&mut self, request: ConnectRequest, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = &self.recents_path {
            let now = recents::now_unix_secs();
            let entry = RecentConnection {
                host: request.host.clone(),
                user: request.user.clone(),
                port: request.port,
                key: request.key.display().to_string(),
                session: request.session.clone(),
                last_connected_unix_secs: now,
            };
            if let Err(e) = recents::record(path, entry, now) {
                warn!(%e, "failed to record recent connection");
            }
        }

        let ssh = SshConfig {
            host: request.host,
            user: request.user,
            port: request.port,
            key: request.key,
            passphrase: request.passphrase,
        };
        let session_name = request.session;

        // Editor surface (#187) wiring: the daemon stream reader forwards
        // worktree structure and `FileContent` replies onto these, and the
        // tree's open requests come back on `open_file`. The daemon-side
        // ends thread into the SSH session below; the GPUI-side ends into
        // the `WorkspaceView`.
        let (worktree_tx, worktree_rx) = flume::unbounded();
        let (buffer_tx, buffer_rx) = flume::unbounded();
        let (nav_daemon_tx, nav_rx) = flume::unbounded::<rift_protocol::DaemonMessage>();
        let (lsp_status_tx, lsp_status_rx) = flume::unbounded::<rift_protocol::DaemonMessage>();
        let (open_file_tx, open_file_rx) = flume::unbounded::<String>();
        let (save_file_tx, save_file_rx) = flume::unbounded::<rift_protocol::ClientMessage>();
        let (buffer_change_tx, buffer_change_rx) =
            flume::unbounded::<rift_protocol::ClientMessage>();
        let (nav_request_tx, nav_request_rx) = flume::unbounded::<rift_protocol::ClientMessage>();
        let (diff_tx, diff_rx) = flume::unbounded::<rift_protocol::DaemonMessage>();
        let (request_diff_tx, request_diff_rx) = flume::unbounded::<String>();
        let (git_op_tx, git_op_rx) = flume::unbounded::<rift_protocol::ClientMessage>();
        let (file_op_tx, file_op_rx) = flume::unbounded::<rift_protocol::ClientMessage>();
        let (file_op_result_tx, file_op_result_rx) =
            flume::unbounded::<rift_protocol::DaemonMessage>();
        let (daemon_unavailable_tx, daemon_unavailable_rx) = flume::unbounded::<()>();
        // Fires once when the session pipeline this attempt spawned ends —
        // orderly exit, a canceled reconnect, or a non-retryable failure
        // (`run_session_with_reconnect`'s `end_reason`) — so the Shell can
        // route back to the Connection screen instead of leaving a dead
        // cockpit up.
        let (session_ended_tx, session_ended_rx) = flume::unbounded::<Option<ConnectError>>();

        let font_size_px = self.font_size_px;
        let state_path = self.state_path.clone();
        let session_view = cx.new(|cx| {
            let (mut view, handle) = SessionView::new(cx);
            // Font-size restore (#225): set before the first tmux snapshot
            // creates any panes, so every pane picks up the restored size
            // from the start rather than flashing the default and then
            // resizing. `apply_font_size` no-ops safely against the
            // still-empty panes map at this point.
            view.set_font_size(px(font_size_px), cx);

            // Feed the statusbar label from this same resolved config rather
            // than a second, independent env resolution — the two previously
            // had divergent defaults (#494).
            view.set_ssh_label(format!("{}@{}", ssh.user, ssh.host));

            // Kept outside `channels` so the reconnect engine can drive the
            // indicator (SshReconnecting/Disconnected) after
            // `run_ssh_session` returns (the in-session clone reports
            // Connected). The cancel receiver likewise belongs to the
            // engine, not to a single session run.
            let status_tx = handle.connection_status_tx.clone();
            let reconnect_cancel_rx = handle.reconnect_cancel_rx.clone();

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
                session_list_tx: handle.session_list_tx,
                session_list_request_rx: handle.session_list_request_rx,
                session_switch_rx: handle.session_switch_rx,
            };

            let editor_channels = EditorChannels {
                worktree_tx,
                buffer_tx,
                nav_tx: nav_daemon_tx,
                lsp_status_tx,
                open_file_rx,
                save_file_rx,
                buffer_change_rx,
                nav_request_rx,
                diff_tx,
                request_diff_rx,
                git_op_rx,
                file_op_rx,
                file_op_result_tx,
                daemon_unavailable_tx,
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
                run_session_with_reconnect(
                    &ssh,
                    SessionRunParams {
                        channels,
                        editor: editor_channels,
                        status_tx,
                        cancel_rx: reconnect_cancel_rx,
                        key_exists,
                        session: session_name,
                        session_ended_tx,
                    },
                );
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
                    lsp_status_rx,
                    diff_rx,
                    open_file_tx,
                    save_file_tx,
                    buffer_change_tx,
                    nav_tx: nav_request_tx,
                    request_diff_tx,
                    git_op_tx,
                    file_op_tx,
                    file_op_result_rx,
                    daemon_unavailable_rx,
                },
                state_path,
                window,
                cx,
            )
        });

        let focus_handle = workspace.focus_handle(cx);
        window.defer(cx, move |window, cx| {
            focus_handle.focus(window, cx);
        });

        // Route back to a fresh Connection screen once this session ends —
        // the SSH thread has already returned by the time this fires, so
        // nothing beyond dropping the dead workspace/session entities is
        // needed.
        cx.spawn_in(window, async move |this, cx| {
            let Ok(reason) = session_ended_rx.recv_async().await else {
                return;
            };
            let _ = this.update_in(cx, |shell, window, cx| {
                shell.return_to_connection_screen(reason, window, cx);
            });
        })
        .detach();

        self.screen = ScreenState::Workspace(workspace);
        cx.notify();
    }

    /// The session pipeline a connect attempt spawned has ended: build a
    /// fresh Connection screen (carrying `reason` when the end was a
    /// non-retryable failure) and swap it in as the window's content.
    fn return_to_connection_screen(
        &mut self,
        reason: Option<ConnectError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let screen =
            Self::build_connection_screen(self.recents_path.as_deref(), reason, window, cx);
        Self::watch_connection_screen(&screen, window, cx);
        Self::focus_connection_screen(&screen, window, cx);
        self.screen = ScreenState::Connection(screen);
        cx.notify();
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        match &self.screen {
            ScreenState::Connection(screen) => screen.clone().into_any_element(),
            ScreenState::Workspace(workspace) => workspace.clone().into_any_element(),
        }
    }
}

/// The SSH-level reconnect engine (#476, `docs/spec-connection-robustness.md`):
/// run the connect pipeline, and when the session dies with a transport-shaped
/// error, retry it forever under [`rift_ssh::ReconnectBackoff`]'s jittered
/// capped schedule (30s cap) — an SSH drop never quits the app. Each retry
/// surfaces as `ConnectionStatus::SshReconnecting { retry }` (danger banner +
/// warning dot); a successful reconnect re-runs the full pipeline (daemon
/// provisioning, attach, resync) and restarts the schedule. The banner's
/// Cancel and non-retryable failures ([`is_retryable_session_error`]) end the
/// loop in the visible `Disconnected` state instead — the Connection screen
/// (#477) owns that state, routed back to by [`Shell::return_to_connection_screen`].
/// An orderly end (`Ok`: the tmux attach exited) also ends the loop without
/// retrying: the remote session is gone on purpose, not lost.
///
/// Each attempt runs on a fresh tokio runtime: dropping the runtime cancels
/// every bridge task the dead session spawned, so a stale bridge can never
/// compete with the fresh session's bridges for the render-side flume
/// receivers (clones of one MPMC channel are competing consumers). The flip
/// side is that nothing consumes those receivers while no session is live, so
/// the engine drops the queued backlog ([`drain_render_backlog`]) before every
/// attempt — replaying an outage's keystrokes into a fresh attach is exactly
/// the buffering the spec forbids.
///
/// Two `watch` channels live at engine scope because their values must survive
/// the per-attempt runtimes: the session the client is currently on (seeded
/// once from the connect request, updated by the session-switch bridge — a
/// reconnect after a cockpit switch, #509, must re-attach the session the user
/// is actually on, the same rule the daemon recovery follows) and the render
/// layer's last known client grid (re-asserted after every fresh attach — the
/// render layer only re-sends on a size change, which a reconnect is not).
///
/// Cancel is observed between attempts (during the backoff wait): a Cancel
/// clicked while a connect attempt is in-flight takes effect only once that
/// attempt resolves — immediately if it fails (the queued Cancel ends the
/// next wait), silently discarded if it succeeds (the connected-path drain
/// below; the user is back on a live session, so the stale Cancel must not
/// kill it). A TCP connect against a black-holed host can keep an attempt
/// in-flight for tens of seconds; the Connection screen (#477) is the place
/// to shorten that perceived latency, not a second cancellation seam.
struct SessionRunParams {
    channels: PtyChannels,
    editor: EditorChannels,
    status_tx: flume::Sender<ConnectionStatus>,
    cancel_rx: flume::Receiver<()>,
    key_exists: bool,
    /// The tmux session name to attach to — the connect request's Session
    /// field, itself prefilled from `RIFT_SESSION` (#477).
    session: String,
    /// Fires exactly once when this run's loop ends, so the Shell can route
    /// back to a fresh Connection screen (#477). Carries [`ConnectError::Passphrase`]
    /// instead of [`ConnectError::General`] when the failure was the SSH key
    /// needing (or rejecting) a passphrase (#478), so the screen points the
    /// error at that field rather than only the general banner.
    session_ended_tx: flume::Sender<Option<ConnectError>>,
}

fn run_session_with_reconnect(ssh: &SshConfig, params: SessionRunParams) {
    let SessionRunParams {
        channels,
        editor,
        status_tx,
        cancel_rx,
        key_exists,
        session,
        session_ended_tx,
    } = params;
    let mut retry: u32 = 0;
    let mut backoff = rift_ssh::ReconnectBackoff::new();
    let connected = AtomicBool::new(false);
    // A non-retryable failure's message, surfaced on the Connection screen
    // (#477) once this loop ends — `None` for every other end (orderly tmux
    // exit, a canceled reconnect, or the render side going away), which the
    // screen treats as "no error to show", not log-only.
    let mut end_reason: Option<ConnectError> = None;
    let watches = EngineWatches {
        session: tokio::sync::watch::channel(session).0,
        viewport: tokio::sync::watch::channel(None::<TermSize>).0,
    };
    loop {
        connected.store(false, Ordering::Relaxed);
        // Discard everything the render side queued while no session was
        // live; only the latest grid survives, folded into the viewport
        // watch. Runs immediately before the attempt, so the residual replay
        // window is the attempt's own connect/provision phase.
        drain_render_backlog(&channels, &editor, &watches);
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        let result = rt.block_on(run_ssh_session(
            ssh,
            channels.clone(),
            editor.clone(),
            &connected,
            &watches,
        ));
        // Cancels the dead session's bridge tasks (see the engine doc above).
        drop(rt);
        if connected.load(Ordering::Relaxed) {
            // The session was fully up before it died: the next outage is a
            // fresh one. Restart the retry schedule and drop a stale Cancel
            // that raced a reconnect which then succeeded.
            retry = 0;
            backoff.reset();
            while cancel_rx.try_recv().is_ok() {}
        }
        let error = match result {
            Ok(()) => {
                info!("SSH session ended (orderly tmux exit)");
                break;
            }
            Err(e) => e,
        };
        if !is_retryable_session_error(&error) {
            error!(
                %error,
                host = %ssh.host,
                port = ssh.port,
                key = %ssh.key.display(),
                key_exists,
                "SSH session failed with a non-retryable error"
            );
            end_reason = Some(classify_connect_error(ssh, &error));
            break;
        }
        retry = retry.saturating_add(1);
        warn!(%error, retry, host = %ssh.host, "SSH connection lost, reconnecting");
        let _ = status_tx.send(ConnectionStatus::SshReconnecting { retry });
        match cancel_rx.recv_timeout(backoff.next_delay()) {
            // The user canceled from the banner: stop retrying and surface
            // the visible not-connected state.
            Ok(()) => {
                info!("SSH reconnect canceled by the user");
                break;
            }
            Err(flume::RecvTimeoutError::Timeout) => {}
            // The render side is gone (window closed): nobody to reconnect
            // for.
            Err(flume::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = session_ended_tx.send(end_reason);
    let _ = status_tx.send(ConnectionStatus::Disconnected);
}

/// Whether the reconnect engine should retry after this session failure: only
/// an error chain carrying a retryable [`rift_ssh::SshError`] — a
/// transport-shaped death — re-enters the loop. Everything else is treated as
/// deterministic (auth/config failures; typeless session errors such as the
/// mid-session protocol version mismatch, whose automatic re-run would
/// re-enter the stale-daemon replacement #475 deliberately cut off) and
/// surfaces as a visible disconnect instead of hiding behind retries (spec
/// gate decision 2026-07-05).
fn is_retryable_session_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<rift_ssh::SshError>()
            .is_some_and(rift_ssh::SshError::is_retryable)
    })
}

/// Classify a non-retryable connect failure for the Connection screen (#478,
/// `docs/spec-connection-robustness.md`): a wrong or missing SSH key
/// passphrase routes to the passphrase field ([`ConnectError::Passphrase`])
/// instead of the general banner. Two chain shapes qualify:
/// [`rift_ssh::SshError::KeyEncrypted`] (the key needs a passphrase and none
/// was supplied — normally caught earlier by the screen's own probe, but
/// classified here too as the connect attempt's own ground truth) and
/// [`rift_ssh::SshError::Key`] when this attempt actually carried a
/// passphrase (almost certainly the wrong one — a `Key` failure with no
/// passphrase supplied is a different problem, e.g. a corrupt or unsupported
/// key, and stays on the general banner).
fn classify_connect_error(ssh: &SshConfig, error: &anyhow::Error) -> ConnectError {
    let is_passphrase_issue = error.chain().any(|cause| match cause.downcast_ref() {
        Some(rift_ssh::SshError::KeyEncrypted) => true,
        Some(rift_ssh::SshError::Key(_)) => ssh.passphrase.is_some(),
        _ => false,
    });
    if is_passphrase_issue {
        // `error.to_string()` would only print the `run_ssh_session` context
        // wrapper ("SSH connection failed"), leaving the passphrase field's
        // error uninformative; the root cause is the actionable message.
        ConnectError::Passphrase(error.root_cause().to_string())
    } else {
        ConnectError::General(error.to_string())
    }
}

/// Drop the render-side backlog that accumulated while no session was live
/// (spec constraint: no unbounded buffering while disconnected — drop and
/// resync on reconnect). The engine's per-attempt runtime drop cancels the
/// dead session's bridge consumers, but the render layer keeps sending on the
/// shared flume channels, so without this drain every keystroke, tmux command,
/// capture, and editor request queued during an outage would replay into the
/// fresh attach — stale input injected into live panes minutes later. The
/// dropped state is replaced by the resync: the daemon replays its snapshot,
/// tmux replays the terminal. The one latest-value signal worth keeping is the
/// client grid: drained resizes fold into the engine's viewport watch (last
/// one wins), which the fresh attach re-asserts. Called only while no bridges
/// are alive, so the engine is the sole consumer of the drained receivers.
fn drain_render_backlog(ch: &PtyChannels, editor: &EditorChannels, watches: &EngineWatches) {
    if let Some(size) = ch.size_changed_rx.drain().last() {
        watches.viewport.send_replace(Some(size));
    }
    // A switch queued mid-outage is dropped without recording it on the
    // session watch: the daemon never saw it, and the resync restores the
    // last session actually asked of the daemon (#475 drop semantics).
    let dropped = ch.input_rx.drain().count()
        + ch.tmux_command_rx.drain().count()
        + ch.capture_request_rx.drain().count()
        + ch.key_table_request_rx.drain().count()
        + ch.session_list_request_rx.drain().count()
        + ch.session_switch_rx.drain().count()
        + editor.open_file_rx.drain().count()
        + editor.save_file_rx.drain().count()
        + editor.buffer_change_rx.drain().count()
        + editor.nav_request_rx.drain().count()
        + editor.request_diff_rx.drain().count()
        + editor.git_op_rx.drain().count()
        + editor.file_op_rx.drain().count();
    if dropped > 0 {
        debug!(
            dropped,
            "dropped render-side backlog queued during the outage"
        );
    }
}

async fn run_ssh_session(
    ssh: &SshConfig,
    ch: PtyChannels,
    editor: EditorChannels,
    connected: &AtomicBool,
    watches: &EngineWatches,
) -> Result<()> {
    use rift_ssh::SshConnection;

    let mut conn = SshConnection::connect(
        &ssh.host,
        ssh.port,
        &ssh.user,
        &ssh.key,
        ssh.passphrase.as_deref(),
    )
    .await
    .context("SSH connection failed")?;

    // Provision the daemon ahead of the terminal: detect the platform, upload the
    // versioned binary when absent, then attach — spawning it detached if none is
    // running — and confirm the transport with a handshake. The detached daemon
    // survives SSH drops, so a reconnect reattaches instead of spawning a second
    // one (#62). Returns the live client on success; `None` when no daemon binary
    // is configured (or a step fails), in which case the legacy tmux path still
    // runs without daemon-backed features. A protocol version mismatch that
    // survives the stale-daemon replacement fails the session instead of
    // degrading — falling back would hide real skew as silent feature death.
    let daemon_client = provision_daemon(&mut conn).await?;

    // Tmux session name: seeded from the Connection screen's connect request
    // into the engine's session watch (the screen's Session field defaults
    // to `RIFT_SESSION`/`rift`, so a second rift instance still mirrors the
    // same live session, or attaches to an isolated one for destructive
    // tests, `RIFT_SESSION=rift-dev` — docs/spec-dogfooding-channels.md, by
    // leaving the field at its prefilled default). Resolved through the
    // watch, not the request, so an SSH-level reconnect re-attaches the
    // session a cockpit switch (#509) moved the client to.
    let session = watches.session.borrow().clone();

    // Terminal byte source (Phase 6 swap, #205): the daemon protocol is the
    // default; the legacy direct `tmux -CC` over an SSH PTY stays as an
    // env-selected escape hatch until the milestone QA gate (gate decision in
    // docs/spec-terminal-streaming.md). The render stack is identical either way —
    // only where the bytes come from changes.
    if use_daemon_terminal() {
        match daemon_client {
            Some((client, endpoint)) => {
                info!("terminal source: daemon protocol");
                return run_daemon_terminal(
                    &mut conn, client, endpoint, watches, ch, editor, connected,
                )
                .await;
            }
            None => {
                warn!(
                    "daemon terminal selected but no daemon available; \
                     falling back to the legacy tmux path"
                );
                // Reactive layer is dead for this session (#619): the legacy
                // tmux path below still runs the terminal, but the explorer,
                // diagnostics, git status, and LSP never light up. Tell the
                // workspace so it can surface that, rather than leaving it
                // silent.
                let _ = editor.daemon_unavailable_tx.send(());
            }
        }
    } else if let Some((client, _endpoint)) = daemon_client {
        // Legacy terminal, but keep the daemon's worktree/git/diagnostics +
        // buffer-channel stream alive on its own task (today's behavior) while
        // tmux drives the terminal. No mid-session daemon recovery on this
        // escape hatch (#475 scopes it to the daemon terminal path): the watch
        // sender is dropped right away, so the bridges resolve this one client
        // for the whole session.
        info!("terminal source: legacy tmux (daemon worktree stream active)");
        let client = std::sync::Arc::new(client);
        let (_client_tx, client_rx) = tokio::sync::watch::channel(client.clone());
        spawn_open_file_bridge(client_rx.clone(), editor.open_file_rx.clone());
        spawn_save_file_bridge(client_rx.clone(), editor.save_file_rx.clone());
        spawn_buffer_change_bridge(client_rx.clone(), editor.buffer_change_rx.clone());
        spawn_nav_bridge(client_rx.clone(), editor.nav_request_rx.clone());
        spawn_request_diff_bridge(client_rx.clone(), editor.request_diff_rx.clone());
        spawn_git_op_bridge(client_rx.clone(), editor.git_op_rx.clone());
        spawn_file_op_bridge(client_rx, editor.file_op_rx.clone());
        tokio::spawn(async move {
            consume_daemon_messages(&client, None, &editor).await;
        });
    } else {
        info!("terminal source: legacy tmux (no daemon configured)");
    }

    run_legacy_terminal(conn, session, ch, connected).await
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
/// into the existing render channels. Blocks until the tmux attach reports
/// `TerminalExit`; returning `Ok` ends the session as an orderly exit, which
/// the SSH-level engine (#476) surfaces as the visible `Disconnected` state
/// without retrying — never an app quit. The SSH connection (`conn`) stays
/// alive for the session; the recovery engine below reuses it to reopen daemon
/// channels.
///
/// A daemon-channel death (EOF, malformed frame, channel error) while SSH is
/// up does NOT end the session (#475, `docs/spec-connection-robustness.md`):
/// the loop surfaces `ConnectionStatus::Reconnecting`, reattaches to the
/// daemon socket under bounded jittered backoff — respawning the daemon if it
/// died — re-runs Hello/Welcome (the per-connection Welcome snapshot replay,
/// #227/#425/#426, restores worktree/git/diagnostics), and re-Attaches the
/// terminal, whose fresh `LayoutSnapshot` resets the render layer (the
/// protocol's reconnect contract). The bridges survive the gap: they resolve
/// the current client through a `watch` channel per message, so the recovery
/// engine swaps a reconnected client in under them. Two more `watch` channels
/// feed the recovery what a re-Attach cannot rediscover on its own: the
/// currently attached session (a cockpit switch, #509, may have moved off the
/// startup one) and the last known client grid (re-asserted as a `ResizePane`
/// after the re-Attach — the fresh tmux child spawns unsized and the render
/// layer only re-sends on a size change). Both live at ENGINE scope
/// ([`EngineWatches`], owned by `run_session_with_reconnect`), not per
/// attempt: the SSH-level reconnect needs the same two answers — otherwise it
/// would silently re-attach the startup session after a cockpit switch and
/// leave the fresh child on the attach-default grid — so this function
/// receives the handles instead of creating them.
async fn run_daemon_terminal(
    conn: &mut rift_ssh::SshConnection,
    client: rift_ssh::DaemonClient,
    endpoint: DaemonEndpoint,
    watches: &EngineWatches,
    ch: PtyChannels,
    editor: EditorChannels,
    connected: &AtomicBool,
) -> Result<()> {
    use rift_protocol::ClientMessage;
    use std::sync::Arc;

    let client = Arc::new(client);
    // The live-client handle the bridges resolve per send; the recovery engine
    // publishes each reconnected client here (`DaemonClientWatch`).
    let (client_tx, client_rx) = tokio::sync::watch::channel(client.clone());
    // The session this client is currently attached to. A cockpit switch
    // (`docs/spec-session-switch.md`) re-attaches mid-session, so the recovery
    // engine resolves the current name here instead of reusing the startup one
    // — otherwise a stream death after a switch would silently re-attach the
    // original session. Updated by `spawn_session_switch_bridge` per sent
    // switch; a switch dropped during an outage stays untracked (dropped,
    // never buffered), so recovery restores the last session actually asked of
    // the daemon.
    let session_rx = watches.session.subscribe();
    // The render layer's last known client grid. Its resize channel only fires
    // on a size *change*, so after a reconnect nobody would re-send the size —
    // the fresh tmux child would stay on the 80x24 attach default until the
    // user happens to resize (#475). `spawn_resize_bridge` caches every grid
    // here; the recovery re-asserts it right after the re-Attach, mirroring
    // the session switch's viewport re-assert. `None` until the first layout
    // fires (then the attach default is all there is to keep).
    let viewport_rx = watches.viewport.subscribe();

    // Open this client's own tmux attach against the engine-tracked current
    // session (seeded end-to-end from the Connection screen's connect
    // request). The daemon answers with a
    // LayoutSnapshot baseline, then the live stream. A failed send means the
    // daemon channel died right behind the handshake — a transport death for
    // the reconnect engine, not an orderly end.
    let session = watches.session.borrow().clone();
    client
        .send(ClientMessage::Attach { session })
        .await
        .context("failed to open daemon terminal attach")?;
    // Re-assert the last known grid behind the attach, exactly like the
    // recovery's re-Attach does: on an SSH-level reconnect the fresh tmux
    // child spawns unsized and the render layer will not re-send an unchanged
    // size. `None` on the very first connect — the initial layout's resize is
    // still in flight and lands through the resize bridge instead. The value
    // is copied out before the send so the watch read guard is never held
    // across an await (`reconnect_daemon` does the same).
    let viewport = *watches.viewport.borrow();
    if let Some(size) = viewport {
        client
            .send(ClientMessage::ResizePane {
                pane_id: 0,
                cols: size.cols as u16,
                rows: size.rows as u16,
            })
            .await
            .context("failed to re-assert client viewport after attach")?;
    }
    let _ = ch.connection_status_tx.send(ConnectionStatus::Connected);
    // Tells the reconnect engine this run was fully up, so the next outage
    // restarts the retry schedule instead of continuing the old one.
    connected.store(true, Ordering::Relaxed);

    // Reverse-path bridges: each forwards a render-side flume stream onto the
    // protocol. They live as long as their render-side channel; a send that
    // fails against a dead daemon stream drops the message (the reconnect
    // resync replaces state). Pane ids cross the seam as tmux's native `%N`
    // form.
    spawn_input_bridge(client_rx.clone(), ch.input_rx);
    spawn_resize_bridge(
        client_rx.clone(),
        ch.size_changed_rx,
        watches.viewport.clone(),
    );
    spawn_command_bridge(client_rx.clone(), ch.tmux_command_rx);
    spawn_capture_bridge(client_rx.clone(), ch.capture_request_rx);
    // Key-table refresh reverse path (tmux key-table mirroring, #212): each
    // request becomes a `QueryKeyTable`; the daemon also issues one unprompted
    // on attach, so this bridge only carries the statusbar's explicit-refresh
    // trigger (a binding-mutating dispatch's refresh is issued inline by
    // `spawn_command_bridge` instead, ordered after the mutation on the same
    // task). The reply returns via `consume_daemon_messages` on
    // `sinks.key_table_result_tx`.
    spawn_key_table_bridge(client_rx.clone(), ch.key_table_request_rx);
    // Session-switch reverse paths (`docs/spec-session-switch.md`): an explicit
    // list refresh (opening the switcher) becomes a `QuerySessionList` — the
    // daemon's churn-driven pushes keep the list live in between — and a
    // switcher selection becomes a re-`Attach` (a clean child swap + fresh
    // `LayoutSnapshot`, zero daemon changes for the switch itself). The switch
    // bridge also records the newly attached session, so a later stream
    // recovery re-attaches to the session the client is actually on, not the
    // one it started on.
    spawn_session_list_bridge(client_rx.clone(), ch.session_list_request_rx);
    spawn_session_switch_bridge(
        client_rx.clone(),
        ch.session_switch_rx,
        watches.session.clone(),
    );
    // Buffer channel reverse path: the editor's open requests become `OpenFile`
    // reads (#187) and its save requests `SaveFile` writes (#188). The forward
    // replies (`FileContent` / `SaveResult` / `SaveConflict`) return via
    // `consume_daemon_messages` on `editor.buffer_tx`.
    spawn_open_file_bridge(client_rx.clone(), editor.open_file_rx.clone());
    spawn_save_file_bridge(client_rx.clone(), editor.save_file_rx.clone());
    // Live-buffer feed reverse path (#189): the editor's `BufferChanged` /
    // `BufferClosed` forward verbatim so the daemon feeds the LSP the live buffer.
    // Push-only — diagnostics return on the worktree stream as `Diagnostics`.
    spawn_buffer_change_bridge(client_rx.clone(), editor.buffer_change_rx.clone());
    // Navigation request reverse path (#196): `DefinitionRequest` forwards verbatim.
    // The `DefinitionResponse` returns via `consume_daemon_messages` on `editor.nav_tx`.
    spawn_nav_bridge(client_rx.clone(), editor.nav_request_rx.clone());
    // Diff pull reverse path (#338): the source-control panel's selection becomes
    // a `RequestDiff`. The `FileDiff` reply returns via `consume_daemon_messages`
    // on `editor.diff_tx`.
    spawn_request_diff_bridge(client_rx.clone(), editor.request_diff_rx.clone());
    // Git write-op reverse path (#546): the source-control panel's stage /
    // unstage / discard / commit actions forward verbatim. Push-only from here —
    // the resulting git change returns on the worktree stream, not as a routed
    // reply.
    spawn_git_op_bridge(client_rx.clone(), editor.git_op_rx.clone());
    // File op reverse path (`docs/spec-explorer-file-ops.md`, #675): the file
    // tree's create/rename/delete/move actions forward verbatim. The
    // `FileOpResult` reply returns via `consume_daemon_messages` on
    // `editor.file_op_result_tx` — routed back for UX only, never the tree
    // mutation, which stays push-only via `UpdateWorktree`.
    spawn_file_op_bridge(client_rx, editor.file_op_rx.clone());

    // Forward path: fold the daemon stream into the render channels (pane output,
    // layout snapshots, capture replies), the file tree, and the editor. Blocks
    // until the tmux attach ends; a stream death instead enters the recovery
    // engine below.
    let sinks = TerminalSinks {
        pane_output_tx: ch.pane_output_tx,
        snapshot_tx: ch.snapshot_tx,
        capture_result_tx: ch.capture_result_tx,
        key_table_result_tx: ch.key_table_result_tx,
        session_list_tx: ch.session_list_tx,
    };
    let mut current = client;
    loop {
        if consume_daemon_messages(&current, Some(&sinks), &editor).await == StreamEnd::TerminalExit
        {
            return Ok(());
        }
        // The stream died under the consumer while SSH is up: recover instead
        // of leaving a frozen reactive layer (#475). Reconnect under bounded
        // backoff, then publish the fresh client to the bridges; the Welcome
        // snapshot replay and the re-Attach's fresh LayoutSnapshot resync
        // worktree/git/diagnostics and the terminal. A recovery that gives up
        // propagates as a session error — the visible failure path
        // (`Disconnected`), never a silent one.
        let _ = ch.connection_status_tx.send(ConnectionStatus::Reconnecting);
        current = reconnect_daemon(conn, &endpoint, &session_rx, &viewport_rx).await?;
        client_tx.send_replace(current.clone());
        let _ = ch.connection_status_tx.send(ConnectionStatus::Connected);
    }
}

/// Why a single daemon reconnect attempt failed: transient failures retry
/// under backoff; a protocol version mismatch aborts recovery (real skew).
enum ReconnectFailure {
    /// The daemon that answered runs a different protocol version — another
    /// (newer) client replaced it mid-session. Retrying cannot converge, and
    /// replacing the daemon from here would kill that client's live session,
    /// so recovery surfaces it as a session error instead.
    VersionMismatch { daemon: u32 },
    /// Anything transport-shaped: probe/spawn/exec failure, handshake timeout,
    /// a send failure on the fresh channel. Retried under backoff.
    Transient(rift_ssh::SshError),
}

/// Reconnect to the daemon after a mid-session stream death (#475): bounded
/// attempts under [`rift_ssh::ReconnectBackoff`]'s jittered capped schedule
/// (first attempt immediate, so a plain daemon kill converges within seconds).
/// Returns the reconnected, re-attached client; errors once
/// [`DAEMON_RECONNECT_MAX_ATTEMPTS`] is exhausted or a version mismatch proves
/// the daemon was replaced mid-session. Session and viewport resolve through
/// their `watch` handles per attempt, so the re-Attach targets the session the
/// client is currently on and re-asserts the freshest grid even when both
/// changed since the stream died.
async fn reconnect_daemon(
    conn: &mut rift_ssh::SshConnection,
    endpoint: &DaemonEndpoint,
    session_rx: &tokio::sync::watch::Receiver<String>,
    viewport_rx: &tokio::sync::watch::Receiver<Option<TermSize>>,
) -> Result<std::sync::Arc<rift_ssh::DaemonClient>> {
    let mut backoff = rift_ssh::ReconnectBackoff::new();
    let mut last_transient: Option<rift_ssh::SshError> = None;
    for attempt in 1..=DAEMON_RECONNECT_MAX_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(backoff.next_delay()).await;
        }
        // A dead SSH transport (drop, exhausted keepalive window, #438)
        // cannot carry a daemon channel: hand the outage to the SSH-level
        // reconnect loop (#476) right away instead of burning the bounded
        // attempts against it.
        if conn.is_closed() {
            return Err(anyhow::Error::new(rift_ssh::SshError::Connection(
                "SSH transport closed".to_string(),
            ))
            .context("daemon stream reconnect aborted"));
        }
        info!(attempt, "reconnecting daemon stream");
        let session = session_rx.borrow().clone();
        let viewport = *viewport_rx.borrow();
        match try_daemon_reconnect(conn, endpoint, &session, viewport).await {
            Ok(client) => {
                info!(attempt, "daemon stream reconnected");
                return Ok(client);
            }
            Err(ReconnectFailure::VersionMismatch { daemon }) => {
                return Err(anyhow::anyhow!(
                    "daemon protocol version changed mid-session (client v{}, daemon v{daemon}); \
                     a newer client replaced the daemon",
                    rift_protocol::PROTOCOL_VERSION
                ));
            }
            Err(ReconnectFailure::Transient(e)) => {
                warn!(%e, attempt, "daemon reconnect attempt failed");
                last_transient = Some(e);
            }
        }
    }
    // Carry the last transport error in the chain so the SSH-level engine can
    // classify the give-up as retryable (`is_retryable_session_error`).
    Err(match last_transient {
        Some(e) => anyhow::Error::new(e).context(format!(
            "daemon stream reconnect gave up after {DAEMON_RECONNECT_MAX_ATTEMPTS} attempts"
        )),
        None => anyhow::anyhow!(
            "daemon stream reconnect gave up after {DAEMON_RECONNECT_MAX_ATTEMPTS} attempts"
        ),
    })
}

/// One reconnect attempt (#475): reattach to the daemon socket — respawning a
/// dead daemon detached, exactly like the initial provisioning — confirm the
/// transport with a bounded Hello/Welcome (the daemon replays its full state
/// snapshot to this connection right behind the Welcome, #227/#425, which IS
/// the worktree/git/diagnostics resync), then re-open this client's tmux
/// attach. The fresh `LayoutSnapshot` the attach answers with resets the
/// render layer per the protocol's reconnect contract; tmux persistence keeps
/// the terminal content intact across the gap. The last known grid is
/// re-asserted as a `ResizePane` on this same task, strictly after the
/// `Attach` (sequential sends land in program order) — the fresh tmux child
/// spawns unsized and the render layer only re-sends on a size *change*, so
/// without it the terminal would stay reflowed to the 80x24 attach default
/// (same re-assert the session-switch bridge does).
async fn try_daemon_reconnect(
    conn: &mut rift_ssh::SshConnection,
    endpoint: &DaemonEndpoint,
    session: &str,
    viewport: Option<TermSize>,
) -> Result<std::sync::Arc<rift_ssh::DaemonClient>, ReconnectFailure> {
    use rift_ssh::Handshake;

    let channel = rift_ssh::connect_or_spawn_daemon(
        conn,
        &endpoint.remote_path,
        &endpoint.socket_path,
        &endpoint.log_path,
        endpoint.project_root.as_deref(),
    )
    .await
    .map_err(ReconnectFailure::Transient)?;
    let client = rift_ssh::DaemonClient::new(channel);
    match client.handshake(DAEMON_HANDSHAKE_TIMEOUT).await {
        Ok(Handshake::Ready) => {}
        Ok(Handshake::VersionMismatch { daemon }) => {
            return Err(ReconnectFailure::VersionMismatch { daemon });
        }
        Err(e) => return Err(ReconnectFailure::Transient(e)),
    }
    client
        .send(rift_protocol::ClientMessage::Attach {
            session: session.to_string(),
        })
        .await
        .map_err(ReconnectFailure::Transient)?;
    if let Some(size) = viewport {
        client
            .send(rift_protocol::ClientMessage::ResizePane {
                pane_id: 0,
                cols: size.cols as u16,
                rows: size.rows as u16,
            })
            .await
            .map_err(ReconnectFailure::Transient)?;
    }
    Ok(std::sync::Arc::new(client))
}

/// The legacy terminal path: open a `tmux -CC` control-mode session over an SSH
/// PTY and stream it through termy's [`TmuxClient`]. Identical to the pre-#205
/// behavior; retained as the env-selected fallback until the milestone QA gate.
async fn run_legacy_terminal(
    mut conn: rift_ssh::SshConnection,
    session: String,
    ch: PtyChannels,
    connected: &AtomicBool,
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
    // See `run_daemon_terminal`: resets the reconnect engine's retry schedule.
    connected.store(true, Ordering::Relaxed);

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
    // Legacy tmux path: termy's snapshot has no daemon-evaluated `is_shell`
    // flag, so the map is empty and every window renders the process glyph.
    let _ = snapshot_tx.send(SessionSnapshot {
        snapshot: initial_snapshot,
        pane_is_shell: std::collections::HashMap::new(),
    });

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
                        let _ = snapshot_tx.send(SessionSnapshot {
                            snapshot,
                            pane_is_shell: std::collections::HashMap::new(),
                        });
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

/// Upper bound on the daemon's `Welcome` reply to our `Hello` (#441). A wedged
/// daemon otherwise blocks [`provision_daemon`] forever, leaving the app stuck
/// at "connecting" with no error.
const DAEMON_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Upper bound on consecutive daemon reconnect attempts after a mid-session
/// stream death (#475). With [`rift_ssh::ReconnectBackoff`]'s capped schedule
/// this covers roughly two minutes of outage before recovery gives up and the
/// session fails over to [`run_session_with_reconnect`] — a daemon that
/// [`reconnect_daemon`] cannot respawn within that window is a structural
/// failure (SSH transport dead, binary gone), which the SSH-level reconnect
/// loop (#476) owns. A dead SSH transport short-circuits the window via
/// `SshConnection::is_closed`, so an SSH drop reaches the SSH-level loop (and
/// its banner) within the keepalive detection bound (#438), not after it.
const DAEMON_RECONNECT_MAX_ATTEMPTS: u32 = 10;

/// The resolved remote daemon endpoint [`provision_daemon`] produced:
/// everything a mid-session reconnect needs to reattach to (or respawn) the
/// same daemon again without re-running the deploy (#475).
struct DaemonEndpoint {
    /// Absolute remote path of the deployed, versioned daemon binary.
    remote_path: String,
    /// Unix socket beside the binary (`<binary>.sock`).
    socket_path: String,
    /// Detached-spawn log beside the binary (`<binary>.log`).
    log_path: String,
    /// Project root a fresh spawn watches; a reattach keeps the running
    /// daemon's root.
    project_root: Option<String>,
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
/// `Hello`/`Welcome` handshake bounded by [`DAEMON_HANDSHAKE_TIMEOUT`],
/// enforcing strict protocol version equality (`docs/protocol.md` —
/// Versioning policy). A mismatched `Welcome` identifies a stale RUNNING
/// daemon; the client owns the replacement
/// (`docs/spec-connection-robustness.md`): stop it via its pidfile (#281),
/// re-run the fingerprinted deploy, respawn detached, re-handshake — one
/// retry. The detached daemon outlives the SSH connection,
/// so a later reconnect reattaches to it instead of spawning a second one (#62).
///
/// Returns the live [`rift_ssh::DaemonClient`] on a clean handshake, paired
/// with the [`DaemonEndpoint`] a mid-session reconnect reuses (#475); the
/// caller decides how to drive it (the terminal byte stream in daemon mode, or
/// just the worktree/git/diagnostics consumer in legacy mode). Provisioning
/// steps are best-effort: an unconfigured binary or any step error logs and
/// returns `Ok(None)`, so the legacy tmux flow keeps working without the
/// daemon. The one hard failure is a version mismatch that persists after the
/// replacement: that is real protocol skew the fallback would hide as silent
/// feature death, so it returns `Err` and fails the session visibly. The
/// socket and log sit beside the versioned binary (`<binary>.sock` /
/// `<binary>.log`), inheriting its path.
async fn provision_daemon(
    conn: &mut rift_ssh::SshConnection,
) -> Result<Option<(rift_ssh::DaemonClient, DaemonEndpoint)>> {
    use rift_ssh::Handshake;

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
                return Ok(None);
            }
        },
    };

    let bytes = match tokio::fs::read(&binary_path).await {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!(%e, path = %binary_path.display(), "failed to read local daemon binary, skipping daemon");
            return Ok(None);
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
            return Ok(None);
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
    // RIFT_SSH_KEY / RIFT_DEFAULT_SSH_KEY split. None makes a freshly spawned daemon
    // refuse to start and this call fall back to tmux-only (see the `Err` arm
    // below); the root is only honored on a fresh spawn, so a reattach keeps the
    // already-running daemon's root regardless of this value.
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
            return Ok(None);
        }
    };

    // Confirm the reattach transport with a protocol round-trip. The daemon
    // answers each Hello with a per-connection Welcome (#473) and replays its
    // full state snapshot to this connection right behind it (#227, ordered
    // by #425), so the stream following the Welcome starts with the complete
    // tree. The wait is bounded (#441): a wedged daemon must fall back like
    // any other provisioning failure instead of holding the app at
    // "connecting" forever.
    let client = rift_ssh::DaemonClient::new(channel);
    let stale_version = match client.handshake(DAEMON_HANDSHAKE_TIMEOUT).await {
        Ok(Handshake::Ready) => {
            info!(
                version = rift_protocol::PROTOCOL_VERSION,
                "daemon transport ready (Hello/Welcome ok)"
            );
            return Ok(Some((
                client,
                DaemonEndpoint {
                    remote_path,
                    socket_path,
                    log_path,
                    project_root,
                },
            )));
        }
        Ok(Handshake::VersionMismatch { daemon }) => daemon,
        Err(e) => {
            warn!(%e, "daemon handshake failed, continuing with tmux only");
            return Ok(None);
        }
    };

    // A RUNNING daemon answered with a different protocol version — a stale
    // process left behind by an older deploy. Strict equality is resolved
    // client-side (`docs/spec-connection-robustness.md`): stop it via its
    // pidfile (#281), re-run the fingerprinted deploy so the on-disk binary
    // matches this client, respawn detached, and re-handshake, bounded like
    // the first attempt. One retry only — a second mismatch is real skew the
    // legacy fallback would hide as silent feature death, so it fails the
    // session as a visible connection error instead.
    warn!(
        daemon = stale_version,
        client = rift_protocol::PROTOCOL_VERSION,
        "daemon protocol version mismatch, replacing the stale daemon"
    );
    drop(client);
    if let Err(e) = rift_ssh::stop_daemon(conn, &socket_path).await {
        warn!(%e, "stale daemon stop failed, continuing with tmux only");
        return Ok(None);
    }
    if let Err(e) =
        rift_ssh::ensure_daemon_deployed(conn, &bytes, &remote_dir, env!("CARGO_PKG_VERSION")).await
    {
        warn!(%e, "daemon redeploy failed, continuing with tmux only");
        return Ok(None);
    }
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
            warn!(%e, "daemon respawn failed, continuing with tmux only");
            return Ok(None);
        }
    };
    let client = rift_ssh::DaemonClient::new(channel);
    match client.handshake(DAEMON_HANDSHAKE_TIMEOUT).await {
        Ok(Handshake::Ready) => {
            info!(
                version = rift_protocol::PROTOCOL_VERSION,
                "daemon transport ready after stale-daemon replacement"
            );
            Ok(Some((
                client,
                DaemonEndpoint {
                    remote_path,
                    socket_path,
                    log_path,
                    project_root,
                },
            )))
        }
        Ok(Handshake::VersionMismatch { daemon }) => Err(anyhow::anyhow!(
            "daemon protocol version mismatch persists after replacement \
             (client v{}, daemon v{daemon})",
            rift_protocol::PROTOCOL_VERSION
        )),
        Err(e) => {
            warn!(%e, "daemon re-handshake failed, continuing with tmux only");
            Ok(None)
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
/// but the worktree + buffer channel still flow. Returns how the stream ended
/// ([`StreamEnd`]): a closed channel is the recoverable stream death the
/// recovery engine reacts to (#475), a `TerminalExit` the orderly end of the
/// active attach; either way the detached daemon keeps running for the next
/// attach. The structure/buffer sends are best-effort: a closed GPUI-side
/// receiver (window gone) drops the message rather than fail the loop.
async fn consume_daemon_messages(
    client: &rift_ssh::DaemonClient,
    terminal: Option<&TerminalSinks>,
    editor: &EditorChannels,
) -> StreamEnd {
    use rift_protocol::DaemonMessage;

    while let Some(msg) = client.recv().await {
        match msg {
            // --- terminal byte stream (daemon terminal mode only) ---
            DaemonMessage::PaneOutput { pane_id, bytes } => {
                if let Some(sinks) = terminal {
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
                if let Some(sinks) = terminal {
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
                if let Some(sinks) = terminal {
                    let _ = sinks
                        .key_table_result_tx
                        .send(KeyTableQueryResult { list_keys, options });
                }
            }
            // The host's session list (the reply to a `QuerySessionList` or the
            // daemon's unprompted churn-driven push): map the protocol entries
            // to the render layer's items — `rift-terminal` does not depend on
            // `rift-protocol`, mirroring the layout seam — and route them to
            // the session switcher, which replaces its whole list.
            DaemonMessage::SessionListReply { sessions } => {
                if let Some(sinks) = terminal {
                    let sessions = sessions
                        .into_iter()
                        .map(|entry| SessionListItem {
                            id: entry.id,
                            name: entry.name,
                            windows: entry.windows,
                            attached: entry.attached,
                        })
                        .collect();
                    let _ = sinks.session_list_tx.send(sessions);
                }
            }
            // Snapshot and update both carry the full latest layout (replace
            // semantics), which is exactly what the render layer's `apply_snapshot`
            // expects — so both fold into one synthesized `TmuxSnapshot`.
            DaemonMessage::LayoutSnapshot { session, windows }
            | DaemonMessage::LayoutUpdate { session, windows } => {
                if let Some(sinks) = terminal {
                    let _ = sinks.snapshot_tx.send(layout_to_snapshot(session, windows));
                }
            }
            DaemonMessage::TerminalExit { session, reason } => {
                info!(%session, ?reason, "daemon terminal path down");
                if terminal.is_some() {
                    // The tmux attach itself ended (an orderly `%exit`, not a
                    // transport death): end the session so it surfaces as the
                    // visible `Disconnected` state without a reconnect (#476).
                    // Never recovered from — recovery is for stream deaths
                    // only (#475).
                    return StreamEnd::TerminalExit;
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
            // read reply (the only message carrying file content), the
            // `SaveFile` write replies (`SaveResult` / `SaveConflict`), and the
            // typed refusals (`OpenError` / `SaveError`, `docs/spec-v1-hardening.md`)
            // that answer a refused open/save immediately instead of leaving the
            // editor to fall back to its own `OPEN_TIMEOUT` / `SAVE_TIMEOUT`.
            // Forward each to the editor, which routes it by path against the
            // open buffer.
            msg @ (DaemonMessage::FileContent { .. }
            | DaemonMessage::SaveResult { .. }
            | DaemonMessage::SaveConflict { .. }
            | DaemonMessage::OpenError { .. }
            | DaemonMessage::SaveError { .. }) => {
                let _ = editor.buffer_tx.send(msg);
            }
            // --- nav replies -> editor (every mode) ---
            // Definition, hover, references, and document-symbol responses route
            // to the editor's nav reply channel; the workspace's `nav_rx` loop
            // dispatches each to the correct `apply_*` method on the GPUI side
            // (#196, #197, #198, editor-chrome breadcrumb).
            msg @ (DaemonMessage::DefinitionResponse { .. }
            | DaemonMessage::HoverResponse { .. }
            | DaemonMessage::ReferencesResponse { .. }
            | DaemonMessage::DocumentSymbolResponse { .. }) => {
                let _ = editor.nav_tx.send(msg);
            }
            // --- language-server health -> composite status line (every mode) ---
            // Push-only lifecycle transitions (starting/running/crashed), keyed
            // by the server's stable name; replayed once per known server behind
            // Welcome so a (re)attaching client sees current health without
            // waiting for the next transition (`docs/spec-status-line.md`).
            msg @ DaemonMessage::LspStatus { .. } => {
                let _ = editor.lsp_status_tx.send(msg);
            }
            // --- diff reply -> diff view (every mode) ---
            // The reply to a `RequestDiff`: forward to the diff view, which
            // routes it by path against the currently open selection (#338).
            msg @ DaemonMessage::FileDiff { .. } => {
                let _ = editor.diff_tx.send(msg);
            }
            // --- file-op reply -> file tree (every mode) ---
            // The reply to a `CreateFile` / `CreateDir` / `RenamePath` /
            // `DeletePath`: forward to the file tree for UX transitions only
            // (`docs/spec-explorer-file-ops.md`, #675) — the tree mutation
            // itself stays push-only via the worktree-family messages above.
            msg @ DaemonMessage::FileOpResult { .. } => {
                let _ = editor.file_op_result_tx.send(msg);
            }
            other => debug!(?other, "daemon message without a consumer yet"),
        }
    }
    info!("daemon message stream ended");
    StreamEnd::Closed
}

/// How [`consume_daemon_messages`] ended, deciding the caller's next move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamEnd {
    /// The channel closed under the consumer — remote EOF, a malformed frame,
    /// a channel error — without an orderly `TerminalExit`. While SSH is up
    /// this is the recoverable daemon-stream death (#475): reconnect + resync.
    Closed,
    /// The daemon reported the tmux attach ended (`%exit`): an orderly end of
    /// the session, not a transport failure to recover from.
    TerminalExit,
}

/// The bridges' handle to the current daemon client: a `watch` receiver the
/// recovery engine updates after a reconnect (#475). A bridge resolves the
/// live client per message instead of capturing one client for its lifetime,
/// so it survives a daemon-stream death; while the stream is down a send fails
/// against the dead client and the message is dropped — never buffered — and
/// the post-reconnect resync (Welcome snapshot replay + fresh LayoutSnapshot)
/// replaces the state those messages would have touched. Each bridge therefore
/// ends only when its render-side channel closes.
type DaemonClientWatch = tokio::sync::watch::Receiver<std::sync::Arc<rift_ssh::DaemonClient>>;

/// Forward the editor's file-open requests onto the protocol as
/// [`rift_protocol::ClientMessage::OpenFile`] reads (#187). Each path the file
/// tree emitted becomes one read request; the daemon answers with a
/// `FileContent` reply that returns through [`consume_daemon_messages`] on
/// `editor.buffer_tx`. A *refused* request (binary / path escape) draws no
/// reply by protocol, so the editor's own timeout recovers it. Ends when the
/// render-side channel closes.
fn spawn_open_file_bridge(client_rx: DaemonClientWatch, open_file_rx: flume::Receiver<String>) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while let Ok(path) = open_file_rx.recv_async().await {
            debug!(%path, "sending open-file request");
            let client = client_rx.borrow().clone();
            let _ = client.send(ClientMessage::OpenFile { path }).await;
        }
    });
}

/// Forward the source-control panel's selections onto the protocol as
/// [`rift_protocol::ClientMessage::RequestDiff`] pulls (#338). Each selected
/// path becomes one diff request; the daemon answers with a `FileDiff` reply
/// that returns through [`consume_daemon_messages`] on `editor.diff_tx`. Ends
/// when the render-side channel closes.
fn spawn_request_diff_bridge(
    client_rx: DaemonClientWatch,
    request_diff_rx: flume::Receiver<String>,
) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while let Ok(path) = request_diff_rx.recv_async().await {
            debug!(%path, "sending diff request");
            let client = client_rx.borrow().clone();
            let _ = client.send(ClientMessage::RequestDiff { path }).await;
        }
    });
}

/// Forward the source-control panel's git write ops onto the protocol (#546):
/// each `StageFile` / `UnstageFile` / `DiscardFile` / `Commit` the panel built
/// is sent verbatim. Push-only from the panel's side — the daemon answers with
/// a `GitOpResult` (`ok`/`error`), but the resulting state change arrives on
/// the worktree stream (`UpdateGitStatus` / `RepoState`), the one source of
/// truth for git state, so no reply is routed back here. Ends when the
/// render-side channel closes.
fn spawn_git_op_bridge(
    client_rx: DaemonClientWatch,
    git_op_rx: flume::Receiver<rift_protocol::ClientMessage>,
) {
    tokio::spawn(async move {
        while let Ok(msg) = git_op_rx.recv_async().await {
            debug!(op = ?msg, "sending git write op");
            let client = client_rx.borrow().clone();
            let _ = client.send(msg).await;
        }
    });
}

/// Forward the file tree's file ops onto the protocol
/// (`docs/spec-explorer-file-ops.md`, #675): each `CreateFile` / `CreateDir` /
/// `RenamePath` / `DeletePath` the tree built (via `FileTreeEvent`, turned
/// into a `ClientMessage` by `workspace.rs`) is sent verbatim — the same
/// shape as [`spawn_git_op_bridge`]. Unlike the git-write channel, the
/// daemon's `FileOpResult` reply IS routed back (`consume_daemon_messages` on
/// `editor.file_op_result_tx`), since the tree needs it for UX transitions;
/// the resulting tree change still arrives only through the push-only
/// `UpdateWorktree` recompute. Ends when the render-side channel closes.
fn spawn_file_op_bridge(
    client_rx: DaemonClientWatch,
    file_op_rx: flume::Receiver<rift_protocol::ClientMessage>,
) {
    tokio::spawn(async move {
        while let Ok(msg) = file_op_rx.recv_async().await {
            debug!(op = ?msg, "sending file op");
            let client = client_rx.borrow().clone();
            let _ = client.send(msg).await;
        }
    });
}

/// Forward the editor's save requests onto the protocol as
/// [`rift_protocol::ClientMessage::SaveFile`] writes (#188). Each is the whole
/// open buffer plus its base `mtime`; the daemon answers with a `SaveResult` or a
/// `SaveConflict` that returns through [`consume_daemon_messages`] on
/// `editor.buffer_tx`. A *refused* write (a path escape, non-UTF-8) draws no reply
/// by protocol, so the editor's own save timeout recovers it. Ends when the
/// render-side channel closes. The editor builds the full `SaveFile` (path,
/// content, base_mtime), so the bridge forwards it unchanged.
fn spawn_save_file_bridge(
    client_rx: DaemonClientWatch,
    save_file_rx: flume::Receiver<rift_protocol::ClientMessage>,
) {
    tokio::spawn(async move {
        while let Ok(msg) = save_file_rx.recv_async().await {
            if let rift_protocol::ClientMessage::SaveFile { path, .. } = &msg {
                debug!(%path, "sending save-file request");
            }
            let client = client_rx.borrow().clone();
            let _ = client.send(msg).await;
        }
    });
}

/// Forward the editor's live-buffer feed onto the protocol (#189): each
/// `BufferChanged` (debounced edit) or `BufferClosed` (close / switch / save) is
/// sent verbatim so the daemon feeds the LSP the live buffer (the disk→buffer
/// source-of-truth shift). Push-only — there is no reply; diagnostics return on
/// the worktree stream as `Diagnostics`. Ends when the render-side channel
/// closes.
fn spawn_buffer_change_bridge(
    client_rx: DaemonClientWatch,
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
            let client = client_rx.borrow().clone();
            let _ = client.send(msg).await;
        }
    });
}

/// Forward the editor's navigation requests onto the protocol (#196, #197, #198):
/// `DefinitionRequest` (ctrl+click / context-menu / F12), `HoverRequest`
/// (Ctrl+K Ctrl+I / context-menu "Show Hover" / mouse-rest debounce), and
/// `ReferencesRequest` (Shift+F12 / context-menu "Find References") are sent
/// verbatim; the daemon answers with `DefinitionResponse` / `HoverResponse` /
/// `ReferencesResponse` that return through [`consume_daemon_messages`] on
/// `editor.nav_tx`. Ends when the render-side channel closes.
fn spawn_nav_bridge(
    client_rx: DaemonClientWatch,
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
            let client = client_rx.borrow().clone();
            let _ = client.send(msg).await;
        }
    });
}

/// The render-side sinks the daemon terminal stream feeds: per-pane output and
/// full-layout snapshots. Held by [`consume_daemon_messages`] in daemon mode; the
/// reverse path (input, resize, commands, capture) runs through the bridge tasks.
struct TerminalSinks {
    pane_output_tx: flume::Sender<PaneOutput>,
    snapshot_tx: flume::Sender<SessionSnapshot>,
    capture_result_tx: flume::Sender<CaptureResult>,
    key_table_result_tx: flume::Sender<KeyTableQueryResult>,
    session_list_tx: flume::Sender<Vec<SessionListItem>>,
}

/// Forward typed input from the render layer onto the protocol as
/// [`rift_protocol::ClientMessage::Input`]; the daemon replays it to the pane via
/// `send-keys -H` (opaque bytes, agent-agnostic). Ends when the render-side
/// channel closes.
fn spawn_input_bridge(client_rx: DaemonClientWatch, input_rx: flume::Receiver<PaneInput>) {
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
            let client = client_rx.borrow().clone();
            let _ = client.send(msg).await;
        }
    });
}

/// Forward client viewport resizes onto the protocol as
/// [`rift_protocol::ClientMessage::ResizePane`]; the daemon applies them with
/// `refresh-client -C <cols>x<rows>` (the control client's single viewport, so
/// `pane_id` is unused there — any value carries). Every grid is also cached
/// on `viewport_tx` — unconditionally, even when the send drops mid-outage —
/// so the recovery engine can re-assert the latest known size after a
/// re-Attach (#475): the render layer itself only re-sends on a size
/// *change*, which a reconnect is not.
fn spawn_resize_bridge(
    client_rx: DaemonClientWatch,
    size_rx: flume::Receiver<TermSize>,
    viewport_tx: tokio::sync::watch::Sender<Option<TermSize>>,
) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while let Ok(size) = size_rx.recv_async().await {
            viewport_tx.send_replace(Some(size));
            let msg = ClientMessage::ResizePane {
                pane_id: 0,
                cols: size.cols as u16,
                rows: size.rows as u16,
            };
            let client = client_rx.borrow().clone();
            let _ = client.send(msg).await;
        }
    });
}

/// Forward raw tmux commands (the session view's window/pane affordances) onto
/// the protocol as [`rift_protocol::ClientMessage::TmuxCommand`]; the daemon runs
/// them verbatim. A command that could mutate the mirrored key table or the
/// prefix/repeat options (`keytable::mutates_bindings`) is followed, on this
/// same task, by the matching key-table refresh request — sequential `send`s on
/// one task land in program order on the shared write queue, so the refresh is
/// guaranteed to reach the daemon after the mutation it is refreshing for.
/// Issuing a refresh from a separate channel/task (as the render layer used
/// to) gave no such ordering guarantee.
fn spawn_command_bridge(client_rx: DaemonClientWatch, cmd_rx: flume::Receiver<String>) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while let Ok(cmd) = cmd_rx.recv_async().await {
            debug!(cmd = %cmd, "sending tmux command (daemon)");
            let refresh_key_table = rift_terminal::keytable::mutates_bindings(&cmd);
            let client = client_rx.borrow().clone();
            if client
                .send(ClientMessage::TmuxCommand { cmd })
                .await
                .is_err()
            {
                // Dropped mid-outage; skip the follow-up refresh too — the
                // daemon re-queries the key table unprompted on the
                // reconnect's re-Attach.
                continue;
            }
            if refresh_key_table {
                let _ = client.send(ClientMessage::QueryKeyTable).await;
            }
        }
    });
}

/// Forward pre-attach scrollback (`capture-pane`) requests onto the protocol as
/// [`rift_protocol::ClientMessage::CapturePane`]; the daemon issues `capture-pane
/// -p -e` and replies with a `PaneCapture` that the consumer routes back to the
/// originating pane as a [`CaptureResult`]. The render-side `start_row`/`end_row`
/// tmux line addresses and the `-J` flag cross the seam unchanged.
fn spawn_capture_bridge(client_rx: DaemonClientWatch, capture_rx: flume::Receiver<CaptureRequest>) {
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
            let client = client_rx.borrow().clone();
            let _ = client.send(msg).await;
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
fn spawn_key_table_bridge(client_rx: DaemonClientWatch, key_table_request_rx: flume::Receiver<()>) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while key_table_request_rx.recv_async().await.is_ok() {
            let client = client_rx.borrow().clone();
            let _ = client.send(ClientMessage::QueryKeyTable).await;
        }
    });
}

/// Forward session-list refresh requests (`docs/spec-session-switch.md`) onto
/// the protocol as [`rift_protocol::ClientMessage::QuerySessionList`]; the
/// daemon answers with a `SessionListReply` that returns through
/// [`consume_daemon_messages`] on `sinks.session_list_tx`. Only the switcher's
/// on-open refresh rides here — the daemon re-queries on session churn by
/// itself and pushes the result unprompted. A request dropped mid-outage is
/// harmless: the reconnect's re-Attach makes the daemon push a fresh list.
/// Ends when the render-side channel closes.
fn spawn_session_list_bridge(
    client_rx: DaemonClientWatch,
    session_list_request_rx: flume::Receiver<()>,
) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while session_list_request_rx.recv_async().await.is_ok() {
            let client = client_rx.borrow().clone();
            let _ = client.send(ClientMessage::QuerySessionList).await;
        }
    });
}

/// Forward cockpit switches from the session switcher onto the protocol as
/// [`rift_protocol::ClientMessage::Attach`] (`docs/spec-session-switch.md`): a
/// second `Attach` on the same connection performs a clean control-child swap
/// and answers with a fresh `LayoutSnapshot` the render layer resets on. The
/// request's grid size is re-asserted as a `ResizePane` on this same task —
/// sequential sends land in program order, so it reaches the daemon strictly
/// after the `Attach` and sizes the fresh child (the render layer's own resize
/// channel only fires on a size *change*, which a switch is not). Each sent
/// switch also records the new name on `session_tx`, so a later stream
/// recovery re-attaches to the session the client is actually on (#475); a
/// switch dropped mid-outage stays untracked — dropped, never buffered — and
/// the resync restores the last session the daemon was actually asked for.
/// Ends when the render-side channel closes.
fn spawn_session_switch_bridge(
    client_rx: DaemonClientWatch,
    session_switch_rx: flume::Receiver<SessionSwitchRequest>,
    session_tx: tokio::sync::watch::Sender<String>,
) {
    use rift_protocol::ClientMessage;
    tokio::spawn(async move {
        while let Ok(req) = session_switch_rx.recv_async().await {
            debug!(session = %req.session, "sending session switch attach");
            let client = client_rx.borrow().clone();
            if client
                .send(ClientMessage::Attach {
                    session: req.session.clone(),
                })
                .await
                .is_err()
            {
                // Dropped mid-outage; skip the viewport re-assert and the
                // session-track update too — the daemon never saw the switch.
                continue;
            }
            session_tx.send_replace(req.session);
            let resize = ClientMessage::ResizePane {
                pane_id: 0,
                cols: req.size.cols as u16,
                rows: req.size.rows as u16,
            };
            let _ = client.send(resize).await;
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

/// Build the render layer's [`SessionSnapshot`] (a `termy_terminal_ui::TmuxSnapshot`
/// paired with the per-pane `is_shell` flags termy's type cannot carry, #510) from
/// the daemon's protocol layout. The render `apply_snapshot` replaces its whole
/// model from this, so a `LayoutSnapshot` and a `LayoutUpdate` map identically. Window and
/// pane ids take tmux's native `@N` / `%N` form, matching the command targets the
/// session view embeds and the `%N` pane ids on `PaneOutput`. The tab number
/// (`TmuxWindowState::index`) carries the daemon's real tmux `window_index`
/// (#495) rather than the window's array position, so a closed window's gap in
/// the numbering matches `tmux list-windows` instead of silently collapsing.
/// Per-pane CWD/command ride the daemon layout query (#442) and refresh on its
/// coalesced re-query cadence; on the legacy path they stay subscription-driven.
fn layout_to_snapshot(
    session: String,
    windows: Vec<rift_protocol::WindowLayout>,
) -> SessionSnapshot {
    use termy_terminal_ui::{TmuxPaneState, TmuxSnapshot, TmuxWindowState};

    // termy's `TmuxPaneState` cannot carry the daemon's `is_shell` flag (#510),
    // so it rides beside the snapshot keyed by the same tmux pane id (`%N`) the
    // render layer targets. Built before `windows` is consumed below.
    let pane_is_shell = windows
        .iter()
        .flat_map(|window| window.panes.iter())
        .map(|pane| (format!("%{}", pane.pane_id), pane.is_shell))
        .collect();

    let windows = windows
        .into_iter()
        .map(|window| {
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
                    current_path: pane.current_path,
                    current_command: pane.current_command,
                })
                .collect();
            TmuxWindowState {
                id: window_id,
                // tmux's real window index (#495): display only, so a closed
                // window's gap in the numbering (`renumber-windows` off, the
                // default) shows up here too — window selection targets the
                // `@N` id, never this.
                index: window.window_index as i32,
                name: window.name,
                layout: String::new(),
                is_active: window.active,
                automatic_rename: false,
                active_pane_id,
                panes,
            }
        })
        .collect();

    SessionSnapshot {
        snapshot: TmuxSnapshot {
            session_name: session,
            session_id: None,
            windows,
        },
        pane_is_shell,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        drain_render_backlog, is_retryable_session_error, layout_to_snapshot, CaptureRequest,
        EditorChannels, EngineWatches, PaneInput, PtyChannels, SessionSwitchRequest, TermSize,
    };

    /// The engine-side ends ([`PtyChannels`] / [`EditorChannels`] /
    /// [`EngineWatches`]) plus every render-side sender the backlog drain can
    /// receive from, wired exactly like the production channel setup.
    struct BacklogHarness {
        ch: PtyChannels,
        editor: EditorChannels,
        watches: EngineWatches,
        input_tx: flume::Sender<PaneInput>,
        size_tx: flume::Sender<TermSize>,
        command_tx: flume::Sender<String>,
        capture_tx: flume::Sender<CaptureRequest>,
        key_table_tx: flume::Sender<()>,
        session_list_tx: flume::Sender<()>,
        switch_tx: flume::Sender<SessionSwitchRequest>,
        open_file_tx: flume::Sender<String>,
        save_file_tx: flume::Sender<rift_protocol::ClientMessage>,
        buffer_change_tx: flume::Sender<rift_protocol::ClientMessage>,
        nav_tx: flume::Sender<rift_protocol::ClientMessage>,
        request_diff_tx: flume::Sender<String>,
        git_op_tx: flume::Sender<rift_protocol::ClientMessage>,
        file_op_tx: flume::Sender<rift_protocol::ClientMessage>,
    }

    fn backlog_harness() -> BacklogHarness {
        let (pane_output_tx, _) = flume::unbounded();
        let (input_tx, input_rx) = flume::unbounded();
        let (size_tx, size_changed_rx) = flume::unbounded();
        let (snapshot_tx, _) = flume::unbounded();
        let (command_tx, tmux_command_rx) = flume::unbounded();
        let (subscription_tx, _) = flume::unbounded();
        let (capture_tx, capture_request_rx) = flume::unbounded();
        let (capture_result_tx, _) = flume::unbounded();
        let (connection_status_tx, _) = flume::unbounded();
        let (key_table_tx, key_table_request_rx) = flume::unbounded();
        let (key_table_result_tx, _) = flume::unbounded();
        let (session_list_result_tx, _) = flume::unbounded();
        let (session_list_tx, session_list_request_rx) = flume::unbounded();
        let (switch_tx, session_switch_rx) = flume::unbounded();
        let (worktree_tx, _) = flume::unbounded();
        let (buffer_tx, _) = flume::unbounded();
        let (nav_reply_tx, _) = flume::unbounded();
        let (lsp_status_tx, _) = flume::unbounded();
        let (open_file_tx, open_file_rx) = flume::unbounded();
        let (save_file_tx, save_file_rx) = flume::unbounded();
        let (buffer_change_tx, buffer_change_rx) = flume::unbounded();
        let (nav_tx, nav_request_rx) = flume::unbounded();
        let (diff_tx, _) = flume::unbounded();
        let (request_diff_tx, request_diff_rx) = flume::unbounded();
        let (git_op_tx, git_op_rx) = flume::unbounded();
        let (file_op_tx, file_op_rx) = flume::unbounded();
        let (file_op_result_tx, _) = flume::unbounded();
        let (daemon_unavailable_tx, _) = flume::unbounded();
        BacklogHarness {
            ch: PtyChannels {
                pane_output_tx,
                input_rx,
                size_changed_rx,
                snapshot_tx,
                tmux_command_rx,
                subscription_tx,
                capture_request_rx,
                capture_result_tx,
                connection_status_tx,
                key_table_request_rx,
                key_table_result_tx,
                session_list_tx: session_list_result_tx,
                session_list_request_rx,
                session_switch_rx,
            },
            editor: EditorChannels {
                worktree_tx,
                buffer_tx,
                nav_tx: nav_reply_tx,
                lsp_status_tx,
                open_file_rx,
                save_file_rx,
                buffer_change_rx,
                nav_request_rx,
                diff_tx,
                request_diff_rx,
                git_op_rx,
                file_op_rx,
                file_op_result_tx,
                daemon_unavailable_tx,
            },
            watches: EngineWatches {
                session: tokio::sync::watch::channel("rift".to_string()).0,
                viewport: tokio::sync::watch::channel(None::<TermSize>).0,
            },
            input_tx,
            size_tx,
            command_tx,
            capture_tx,
            key_table_tx,
            session_list_tx,
            switch_tx,
            open_file_tx,
            save_file_tx,
            buffer_change_tx,
            nav_tx,
            request_diff_tx,
            git_op_tx,
            file_op_tx,
        }
    }

    #[test]
    fn test_drain_render_backlog_queued_backlog_discarded() {
        let h = backlog_harness();
        h.input_tx
            .send(PaneInput {
                pane_id: "%1".into(),
                bytes: b"stale keystrokes\r".to_vec(),
            })
            .expect("send input");
        h.command_tx
            .send("kill-pane -t %1".into())
            .expect("send command");
        h.capture_tx
            .send(CaptureRequest {
                pane_id: "%1".into(),
                start_row: "-".into(),
                end_row: "-1".into(),
                join_wraps: false,
            })
            .expect("send capture");
        h.key_table_tx.send(()).expect("send key-table refresh");
        h.session_list_tx
            .send(())
            .expect("send session-list refresh");
        h.switch_tx
            .send(SessionSwitchRequest {
                session: "elsewhere".into(),
                size: TermSize { cols: 80, rows: 24 },
            })
            .expect("send switch");
        h.open_file_tx
            .send("src/main.rs".into())
            .expect("send open");
        h.save_file_tx
            .send(rift_protocol::ClientMessage::SaveFile {
                path: "src/main.rs".into(),
                content: "fn main() {}\n".into(),
                base_mtime: std::time::SystemTime::UNIX_EPOCH,
            })
            .expect("send save");
        h.buffer_change_tx
            .send(rift_protocol::ClientMessage::BufferClosed {
                path: "src/main.rs".into(),
            })
            .expect("send buffer change");
        h.nav_tx
            .send(rift_protocol::ClientMessage::DefinitionRequest {
                id: rift_protocol::NavRequestId(1),
                path: "src/main.rs".into(),
                position: rift_protocol::Position {
                    line: 0,
                    character: 0,
                },
            })
            .expect("send nav");
        h.request_diff_tx
            .send("src/main.rs".into())
            .expect("send diff request");
        h.git_op_tx
            .send(rift_protocol::ClientMessage::StageFile {
                path: "src/main.rs".into(),
            })
            .expect("send git op");
        h.file_op_tx
            .send(rift_protocol::ClientMessage::RenamePath {
                from: "src/main.rs".into(),
                to: "src/lib.rs".into(),
            })
            .expect("send file op");

        drain_render_backlog(&h.ch, &h.editor, &h.watches);

        assert!(h.ch.input_rx.is_empty());
        assert!(h.ch.tmux_command_rx.is_empty());
        assert!(h.ch.capture_request_rx.is_empty());
        assert!(h.ch.key_table_request_rx.is_empty());
        assert!(h.ch.session_list_request_rx.is_empty());
        assert!(h.ch.session_switch_rx.is_empty());
        assert!(h.editor.open_file_rx.is_empty());
        assert!(h.editor.save_file_rx.is_empty());
        assert!(h.editor.buffer_change_rx.is_empty());
        assert!(h.editor.nav_request_rx.is_empty());
        assert!(h.editor.request_diff_rx.is_empty());
        assert!(h.editor.git_op_rx.is_empty());
        assert!(h.editor.file_op_rx.is_empty());
    }

    #[test]
    fn test_drain_render_backlog_latest_size_folded_into_viewport_watch() {
        let h = backlog_harness();
        h.size_tx
            .send(TermSize {
                cols: 120,
                rows: 40,
            })
            .expect("send size");
        h.size_tx
            .send(TermSize {
                cols: 200,
                rows: 60,
            })
            .expect("send size");

        drain_render_backlog(&h.ch, &h.editor, &h.watches);

        assert_eq!(
            *h.watches.viewport.borrow(),
            Some(TermSize {
                cols: 200,
                rows: 60
            })
        );
        assert!(h.ch.size_changed_rx.is_empty());
    }

    #[test]
    fn test_drain_render_backlog_no_queued_size_keeps_viewport_watch() {
        let h = backlog_harness();
        drain_render_backlog(&h.ch, &h.editor, &h.watches);
        assert_eq!(*h.watches.viewport.borrow(), None);
    }

    #[test]
    fn test_drain_render_backlog_dropped_switch_leaves_session_watch_untouched() {
        // A switch queued mid-outage never reached the daemon: it is dropped,
        // never buffered, and must not move the session watch (#475 drop
        // semantics) — the reconnect restores the last session actually asked
        // of the daemon.
        let h = backlog_harness();
        h.switch_tx
            .send(SessionSwitchRequest {
                session: "elsewhere".into(),
                size: TermSize { cols: 80, rows: 24 },
            })
            .expect("send switch");

        drain_render_backlog(&h.ch, &h.editor, &h.watches);

        assert_eq!(*h.watches.session.borrow(), "rift");
    }

    #[test]
    fn test_is_retryable_session_error_transport_chain_returns_true() {
        let error = anyhow::Error::new(rift_ssh::SshError::Connection(
            "connection reset by peer".into(),
        ))
        .context("SSH connection failed");
        assert!(is_retryable_session_error(&error));
    }

    #[test]
    fn test_is_retryable_session_error_auth_chain_returns_false() {
        let error = anyhow::Error::new(rift_ssh::SshError::Auth(
            "public key authentication failed".into(),
        ))
        .context("SSH connection failed");
        assert!(!is_retryable_session_error(&error));
    }

    #[test]
    fn test_is_retryable_session_error_plain_message_returns_false() {
        // A typeless session error (e.g. the mid-session protocol version
        // mismatch, #475) must not re-enter the pipeline: retrying would
        // re-run the stale-daemon replacement against a daemon another live
        // client owns.
        let error = anyhow::anyhow!("daemon protocol version changed mid-session");
        assert!(!is_retryable_session_error(&error));
    }

    #[test]
    fn test_is_retryable_session_error_nested_transport_chain_returns_true() {
        // The daemon recovery's give-up carries its last transport error as
        // the source, so the engine classifies the whole outage as retryable.
        let error = anyhow::Error::new(rift_ssh::SshError::Channel("channel closed".into()))
            .context("daemon stream reconnect gave up after 10 attempts");
        assert!(is_retryable_session_error(&error));
    }

    fn window_layout(window_id: u32, window_index: u32) -> rift_protocol::WindowLayout {
        rift_protocol::WindowLayout {
            window_id,
            window_index,
            name: format!("win-{window_id}"),
            active: false,
            panes: vec![rift_protocol::PaneLayout {
                pane_id: window_id,
                active: true,
                left: 0,
                top: 0,
                width: 80,
                height: 24,
                current_path: String::new(),
                current_command: String::new(),
                is_shell: false,
            }],
        }
    }

    #[test]
    fn test_layout_to_snapshot_uses_real_window_index_not_array_position() {
        // Closing window 1 leaves a gap in tmux's own numbering
        // (`renumber-windows` off, the default): the daemon layout carries
        // window_index 0 and 2, not the contiguous 0 and 1 an array-position
        // stand-in would produce (#495).
        let windows = vec![window_layout(0, 0), window_layout(5, 2)];

        let snapshot = layout_to_snapshot("rift".to_owned(), windows);

        let indices: Vec<i32> = snapshot.snapshot.windows.iter().map(|w| w.index).collect();
        assert_eq!(
            indices,
            vec![0, 2],
            "the tab index must be the real tmux window_index, not array position"
        );
    }

    #[test]
    fn test_layout_to_snapshot_maps_is_shell_by_pane_id() {
        // Each pane's daemon-evaluated `is_shell` flag (#510) must ride the
        // paired map keyed by the same `%N` pane id the render layer targets,
        // since termy's `TmuxPaneState` cannot carry it.
        let mut window = window_layout(0, 0);
        window.panes[0].is_shell = true;
        window.panes.push(rift_protocol::PaneLayout {
            pane_id: 7,
            active: false,
            left: 0,
            top: 0,
            width: 80,
            height: 24,
            current_path: String::new(),
            current_command: "vim".to_owned(),
            is_shell: false,
        });

        let snapshot = layout_to_snapshot("rift".to_owned(), vec![window]);

        assert_eq!(snapshot.pane_is_shell.get("%0"), Some(&true));
        assert_eq!(snapshot.pane_is_shell.get("%7"), Some(&false));
    }

    #[test]
    fn test_gpui_component_icon_asset_is_embedded_in_product_build() {
        use gpui::AssetSource as _;
        use gpui_component_assets::Assets;

        // The activity-rail and window-control glyphs resolve through
        // gpui-component-assets. Guard that its icon source is wired into the
        // product build (not just the `gallery` feature) and that its
        // `icons/<name>.svg` layout still resolves, so the blank-icon regression
        // (#597) stays fixed.
        let icon = Assets
            .load("icons/folder.svg")
            .expect("icon asset load must not error")
            .expect("gpui-component icon must be embedded in the product build");
        assert!(!icon.is_empty(), "embedded icon SVG must not be empty");
    }

    #[test]
    fn test_rift_file_icon_asset_is_embedded_in_product_build() {
        use gpui::AssetSource as _;

        use super::RiftAssets;

        // A vendored file-type glyph (`crates/app/assets/file_icons/rust.svg`)
        // must resolve through the delegating RiftAssets source registered by
        // `main` (#668), so the explorer's icon slot can render real file-type
        // glyphs in the shipping binary, not only under the dev-only `gallery`
        // feature.
        let icon = RiftAssets
            .load("file_icons/rust.svg")
            .expect("file-type icon asset load must not error")
            .expect("vendored file-type icon must be embedded in the product build");
        assert!(
            !icon.is_empty(),
            "embedded file-type icon SVG must not be empty"
        );
    }

    #[test]
    fn test_rift_newly_vendored_file_icon_asset_is_embedded_in_product_build() {
        use gpui::AssetSource as _;

        use super::RiftAssets;

        // A file-type glyph vendored by the industry-standard mapping
        // broadening (`docs/spec-explorer-polish.md`, issue #712) must
        // resolve through the same delegating RiftAssets source as the
        // pre-existing set, so the broadened mapping renders real glyphs in
        // the shipping binary too, not only under the dev-only `gallery`
        // feature.
        let icon = RiftAssets
            .load("file_icons/typescript.svg")
            .expect("file-type icon asset load must not error")
            .expect("newly vendored file-type icon must be embedded in the product build");
        assert!(
            !icon.is_empty(),
            "embedded file-type icon SVG must not be empty"
        );
    }
}
