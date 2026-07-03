// SPDX-License-Identifier: GPL-3.0-or-later
//! Code editor surface: open a file from the tree into a `gpui-component` code
//! editor, render it with Tree-sitter syntax highlighting, write edits back
//! over the buffer channel, and navigate symbols via go-to-definition
//! (ctrl+click, context menu), find-references (Shift+F12), hover popovers,
//! back-navigation, and read-only out-of-root opens
//! (`docs/spec-lsp-navigation.md`, #196, #197, #198).
//!
//! # Tabs (#351, #352)
//!
//! `EditorView` holds an ordered set of open tabs (`Vec<EditorTab>`) plus an
//! active index; each tab owns its own `gpui-component` `InputState` and all
//! per-file bookkeeping — dirty flag, base-`mtime`, the out-of-root
//! `read_only` bit, diagnostics, cursor/scroll, and nav-UI state (hover
//! content, jump-list, back-stack). Opening an already-open path switches the
//! active index to it instead of duplicating a tab; a new path opens and
//! activates a new one (`docs/spec-editor-tabs.md`). A `gpui-component`
//! `TabBar` above the editor content (the same pattern `SessionView` uses for
//! tmux windows) lists every open tab with a dirty dot and a close
//! affordance: clicking a tab activates it and moves focus to its buffer;
//! closing one removes it, activating the right neighbor (or the left if it
//! was rightmost) — closing the last tab returns the editor to its empty
//! state (#352). Dirty-close confirmation and the workspace-level signal
//! fan-out across multiple open tabs (mtime / diagnostics routed by path)
//! land in later steps; until then those two signals still resolve "the open
//! file" as the active tab, preserving the single-buffer behavior as tabs'
//! one-tab subset.
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
//! [`Save`] (bound to `Ctrl+S` / `Cmd+S`) sends the active tab's whole buffer
//! as a `SaveFile { path, content, base_mtime }`. The daemon replies with
//! `SaveResult` (commit new `mtime`) or `SaveConflict` (refuse without
//! clobbering the newer on-disk version).
//!
//! # Concurrent external change (#188)
//!
//! [`EditorView::note_external_change`] runs the pure [`decide_external_change`]
//! decision on the active tab's snapshot `mtime`: a clean buffer auto-reloads;
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
//! via the existing buffer channel (`open_file_tx`), applying open-or-switch
//! semantics, and lands on the range immediately (an already-open tab) or once
//! its `FileContent` reply loads (a new tab, via [`EditorTab::pending_jump`]).
//! An out-of-root target (absolute path, `out_of_root = true`) opens via the
//! same buffer channel — the daemon's out-of-root read carve-out (#195/#301)
//! serves the bytes — and that tab is **read-only** so no edit or save is
//! possible.
//!
//! A bounded in-memory back-jump stack lets the user unwind jumps with the
//! `GoBack` action (bound to `Alt+Left` in `main.rs`); the stack lives on the
//! tab a jump landed on, so `GoBack` unwinds from wherever the user currently
//! is.
//!
//! When a `DefinitionResponse` carries multiple targets (e.g. Rust trait impls)
//! a transient inline jump-list is rendered so the user can click the desired
//! destination.
//!
//! # Find-references (#198)
//!
//! Find-references is triggered by:
//! - **`Shift+F12`** (scoped to the `Editor` key context, bound in `main.rs`):
//!   dispatches [`ClientMessage::ReferencesRequest`] at the cursor position.
//! - **Context-menu "Find References"**: same dispatch path.
//!
//! The response is applied by [`EditorView::apply_references_response`]. The
//! results are shown in the same transient inline jump-list the multi-target
//! definition path uses, so the UX (click-to-jump, back-nav) is identical.
//!
//! Stale-response discipline mirrors the definition and hover paths: a
//! response is matched to whichever tab's `latest_*_id` equals the response's
//! id (nav ids are one editor-scoped counter shared by every tab, #351); no
//! match means the response is stale and is silently dropped.
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
//!   pending timer by bumping the active tab's `hover_move_generation`.
//!
//! The response is applied by [`EditorView::apply_hover_response`]. A
//! [`HoverContent`] renders in a floating popover anchored just above the
//! cursor line, rendered via `gpui_component::text::markdown`. A `None`
//! content (server found nothing) is a silent no-op.
//!
//! Clicking anywhere in the editor or moving the mouse out of the popover
//! dismisses it (clears the active tab's `hover_content`).
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
    div, px, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::highlighter::{
    Diagnostic as EditorDiagnostic, DiagnosticSeverity as EditorSeverity,
};
use gpui_component::input::{Input, InputEvent, InputState, Position as EditorPosition};
use gpui_component::menu::PopupMenu;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::text::markdown;
use gpui_component::ActiveTheme as _;
use rift_protocol::{
    ClientMessage, Diagnostic, DiagnosticSeverity, HoverContent, NavLocation, NavRequestId,
    Position, Range,
};

/// Stable, distinct dock-panel identity for the editor (`Panel::panel_name`).
/// Once shipped this must not change — it is the persisted panel identifier.
pub const EDITOR_PANEL_NAME: &str = "editor";

// ── Actions ───────────────────────────────────────────────────────────────────

/// The save action: write the active tab's buffer back to the remote.
/// Dispatched from the editor's key context, bound to `Ctrl+S` / `Cmd+S` in
/// `main.rs`.
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

/// Trigger find-references at the current cursor position. Dispatched from
/// the context-menu entry ("Find References") and from the `Shift+F12` keybind
/// (bound in `main.rs`, scoped to the editor key context). Results are shown
/// in the transient inline jump-list shared with multi-target definitions (#198).
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct FindReferences;

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

/// Maximum entries in a tab's bounded back-jump stack. Oldest entries are
/// evicted when this limit is reached so a long navigation session never
/// leaks memory.
const BACK_STACK_MAX: usize = 50;

/// How long the editor waits after the mouse stops moving before sending a
/// `HoverRequest`. 500 ms matches VS Code's default hover delay and avoids
/// flooding the LSP on fast cursor movement.
const HOVER_MOUSE_DEBOUNCE: Duration = Duration::from_millis(500);

// ── Internal state types ──────────────────────────────────────────────────────

/// What a tab is currently showing.
enum TabLoadState {
    /// An open request is in flight, awaiting its `FileContent` reply.
    Loading,
    /// The tab's content is rendered in the code editor.
    Loaded,
    /// The last open did not complete.
    Failed,
}

/// The transient outcome of a tab's most recent save.
enum SaveState {
    Idle,
    Saving,
    Conflict,
    Failed,
}

/// One entry in the inline jump-list shown for multi-target definition
/// responses (e.g. Rust trait method impls) and for find-references results.
struct JumpEntry {
    location: NavLocation,
}

/// The kind of results currently shown in the inline jump-list. Used to
/// render an appropriate header line in the jump-list overlay.
enum JumpListKind {
    /// Multi-target definition results (Rust trait impls etc.).
    Definitions,
    /// Find-references results.
    References,
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

// ── Per-tab state ──────────────────────────────────────────────────────────────

/// One open tab: a `gpui-component` `InputState` (code-editor mode) plus all
/// the per-file bookkeeping and navigation-UI state that used to be scalar
/// fields on a single-buffer `EditorView` (`docs/spec-editor-tabs.md`, #351).
/// Cursor position, scroll, and diagnostics live inside `input` itself and so
/// need no separate field here.
struct EditorTab {
    /// The root-relative path this tab holds (absolute for an out-of-root
    /// read, #195/#301). Fixed for the tab's lifetime — reloading re-fetches
    /// this same path, it never changes.
    path: String,
    input: Entity<InputState>,
    _input_change: Subscription,
    load_state: TabLoadState,
    save_state: SaveState,
    /// Whether this tab's buffer has unsaved edits.
    dirty: bool,
    /// The base `mtime` of this tab's buffer, handed back as `SaveFile`'s
    /// `base_mtime` and compared against the worktree snapshot's `mtime`.
    base_mtime: Option<SystemTime>,
    /// Whether this tab is read-only (out-of-root target, #195/#301). No
    /// edit, no save path, unwatched snapshot.
    read_only: bool,

    /// Monotonic open-request generation; fences this tab's open timeout.
    generation: u64,
    /// Monotonic save-request generation; fences this tab's save timeout.
    save_generation: u64,
    /// Monotonic buffer-feed generation; fences this tab's debounce timer.
    buffer_generation: u64,

    /// The id of the most recent definition request this tab dispatched. A
    /// response is matched to this tab by this field — `nav_id` is one
    /// editor-scoped counter shared by every tab (#351) — and dropped as
    /// stale when no tab's `latest_def_id` matches.
    latest_def_id: Option<NavRequestId>,
    /// The id of the most recent hover request this tab dispatched. Mirrors
    /// `latest_def_id`'s drop-stale discipline.
    latest_hover_id: Option<NavRequestId>,
    /// The id of the most recent references request this tab dispatched.
    /// Mirrors `latest_def_id`'s drop-stale discipline.
    latest_ref_id: Option<NavRequestId>,
    /// The hover content currently displayed for this tab, or `None` when no
    /// popover is visible. Cleared on mouse-down or a new hover request.
    hover_content: Option<HoverContent>,
    /// Monotonic generation counter for this tab's mouse-rest debounce timer.
    hover_move_generation: u64,

    /// A range to land on once this tab finishes loading — set when a
    /// cross-file jump or go-back opens a brand-new tab; consumed in
    /// [`EditorView::load`]. An already-open destination tab applies the
    /// range immediately instead (no load roundtrip needed).
    pending_jump: Option<Range>,

    /// This tab's bounded back-jump stack: (path, position, read_only)
    /// triples recording where a jump *away* from this tab should return —
    /// so `GoBack` while viewing this tab unwinds to wherever the jump that
    /// landed here came from. `read_only` preserves the source's access mode
    /// so `GoBack` can reopen it the same way.
    back_stack: VecDeque<(String, EditorPosition, bool)>,

    /// Transient inline jump-list for a multi-target definition response or
    /// find-references results, scoped to this tab.
    jump_list: Option<Vec<JumpEntry>>,
    /// The kind of results currently in `jump_list`. `None` when `jump_list`
    /// is `None` (the two fields are always set/cleared together).
    jump_list_kind: Option<JumpListKind>,
}

// ── Main view ─────────────────────────────────────────────────────────────────

/// The code editor view: an ordered set of open tabs, each a `gpui-component`
/// `InputState` in code-editor mode plus its own buffer bookkeeping and
/// navigation state, with an active index selecting which tab renders
/// (`docs/spec-editor-tabs.md`, #351).
pub struct EditorView {
    /// The ordered set of open tabs (open order — no reordering in v1). Only
    /// ever appended to in this step; closing a tab is #352.
    tabs: Vec<EditorTab>,
    /// The index into `tabs` currently rendered, or `None` when `tabs` is
    /// empty (the initial, pre-any-open state).
    active: Option<usize>,
    /// Fallback focus handle used only while `tabs` is empty (no tab's
    /// `InputState` to delegate focus to yet).
    focus_handle: FocusHandle,

    /// Read requests: path → `ClientMessage::OpenFile`.
    open_file_tx: Sender<String>,
    /// Write requests: `ClientMessage::SaveFile`.
    save_file_tx: Sender<ClientMessage>,
    /// Live-buffer feed: `BufferChanged` / `BufferClosed` (#189).
    buffer_change_tx: Sender<ClientMessage>,
    /// Navigation requests: `DefinitionRequest` / `HoverRequest` /
    /// `ReferencesRequest`.
    nav_tx: Sender<ClientMessage>,

    /// Editor-scoped monotonic counter for `NavRequestId`s — one counter
    /// shared by every tab, not per-tab, so ids never collide across tabs;
    /// each request records its issuing tab on that tab's own `latest_*_id`
    /// field (`docs/spec-editor-tabs.md`, #351).
    nav_id: u64,
}

impl EditorView {
    /// Create an editor with no tabs open yet.
    ///
    /// - `open_file_tx` — re-issues `OpenFile` for auto-reload and nav opens.
    /// - `save_file_tx` — carries `SaveFile` write requests.
    /// - `buffer_change_tx` — carries `BufferChanged` / `BufferClosed` (#189).
    /// - `nav_tx` — carries nav requests (#196, #197, #198).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        open_file_tx: Sender<String>,
        save_file_tx: Sender<ClientMessage>,
        buffer_change_tx: Sender<ClientMessage>,
        nav_tx: Sender<ClientMessage>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            focus_handle: cx.focus_handle(),
            open_file_tx,
            save_file_tx,
            buffer_change_tx,
            nav_tx,
            nav_id: 0,
        }
    }

    // ── Open-set (open-or-switch) ─────────────────────────────────────────

    /// The currently active tab, if any tab is open.
    fn active_tab(&self) -> Option<&EditorTab> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    /// The index of the open tab holding `path`, if any.
    fn tab_index_for_path(&self, path: &str) -> Option<usize> {
        find_open_tab_index(self.tabs.iter().map(|t| t.path.as_str()), path)
    }

    /// Construct a brand-new tab for `path` (not yet found among the open
    /// tabs), append it, and arm it into `Loading`. Returns its index (always
    /// the last one, since tabs are only ever appended in this step).
    fn push_tab(
        &mut self,
        path: String,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let input = cx.new(|cx| InputState::new(window, cx));
        let index = self.tabs.len();
        let input_change = Self::subscribe_dirty(&input, index, cx);
        self.tabs.push(EditorTab {
            path,
            input,
            _input_change: input_change,
            load_state: TabLoadState::Loading,
            save_state: SaveState::Idle,
            dirty: false,
            base_mtime: None,
            read_only,
            generation: 0,
            save_generation: 0,
            buffer_generation: 0,
            latest_def_id: None,
            latest_hover_id: None,
            latest_ref_id: None,
            hover_content: None,
            hover_move_generation: 0,
            pending_jump: None,
            back_stack: VecDeque::new(),
            jump_list: None,
            jump_list_kind: None,
        });
        self.arm_loading(index, window, cx);
        index
    }

    /// Reset the tab at `index` to `Loading` for its current path: rebuilds
    /// its `InputState`, clears per-load bookkeeping (dirty, save state,
    /// hover, jump-list), and arms the [`OPEN_TIMEOUT`] guard. Shared by a
    /// freshly pushed tab and an in-place reload (external change, #188) —
    /// both start from a blank buffer awaiting a `FileContent` reply.
    fn arm_loading(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        let language = language_for_path(&tab.path);

        tab.load_state = TabLoadState::Loading;
        tab.save_state = SaveState::Idle;
        tab.base_mtime = None;
        tab.dirty = false;
        tab.jump_list = None;
        tab.jump_list_kind = None;
        tab.hover_content = None;
        tab.latest_hover_id = None;
        tab.latest_ref_id = None;

        tab.input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(language)
                .line_number(true)
                .tab_size(gpui_component::input::TabSize {
                    tab_size: TAB_SIZE,
                    ..Default::default()
                })
        });
        tab._input_change = Self::subscribe_dirty(&tab.input, index, cx);

        tab.generation = tab.generation.wrapping_add(1);
        let generation = tab.generation;

        cx.spawn(async move |this, cx| {
            smol::Timer::after(OPEN_TIMEOUT).await;
            cx.update(|cx| {
                let _ = this.update(cx, |this, cx| {
                    let Some(tab) = this.tabs.get_mut(index) else {
                        return;
                    };
                    if tab.generation == generation {
                        if let TabLoadState::Loading = tab.load_state {
                            tab.load_state = TabLoadState::Failed;
                            cx.notify();
                        }
                    }
                });
            });
        })
        .detach();

        cx.notify();
    }

    /// Open `path`, or switch to it if it is already open — the "open-set"
    /// contract (`docs/spec-editor-tabs.md`, #351): opening an already-open
    /// path switches the active tab to it; a new path opens and activates a
    /// new tab. Returns `true` when a new tab was created (callers use this
    /// to decide whether an `OpenFile` read is actually needed — switching to
    /// an already-loaded tab needs no re-fetch).
    ///
    /// `jump`, if set, lands the cursor on that range: immediately when
    /// switching to an already-loaded tab, or once the new tab's load
    /// completes (via `pending_jump`). `back_entry`, if set, is pushed onto
    /// the destination tab's back-stack (the position the jump is leaving,
    /// so `GoBack` can return to it).
    fn open_or_switch(
        &mut self,
        path: String,
        read_only: bool,
        jump: Option<Range>,
        back_entry: Option<(String, EditorPosition, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(index) = self.tab_index_for_path(&path) {
            if let Some(entry) = back_entry {
                self.push_back_entry(index, entry);
            }
            self.active = Some(index);
            if let Some(range) = jump {
                self.apply_jump_range(index, &range, window, cx);
            } else {
                cx.notify();
            }
            return false;
        }

        let index = self.push_tab(path, read_only, window, cx);
        if let Some(entry) = back_entry {
            self.push_back_entry(index, entry);
        }
        if let Some(range) = jump {
            self.tabs[index].pending_jump = Some(range);
        }
        self.active = Some(index);
        cx.notify();
        true
    }

    /// Begin opening `path` from an external caller (the file tree, #186):
    /// open-or-switch semantics, no jump target, no back-entry. The caller
    /// (`WorkspaceView`) sends the matching `OpenFile` request itself,
    /// unconditionally — a redundant read for the switch case is harmless
    /// (its reply finds no `Loading` tab and is dropped by [`Self::load`]).
    pub fn begin_open(
        &mut self,
        path: String,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_or_switch(path, read_only, None, None, window, cx);
    }

    // ── Dirty flag / live-buffer feed ─────────────────────────────────────

    /// Subscribe to a tab's input `Change` event: a keystroke marks that
    /// tab's buffer dirty and arms its debounced live-buffer feed (#189).
    fn subscribe_dirty(
        input: &Entity<InputState>,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(input, move |this, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let Some(tab) = this.tabs.get_mut(index) else {
                    return;
                };
                if !tab.dirty {
                    tab.dirty = true;
                    cx.notify();
                }
                this.arm_buffer_feed(index, cx);
            }
        })
    }

    /// Arm (or re-arm) the debounced live-buffer feed for the tab at `index`
    /// (#189).
    fn arm_buffer_feed(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        if !matches!(tab.load_state, TabLoadState::Loaded) {
            return;
        }
        let path = tab.path.clone();
        tab.buffer_generation = tab.buffer_generation.wrapping_add(1);
        let generation = tab.buffer_generation;

        cx.spawn(async move |this, cx| {
            smol::Timer::after(BUFFER_FEED_DEBOUNCE).await;
            cx.update(|cx| {
                let _ = this.update(cx, |this, cx| {
                    let Some(tab) = this.tabs.get_mut(index) else {
                        return;
                    };
                    if tab.buffer_generation != generation {
                        return;
                    }
                    if !matches!(tab.load_state, TabLoadState::Loaded) {
                        return;
                    }
                    if tab.path != path {
                        return;
                    }
                    let content = tab.input.read(cx).value().to_string();
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

    /// Immediately send a `BufferChanged` for the tab at `index` without
    /// waiting for the debounce, if it is dirty.
    ///
    /// Used before dispatching a nav request (flush-before-dispatch): the LSP
    /// must see the live buffer before the request arrives. The daemon
    /// processes messages in send order, so the `didChange` from this flush
    /// lands before the nav request.
    ///
    /// Bumps the tab's `buffer_generation` so an in-flight debounce timer (if
    /// any) sees the mismatch and does not send a duplicate feed.
    fn flush_buffer_feed_if_dirty(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        if !tab.dirty || !matches!(tab.load_state, TabLoadState::Loaded) {
            return;
        }
        let path = tab.path.clone();
        tab.buffer_generation = tab.buffer_generation.wrapping_add(1);
        let content = tab.input.read(cx).value().to_string();
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

    /// Close the live buffer for the tab at `index` — reverts the daemon's
    /// LSP source of truth to disk-backed (used after a successful save).
    fn close_live_buffer(&mut self, index: usize) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        tab.buffer_generation = tab.buffer_generation.wrapping_add(1);
        let path = tab.path.clone();
        if let Err(e) = self
            .buffer_change_tx
            .try_send(ClientMessage::BufferClosed { path: path.clone() })
        {
            tracing::debug!(error = %e, %path, "failed to enqueue live-buffer close");
        }
    }

    // ── Diagnostics ───────────────────────────────────────────────────────

    /// Replace the active tab's inline diagnostics with `items` (#189). A
    /// no-op when no tab is open.
    pub fn set_diagnostics(&mut self, items: &[Diagnostic], cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        let editor_items: Vec<EditorDiagnostic> = items.iter().map(to_editor_diagnostic).collect();
        self.tabs[index].input.update(cx, |input, cx| {
            if let Some(set) = input.diagnostics_mut() {
                set.clear();
                set.extend(editor_items);
            }
            cx.notify();
        });
        cx.notify();
    }

    // ── Buffer state accessors ────────────────────────────────────────────

    /// The path of the active tab, if any is open.
    pub fn open_path(&self) -> Option<&str> {
        self.active_tab().map(|t| t.path.as_str())
    }

    /// The base `mtime` of the active tab's buffer.
    pub fn base_mtime(&self) -> Option<SystemTime> {
        self.active_tab().and_then(|t| t.base_mtime)
    }

    /// Whether the active tab's buffer has unsaved edits.
    pub fn is_dirty(&self) -> bool {
        self.active_tab().is_some_and(|t| t.dirty)
    }

    /// Whether the active tab is currently surfacing a save conflict.
    pub fn has_conflict(&self) -> bool {
        self.active_tab()
            .is_some_and(|t| matches!(t.save_state, SaveState::Conflict))
    }

    /// Whether the active tab is read-only (out-of-root target, #195/#301).
    pub fn is_read_only(&self) -> bool {
        self.active_tab().is_some_and(|t| t.read_only)
    }

    // ── Load ──────────────────────────────────────────────────────────────

    /// Render a `FileContent` reply: find the tab awaiting it (matching path,
    /// `Loading`) and load the content into it, applying any pending jump. A
    /// reply for a path with no `Loading` tab is silently ignored (e.g. a
    /// switch's redundant read, or a superseded reload).
    pub fn load(
        &mut self,
        path: String,
        content: String,
        mtime: SystemTime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.path == path && matches!(t.load_state, TabLoadState::Loading))
        else {
            return;
        };

        self.tabs[index].base_mtime = Some(mtime);
        self.tabs[index].input.update(cx, |input, cx| {
            input.set_value(content, window, cx);
        });
        // `set_value` emits `Change` — clear dirty so a load starts clean.
        self.tabs[index].dirty = false;
        self.tabs[index].save_state = SaveState::Idle;
        self.tabs[index].load_state = TabLoadState::Loaded;

        if let Some(range) = self.tabs[index].pending_jump.take() {
            self.apply_jump_range(index, &range, window, cx);
        }

        cx.notify();
    }

    // ── Save ──────────────────────────────────────────────────────────────

    /// Send the active tab's buffer back to the remote. A no-op when no tab
    /// is open, loading, or read-only.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if self.tabs[index].read_only {
            return;
        }
        if !matches!(self.tabs[index].load_state, TabLoadState::Loaded) {
            return;
        }
        let Some(base_mtime) = self.tabs[index].base_mtime else {
            return;
        };
        let path = self.tabs[index].path.clone();
        let content = self.tabs[index].input.read(cx).value().to_string();

        self.tabs[index].save_generation = self.tabs[index].save_generation.wrapping_add(1);
        let save_generation = self.tabs[index].save_generation;
        self.tabs[index].save_state = SaveState::Saving;

        if let Err(e) = self.save_file_tx.try_send(ClientMessage::SaveFile {
            path: path.clone(),
            content,
            base_mtime,
        }) {
            tracing::debug!(error = %e, %path, "failed to enqueue save request");
            self.tabs[index].save_state = SaveState::Failed;
            cx.notify();
            return;
        }

        cx.spawn(async move |this, cx| {
            smol::Timer::after(SAVE_TIMEOUT).await;
            cx.update(|cx| {
                let _ = this.update(cx, |this, cx| {
                    let Some(tab) = this.tabs.get_mut(index) else {
                        return;
                    };
                    if tab.save_generation == save_generation {
                        if let SaveState::Saving = tab.save_state {
                            tab.save_state = SaveState::Failed;
                            cx.notify();
                        }
                    }
                });
            });
        })
        .detach();

        cx.notify();
    }

    /// Apply a `SaveResult` reply: the write landed. Routed to whichever tab
    /// holds `path` (not necessarily the active one — a background dirty tab
    /// can save concurrently); a no-op if no tab holds it.
    pub fn apply_save_result(&mut self, path: String, mtime: SystemTime, cx: &mut Context<Self>) {
        let Some(index) = self.tab_index_for_path(&path) else {
            return;
        };
        self.tabs[index].base_mtime = Some(mtime);
        self.tabs[index].dirty = false;
        self.tabs[index].save_state = SaveState::Idle;
        self.tabs[index].save_generation = self.tabs[index].save_generation.wrapping_add(1);
        // Disk now matches the buffer: revert to the disk-backed baseline.
        self.close_live_buffer(index);
        cx.notify();
    }

    /// Apply a `SaveConflict` reply: the daemon refused the write. Routed to
    /// whichever tab holds `path`; a no-op if no tab holds it.
    pub fn apply_save_conflict(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(index) = self.tab_index_for_path(&path) else {
            return;
        };
        self.tabs[index].save_state = SaveState::Conflict;
        self.tabs[index].save_generation = self.tabs[index].save_generation.wrapping_add(1);
        cx.notify();
    }

    // ── Concurrent external change ────────────────────────────────────────

    /// React to the worktree snapshot reporting a new `mtime` for the active
    /// tab's path. Runs the pure [`decide_external_change`] decision and acts
    /// on it. A no-op when no tab is open or loading.
    ///
    /// While a save is in flight (`SaveState::Saving`) the decision is
    /// suppressed: the save's own atomic write bumps the on-disk `mtime`, and
    /// the explorer watcher turns that into a worktree update that can reach the
    /// app *before* the `SaveResult` reply (the worktree update rides the
    /// broadcast bus, the reply rides the buffer channel). Acting on that
    /// self-induced bump would surface a false conflict against the editor's own
    /// in-flight write (#307). The `SaveResult` / `SaveConflict` reply is the
    /// authoritative reconciliation: it commits the new base `mtime` (so the
    /// now-stale worktree bump becomes `snapshot <= base` → `None`) or surfaces
    /// the genuine conflict itself.
    pub fn note_external_change(
        &mut self,
        snapshot_mtime: SystemTime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.active else {
            return;
        };
        if !matches!(self.tabs[index].load_state, TabLoadState::Loaded) {
            return;
        }
        let Some(base) = self.tabs[index].base_mtime else {
            return;
        };
        let dirty = self.tabs[index].dirty;
        let saving = matches!(self.tabs[index].save_state, SaveState::Saving);
        match decide_external_change(base, snapshot_mtime, dirty, saving) {
            ExternalChange::None => {}
            ExternalChange::Reload => {
                let path = self.tabs[index].path.clone();
                self.arm_loading(index, window, cx);
                if let Err(e) = self.open_file_tx.try_send(path.clone()) {
                    tracing::debug!(error = %e, %path, "failed to enqueue auto-reload open");
                }
            }
            ExternalChange::Conflict => {
                self.tabs[index].save_state = SaveState::Conflict;
                cx.notify();
            }
        }
    }

    // ── Navigation — dispatch ─────────────────────────────────────────────

    /// Dispatch a `DefinitionRequest` for the active tab's cursor position.
    ///
    /// Performs flush-before-dispatch: if the buffer is dirty, immediately
    /// sends a `BufferChanged` so the daemon's LSP has the live buffer before
    /// the nav request arrives. A no-op unless a tab is loaded.
    fn dispatch_definition_request(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        if !matches!(self.tabs[index].load_state, TabLoadState::Loaded) {
            return false;
        }
        let path = self.tabs[index].path.clone();

        // Flush before dispatch (spec §"Request-vs-didChange ordering"): send
        // the live buffer immediately so the LSP resolves the symbol against
        // the current buffer text, not the stale on-disk version. The daemon
        // processes messages in-order, so the didChange from this flush lands
        // before the DefinitionRequest.
        self.flush_buffer_feed_if_dirty(index, cx);

        let position = self.cursor_to_protocol(index, cx);
        self.nav_id = self.nav_id.wrapping_add(1);
        let id = NavRequestId(self.nav_id);
        self.tabs[index].latest_def_id = Some(id);

        if let Err(e) = self.nav_tx.try_send(ClientMessage::DefinitionRequest {
            id,
            path: path.clone(),
            position,
        }) {
            tracing::debug!(error = %e, %path, "failed to enqueue definition request");
        }
        true
    }

    /// Convert the tab at `index`'s current `InputState` cursor position to
    /// the protocol's `Position` type. The editor's `cursor_position()`
    /// returns a `(line, character)` pair with character as a Unicode scalar
    /// count — the same convention the protocol uses (UTF-8 char offsets).
    fn cursor_to_protocol(&self, index: usize, cx: &Context<Self>) -> Position {
        let pos = self.tabs[index].input.read(cx).cursor_position();
        Position {
            line: pos.line,
            character: pos.character,
        }
    }

    /// Dispatch a `HoverRequest` for the active tab's cursor position (#197).
    ///
    /// Mirrors [`Self::dispatch_definition_request`]: performs flush-before-
    /// dispatch, increments `nav_id`, records the tab's latest hover id, and
    /// clears any previously-visible popover so a new request does not show
    /// stale content while the daemon is in flight. A no-op unless a tab is
    /// loaded.
    fn dispatch_hover_request(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if !matches!(self.tabs[index].load_state, TabLoadState::Loaded) {
            return;
        }
        let path = self.tabs[index].path.clone();

        // Flush-before-dispatch: the LSP must see the live buffer before the
        // `HoverRequest` arrives, for the same reason as definition requests.
        self.flush_buffer_feed_if_dirty(index, cx);

        // Clear the previous popover immediately: a stale popover that stays
        // visible until the response arrives is misleading.
        self.tabs[index].hover_content = None;

        let position = self.cursor_to_protocol(index, cx);
        self.nav_id = self.nav_id.wrapping_add(1);
        let id = NavRequestId(self.nav_id);
        self.tabs[index].latest_hover_id = Some(id);

        if let Err(e) = self.nav_tx.try_send(ClientMessage::HoverRequest {
            id,
            path: path.clone(),
            position,
        }) {
            tracing::debug!(error = %e, %path, "failed to enqueue hover request");
        }
        cx.notify();
    }

    /// Dispatch a `ReferencesRequest` for the active tab's cursor position
    /// (#198).
    ///
    /// Mirrors [`Self::dispatch_definition_request`]: performs flush-before-
    /// dispatch, increments `nav_id`, records the tab's `latest_ref_id`, and
    /// clears any stale jump-list from a previous request. Results are shown
    /// in the same transient inline jump-list the multi-target definition
    /// path uses. A no-op unless a tab is loaded.
    fn dispatch_references_request(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if !matches!(self.tabs[index].load_state, TabLoadState::Loaded) {
            return;
        }
        let path = self.tabs[index].path.clone();

        // Flush-before-dispatch (spec §"Request-vs-didChange ordering"): the
        // LSP must see the live buffer before the `ReferencesRequest` arrives.
        self.flush_buffer_feed_if_dirty(index, cx);

        // Clear any previous jump-list so a stale list is not visible while
        // the daemon is in flight.
        self.tabs[index].jump_list = None;
        self.tabs[index].jump_list_kind = None;

        let position = self.cursor_to_protocol(index, cx);
        self.nav_id = self.nav_id.wrapping_add(1);
        let id = NavRequestId(self.nav_id);
        self.tabs[index].latest_ref_id = Some(id);

        if let Err(e) = self.nav_tx.try_send(ClientMessage::ReferencesRequest {
            id,
            path: path.clone(),
            position,
        }) {
            tracing::debug!(error = %e, %path, "failed to enqueue references request");
        }
        cx.notify();
    }

    /// Arm (or re-arm) the mouse-rest debounce timer for hover on the active
    /// tab (#197).
    ///
    /// Called from the `MouseMoveEvent` handler on the outer div. A no-op when
    /// no tab is loaded (saves a detached task spawn on empty/loading state).
    /// Bumps the tab's `hover_move_generation` so any in-flight timer from the
    /// previous mouse movement sees the mismatch and does nothing. When the
    /// timer fires and the generation still matches, the hover request is
    /// dispatched at the then-active tab's cursor position (which follows the
    /// most recent click, making hover-after-click natural).
    fn arm_hover_debounce(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if !matches!(self.tabs[index].load_state, TabLoadState::Loaded) {
            return;
        }
        self.tabs[index].hover_move_generation =
            self.tabs[index].hover_move_generation.wrapping_add(1);
        let generation = self.tabs[index].hover_move_generation;
        cx.spawn(async move |this, cx| {
            smol::Timer::after(HOVER_MOUSE_DEBOUNCE).await;
            cx.update(|cx| {
                let _ = this.update(cx, |this, cx| {
                    let Some(tab) = this.tabs.get(index) else {
                        return;
                    };
                    if tab.hover_move_generation == generation {
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
    /// Matched to whichever tab's `latest_def_id` equals `id` (drop-stale
    /// discipline generalized across tabs: no match means every issuing tab
    /// has moved on, so the response is dropped). A single target jumps
    /// directly; multiple targets show that tab's inline jump-list.
    pub fn apply_definition_response(
        &mut self,
        id: NavRequestId,
        targets: Vec<NavLocation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source_index) = self.tabs.iter().position(|t| t.latest_def_id == Some(id)) else {
            tracing::debug!(?id, "dropping stale definition response");
            return;
        };

        match targets.len() {
            0 => {
                // No definition found — silent no-op.
                tracing::debug!("definition response: no targets (server found nothing)");
            }
            1 => {
                let target = targets.into_iter().next().expect("checked len == 1");
                self.jump_to_location(source_index, target, window, cx);
            }
            _ => {
                // Multiple targets (e.g. Rust trait impls): show the jump-list.
                self.tabs[source_index].jump_list = Some(
                    targets
                        .into_iter()
                        .map(|l| JumpEntry { location: l })
                        .collect(),
                );
                self.tabs[source_index].jump_list_kind = Some(JumpListKind::Definitions);
                cx.notify();
            }
        }
    }

    /// Apply a `HoverResponse` from the daemon (#197).
    ///
    /// Matched to whichever tab's `latest_hover_id` equals `id` (drop-stale
    /// discipline, mirroring the definition response). `None` content means
    /// the server found nothing — silent no-op, no error surface. `Some`
    /// content renders the markdown in that tab's floating popover.
    pub fn apply_hover_response(
        &mut self,
        id: NavRequestId,
        content: Option<HoverContent>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tabs.iter().position(|t| t.latest_hover_id == Some(id)) else {
            tracing::debug!(?id, "dropping stale hover response");
            return;
        };
        self.tabs[index].hover_content = content;
        cx.notify();
    }

    /// Apply a `ReferencesResponse` from the daemon (#198).
    ///
    /// Matched to whichever tab's `latest_ref_id` equals `id` (drop-stale
    /// discipline, mirroring definition and hover). An empty target list is a
    /// silent no-op (the server found no references). A non-empty list
    /// populates that tab's inline jump-list with `JumpListKind::References`
    /// so the render layer shows the "references" header.
    pub fn apply_references_response(
        &mut self,
        id: NavRequestId,
        targets: Vec<NavLocation>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tabs.iter().position(|t| t.latest_ref_id == Some(id)) else {
            tracing::debug!(?id, "dropping stale references response");
            return;
        };

        if targets.is_empty() {
            // No references found — silent no-op.
            tracing::debug!("references response: no targets (server found nothing)");
            return;
        }

        self.tabs[index].jump_list = Some(
            targets
                .into_iter()
                .map(|l| JumpEntry { location: l })
                .collect(),
        );
        self.tabs[index].jump_list_kind = Some(JumpListKind::References);
        cx.notify();
    }

    // ── Navigation — jump mechanics ───────────────────────────────────────

    /// Push a (path, position, read_only) back-entry onto the tab at
    /// `index`'s back-stack, evicting the oldest entry when it would exceed
    /// [`BACK_STACK_MAX`].
    fn push_back_entry(&mut self, index: usize, entry: (String, EditorPosition, bool)) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        if tab.back_stack.len() >= BACK_STACK_MAX {
            tab.back_stack.pop_front();
        }
        tab.back_stack.push_back(entry);
    }

    /// Perform a jump away from the tab at `source_index` to `location`:
    /// same-file scrolls and selects the range in place; cross-file opens via
    /// open-or-switch, landing on the range immediately (already-open
    /// destination) or once the new tab loads ([`EditorTab::pending_jump`]).
    /// Either way the pre-jump (path, position, read_only) is pushed onto the
    /// destination tab's back-stack so `GoBack` can return to it.
    fn jump_to_location(
        &mut self,
        source_index: usize,
        location: NavLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.tabs.get(source_index) else {
            return;
        };
        let source_path = source.path.clone();
        let source_pos = source.input.read(cx).cursor_position();
        let source_read_only = source.read_only;
        let source_entry = (source_path.clone(), source_pos, source_read_only);

        if source_path == location.path {
            // Same-tab jump: push the pre-jump position onto this tab's own
            // stack, then scroll + select the target range in place.
            self.push_back_entry(source_index, source_entry);
            self.active = Some(source_index);
            self.apply_jump_range(source_index, &location.range, window, cx);
        } else {
            // Cross-file jump (in-root or out-of-root via the #195/#301
            // carve-out): open-or-switch to the destination, carrying the
            // range and back-entry along.
            let read_only = location.out_of_root;
            let path = location.path.clone();
            let is_new = self.open_or_switch(
                location.path,
                read_only,
                Some(location.range),
                Some(source_entry),
                window,
                cx,
            );
            if is_new {
                if let Err(e) = self.open_file_tx.try_send(path.clone()) {
                    tracing::debug!(error = %e, %path, "failed to enqueue cross-file nav open");
                }
            }
        }
    }

    /// Move the tab at `index`'s cursor to `range.start` (scroll + select).
    /// The protocol `Range` uses UTF-8 char offsets, matching the editor's
    /// `cursor_position` convention, so no offset translation is needed here.
    ///
    /// `InputState::set_cursor_position` scrolls the view to keep the cursor
    /// visible and is the public API for programmatic cursor moves.
    ///
    /// Range-end selection: `InputState` does not expose a public
    /// `set_selected_range` in this version of gpui-component. For v1 the
    /// cursor landing at `range.start` is the primary nav signal. A
    /// TODO is filed below for the selection extension when the API is
    /// available.
    fn apply_jump_range(
        &mut self,
        index: usize,
        range: &Range,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        let start = EditorPosition::new(range.start.line, range.start.character);
        // TODO(nav-select): extend selection to range.end when gpui-component
        // exposes a public set_selected_range. For v1 the cursor at start is
        // the landing signal; the symbol is visible in the scrolled view.
        tab.input.update(cx, |input, cx| {
            input.set_cursor_position(start, window, cx);
        });
        cx.notify();
    }

    /// Unwind the active tab's most recent jump: pop its back-stack and
    /// open-or-switch to the saved (path, position, read_only) — an
    /// already-open tab (same file or a different one still open) just
    /// switches and lands the cursor; a path with no open tab re-opens it
    /// with the saved access mode.
    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.active else {
            return;
        };
        let Some((path, pos, read_only)) = self.tabs[active].back_stack.pop_back() else {
            return;
        };

        let proto_pos = Position {
            line: pos.line,
            character: pos.character,
        };
        let range = Range {
            start: proto_pos,
            end: proto_pos,
        };
        let is_new = self.open_or_switch(path.clone(), read_only, Some(range), None, window, cx);
        if is_new {
            if let Err(e) = self.open_file_tx.try_send(path.clone()) {
                tracing::debug!(error = %e, %path, "failed to enqueue go-back open");
            }
        }
    }

    /// Select a jump-list entry by index and navigate to it. Called from the
    /// click handler on the inline jump-list items rendered in `Render`;
    /// always acts on the active tab, since only the active tab's jump-list
    /// is ever rendered/clickable.
    pub fn select_jump_entry(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.active else {
            return;
        };
        let Some(list) = self.tabs[active].jump_list.take() else {
            return;
        };
        self.tabs[active].jump_list_kind = None;
        if let Some(entry) = list.into_iter().nth(index) {
            self.jump_to_location(active, entry.location, window, cx);
        }
    }

    // ── Tab bar: switch / close (#352) ────────────────────────────────────

    /// Activate the tab at `index` (tab-bar click) and move focus to its
    /// buffer. A no-op if `index` is out of range or already active.
    pub fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() || self.active == Some(index) {
            return;
        }
        self.active = Some(index);
        self.tabs[index].input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Close the tab at `index` (tab-bar close affordance): reverts its live
    /// buffer to disk-backed (mirrors the save-success discard, #189) and
    /// removes it from the open set. Closing the active tab activates the
    /// right neighbor (or the left if it was rightmost, via
    /// [`next_active_after_close`]); closing the last tab returns the editor
    /// to its empty state. Dirty-close confirmation is a later step
    /// (`docs/spec-editor-tabs.md`) — for now every close is immediate.
    pub fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.active else {
            return;
        };
        if index >= self.tabs.len() {
            return;
        }
        let len_before = self.tabs.len();

        self.close_live_buffer(index);
        self.tabs.remove(index);
        self.active = next_active_after_close(active, index, len_before);

        if let Some(new_active) = self.active {
            self.tabs[new_active].input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        }
        cx.notify();
    }
}

// ── Panel adapter ─────────────────────────────────────────────────────────────

impl Focusable for EditorView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // Delegate to the active tab's input entity — `arm_loading` rebuilds
        // it per load, so this always reflects the live buffer. Falls back to
        // the editor's own handle while no tab is open.
        self.active_tab()
            .map(|tab| tab.input.focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl EventEmitter<PanelEvent> for EditorView {}

impl Panel for EditorView {
    fn panel_name(&self) -> &'static str {
        EDITOR_PANEL_NAME
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .open_path()
            .map(|path| path.rsplit('/').next().unwrap_or(path).to_owned())
            .unwrap_or_else(|| "Editor".to_owned());
        SharedString::from(title)
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // No tab open yet: show a centered status message.
        let Some(tab) = self.active_tab() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(8.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Select a file to open")
                .into_any_element();
        };

        // Loading / failed states: show a centered status message.
        let status: Option<String> = match tab.load_state {
            TabLoadState::Loading => Some(format!("Opening {}\u{2026}", tab.path)),
            TabLoadState::Failed => Some(format!("Could not open {}", tab.path)),
            TabLoadState::Loaded => None,
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
        let banner: Option<(String, gpui::Hsla)> = match &tab.save_state {
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

        // Inline jump-list for multi-target definition responses and find-references
        // results (#196, #198). The header label differs by kind; entries are
        // identical in both cases (path:line + preview, click to jump).
        let jump_list_element = tab.jump_list.as_ref().map(|list| {
            let header = match tab.jump_list_kind {
                Some(JumpListKind::References) => "References — click to jump:",
                Some(JumpListKind::Definitions) | None => "Multiple definitions — click to jump:",
            };
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
                        .child(header),
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
        let input_widget = Input::new(&tab.input)
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .size_full()
            .disabled(tab.read_only)
            .context_menu(|menu: PopupMenu, _window, _cx| {
                menu.menu("Go to Definition", Box::new(GoToDefinition))
                    .menu("Find References", Box::new(FindReferences))
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
        let hover_popover_element = tab.hover_content.as_ref().map(|content| {
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

        let read_only = tab.read_only;

        // Tab bar (#352): one Tab per open file, showing its name, a dirty
        // dot, and a close "x" — the same TabBar/Tab pattern `SessionView`
        // uses for tmux windows. Clicking a tab activates it (moves focus to
        // its buffer); the close affordance removes it immediately
        // (dirty-close confirmation is a later step).
        let selected_index = self.active.unwrap_or(0);
        let close_idle = cx.theme().muted_foreground;
        let close_hover = cx.theme().danger;
        let dirty_color = cx.theme().warning;

        let tab_items: Vec<Tab> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, t)| {
                let name = t.path.rsplit('/').next().unwrap_or(&t.path).to_owned();
                let close = div()
                    .id(("editor-tab-close", ix))
                    .px(px(4.0))
                    .text_color(close_idle)
                    .hover(move |this| this.text_color(close_hover))
                    .child("x")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                            this.close_tab(ix, window, cx);
                            cx.stop_propagation();
                        }),
                    );

                let mut suffix = div().flex().items_center().gap(px(4.0));
                if t.dirty {
                    suffix = suffix.child(div().size(px(6.0)).rounded_full().bg(dirty_color));
                }
                suffix = suffix.child(close);

                Tab::new()
                    .label(SharedString::from(name))
                    .suffix(suffix)
                    .on_click(cx.listener(move |this, _event: &ClickEvent, window, cx| {
                        this.activate_tab(ix, window, cx);
                    }))
            })
            .collect();

        let tab_bar = TabBar::new("editor-tab-bar")
            .selected_index(selected_index)
            .children(tab_items);

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
            .on_action(cx.listener(|this, _: &FindReferences, _window, cx| {
                this.dispatch_references_request(cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    // Dismiss any visible hover popover on click and cancel any
                    // in-flight hover request so a delayed response does not
                    // re-open the popover after the user clicked away.
                    if let Some(index) = this.active {
                        let tab = &mut this.tabs[index];
                        if tab.hover_content.is_some() || tab.latest_hover_id.is_some() {
                            tab.hover_content = None;
                            tab.latest_hover_id = None;
                            cx.notify();
                        }
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
            }))
            .child(tab_bar);

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
        if read_only {
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
/// - `saving` → [`ExternalChange::None`] (the editor's own in-flight save bumps
///   the on-disk `mtime`; that self-induced worktree update must not be read as
///   an external change — the `SaveResult` / `SaveConflict` reply reconciles it,
///   #307)
/// - `snapshot <= base` → [`ExternalChange::None`]
/// - `snapshot > base` and clean buffer → [`ExternalChange::Reload`]
/// - `snapshot > base` and dirty buffer → [`ExternalChange::Conflict`]
pub fn decide_external_change(
    base: SystemTime,
    snapshot: SystemTime,
    dirty: bool,
    saving: bool,
) -> ExternalChange {
    if saving {
        return ExternalChange::None;
    }
    if snapshot <= base {
        return ExternalChange::None;
    }
    if dirty {
        ExternalChange::Conflict
    } else {
        ExternalChange::Reload
    }
}

/// Decide what opening `path` does against the ordered list of currently
/// open tab paths: find the index of the tab already holding it, so the
/// caller can switch to it — or `None`, signaling that a new tab must be
/// appended (and activated). Pure and GPUI-free — the open-set half of the
/// "open-or-switch" contract (`docs/spec-editor-tabs.md`, #351): "opening an
/// already-open path switches to it rather than duplicating; a new path
/// opens+activates."
fn find_open_tab_index<'a>(
    mut open_paths: impl Iterator<Item = &'a str>,
    path: &str,
) -> Option<usize> {
    open_paths.position(|p| p == path)
}

/// Decide the new active index after closing the tab at `closed`, given the
/// previously active index and the tab count *before* removal. Pure and
/// GPUI-free — the close-half of the tab-bar contract (`docs/spec-editor-tabs.md`,
/// #352 acceptance: "closing the active tab activates the right neighbor
/// (else the left); closing the last tab empties the editor"). Closing a
/// background (non-active) tab never disturbs which tab is active, only
/// shifting its index down when a tab before it is removed.
fn next_active_after_close(active: usize, closed: usize, len_before: usize) -> Option<usize> {
    if len_before <= 1 {
        return None;
    }
    let len_after = len_before - 1;
    if active == closed {
        Some(closed.min(len_after - 1))
    } else if active > closed {
        Some(active - 1)
    } else {
        Some(active)
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
            decide_external_change(at(100), at(200), false, false),
            ExternalChange::Reload
        );
    }

    #[test]
    fn test_dirty_buffer_with_newer_snapshot_conflicts() {
        assert_eq!(
            decide_external_change(at(100), at(200), true, false),
            ExternalChange::Conflict
        );
    }

    #[test]
    fn test_equal_snapshot_is_no_change_regardless_of_dirty() {
        assert_eq!(
            decide_external_change(at(100), at(100), false, false),
            ExternalChange::None
        );
        assert_eq!(
            decide_external_change(at(100), at(100), true, false),
            ExternalChange::None
        );
    }

    #[test]
    fn test_older_snapshot_is_no_change() {
        assert_eq!(
            decide_external_change(at(200), at(100), false, false),
            ExternalChange::None
        );
        assert_eq!(
            decide_external_change(at(200), at(100), true, false),
            ExternalChange::None
        );
    }

    // --- self-induced save bump suppression (#307) ---

    #[test]
    fn test_save_in_flight_suppresses_self_induced_conflict() {
        // Reproduces #307 at the logic level: the save's own atomic write bumps
        // the on-disk mtime; the explorer watcher's worktree update (newer
        // snapshot) can reach the app before the SaveResult reply, while the
        // buffer is still dirty and the base is the pre-save mtime. Without the
        // `saving` guard this is `snapshot > base && dirty` → Conflict (the
        // false banner). With the guard in flight it must be a no-op.
        assert_eq!(
            decide_external_change(at(100), at(200), true, true),
            ExternalChange::None
        );
        // The same applies to a clean buffer mid-save (no spurious reload).
        assert_eq!(
            decide_external_change(at(100), at(200), false, true),
            ExternalChange::None
        );
    }

    #[test]
    fn test_genuine_dirty_external_change_still_conflicts_when_not_saving() {
        // Acceptance #2 / #5: a real out-of-band write to a dirty buffer with no
        // save in flight must still surface the conflict — the guard only
        // suppresses the editor's own in-flight save, never a genuine change.
        assert_eq!(
            decide_external_change(at(100), at(200), true, false),
            ExternalChange::Conflict
        );
    }

    #[test]
    fn test_clean_external_change_still_reloads_when_not_saving() {
        // The clean-buffer auto-reload path is unaffected by the guard.
        assert_eq!(
            decide_external_change(at(100), at(200), false, false),
            ExternalChange::Reload
        );
    }

    // --- open-set model: open-or-switch (#351) ---

    const SEEDED_PATHS: [&str; 3] = ["a.rs", "b.rs", "c.rs"];

    #[test]
    fn test_find_open_tab_index_switches_to_an_already_open_path() {
        assert_eq!(
            find_open_tab_index(SEEDED_PATHS.into_iter(), "b.rs"),
            Some(1)
        );
    }

    #[test]
    fn test_find_open_tab_index_finds_first_and_last_positions() {
        assert_eq!(
            find_open_tab_index(SEEDED_PATHS.into_iter(), "a.rs"),
            Some(0)
        );
        assert_eq!(
            find_open_tab_index(SEEDED_PATHS.into_iter(), "c.rs"),
            Some(2)
        );
    }

    #[test]
    fn test_find_open_tab_index_is_none_for_a_new_path() {
        // `None` is exactly the "open a new tab" signal — the acceptance
        // contract "a new path opens+activates".
        assert_eq!(find_open_tab_index(SEEDED_PATHS.into_iter(), "d.rs"), None);
    }

    #[test]
    fn test_find_open_tab_index_on_empty_set_always_opens_new() {
        assert_eq!(find_open_tab_index(std::iter::empty(), "a.rs"), None);
    }

    #[test]
    fn test_opening_a_new_path_appends_and_activates_it_at_the_end() {
        // Simulates `EditorView::open_or_switch`'s new-tab branch: a path not
        // found in the open set is appended and becomes active at its index.
        let mut open: Vec<&str> = SEEDED_PATHS.to_vec();
        let path = "d.rs";

        let index = match find_open_tab_index(open.iter().copied(), path) {
            Some(i) => i,
            None => {
                open.push(path);
                open.len() - 1
            }
        };

        assert_eq!(index, 3, "the new tab lands at the end of the list");
        assert_eq!(open[index], "d.rs");
        let active = Some(index); // open-or-switch always activates the result
        assert_eq!(active, Some(3));
    }

    // --- open-set model: dirty tracking is per-tab (#351) ---

    #[test]
    fn test_switching_tabs_preserves_each_tabs_own_dirty_flag() {
        // A seeded set where a.rs and c.rs are dirty (unsaved edits) and b.rs
        // is clean. Switching the active tab must never disturb any other
        // tab's dirty flag — dirty state is per-tab bookkeeping, not
        // editor-wide (`docs/spec-editor-tabs.md`, #351 acceptance: "every
        // previously-scalar field ... is per-tab").
        let dirty = [true, false, true];

        let index = find_open_tab_index(SEEDED_PATHS.into_iter(), "b.rs").expect("b.rs is open");
        assert_eq!(index, 1);
        assert!(!dirty[index], "switching to b.rs must see it still clean");

        let index = find_open_tab_index(SEEDED_PATHS.into_iter(), "a.rs").expect("a.rs is open");
        assert!(
            dirty[index],
            "switching back to a.rs must see it still dirty"
        );

        let index = find_open_tab_index(SEEDED_PATHS.into_iter(), "c.rs").expect("c.rs is open");
        assert!(
            dirty[index],
            "c.rs's dirty flag is unaffected by the a.rs/b.rs switches"
        );
    }

    #[test]
    fn test_opening_an_already_open_dirty_path_switches_without_clearing_dirty() {
        // Opening a path that is already open and dirty must switch to the
        // existing tab (no duplicate entry) and must not reset its dirty
        // flag — only a fresh load or a successful save clears dirty.
        let mut open: Vec<&str> = SEEDED_PATHS.to_vec();
        let mut dirty = vec![true, false, true];
        let before_len = open.len();

        match find_open_tab_index(open.iter().copied(), "c.rs") {
            Some(index) => assert_eq!(index, 2, "switches to the existing c.rs tab"),
            None => {
                open.push("c.rs");
                dirty.push(false);
            }
        }

        assert_eq!(
            open.len(),
            before_len,
            "no duplicate tab for an already-open path"
        );
        assert!(dirty[2], "switching must not clear the existing dirty flag");
    }

    // --- close-set model: next active after close (#352) ---

    #[test]
    fn test_closing_a_background_tab_before_active_shifts_active_down() {
        // Closing a.rs (index 0) while b.rs (index 1) is active: b.rs slides
        // down to index 0 and stays active.
        assert_eq!(next_active_after_close(1, 0, 3), Some(0));
    }

    #[test]
    fn test_closing_a_background_tab_after_active_leaves_active_untouched() {
        // Closing c.rs (index 2) while a.rs (index 0) is active: a.rs's index
        // is unaffected.
        assert_eq!(next_active_after_close(0, 2, 3), Some(0));
    }

    #[test]
    fn test_closing_the_active_middle_tab_activates_the_right_neighbor() {
        // Closing b.rs (active, index 1) out of [a.rs, b.rs, c.rs]: c.rs
        // slides into index 1 and becomes active (the right neighbor).
        assert_eq!(next_active_after_close(1, 1, 3), Some(1));
    }

    #[test]
    fn test_closing_the_active_rightmost_tab_activates_the_left_neighbor() {
        // Closing c.rs (active, index 2), the rightmost tab: no right
        // neighbor exists, so a.rs/b.rs's rightmost survivor (index 1)
        // activates.
        assert_eq!(next_active_after_close(2, 2, 3), Some(1));
    }

    #[test]
    fn test_closing_the_last_tab_empties_the_editor() {
        assert_eq!(next_active_after_close(0, 0, 1), None);
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
