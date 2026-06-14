//! Code editor surface: open a file from the tree into a `gpui-component` code
//! editor, render it with Tree-sitter syntax highlighting, and write edits back
//! over the buffer channel with concurrent-write handling (`docs/spec-editor.md`,
//! the editor surface + write-back; #187, #188).
//!
//! This is the **client side of the buffer channel** (the protocol's first
//! request/response pair). The [`crate::workspace::WorkspaceView`] subscribes to
//! the file tree's [`crate::file_tree::FileTreeEvent::OpenFile`], issues an
//! `OpenFile` request over the daemon transport, and routes the
//! `FileContent { path, content, mtime }` reply back here via [`EditorView::load`].
//! The editor renders `content` byte-for-byte in a `gpui-component` `InputState`
//! in code-editor mode, deriving the highlighting language from the file
//! extension. The component's editor virtualizes (it paints only visible lines),
//! so a large file opens and scrolls without loading every line eagerly.
//!
//! ## Write-back (#188)
//!
//! The buffer is editable; [`Save`] (bound to `Ctrl+S` / `Cmd+S` in `main.rs`,
//! scoped to the editor's key context) sends the whole buffer back as
//! `SaveFile { path, content, base_mtime }`, carrying the base `mtime` the open
//! kept. The daemon replies with one of:
//!
//! - `SaveResult { mtime }` — the write landed; [`EditorView::apply_save_result`]
//!   adopts the new `mtime` as the buffer's base and marks the buffer clean.
//! - `SaveConflict { disk_mtime }` — the file changed under the editor since it
//!   was read, so the daemon refused the write rather than clobber the newer
//!   on-disk version; [`EditorView::apply_save_conflict`] surfaces the conflict
//!   **without losing the buffer** (the user's edits stay; no merge UI — the
//!   depth is detect + a reload-or-keep choice).
//!
//! ## Concurrent external change (#188)
//!
//! When an agent (in a pane) edits a file open in the editor, the worktree
//! snapshot's per-entry `mtime` (#107) is the "changed under you" signal — the
//! same `SystemTime` the buffer's base `mtime` is, compared directly (never an
//! independently sampled stat). [`EditorView::note_external_change`] runs the
//! pure [`decide_external_change`] decision on the open path's snapshot `mtime`:
//!
//! - a **clean** buffer + a newer snapshot `mtime` → silent auto-reload (re-issue
//!   `OpenFile`; the editor watches the agent's edit live), and
//! - a **dirty** buffer + a newer snapshot `mtime` → a surfaced conflict, losing
//!   neither side.
//!
//! ## Timeout, not a hang
//!
//! A daemon refusal (binary / non-UTF-8 content, a path that escapes the root)
//! produces *no reply* — the daemon stays silent and logs to its stderr
//! (`crates/daemon/src/lib.rs::buffer_reply`). So both the open **and the save**
//! carry their own bounded timeouts ([`OPEN_TIMEOUT`] / [`SAVE_TIMEOUT`]): if no
//! reply arrives the editor recovers to an unobtrusive state rather than waiting
//! forever. An open reply is matched to the live open **by path** ([`EditorView::load`]
//! only accepts a `FileContent` whose path is the one currently `Loading`); a
//! save reply is matched by path too. Each timeout is fenced by a **monotonic
//! generation** (one for opens, one for saves), so a fired timer for a request
//! that has since been superseded never trips the current one.
//!
//! Opening, editing, or saving a file touches no tmux pane or window state — the
//! editor is a GUI surface, not a pane — and nothing here inspects pane
//! processes, agents, or editor processes (agent-agnostic by construction; it
//! only ever handles a file path, its bytes, and its `mtime`).

use std::path::Path;
use std::time::{Duration, SystemTime};

use flume::Sender;
use gpui::{
    div, px, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::ActiveTheme as _;
use rift_protocol::ClientMessage;

/// The save action: write the open buffer back to the remote. Dispatched from the
/// editor's key context, bound to `Ctrl+S` / `Cmd+S` in `main.rs`. A bare
/// unit-struct action (no payload — the editor knows its own open path).
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct Save;

/// The GPUI key context the editor establishes around its input, so the [`Save`]
/// binding is scoped to the editor surface and never fires for an unrelated input
/// (e.g. the gallery's search box).
pub const EDITOR_KEY_CONTEXT: &str = "Editor";

/// How long the editor waits for a `FileContent` reply before giving up on an
/// open. The daemon answers a local read in well under this; the budget exists
/// only so a *refused* request (which gets no reply, by protocol) or a lost one
/// cannot wedge the editor. Generous enough not to trip on a slow link, short
/// enough to recover promptly.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the editor waits for a `SaveResult` / `SaveConflict` reply before
/// giving up on a save. Like the open path, a *refused* save (a path escape, a
/// non-UTF-8 surprise on the daemon side) draws no reply by protocol, so the save
/// recovers to a clear "save failed" surface rather than leaving a spinner up
/// forever. Same budget as the open — a local write answers far quicker.
const SAVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Default tab width for the code editor, matching the gallery demo.
const TAB_SIZE: usize = 4;

/// What the editor is currently showing.
enum EditorState {
    /// No file opened yet — the initial empty surface.
    Empty,
    /// An open request is in flight, awaiting its `FileContent` reply (or the
    /// timeout). Carries the path being opened so a stale reply can be told from
    /// the live one.
    Loading { path: String },
    /// A file's content is rendered in the code editor.
    Loaded { path: String },
    /// The last open did not complete — the daemon refused it (binary / path
    /// escape, which gets no reply) or the reply was lost and the timeout fired.
    /// An unobtrusive recovery state, never a hang.
    Failed { path: String },
}

/// The transient outcome of the most recent save, surfaced as a one-line banner
/// over the editor. Independent of [`EditorState`] (the buffer stays loaded and
/// editable throughout) so a conflict never tears down or discards the buffer.
enum SaveState {
    /// No save in flight or just completed cleanly — nothing to surface.
    Idle,
    /// A `SaveFile` is in flight, awaiting its reply (or the timeout).
    Saving,
    /// The daemon refused the save (`SaveConflict`): the file changed on disk
    /// under the editor. The buffer is untouched; the user chooses reload-or-keep.
    /// `disk_mtime` is the current on-disk value the conflict reported (kept for
    /// the headless handle).
    Conflict { disk_mtime: SystemTime },
    /// The save drew no reply within [`SAVE_TIMEOUT`] (refused or lost). The
    /// buffer is untouched; the user may retry.
    Failed,
}

/// The decision an external (snapshot-`mtime`) change to the open path forces.
///
/// The pure core of the concurrent-write handling — computed by
/// [`decide_external_change`] from three inputs and nothing else, so it is unit-
/// testable without GPUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalChange {
    /// The snapshot `mtime` is not newer than the buffer's base — nothing changed
    /// under the editor (or the snapshot is the very `mtime` the buffer is based
    /// on). Do nothing.
    None,
    /// The file changed under a **clean** buffer: silently auto-reload it (re-issue
    /// `OpenFile`) so the editor shows the agent's edit live.
    Reload,
    /// The file changed under a **dirty** buffer: surface a conflict, keeping the
    /// buffer — never silently discard the user's unsaved edits.
    Conflict,
}

/// Decide what an external change to the open path forces, comparing the buffer's
/// `base` `mtime` against the worktree snapshot's `mtime` for that path.
///
/// This is the load-bearing concurrent-write rule (`docs/spec-editor.md`):
///
/// - `snapshot <= base` → [`ExternalChange::None`]: the on-disk file is not newer
///   than what the buffer is based on, so there is nothing to react to. (Equality
///   is the common case — the snapshot that first delivered the entry carries the
///   same `mtime` the open read.)
/// - `snapshot > base` and the buffer is **clean** → [`ExternalChange::Reload`].
/// - `snapshot > base` and the buffer is **dirty** → [`ExternalChange::Conflict`].
///
/// The comparison is exactly base-vs-snapshot — never an independently sampled
/// stat — because the two `mtime`s are the identical `SystemTime` clock source
/// across the structure and buffer paths (`rift_protocol`, #107).
pub fn decide_external_change(
    base: SystemTime,
    snapshot: SystemTime,
    dirty: bool,
) -> ExternalChange {
    if snapshot <= base {
        return ExternalChange::None;
    }
    if dirty {
        ExternalChange::Conflict
    } else {
        ExternalChange::Reload
    }
}

/// The code editor view: a `gpui-component` `InputState` in code-editor mode plus
/// the open buffer's bookkeeping.
pub struct EditorView {
    /// The code-editor input state. Created once; its content is replaced via
    /// [`InputState::set_value`] each time a file loads, and its highlighting
    /// language is reset per open to match the new file's extension.
    input: Entity<InputState>,
    /// Subscription to the input's `Change` events, so a keystroke marks the
    /// buffer dirty. Recreated alongside `input` on every open. Held to keep the
    /// subscription alive (dropping it cancels it).
    _input_change: Subscription,
    state: EditorState,
    save_state: SaveState,
    /// Whether the buffer has unsaved edits — set on the first `Change` after a
    /// load, cleared on a successful load or save. The concurrent-write detector's
    /// clean/dirty input.
    dirty: bool,
    /// The base `mtime` the daemon reported for the open buffer — handed back as
    /// `SaveFile`'s `base_mtime` and compared against the worktree snapshot's
    /// `mtime` to detect a concurrent external change. `None` until a file loads.
    base_mtime: Option<SystemTime>,
    /// Read-request sender: re-issuing an `OpenFile` for a clean auto-reload goes
    /// through here (the same channel the workspace's tree-open path uses), so the
    /// daemon's `FileContent` reply rebases the buffer.
    open_file_tx: Sender<String>,
    /// Write-request sender: a [`Save`] turns the buffer into a `SaveFile` on this
    /// channel, which the tokio side emits as `ClientMessage::SaveFile`.
    save_file_tx: Sender<ClientMessage>,
    /// Monotonic open-request id. Incremented on every [`EditorView::begin_open`];
    /// the timeout and the reply both carry the generation they were issued for,
    /// so a late reply or a fired timer for a superseded open is ignored.
    generation: u64,
    /// Monotonic save-request id, fenced independently of `generation`: a fired
    /// save timeout for a save that has since been answered (or superseded by a
    /// later save) never trips the current one.
    save_generation: u64,
}

impl EditorView {
    /// Create an empty editor. The `InputState` starts in code-editor mode with a
    /// neutral language; [`EditorView::begin_open`] re-derives the language from
    /// each opened file's extension before its content loads.
    ///
    /// `open_file_tx` re-issues an `OpenFile` for a clean auto-reload, and
    /// `save_file_tx` carries a `SaveFile` for a save — both threaded to the tokio
    /// side by `main.rs`.
    pub fn new(
        open_file_tx: Sender<String>,
        save_file_tx: Sender<ClientMessage>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("text")
                .line_number(true)
                .tab_size(gpui_component::input::TabSize {
                    tab_size: TAB_SIZE,
                    ..Default::default()
                })
        });
        let input_change = Self::subscribe_dirty(&input, cx);
        Self {
            input,
            _input_change: input_change,
            state: EditorState::Empty,
            save_state: SaveState::Idle,
            dirty: false,
            base_mtime: None,
            open_file_tx,
            save_file_tx,
            generation: 0,
            save_generation: 0,
        }
    }

    /// Subscribe to an input's `Change` event so an edit marks the buffer dirty.
    /// A programmatic `set_value` (load / auto-reload) also emits `Change`, so the
    /// dirty flag is cleared right after those calls — see [`EditorView::load`].
    fn subscribe_dirty(input: &Entity<InputState>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(input, |this, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && !this.dirty {
                this.dirty = true;
                cx.notify();
            }
        })
    }

    /// The path of the file currently open or loading, if any — a headless handle
    /// for tests and for the workspace to correlate a reply.
    pub fn open_path(&self) -> Option<&str> {
        match &self.state {
            EditorState::Loading { path }
            | EditorState::Loaded { path }
            | EditorState::Failed { path } => Some(path.as_str()),
            EditorState::Empty => None,
        }
    }

    /// The base `mtime` of the open buffer — what write-back hands back as
    /// `SaveFile`'s `base_mtime`. `None` until a file has loaded.
    pub fn base_mtime(&self) -> Option<SystemTime> {
        self.base_mtime
    }

    /// Whether the buffer has unsaved edits — the concurrent-write detector's
    /// clean/dirty input and a headless test handle.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Whether the editor is currently surfacing a save conflict (a `SaveConflict`
    /// reply, or a dirty-buffer concurrent external change) — a headless handle
    /// for the conflict assertions.
    pub fn has_conflict(&self) -> bool {
        matches!(self.save_state, SaveState::Conflict { .. })
    }

    /// Begin opening `path`: switch the editor to its loading state, point the
    /// code editor at the file's language (derived from the extension), and arm
    /// the open timeout. The caller sends the matching `OpenFile` request; the
    /// reply is accepted by [`EditorView::load`] only when its path matches the
    /// open in flight.
    ///
    /// The content is *not* set here — it arrives on the `FileContent` reply. The
    /// editor is cleared to empty content meanwhile so a previous file's text is
    /// never shown under the new path.
    pub fn begin_open(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;

        let language = language_for_path(&path);
        let path_for_timer = path.clone();
        self.state = EditorState::Loading { path };
        self.save_state = SaveState::Idle;
        self.base_mtime = None;
        self.dirty = false;

        // Recreate the input in the new file's language, which also clears the
        // previous file's text and highlighter — so no stale content or coloring
        // lingers under the new path while its content loads. The freshly built
        // state starts empty; the `FileContent` reply fills it in `load`.
        self.input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(language)
                .line_number(true)
                .tab_size(gpui_component::input::TabSize {
                    tab_size: TAB_SIZE,
                    ..Default::default()
                })
        });
        self._input_change = Self::subscribe_dirty(&self.input, cx);

        // Arm the timeout on the GPUI executor (`smol::Timer`; tokio's timer does
        // not fire here — docs/patterns.md). If, when it fires, this is still the
        // live request and still loading, the reply never came (refused / lost):
        // fall back to the failed state rather than wait forever.
        cx.spawn(async move |this, cx| {
            smol::Timer::after(OPEN_TIMEOUT).await;
            // `AsyncApp::update` returns its closure's value (`()` here), not a
            // `Result`; the inner `WeakEntity::update` `Result` is discarded (a
            // dropped view makes the timeout moot). Bare statement, so the unit
            // value is not let-bound (clippy::let_unit_value).
            cx.update(|cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        if let EditorState::Loading { .. } = this.state {
                            this.state = EditorState::Failed {
                                path: path_for_timer.clone(),
                            };
                            cx.notify();
                        }
                    }
                });
            });
        })
        .detach();

        cx.notify();
    }

    /// Render a `FileContent` reply: if it matches the file currently loading,
    /// load its `content` into the code editor and keep its `mtime` as the
    /// buffer's base. A reply whose path does not match the live open (a
    /// superseded or stray reply) is ignored — only the most recent open is live.
    pub fn load(
        &mut self,
        path: String,
        content: String,
        mtime: SystemTime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Accept only the reply for the open currently in flight; a reply for a
        // path we are no longer loading is stale (the user opened another file).
        let matches = matches!(&self.state, EditorState::Loading { path: p } if *p == path);
        if !matches {
            return;
        }

        self.base_mtime = Some(mtime);
        self.input.update(cx, |input, cx| {
            input.set_value(content, window, cx);
        });
        // `set_value` emits `Change`, which would otherwise mark the freshly loaded
        // buffer dirty — clear it after, so a load (and a clean auto-reload) starts
        // clean against the new base.
        self.dirty = false;
        self.save_state = SaveState::Idle;
        self.state = EditorState::Loaded { path };
        cx.notify();
    }

    /// Send the open buffer back to the remote as a `SaveFile`, carrying the base
    /// `mtime` the open kept. Arms the save timeout. A no-op unless a file is
    /// loaded — there is nothing to save from the empty / loading / failed states.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let EditorState::Loaded { path } = &self.state else {
            return;
        };
        let path = path.clone();
        let Some(base_mtime) = self.base_mtime else {
            return;
        };
        let content = self.input.read(cx).value().to_string();

        self.save_generation = self.save_generation.wrapping_add(1);
        let save_generation = self.save_generation;
        self.save_state = SaveState::Saving;

        if let Err(e) = self.save_file_tx.try_send(ClientMessage::SaveFile {
            path: path.clone(),
            content,
            base_mtime,
        }) {
            tracing::debug!(error = %e, %path, "failed to enqueue save request");
            self.save_state = SaveState::Failed;
            cx.notify();
            return;
        }

        // Save timeout, fenced by the save generation: a refused / lost save draws
        // no reply (same protocol as the open path), so recover to the failed
        // surface rather than leave the saving banner up forever.
        cx.spawn(async move |this, cx| {
            smol::Timer::after(SAVE_TIMEOUT).await;
            cx.update(|cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.save_generation == save_generation {
                        if let SaveState::Saving = this.save_state {
                            this.save_state = SaveState::Failed;
                            cx.notify();
                        }
                    }
                });
            });
        })
        .detach();

        cx.notify();
    }

    /// Apply a `SaveResult` reply: the write landed, so adopt the new `mtime` as
    /// the buffer's base and mark it clean. Accepted only when it matches the open
    /// path — a reply for a since-superseded file is ignored.
    pub fn apply_save_result(&mut self, path: String, mtime: SystemTime, cx: &mut Context<Self>) {
        if self.open_path() != Some(path.as_str()) {
            return;
        }
        self.base_mtime = Some(mtime);
        self.dirty = false;
        self.save_state = SaveState::Idle;
        self.save_generation = self.save_generation.wrapping_add(1);
        cx.notify();
    }

    /// Apply a `SaveConflict` reply: the file changed on disk under the editor, so
    /// the daemon refused the write. Surface the conflict **without touching the
    /// buffer** — the user's edits stay; they choose reload-or-keep. Accepted only
    /// when it matches the open path.
    pub fn apply_save_conflict(
        &mut self,
        path: String,
        disk_mtime: SystemTime,
        cx: &mut Context<Self>,
    ) {
        if self.open_path() != Some(path.as_str()) {
            return;
        }
        self.save_state = SaveState::Conflict { disk_mtime };
        self.save_generation = self.save_generation.wrapping_add(1);
        cx.notify();
    }

    /// React to a worktree snapshot reporting `snapshot_mtime` for the open path:
    /// the concurrent-write detector. Compares the buffer's base `mtime` against
    /// the snapshot `mtime` via the pure [`decide_external_change`] and acts on it:
    ///
    /// - [`ExternalChange::None`] → do nothing.
    /// - [`ExternalChange::Reload`] → re-issue `OpenFile` to rebase the clean
    ///   buffer (watch the agent's edit live).
    /// - [`ExternalChange::Conflict`] → surface a conflict, keeping the dirty
    ///   buffer.
    ///
    /// Driven only while a file is loaded; the snapshot `mtime` is the identical
    /// `SystemTime` clock source as the base, compared directly (never a separate
    /// stat).
    pub fn note_external_change(
        &mut self,
        snapshot_mtime: SystemTime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Clone the open path out up front so no immutable borrow of `self.state`
        // is held across the `&mut self` reopen below (borrowck).
        let EditorState::Loaded { path } = &self.state else {
            return;
        };
        let path = path.clone();
        let Some(base) = self.base_mtime else {
            return;
        };
        match decide_external_change(base, snapshot_mtime, self.dirty) {
            ExternalChange::None => {}
            ExternalChange::Reload => {
                // Re-open the same path: `begin_open` arms a fresh load, and the
                // `FileContent` reply rebases the buffer with the agent's content +
                // the new `mtime`. The clean buffer had no edits to lose.
                self.begin_open(path.clone(), window, cx);
                if let Err(e) = self.open_file_tx.try_send(path.clone()) {
                    tracing::debug!(error = %e, %path, "failed to enqueue auto-reload open");
                }
            }
            ExternalChange::Conflict => {
                self.save_state = SaveState::Conflict {
                    disk_mtime: snapshot_mtime,
                };
                cx.notify();
            }
        }
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status: Option<String> = match &self.state {
            EditorState::Empty => Some("Select a file to open".to_owned()),
            EditorState::Loading { path } => Some(format!("Opening {path}\u{2026}")),
            EditorState::Failed { path } => Some(format!("Could not open {path}")),
            EditorState::Loaded { .. } => None,
        };

        if let Some(message) = status {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(8.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message)
                .into_any_element();
        }

        // A one-line banner over the editor for the transient save outcome. The
        // conflict line names the reload-or-keep choice without a modal — the
        // buffer stays editable beneath it (depth = detect + reload/keep, no merge
        // UI). Save again to overwrite-keep once rebased, or re-open to reload.
        let banner: Option<(String, gpui::Hsla)> = match &self.save_state {
            SaveState::Idle => None,
            SaveState::Saving => Some(("Saving\u{2026}".to_owned(), cx.theme().muted_foreground)),
            SaveState::Conflict { .. } => Some((
                "Changed on disk since you opened it \u{2014} re-open to reload, \
                 or save again to keep your version"
                    .to_owned(),
                cx.theme().danger,
            )),
            SaveState::Failed => Some(("Save failed".to_owned(), cx.theme().danger)),
        };

        let editor = div().flex_1().min_h_0().child(
            Input::new(&self.input)
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(cx.theme().mono_font_size)
                .size_full(),
        );

        // The editor establishes its own key context and hosts the `Save` action,
        // so the `Ctrl+S` / `Cmd+S` binding (bound in `main.rs`, scoped here) fires
        // only when focus is within the editor — never for an unrelated input.
        let mut root = div()
            .key_context(EDITOR_KEY_CONTEXT)
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &Save, _window, cx| {
                this.save(cx);
            }));

        if let Some((text, color)) = banner {
            root = root.child(
                div()
                    .flex_shrink_0()
                    .px(px(8.0))
                    .py(px(4.0))
                    .text_xs()
                    .text_color(color)
                    .bg(cx.theme().muted)
                    .child(text),
            );
        }

        root.child(editor).into_any_element()
    }
}

/// Derive the highlighting language token for a path from its extension.
///
/// `gpui-component`'s `code_editor` accepts an extension or a language name and
/// resolves the Tree-sitter grammar itself, falling back to plain text for an
/// unknown one (`Language::from_str`). So the extension is passed straight
/// through; a path with no extension uses `"text"` (plain). The leaf is matched
/// case-insensitively for the extension only — the full path never reaches the
/// editor.
fn language_for_path(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_else(|| "text".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_for_path_uses_extension() {
        assert_eq!(language_for_path("src/main.rs"), "rs");
        assert_eq!(language_for_path("Cargo.toml"), "toml");
        assert_eq!(language_for_path("docs/readme.MD"), "md");
        assert_eq!(language_for_path("a/b/script.py"), "py");
    }

    #[test]
    fn test_language_for_path_without_extension_is_plain_text() {
        assert_eq!(language_for_path("Makefile"), "text");
        assert_eq!(language_for_path("src/noext"), "text");
        assert_eq!(language_for_path(".gitignore"), "text");
    }

    #[test]
    fn test_language_for_path_lowercases_extension() {
        assert_eq!(language_for_path("MAIN.RS"), "rs");
        assert_eq!(language_for_path("Config.TOML"), "toml");
    }

    // --- concurrent-write decision (#188) ---
    //
    // The decision function is pure (base vs snapshot mtime + dirty flag), so the
    // load-bearing concurrent-write rule is verified here without GPUI: a clean
    // buffer auto-reloads on a newer snapshot, a dirty buffer conflicts, and an
    // unchanged (or older) snapshot is a no-op either way.

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn test_clean_buffer_with_newer_snapshot_reloads() {
        // The agent edited the open file; the buffer has no unsaved edits, so the
        // editor silently reloads to show the agent's change live.
        assert_eq!(
            decide_external_change(at(100), at(200), false),
            ExternalChange::Reload
        );
    }

    #[test]
    fn test_dirty_buffer_with_newer_snapshot_conflicts() {
        // The agent edited the open file while the user has unsaved edits: neither
        // side may be lost, so the editor surfaces a conflict instead of reloading.
        assert_eq!(
            decide_external_change(at(100), at(200), true),
            ExternalChange::Conflict
        );
    }

    #[test]
    fn test_equal_snapshot_is_no_change_regardless_of_dirty() {
        // The common case: the snapshot carries the very `mtime` the buffer is
        // based on (the entry that delivered it). Never reacts, clean or dirty —
        // the comparison is base-vs-snapshot, not a fresh stat.
        assert_eq!(
            decide_external_change(at(100), at(100), false),
            ExternalChange::None
        );
        assert_eq!(
            decide_external_change(at(100), at(100), true),
            ExternalChange::None
        );
    }

    #[test]
    fn test_older_snapshot_is_no_change() {
        // A snapshot `mtime` strictly older than the base (e.g. a save just bumped
        // the base ahead of an in-flight snapshot) is no external change.
        assert_eq!(
            decide_external_change(at(200), at(100), false),
            ExternalChange::None
        );
        assert_eq!(
            decide_external_change(at(200), at(100), true),
            ExternalChange::None
        );
    }
}
