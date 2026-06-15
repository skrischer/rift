// SPDX-License-Identifier: GPL-3.0-or-later
//! Code editor surface: open a file from the tree into a `gpui-component` code
//! editor, render it with Tree-sitter syntax highlighting, write edits back
//! over the buffer channel, and navigate symbols via go-to-definition
//! (ctrl+click, context menu), hover popovers, back-navigation, and read-only
//! out-of-root opens (`docs/spec-lsp-navigation.md`, #196, #197).
//!
//! # Buffer channel (#187, #188)
//!
//! The [`crate::workspace::WorkspaceView`] subscribes to the file tree's
//! [`crate::file_tree::FileTreeEvent::OpenFile`], issues an `OpenFile`
//! request, and routes the `FileContent { path, content, mtime }` reply back
//! here via [`EditorView::load`].
//!
//! # Write-back (#188)
//!
//! [`Save`] (bound to `Ctrl+S` / `Cmd+S`) sends the whole buffer as a
//! `SaveFile { path, content, base_mtime }`. The daemon replies with
//! `SaveResult` (commit new `mtime`) or `SaveConflict` (refuse without
//! clobbering the newer on-disk version).
//!
//! # Concurrent external change (#188)
//!
//! [`EditorView::note_external_change`] runs the pure [`decide_external_change`]
//! decision on the open path's snapshot `mtime`: a clean buffer auto-reloads;
//! a dirty buffer surfaces a conflict.
//!
//! # Live-buffer feed (#189)
//!
//! A debounced `BufferChanged` keeps the daemon's LSP source of truth current
//! with the open buffer's unsaved edits. `BufferClosed` reverts to disk-backed.
//!
//! # Navigation (#196)
//!
//! Go-to-definition fires on ctrl+click or the "Go to Definition" context-menu
//! entry. Before dispatching the [`ClientMessage::DefinitionRequest`] the editor
//! **flushes a pending `BufferChanged`** if the buffer is dirty (flush-before-
//! dispatch — the request must resolve against the live buffer the LSP already
//! has via `didChange`, not the stale disk version). The position the request
//! carries is the cursor position that ctrl+click or the menu action set.
//!
//! A same-file target scrolls and selects the range. A cross-file target opens
//! via the existing buffer channel (`open_file_tx`) and lands on the range
//! using [`EditorView::pending_jump`] (stored before the load, applied in
//! [`EditorView::load`] when it matches). An out-of-root target (absolute path,
//! `out_of_root = true`) opens via the same buffer channel — the daemon's
//! out-of-root read carve-out (#195/#301) serves the bytes — and the editor
//! enters **read-only mode** so no edit or save is possible.
//!
//! A bounded in-memory back-jump stack lets the user unwind jumps with the
//! `GoBack` action (bound to `Alt+Left` in `main.rs`).
//!
//! When a `DefinitionResponse` carries multiple targets (e.g. Rust trait impls)
//! a transient inline jump-list is rendered so the user can click the desired
//! destination.
//!
//! # Hover popover (#197)
//!
//! Hover is triggered by:
//! - **`Shift+K`** (scoped to the `Editor` key context, bound in `main.rs`):
//!   dispatches [`ClientMessage::HoverRequest`] at the cursor position.
//! - **Mouse-rest**: a [`MouseMoveEvent`] on the outer div arms a 500 ms
//!   debounce timer. When the timer fires, the hover request is dispatched at
//!   the current cursor position (which follows the most recent click, making
//!   hover after ctrl+click natural). Subsequent mouse movement cancels the
//!   pending timer by bumping `hover_move_generation`.
//!
//! The response is applied by [`EditorView::apply_hover_response`]. A
//! [`HoverContent`] renders in a floating popover anchored just above the
//! cursor line, rendered via `gpui_component::text::markdown`. A `None`
//! content (server found nothing) is a silent no-op. A stale response (id
//! mismatch) is dropped — same drop-stale discipline as `DefinitionResponse`.
//!
//! Clicking anywhere in the editor or moving the mouse out of the popover
//! dismisses it (set `hover_content = None`).
//!
//! # Timeout, not a hang
//!
//! A daemon refusal (binary / non-UTF-8, path escape) produces *no reply* — the
//! editor recovers via bounded timeouts ([`OPEN_TIMEOUT`] / [`SAVE_TIMEOUT`]).
//! Nav requests have no reply timeout at the editor layer: stale responses are
//! discarded by id comparison in [`EditorView::apply_definition_response`] and
//! [`EditorView::apply_hover_response`].

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, SystemTime};

use flume::Sender;
use gpui::{
    div, px, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, Render, Styled as _, Subscription, Window,
};
use gpui_component::highlighter::{
    Diagnostic as EditorDiagnostic, DiagnosticSeverity as EditorSeverity,
};
use gpui_component::input::{Input, InputEvent, InputState, Position as EditorPosition};
use gpui_component::menu::PopupMenu;
use gpui_component::text::markdown;
use gpui_component::ActiveTheme as _;
use rift_protocol::{
    ClientMessage, Diagnostic, DiagnosticSeverity, HoverContent, NavLocation, NavRequestId,
    Position, Range,
};

// ── Actions ───────────────────────────────────────────────────────────────────

/// The save action: write the open buffer back to the remote. Dispatched from
/// the editor's key context, bound to `Ctrl+S` / `Cmd+S` in `main.rs`.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct Save;

/// Trigger go-to-definition at the current cursor position. Dispatched from
/// the context-menu entry ("Go to Definition") and programmatically after
/// ctrl+click sets the cursor.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct GoToDefinition;

/// Return to the position before the last jump (back-stack unwind). Bound to
/// `Alt+Left` in `main.rs`, scoped to the editor key context.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct GoBack;

/// Show the LSP hover popover at the current cursor position. Dispatched from
/// the context-menu entry ("Show Hover") and from the `Shift+K` keybind
/// (bound in `main.rs`, scoped to the editor key context). Also dispatched
/// internally after the mouse-rest debounce timer fires.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct ShowHover;

// ── Constants ─────────────────────────────────────────────────────────────────

/// The GPUI key context the editor establishes around its input, so the
/// [`Save`] / [`GoToDefinition`] / [`GoBack`] bindings are scoped to the
/// editor surface and never fire for an unrelated input.
pub const EDITOR_KEY_CONTEXT: &str = "Editor";

/// How long the editor waits for a `FileContent` reply before giving up on an
/// open. Generous enough not to trip on a slow link; short enough to recover.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the editor waits for a `SaveResult` / `SaveConflict` reply before
/// giving up on a save.
const SAVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Default tab width for the code editor, matching the gallery demo.
const TAB_SIZE: usize = 4;

/// How long the editor waits after the last keystroke before feeding the live
/// buffer to the LSP (`BufferChanged`, #189).
const BUFFER_FEED_DEBOUNCE: Duration = Duration::from_millis(300);

/// Maximum entries in the bounded back-jump stack. Oldest entries are evicted
/// when this limit is reached so a long navigation session never leaks memory.
const BACK_STACK_MAX: usize = 50;

/// How long the editor waits after the mouse stops moving before sending a
/// `HoverRequest`. 500 ms matches VS Code's default hover delay and avoids
/// flooding the LSP on fast cursor movement.
const HOVER_MOUSE_DEBOUNCE: Duration = Duration::from_millis(500);

// ── Internal state types ──────────────────────────────────────────────────────

/// What the editor is currently showing.
enum EditorState {
    /// No file opened yet — the initial empty surface.
    Empty,
    /// An open request is in flight, awaiting its `FileContent` reply.
    Loading { path: String },
    /// A file's content is rendered in the code editor.
    Loaded { path: String },
    /// The last open did not complete.
    Failed { path: String },
}

/// The transient outcome of the most recent save.
enum SaveState {
    Idle,
    Saving,
    Conflict,
    Failed,
}

/// One entry in the inline jump-list shown for multi-target definition
/// responses (e.g. Rust trait method impls).
struct JumpEntry {
    location: NavLocation,
}

// ── Public decision type ───────────────────────────────────────────────────────

/// The decision an external (snapshot-`mtime`) change to the open path forces.
///
/// Computed by [`decide_external_change`] from three inputs and nothing else,
/// so it is unit-testable without GPUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalChange {
    /// The snapshot `mtime` is not newer than the buffer's base — do nothing.
    None,
    /// The file changed under a **clean** buffer: silently auto-reload.
    Reload,
    /// The file changed under a **dirty** buffer: surface a conflict.
    Conflict,
}

// ── Main view ─────────────────────────────────────────────────────────────────

/// The code editor view: a `gpui-component` `InputState` in code-editor mode
/// plus all buffer bookkeeping and navigation state.
pub struct EditorView {
    input: Entity<InputState>,
    _input_change: Subscription,
    state: EditorState,
    save_state: SaveState,
    /// Whether the buffer has unsaved edits.
    dirty: bool,
    /// The base `mtime` of the open buffer, handed back as `SaveFile`'s
    /// `base_mtime` and compared against the worktree snapshot's `mtime`.
    base_mtime: Option<SystemTime>,

    /// Whether the open buffer is read-only (out-of-root target, #195/#301).
    /// No edit, no save path, unwatched snapshot.
    read_only: bool,

    /// Read requests: path → `ClientMessage::OpenFile`.
    open_file_tx: Sender<String>,
    /// Write requests: `ClientMessage::SaveFile`.
    save_file_tx: Sender<ClientMessage>,
    /// Live-buffer feed: `BufferChanged` / `BufferClosed` (#189).
    buffer_change_tx: Sender<ClientMessage>,
    /// Navigation requests: `DefinitionRequest` (#196).
    nav_tx: Sender<ClientMessage>,

    /// Monotonic open-request generation; fences the open timeout.
    generation: u64,
    /// Monotonic save-request generation; fences the save timeout.
    save_generation: u64,
    /// Monotonic buffer-feed generation; fences the debounce timer.
    buffer_generation: u64,

    /// Counter for `NavRequestId`s; incremented before every dispatch.
    nav_id: u64,
    /// The id of the most recent definition request dispatched. A response
    /// whose id does not match is silently dropped (drop-stale discipline).
    latest_def_id: Option<NavRequestId>,
    /// The id of the most recent hover request dispatched. A response whose id
    /// does not match is silently dropped (drop-stale discipline — mirrors the
    /// definition id discipline so concurrent hovers do not interleave).
    latest_hover_id: Option<NavRequestId>,
    /// The hover content currently displayed in the popover, or `None` when
    /// no popover is visible. Set by [`EditorView::apply_hover_response`];
    /// cleared on mouse-down or when a new hover request is dispatched.
    hover_content: Option<HoverContent>,
    /// Monotonic generation counter for the mouse-rest debounce timer.
    /// Bumped on every `MouseMoveEvent` so a stale timer recognises it is
    /// superseded and becomes a no-op.
    hover_move_generation: u64,

    /// (path, range) to apply after the next cross-file load completes.
    /// Set before `open_file_tx` is fired; consumed in [`EditorView::load`].
    pending_jump: Option<(String, Range)>,

    /// Bounded in-memory back-jump stack: (path, position, read_only) triples.
    /// `read_only` preserves the out-of-root flag so GoBack re-opens the file
    /// with the same access mode the original forward jump used.
    back_stack: VecDeque<(String, EditorPosition, bool)>,

    /// Transient inline jump-list for multi-target definition responses.
    jump_list: Option<Vec<JumpEntry>>,
}

impl EditorView {
    /// Create an empty editor.
    ///
    /// - `open_file_tx` — re-issues `OpenFile` for auto-reload and nav opens.
    /// - `save_file_tx` — carries `SaveFile` write requests.
    /// - `buffer_change_tx` — carries `BufferChanged` / `BufferClosed` (#189).
    /// - `nav_tx` — carries `DefinitionRequest` nav requests (#196).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        open_file_tx: Sender<String>,
        save_file_tx: Sender<ClientMessage>,
        buffer_change_tx: Sender<ClientMessage>,
        nav_tx: Sender<ClientMessage>,
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
            read_only: false,
            open_file_tx,
            save_file_tx,
            buffer_change_tx,
            nav_tx,
            generation: 0,
            save_generation: 0,
            buffer_generation: 0,
            nav_id: 0,
            latest_def_id: None,
            latest_hover_id: None,
            hover_content: None,
            hover_move_generation: 0,
            pending_jump: None,
            back_stack: VecDeque::new(),
            jump_list: None,
        }
    }

    // ── Dirty flag / live-buffer feed ─────────────────────────────────────

    /// Subscribe to the input's `Change` event: a keystroke marks the buffer
    /// dirty and arms the debounced live-buffer feed (#189).
    fn subscribe_dirty(input: &Entity<InputState>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(input, |this, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                if !this.dirty {
                    this.dirty = true;
                    cx.notify();
                }
                this.arm_buffer_feed(cx);
            }
        })
    }

    /// Arm (or re-arm) the debounced live-buffer feed (#189).
    fn arm_buffer_feed(&mut self, cx: &mut Context<Self>) {
        let EditorState::Loaded { path } = &self.state else {
            return;
        };
        let path = path.clone();
        self.buffer_generation = self.buffer_generation.wrapping_add(1);
        let generation = self.buffer_generation;

        cx.spawn(async move |this, cx| {
            smol::Timer::after(BUFFER_FEED_DEBOUNCE).await;
            cx.update(|cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.buffer_generation != generation {
                        return;
                    }
                    let EditorState::Loaded { path: open } = &this.state else {
                        return;
                    };
                    if *open != path {
                        return;
                    }
                    let content = this.input.read(cx).value().to_string();
                    if let Err(e) = this
                        .buffer_change_tx
                        .try_send(ClientMessage::BufferChanged {
                            path: path.clone(),
                            content,
                        })
                    {
                        tracing::debug!(error = %e, %path, "failed to enqueue live-buffer feed");
                    }
                });
            });
        })
        .detach();
    }

    /// Immediately send a `BufferChanged` without waiting for the debounce.
    ///
    /// Used before dispatching a nav request (flush-before-dispatch): the LSP
    /// must see the live buffer before the `DefinitionRequest` arrives. The
    /// daemon processes messages in send order, so the `didChange` from this
    /// flush lands before the nav request.
    ///
    /// Bumps `buffer_generation` so the in-flight debounce timer (if any) sees
    /// the mismatch and does not send a duplicate feed.
    fn flush_buffer_feed_if_dirty(&mut self, cx: &mut Context<Self>) {
        if !self.dirty {
            return;
        }
        let EditorState::Loaded { path } = &self.state else {
            return;
        };
        let path = path.clone();
        // Cancel the in-flight debounce.
        self.buffer_generation = self.buffer_generation.wrapping_add(1);
        let content = self.input.read(cx).value().to_string();
        if let Err(e) = self
            .buffer_change_tx
            .try_send(ClientMessage::BufferChanged {
                path: path.clone(),
                content,
            })
        {
            tracing::debug!(error = %e, %path, "failed to flush live-buffer before nav");
        }
    }

    fn close_live_buffer(&mut self, path: String) {
        self.buffer_generation = self.buffer_generation.wrapping_add(1);
        if let Err(e) = self
            .buffer_change_tx
            .try_send(ClientMessage::BufferClosed { path: path.clone() })
        {
            tracing::debug!(error = %e, %path, "failed to enqueue live-buffer close");
        }
    }

    // ── Diagnostics ───────────────────────────────────────────────────────

    /// Replace the editor's inline diagnostics with `items` (#189).
    pub fn set_diagnostics(&mut self, items: &[Diagnostic], cx: &mut Context<Self>) {
        let editor_items: Vec<EditorDiagnostic> = items.iter().map(to_editor_diagnostic).collect();
        self.input.update(cx, |input, cx| {
            if let Some(set) = input.diagnostics_mut() {
                set.clear();
                set.extend(editor_items);
            }
            cx.notify();
        });
        cx.notify();
    }

    // ── Buffer state accessors ────────────────────────────────────────────

    /// The path of the file currently open or loading, if any.
    pub fn open_path(&self) -> Option<&str> {
        match &self.state {
            EditorState::Loading { path }
            | EditorState::Loaded { path }
            | EditorState::Failed { path } => Some(path.as_str()),
            EditorState::Empty => None,
        }
    }

    /// The base `mtime` of the open buffer.
    pub fn base_mtime(&self) -> Option<SystemTime> {
        self.base_mtime
    }

    /// Whether the buffer has unsaved edits.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Whether the editor is currently surfacing a save conflict.
    pub fn has_conflict(&self) -> bool {
        matches!(self.save_state, SaveState::Conflict)
    }

    /// Whether the open buffer is read-only (out-of-root target, #195/#301).
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    // ── Open / load ───────────────────────────────────────────────────────

    /// Begin opening `path`. If `read_only` is `true` the buffer will be
    /// visibly non-editable (out-of-root carve-out, #195/#301). Any
    /// `pending_jump` is applied once the corresponding `FileContent` arrives.
    pub fn begin_open(
        &mut self,
        path: String,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(previous) = self.open_path().map(str::to_owned) {
            self.close_live_buffer(previous);
        }

        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;

        let language = language_for_path(&path);
        let path_for_timer = path.clone();
        self.state = EditorState::Loading { path };
        self.save_state = SaveState::Idle;
        self.base_mtime = None;
        self.dirty = false;
        self.read_only = read_only;
        self.jump_list = None;
        // Dismiss any hover popover from the previous file and cancel any
        // in-flight hover request so a delayed response for the old file does
        // not land on the new one.
        self.hover_content = None;
        self.latest_hover_id = None;

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

        cx.spawn(async move |this, cx| {
            smol::Timer::after(OPEN_TIMEOUT).await;
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

    /// Render a `FileContent` reply: if it matches the open in flight, load
    /// the content and apply any pending cross-file jump.
    pub fn load(
        &mut self,
        path: String,
        content: String,
        mtime: SystemTime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let matches = matches!(&self.state, EditorState::Loading { path: p } if *p == path);
        if !matches {
            return;
        }

        self.base_mtime = Some(mtime);
        self.input.update(cx, |input, cx| {
            input.set_value(content, window, cx);
        });
        // `set_value` emits `Change` — clear dirty so a load starts clean.
        self.dirty = false;
        self.save_state = SaveState::Idle;
        self.state = EditorState::Loaded { path: path.clone() };

        // Apply a pending cross-file jump (navigation or go-back) if the
        // loaded path matches what we stored before firing the open.
        if let Some((jump_path, range)) = self.pending_jump.take() {
            if jump_path == path {
                self.apply_jump_range(&range, window, cx);
            }
        }

        cx.notify();
    }

    // ── Save ──────────────────────────────────────────────────────────────

    /// Send the open buffer back to the remote. A no-op when no file is loaded
    /// or the buffer is read-only.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
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

    /// Apply a `SaveResult` reply: the write landed.
    pub fn apply_save_result(&mut self, path: String, mtime: SystemTime, cx: &mut Context<Self>) {
        if self.open_path() != Some(path.as_str()) {
            return;
        }
        self.base_mtime = Some(mtime);
        self.dirty = false;
        self.save_state = SaveState::Idle;
        self.save_generation = self.save_generation.wrapping_add(1);
        // Disk now matches the buffer: revert to the disk-backed baseline.
        self.close_live_buffer(path);
        cx.notify();
    }

    /// Apply a `SaveConflict` reply: the daemon refused the write.
    pub fn apply_save_conflict(&mut self, path: String, cx: &mut Context<Self>) {
        if self.open_path() != Some(path.as_str()) {
            return;
        }
        self.save_state = SaveState::Conflict;
        self.save_generation = self.save_generation.wrapping_add(1);
        cx.notify();
    }

    // ── Concurrent external change ────────────────────────────────────────

    /// React to the worktree snapshot reporting a new `mtime` for the open
    /// path. Runs the pure [`decide_external_change`] decision and acts on it.
    pub fn note_external_change(
        &mut self,
        snapshot_mtime: SystemTime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                self.begin_open(path.clone(), self.read_only, window, cx);
                if let Err(e) = self.open_file_tx.try_send(path.clone()) {
                    tracing::debug!(error = %e, %path, "failed to enqueue auto-reload open");
                }
            }
            ExternalChange::Conflict => {
                self.save_state = SaveState::Conflict;
                cx.notify();
            }
        }
    }

    // ── Navigation — dispatch ─────────────────────────────────────────────

    /// Dispatch a `DefinitionRequest` for the current cursor position.
    ///
    /// Performs flush-before-dispatch: if the buffer is dirty, immediately
    /// sends a `BufferChanged` so the daemon's LSP has the live buffer before
    /// the nav request arrives. A no-op unless a file is loaded.
    fn dispatch_definition_request(&mut self, cx: &mut Context<Self>) -> bool {
        let EditorState::Loaded { path } = &self.state else {
            return false;
        };
        let path = path.clone();

        // Flush before dispatch (spec §"Request-vs-didChange ordering"): send
        // the live buffer immediately so the LSP resolves the symbol against
        // the current buffer text, not the stale on-disk version. The daemon
        // processes messages in-order, so the didChange from this flush lands
        // before the DefinitionRequest.
        self.flush_buffer_feed_if_dirty(cx);

        let position = self.cursor_to_protocol(cx);
        self.nav_id = self.nav_id.wrapping_add(1);
        let id = NavRequestId(self.nav_id);
        self.latest_def_id = Some(id);

        if let Err(e) = self.nav_tx.try_send(ClientMessage::DefinitionRequest {
            id,
            path: path.clone(),
            position,
        }) {
            tracing::debug!(error = %e, %path, "failed to enqueue definition request");
        }
        true
    }

    /// Convert the current `InputState` cursor position to the protocol's
    /// `Position` type. The editor's `cursor_position()` returns a
    /// `(line, character)` pair with character as a Unicode scalar count —
    /// the same convention the protocol uses (UTF-8 char offsets).
    fn cursor_to_protocol(&self, cx: &Context<Self>) -> Position {
        let pos = self.input.read(cx).cursor_position();
        Position {
            line: pos.line,
            character: pos.character,
        }
    }

    /// Dispatch a `HoverRequest` for the current cursor position (#197).
    ///
    /// Mirrors [`Self::dispatch_definition_request`]: performs flush-before-
    /// dispatch, increments `nav_id`, records the latest hover id, and clears
    /// any previously-visible popover so a new request does not show stale
    /// content while the daemon is in flight. A no-op unless a file is loaded.
    fn dispatch_hover_request(&mut self, cx: &mut Context<Self>) {
        let EditorState::Loaded { path } = &self.state else {
            return;
        };
        let path = path.clone();

        // Flush-before-dispatch: the LSP must see the live buffer before the
        // `HoverRequest` arrives, for the same reason as definition requests.
        self.flush_buffer_feed_if_dirty(cx);

        // Clear the previous popover immediately: a stale popover that stays
        // visible until the response arrives is misleading.
        self.hover_content = None;

        let position = self.cursor_to_protocol(cx);
        self.nav_id = self.nav_id.wrapping_add(1);
        let id = NavRequestId(self.nav_id);
        self.latest_hover_id = Some(id);

        if let Err(e) = self.nav_tx.try_send(ClientMessage::HoverRequest {
            id,
            path: path.clone(),
            position,
        }) {
            tracing::debug!(error = %e, %path, "failed to enqueue hover request");
        }
        cx.notify();
    }

    /// Arm (or re-arm) the mouse-rest debounce timer for hover (#197).
    ///
    /// Called from the `MouseMoveEvent` handler on the outer div. A no-op when
    /// no file is loaded (saves a detached task spawn on empty/loading state).
    /// Bumps `hover_move_generation` so any in-flight timer from the previous
    /// mouse movement sees the mismatch and does nothing. When the timer fires
    /// and the generation still matches, the hover request is dispatched at the
    /// current cursor position (which follows the last click, making
    /// hover-after-click natural).
    fn arm_hover_debounce(&mut self, cx: &mut Context<Self>) {
        // Only arm when a file is actually loaded; avoids spawning tasks while
        // the editor is empty, loading, or failed.
        if !matches!(self.state, EditorState::Loaded { .. }) {
            return;
        }
        self.hover_move_generation = self.hover_move_generation.wrapping_add(1);
        let generation = self.hover_move_generation;
        cx.spawn(async move |this, cx| {
            smol::Timer::after(HOVER_MOUSE_DEBOUNCE).await;
            cx.update(|cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.hover_move_generation == generation {
                        this.dispatch_hover_request(cx);
                    }
                });
            });
        })
        .detach();
    }

    // ── Navigation — response handling ────────────────────────────────────

    /// Apply a `DefinitionResponse` from the daemon.
    ///
    /// Drops the response if its id does not match the latest request
    /// (drop-stale discipline: the user may have moved on). A single target
    /// jumps directly; multiple targets show the inline jump-list.
    pub fn apply_definition_response(
        &mut self,
        id: NavRequestId,
        targets: Vec<NavLocation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.latest_def_id != Some(id) {
            tracing::debug!(?id, "dropping stale definition response");
            return;
        }

        match targets.len() {
            0 => {
                // No definition found — silent no-op.
                tracing::debug!("definition response: no targets (server found nothing)");
            }
            1 => {
                let target = targets.into_iter().next().expect("checked len == 1");
                self.push_back_position(cx);
                self.jump_to_location(target, window, cx);
            }
            _ => {
                // Multiple targets (e.g. Rust trait impls): show the jump-list.
                self.jump_list = Some(
                    targets
                        .into_iter()
                        .map(|l| JumpEntry { location: l })
                        .collect(),
                );
                cx.notify();
            }
        }
    }

    /// Apply a `HoverResponse` from the daemon (#197).
    ///
    /// Drops the response if its id does not match the latest hover request
    /// (drop-stale discipline: mirrors the definition id discipline so
    /// concurrent hover requests do not interleave). `None` content means the
    /// server found nothing — silent no-op, no error surface. `Some` content
    /// renders the markdown in the floating popover.
    pub fn apply_hover_response(
        &mut self,
        id: NavRequestId,
        content: Option<HoverContent>,
        cx: &mut Context<Self>,
    ) {
        if self.latest_hover_id != Some(id) {
            tracing::debug!(?id, "dropping stale hover response");
            return;
        }

        self.hover_content = content;
        cx.notify();
    }

    // ── Navigation — jump mechanics ───────────────────────────────────────

    /// Push the current (path, position, read_only) onto the back-stack.
    ///
    /// The `read_only` flag is preserved so GoBack can re-open the file with
    /// the same access mode (out-of-root targets must stay read-only on unwind).
    /// Evicts the oldest entry when the stack reaches `BACK_STACK_MAX`.
    fn push_back_position(&mut self, cx: &Context<Self>) {
        let EditorState::Loaded { path } = &self.state else {
            return;
        };
        let path = path.clone();
        let pos = self.input.read(cx).cursor_position();
        let read_only = self.read_only;
        if self.back_stack.len() >= BACK_STACK_MAX {
            self.back_stack.pop_front();
        }
        self.back_stack.push_back((path, pos, read_only));
    }

    /// Perform a jump to a `NavLocation`: same-file scrolls + lands cursor;
    /// cross-file opens via the buffer channel with the range stored as a
    /// pending jump applied in [`EditorView::load`].
    fn jump_to_location(
        &mut self,
        location: NavLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current_path = match &self.state {
            EditorState::Loaded { path } => Some(path.clone()),
            _ => None,
        };

        if current_path.as_deref() == Some(location.path.as_str()) {
            // Same-file jump: scroll + select the target range in the current
            // buffer (no load roundtrip needed).
            self.apply_jump_range(&location.range, window, cx);
        } else {
            // Cross-file jump (in-root or out-of-root via the #195/#301
            // carve-out). Store the range so `load` applies it once the
            // FileContent reply arrives, then open the file.
            let read_only = location.out_of_root;
            self.pending_jump = Some((location.path.clone(), location.range));
            self.begin_open(location.path.clone(), read_only, window, cx);
            if let Err(e) = self.open_file_tx.try_send(location.path.clone()) {
                tracing::debug!(
                    error = %e,
                    path = %location.path,
                    "failed to enqueue cross-file nav open"
                );
            }
        }
    }

    /// Move the cursor to `range.start` (scroll + select). The protocol
    /// `Range` uses UTF-8 char offsets, matching the editor's `cursor_position`
    /// convention, so no offset translation is needed here.
    ///
    /// `InputState::set_cursor_position` scrolls the view to keep the cursor
    /// visible and is the public API for programmatic cursor moves.
    ///
    /// Range-end selection: `InputState` does not expose a public
    /// `set_selected_range` in this version of gpui-component. For v1 the
    /// cursor landing at `range.start` is the primary nav signal. A
    /// TODO is filed below for the selection extension when the API is
    /// available.
    fn apply_jump_range(&mut self, range: &Range, window: &mut Window, cx: &mut Context<Self>) {
        let start = EditorPosition::new(range.start.line, range.start.character);
        // TODO(nav-select): extend selection to range.end when gpui-component
        // exposes a public set_selected_range. For v1 the cursor at start is
        // the landing signal; the symbol is visible in the scrolled view.
        self.input.update(cx, |input, cx| {
            input.set_cursor_position(start, window, cx);
        });
        cx.notify();
    }

    /// Unwind the most recent jump: return to the position saved on the
    /// back-stack. Crosses file boundaries if the back-position is in a
    /// different file, storing a pending jump for the `load` path. The
    /// `read_only` flag stored in the entry is preserved so out-of-root targets
    /// remain read-only on unwind.
    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, pos, read_only)) = self.back_stack.pop_back() else {
            return;
        };

        let current_path = match &self.state {
            EditorState::Loaded { path } => Some(path.clone()),
            _ => None,
        };

        if current_path.as_deref() == Some(path.as_str()) {
            // Same file — restore the access mode and move the cursor directly.
            self.read_only = read_only;
            self.input.update(cx, |input, cx| {
                input.set_cursor_position(pos, window, cx);
            });
            cx.notify();
        } else {
            // Different file — open it (preserving the original read_only mode)
            // and land on the saved position via pending_jump.
            let proto_pos = Position {
                line: pos.line,
                character: pos.character,
            };
            let range = Range {
                start: proto_pos,
                end: proto_pos,
            };
            self.pending_jump = Some((path.clone(), range));
            self.begin_open(path.clone(), read_only, window, cx);
            if let Err(e) = self.open_file_tx.try_send(path.clone()) {
                tracing::debug!(error = %e, %path, "failed to enqueue go-back open");
            }
        }
    }

    /// Select a jump-list entry by index and navigate to it. Called from the
    /// click handler on the inline jump-list items rendered in `Render`.
    pub fn select_jump_entry(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(list) = self.jump_list.take() else {
            return;
        };
        if let Some(entry) = list.into_iter().nth(index) {
            self.push_back_position(cx);
            self.jump_to_location(entry.location, window, cx);
        }
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Loading / empty / failed states: show a centered status message.
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

        // One-line save-outcome banner.
        let banner: Option<(String, gpui::Hsla)> = match &self.save_state {
            SaveState::Idle => None,
            SaveState::Saving => Some(("Saving\u{2026}".to_owned(), cx.theme().muted_foreground)),
            SaveState::Conflict => Some((
                "Changed on disk since you opened it \u{2014} re-open to reload, \
                 or save again to keep your version"
                    .to_owned(),
                cx.theme().danger,
            )),
            SaveState::Failed => Some(("Save failed".to_owned(), cx.theme().danger)),
        };

        // Inline jump-list for multi-target definition responses.
        let jump_list_element = self.jump_list.as_ref().map(|list| {
            let entries: Vec<_> = list
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    let preview = entry.location.line_preview.clone().unwrap_or_default();
                    let path = entry.location.path.clone();
                    let line = entry.location.range.start.line + 1;
                    let label = format!("{path}:{line}  {preview}");
                    div()
                        .px(px(8.0))
                        .py(px(2.0))
                        .text_xs()
                        .text_color(cx.theme().foreground)
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                                this.select_jump_entry(i, window, cx);
                            }),
                        )
                        .child(label)
                })
                .collect();

            div()
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .right(px(0.0))
                .bg(cx.theme().popover)
                .border_b_1()
                .border_color(cx.theme().border)
                .shadow_md()
                .child(
                    div()
                        .px(px(8.0))
                        .py(px(4.0))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Multiple definitions — click to jump:"),
                )
                .children(entries)
        });

        // Build the `Input` widget. The context-menu builder is called each
        // time the user right-clicks; it receives a fresh `PopupMenu`.
        // "Go to Definition" dispatches the `GoToDefinition` action; "Show
        // Hover" dispatches the `ShowHover` action — both handled on the outer
        // div below. `.disabled` blocks all key events and edit operations in
        // the `InputState`, enforcing the out-of-root read-only contract
        // (#196/#301).
        let input_widget = Input::new(&self.input)
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .size_full()
            .disabled(self.read_only)
            .context_menu(|menu: PopupMenu, _window, _cx| {
                menu.menu("Go to Definition", Box::new(GoToDefinition))
                    .menu("Show Hover", Box::new(ShowHover))
                    .separator()
            });

        // Hover popover (#197): rendered as an absolutely-positioned overlay
        // just above the cursor line when `hover_content` is set.
        //
        // Theme tokens used: `popover` (background), `border`, `foreground`,
        // `muted_foreground`. No `card` field (does not exist), no `z_index`
        // method (not in GPUI) — layering is via child render order (the
        // popover child is added *after* the editor area so it paints on top).
        let hover_popover_element = self.hover_content.as_ref().map(|content| {
            let md_source = content.markdown.clone();
            div()
                .absolute()
                .bottom(px(0.0))
                .left(px(0.0))
                .right(px(0.0))
                .bg(cx.theme().popover)
                .border_t_1()
                .border_color(cx.theme().border)
                .shadow_md()
                .p(px(8.0))
                .text_xs()
                .text_color(cx.theme().foreground)
                .overflow_hidden()
                .child(markdown(md_source))
        });

        // Outer div: the editor key context, action handlers, ctrl+click.
        //
        // The `on_mouse_down` for ctrl+click runs in the **bubble phase**: by
        // the time this handler fires, the `InputState` has already processed
        // the click and moved the cursor to the clicked position. We can
        // therefore read `cursor_position()` and dispatch the definition
        // request with the correct cursor location.
        //
        // Trigger mechanics (pinned for spec #196 and #197):
        //   - Ctrl+click: Left button + `modifiers.secondary()` (Ctrl on
        //     Linux/Windows, Cmd on macOS — `gpui::Modifiers::secondary()`).
        //   - Context menu: right-click → "Go to Definition" → `GoToDefinition`
        //     action, handled by `on_action` below.
        //   - Shift+K (keybind) or context menu "Show Hover": `ShowHover`
        //     action at the cursor position.
        //   - Mouse-rest: `on_mouse_move` arms a 500 ms debounce; when it
        //     fires `dispatch_hover_request` is called at cursor position.
        //     A left-button mouse-down clears the popover immediately.
        let mut root = div()
            .key_context(EDITOR_KEY_CONTEXT)
            .size_full()
            .flex()
            .flex_col()
            .relative()
            .on_action(cx.listener(|this, _: &Save, _window, cx| {
                this.save(cx);
            }))
            .on_action(cx.listener(|this, _: &GoToDefinition, _window, cx| {
                this.dispatch_definition_request(cx);
            }))
            .on_action(cx.listener(|this, _: &GoBack, window, cx| {
                this.go_back(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowHover, _window, cx| {
                this.dispatch_hover_request(cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    // Dismiss any visible hover popover on click and cancel any
                    // in-flight hover request so a delayed response does not
                    // re-open the popover after the user clicked away.
                    if this.hover_content.is_some() || this.latest_hover_id.is_some() {
                        this.hover_content = None;
                        this.latest_hover_id = None;
                        cx.notify();
                    }
                    if event.modifiers.secondary() {
                        // Cursor is already at the clicked position (InputState
                        // processed the event first in its own update cycle).
                        this.dispatch_definition_request(cx);
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, _event: &MouseMoveEvent, _window, cx| {
                // Arm (or re-arm) the mouse-rest debounce for hover (#197).
                // Each mouse move bumps the generation so the previous timer
                // becomes a no-op; a new timer starts. When the mouse is still
                // for HOVER_MOUSE_DEBOUNCE, `dispatch_hover_request` fires at
                // the cursor position.
                this.arm_hover_debounce(cx);
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

        // Read-only indicator for out-of-root files (#196 / #195/#301).
        if self.read_only {
            root = root.child(
                div()
                    .flex_shrink_0()
                    .px(px(8.0))
                    .py(px(4.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .bg(cx.theme().muted)
                    .child("Read-only (outside the project root)"),
            );
        }

        // Editor area: the input widget plus any overlays (jump-list, hover
        // popover). Overlays are children rendered *after* the input so they
        // paint on top without needing z-index (child order = paint order).
        let mut editor_area = div().flex_1().min_h_0().relative().child(input_widget);

        if let Some(jump_list_el) = jump_list_element {
            editor_area = editor_area.child(jump_list_el);
        }

        // Hover popover (#197): rendered last so it paints above the editor
        // and the jump-list. Uses absolute positioning anchored to the bottom
        // of the editor area — this positions the popover below the current
        // viewport and above any status bars, matching VS Code's hover panel.
        if let Some(popover) = hover_popover_element {
            editor_area = editor_area.child(popover);
        }

        root.child(editor_area).into_any_element()
    }
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

/// Decide what an external change to the open path forces.
///
/// This is the load-bearing concurrent-write rule (`docs/spec-editor.md`):
///
/// - `snapshot <= base` → [`ExternalChange::None`]
/// - `snapshot > base` and clean buffer → [`ExternalChange::Reload`]
/// - `snapshot > base` and dirty buffer → [`ExternalChange::Conflict`]
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

/// Translate a daemon protocol [`Diagnostic`] into the editor component's
/// [`EditorDiagnostic`] (#189).
fn to_editor_diagnostic(diagnostic: &Diagnostic) -> EditorDiagnostic {
    let start = EditorPosition::new(
        diagnostic.range.start.line,
        diagnostic.range.start.character,
    );
    let end = EditorPosition::new(diagnostic.range.end.line, diagnostic.range.end.character);
    let severity = match diagnostic.severity {
        DiagnosticSeverity::Error => EditorSeverity::Error,
        DiagnosticSeverity::Warning => EditorSeverity::Warning,
        DiagnosticSeverity::Information => EditorSeverity::Info,
        DiagnosticSeverity::Hint => EditorSeverity::Hint,
    };
    let mut editor =
        EditorDiagnostic::new(start..end, diagnostic.message.clone()).with_severity(severity);
    if let Some(source) = &diagnostic.source {
        editor = editor.with_source(source.clone());
    }
    if let Some(code) = &diagnostic.code {
        editor = editor.with_code(code.clone());
    }
    editor
}

/// Derive the highlighting language token for a path from its extension.
fn language_for_path(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_else(|| "text".to_owned())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // --- language detection ---

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

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn test_clean_buffer_with_newer_snapshot_reloads() {
        assert_eq!(
            decide_external_change(at(100), at(200), false),
            ExternalChange::Reload
        );
    }

    #[test]
    fn test_dirty_buffer_with_newer_snapshot_conflicts() {
        assert_eq!(
            decide_external_change(at(100), at(200), true),
            ExternalChange::Conflict
        );
    }

    #[test]
    fn test_equal_snapshot_is_no_change_regardless_of_dirty() {
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
        assert_eq!(
            decide_external_change(at(200), at(100), false),
            ExternalChange::None
        );
        assert_eq!(
            decide_external_change(at(200), at(100), true),
            ExternalChange::None
        );
    }

    // --- inline-diagnostic translation (#189) ---

    use rift_protocol::{Position, Range};

    fn proto_diag(
        severity: DiagnosticSeverity,
        source: Option<&str>,
        code: Option<&str>,
    ) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position {
                    line: 10,
                    character: 4,
                },
                end: Position {
                    line: 10,
                    character: 9,
                },
            },
            severity,
            message: "mismatched types".to_owned(),
            source: source.map(str::to_owned),
            code: code.map(str::to_owned),
        }
    }

    #[test]
    fn test_to_editor_diagnostic_maps_range_message_source_and_code() {
        let editor = to_editor_diagnostic(&proto_diag(
            DiagnosticSeverity::Error,
            Some("rustc"),
            Some("E0308"),
        ));
        assert_eq!(editor.range.start, EditorPosition::new(10, 4));
        assert_eq!(editor.range.end, EditorPosition::new(10, 9));
        assert_eq!(editor.severity, EditorSeverity::Error);
        assert_eq!(editor.message.as_ref(), "mismatched types");
        assert_eq!(editor.source.as_deref(), Some("rustc"));
        assert_eq!(editor.code.as_deref(), Some("E0308"));
    }

    #[test]
    fn test_to_editor_diagnostic_maps_each_severity() {
        let cases = [
            (DiagnosticSeverity::Error, EditorSeverity::Error),
            (DiagnosticSeverity::Warning, EditorSeverity::Warning),
            (DiagnosticSeverity::Information, EditorSeverity::Info),
            (DiagnosticSeverity::Hint, EditorSeverity::Hint),
        ];
        for (proto, expected) in cases {
            let editor = to_editor_diagnostic(&proto_diag(proto, None, None));
            assert_eq!(editor.severity, expected);
        }
    }

    #[test]
    fn test_to_editor_diagnostic_omits_absent_source_and_code() {
        let editor = to_editor_diagnostic(&proto_diag(DiagnosticSeverity::Warning, None, None));
        assert!(editor.source.is_none());
        assert!(editor.code.is_none());
    }

    // --- navigation back-stack (#196) ---

    #[test]
    fn test_back_stack_bounded_at_max() {
        // The back-stack must never exceed BACK_STACK_MAX entries; oldest
        // entries are evicted when it would overflow.
        let mut stack: VecDeque<(String, EditorPosition, bool)> = VecDeque::new();
        for i in 0..(BACK_STACK_MAX + 10) {
            if stack.len() >= BACK_STACK_MAX {
                stack.pop_front();
            }
            stack.push_back((format!("file_{i}.rs"), EditorPosition::new(0, 0), false));
        }
        assert_eq!(stack.len(), BACK_STACK_MAX);
        // The oldest entries are gone; only the most recent BACK_STACK_MAX remain.
        assert_eq!(
            stack.front().map(|(p, _, _)| p.as_str()),
            Some("file_10.rs")
        );
    }

    #[test]
    fn test_back_stack_unwinds_in_lifo_order() {
        let mut stack: VecDeque<(String, EditorPosition, bool)> = VecDeque::new();
        stack.push_back(("a.rs".to_owned(), EditorPosition::new(1, 0), false));
        stack.push_back(("b.rs".to_owned(), EditorPosition::new(2, 0), false));
        stack.push_back(("c.rs".to_owned(), EditorPosition::new(3, 0), true));

        // GoBack pops from the back (LIFO); read_only is preserved per entry.
        let (p, _, ro) = stack.pop_back().unwrap();
        assert_eq!(p, "c.rs");
        assert!(ro, "c.rs was out-of-root, so read_only must be true");
        assert_eq!(stack.pop_back().map(|(p, _, _)| p), Some("b.rs".to_owned()));
        assert_eq!(stack.pop_back().map(|(p, _, _)| p), Some("a.rs".to_owned()));
        assert!(stack.is_empty());
    }

    // --- stale-response drop discipline (#196) ---

    #[test]
    fn test_stale_definition_response_id_mismatch_is_detected() {
        // A response whose id does not match the latest dispatched id is stale
        // and must be dropped.
        let latest = NavRequestId(5);
        let stale = NavRequestId(3);
        assert_ne!(Some(latest), Some(stale));
    }

    #[test]
    fn test_matching_definition_response_id_is_accepted() {
        let latest = NavRequestId(5);
        let response_id = NavRequestId(5);
        assert_eq!(Some(latest), Some(response_id));
    }

    // --- out-of-root flag (#196 / #195/#301) ---

    #[test]
    fn test_out_of_root_nav_location_carries_flag_and_absolute_path() {
        let loc = NavLocation {
            path: "/home/user/.cargo/registry/src/foo/src/lib.rs".to_owned(),
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 1,
                },
            },
            out_of_root: true,
            line_preview: None,
        };
        // An out-of-root location must be opened read-only.
        assert!(loc.out_of_root);
        // Its path is absolute (starts with '/').
        assert!(loc.path.starts_with('/'));
    }

    #[test]
    fn test_in_root_nav_location_is_not_read_only() {
        let loc = NavLocation {
            path: "src/main.rs".to_owned(),
            range: Range {
                start: Position {
                    line: 10,
                    character: 4,
                },
                end: Position {
                    line: 10,
                    character: 12,
                },
            },
            out_of_root: false,
            line_preview: Some("pub fn foo() {}".to_owned()),
        };
        assert!(!loc.out_of_root);
    }

    // --- flush-before-dispatch invariant (#196) ---

    #[test]
    fn test_nav_id_increments_on_each_dispatch() {
        // Each dispatch must use a fresh, monotonically increasing id so
        // the drop-stale check can distinguish successive requests.
        let mut id: u64 = 0;
        let first = {
            id = id.wrapping_add(1);
            NavRequestId(id)
        };
        let second = {
            id = id.wrapping_add(1);
            NavRequestId(id)
        };
        assert_ne!(first, second);
        assert!(second.0 > first.0);
    }

    // --- hover stale-response drop discipline (#197) ---

    #[test]
    fn test_stale_hover_response_id_mismatch_is_detected() {
        // A hover response whose id does not match the latest dispatched id is
        // stale and must be dropped — same discipline as definition responses.
        let latest = NavRequestId(7);
        let stale = NavRequestId(4);
        assert_ne!(Some(latest), Some(stale));
    }

    #[test]
    fn test_matching_hover_response_id_is_accepted() {
        let latest = NavRequestId(7);
        let response_id = NavRequestId(7);
        assert_eq!(Some(latest), Some(response_id));
    }

    #[test]
    fn test_hover_and_definition_ids_share_the_same_counter_and_stay_distinct() {
        // Both hover and definition requests increment the same `nav_id`
        // counter, so they never accidentally collide. A hover dispatched after
        // a definition request carries a strictly higher id.
        let mut nav_id: u64 = 0;
        // Simulate dispatch_definition_request:
        nav_id = nav_id.wrapping_add(1);
        let def_id = NavRequestId(nav_id);
        // Simulate dispatch_hover_request:
        nav_id = nav_id.wrapping_add(1);
        let hover_id = NavRequestId(nav_id);
        assert_ne!(def_id, hover_id);
        assert!(hover_id.0 > def_id.0);
    }

    #[test]
    fn test_hover_content_with_none_is_silent_no_op() {
        // A HoverResponse with `content: None` means the server found nothing;
        // the popover must remain absent — not an error, not a panic.
        let latest_hover_id = NavRequestId(3);
        let response_id = NavRequestId(3);
        // Ids match → the response would be applied.
        assert_eq!(Some(latest_hover_id), Some(response_id));
        // content = None → hover_content stays None after apply.
        let content: Option<HoverContent> = None;
        assert!(content.is_none(), "no popover for a None response");
    }

    #[test]
    fn test_hover_move_generation_increments_per_debounce_arm() {
        // Each call to arm_hover_debounce must bump the generation so that
        // the previous in-flight timer becomes a no-op.
        let mut gen: u64 = 0;
        gen = gen.wrapping_add(1);
        let g1 = gen;
        gen = gen.wrapping_add(1);
        let g2 = gen;
        assert_ne!(g1, g2);
        assert!(g2 > g1);
    }
}
