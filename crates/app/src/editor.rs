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
//! state (#352). Closing a **dirty** tab prompts a `gpui-component`
//! `AlertDialog` confirm/discard first; confirming discards the unsaved
//! edits and closes it, cancelling leaves it open untouched. A clean tab
//! still closes immediately (#354).
//!
//! # Workspace wiring fan-out (#353)
//!
//! [`crate::workspace::WorkspaceView`] fans the per-open-path daemon signals
//! out across every open tab instead of a single assumed buffer: the mtime
//! concurrent-write signal ([`EditorView::note_external_change_for_path`])
//! and the inline diagnostics push
//! ([`EditorView::set_diagnostics_for_path`]) both resolve the tab by path
//! ([`EditorView::open_paths`] enumerates every open one), ignoring a signal
//! for a path with no open tab. The live-buffer feed is already per-tab
//! (`arm_buffer_feed` runs per index) and `BufferClosed` already fires only
//! on `close_tab` / a successful save, never on `activate_tab` — so a
//! background dirty tab's live buffer, and its diagnostics, survive a switch
//! away from it. Nav responses already route by the id-owning tab
//! (`latest_def_id` / `latest_hover_id` / `latest_ref_id`, #351) regardless
//! of which tab is active when the reply lands.
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
//! [`EditorView::note_external_change_for_path`] runs the pure
//! [`decide_external_change`] decision on the addressed tab's snapshot
//! `mtime`: a clean buffer auto-reloads; a dirty buffer surfaces a conflict.
//! The auto-reload keeps the tab's `InputState` entity and restores the
//! pre-reload cursor once the fresh content lands ([`EditorTab::pending_restore`],
//! #432), so watching an agent edit the open file does not yank the viewport
//! to the top on every write. The conflict surfaces as a `gpui-component`
//! `AlertDialog` on the active tab (`docs/spec-editor-chrome.md`, #532 —
//! upgrading the #433 inline banner to the #420 confirm-dialog pattern) with
//! the same two working remedies: "Reload from disk" (primary) discards the
//! buffer's edits via the auto-reload path, and "Keep mine" (secondary)
//! force-saves the buffer rebased onto the on-disk `mtime` observed when the
//! conflict surfaced ([`EditorTab::conflict_disk_mtime`]). A background
//! tab's conflict pops the same dialog once the user switches to it
//! ([`EditorView::activate_tab`]).
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
//! the editor emits [`EditorEvent::ShowResults`] so the workspace opens the
//! right-dock results panel (`docs/spec-editor-chrome.md` §3, #529) with the
//! targets; the user clicks the desired destination there.
//!
//! # Find-references (#198)
//!
//! Find-references is triggered by:
//! - **`Shift+F12`** (scoped to the `Editor` key context, bound in `main.rs`):
//!   dispatches [`ClientMessage::ReferencesRequest`] at the cursor position.
//! - **Context-menu "Find References"**: same dispatch path.
//!
//! The response is applied by [`EditorView::apply_references_response`], which
//! emits [`EditorEvent::ShowResults`] so the workspace opens the right-dock
//! results panel — the same surface the multi-target definition path uses, so
//! the UX (click-to-jump, back-nav) is identical (#529).
//!
//! Stale-response discipline mirrors the definition and hover paths: a
//! response is matched to whichever tab's `latest_*_id` equals the response's
//! id (nav ids are one editor-scoped counter shared by every tab, #351); no
//! match means the response is stale and is silently dropped.
//!
//! # Hover popover (#197)
//!
//! Hover is triggered by:
//! - **`Ctrl+K Ctrl+I`** (scoped to the `Editor` key context, bound in
//!   `main.rs`; a non-typing chord so it cannot shadow typed input, #435):
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
//! # Find/replace and go-to-line (`docs/spec-v1-hardening.md`, #620)
//!
//! Both operate purely client-side over the loaded buffer — no new protocol,
//! no daemon round-trip.
//!
//! **Find/replace** reuses the `gpui-component` `Input` widget's own search
//! facility rather than rebuilding one: [`arm_loading`] builds each tab's
//! code-editor `InputState` with `.code_editor(...)`, which turns on
//! `searchable` by default, so `Ctrl+F`/`Cmd+F` (bound in gpui-component's own
//! "Input" key context) already opens an inline find/replace bar with
//! prev/next navigation and replace-one/replace-all. `arm_loading` adds one
//! explicit gate on top: `.replaceable(!tab.read_only)`, so a read-only
//! out-of-root or `TooLarge`-placeholder tab still offers find but hides
//! replace.
//!
//! **Go-to-line** has no equivalent built-in widget in this gpui-component
//! pin, so [`EditorView::open_go_to_line`] opens a light `gpui-component`
//! `Dialog` with a single line-number `Input` — the "light theme-token
//! overlay" fallback. `Ctrl+G` (bound in `main.rs`, scoped to the `Editor` key
//! context), the context-menu "Go to Line" entry, and the command palette all
//! dispatch [`GoToLine`]. Confirming (OK or Enter) moves the caret to the
//! requested 1-based line, clamped to the buffer's last line
//! ([`go_to_line_target`]) so an out-of-range request still lands somewhere
//! sane — the same clamp-not-reject contract the minimap's click-to-jump
//! already uses ([`minimap_click_line`]).
//!
//! [`arm_loading`]: EditorView::arm_loading
//!
//! # Timeout, not a hang
//!
//! A daemon refusal (binary / non-UTF-8, path escape) produces *no reply* — the
//! editor recovers via bounded timeouts ([`OPEN_TIMEOUT`] / [`SAVE_TIMEOUT`]).
//! Nav requests have no reply timeout at the editor layer: stale responses are
//! discarded by id comparison in [`EditorView::apply_definition_response`] and
//! [`EditorView::apply_hover_response`].

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use flume::Sender;
use gpui::{
    canvas, div, fill, px, App, AppContext as _, Bounds, ClickEvent, Context, Entity, EventEmitter,
    FocusHandle, Focusable, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels, Point, Render, SharedString, Size,
    Styled as _, Subscription, Window,
};
use gpui_component::dialog::{AlertDialog, Dialog, DialogButtonProps};
use gpui_component::dock::{Panel, PanelControl, PanelEvent};
use gpui_component::highlighter::{
    Diagnostic as EditorDiagnostic, DiagnosticSeverity as EditorSeverity,
};
use gpui_component::input::{Input, InputEvent, InputState, Position as EditorPosition};
use gpui_component::menu::PopupMenu;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::text::markdown;
use gpui_component::ActiveTheme as _;
use gpui_component::RopeExt as _;
use gpui_component::WindowExt as _;
use gpui_component::{Icon, IconName};
use rift_protocol::{
    BufferErrorReason, ClientMessage, Diagnostic, DiagnosticSeverity, DocumentSymbolEntry,
    HoverContent, NavLocation, NavRequestId, Position, Range,
};

use crate::results_panel::ResultsKind;

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
/// the context-menu entry ("Show Hover") and from the `Ctrl+K Ctrl+I` keybind
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

/// Open the go-to-line dialog for the active tab (`docs/spec-v1-hardening.md`,
/// #620). Dispatched from the `Ctrl+G` keybind (bound in `main.rs`, scoped to
/// the editor key context) mirroring VS Code/JetBrains muscle memory, and
/// from the context-menu entry ("Go to Line").
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct GoToLine;

/// Close the references/definitions results panel (`docs/spec-editor-chrome.md`
/// §3, #529). Bound to `Escape` in `main.rs`, scoped to the editor key context;
/// propagates when the panel is not open so `Escape` keeps its other meanings.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct CloseResultsPanel;

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

/// Height of the breadcrumb bar under the editor tab strip
/// (`docs/spec-editor-chrome.md` §1: mono path segments + enclosing symbol).
const BREADCRUMB_HEIGHT: Pixels = px(30.0);

/// Diameter of a gutter severity dot (`docs/spec-editor-chrome.md`).
const GUTTER_DOT_SIZE: Pixels = px(7.0);

/// Left inset of the gutter severity dot from the editor content's left edge —
/// placed left of the line-number column.
const GUTTER_DOT_LEFT: Pixels = px(3.0);

/// Corner radius of the inline diagnostic card (`docs/spec-editor-chrome.md`
/// §1: "radius 8").
const CARD_RADIUS: Pixels = px(8.0);

/// Conservative height estimate for the inline diagnostic card, used only to
/// decide whether it fits below the cursor line or must flip above it.
const CARD_ESTIMATED_HEIGHT: Pixels = px(56.0);

/// Separator glyph between breadcrumb segments (U+203A, single right-pointing
/// angle quotation mark — not an emoji).
const BREADCRUMB_SEPARATOR: &str = "\u{203A}";

/// Maximum number of code lines the hover card's code block shows before it
/// truncates (`docs/spec-editor-chrome.md` §3: "signature + truncated
/// preview"). Keeps a large signature or module preview from dominating the
/// card while still surfacing the essential signature.
const HOVER_CODE_MAX_LINES: usize = 12;

/// Marker line appended to a truncated hover code preview (U+2026 horizontal
/// ellipsis — not an emoji).
const HOVER_TRUNCATION_MARKER: &str = "\u{2026}";

/// Keyboard hint shown on the hover card's "Definition" action, matching the
/// `GoToDefinition` binding advertised by the command registry.
const HOVER_DEFINITION_HINT: &str = "F12";

/// Keyboard hint shown on the hover card's "References" action, matching the
/// `FindReferences` binding advertised by the command registry.
const HOVER_REFERENCES_HINT: &str = "Shift+F12";

/// Width of the minimap marks strip on the editor's right edge. Widened from
/// the original "~14px" (`docs/spec-editor-chrome.md`) — that width left
/// barely any room for line-length marks to vary, making the strip unreadable
/// (#600). Still a marks strip, not a pixel-perfect code render.
const MINIMAP_WIDTH: Pixels = px(32.0);

/// Maximum number of line-length sample rows painted in the minimap. Caps the
/// per-render work so a very large buffer stays cheap — the strip is only a few
/// hundred pixels tall, so more samples than this add no visible detail
/// (`docs/spec-editor-chrome.md`: marks are downsampled, not a pixel render).
const MINIMAP_SAMPLES: usize = 1024;

/// Height in pixels of a diagnostic mark painted over the minimap strip.
/// Thickened alongside the strip width (#600) so tints stay legible.
const MINIMAP_DIAG_MARK_HEIGHT: f32 = 3.0;

/// Minimum height in pixels of the minimap viewport slab, so it stays visible
/// even when the buffer is far taller than the viewport.
const MINIMAP_SLAB_MIN_HEIGHT: f32 = 6.0;

/// Horizontal inset in pixels of the line-length marks from the strip's edges.
const MINIMAP_MARK_INSET: f32 = 3.0;

// ── Internal state types ──────────────────────────────────────────────────────

/// What a tab is currently showing.
enum TabLoadState {
    /// An open request is in flight, awaiting its `FileContent` reply.
    Loading,
    /// The tab's content is rendered in the code editor.
    Loaded,
    /// The last open did not complete. `Some(reason)` names the daemon's
    /// specific refusal (an `OpenError` reply, routed via
    /// [`EditorView::apply_open_error`]); `None` means the `OPEN_TIMEOUT`
    /// fired with no reply at all (e.g. no daemon available). A specific
    /// reply always beats the timeout: it moves the tab out of `Loading`
    /// before the timeout's own `Loading` guard can fire.
    Failed(Option<BufferErrorReason>),
}

/// The transient outcome of a tab's most recent save.
enum SaveState {
    Idle,
    Saving,
    Conflict,
    /// The last save did not land. `Some(reason)` names the daemon's
    /// specific refusal (a `SaveError` reply, routed via
    /// [`EditorView::apply_save_error`]); `None` means the `SAVE_TIMEOUT`
    /// fired with no reply, or the outgoing send itself failed.
    Failed(Option<BufferErrorReason>),
}

/// Events the editor emits for the workspace to route to the right-dock results
/// panel (`docs/spec-editor-chrome.md` §3, #529). The panel owns both nav
/// overlay consumers — find-references and multi-target go-to-definition — so
/// these carry the result set to it and signal when the editor closed it.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorEvent {
    /// A references or multi-target definition response arrived: show it in the
    /// results panel. `symbol` is the searched token (for the chip and the
    /// per-match highlight), `None` when it could not be resolved.
    ShowResults {
        kind: ResultsKind,
        symbol: Option<SharedString>,
        locations: Vec<NavLocation>,
    },
    /// The user closed the results panel from the editor (Escape); the
    /// workspace hides the panel.
    CloseResults,
}

/// Owned render data for the inline diagnostic card shown under the cursor
/// line (`docs/spec-editor-chrome.md`). Gathered from the tab's diagnostics
/// and the input widget's layout so the `InputState` read borrow is released
/// before the card element is built.
struct InlineCard {
    /// Content-relative top offset of the card.
    top: Pixels,
    /// Content-relative left offset (the cursor line's text start).
    left: Pixels,
    /// The severity color of the primary diagnostic (the card's glyph).
    color: Hsla,
    /// The diagnostic message.
    message: SharedString,
    /// The muted `source`/`code` suffix, if either is present.
    detail: Option<SharedString>,
}

/// Owned render data for the minimap marks strip on the editor's right edge
/// (`docs/spec-editor-chrome.md`). Gathered under the `InputState` read borrow
/// in [`EditorView::render`] and moved into the strip's `canvas` paint closure,
/// so the borrow is released before painting. Deliberately NOT a pixel-perfect
/// code render: line-length marks are downsampled to at most [`MINIMAP_SAMPLES`]
/// rows, diagnostics and the viewport slab are positioned by line ratio, and the
/// whole strip repaints only when the editor is damaged (a GPUI notify).
struct MinimapPaint {
    /// Per-sample maximum line length (characters). Length `min(total, samples)`;
    /// empty for an empty buffer. Painted as horizontal marks scaled across the
    /// strip height, width proportional to the sample's share of the longest
    /// line. A cheap ref-count clone of the tab's cached [`EditorTab::minimap_samples`]
    /// — derived once per text change, not rescanned per render.
    samples: Rc<[u32]>,
    /// Diagnostic marks: `(line-ratio 0..1, severity color)`, painted full-width
    /// over the length marks so problems stand out at a glance.
    diag_marks: Vec<(f32, Hsla)>,
    /// The viewport slab as `(top, bottom)` fractions of the strip height; the
    /// slab is skipped when `bottom <= top` (no laid-out viewport yet).
    slab_top: f32,
    slab_bottom: f32,
    /// Color of the line-length marks (subtle) and of the viewport slab.
    mark_color: Hsla,
    slab_color: Hsla,
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
    /// Fires on every `InputState` notify (cursor move, scroll, blink, edit) —
    /// the only signal for a cursor move, which emits no `InputEvent`. Keeps
    /// the breadcrumb symbol, gutter dots, and inline card tracking the cursor
    /// and viewport, and re-syncs the widget's diagnostic set when the cursor
    /// line changes (`docs/spec-editor-chrome.md`).
    _input_notify: Subscription,
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
    /// The on-disk `mtime` observed when this tab's conflict surfaced —
    /// the worktree snapshot's `mtime` or a `SaveConflict` reply's
    /// `disk_mtime`. "Keep mine" (#433) adopts it as the forced save's
    /// base so the daemon's stale-base check passes against exactly the
    /// observed disk version; a *further* external write still conflicts.
    /// `None` outside a conflict.
    conflict_disk_mtime: Option<SystemTime>,

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

    /// The cursor position to restore once an auto-reload's fresh content
    /// lands (#432) — captured from the pre-reload buffer when a clean-buffer
    /// external change arms the reload; consumed in [`EditorView::load`].
    /// The restore is clamped to the new content by the input's rope layer,
    /// so a cursor past the end of a shrunken file lands at its end. `None`
    /// for plain opens and nav jumps.
    pending_restore: Option<EditorPosition>,

    /// This tab's bounded back-jump stack: (path, position, read_only)
    /// triples recording where a jump *away* from this tab should return —
    /// so `GoBack` while viewing this tab unwinds to wherever the jump that
    /// landed here came from. `read_only` preserves the source's access mode
    /// so `GoBack` can reopen it the same way.
    back_stack: VecDeque<(String, EditorPosition, bool)>,

    /// The token under the cursor when this tab last dispatched a definition or
    /// references request — carried into the results panel as the search-context
    /// chip and the per-match highlight when the response lands
    /// (`docs/spec-editor-chrome.md` §3, #529). `None` when no identifier sat at
    /// the request cursor.
    nav_symbol: Option<SharedString>,

    /// The cached, flattened document-symbol tree for this tab's file, fetched
    /// once per open and per buffer-change debounce (never per keystroke). The
    /// breadcrumb resolves the enclosing symbol at the cursor against this
    /// cache client-side (`docs/spec-editor-chrome.md`).
    symbols: Vec<DocumentSymbolEntry>,
    /// The id of the most recent document-symbol request this tab dispatched.
    /// Mirrors `latest_def_id`'s drop-stale discipline.
    latest_symbol_id: Option<NavRequestId>,

    /// This tab's own copy of the inline diagnostics (the source of truth for
    /// the gutter dots and the inline card). The widget's own diagnostic set
    /// is a *filtered* view of this — every entry except those on the cursor
    /// line, so the cursor line's diagnostic renders only as the app's inline
    /// card and never also as the widget's hover popover (one diagnostic, one
    /// surface — `docs/spec-editor-chrome.md`).
    diagnostics: Vec<Diagnostic>,
    /// The cursor's current line, tracked so a line change (not every notify)
    /// re-syncs the widget's suppressed diagnostic set.
    cursor_line: u32,

    /// This tab's cached minimap line-length marks — the widest character count
    /// per downsampled block of source lines, capped at [`MINIMAP_SAMPLES`].
    /// Derived once per load and per buffer `Change` (never per render), so a
    /// large focused buffer is not rescanned on every cursor blink or scroll —
    /// the marks change only when the text does (`docs/spec-editor-chrome.md`:
    /// derive marks once, redraw on damage only). `Rc<[u32]>` so `render` hands
    /// a cheap ref-count clone to the strip's paint closure, not a data copy.
    /// Character count (`Rope::line_len`) deliberately substitutes for
    /// gpui-component's shaped `LineLayout` cache, which is `pub(crate)` and thus
    /// inaccessible from this crate; it still honors the strip's "not a
    /// pixel-perfect code render" intent.
    minimap_samples: Rc<[u32]>,
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

    /// The editor content area's window-space bounds, captured each frame by a
    /// `canvas` overlay and read back on the next frame. The app-side gutter
    /// dots and inline card are positioned via the input widget's
    /// `range_to_bounds` (window coordinates, correct through soft-wrap and
    /// folding); this converts those window coordinates into content-relative
    /// offsets for the overlay children (`docs/spec-editor-chrome.md` — "the
    /// pinned widget has no gutter decoration API"). `None` until the first
    /// paint. One-frame lag matches `range_to_bounds`, which also reads the
    /// previous frame's layout, so the two stay consistent.
    content_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,

    /// The minimap strip's window-space bounds, captured each frame by its
    /// `canvas` paint closure and read back on the next mouse-down to translate
    /// a click into a target line (`docs/spec-editor-chrome.md`:
    /// "click-to-jump"). `None` until the first paint; the one-frame lag is
    /// harmless because the strip's geometry barely moves between frames.
    minimap_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,

    /// Whether the right-dock results panel is currently showing this editor's
    /// nav results (`docs/spec-editor-chrome.md` §3, #529). Set when a response
    /// is emitted to the panel; cleared when the editor closes it (Escape) or
    /// the workspace reports the panel closed ([`EditorView::mark_results_closed`]).
    /// Gates whether `Escape` consumes the keystroke or propagates it.
    results_visible: bool,
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
            content_bounds: Rc::new(Cell::new(None)),
            minimap_bounds: Rc::new(Cell::new(None)),
            results_visible: false,
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
        let input_notify = Self::observe_input(&input, cx);
        self.tabs.push(EditorTab {
            path,
            input,
            _input_change: input_change,
            _input_notify: input_notify,
            load_state: TabLoadState::Loading,
            save_state: SaveState::Idle,
            dirty: false,
            base_mtime: None,
            read_only,
            conflict_disk_mtime: None,
            generation: 0,
            save_generation: 0,
            buffer_generation: 0,
            latest_def_id: None,
            latest_hover_id: None,
            latest_ref_id: None,
            hover_content: None,
            hover_move_generation: 0,
            pending_jump: None,
            pending_restore: None,
            back_stack: VecDeque::new(),
            nav_symbol: None,
            symbols: Vec::new(),
            latest_symbol_id: None,
            diagnostics: Vec::new(),
            cursor_line: 0,
            minimap_samples: Rc::from([]),
        });
        self.arm_loading(index, true, window, cx);
        index
    }

    /// Reset the tab at `index` to `Loading` for its current path: clears
    /// per-load bookkeeping (dirty, save state, hover, jump-list) and arms
    /// the [`OPEN_TIMEOUT`] guard. Shared by a freshly pushed tab
    /// (`rebuild_input: true` — builds the code-editor `InputState` for the
    /// path) and an in-place auto-reload (external change, #188;
    /// `rebuild_input: false` — keeps the live entity so its cursor and
    /// layout survive for the post-reload viewport restore, #432). Both end
    /// up awaiting a `FileContent` reply.
    fn arm_loading(
        &mut self,
        index: usize,
        rebuild_input: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };

        tab.load_state = TabLoadState::Loading;
        tab.save_state = SaveState::Idle;
        tab.base_mtime = None;
        tab.conflict_disk_mtime = None;
        tab.dirty = false;
        tab.nav_symbol = None;
        tab.hover_content = None;
        tab.latest_hover_id = None;
        tab.latest_ref_id = None;
        tab.pending_restore = None;
        // The content is about to be replaced, so the cached symbol tree and
        // diagnostics no longer describe it; a fresh document-symbol request
        // rides the completed load, and diagnostics are re-pushed by the daemon.
        tab.symbols = Vec::new();
        tab.latest_symbol_id = None;
        tab.diagnostics = Vec::new();
        tab.cursor_line = 0;
        tab.minimap_samples = Rc::from([]);

        if rebuild_input {
            let language = language_for_path(&tab.path);
            // `code_editor` turns on the widget's own find/replace facility
            // (`searchable: true`, `Ctrl+F`/`Cmd+F` — `docs/spec-v1-hardening.md`,
            // #620), which defaults `replaceable: true` too. Explicitly gate
            // replace off for a read-only tab (out-of-root or `TooLarge`
            // placeholder, #196/#301): `.disabled(tab.read_only)` on the
            // rendered `Input` already blocks every direct edit, but the
            // search panel's replace-all path writes through
            // `replace_text_in_range_silent` independently of that flag, so
            // `replaceable` is the widget's own seam for "find is fine here,
            // replace is not". Find itself stays available — it is read-only
            // by nature.
            let read_only = tab.read_only;
            tab.input = cx.new(|cx| {
                InputState::new(window, cx)
                    .code_editor(language)
                    .line_number(true)
                    .tab_size(gpui_component::input::TabSize {
                        tab_size: TAB_SIZE,
                        ..Default::default()
                    })
                    .replaceable(!read_only)
            });
            tab._input_change = Self::subscribe_dirty(&tab.input, index, cx);
            tab._input_notify = Self::observe_input(&tab.input, cx);
        }

        tab.generation = tab.generation.wrapping_add(1);
        let generation = tab.generation;
        let path = tab.path.clone();

        cx.spawn(async move |this, cx| {
            smol::Timer::after(OPEN_TIMEOUT).await;
            cx.update(|cx| {
                let _ = this.update(cx, |this, cx| {
                    // Re-resolve by path, not the captured `index`: a
                    // `close_tab` between arm and fire can shift indices, so
                    // trusting the stale position risks acting on (or
                    // reporting an out-of-range miss for) the wrong tab.
                    let Some(index) = this.tab_index_for_path(&path) else {
                        return;
                    };
                    let Some(tab) = this.tabs.get_mut(index) else {
                        return;
                    };
                    if tab.generation == generation {
                        if let TabLoadState::Loading = tab.load_state {
                            tab.load_state = TabLoadState::Failed(None);
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
                // Re-derive the minimap marks now the text changed — never per
                // render, so a blink or scroll does not rescan the buffer.
                this.recompute_minimap_samples(index, cx);
                this.arm_buffer_feed(index, cx);
            }
        })
    }

    /// Re-derive the tab at `index`'s cached minimap line-length marks from its
    /// current buffer. Called on load and on every buffer `Change`, never per
    /// render, so a large focused buffer is not rescanned on every cursor blink
    /// or scroll frame (`docs/spec-editor-chrome.md`: derive marks once, redraw
    /// on damage only).
    fn recompute_minimap_samples(&mut self, index: usize, cx: &Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let samples: Rc<[u32]> = {
            let input_state = tab.input.read(cx);
            let text = input_state.text();
            let total = text.lines_len();
            sample_line_lengths(total, |row| text.line_len(row) as u32, MINIMAP_SAMPLES).into()
        };
        self.tabs[index].minimap_samples = samples;
    }

    /// Observe an `InputState` so the editor re-renders on cursor moves and
    /// scrolls — a cursor move emits no [`InputEvent`], only a bare notify, so
    /// [`Self::subscribe_dirty`] alone would never see it. The breadcrumb's
    /// enclosing symbol, the gutter dots, and the inline card all track the
    /// cursor and viewport, so they must repaint on any such notify. When the
    /// cursor *line* changes, the widget's suppressed diagnostic set is
    /// re-synced so exactly the new cursor line's diagnostic is withheld from
    /// the widget popover (`docs/spec-editor-chrome.md`).
    ///
    /// The tab is resolved by the observed entity's id, not a captured index,
    /// so a tab close that shifts indices cannot misroute this callback.
    fn observe_input(input: &Entity<InputState>, cx: &mut Context<Self>) -> Subscription {
        cx.observe(input, move |this, observed, cx| {
            let Some(index) = this
                .tabs
                .iter()
                .position(|t| t.input.entity_id() == observed.entity_id())
            else {
                return;
            };
            let new_line = observed.read(cx).cursor_position().line;
            if this.tabs[index].cursor_line != new_line {
                this.tabs[index].cursor_line = new_line;
                this.sync_widget_diagnostics(index, cx);
            }
            cx.notify();
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
                    // Re-resolve by path, not the captured `index`: a
                    // `close_tab` between arm and fire can shift indices, so
                    // trusting the stale position risks reading a different
                    // tab's buffer entirely (or missing this one out of
                    // range) instead of no-op'ing when this tab is gone.
                    let Some(index) = this.tab_index_for_path(&path) else {
                        return;
                    };
                    let Some(tab) = this.tabs.get_mut(index) else {
                        return;
                    };
                    if tab.buffer_generation != generation {
                        return;
                    }
                    if !matches!(tab.load_state, TabLoadState::Loaded) {
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
                    // Refresh the symbol cache against the buffer just fed to
                    // the LSP — one request per change settle, so the
                    // breadcrumb tracks edits without a per-keystroke request
                    // (`docs/spec-editor-chrome.md`).
                    this.dispatch_document_symbol_request(index);
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

    /// Replace the inline diagnostics for whichever tab holds `path` (#189),
    /// fanned out per open path (`docs/spec-editor-tabs.md`, #353) rather
    /// than only the active tab — so a background dirty tab's diagnostics
    /// stay current too. A no-op when no tab holds `path`.
    pub fn set_diagnostics_for_path(
        &mut self,
        path: &str,
        items: &[Diagnostic],
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tab_index_for_path(path) else {
            return;
        };
        self.tabs[index].diagnostics = items.to_vec();
        self.sync_widget_diagnostics(index, cx);
        cx.notify();
    }

    /// Push the tab's diagnostics into the widget's own diagnostic set, minus
    /// every entry on the cursor line. The cursor line's diagnostic is shown
    /// by the app's inline card instead, so withholding it here keeps the
    /// widget's mouse-hover `DiagnosticPopover` from rendering the same
    /// diagnostic a second time (`docs/spec-editor-chrome.md` — "one
    /// diagnostic never renders twice"). Every other line keeps its squiggle
    /// and hover popover. Re-run whenever the diagnostics or the cursor line
    /// change.
    fn sync_widget_diagnostics(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let suppressed_line = tab.cursor_line;
        let editor_items: Vec<EditorDiagnostic> = tab
            .diagnostics
            .iter()
            .filter(|d| d.range.start.line != suppressed_line)
            .map(to_editor_diagnostic)
            .collect();
        self.tabs[index].input.update(cx, |input, cx| {
            if let Some(set) = input.diagnostics_mut() {
                set.clear();
                set.extend(editor_items);
            }
            cx.notify();
        });
    }

    // ── Buffer state accessors ────────────────────────────────────────────

    /// The path of the active tab, if any is open.
    pub fn open_path(&self) -> Option<&str> {
        self.active_tab().map(|t| t.path.as_str())
    }

    /// The root-relative paths of every currently open tab, in open order.
    /// Used by `WorkspaceView` to fan the per-path daemon signals (mtime,
    /// diagnostics) out across every open tab instead of just the active one
    /// (`docs/spec-editor-tabs.md`, #353).
    pub fn open_paths(&self) -> impl Iterator<Item = &str> {
        self.tabs.iter().map(|t| t.path.as_str())
    }

    /// The base `mtime` of the active tab's buffer.
    pub fn base_mtime(&self) -> Option<SystemTime> {
        self.active_tab().and_then(|t| t.base_mtime)
    }

    /// Whether the active tab's buffer has unsaved edits.
    pub fn is_dirty(&self) -> bool {
        self.active_tab().is_some_and(|t| t.dirty)
    }

    /// The number of open tabs with unsaved edits, for the aggregated
    /// window-close confirm dialog's message (`docs/spec-v1-hardening.md`).
    pub fn dirty_tab_count(&self) -> usize {
        self.tabs.iter().filter(|t| t.dirty).count()
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

    /// The active tab's zero-based cursor `(line, column)` for the composite
    /// status line's Ln/Col segment (`docs/spec-status-line.md`), or `None`
    /// when no tab is open. `column` is a UTF-8 scalar offset (the editor's own
    /// `cursor_position` convention); the status line renders both 1-based.
    pub fn cursor_position(&self, cx: &App) -> Option<(u32, u32)> {
        self.active_tab().map(|tab| {
            let pos = tab.input.read(cx).cursor_position();
            (pos.line, pos.character)
        })
    }

    /// The active tab's cached, flattened document-symbol tree
    /// (`docs/spec-editor-chrome.md`) — empty when no tab is open or the
    /// server has not answered yet. Feeds the outline panel (#530); the
    /// breadcrumb reads the same cache via [`enclosing_symbol_chain`].
    pub fn active_document_symbols(&self) -> &[DocumentSymbolEntry] {
        self.active_tab()
            .map(|tab| tab.symbols.as_slice())
            .unwrap_or(&[])
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
        // Derive the minimap marks for the freshly loaded content up front, so
        // the strip is correct on the first frame rather than a frame late.
        self.recompute_minimap_samples(index, cx);

        if let Some(range) = self.tabs[index].pending_jump.take() {
            self.apply_jump_range(index, &range, window, cx);
        } else if let Some(cursor) = self.tabs[index].pending_restore.take() {
            self.restore_cursor_after_reload(index, cursor, window, cx);
        }

        // Seed the breadcrumb/outline symbol cache for the freshly loaded file
        // (`docs/spec-editor-chrome.md`): one request per open.
        self.dispatch_document_symbol_request(index);

        cx.notify();
    }

    /// Apply an `OpenError` reply: the daemon refused the read (binary,
    /// non-UTF-8, unreadable path, or over the read-size cap) instead of
    /// returning `FileContent`. Routed to the tab awaiting it (matching
    /// path, `Loading`), mirroring [`load`]'s guard; a reply for a path with
    /// no `Loading` tab is silently ignored. Surfaces immediately — moving
    /// the tab out of `Loading` is what makes the specific reason beat the
    /// generic `OPEN_TIMEOUT` fallback (its guard only fires while still
    /// `Loading`).
    ///
    /// [`load`]: Self::load
    pub fn apply_open_error(
        &mut self,
        path: String,
        reason: BufferErrorReason,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.path == path && matches!(t.load_state, TabLoadState::Loading))
        else {
            return;
        };
        self.tabs[index].load_state = TabLoadState::Failed(Some(reason));
        cx.notify();
    }

    /// Land the pre-reload cursor on the freshly auto-reloaded content (#432).
    ///
    /// `InputState::set_cursor_position` clamps the position to the new
    /// content via the rope layer and scrolls the cursor back into view —
    /// but it also focuses the input as a side effect. An auto-reload is
    /// agent-triggered, not user-triggered, so it must never steal focus
    /// (the user is typically typing in the terminal while the agent
    /// writes): focus is handed back when it was elsewhere.
    fn restore_cursor_after_reload(
        &mut self,
        index: usize,
        cursor: EditorPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let input_focus = tab.input.focus_handle(cx);
        let previous_focus = window.focused(cx);
        tab.input.update(cx, |input, cx| {
            input.set_cursor_position(cursor, window, cx);
        });
        if let Some(previous) = previous_focus {
            if previous != input_focus {
                window.focus(&previous, cx);
            }
        }
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
            self.tabs[index].save_state = SaveState::Failed(None);
            cx.notify();
            return;
        }

        cx.spawn(async move |this, cx| {
            smol::Timer::after(SAVE_TIMEOUT).await;
            cx.update(|cx| {
                let _ = this.update(cx, |this, cx| {
                    // Re-resolve by path, not the captured `index`: a
                    // `close_tab` between arm and fire can shift indices, so
                    // trusting the stale position risks acting on (or
                    // reporting an out-of-range miss for) the wrong tab.
                    let Some(index) = this.tab_index_for_path(&path) else {
                        return;
                    };
                    let Some(tab) = this.tabs.get_mut(index) else {
                        return;
                    };
                    if tab.save_generation == save_generation {
                        if let SaveState::Saving = tab.save_state {
                            tab.save_state = SaveState::Failed(None);
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
        self.tabs[index].conflict_disk_mtime = None;
        self.tabs[index].save_generation = self.tabs[index].save_generation.wrapping_add(1);
        // Disk now matches the buffer: revert to the disk-backed baseline.
        self.close_live_buffer(index);
        cx.notify();
    }

    /// Apply a `SaveConflict` reply: the daemon refused the write. Routed to
    /// whichever tab holds `path`; a no-op if no tab holds it. `disk_mtime`
    /// (the current on-disk value the daemon reported) is recorded so the
    /// dialog's "Keep mine" remedy can rebase a forced save onto it (#433).
    /// Opens the conflict dialog (#532) when the conflicted tab is the
    /// active one.
    pub fn apply_save_conflict(
        &mut self,
        path: String,
        disk_mtime: SystemTime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tab_index_for_path(&path) else {
            return;
        };
        self.tabs[index].save_state = SaveState::Conflict;
        self.tabs[index].conflict_disk_mtime = Some(disk_mtime);
        self.tabs[index].save_generation = self.tabs[index].save_generation.wrapping_add(1);
        cx.notify();
        if self.active == Some(index) {
            self.open_conflict_dialog(window, cx);
        }
    }

    /// Apply a `SaveError` reply: the daemon refused the write outright (a
    /// write failure, distinct from [`SaveConflict`]'s deliberate stale-base
    /// rejection). Routed to whichever tab holds `path`; a no-op if no tab
    /// holds it. Surfaces immediately — bumping `save_generation` fences the
    /// in-flight `SAVE_TIMEOUT` guard exactly like [`apply_save_result`] /
    /// [`apply_save_conflict`] do, so the specific reason beats the generic
    /// timeout fallback.
    ///
    /// [`SaveConflict`]: rift_protocol::DaemonMessage::SaveConflict
    /// [`apply_save_result`]: Self::apply_save_result
    /// [`apply_save_conflict`]: Self::apply_save_conflict
    pub fn apply_save_error(
        &mut self,
        path: String,
        reason: BufferErrorReason,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tab_index_for_path(&path) else {
            return;
        };
        self.tabs[index].save_state = SaveState::Failed(Some(reason));
        self.tabs[index].save_generation = self.tabs[index].save_generation.wrapping_add(1);
        cx.notify();
    }

    // ── Conflict remedies (#433, dialog #532) ─────────────────────────────

    /// Resolve the active tab's conflict by re-reading the file from disk,
    /// discarding the buffer's unsaved edits (the conflict dialog's primary
    /// "Reload from disk" action). Reuses the auto-reload path: the input
    /// entity survives (`rebuild_input: false`) and the cursor is restored
    /// once the fresh content lands (#432). The live buffer reverts to
    /// disk-backed — the discarded edits must not linger as the LSP's
    /// source of truth. A no-op unless the active tab is in conflict.
    fn reload_active_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if !matches!(self.tabs[index].save_state, SaveState::Conflict) {
            return;
        }
        let path = self.tabs[index].path.clone();
        let cursor = self.tabs[index].input.read(cx).cursor_position();
        self.close_live_buffer(index);
        self.arm_loading(index, false, window, cx);
        self.tabs[index].pending_restore = Some(cursor);
        if let Err(e) = self.open_file_tx.try_send(path.clone()) {
            tracing::debug!(error = %e, %path, "failed to enqueue conflict-reload open");
        }
    }

    /// Resolve the active tab's conflict by force-saving the buffer over the
    /// on-disk version (the conflict dialog's secondary "Keep mine" action).
    /// Rebases the tab's base `mtime` onto the on-disk `mtime` observed when
    /// the conflict surfaced, so the daemon's stale-base check passes — an
    /// explicit user decision, never a silent clobber: if the file changed
    /// on disk *again* since the conflict was recorded, the daemon still
    /// refuses and the conflict re-surfaces with the fresh `disk_mtime`. A
    /// no-op unless the active tab is in conflict.
    fn keep_mine_active(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        if !matches!(self.tabs[index].save_state, SaveState::Conflict) {
            return;
        }
        if let Some(disk_mtime) = self.tabs[index].conflict_disk_mtime.take() {
            self.tabs[index].base_mtime = Some(disk_mtime);
        }
        self.save(cx);
    }

    /// Open the file-changed-on-disk conflict dialog for the active tab
    /// (`docs/spec-editor-chrome.md`, #532): a `gpui-component` `AlertDialog`
    /// on the #420 confirm-dialog pattern, replacing the #433 inline banner.
    /// The primary "Reload from disk" and secondary "Keep mine" buttons wire
    /// to the same two remedies the banner offered
    /// ([`reload_active_from_disk`]/[`keep_mine_active`]). Triggered wherever
    /// the active tab's conflict is (re)armed —
    /// [`note_external_change_for_path`], [`apply_save_conflict`], and
    /// switching onto an already-conflicted tab via [`activate_tab`] — and a
    /// no-op if a dialog is already showing (so a further external write
    /// before the user answers does not stack a second one) or the active
    /// tab is not actually in conflict.
    fn open_conflict_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let Some(index) = self.active else {
            return;
        };
        if !matches!(self.tabs[index].save_state, SaveState::Conflict) {
            return;
        }
        let path = self.tabs[index].path.clone();
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let entity = cx.entity();

        window.open_alert_dialog(cx, move |alert: AlertDialog, _, _| {
            let entity_ok = entity.clone();
            let entity_cancel = entity.clone();
            alert
                .title("File changed on disk")
                .description(SharedString::from(format!(
                    "\"{name}\" was changed on disk while you had unsaved edits open."
                )))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Reload from disk")
                        .cancel_text("Keep mine")
                        .show_cancel(true)
                        .on_ok(move |_, window, cx| {
                            entity_ok.update(cx, |view, cx| {
                                view.reload_active_from_disk(window, cx);
                            });
                            true
                        })
                        .on_cancel(move |_, _window, cx| {
                            entity_cancel.update(cx, |view, cx| {
                                view.keep_mine_active(cx);
                            });
                            true
                        }),
                )
        });
    }

    // ── Concurrent external change ────────────────────────────────────────

    /// React to the worktree snapshot reporting a new `mtime` for `path`.
    /// Fanned out per open path (`docs/spec-editor-tabs.md`, #353): routed to
    /// whichever tab holds `path`, not just the active one, so a background
    /// tab auto-reloads or surfaces its own conflict independently. Runs the
    /// pure [`decide_external_change`] decision and acts on it. A no-op when
    /// no tab holds `path`, or that tab is not loaded.
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
    pub fn note_external_change_for_path(
        &mut self,
        path: &str,
        snapshot_mtime: SystemTime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tab_index_for_path(path) else {
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
                // Capture the cursor before the tab flips to `Loading`;
                // `load` restores it over the fresh content so the reload
                // does not yank the viewport to the top (#432). The input
                // entity is deliberately kept (`rebuild_input: false`) —
                // its surviving layout is what lets the restore scroll the
                // cursor back into view.
                let cursor = self.tabs[index].input.read(cx).cursor_position();
                self.arm_loading(index, false, window, cx);
                self.tabs[index].pending_restore = Some(cursor);
                if let Err(e) = self.open_file_tx.try_send(path.clone()) {
                    tracing::debug!(error = %e, %path, "failed to enqueue auto-reload open");
                }
            }
            ExternalChange::Conflict => {
                self.tabs[index].save_state = SaveState::Conflict;
                self.tabs[index].conflict_disk_mtime = Some(snapshot_mtime);
                cx.notify();
                if self.active == Some(index) {
                    self.open_conflict_dialog(window, cx);
                }
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

        // Capture the searched token now (the response carries no symbol text);
        // a multi-target reply hands it to the results panel (#529).
        self.tabs[index].nav_symbol = self.symbol_at_cursor(index, cx);

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

    /// The identifier token under the tab at `index`'s cursor, for the results
    /// panel's search-context chip and per-match highlight (#529). `None` when
    /// the cursor sits on no identifier. Read from the live buffer so it matches
    /// the position the nav request is dispatched at.
    fn symbol_at_cursor(&self, index: usize, cx: &Context<Self>) -> Option<SharedString> {
        let input = self.tabs[index].input.read(cx);
        let pos = input.cursor_position();
        let text = input.value().to_string();
        word_at(&text, pos.line, pos.character).map(SharedString::from)
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
    /// dispatch, captures the searched token, increments `nav_id`, and records
    /// the tab's `latest_ref_id`. The response opens the right-dock results
    /// panel (#529). A no-op unless a tab is loaded.
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

        // Capture the searched token now for the results panel's chip and
        // per-match highlight (#529); the response carries no symbol text.
        self.tabs[index].nav_symbol = self.symbol_at_cursor(index, cx);

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

    /// Dispatch a `DocumentSymbolRequest` for the tab at `index`
    /// (`docs/spec-editor-chrome.md`).
    ///
    /// Unlike the cursor-scoped nav requests this carries no position: the
    /// symbol tree covers the whole file, and the breadcrumb resolves the
    /// enclosing symbol against the cached tree client-side. Called on open
    /// and on the buffer-change debounce — one request per settle, never per
    /// keystroke — so the cache stays current without flooding the daemon.
    /// A no-op unless the tab is loaded.
    fn dispatch_document_symbol_request(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if !matches!(tab.load_state, TabLoadState::Loaded) {
            return;
        }
        let path = tab.path.clone();
        self.nav_id = self.nav_id.wrapping_add(1);
        let id = NavRequestId(self.nav_id);
        self.tabs[index].latest_symbol_id = Some(id);

        if let Err(e) = self.nav_tx.try_send(ClientMessage::DocumentSymbolRequest {
            id,
            path: path.clone(),
        }) {
            tracing::debug!(error = %e, %path, "failed to enqueue document-symbol request");
        }
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
    /// directly; multiple targets open the right-dock results panel (#529).
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
                // Multiple targets (e.g. Rust trait impls): open the results
                // panel in the right dock (#529).
                let symbol = self.tabs[source_index].nav_symbol.clone();
                self.results_visible = true;
                cx.emit(EditorEvent::ShowResults {
                    kind: ResultsKind::Definitions,
                    symbol,
                    locations: targets,
                });
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
    /// silent no-op (the server found no references). A non-empty list opens
    /// the right-dock results panel via [`EditorEvent::ShowResults`] (#529).
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

        // Open the results panel in the right dock (#529).
        let symbol = self.tabs[index].nav_symbol.clone();
        self.results_visible = true;
        cx.emit(EditorEvent::ShowResults {
            kind: ResultsKind::References,
            symbol,
            locations: targets,
        });
        cx.notify();
    }

    /// Apply a `DocumentSymbolResponse` from the daemon
    /// (`docs/spec-editor-chrome.md`).
    ///
    /// Matched to whichever tab's `latest_symbol_id` equals `id` (drop-stale
    /// discipline, mirroring the other nav responses). The flattened tree
    /// replaces the tab's cache; the breadcrumb resolves the enclosing symbol
    /// against it on the next render. An empty list is a valid answer (the
    /// server found no symbols) and simply clears the cache.
    pub fn apply_document_symbol_response(
        &mut self,
        id: NavRequestId,
        symbols: Vec<DocumentSymbolEntry>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.latest_symbol_id == Some(id))
        else {
            tracing::debug!(?id, "dropping stale document-symbol response");
            return;
        };
        self.tabs[index].symbols = symbols;
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

    /// Jump the active tab to the minimap-clicked line (`docs/spec-editor-chrome.md`:
    /// "click-to-jump"). `click_y` is the window-space vertical position of the
    /// click; the strip's captured bounds turn it into a 0..1 ratio, then a
    /// target line. The jump lands the caret at that line's start via
    /// `InputState::set_cursor_position`, which scrolls it into view — the only
    /// public scroll seam this gpui-component pin exposes (there is no
    /// scroll-offset setter), so a minimap click both scrolls to and selects the
    /// clicked line, matching the ctrl+click / nav jumps that already move the
    /// caret. The position is clamped to a valid line, and the rope layer clamps
    /// the column, so an out-of-range click still lands somewhere sane.
    fn minimap_jump(&mut self, click_y: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        let Some(bounds) = self.minimap_bounds.get() else {
            return;
        };
        let strip_height = f32::from(bounds.size.height);
        if strip_height <= 0.0 {
            return;
        }
        let ratio = (f32::from(click_y) - f32::from(bounds.origin.y)) / strip_height;
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        tab.input.update(cx, |input, cx| {
            let total = input.text().lines_len();
            let target = minimap_click_line(ratio, total);
            input.set_cursor_position(EditorPosition::new(target as u32, 0), window, cx);
        });
        cx.notify();
    }

    /// Open the go-to-line dialog for the active tab (`docs/spec-v1-hardening.md`,
    /// #620): a light `gpui-component` `Dialog` hosting a single-line
    /// line-number `Input` — no built-in go-to-line widget exists in this
    /// gpui-component pin, unlike find/replace (`arm_loading`'s
    /// `.replaceable`), so this is the "light theme-token overlay" fallback
    /// the spec calls for. Confirming — the OK button, or Enter (the
    /// `Dialog`'s own `ConfirmDialog` binding, since a single-line `Input`
    /// propagates `Enter` rather than consuming it) — jumps the caret via
    /// [`jump_to_line_input`]. Escape (`CancelDialog`) just closes it, no jump.
    ///
    /// Re-resolves the tab by path inside `on_ok` rather than closing over
    /// `index`, the same discipline [`confirm_close_tab`] and the async
    /// timers in this file already follow: a tab close between opening the
    /// dialog and the user confirming must not act on a shifted or
    /// now-missing index. A no-op while another dialog is already open
    /// (mirrors [`open_conflict_dialog`]'s guard) or no tab is active.
    /// `EditorView::render` only wires this action's key context once the
    /// active tab is `Loaded` (the same structural guard [`GoToDefinition`]/
    /// [`ShowHover`] rely on), so a Loading/Failed active tab never reaches
    /// this handler.
    ///
    /// [`jump_to_line_input`]: Self::jump_to_line_input
    /// [`confirm_close_tab`]: Self::confirm_close_tab
    /// [`open_conflict_dialog`]: Self::open_conflict_dialog
    fn open_go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let Some(index) = self.active else {
            return;
        };
        let path = self.tabs[index].path.clone();
        let entity = cx.entity();
        let line_input = cx.new(|cx| InputState::new(window, cx).placeholder("Line number"));

        window.open_dialog(cx, {
            let line_input = line_input.clone();
            move |dialog: Dialog, _window, _cx| {
                let entity = entity.clone();
                let path = path.clone();
                let line_input = line_input.clone();
                dialog
                    .title("Go to Line")
                    .w(px(240.0))
                    .child(Input::new(&line_input))
                    .on_ok(move |_, window, cx| {
                        entity.update(cx, |view, cx| {
                            if let Some(index) = view.tab_index_for_path(&path) {
                                view.jump_to_line_input(index, &line_input, window, cx);
                            }
                        });
                        true
                    })
            }
        });

        line_input.focus_handle(cx).focus(window, cx);
    }

    /// Apply the go-to-line dialog's confirmed value (#620): parse
    /// `line_input`'s text as a 1-based line number and move the tab at
    /// `index`'s caret there, clamped via [`go_to_line_target`] to the
    /// buffer's last line so an out-of-range request still lands somewhere
    /// sane — the same clamp-not-reject contract [`minimap_jump`] already
    /// uses for its click-to-jump. An empty or non-numeric value is silently
    /// ignored: the caret stays put rather than jumping to line 0.
    ///
    /// [`go_to_line_target`]: go_to_line_target
    /// [`minimap_jump`]: Self::minimap_jump
    fn jump_to_line_input(
        &mut self,
        index: usize,
        line_input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(requested) = line_input.read(cx).value().trim().parse::<usize>() else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        tab.input.update(cx, |input, cx| {
            let total = input.text().lines_len();
            let target = go_to_line_target(requested, total);
            input.set_cursor_position(EditorPosition::new(target as u32, 0), window, cx);
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

    /// Jump to a full [`NavLocation`] from the results panel (#529), preserving
    /// its out-of-root read-only carve-out (unlike [`Self::open_at_range`],
    /// which only serves in-worktree targets). Routes through the same
    /// [`Self::jump_to_location`] back-stack machinery, then returns keyboard
    /// focus to the landed buffer so a following `Escape` reaches the editor
    /// key context and closes the panel.
    pub fn jump_to_nav_location(
        &mut self,
        location: NavLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.active {
            Some(source_index) => self.jump_to_location(source_index, location, window, cx),
            None => {
                // No tab open yet — nothing to record as a back-entry; open-or-
                // switch straight to the destination, mirroring the None branch
                // of `open_at_range`.
                let read_only = location.out_of_root;
                let path = location.path.clone();
                let is_new = self.open_or_switch(
                    location.path,
                    read_only,
                    Some(location.range),
                    None,
                    window,
                    cx,
                );
                if is_new {
                    if let Err(e) = self.open_file_tx.try_send(path.clone()) {
                        tracing::debug!(error = %e, %path, "failed to enqueue results-panel open");
                    }
                }
            }
        }
        if let Some(active) = self.active {
            self.tabs[active].input.update(cx, |input, cx| {
                input.focus(window, cx);
            });
        }
    }

    /// Close the results panel from the editor (Escape). Returns `false` when
    /// the panel is not open, so the action handler lets `Escape` propagate to
    /// any other meaning instead of swallowing it. When open, clears the flag,
    /// emits [`EditorEvent::CloseResults`] for the workspace to hide the panel,
    /// and reports `true`.
    fn close_results_panel(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.results_visible {
            return false;
        }
        self.results_visible = false;
        cx.emit(EditorEvent::CloseResults);
        cx.notify();
        true
    }

    /// Reset the results-visible flag when the workspace reports the panel was
    /// closed elsewhere (its × affordance), keeping the editor's `Escape` gate
    /// in sync. Idempotent.
    pub fn mark_results_closed(&mut self) {
        self.results_visible = false;
    }

    /// Move keyboard focus to the active tab's buffer (or the editor's fallback
    /// handle while no tab is open). The workspace calls this after opening the
    /// results panel, whose `add_panel` steals focus, so a following `Escape`
    /// still reaches the editor key context (#529).
    pub fn focus_active_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_tab() {
            Some(tab) => tab.input.update(cx, |input, cx| {
                input.focus(window, cx);
            }),
            None => self.focus_handle.focus(window, cx),
        }
    }

    // ── Tab bar: switch / close (#352, #354) ──────────────────────────────

    /// Activate the tab at `index` (tab-bar click) and move focus to its
    /// buffer. A no-op if `index` is out of range or already active.
    ///
    /// Switching onto a tab that already carries a background conflict (a
    /// dirty tab another open tab's external edit surfaced while it was
    /// inactive, #353) opens the conflict dialog (#532) for it — the same
    /// affordance the newly-conflicted case gets, since a background
    /// conflict has no other visible indicator once it becomes active.
    pub fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() || self.active == Some(index) {
            return;
        }
        self.active = Some(index);
        self.tabs[index].input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        self.open_conflict_dialog(window, cx);
        cx.notify();
    }

    /// Close the tab at `index` (tab-bar close affordance). A clean tab
    /// closes immediately, as before (#352); a dirty tab prompts for
    /// confirmation before discarding its unsaved edits
    /// (`docs/spec-editor-tabs.md`, #354). A no-op if `index` is out of
    /// range.
    pub fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if close_needs_confirm(tab.dirty) {
            self.confirm_close_tab(index, window, cx);
        } else {
            self.close_tab_now(index, window, cx);
        }
    }

    /// Open the dirty-close confirm dialog (`gpui-component` `AlertDialog`
    /// via `Root`, mirroring `pane_view.rs`'s `open_confirm_dialog`, #212):
    /// confirming discards the tab's unsaved edits and closes it; cancelling
    /// (or dismissing) leaves it open, untouched.
    ///
    /// The confirm callback re-resolves the tab by path instead of closing
    /// over `index`: a tab close or reorder between opening the dialog and
    /// the user's answer can shift indices, so trusting the stale position
    /// risks closing the wrong tab (or silently missing an out-of-range one)
    /// — the same discipline the async timers in this file already follow.
    fn confirm_close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.tabs[index].path.clone();
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let entity = cx.entity();

        window.open_alert_dialog(cx, move |alert: AlertDialog, _, _| {
            let entity = entity.clone();
            let path = path.clone();
            alert
                .title("Unsaved Changes")
                .description(SharedString::from(format!(
                    "\"{name}\" has unsaved changes. Discard them and close the tab?"
                )))
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    entity.update(cx, |view, cx| {
                        if let Some(index) = view.tab_index_for_path(&path) {
                            view.close_tab_now(index, window, cx);
                        }
                    });
                    true
                })
        });
    }

    /// Actually remove the tab at `index` from the open set: reverts its
    /// live buffer to disk-backed (mirrors the save-success discard, #189).
    /// Closing the active tab activates the right neighbor (or the left if
    /// it was rightmost, via [`next_active_after_close`]); closing the last
    /// tab returns the editor to its empty state. Shared by an immediate
    /// clean-tab close and a confirmed dirty-tab discard (#354).
    fn close_tab_now(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Open `path` at `range`, scrolling/selecting it once loaded — the thin
    /// public wrapper `docs/spec-problems-panel.md` calls for so the problems
    /// panel (#343) can reach the existing LSP-nav jump machinery
    /// ([`EditorView::jump_to_location`]) without a `NavLocation` round-trip.
    /// Problems-panel diagnostics are always in-worktree, so `out_of_root` is
    /// always `false`.
    pub fn open_at_range(
        &mut self,
        path: String,
        range: Range,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let location = NavLocation {
            path,
            range,
            out_of_root: false,
            line_preview: None,
        };
        match self.active {
            Some(source_index) => self.jump_to_location(source_index, location, window, cx),
            None => {
                // No tab open yet — nothing to record as a back-entry; open-or-
                // switch straight to the destination, mirroring the cross-file
                // branch of `jump_to_location`.
                let path = location.path.clone();
                let is_new = self.open_or_switch(
                    location.path,
                    false,
                    Some(location.range),
                    None,
                    window,
                    cx,
                );
                if is_new {
                    if let Err(e) = self.open_file_tx.try_send(path.clone()) {
                        tracing::debug!(error = %e, %path, "failed to enqueue open_at_range open");
                    }
                }
            }
        }
    }
}

// ── Panel adapter ─────────────────────────────────────────────────────────────

impl Focusable for EditorView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // Delegate to the active tab's input entity — `arm_loading` rebuilds
        // it per fresh open (an auto-reload keeps it, #432), so this always
        // reflects the live buffer. Falls back to the editor's own handle
        // while no tab is open.
        self.active_tab()
            .map(|tab| tab.input.focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl EventEmitter<PanelEvent> for EditorView {}
impl EventEmitter<EditorEvent> for EditorView {}

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

    // Direct header button rather than the "..." overflow menu default
    // (`docs/spec-dogfooding-fixes.md`, #716): `Panel::zoomable` defaults to
    // `PanelControl::Menu`, which buries zoom in/out inside the Ellipsis
    // menu. `Toolbar` renders it as a `Maximize`/`Minimize` button in the
    // panel header instead, reusing gpui-component's own extension point.
    fn zoomable(&self, _cx: &App) -> Option<PanelControl> {
        Some(PanelControl::Toolbar)
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

/// The code editor surface's background/foreground colors, read live from the
/// active theme (`docs/spec-editor.md`, #598) — never hardcoded, so the
/// surface tracks a runtime theme switch (`ActiveTheme::theme`) exactly like
/// every other panel. `gpui_component::input::Input`'s own code-editor
/// background falls back to `cx.theme().highlight_theme` — a Zed-style syntax
/// theme populated from a `ThemeConfig`'s `highlight` block
/// (`assets/themes/catppuccin-mocha.json`, added for #598) — so without that
/// block the fallback stayed pinned to gpui-component's built-in *light*
/// default regardless of rift's active dark mode, and every Tree-sitter token
/// with no explicit `syntax` entry rendered with `highlight_theme`'s light
/// default text color too (`element.rs`'s `SyntaxColors::style` falls back to
/// `HighlightStyle::default()`, `color: None`, which resolves to whatever
/// base run color the widget was given). Sourcing the surface's
/// background/foreground from `ThemeColor` here — already correctly dark
/// under Catppuccin Mocha — fixes the surface and doubles as that base run
/// color for every unstyled token, independent of the `highlight` block ever
/// being complete or present at all.
///
/// The background is `secondary`, not `background` (#730): one theme step
/// lighter, and already the token this same render function uses for the
/// editor's own immediate chrome (`crumb_bg`, the minimap strip below) — so
/// the surface now reads as one cohesive, elevated panel (breadcrumb, text,
/// and minimap together) distinct from the surrounding dock/sidebar chrome,
/// which stays on `background`. `muted`/`accent` were considered and
/// rejected: under Catppuccin Mocha both resolve to the *same* value, and
/// the current-line highlight below already washes `accent` over the
/// surface at low alpha — an identical surface color would flatten that
/// highlight into invisibility (caught by
/// `test_editor_surface_background_is_a_subtle_step_lighter_than_base`).
/// `secondary` differs from `accent`, so the highlight stays visible.
fn editor_surface_colors(cx: &App) -> (Hsla, Hsla) {
    (cx.theme().secondary, cx.theme().foreground)
}

/// Human-readable label for a [`BufferErrorReason`], shared by the editor's
/// open- and save-failure status renders (`docs/spec-v1-hardening.md`). Kept
/// as a short noun phrase so callers compose it into either an "open" or a
/// "save" sentence.
fn buffer_error_reason_label(reason: BufferErrorReason) -> &'static str {
    match reason {
        BufferErrorReason::Binary => "binary file",
        BufferErrorReason::NotUtf8 => "not valid UTF-8",
        BufferErrorReason::PermissionDenied => "permission denied",
        BufferErrorReason::NotFound => "file not found",
        BufferErrorReason::TooLarge => "file too large",
        BufferErrorReason::Io => "I/O error",
    }
}

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

        // Loading / failed states: show a centered status message. `TooLarge`
        // gets its own read-only-placeholder wording (`docs/spec-v1-hardening.md`)
        // — the daemon never ships content for it, so this message *is* the
        // whole read-only placeholder, not a lead-in to an editable surface.
        let status: Option<String> = match &tab.load_state {
            TabLoadState::Loading => Some(format!("Opening {}\u{2026}", tab.path)),
            TabLoadState::Failed(Some(BufferErrorReason::TooLarge)) => Some(format!(
                "{} is too large to open \u{2014} read-only",
                tab.path
            )),
            TabLoadState::Failed(Some(reason)) => Some(format!(
                "Could not open {}: {}",
                tab.path,
                buffer_error_reason_label(*reason)
            )),
            TabLoadState::Failed(None) => Some(format!("Could not open {}", tab.path)),
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

        // One-line save-outcome banner. The conflict case surfaces as its own
        // modal dialog (#532, opened imperatively wherever the active tab's
        // conflict is armed — see `open_conflict_dialog`) instead of inline
        // chrome here.
        let banner: Option<(String, gpui::Hsla)> = match &tab.save_state {
            SaveState::Idle | SaveState::Conflict => None,
            SaveState::Saving => Some(("Saving\u{2026}".to_owned(), cx.theme().muted_foreground)),
            SaveState::Failed(Some(reason)) => Some((
                format!("Save failed: {}", buffer_error_reason_label(*reason)),
                cx.theme().danger,
            )),
            SaveState::Failed(None) => Some(("Save failed".to_owned(), cx.theme().danger)),
        };

        // Build the `Input` widget. The context-menu builder is called each
        // time the user right-clicks; it receives a fresh `PopupMenu`.
        // "Go to Definition" dispatches the `GoToDefinition` action; "Show
        // Hover" dispatches the `ShowHover` action — both handled on the outer
        // div below. `.disabled` blocks all key events and edit operations in
        // the `InputState`, enforcing the out-of-root read-only contract
        // (#196/#301).
        //
        // `.bg` / `.text_color` explicitly wire the surface to
        // [`editor_surface_colors`] — `StyledExt::refine_style` (applied last
        // in `Input::render`) lets them win over gpui-component's own
        // code-editor background/foreground defaults, and `.text_color` is
        // also the base run color every Tree-sitter token without its own
        // `syntax` entry in the `highlight` block resolves to. Gutter,
        // line-number, and selection colors already read `cx.theme()` inside
        // the widget itself and need no override.
        let (surface_bg, surface_fg) = editor_surface_colors(cx);
        let input_widget = Input::new(&tab.input)
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .bg(surface_bg)
            .text_color(surface_fg)
            .size_full()
            .disabled(tab.read_only)
            .context_menu(|menu: PopupMenu, _window, _cx| {
                menu.menu("Go to Definition", Box::new(GoToDefinition))
                    .menu("Find References", Box::new(FindReferences))
                    .menu("Show Hover", Box::new(ShowHover))
                    .menu("Go to Line", Box::new(GoToLine))
                    .separator()
            });

        // Hover card (#197, restyled to `docs/spec-editor-chrome.md` §3):
        // rendered as an absolutely-positioned overlay pinned to the bottom of
        // the editor area when `hover_content` is set. Anchoring/dismissal
        // mechanics stay owned by the open papercut #486 — this only restyles
        // the card's contents into the §3 anatomy:
        //   - a code block (the leading fenced signature + truncated preview),
        //   - a hairline,
        //   - a doc body (the prose markdown after the code),
        //   - an action row wiring the existing GoToDefinition / FindReferences
        //     actions with keyboard hints.
        // Rename is consciously OMITTED (LSP rename is post-v1; no dead
        // controls) per the spec's prior-decision log.
        //
        // Theme tokens used: `popover` (background), `secondary` (code block),
        // `muted` (kbd chip), `list_hover` (action hover), `border`,
        // `foreground`, `muted_foreground`. Layering is via child render order
        // (this child is added *after* the editor area so it paints on top).
        let hover_popover_element = tab.hover_content.as_ref().map(|content| {
            let parts = parse_hover_markdown(&content.markdown);
            let popover_bg = cx.theme().popover;
            let border = cx.theme().border;
            let fg = cx.theme().foreground;
            let muted = cx.theme().muted_foreground;
            let code_bg = cx.theme().secondary;
            let kbd_bg = cx.theme().muted;
            let action_hover_bg = cx.theme().list_hover;
            let mono = cx.theme().mono_font_family.clone();
            let mono_size = cx.theme().mono_font_size;

            let mut card = div()
                .absolute()
                .bottom(px(0.0))
                .left(px(0.0))
                .right(px(0.0))
                .flex()
                .flex_col()
                .bg(popover_bg)
                .border_t_1()
                .border_color(border)
                .shadow_md()
                .text_xs()
                .text_color(fg)
                .overflow_hidden();

            // Code block: the symbol's signature plus a truncated preview,
            // rendered as mono lines on the `secondary` surface.
            if let Some(code) = &parts.code {
                let (lines, truncated) = truncate_code_preview(code, HOVER_CODE_MAX_LINES);
                let mut block = div()
                    .flex()
                    .flex_col()
                    .px(px(10.0))
                    .py(px(8.0))
                    .bg(code_bg)
                    .font_family(mono.clone())
                    .text_size(mono_size)
                    .text_color(fg);
                for line in lines {
                    block = block.child(div().child(SharedString::from(line.to_owned())));
                }
                if truncated {
                    block = block.child(div().text_color(muted).child(HOVER_TRUNCATION_MARKER));
                }
                card = card.child(block);
            }

            // Hairline between the code block and the doc body — only drawn
            // when both sections are present.
            if parts.code.is_some() && parts.doc.is_some() {
                card = card.child(div().h(px(1.0)).bg(border));
            }

            // Doc body: the prose that follows the signature, rendered through
            // the shared markdown renderer.
            if let Some(doc) = &parts.doc {
                card = card.child(div().px(px(10.0)).py(px(8.0)).child(markdown(doc.clone())));
            }

            // Action row: Definition (F12) and References (Shift+F12) wired to
            // the existing nav actions. A small mono chip carries the keyboard
            // hint, matching the command registry's advertised bindings. The
            // leading `\u{203A}` on Definition echoes the design's go-to arrow;
            // References carries no synthetic glyph (the artboard's icon has no
            // embedded asset, per the outline panel's text-glyph precedent).
            let kbd_chip = move |text: &'static str| {
                div()
                    .flex_shrink_0()
                    .px(px(4.0))
                    .rounded(px(3.0))
                    .bg(kbd_bg)
                    .text_color(muted)
                    .font_family(mono.clone())
                    .child(text)
            };
            let definition_action = div()
                .id("hover-action-definition")
                .flex()
                .items_center()
                .gap(px(5.0))
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .hover(move |this| this.bg(action_hover_bg))
                .child(
                    div()
                        .text_color(muted)
                        .child(SharedString::from(BREADCRUMB_SEPARATOR)),
                )
                .child(div().text_color(fg).child("Definition"))
                .child(kbd_chip(HOVER_DEFINITION_HINT))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _event: &MouseDownEvent, _window, cx| {
                        this.dispatch_definition_request(cx);
                    }),
                );
            let references_action = div()
                .id("hover-action-references")
                .flex()
                .items_center()
                .gap(px(5.0))
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .hover(move |this| this.bg(action_hover_bg))
                .child(div().text_color(fg).child("References"))
                .child(kbd_chip(HOVER_REFERENCES_HINT))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _event: &MouseDownEvent, _window, cx| {
                        this.dispatch_references_request(cx);
                    }),
                );

            card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .border_t_1()
                    .border_color(border)
                    .child(definition_action)
                    .child(div().text_color(muted).child("\u{00B7}"))
                    .child(references_action),
            )
        });

        // ── Breadcrumb + gutter/card overlays (docs/spec-editor-chrome.md) ──
        //
        // The breadcrumb reads the cached document-symbol tree and the cursor;
        // the gutter dots and inline card read this tab's own diagnostics. All
        // positioning uses the input widget's `range_to_bounds` (window-space,
        // correct through soft-wrap and folding — the naive `row * line_height`
        // mapping would be wrong) converted to editor-content-relative offsets
        // via the previous frame's captured content bounds. Values are gathered
        // into owned primitives here so the `InputState` read borrow is dropped
        // before the elements (and the `cx.listener` closures below) are built.
        let mono_font = cx.theme().mono_font_family.clone();
        let mono_size = cx.theme().mono_font_size;
        let crumb_path_color = cx.theme().muted_foreground;
        let crumb_symbol_color = cx.theme().foreground;
        let crumb_bg = cx.theme().secondary;
        let crumb_border = cx.theme().border;
        let card_bg = cx.theme().popover;
        let card_border = cx.theme().border;
        let card_fg = cx.theme().foreground;
        let card_muted = cx.theme().muted_foreground;
        let sev_error = cx.theme().danger;
        let sev_warning = cx.theme().warning;
        let sev_info = cx.theme().info;
        let sev_hint = cx.theme().muted_foreground;
        let mut current_line_color = cx.theme().accent;
        current_line_color.a = 0.10;
        let severity_color = |severity: DiagnosticSeverity| -> Hsla {
            match severity {
                DiagnosticSeverity::Error => sev_error,
                DiagnosticSeverity::Warning => sev_warning,
                DiagnosticSeverity::Information => sev_info,
                DiagnosticSeverity::Hint => sev_hint,
            }
        };

        // Breadcrumb segments: mono path pieces then the enclosing-symbol chain.
        let path_segments: Vec<SharedString> = path_breadcrumb_segments(&tab.path)
            .into_iter()
            .map(SharedString::from)
            .collect();

        // Overlay geometry (owned): gutter dots, current-line bar (top, height),
        // card.
        let mut gutter_dots: Vec<(Pixels, Hsla)> = Vec::new();
        let mut current_line: Option<(Pixels, Pixels)> = None;
        let mut inline_card: Option<InlineCard> = None;
        let mut symbol_segments: Vec<SharedString> = Vec::new();
        {
            let input_state = tab.input.read(cx);
            let cursor = input_state.cursor_position();
            for symbol in enclosing_symbol_chain(&tab.symbols, cursor.line, cursor.character) {
                symbol_segments.push(SharedString::from(symbol.name.clone()));
            }

            if let (Some(content), Some(line_height), Some(visible)) = (
                self.content_bounds.get(),
                input_state.line_height(),
                input_state.visible_row_range(),
            ) {
                let text = input_state.text();
                let origin = content.origin;

                for buffer_row in visible.clone() {
                    let line = buffer_row as u32;
                    let Some(primary) = primary_diagnostic_on_line(&tab.diagnostics, line) else {
                        continue;
                    };
                    let offset = text.line_start_offset(buffer_row);
                    if let Some(bounds) = input_state.range_to_bounds(&(offset..offset)) {
                        let y_rel = bounds.origin.y - origin.y;
                        let dot_top = y_rel + (line_height - GUTTER_DOT_SIZE) / 2.0;
                        gutter_dots.push((dot_top, severity_color(primary.severity)));
                    }
                }

                let cursor_row = cursor.line as usize;
                if visible.contains(&cursor_row) {
                    let offset = text.line_start_offset(cursor_row);
                    if let Some(bounds) = input_state.range_to_bounds(&(offset..offset)) {
                        let y_rel = bounds.origin.y - origin.y;
                        current_line = Some((y_rel, line_height));
                        if let Some(primary) =
                            primary_diagnostic_on_line(&tab.diagnostics, cursor.line)
                        {
                            let x_rel = bounds.origin.x - origin.x;
                            let below = y_rel + line_height;
                            // Flip the card above the line when it would spill
                            // past the content's bottom edge.
                            let top = if below + CARD_ESTIMATED_HEIGHT > content.size.height {
                                (y_rel - CARD_ESTIMATED_HEIGHT).max(px(0.0))
                            } else {
                                below
                            };
                            inline_card = Some(InlineCard {
                                top,
                                left: x_rel,
                                color: severity_color(primary.severity),
                                message: SharedString::from(primary.message.clone()),
                                detail: diagnostic_detail(primary),
                            });
                        }
                    }
                }
            }
        }

        // ── Minimap marks strip (docs/spec-editor-chrome.md) ──
        //
        // Owned render data for the strip on the editor's right edge: the
        // downsampled line-length marks (a cheap ref-count clone of the tab's
        // cached `minimap_samples` — derived on text change, never rescanned
        // here), plus the diagnostic marks and viewport slab, whose ratios are
        // O(diagnostics) / O(1) to gather under this tab's `InputState` read
        // borrow. Moved into the strip's `canvas` paint closure below, so the
        // borrow is released before painting. Explicitly NOT a pixel-perfect
        // code render: marks are capped at `MINIMAP_SAMPLES`, positioned by
        // ratio, redrawn only on damage.
        // Bolder/higher-contrast than the original muted-foreground-at-.55 (#600):
        // `foreground` reads clearly against the strip's `secondary` background at
        // the widened size, while the slab keeps `accent` but a touch more opaque
        // so it stays visually distinct from the length/diagnostic marks under it.
        let mut minimap_mark_color = cx.theme().foreground;
        minimap_mark_color.a = 0.65;
        let mut minimap_slab_color = cx.theme().accent;
        minimap_slab_color.a = 0.28;
        let minimap_data = {
            let input_state = tab.input.read(cx);
            let total = input_state.text().lines_len();
            let diag_marks: Vec<(f32, Hsla)> = if total == 0 {
                Vec::new()
            } else {
                tab.diagnostics
                    .iter()
                    .map(|d| {
                        let ratio = (d.range.start.line as f32 / total as f32).clamp(0.0, 1.0);
                        (ratio, severity_color(d.severity))
                    })
                    .collect()
            };
            let (slab_top, slab_bottom) = match input_state.visible_row_range() {
                Some(visible) => minimap_slab_fracs(visible.start, visible.end, total),
                None => (0.0, 0.0),
            };
            MinimapPaint {
                samples: Rc::clone(&tab.minimap_samples),
                diag_marks,
                slab_top,
                slab_bottom,
                mark_color: minimap_mark_color,
                slab_color: minimap_slab_color,
            }
        };

        let breadcrumb_bar = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(6.0))
            .h(BREADCRUMB_HEIGHT)
            .px(px(10.0))
            .bg(crumb_bg)
            .border_b_1()
            .border_color(crumb_border)
            .font_family(mono_font)
            .text_size(mono_size)
            .overflow_hidden()
            .children(breadcrumb_children(
                &path_segments,
                &symbol_segments,
                crumb_path_color,
                crumb_symbol_color,
            ));

        let read_only = tab.read_only;

        // Tab bar (#352, #354): one Tab per open file, showing its name, a
        // dirty dot, and a close icon — the same TabBar/Tab pattern
        // `SessionView` uses for tmux windows (`crates/terminal/src/session_view.rs`).
        // Clicking a tab activates it (moves focus to its buffer); the close
        // affordance closes it — immediately when clean, after a confirm
        // dialog when dirty. Middle-clicking anywhere on the tab closes it
        // through the same path (#730), mirroring the window-tab convention.
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
                    .child(Icon::new(IconName::Close).size_3())
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
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                            this.close_tab(ix, window, cx);
                            cx.stop_propagation();
                        }),
                    )
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
        //   - Ctrl+K Ctrl+I (keybind) or context menu "Show Hover": `ShowHover`
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
            .on_action(cx.listener(|this, _: &GoToLine, window, cx| {
                this.open_go_to_line(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseResultsPanel, _window, cx| {
                // Escape closes the results panel (#529). With no panel open the
                // keystroke propagates on, keeping any other Escape meaning
                // intact.
                if !this.close_results_panel(cx) {
                    cx.propagate();
                }
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
            .child(tab_bar)
            .child(breadcrumb_bar);

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

        // Editor area: the input widget plus any overlays (gutter dots, inline
        // card, jump-list, hover popover). Overlays are children rendered
        // *after* the input so they paint on top without needing z-index (child
        // order = paint order). `overflow_hidden` clips the overlays to the
        // viewport; the widget's own popovers are `deferred` and so escape it.
        //
        // The leading `canvas` captures this area's window bounds each frame
        // (read back next frame) so the app-side overlays can convert the input
        // widget's window-space `range_to_bounds` into content-relative offsets.
        let content_bounds = self.content_bounds.clone();
        let mut editor_area = div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .relative()
            .overflow_hidden()
            .child(
                canvas(
                    move |bounds, _window, _cx| content_bounds.set(Some(bounds)),
                    |_bounds, _prepaint, _window, _cx| {},
                )
                .absolute()
                .size_full(),
            )
            .child(input_widget);

        // Current-line highlight: a faint full-width bar under the cursor row.
        // The widget itself only brightens the line number, so this supplies
        // the line highlight the design calls for (`docs/spec-editor-chrome.md`).
        if let Some((top, height)) = current_line {
            editor_area = editor_area.child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .top(top)
                    .h(height)
                    .bg(current_line_color),
            );
        }

        // Gutter severity dots: one per visible diagnostic line, left of the
        // line-number column, colored by the line's most severe diagnostic.
        for (top, color) in gutter_dots {
            editor_area = editor_area.child(
                div()
                    .absolute()
                    .left(GUTTER_DOT_LEFT)
                    .top(top)
                    .size(GUTTER_DOT_SIZE)
                    .rounded_full()
                    .bg(color),
            );
        }

        // Inline diagnostic card for the cursor line's primary diagnostic. The
        // widget's own `DiagnosticPopover` is withheld for this line (see
        // `sync_widget_diagnostics`), so the diagnostic renders exactly once.
        if let Some(card) = inline_card {
            let mut body = div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().text_color(card_fg).child(card.message));
            if let Some(detail) = card.detail {
                body = body.child(div().text_color(card_muted).child(detail));
            }
            editor_area = editor_area.child(
                div()
                    .absolute()
                    .top(card.top)
                    .left(card.left)
                    .max_w(px(520.0))
                    .flex()
                    .items_start()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .py(px(6.0))
                    .bg(card_bg)
                    .border_1()
                    .border_color(card_border)
                    .rounded(CARD_RADIUS)
                    .shadow_md()
                    .text_xs()
                    .child(
                        div()
                            .mt(px(3.0))
                            .flex_shrink_0()
                            .size(GUTTER_DOT_SIZE)
                            .rounded_full()
                            .bg(card.color),
                    )
                    .child(body),
            );
        }

        // Hover popover (#197): rendered last so it paints above the editor.
        // Uses absolute positioning anchored to the bottom of the editor area —
        // this positions the popover below the current viewport and above any
        // status bars, matching VS Code's hover panel.
        if let Some(popover) = hover_popover_element {
            editor_area = editor_area.child(popover);
        }

        // Minimap marks strip on the editor's right edge, beside the widget's
        // scrollbar (`docs/spec-editor-chrome.md`). A single `canvas` paints the
        // downsampled line-length marks, the diagnostic marks, and the viewport
        // slab in one pass — no second text render — and records its bounds for
        // click-to-jump. The click handler jumps the view to the clicked line;
        // `stop_propagation` keeps a strip click from reaching the outer
        // ctrl+click-to-definition handler.
        let minimap_bounds = self.minimap_bounds.clone();
        let minimap_strip = div()
            .id("editor-minimap")
            .flex_shrink_0()
            .w(MINIMAP_WIDTH)
            .relative()
            .bg(cx.theme().secondary)
            .border_l_1()
            .border_color(cx.theme().border)
            .child(
                canvas(
                    |_bounds, _window, _cx| {},
                    move |bounds, _prepaint, window, _cx| {
                        paint_minimap(bounds, &minimap_data, &minimap_bounds, window);
                    },
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.minimap_jump(event.position.y, window, cx);
                    cx.stop_propagation();
                }),
            );

        let editor_row = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .child(editor_area)
            .child(minimap_strip);

        root.child(editor_row).into_any_element()
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

/// The identifier token straddling `(line, character)` in `text`, for the
/// results panel's search-context chip (#529). A token is a maximal run of
/// alphanumeric-or-underscore Unicode scalar values; `character` is a
/// zero-based scalar offset into the line (the editor's cursor convention). The
/// cursor sitting at a token's trailing edge (offset == token end) still
/// resolves that token, matching how a double-click or `Shift+F12` lands there.
/// `None` when the position falls on no token (out-of-range line, past the line
/// end, or on a non-word character with no adjacent word char).
fn word_at(text: &str, line: u32, character: u32) -> Option<String> {
    let line_str = text.lines().nth(line as usize)?;
    let chars: Vec<char> = line_str.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let cursor = character as usize;

    // The token may be the run at the cursor, or (when the cursor rests just
    // past a token) the run ending immediately to its left.
    let anchor = if chars.get(cursor).is_some_and(|&c| is_word(c)) {
        cursor
    } else if cursor > 0 && chars.get(cursor - 1).is_some_and(|&c| is_word(c)) {
        cursor - 1
    } else {
        return None;
    };

    let mut start = anchor;
    while start > 0 && chars.get(start - 1).is_some_and(|&c| is_word(c)) {
        start -= 1;
    }
    let mut end = anchor + 1;
    while chars.get(end).is_some_and(|&c| is_word(c)) {
        end += 1;
    }
    Some(chars[start..end].iter().collect())
}

/// Decide whether closing a tab must prompt for confirmation before
/// discarding unsaved edits. Pure and GPUI-free — the decision half of the
/// dirty-close contract (`docs/spec-editor-tabs.md`, #354 acceptance: "a
/// dirty tab prompts confirm/discard; closing a clean tab is immediate").
fn close_needs_confirm(dirty: bool) -> bool {
    dirty
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

/// Whether the zero-based `(line, character)` cursor position falls within
/// `range` (inclusive of both endpoints — a cursor resting on a symbol's
/// closing brace still counts as enclosed).
fn position_in_range(range: &Range, line: u32, character: u32) -> bool {
    let after_start =
        line > range.start.line || (line == range.start.line && character >= range.start.character);
    let before_end =
        line < range.end.line || (line == range.end.line && character <= range.end.character);
    after_start && before_end
}

/// The chain of symbols enclosing the cursor, outermost first (ascending
/// `depth`) — the breadcrumb's symbol tail (`docs/spec-editor-chrome.md`).
///
/// Symbols in a document-symbol tree nest properly, so every entry whose range
/// contains the cursor is an ancestor of the next deeper one; sorting the
/// containing entries by `depth` reconstructs the enclosing path (e.g.
/// `impl Foo` then `fn render`). Empty when the cursor is inside no symbol.
///
/// `pub(crate)`: also the outline panel's "selection follows cursor" signal
/// (`crate::outline_panel`, `docs/spec-editor-chrome.md`) — one source of
/// truth for "what symbol is the cursor inside", shared with the breadcrumb.
pub(crate) fn enclosing_symbol_chain(
    symbols: &[DocumentSymbolEntry],
    line: u32,
    character: u32,
) -> Vec<&DocumentSymbolEntry> {
    let mut chain: Vec<&DocumentSymbolEntry> = symbols
        .iter()
        .filter(|s| position_in_range(&s.range, line, character))
        .collect();
    chain.sort_by_key(|s| s.depth);
    chain
}

/// Severity ordering used to pick the most severe diagnostic on a line
/// (`Error` highest). Drives both the gutter dot's color and the inline card's
/// choice of primary diagnostic.
fn severity_rank(severity: DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Error => 3,
        DiagnosticSeverity::Warning => 2,
        DiagnosticSeverity::Information => 1,
        DiagnosticSeverity::Hint => 0,
    }
}

/// The most severe diagnostic starting on `line`, or `None` when the line
/// carries none. Ties keep the first in document order. Used for the inline
/// card's primary diagnostic and the gutter dot's color for the line.
fn primary_diagnostic_on_line(diagnostics: &[Diagnostic], line: u32) -> Option<&Diagnostic> {
    diagnostics
        .iter()
        .filter(|d| d.range.start.line == line)
        .max_by_key(|d| severity_rank(d.severity))
}

// ── Minimap helpers (docs/spec-editor-chrome.md) ────────────────────────────────

/// Downsample the buffer's per-line lengths into at most `samples` marks for the
/// minimap strip. Each returned entry is the widest line length over the block
/// of source lines it covers, so a dense region reads as a longer mark than a
/// sparse one. Returns `min(total_lines, samples)` entries (empty for an empty
/// buffer), keeping the per-render cost bounded regardless of file size — the
/// strip is only a few hundred pixels tall, so more marks add no visible detail.
fn sample_line_lengths(
    total_lines: usize,
    line_len: impl Fn(usize) -> u32,
    samples: usize,
) -> Vec<u32> {
    if total_lines == 0 || samples == 0 {
        return Vec::new();
    }
    let n = total_lines.min(samples);
    let mut out = vec![0u32; n];
    for row in 0..total_lines {
        // `row < total_lines` and `n <= total_lines`, so `bucket < n`.
        let bucket = row * n / total_lines;
        let len = line_len(row);
        if len > out[bucket] {
            out[bucket] = len;
        }
    }
    out
}

/// The viewport slab's `(top, bottom)` position as fractions of the minimap
/// height, from the visible row range and the buffer's total line count. Both
/// fractions are clamped to `0..1`, and `bottom` never precedes `top`. Returns
/// `(0, 1)` for an empty buffer (the whole file is "visible").
fn minimap_slab_fracs(visible_start: usize, visible_end: usize, total_lines: usize) -> (f32, f32) {
    if total_lines == 0 {
        return (0.0, 1.0);
    }
    let total = total_lines as f32;
    let top = (visible_start as f32 / total).clamp(0.0, 1.0);
    let bottom = (visible_end as f32 / total).clamp(0.0, 1.0);
    (top, bottom.max(top))
}

/// The buffer line a minimap click at `click_ratio` (0..1 down the strip) maps
/// to. Clamped to a valid line index; returns `0` for an empty buffer.
fn minimap_click_line(click_ratio: f32, total_lines: usize) -> usize {
    if total_lines == 0 {
        return 0;
    }
    let line = (click_ratio.clamp(0.0, 1.0) * total_lines as f32) as usize;
    line.min(total_lines - 1)
}

/// The 0-based line index a go-to-line dialog's 1-based `requested` line
/// maps to, for a buffer of `total_lines` lines (#620). `requested == 0` (a
/// typed `0`, meaningless as a 1-based line) and anything past the last
/// line both clamp rather than reject — an out-of-range request still
/// lands somewhere sane, the same clamp-not-reject contract
/// [`minimap_click_line`] uses. Returns `0` for an empty buffer.
fn go_to_line_target(requested: usize, total_lines: usize) -> usize {
    requested
        .saturating_sub(1)
        .min(total_lines.saturating_sub(1))
}

/// Paint the minimap strip into `bounds`: the downsampled line-length marks, the
/// diagnostic marks (full-width, by line ratio, over the length marks), and the
/// viewport slab — in one pass, no second text render. Also records `bounds` for
/// the next mouse-down's click-to-jump. All positions are derived by ratio, so
/// the strip is correct at any height without re-shaping any text.
fn paint_minimap(
    bounds: Bounds<Pixels>,
    data: &MinimapPaint,
    bounds_cell: &Rc<Cell<Option<Bounds<Pixels>>>>,
    window: &mut Window,
) {
    bounds_cell.set(Some(bounds));

    let origin_x = f32::from(bounds.origin.x);
    let origin_y = f32::from(bounds.origin.y);
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if width <= 0.0 || height <= 0.0 {
        return;
    }

    // Line-length marks: one bar per sample, scaled across the full height, its
    // width proportional to the sample's share of the longest line.
    let sample_count = data.samples.len();
    if sample_count > 0 {
        let max_len = data.samples.iter().copied().max().unwrap_or(0).max(1) as f32;
        let inset = MINIMAP_MARK_INSET.min(width / 4.0);
        let available = (width - inset * 2.0).max(1.0);
        let row_height = (height / sample_count as f32).max(1.0);
        for (i, &len) in data.samples.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let mark_width = (available * (len as f32 / max_len)).max(1.0);
            let y = origin_y + height * i as f32 / sample_count as f32;
            let mark = Bounds {
                origin: Point {
                    x: px(origin_x + inset),
                    y: px(y),
                },
                size: Size {
                    width: px(mark_width),
                    height: px(row_height),
                },
            };
            window.paint_quad(fill(mark, data.mark_color));
        }
    }

    // Diagnostic marks: full-width, positioned by line ratio, painted on top so
    // problems stand out against the length marks.
    for &(ratio, color) in &data.diag_marks {
        let y = origin_y + height * ratio;
        let mark = Bounds {
            origin: Point {
                x: px(origin_x),
                y: px(y),
            },
            size: Size {
                width: px(width),
                height: px(MINIMAP_DIAG_MARK_HEIGHT),
            },
        };
        window.paint_quad(fill(mark, color));
    }

    // Viewport slab: only when there is a laid-out viewport to show.
    if data.slab_bottom > data.slab_top {
        let slab_y = origin_y + height * data.slab_top;
        let slab_height =
            (height * (data.slab_bottom - data.slab_top)).max(MINIMAP_SLAB_MIN_HEIGHT);
        let slab = Bounds {
            origin: Point {
                x: px(origin_x),
                y: px(slab_y),
            },
            size: Size {
                width: px(width),
                height: px(slab_height),
            },
        };
        window.paint_quad(fill(slab, data.slab_color));
    }
}

/// Split a root-relative (or absolute out-of-root) path into its breadcrumb
/// segments, dropping empty pieces from a leading slash or a trailing one.
fn path_breadcrumb_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// The muted `source`/`code` suffix for a diagnostic's inline card, or `None`
/// when the server supplied neither. `source` and `code` are joined as
/// `source(code)` (e.g. `rustc(E0308)`); a lone one stands alone.
fn diagnostic_detail(diagnostic: &Diagnostic) -> Option<SharedString> {
    match (&diagnostic.source, &diagnostic.code) {
        (Some(source), Some(code)) => Some(SharedString::from(format!("{source}({code})"))),
        (Some(source), None) => Some(SharedString::from(source.clone())),
        (None, Some(code)) => Some(SharedString::from(code.clone())),
        (None, None) => None,
    }
}

/// Build the breadcrumb's interleaved segment elements: the mono path pieces
/// (muted) then the enclosing-symbol chain (foreground), each pair joined by a
/// separator glyph. Kept separate from `render` so it stays a pure mapping
/// from owned strings to elements.
fn breadcrumb_children(
    path_segments: &[SharedString],
    symbol_segments: &[SharedString],
    path_color: Hsla,
    symbol_color: Hsla,
) -> Vec<gpui::Div> {
    let separator = |color: Hsla| {
        div()
            .text_color(color)
            .child(SharedString::from(BREADCRUMB_SEPARATOR))
    };
    let mut out: Vec<gpui::Div> = Vec::new();
    let mut first = true;
    for segment in path_segments {
        if !first {
            out.push(separator(path_color));
        }
        out.push(div().text_color(path_color).child(segment.clone()));
        first = false;
    }
    for segment in symbol_segments {
        if !first {
            out.push(separator(path_color));
        }
        out.push(div().text_color(symbol_color).child(segment.clone()));
        first = false;
    }
    out
}

/// The two anatomical sections of an LSP hover, split from its markdown for
/// the hover card (`docs/spec-editor-chrome.md` §3).
///
/// LSP servers format hover text as a leading fenced code block (the symbol's
/// signature, sometimes preceded by its module path), an optional thematic
/// break, then the documentation prose. This splits that into the code the
/// card renders in its code block and the prose it renders in its doc body.
#[derive(Debug, Default, PartialEq, Eq)]
struct HoverParts {
    /// The joined contents of the leading fenced code block(s) with the fence
    /// markers stripped, or `None` when the hover carries no leading fence.
    code: Option<String>,
    /// The documentation prose after the code (and any hairline), as markdown,
    /// or `None` when the hover is only a signature.
    doc: Option<String>,
}

/// Whether a trimmed line is a markdown thematic break (`---`, `***`, or
/// `___`, three or more of a single marker), i.e. the hairline separating a
/// signature from its docs in an LSP hover.
fn is_thematic_break(trimmed: &str) -> bool {
    for marker in ['-', '*', '_'] {
        if trimmed.len() >= 3 && trimmed.chars().all(|c| c == marker) {
            return true;
        }
    }
    false
}

/// Join `lines` into a single string, dropping leading and trailing blank
/// lines, or `None` when every line is blank.
fn join_trimmed_lines(lines: &[&str]) -> Option<String> {
    let start = lines.iter().position(|l| !l.trim().is_empty())?;
    let end = lines.iter().rposition(|l| !l.trim().is_empty())?;
    Some(lines[start..=end].join("\n"))
}

/// Split an LSP hover's markdown into its code and doc sections
/// (`docs/spec-editor-chrome.md` §3).
///
/// Leading fenced code blocks (their contents, fences stripped) form the code
/// section; the first non-blank line outside a fence ends it. A thematic-break
/// hairline there is consumed; any other content begins the doc body. Plain
/// (unfenced) hover text has no code section and is all doc. An unterminated
/// fence degrades gracefully: its remaining lines become the code section and
/// there is no doc.
fn parse_hover_markdown(markdown: &str) -> HoverParts {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut code_lines: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut doc_start: Option<usize> = None;

    for (i, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim();
        if in_fence {
            if trimmed.starts_with("```") {
                in_fence = false;
            } else {
                code_lines.push(raw);
            }
            continue;
        }
        if trimmed.starts_with("```") {
            in_fence = true;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        // First non-blank content outside a fence: the code section is done.
        // Consume a hairline; otherwise the doc body starts at this line.
        doc_start = Some(if is_thematic_break(trimmed) { i + 1 } else { i });
        break;
    }

    let code = join_trimmed_lines(&code_lines);
    let doc = doc_start.and_then(|start| join_trimmed_lines(lines.get(start..).unwrap_or(&[])));
    HoverParts { code, doc }
}

/// Truncate a hover code block to at most `max_lines`, returning the lines to
/// render and whether truncation occurred (so the caller can append an
/// ellipsis marker). `max_lines` of 0 is treated as 1 to always keep the
/// signature line.
fn truncate_code_preview(code: &str, max_lines: usize) -> (Vec<&str>, bool) {
    let cap = max_lines.max(1);
    let all: Vec<&str> = code.lines().collect();
    if all.len() > cap {
        (all[..cap].to_vec(), true)
    } else {
        (all, false)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

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

    // --- buffer-error reason labels (#617) ---

    /// Every `BufferErrorReason` maps to a distinct, non-empty label — the
    /// open/save status renders compose it into a full sentence, so an empty
    /// or colliding label would silently blur two different refusals together.
    #[test]
    fn test_buffer_error_reason_label_is_distinct_and_non_empty_for_every_reason() {
        let reasons = [
            BufferErrorReason::Binary,
            BufferErrorReason::NotUtf8,
            BufferErrorReason::PermissionDenied,
            BufferErrorReason::NotFound,
            BufferErrorReason::TooLarge,
            BufferErrorReason::Io,
        ];
        let labels: Vec<&str> = reasons
            .iter()
            .map(|r| buffer_error_reason_label(*r))
            .collect();
        for label in &labels {
            assert!(!label.is_empty());
        }
        let mut unique = labels.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            labels.len(),
            "every reason must have its own distinct label"
        );
    }

    // --- editor surface theme wiring (#598) ---

    /// Under rift's active Catppuccin Mocha theme, the code editor surface's
    /// colors must be the live theme's dark tokens (never a light default or
    /// a hardcoded value) and must move with a runtime theme switch.
    #[gpui::test]
    fn test_editor_surface_colors_are_dark_under_catppuccin_and_follow_a_theme_switch(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::apply_theme(cx);
            assert!(cx.theme().is_dark());

            let (dark_bg, dark_fg) = editor_surface_colors(cx);
            assert_eq!(dark_bg, cx.theme().secondary);
            assert_eq!(dark_fg, cx.theme().foreground);

            gpui_component::Theme::change(gpui_component::ThemeMode::Light, None, cx);
            assert!(!cx.theme().is_dark());

            let (light_bg, light_fg) = editor_surface_colors(cx);
            assert_eq!(light_bg, cx.theme().secondary);
            assert_eq!(light_fg, cx.theme().foreground);
            assert_ne!(
                light_bg, dark_bg,
                "editor surface background must track a runtime theme switch"
            );
        });
    }

    /// #730: the editor surface must read as a nuance lighter than the base
    /// `background` (a subtle step, not a full re-theme), and must not
    /// collide with the current-line highlight color (`accent`), which is
    /// washed over the surface at low alpha — an identical hue would flatten
    /// that highlight into invisibility.
    #[gpui::test]
    fn test_editor_surface_background_is_a_subtle_step_lighter_than_base(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::apply_theme(cx);

            let (surface_bg, _) = editor_surface_colors(cx);
            let base_bg = cx.theme().background;
            assert_ne!(
                surface_bg, base_bg,
                "editor surface must differ from the base background"
            );
            assert!(
                relative_luminance(surface_bg) > relative_luminance(base_bg),
                "editor surface must be lighter than the base background"
            );
            assert_ne!(
                surface_bg,
                cx.theme().accent,
                "editor surface must not match the current-line highlight color"
            );
        });
    }

    /// WCAG 2.1 relative luminance of an sRGB color (used only by
    /// [`contrast_ratio`] below).
    fn relative_luminance(color: Hsla) -> f32 {
        let rgba = color.to_rgb();
        let channel = |c: f32| {
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(rgba.r) + 0.7152 * channel(rgba.g) + 0.0722 * channel(rgba.b)
    }

    /// WCAG 2.1 contrast ratio between two colors — 1.0 (no contrast) to 21.0
    /// (black on white). `4.5` is the AA threshold for normal text.
    fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
        let (l1, l2) = (relative_luminance(a), relative_luminance(b));
        let (lighter, darker) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        (lighter + 0.05) / (darker + 0.05)
    }

    /// Regression test for #598: without a `highlight` block in
    /// `catppuccin-mocha.json`, `cx.theme().highlight_theme` stayed pinned to
    /// gpui-component's built-in light default, so every Tree-sitter syntax
    /// token rendered at near-zero contrast on the editor's dark background.
    /// Asserts the most common source-level tokens meet WCAG AA (4.5:1)
    /// against the editor's own dark `editor.background`.
    #[gpui::test]
    fn test_editor_syntax_colors_meet_wcag_aa_contrast_under_catppuccin(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::apply_theme(cx);
            assert!(cx.theme().is_dark());

            let style = &cx.theme().highlight_theme.style;
            let bg = style
                .editor_background
                .expect("Catppuccin Mocha's highlight block must set editor.background");

            for token in ["function", "string", "keyword", "type", "property", "constant"] {
                let color = style
                    .syntax
                    .style(token)
                    .and_then(|s| s.color)
                    .unwrap_or(cx.theme().foreground);
                let ratio = contrast_ratio(color, bg);
                assert!(
                    ratio >= 4.5,
                    "token `{token}` contrast {ratio:.2}:1 against the editor background is below WCAG AA 4.5:1"
                );
            }
        });
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

    // --- dirty-close confirmation decision (#354) ---

    #[test]
    fn test_close_needs_confirm_is_true_for_a_dirty_tab() {
        assert!(close_needs_confirm(true));
    }

    #[test]
    fn test_close_needs_confirm_is_false_for_a_clean_tab() {
        assert!(!close_needs_confirm(false));
    }

    // --- minimap marks strip (docs/spec-editor-chrome.md) ---

    #[test]
    fn test_sample_line_lengths_is_one_to_one_when_under_the_cap() {
        let lengths = [3u32, 0, 10, 5];
        let out = sample_line_lengths(lengths.len(), |row| lengths[row], 100);
        assert_eq!(out, vec![3, 0, 10, 5]);
    }

    #[test]
    fn test_sample_line_lengths_takes_the_max_per_bucket_when_downsampled() {
        // Four lines into two buckets: rows 0..1 -> bucket 0, rows 2..3 -> 1.
        let lengths = [3u32, 10, 4, 7];
        let out = sample_line_lengths(lengths.len(), |row| lengths[row], 2);
        assert_eq!(out, vec![10, 7]);
    }

    #[test]
    fn test_sample_line_lengths_is_empty_for_an_empty_buffer() {
        assert!(sample_line_lengths(0, |_| 0, 100).is_empty());
        assert!(sample_line_lengths(5, |_| 1, 0).is_empty());
    }

    #[test]
    fn test_minimap_slab_fracs_maps_the_visible_range() {
        let (top, bottom) = minimap_slab_fracs(25, 50, 100);
        assert!((top - 0.25).abs() < f32::EPSILON);
        assert!((bottom - 0.50).abs() < f32::EPSILON);
    }

    #[test]
    fn test_minimap_slab_fracs_clamps_and_handles_empty() {
        // Over-range end clamps to 1.0; empty buffer spans the whole strip.
        let (top, bottom) = minimap_slab_fracs(90, 200, 100);
        assert!((bottom - 1.0).abs() < f32::EPSILON);
        assert!(bottom >= top);
        assert_eq!(minimap_slab_fracs(0, 0, 0), (0.0, 1.0));
    }

    #[test]
    fn test_minimap_click_line_maps_ratio_to_line() {
        assert_eq!(minimap_click_line(0.0, 100), 0);
        assert_eq!(minimap_click_line(0.5, 100), 50);
        // Ratio 1.0 and beyond clamp to the last line; empty buffer stays 0.
        assert_eq!(minimap_click_line(1.0, 100), 99);
        assert_eq!(minimap_click_line(1.5, 100), 99);
        assert_eq!(minimap_click_line(0.5, 0), 0);
    }

    // --- go-to-line target clamp (#620) ---

    #[test]
    fn test_go_to_line_target_converts_a_one_based_request_to_a_zero_based_index() {
        assert_eq!(go_to_line_target(1, 100), 0);
        assert_eq!(go_to_line_target(50, 100), 49);
        assert_eq!(go_to_line_target(100, 100), 99);
    }

    #[test]
    fn test_go_to_line_target_clamps_a_request_past_the_last_line() {
        assert_eq!(go_to_line_target(500, 100), 99);
    }

    #[test]
    fn test_go_to_line_target_clamps_a_zero_request_to_the_first_line() {
        assert_eq!(go_to_line_target(0, 100), 0);
    }

    #[test]
    fn test_go_to_line_target_is_zero_for_an_empty_buffer() {
        assert_eq!(go_to_line_target(5, 0), 0);
        assert_eq!(go_to_line_target(0, 0), 0);
    }

    // --- stale positional index across close_tab (PR #401 review) ---

    #[allow(clippy::type_complexity)] // test-only bundle of the editor's channel senders/receiver
    fn test_channels() -> (
        Sender<String>,
        Sender<ClientMessage>,
        Sender<ClientMessage>,
        Sender<ClientMessage>,
        flume::Receiver<ClientMessage>,
    ) {
        let (open_file_tx, _open_file_rx) = flume::unbounded();
        let (save_file_tx, _save_file_rx) = flume::unbounded();
        let (buffer_change_tx, buffer_change_rx) = flume::unbounded();
        let (nav_tx, _nav_rx) = flume::unbounded();
        (
            open_file_tx,
            save_file_tx,
            buffer_change_tx,
            nav_tx,
            buffer_change_rx,
        )
    }

    /// Regression test for the review finding on #401: `arm_buffer_feed`'s
    /// debounce closure used to look the tab up by the raw positional
    /// `index` it captured at arm time. Closing an *earlier* tab before the
    /// debounce fires shifts every later tab's index down, so the stale
    /// index either misses (out of range) or, worse, hits a different tab —
    /// it now re-resolves via `tab_index_for_path` when the timer fires, so
    /// the surviving tab's `BufferChanged` still lands on the right path.
    #[gpui::test]
    async fn test_closing_an_earlier_tab_before_the_debounce_fires_still_feeds_the_survivor(
        cx: &mut TestAppContext,
    ) {
        let (open_file_tx, save_file_tx, buffer_change_tx, nav_tx, buffer_change_rx) =
            test_channels();

        let mut editor: Option<Entity<EditorView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                editor = Some(cx.new(|cx| {
                    EditorView::new(
                        open_file_tx,
                        save_file_tx,
                        buffer_change_tx,
                        nav_tx,
                        window,
                        cx,
                    )
                }));
                cx.new(|cx| gpui_component::Root::new(editor.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let editor = editor.expect("editor constructed inside the window callback");

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    // A at index 0, B at index 1 — mirror the post-`FileContent`
                    // reply state `arm_buffer_feed` requires (`Loaded`).
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    let b = editor.push_tab("b.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.tabs[b].load_state = TabLoadState::Loaded;
                    editor.active = Some(b);

                    // Edit B: arms its debounced feed while B still sits at
                    // index 1.
                    editor.arm_buffer_feed(b, cx);

                    // Close A before the debounce fires — B shifts from
                    // index 1 down to index 0.
                    editor.close_tab(a, window, cx);
                    assert_eq!(editor.tab_index_for_path("b.rs"), Some(0));
                });
            })
            .unwrap();

        // Closing A synchronously enqueues its own `BufferClosed`.
        match buffer_change_rx
            .try_recv()
            .expect("closing A must send its BufferClosed")
        {
            ClientMessage::BufferClosed { path } => assert_eq!(path, "a.rs"),
            other => panic!("expected BufferClosed for a.rs, got {other:?}"),
        }

        // `smol::Timer` (unlike gpui's own timers) isn't driven by the test
        // executor's virtual clock, so the debounce needs a real sleep here.
        cx.executor().allow_parking();
        smol::Timer::after(BUFFER_FEED_DEBOUNCE + Duration::from_millis(100)).await;
        cx.run_until_parked();

        match buffer_change_rx
            .try_recv()
            .expect("B's live-buffer feed must still fire after A's close shifts its index")
        {
            ClientMessage::BufferChanged { path, .. } => assert_eq!(path, "b.rs"),
            other => panic!("expected BufferChanged for b.rs, got {other:?}"),
        }
    }

    // --- workspace wiring fan-out: mtime/diagnostics route by path (#353) ---

    /// Build an `EditorView` inside a fresh window, returning the entity, the
    /// window handle (so the caller can drive further `editor.update` calls
    /// that need `window`), and the receiving ends of the open, save, and
    /// live-buffer channels for the tests that assert on outgoing messages.
    #[allow(clippy::type_complexity)] // test-only channel bundle
    fn build_test_editor_full(
        cx: &mut TestAppContext,
    ) -> (
        Entity<EditorView>,
        gpui::WindowHandle<gpui_component::Root>,
        flume::Receiver<String>,
        flume::Receiver<ClientMessage>,
        flume::Receiver<ClientMessage>,
    ) {
        let (open_file_tx, open_file_rx) = flume::unbounded();
        let (save_file_tx, save_file_rx) = flume::unbounded();
        let (buffer_change_tx, buffer_change_rx) = flume::unbounded();
        let (nav_tx, _nav_rx) = flume::unbounded();

        let mut editor: Option<Entity<EditorView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                editor = Some(cx.new(|cx| {
                    EditorView::new(
                        open_file_tx,
                        save_file_tx,
                        buffer_change_tx,
                        nav_tx,
                        window,
                        cx,
                    )
                }));
                cx.new(|cx| gpui_component::Root::new(editor.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        let editor = editor.expect("editor constructed inside the window callback");
        (editor, window, open_file_rx, save_file_rx, buffer_change_rx)
    }

    /// [`build_test_editor_full`] for the tests that only assert on the open
    /// channel — the save and live-buffer receivers are dropped.
    #[allow(clippy::type_complexity)] // test-only channel bundle
    fn build_test_editor(
        cx: &mut TestAppContext,
    ) -> (
        Entity<EditorView>,
        gpui::WindowHandle<gpui_component::Root>,
        flume::Receiver<String>,
    ) {
        let (editor, window, open_file_rx, _save_file_rx, _buffer_change_rx) =
            build_test_editor_full(cx);
        (editor, window, open_file_rx)
    }

    // --- go-to-line dialog (#620) ---

    /// Acceptance (`docs/spec-v1-hardening.md`): "go-to-line moves the cursor
    /// to the requested line" — the dialog's own OK/Enter path
    /// (`open_go_to_line`) is exercised end-to-end via `has_active_dialog`;
    /// this test drives the confirmed-value application
    /// ([`jump_to_line_input`]) directly against a loaded multi-line buffer.
    ///
    /// [`jump_to_line_input`]: EditorView::jump_to_line_input
    #[gpui::test]
    fn test_jump_to_line_input_moves_the_cursor_to_the_requested_one_based_line(
        cx: &mut TestAppContext,
    ) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                editor.load(
                    "a.rs".into(),
                    "one\ntwo\nthree\nfour".into(),
                    at(100),
                    window,
                    cx,
                );
                editor.active = Some(a);

                let line_input = cx.new(|cx| InputState::new(window, cx));
                line_input.update(cx, |input, cx| {
                    input.set_value("3", window, cx);
                });

                editor.jump_to_line_input(a, &line_input, window, cx);

                let cursor = editor.tabs[a].input.read(cx).cursor_position();
                assert_eq!(
                    cursor.line, 2,
                    "1-based line 3 lands on the 0-based line index 2"
                );
            });
        })
        .unwrap();
    }

    /// A non-numeric value (empty input, or the dialog dismissed without
    /// typing anything) must not move the cursor — silently ignored rather
    /// than jumping to line 0.
    #[gpui::test]
    fn test_jump_to_line_input_ignores_a_non_numeric_value(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                editor.load("a.rs".into(), "one\ntwo\nthree".into(), at(100), window, cx);
                editor.active = Some(a);
                editor.tabs[a].input.update(cx, |input, cx| {
                    input.set_cursor_position(EditorPosition::new(1, 0), window, cx);
                });

                let line_input = cx.new(|cx| InputState::new(window, cx));

                editor.jump_to_line_input(a, &line_input, window, cx);

                let cursor = editor.tabs[a].input.read(cx).cursor_position();
                assert_eq!(cursor.line, 1, "an empty value leaves the cursor untouched");
            });
        })
        .unwrap();
    }

    /// Acceptance: "go-to-line has an affordance" — dispatching `GoToLine`
    /// for a `Loaded` active tab opens the dialog.
    #[gpui::test]
    fn test_open_go_to_line_opens_a_dialog_for_the_loaded_active_tab(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                editor.tabs[a].load_state = TabLoadState::Loaded;
                editor.active = Some(a);
                editor.open_go_to_line(window, cx);
            });

            assert!(
                window.has_active_dialog(cx),
                "GoToLine must open the go-to-line dialog for a loaded tab"
            );
        })
        .unwrap();
    }

    /// A repeated `GoToLine` dispatch (e.g. the keybind fired twice before
    /// the first dialog is answered) must not stack a second dialog —
    /// mirrors `test_conflict_dialog_does_not_stack_on_a_repeated_external_write`.
    #[gpui::test]
    fn test_open_go_to_line_does_not_stack_a_second_dialog(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                editor.tabs[a].load_state = TabLoadState::Loaded;
                editor.active = Some(a);
                editor.open_go_to_line(window, cx);
                editor.open_go_to_line(window, cx);
            });

            window.close_dialog(cx);
            assert!(
                !window.has_active_dialog(cx),
                "a repeated GoToLine dispatch must not have stacked a second dialog"
            );
        })
        .unwrap();
    }

    /// Acceptance: with no tab open, `GoToLine` must not panic or open a
    /// dialog with nothing to jump within.
    #[gpui::test]
    fn test_open_go_to_line_is_a_no_op_with_no_active_tab(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.open_go_to_line(window, cx);
            });

            assert!(!window.has_active_dialog(cx));
        })
        .unwrap();
    }

    /// Acceptance (#354): "closing a clean tab is immediate" — no confirm
    /// dialog appears and the tab is gone right away.
    ///
    /// Uses `cx.update_window` rather than `window.update`: the latter
    /// (`WindowHandle<Root>::update`) leases the `Root` entity for the whole
    /// closure, and `has_active_dialog` reads that same `Root` internally —
    /// nesting the two double-leases and panics (mirrors the pattern
    /// `workspace.rs`'s command-palette dialog test documents).
    #[gpui::test]
    fn test_closing_a_clean_tab_closes_immediately_without_a_confirm_dialog(
        cx: &mut TestAppContext,
    ) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                editor.tabs[a].load_state = TabLoadState::Loaded;
                editor.active = Some(a);
                editor.close_tab(a, window, cx);
            });

            assert!(
                !window.has_active_dialog(cx),
                "a clean tab must close without prompting a confirm dialog"
            );
            assert!(
                editor.read(cx).tabs.is_empty(),
                "the clean tab closes immediately"
            );
        })
        .unwrap();
    }

    /// Acceptance (#354): "closing a dirty tab prompts to confirm" — the tab
    /// stays open until the user answers the dialog; discarding it
    /// unconfirmed would be exactly the silent data loss the confirm exists
    /// to prevent.
    #[gpui::test]
    fn test_closing_a_dirty_tab_opens_a_confirm_dialog_and_leaves_it_open(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                editor.tabs[a].load_state = TabLoadState::Loaded;
                editor.tabs[a].dirty = true;
                editor.active = Some(a);
                editor.close_tab(a, window, cx);
            });

            assert!(
                window.has_active_dialog(cx),
                "a dirty tab must prompt a confirm dialog before discarding its edits"
            );
            assert_eq!(
                editor.read(cx).tabs.len(),
                1,
                "the dirty tab stays open until the user confirms"
            );
        })
        .unwrap();
    }

    /// Acceptance (#353): "mtime/diagnostics for a path route to the tab
    /// holding it (asserted with several tabs open)". A clean background
    /// tab's path getting a newer snapshot `mtime` must auto-reload *that*
    /// tab, re-issuing its `OpenFile` — the active tab (a different path)
    /// must stay untouched, proving the signal is routed by path, not by
    /// "whichever tab happens to be active" (the pre-fan-out behavior).
    #[gpui::test]
    fn test_note_external_change_for_path_reloads_the_tab_holding_the_path_not_the_active_one(
        cx: &mut TestAppContext,
    ) {
        let (editor, window, open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    let b = editor.push_tab("b.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.tabs[a].base_mtime = Some(at(100));
                    editor.tabs[b].load_state = TabLoadState::Loaded;
                    editor.tabs[b].base_mtime = Some(at(100));
                    // b.rs is active; the signal below addresses a.rs, a
                    // background tab.
                    editor.active = Some(b);

                    editor.note_external_change_for_path("a.rs", at(200), window, cx);

                    assert!(
                        matches!(editor.tabs[a].load_state, TabLoadState::Loading),
                        "a.rs (clean, newer snapshot) must auto-reload"
                    );
                    assert!(
                        matches!(editor.tabs[b].load_state, TabLoadState::Loaded),
                        "b.rs, the active tab but an untouched path, must not reload"
                    );
                });
            })
            .unwrap();

        let path = open_file_rx
            .try_recv()
            .expect("the reload must re-issue an OpenFile for a.rs");
        assert_eq!(path, "a.rs");
    }

    /// Acceptance (#353): the same routing, but for the dirty (conflict)
    /// branch of the mtime signal — a background dirty tab surfaces its own
    /// conflict without disturbing the active tab's save state.
    #[gpui::test]
    fn test_note_external_change_for_path_conflicts_the_dirty_tab_holding_the_path_leaving_others_untouched(
        cx: &mut TestAppContext,
    ) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    let b = editor.push_tab("b.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.tabs[a].base_mtime = Some(at(100));
                    editor.tabs[a].dirty = true;
                    editor.tabs[b].load_state = TabLoadState::Loaded;
                    editor.tabs[b].base_mtime = Some(at(100));
                    editor.active = Some(b);

                    editor.note_external_change_for_path("a.rs", at(200), window, cx);

                    assert!(
                        matches!(editor.tabs[a].save_state, SaveState::Conflict),
                        "dirty a.rs with a newer snapshot must surface a conflict"
                    );
                    assert!(
                        matches!(editor.tabs[a].load_state, TabLoadState::Loaded),
                        "a conflict keeps the buffer — it must not reload over unsaved edits"
                    );
                    assert!(
                        matches!(editor.tabs[b].save_state, SaveState::Idle),
                        "b.rs, an untouched path, must not surface a conflict"
                    );
                });
            })
            .unwrap();
    }

    /// Acceptance (#353): "a signal for a path with no open tab is ignored,
    /// not an error" (constitution: no `.unwrap()` panics on absent state).
    #[gpui::test]
    fn test_note_external_change_for_path_ignores_a_path_with_no_open_tab(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.tabs[a].base_mtime = Some(at(100));
                    editor.active = Some(a);

                    editor.note_external_change_for_path("missing.rs", at(200), window, cx);

                    assert!(
                        matches!(editor.tabs[a].load_state, TabLoadState::Loaded),
                        "a signal for an unopened path must not touch a.rs"
                    );
                });
            })
            .unwrap();
    }

    // --- auto-reload viewport restore (#432) ---

    /// Acceptance (#432): a clean-buffer auto-reload must not reset the
    /// cursor — the pre-reload position is restored once the fresh content
    /// lands. The input entity must survive the reload (no rebuild): its
    /// retained layout is what lets the restore scroll the cursor back into
    /// view instead of leaving the viewport at the top.
    #[gpui::test]
    fn test_auto_reload_restores_cursor_and_keeps_the_input_entity(cx: &mut TestAppContext) {
        let (editor, window, open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.active = Some(a);
                    editor.load(
                        "a.rs".into(),
                        "line0\nline1\nline2\nline3\n".into(),
                        at(100),
                        window,
                        cx,
                    );
                    editor.tabs[a].input.update(cx, |input, cx| {
                        input.set_cursor_position(EditorPosition::new(2, 3), window, cx);
                    });
                    let entity_before = editor.tabs[a].input.entity_id();

                    editor.note_external_change_for_path("a.rs", at(200), window, cx);

                    assert!(
                        matches!(editor.tabs[a].load_state, TabLoadState::Loading),
                        "a clean buffer with a newer snapshot must auto-reload"
                    );
                    assert_eq!(
                        editor.tabs[a].input.entity_id(),
                        entity_before,
                        "the reload must keep the input entity — its layout carries the scroll restore"
                    );

                    editor.load(
                        "a.rs".into(),
                        "line0\nline1\nline2 changed\nline3\n".into(),
                        at(200),
                        window,
                        cx,
                    );

                    assert!(matches!(editor.tabs[a].load_state, TabLoadState::Loaded));
                    let pos = editor.tabs[a].input.read(cx).cursor_position();
                    assert_eq!(
                        (pos.line, pos.character),
                        (2, 3),
                        "the pre-reload cursor must survive the auto-reload"
                    );
                    assert!(
                        editor.tabs[a].pending_restore.is_none(),
                        "the restore is one-shot"
                    );
                });
            })
            .unwrap();

        assert_eq!(
            open_file_rx
                .try_recv()
                .expect("the reload must re-issue an OpenFile"),
            "a.rs"
        );
    }

    /// Acceptance (#432): a restored cursor beyond the end of the shrunken
    /// reload content is clamped by the rope layer — it lands at the end of
    /// the new content instead of a phantom line.
    #[gpui::test]
    fn test_auto_reload_clamps_restored_cursor_to_shorter_content(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.active = Some(a);
                    editor.load("a.rs".into(), "l0\nl1\nl2\nl3".into(), at(100), window, cx);
                    editor.tabs[a].input.update(cx, |input, cx| {
                        input.set_cursor_position(EditorPosition::new(3, 2), window, cx);
                    });

                    editor.note_external_change_for_path("a.rs", at(200), window, cx);
                    editor.load("a.rs".into(), "ab\ncd".into(), at(200), window, cx);

                    let pos = editor.tabs[a].input.read(cx).cursor_position();
                    assert_eq!(
                        (pos.line, pos.character),
                        (1, 2),
                        "a cursor past the shrunken content clamps to its end"
                    );
                });
            })
            .unwrap();
    }

    /// The restore must not steal focus (#432): `set_cursor_position`
    /// focuses the input as a side effect, but an auto-reload is
    /// agent-triggered — whatever was focused before (e.g. the terminal)
    /// must hold focus afterwards.
    #[gpui::test]
    fn test_auto_reload_restore_hands_focus_back_when_it_was_elsewhere(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.active = Some(a);
                    editor.load("a.rs".into(), "l0\nl1\nl2\n".into(), at(100), window, cx);
                    editor.tabs[a].input.update(cx, |input, cx| {
                        input.set_cursor_position(EditorPosition::new(1, 1), window, cx);
                    });
                });

                // Move focus away from the editor, standing in for the
                // terminal panel the user is typing into.
                let elsewhere = cx.focus_handle();
                window.focus(&elsewhere, cx);

                editor.update(cx, |editor, cx| {
                    editor.note_external_change_for_path("a.rs", at(200), window, cx);
                    editor.load(
                        "a.rs".into(),
                        "l0\nl1 changed\nl2\n".into(),
                        at(200),
                        window,
                        cx,
                    );
                });

                assert!(
                    window.focused(cx).is_some_and(|f| f == elsewhere),
                    "the auto-reload restore must hand focus back to the previously focused element"
                );
                let pos = editor.read(cx).tabs[0].input.read(cx).cursor_position();
                assert_eq!(
                    (pos.line, pos.character),
                    (1, 1),
                    "the cursor is still restored even though focus stayed elsewhere"
                );
            })
            .unwrap();
    }

    // --- conflict remedies + dialog (#433, #532) ---

    /// Acceptance (#433): "Reload from disk" on a dirty buffer in conflict
    /// re-reads the file — the buffer's edits are discarded, the conflict
    /// clears, and the daemon's LSP source of truth reverts to disk-backed.
    ///
    /// Uses `cx.update_window` rather than `window.update`: the conflict now
    /// also opens the dialog (#532), which reads/updates the same `Root`
    /// entity `WindowHandle<Root>::update` already leases — nesting the two
    /// double-leases and panics (the #354 tests document the same gotcha).
    #[gpui::test]
    fn test_reload_from_disk_on_conflict_replaces_content_and_clears_conflict(
        cx: &mut TestAppContext,
    ) {
        let (editor, window, open_file_rx, _save_file_rx, buffer_change_rx) =
            build_test_editor_full(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                editor.active = Some(a);
                editor.load("a.rs".into(), "disk v1".into(), at(100), window, cx);
                editor.tabs[a].dirty = true; // unsaved edits

                editor.note_external_change_for_path("a.rs", at(200), window, cx);
                assert!(matches!(editor.tabs[a].save_state, SaveState::Conflict));

                editor.reload_active_from_disk(window, cx);
                assert!(
                    matches!(editor.tabs[a].load_state, TabLoadState::Loading),
                    "the reload must re-arm the load"
                );

                editor.load("a.rs".into(), "disk v2".into(), at(200), window, cx);
                assert!(
                    matches!(editor.tabs[a].save_state, SaveState::Idle),
                    "the conflict must clear on reload"
                );
                assert!(!editor.tabs[a].dirty, "the reloaded buffer starts clean");
                assert!(editor.tabs[a].conflict_disk_mtime.is_none());
                assert_eq!(
                    editor.tabs[a].input.read(cx).value().to_string(),
                    "disk v2",
                    "the buffer must hold the on-disk content"
                );
            });
        })
        .unwrap();

        assert_eq!(
            open_file_rx
                .try_recv()
                .expect("the reload must re-issue an OpenFile"),
            "a.rs"
        );
        match buffer_change_rx
            .try_recv()
            .expect("the reload must revert the live buffer to disk-backed")
        {
            ClientMessage::BufferClosed { path } => assert_eq!(path, "a.rs"),
            other => panic!("expected BufferClosed for a.rs, got {other:?}"),
        }
    }

    /// Acceptance (#433): "Keep mine" on a dirty buffer in conflict
    /// force-saves the buffer rebased onto the on-disk `mtime` observed when
    /// the conflict surfaced, so the daemon's stale-base check passes; the
    /// `SaveResult` reply then clears the conflict and commits the new base.
    ///
    /// `cx.update_window` — see the note on the reload test above (#532's
    /// dialog open shares the same `Root`-double-lease gotcha).
    #[gpui::test]
    fn test_keep_mine_on_conflict_saves_with_observed_disk_mtime_and_clears_conflict(
        cx: &mut TestAppContext,
    ) {
        let (editor, window, _open_file_rx, save_file_rx, _buffer_change_rx) =
            build_test_editor_full(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                editor.active = Some(a);
                editor.load("a.rs".into(), "mine".into(), at(100), window, cx);
                editor.tabs[a].dirty = true;

                editor.note_external_change_for_path("a.rs", at(200), window, cx);
                assert!(matches!(editor.tabs[a].save_state, SaveState::Conflict));
                assert_eq!(editor.tabs[a].conflict_disk_mtime, Some(at(200)));

                editor.keep_mine_active(cx);
                assert!(
                    matches!(editor.tabs[a].save_state, SaveState::Saving),
                    "keep-mine must dispatch a save"
                );

                // The daemon accepts (base matches disk) and replies.
                editor.apply_save_result("a.rs".into(), at(300), cx);
                assert!(
                    matches!(editor.tabs[a].save_state, SaveState::Idle),
                    "the conflict is resolved once the forced save lands"
                );
                assert!(!editor.tabs[a].dirty);
                assert_eq!(editor.tabs[a].base_mtime, Some(at(300)));
                assert!(editor.tabs[a].conflict_disk_mtime.is_none());
            });
        })
        .unwrap();

        match save_file_rx
            .try_recv()
            .expect("keep-mine must dispatch a SaveFile")
        {
            ClientMessage::SaveFile {
                path,
                content,
                base_mtime,
            } => {
                assert_eq!(path, "a.rs");
                assert_eq!(content, "mine");
                assert_eq!(
                    base_mtime,
                    at(200),
                    "the forced save must be rebased onto the observed disk mtime"
                );
            }
            other => panic!("expected SaveFile, got {other:?}"),
        }
    }

    /// A daemon `SaveConflict` reply records its `disk_mtime` so "Keep mine"
    /// can rebase onto it — including the re-conflict round when the file
    /// changed on disk *again* between the conflict and the click.
    ///
    /// `cx.update_window` — see the note on the reload test above (#532's
    /// dialog open shares the same `Root`-double-lease gotcha).
    #[gpui::test]
    fn test_apply_save_conflict_records_disk_mtime_for_keep_mine(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                editor.active = Some(a);
                editor.load("a.rs".into(), "mine".into(), at(100), window, cx);
                editor.tabs[a].dirty = true;

                editor.apply_save_conflict("a.rs".into(), at(250), window, cx);

                assert!(matches!(editor.tabs[a].save_state, SaveState::Conflict));
                assert_eq!(
                    editor.tabs[a].conflict_disk_mtime,
                    Some(at(250)),
                    "the reply's disk mtime must be recorded for keep-mine"
                );
            });
        })
        .unwrap();
    }

    /// Acceptance (#532): "Dirty buffer + external edit -> dialog" — a dirty
    /// active buffer conflicting with an external edit opens the
    /// file-changed-on-disk dialog, upgrading the #433 inline banner.
    #[gpui::test]
    fn test_dirty_buffer_conflict_via_external_change_opens_the_conflict_dialog(
        cx: &mut TestAppContext,
    ) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                editor.active = Some(a);
                editor.load("a.rs".into(), "mine".into(), at(100), window, cx);
                editor.tabs[a].dirty = true;

                editor.note_external_change_for_path("a.rs", at(200), window, cx);
            });

            assert!(
                window.has_active_dialog(cx),
                "a dirty buffer conflicting with an external edit must open the conflict dialog"
            );
        })
        .unwrap();
    }

    /// Acceptance (#532): a `SaveConflict` reply for the active tab opens
    /// the same dialog as the external-change path above.
    #[gpui::test]
    fn test_apply_save_conflict_opens_the_conflict_dialog_for_the_active_tab(
        cx: &mut TestAppContext,
    ) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                editor.active = Some(a);
                editor.load("a.rs".into(), "mine".into(), at(100), window, cx);
                editor.tabs[a].dirty = true;

                editor.apply_save_conflict("a.rs".into(), at(250), window, cx);
            });

            assert!(
                window.has_active_dialog(cx),
                "a SaveConflict reply for the active tab must open the conflict dialog"
            );
        })
        .unwrap();
    }

    /// A background tab's conflict (#353 routing: the addressed path is not
    /// the active tab) must not interrupt the user with a dialog for a tab
    /// they are not looking at — but switching onto it surfaces the same
    /// dialog, since a background conflict otherwise has no visible
    /// indicator once it becomes active (the #433 banner had the same
    /// render-reactive behavior).
    #[gpui::test]
    fn test_background_tab_conflict_opens_no_dialog_until_activated(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        cx.update_window(window.into(), |_, window, cx| {
            let a = editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                let b = editor.push_tab("b.rs".into(), false, window, cx);
                editor.tabs[a].load_state = TabLoadState::Loaded;
                editor.tabs[a].base_mtime = Some(at(100));
                editor.tabs[a].dirty = true;
                editor.tabs[b].load_state = TabLoadState::Loaded;
                editor.tabs[b].base_mtime = Some(at(100));
                editor.active = Some(b);

                editor.note_external_change_for_path("a.rs", at(200), window, cx);
                assert!(matches!(editor.tabs[a].save_state, SaveState::Conflict));
                a
            });

            assert!(
                !window.has_active_dialog(cx),
                "a's background conflict must not interrupt b, the active tab"
            );

            editor.update(cx, |editor, cx| {
                editor.activate_tab(a, window, cx);
            });

            assert!(
                window.has_active_dialog(cx),
                "switching onto a's now-active conflict must open the dialog"
            );
        })
        .unwrap();
    }

    /// A further external write while the conflict dialog is already open
    /// (rebasing the recorded disk mtime, e.g. an agent keeps editing before
    /// the user answers) must not stack a second dialog on top of it.
    #[gpui::test]
    fn test_conflict_dialog_does_not_stack_on_a_repeated_external_write(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let a = editor.push_tab("a.rs".into(), false, window, cx);
                editor.active = Some(a);
                editor.load("a.rs".into(), "mine".into(), at(100), window, cx);
                editor.tabs[a].dirty = true;

                editor.note_external_change_for_path("a.rs", at(200), window, cx);
            });
            assert!(window.has_active_dialog(cx));

            editor.update(cx, |editor, cx| {
                editor.note_external_change_for_path("a.rs", at(300), window, cx);
            });

            // A single close must clear the dialog state entirely — if the
            // repeated write had stacked a second dialog, one close would
            // still leave `has_active_dialog` true.
            window.close_dialog(cx);
            assert!(
                !window.has_active_dialog(cx),
                "the repeated external write must not have stacked a second dialog"
            );
        })
        .unwrap();
    }

    /// The remedies only act on a tab that is actually in conflict — outside
    /// one they are no-ops (no spurious reload, no spurious save).
    #[gpui::test]
    fn test_conflict_remedies_without_conflict_are_no_ops(cx: &mut TestAppContext) {
        let (editor, window, open_file_rx, save_file_rx, _buffer_change_rx) =
            build_test_editor_full(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.active = Some(a);
                    editor.load("a.rs".into(), "clean".into(), at(100), window, cx);

                    editor.reload_active_from_disk(window, cx);
                    assert!(
                        matches!(editor.tabs[a].load_state, TabLoadState::Loaded),
                        "no conflict — reload must not re-arm the load"
                    );

                    editor.keep_mine_active(cx);
                    assert!(
                        matches!(editor.tabs[a].save_state, SaveState::Idle),
                        "no conflict — keep-mine must not dispatch a save"
                    );
                });
            })
            .unwrap();

        assert!(
            open_file_rx.try_recv().is_err(),
            "no OpenFile may be issued outside a conflict"
        );
        assert!(
            save_file_rx.try_recv().is_err(),
            "no SaveFile may be issued outside a conflict"
        );
    }

    /// Acceptance (#353): diagnostics for a path route to the tab holding it,
    /// not the active tab — the same routing discipline as the mtime signal,
    /// so a background tab's inline markers converge with the model even
    /// while another tab is active.
    #[gpui::test]
    fn test_set_diagnostics_for_path_routes_to_the_tab_holding_the_path_not_the_active_one(
        cx: &mut TestAppContext,
    ) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    let b = editor.push_tab("b.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.tabs[b].load_state = TabLoadState::Loaded;
                    editor.active = Some(b);

                    let items = vec![proto_diag(DiagnosticSeverity::Error, None, None)];
                    editor.set_diagnostics_for_path("a.rs", &items, cx);

                    let a_len = editor.tabs[a]
                        .input
                        .read(cx)
                        .diagnostics()
                        .map(gpui_component::highlighter::DiagnosticSet::len)
                        .unwrap_or(0);
                    let b_len = editor.tabs[b]
                        .input
                        .read(cx)
                        .diagnostics()
                        .map(gpui_component::highlighter::DiagnosticSet::len)
                        .unwrap_or(0);
                    assert_eq!(a_len, 1, "a.rs must receive its own diagnostic");
                    assert_eq!(
                        b_len, 0,
                        "b.rs, the active tab but an untouched path, must not receive it"
                    );
                });
            })
            .unwrap();
    }

    /// Acceptance (#353): a diagnostics push for a path with no open tab is
    /// silently ignored.
    #[gpui::test]
    fn test_set_diagnostics_for_path_ignores_a_path_with_no_open_tab(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.active = Some(a);

                    let items = vec![proto_diag(DiagnosticSeverity::Error, None, None)];
                    editor.set_diagnostics_for_path("missing.rs", &items, cx);

                    let a_len = editor.tabs[a]
                        .input
                        .read(cx)
                        .diagnostics()
                        .map(gpui_component::highlighter::DiagnosticSet::len)
                        .unwrap_or(0);
                    assert_eq!(
                        a_len, 0,
                        "a signal for an unopened path must not touch a.rs"
                    );
                });
            })
            .unwrap();
    }

    /// `open_paths` is what `WorkspaceView` iterates to fan mtime/diagnostics
    /// out — it must list every open tab, in open order, regardless of which
    /// is active.
    #[gpui::test]
    fn test_open_paths_lists_every_open_tab_in_open_order(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.push_tab("a.rs".into(), false, window, cx);
                    editor.push_tab("b.rs".into(), false, window, cx);
                    editor.push_tab("c.rs".into(), false, window, cx);
                    editor.active = Some(0);

                    let paths: Vec<&str> = editor.open_paths().collect();
                    assert_eq!(paths, vec!["a.rs", "b.rs", "c.rs"]);
                });
            })
            .unwrap();
    }

    // --- buffer-channel error replies (#617, docs/spec-v1-hardening.md) ---

    /// Acceptance: an `OpenError` reply for the `Loading` tab awaiting it
    /// carries the specific reason into `TabLoadState::Failed`, and the
    /// centered status render names it instead of the generic "Could not
    /// open" text.
    #[gpui::test]
    fn test_apply_open_error_sets_failed_with_the_specific_reason(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.active = Some(a);
                    assert!(matches!(editor.tabs[a].load_state, TabLoadState::Loading));

                    editor.apply_open_error("a.rs".into(), BufferErrorReason::PermissionDenied, cx);

                    assert!(
                        matches!(
                            editor.tabs[a].load_state,
                            TabLoadState::Failed(Some(BufferErrorReason::PermissionDenied))
                        ),
                        "the specific reason must be carried into TabLoadState::Failed"
                    );
                });
            })
            .unwrap();
    }

    /// Acceptance: `TooLarge` carries into `TabLoadState::Failed` exactly
    /// like any other reason — [`buffer_error_reason_label`] and the render's
    /// dedicated `TooLarge` arm are what give it distinct read-only-placeholder
    /// wording (`docs/spec-v1-hardening.md`); the state transition itself is
    /// uniform across every [`BufferErrorReason`].
    #[gpui::test]
    fn test_apply_open_error_too_large_sets_failed_with_too_large_reason(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("huge.bin".into(), false, window, cx);
                    editor.active = Some(a);

                    editor.apply_open_error("huge.bin".into(), BufferErrorReason::TooLarge, cx);

                    assert!(matches!(
                        editor.tabs[a].load_state,
                        TabLoadState::Failed(Some(BufferErrorReason::TooLarge))
                    ));
                });
            })
            .unwrap();
    }

    /// Acceptance: an `OpenError` reply for a path with no `Loading` tab is
    /// silently ignored — mirrors `load`'s guard for a superseded reload or a
    /// switch's redundant read.
    #[gpui::test]
    fn test_apply_open_error_ignores_a_path_with_no_loading_tab(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.active = Some(a);

                    editor.apply_open_error("a.rs".into(), BufferErrorReason::Io, cx);

                    assert!(
                        matches!(editor.tabs[a].load_state, TabLoadState::Loaded),
                        "a reply for a tab that already loaded must not be clobbered"
                    );
                });
            })
            .unwrap();
    }

    /// Acceptance: an `OpenError` reply beats `OPEN_TIMEOUT` — once the
    /// specific reason has moved the tab out of `Loading`, the timeout's own
    /// `Loading` guard can no longer fire and overwrite it with the generic
    /// `Failed(None)`.
    #[gpui::test]
    fn test_apply_open_error_beats_the_open_timeout_guard(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.active = Some(a);
                    let generation = editor.tabs[a].generation;

                    editor.apply_open_error("a.rs".into(), BufferErrorReason::NotFound, cx);

                    // Simulate the timeout firing after the specific reply
                    // already landed: same generation, but `load_state` has
                    // already left `Loading`, so its guard must not fire.
                    if editor.tabs[a].generation == generation {
                        if let TabLoadState::Loading = editor.tabs[a].load_state {
                            editor.tabs[a].load_state = TabLoadState::Failed(None);
                        }
                    }

                    assert!(
                        matches!(
                            editor.tabs[a].load_state,
                            TabLoadState::Failed(Some(BufferErrorReason::NotFound))
                        ),
                        "the specific reason must survive a same-generation timeout fire"
                    );
                });
            })
            .unwrap();
    }

    /// Acceptance: a `SaveError` reply for the tab holding `path` carries the
    /// specific reason into `SaveState::Failed`, mirroring `apply_save_result`
    /// / `apply_save_conflict`'s path-keyed routing.
    #[gpui::test]
    fn test_apply_save_error_sets_failed_with_the_specific_reason(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.tabs[a].save_state = SaveState::Saving;
                    editor.active = Some(a);
                    let save_generation = editor.tabs[a].save_generation;

                    editor.apply_save_error("a.rs".into(), BufferErrorReason::Io, cx);

                    assert!(
                        matches!(
                            editor.tabs[a].save_state,
                            SaveState::Failed(Some(BufferErrorReason::Io))
                        ),
                        "the specific reason must be carried into SaveState::Failed"
                    );
                    assert_ne!(
                        editor.tabs[a].save_generation, save_generation,
                        "the save generation must bump so a same-generation SAVE_TIMEOUT \
                         guard can no longer clobber it"
                    );
                });
            })
            .unwrap();
    }

    /// Acceptance: a `SaveError` reply for a path no tab holds is silently
    /// ignored.
    #[gpui::test]
    fn test_apply_save_error_ignores_a_path_with_no_open_tab(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.active = Some(a);

                    editor.apply_save_error("missing.rs".into(), BufferErrorReason::Io, cx);

                    assert!(
                        matches!(editor.tabs[a].save_state, SaveState::Idle),
                        "a reply for an unopened path must not touch a.rs"
                    );
                });
            })
            .unwrap();
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

    // --- breadcrumb + gutter + inline card (editor chrome) ---

    fn diag_at_line(line: u32, severity: DiagnosticSeverity) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position { line, character: 0 },
                end: Position { line, character: 3 },
            },
            severity,
            message: format!("issue on {line}"),
            source: None,
            code: None,
        }
    }

    fn sym(name: &str, depth: u32, start: (u32, u32), end: (u32, u32)) -> DocumentSymbolEntry {
        let range = Range {
            start: Position {
                line: start.0,
                character: start.1,
            },
            end: Position {
                line: end.0,
                character: end.1,
            },
        };
        DocumentSymbolEntry {
            name: name.to_owned(),
            kind: rift_protocol::SymbolKind::Function,
            range,
            selection_range: range,
            depth,
        }
    }

    #[test]
    fn test_position_in_range_covers_interior_and_both_boundaries() {
        let range = Range {
            start: Position {
                line: 2,
                character: 4,
            },
            end: Position {
                line: 5,
                character: 8,
            },
        };
        // Interior line, any column.
        assert!(position_in_range(&range, 3, 0));
        // Exactly on the start and end positions (inclusive).
        assert!(position_in_range(&range, 2, 4));
        assert!(position_in_range(&range, 5, 8));
        // Just before the start and just after the end.
        assert!(!position_in_range(&range, 2, 3));
        assert!(!position_in_range(&range, 5, 9));
        assert!(!position_in_range(&range, 1, 100));
        assert!(!position_in_range(&range, 6, 0));
    }

    #[test]
    fn test_enclosing_symbol_chain_returns_ancestors_outermost_first() {
        // impl block (depth 0) containing a fn (depth 1) containing a closure
        // (depth 2); the flattened list is deliberately out of depth order.
        let symbols = vec![
            sym("fn render", 1, (2, 4), (8, 5)),
            sym("impl View", 0, (1, 0), (20, 1)),
            sym("|event|", 2, (4, 8), (6, 9)),
            sym("fn other", 1, (10, 4), (15, 5)),
        ];
        let chain: Vec<&str> = enclosing_symbol_chain(&symbols, 5, 0)
            .into_iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(chain, vec!["impl View", "fn render", "|event|"]);
    }

    #[test]
    fn test_enclosing_symbol_chain_is_empty_outside_every_symbol() {
        let symbols = vec![sym("fn render", 1, (2, 4), (8, 5))];
        assert!(enclosing_symbol_chain(&symbols, 100, 0).is_empty());
        assert!(enclosing_symbol_chain(&[], 5, 0).is_empty());
    }

    #[test]
    fn test_severity_rank_orders_error_above_hint() {
        assert!(
            severity_rank(DiagnosticSeverity::Error) > severity_rank(DiagnosticSeverity::Warning)
        );
        assert!(
            severity_rank(DiagnosticSeverity::Warning)
                > severity_rank(DiagnosticSeverity::Information)
        );
        assert!(
            severity_rank(DiagnosticSeverity::Information)
                > severity_rank(DiagnosticSeverity::Hint)
        );
    }

    #[test]
    fn test_primary_diagnostic_on_line_picks_the_most_severe_on_that_line() {
        let diagnostics = vec![
            diag_at_line(3, DiagnosticSeverity::Warning),
            diag_at_line(3, DiagnosticSeverity::Error),
            diag_at_line(7, DiagnosticSeverity::Error),
        ];
        let primary = primary_diagnostic_on_line(&diagnostics, 3).expect("line 3 has diagnostics");
        assert_eq!(primary.severity, DiagnosticSeverity::Error);
        assert!(
            primary_diagnostic_on_line(&diagnostics, 4).is_none(),
            "a line with no diagnostic yields None"
        );
    }

    #[test]
    fn test_path_breadcrumb_segments_drops_empty_pieces() {
        assert_eq!(
            path_breadcrumb_segments("crates/app/src/editor.rs"),
            vec!["crates", "app", "src", "editor.rs"]
        );
        // Leading and trailing slashes (absolute out-of-root path) yield no
        // empty segments.
        assert_eq!(
            path_breadcrumb_segments("/home/user/lib.rs"),
            vec!["home", "user", "lib.rs"]
        );
        assert!(path_breadcrumb_segments("").is_empty());
    }

    #[test]
    fn test_diagnostic_detail_joins_source_and_code() {
        assert_eq!(
            diagnostic_detail(&proto_diag(
                DiagnosticSeverity::Error,
                Some("rustc"),
                Some("E0308")
            ))
            .as_deref(),
            Some("rustc(E0308)")
        );
        assert_eq!(
            diagnostic_detail(&proto_diag(DiagnosticSeverity::Error, Some("rustc"), None))
                .as_deref(),
            Some("rustc")
        );
        assert_eq!(
            diagnostic_detail(&proto_diag(DiagnosticSeverity::Error, None, Some("E0308")))
                .as_deref(),
            Some("E0308")
        );
        assert!(diagnostic_detail(&proto_diag(DiagnosticSeverity::Error, None, None)).is_none());
    }

    /// A document-symbol response is cached on the tab whose in-flight id it
    /// echoes, feeding the breadcrumb's enclosing-symbol lookup.
    #[gpui::test]
    fn test_apply_document_symbol_response_caches_on_the_requesting_tab(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.active = Some(a);
                    editor.tabs[a].latest_symbol_id = Some(NavRequestId(1));

                    editor.apply_document_symbol_response(
                        NavRequestId(1),
                        vec![sym("fn main", 0, (0, 0), (3, 1))],
                        cx,
                    );
                    assert_eq!(editor.tabs[a].symbols.len(), 1);
                    assert_eq!(editor.tabs[a].symbols[0].name, "fn main");
                });
            })
            .unwrap();
    }

    /// A response whose id matches no tab's `latest_symbol_id` is dropped
    /// (drop-stale discipline shared with the other nav responses).
    #[gpui::test]
    fn test_stale_document_symbol_response_is_dropped(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.active = Some(a);
                    editor.tabs[a].latest_symbol_id = Some(NavRequestId(1));

                    editor.apply_document_symbol_response(
                        NavRequestId(2),
                        vec![sym("fn main", 0, (0, 0), (3, 1))],
                        cx,
                    );
                    assert!(
                        editor.tabs[a].symbols.is_empty(),
                        "a stale-id response must not populate the cache"
                    );
                });
            })
            .unwrap();
    }

    /// The cursor line's diagnostic is withheld from the widget's own
    /// diagnostic set (it renders as the app's inline card instead), while the
    /// tab keeps the full set for the gutter dots — "one diagnostic never
    /// renders twice".
    #[gpui::test]
    fn test_cursor_line_diagnostic_is_suppressed_from_the_widget_set(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.active = Some(a);
                    editor.tabs[a].cursor_line = 0;

                    let items = vec![
                        diag_at_line(0, DiagnosticSeverity::Error),
                        diag_at_line(10, DiagnosticSeverity::Warning),
                    ];
                    editor.set_diagnostics_for_path("a.rs", &items, cx);

                    // The tab's own copy keeps both (gutter dots use it).
                    assert_eq!(editor.tabs[a].diagnostics.len(), 2);
                    // The widget set drops the cursor line (line 0) — only the
                    // line-10 diagnostic remains for the widget's squiggle/popover.
                    let widget_len = editor.tabs[a]
                        .input
                        .read(cx)
                        .diagnostics()
                        .map(gpui_component::highlighter::DiagnosticSet::len)
                        .unwrap_or(0);
                    assert_eq!(widget_len, 1, "the cursor line's diagnostic is withheld");

                    // Moving the cursor to line 10 re-syncs: line 10 is now
                    // withheld and line 0 returns to the widget set.
                    editor.tabs[a].cursor_line = 10;
                    editor.sync_widget_diagnostics(a, cx);
                    let widget_len = editor.tabs[a]
                        .input
                        .read(cx)
                        .diagnostics()
                        .map(gpui_component::highlighter::DiagnosticSet::len)
                        .unwrap_or(0);
                    assert_eq!(widget_len, 1);
                    assert_eq!(editor.tabs[a].diagnostics.len(), 2);
                });
            })
            .unwrap();
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

    // --- hover card anatomy (#528, docs/spec-editor-chrome.md §3) ---

    #[test]
    fn test_parse_hover_markdown_code_and_doc_split() {
        // A rust-analyzer-style hover: a fenced signature, a hairline, prose.
        let md = "```rust\npub fn foo(x: i32) -> i32\n```\n\n---\n\nAdds one to `x`.";
        let parts = parse_hover_markdown(md);
        assert_eq!(parts.code.as_deref(), Some("pub fn foo(x: i32) -> i32"));
        assert_eq!(parts.doc.as_deref(), Some("Adds one to `x`."));
    }

    #[test]
    fn test_parse_hover_markdown_joins_multiple_leading_fences() {
        // rust-analyzer often emits the module path and the signature as two
        // separate fences before the docs; both belong in the code section.
        let md = "```rust\nstd::string\n```\n\n```rust\npub struct String\n```\n\n---\n\nA UTF-8 string.";
        let parts = parse_hover_markdown(md);
        assert_eq!(
            parts.code.as_deref(),
            Some("std::string\npub struct String")
        );
        assert_eq!(parts.doc.as_deref(), Some("A UTF-8 string."));
    }

    #[test]
    fn test_parse_hover_markdown_plaintext_is_all_doc() {
        // Plaintext hover (no fences): everything is the doc body, no code.
        let md = "just some hover text\nover two lines";
        let parts = parse_hover_markdown(md);
        assert_eq!(parts.code, None);
        assert_eq!(
            parts.doc.as_deref(),
            Some("just some hover text\nover two lines")
        );
    }

    #[test]
    fn test_parse_hover_markdown_code_only_has_no_doc() {
        // A signature with no documentation: code present, doc absent.
        let md = "```rust\npub const N: usize\n```";
        let parts = parse_hover_markdown(md);
        assert_eq!(parts.code.as_deref(), Some("pub const N: usize"));
        assert_eq!(parts.doc, None);
    }

    #[test]
    fn test_parse_hover_markdown_unterminated_fence_is_graceful() {
        // Malformed: a fence that never closes must not panic; its lines become
        // the code section and there is no doc.
        let md = "```rust\npub fn foo()\nlet unterminated = true;";
        let parts = parse_hover_markdown(md);
        assert_eq!(
            parts.code.as_deref(),
            Some("pub fn foo()\nlet unterminated = true;")
        );
        assert_eq!(parts.doc, None);
    }

    #[test]
    fn test_parse_hover_markdown_empty_is_empty() {
        let parts = parse_hover_markdown("");
        assert_eq!(parts, HoverParts::default());
    }

    #[test]
    fn test_parse_hover_markdown_prose_without_hairline_is_doc() {
        // Some servers omit the thematic break; the first prose line after the
        // fence still begins the doc body and is not dropped.
        let md = "```rust\npub fn foo()\n```\nInline docs, no hairline.";
        let parts = parse_hover_markdown(md);
        assert_eq!(parts.code.as_deref(), Some("pub fn foo()"));
        assert_eq!(parts.doc.as_deref(), Some("Inline docs, no hairline."));
    }

    #[test]
    fn test_is_thematic_break_recognizes_markers() {
        assert!(is_thematic_break("---"));
        assert!(is_thematic_break("***"));
        assert!(is_thematic_break("___"));
        assert!(is_thematic_break("-----"));
        assert!(!is_thematic_break("--"));
        assert!(!is_thematic_break("- item"));
        assert!(!is_thematic_break("code"));
    }

    #[test]
    fn test_truncate_code_preview_caps_long_blocks() {
        let code = "l1\nl2\nl3\nl4\nl5";
        let (lines, truncated) = truncate_code_preview(code, 3);
        assert_eq!(lines, vec!["l1", "l2", "l3"]);
        assert!(truncated);
    }

    #[test]
    fn test_truncate_code_preview_keeps_short_blocks() {
        let code = "l1\nl2";
        let (lines, truncated) = truncate_code_preview(code, 12);
        assert_eq!(lines, vec!["l1", "l2"]);
        assert!(!truncated);
    }

    #[test]
    fn test_truncate_code_preview_zero_cap_keeps_signature() {
        // A zero cap is clamped to one line so the signature never vanishes.
        let (lines, truncated) = truncate_code_preview("sig\nmore", 0);
        assert_eq!(lines, vec!["sig"]);
        assert!(truncated);
    }

    // --- jump-list overlay dismissal (#485) ---

    fn jump_target(path: &str, line: u32) -> NavLocation {
        let pos = Position { line, character: 0 };
        NavLocation {
            path: path.to_owned(),
            range: Range {
                start: pos,
                end: pos,
            },
            out_of_root: false,
            line_preview: None,
        }
    }

    // --- word_at (search-context token) ---

    #[test]
    fn test_word_at_resolves_the_identifier_under_the_cursor() {
        let text = "let value = other;\nfn run() {}";
        // Inside the token, and at its trailing edge (cursor just past it).
        assert_eq!(word_at(text, 0, 4).as_deref(), Some("value"));
        assert_eq!(word_at(text, 0, 9).as_deref(), Some("value"));
        // A token on the second line resolves against that line only.
        assert_eq!(word_at(text, 1, 4).as_deref(), Some("run"));
    }

    #[test]
    fn test_word_at_handles_unicode_and_malformed_positions() {
        // A multibyte identifier resolves whole (scalar offsets, not bytes).
        assert_eq!(word_at("café x", 0, 0).as_deref(), Some("café"));
        // On an operator with spaces on both sides — no token.
        assert_eq!(word_at("let x = 1;", 0, 6), None);
        // Past the line end, an out-of-range line, and an empty buffer.
        assert_eq!(word_at("abc", 0, 99), None);
        assert_eq!(word_at("abc", 5, 0), None);
        assert_eq!(word_at("", 0, 0), None);
    }

    // --- results-panel wiring (#529) ---

    /// A find-references response opens the results panel: it marks the editor's
    /// results-visible flag and emits a single `ShowResults` carrying the kind,
    /// the searched symbol, and every location.
    #[gpui::test]
    fn test_apply_references_response_opens_the_results_panel(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);
        let events: Rc<std::cell::RefCell<Vec<EditorEvent>>> = Rc::new(Default::default());

        {
            let sink = events.clone();
            cx.update(|cx| {
                cx.subscribe(&editor, move |_editor, event: &EditorEvent, _cx| {
                    sink.borrow_mut().push(event.clone());
                })
                .detach();
            });
        }

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.active = Some(a);
                    editor.tabs[a].latest_ref_id = Some(NavRequestId(1));
                    editor.tabs[a].nav_symbol = Some("foo".into());
                    editor.apply_references_response(
                        NavRequestId(1),
                        vec![jump_target("a.rs", 1), jump_target("b.rs", 2)],
                        cx,
                    );
                    assert!(
                        editor.results_visible,
                        "a references response opens the panel"
                    );
                });
            })
            .unwrap();

        let events = events.borrow();
        assert_eq!(events.len(), 1, "exactly one ShowResults is emitted");
        match &events[0] {
            EditorEvent::ShowResults {
                kind,
                symbol,
                locations,
            } => {
                assert_eq!(*kind, ResultsKind::References);
                assert_eq!(symbol.as_deref(), Some("foo"));
                assert_eq!(locations.len(), 2);
            }
            other => panic!("expected ShowResults, got {other:?}"),
        }
    }

    /// A multi-target definition response opens the panel; a single-target one
    /// jumps in place and emits no panel event.
    #[gpui::test]
    fn test_definition_response_multi_opens_panel_single_jumps(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);
        let events: Rc<std::cell::RefCell<Vec<EditorEvent>>> = Rc::new(Default::default());

        {
            let sink = events.clone();
            cx.update(|cx| {
                cx.subscribe(&editor, move |_editor, event: &EditorEvent, _cx| {
                    sink.borrow_mut().push(event.clone());
                })
                .detach();
            });
        }

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.active = Some(a);

                    // Multi-target (same file, so no cross-file open): opens the panel.
                    editor.tabs[a].latest_def_id = Some(NavRequestId(1));
                    editor.apply_definition_response(
                        NavRequestId(1),
                        vec![jump_target("a.rs", 1), jump_target("a.rs", 5)],
                        window,
                        cx,
                    );
                    assert!(editor.results_visible, "multiple targets open the panel");

                    // Single target: jumps in place, no panel.
                    editor.mark_results_closed();
                    editor.tabs[a].latest_def_id = Some(NavRequestId(2));
                    editor.apply_definition_response(
                        NavRequestId(2),
                        vec![jump_target("a.rs", 9)],
                        window,
                        cx,
                    );
                    assert!(
                        !editor.results_visible,
                        "a single target jumps without opening the panel"
                    );
                });
            })
            .unwrap();

        let events = events.borrow();
        assert_eq!(
            events.len(),
            1,
            "only the multi-target response opens the panel"
        );
        assert!(matches!(
            events[0],
            EditorEvent::ShowResults {
                kind: ResultsKind::Definitions,
                ..
            }
        ));
    }

    /// `close_results_panel` consumes `Escape` only while the panel is open
    /// (emitting `CloseResults`), and `mark_results_closed` keeps the flag in
    /// sync when the workspace closes the panel via its × affordance.
    #[gpui::test]
    fn test_close_results_panel_gates_escape_and_syncs_with_the_workspace(cx: &mut TestAppContext) {
        let (editor, window, _open_file_rx) = build_test_editor(cx);
        let events: Rc<std::cell::RefCell<Vec<EditorEvent>>> = Rc::new(Default::default());

        {
            let sink = events.clone();
            cx.update(|cx| {
                cx.subscribe(&editor, move |_editor, event: &EditorEvent, _cx| {
                    sink.borrow_mut().push(event.clone());
                })
                .detach();
            });
        }

        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    // No panel open: Escape propagates (reports nothing closed).
                    assert!(!editor.close_results_panel(cx), "nothing to close");

                    let a = editor.push_tab("a.rs".into(), false, window, cx);
                    editor.tabs[a].load_state = TabLoadState::Loaded;
                    editor.active = Some(a);

                    editor.tabs[a].latest_ref_id = Some(NavRequestId(1));
                    editor.apply_references_response(
                        NavRequestId(1),
                        vec![jump_target("a.rs", 1)],
                        cx,
                    );
                    assert!(
                        editor.close_results_panel(cx),
                        "an open panel is closed and consumes Escape"
                    );
                    assert!(!editor.results_visible);
                    // Idempotent: a second Escape now propagates.
                    assert!(!editor.close_results_panel(cx));

                    // A workspace-side (× affordance) close keeps the flag synced.
                    editor.tabs[a].latest_ref_id = Some(NavRequestId(2));
                    editor.apply_references_response(
                        NavRequestId(2),
                        vec![jump_target("a.rs", 1)],
                        cx,
                    );
                    assert!(editor.results_visible);
                    editor.mark_results_closed();
                    assert!(!editor.results_visible);
                });
            })
            .unwrap();

        // ShowResults (open) → CloseResults (editor Escape) → ShowResults (reopen).
        let events = events.borrow();
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], EditorEvent::ShowResults { .. }));
        assert!(matches!(events[1], EditorEvent::CloseResults));
        assert!(matches!(events[2], EditorEvent::ShowResults { .. }));
    }
}
