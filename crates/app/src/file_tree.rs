//! File-tree render: the navigable explorer panel built from the client
//! worktree model (`docs/spec-editor.md`, the file-tree render debut; #186).
//!
//! The [`WorktreeModel`] is a flat `path -> entry` map (snapshot as source of
//! truth). This view derives a depth-annotated, collapse-aware *visible row*
//! list from it and caches it (the zed `EntryDetails` pattern): a model fold
//! (via [`FileTree::model_mut`]), a collapse toggle, or a selection change
//! marks the cache dirty, and it is rebuilt once, on the next render — not on
//! every paint, which used to run the derivation twice per frame (once for
//! sizing, once inside the virtual list's row closure) and froze interaction.
//! It renders that cached list virtualized via `gpui-component`'s
//! [`v_virtual_list`] (so a directory with thousands of entries paints only the
//! rows on screen), and lets the user expand/collapse directories and select a
//! file.
//!
//! Bounded to **navigate + open + decorate**, plus inline rename (artboard
//! **State C**, `docs/spec-explorer-file-ops.md`, #675), the context-menu
//! write group (artboard **State D**, #676), drag & drop move (#677), and an
//! in-panel fuzzy filter bar (artboard **State B**,
//! `docs/spec-explorer-search.md`, #679): toggling the header's search
//! control reveals a `gpui-component` `Input` in the header→tree seam;
//! typing narrows [`FileTree::visible_rows`] to matching files plus their
//! force-expanded ancestor directories, over the [`crate::fuzzy_match`]
//! substrate, without ever touching the real `collapsed` set; and a discrete
//! multi-select (artboard **State B**, `docs/spec-explorer-search.md`, #680):
//! `Ctrl`/`Cmd+Click` toggles a path, `Shift+Click` ranges from the cursor,
//! and `Shift+Up`/`Shift+Down` extend it from the keyboard, alongside — never
//! replacing — the single `selected` cursor below. Selecting a
//! file emits [`FileTreeEvent::OpenFile`] carrying its
//! root-relative path — the clean signal the editor surface (#187)
//! subscribes to; activating a multi-selection emits it once per selected
//! file (open-many into the editor's existing `TabPanel`). Rows carry git
//! status and diagnostic severity from the model, rolled up onto ancestor
//! directories (`compute_rollup`, #329) so a
//! collapsed folder still surfaces a modified/errored descendant; a deleted
//! tracked file, whose own row is gone, rolls its status up onto surviving
//! ancestors the same way (#480). Selecting changes no tmux pane/window
//! state — this is a pure GUI surface, agent-agnostic by construction (it only
//! ever reads file paths, kinds, git status, diagnostics, and the `ignored`
//! flag; it never inspects pane processes or file contents). A rename,
//! create, delete, or drag-drop move is user intent over the filesystem, sent
//! as a [`FileTreeEvent::RenameRequested`] / [`FileTreeEvent::CreateRequested`]
//! / [`FileTreeEvent::DeleteRequested`] for `workspace.rs` to forward — no
//! different in kind from any other write. *New File…* / *New Folder…* reuse
//! the State-C inline-editor mechanism for a transient, not-yet-real row;
//! *Delete* is gated behind the `#420` destructive confirm-dialog pattern,
//! never batched. Dragging a row onto a directory (or a file, resolving to
//! its parent) emits the same `RenameRequested` inline rename uses — one
//! message covers both, the client only decides `to`
//! (`docs/spec-explorer-file-ops.md`) — refused client-side before it is ever
//! sent for a no-op same-parent move or a directory dropped into its own
//! subtree ([`resolve_drop`]).
//!
//! Implements `gpui-component`'s `Panel` trait directly (`docs/spec-ide-shell.md`,
//! issue #323), so it can be mounted as a dock panel once the shell adopts
//! `DockArea` (#324).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, px, AnyElement, App, AppContext as _, ClickEvent, ClipboardItem, Context, Div, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, ParentElement as _, Pixels, Render, ScrollStrategy, SharedString,
    Size, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
};
use gpui_component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_component::dialog::{AlertDialog, DialogButtonProps};
use gpui_component::dock::{Panel, PanelControl, PanelEvent};
use gpui_component::input::{
    Escape, Input, InputEvent, InputState, MoveToStart, SelectToNextWordEnd,
};
use gpui_component::menu::{ContextMenuExt as _, PopupMenu};
use gpui_component::{
    h_flex, v_virtual_list, ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _,
    VirtualListScrollHandle, WindowExt as _,
};
use rift_protocol::{
    DiagnosticSeverity, EntryKind, FileOp, FileOpError, GitEntryStatus, GitStatusCode,
};

use crate::file_icons::{self, Glyph};
use crate::fuzzy_match::fuzzy_match;
use crate::worktree::WorktreeModel;

/// Stable, distinct dock-panel identity for the file tree (`Panel::panel_name`).
/// Once shipped this must not change — it is the persisted panel identifier.
pub const FILE_TREE_PANEL_NAME: &str = "explorer";

/// Fixed row height for every tree entry, from the "Explorer — Redesign"
/// artboard's row density (`docs/spec-explorer-redesign.md`) — the 4px
/// [`ROW_BLOCK_PADDING_Y`] top and bottom plus the row's line height,
/// replacing the shipped 22px. The virtual list needs a height per item; a
/// uniform row keeps the size vector trivial to build and the scroll math
/// exact.
const ROW_HEIGHT: Pixels = px(28.0);

/// Vertical padding inside every row (top and bottom), from the artboard's
/// row density.
const ROW_BLOCK_PADDING_Y: Pixels = px(4.0);

/// Corner radius on a row's hover/selected background, from the artboard's
/// row density.
const ROW_RADIUS: Pixels = px(5.0);

/// Gap between every row slot (icon, name, diagnostic dot, git letter),
/// from the artboard's row density.
const ROW_SLOT_GAP: Pixels = px(6.0);

/// Height of the `EXPLORER` header band, from the "Explorer — Redesign"
/// artboard's measured 38px header (`docs/spec-explorer-redesign.md`) —
/// replacing the shipped 28px, which matched the status line's band instead
/// of the artboard's own rhythm.
const HEADER_HEIGHT: Pixels = px(38.0);

/// Left padding inside the header band, from the artboard's measured header
/// inset (asymmetric with [`HEADER_PADDING_RIGHT`] — the artboard gives the
/// `EXPLORER` label more room than the action cluster).
const HEADER_PADDING_LEFT: Pixels = px(14.0);

/// Right padding inside the header band, from the artboard's measured header
/// inset.
const HEADER_PADDING_RIGHT: Pixels = px(12.0);

/// Gap between action-row slots in the header, from the artboard's action
/// row, replacing the shipped 2px.
const HEADER_ACTION_GAP: Pixels = px(12.0);

/// Horizontal padding inside the workspace-root (`RIFT`) row, from the
/// artboard's measured root-row inset — wider than a tree row's padding
/// since the root row carries no reserved icon slot.
const ROOT_ROW_PADDING_X: Pixels = px(12.0);

/// Base horizontal indent at depth 0, before any per-level indent is added —
/// the artboard's indent lanes start at 8px, not flush against the row edge.
const INDENT_BASE: Pixels = px(8.0);

/// Horizontal indent added per nesting level, from the artboard's indent
/// lanes (8/24/40/56 — 16px per level over [`INDENT_BASE`]), replacing the
/// shipped flat 14px-per-level indent.
const INDENT_PER_LEVEL: f32 = 16.0;

/// Width of one indent guide line (`docs/spec-explorer-polish.md`, #711) — a
/// hairline, thin enough to read as a lane divider rather than a second
/// border.
const INDENT_GUIDE_WIDTH: Pixels = px(1.0);

/// Fixed width of the reserved icon slot, the row's leading slot since the
/// chevron twisty was removed (`docs/spec-explorer-polish.md`, #710) —
/// sized to the artboard's icon glyph (`docs/spec-explorer-redesign.md`).
/// Renders the mapped file-type glyph, or the open/closed folder glyph that
/// is now the sole per-folder disclosure affordance ([`file_icons`]).
const ICON_SLOT_WIDTH: Pixels = px(14.0);

/// Diameter of the diagnostic-severity dot and the width of its slot in the
/// trailing cluster, from the artboard's re-spacing (was a 6px dot in an 8px
/// slot).
const DIAGNOSTIC_DOT_SIZE: Pixels = px(7.0);

/// Fixed width of the right-aligned git-status-letter slot in the trailing
/// cluster, from the artboard's re-spacing. Unchanged from the shipped tree.
const GIT_LETTER_SLOT_WIDTH: Pixels = px(12.0);

// ── Actions ───────────────────────────────────────────────────────────────────

/// Move the selection to the previous visible row. Bound to `Up` in
/// `main.rs`, scoped to [`FILE_TREE_KEY_CONTEXT`].
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SelectUp;

/// Move the selection to the next visible row. Bound to `Down`.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SelectDown;

/// Collapse the selected directory if expanded, otherwise select its parent.
/// Bound to `Left`.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct CollapseOrSelectParent;

/// Expand the selected directory if collapsed, otherwise select its first
/// child. Bound to `Right`.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct ExpandOrSelectChild;

/// Open the selected file, or toggle the selected directory. Bound to
/// `Enter`.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct OpenSelected;

/// Select the first visible row. Bound to `Home`.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SelectFirst;

/// Select the last visible row. Bound to `End`.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct SelectLast;

/// Extend the multi-select set (artboard **State B**,
/// `docs/spec-explorer-search.md`, Phase 31, #680) to include the previous
/// visible row, moving the cursor there too — the keyboard counterpart of
/// `Shift+Click`. Bound to `Shift+Up`, scoped to [`FILE_TREE_KEY_CONTEXT`].
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct ExtendSelectionUp;

/// Extend the multi-select set to include the next visible row, moving the
/// cursor there too. Bound to `Shift+Down`.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct ExtendSelectionDown;

/// Reveal the selected row in the tree — expand its ancestors, select it, and
/// scroll it into view (reuses [`FileTree::reveal`]). Dispatched by the row
/// context menu's "Reveal in tree" item; pointer-only, not bound to a key.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct RevealInTree;

/// Copy the selected row's absolute path (the worktree root joined with the
/// row's root-relative path) to the system clipboard. Dispatched by the row
/// context menu's "Copy path" item; pointer-only, not bound to a key.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct CopyAbsolutePath;

/// Copy the selected row's root-relative path verbatim to the system
/// clipboard. Dispatched by the row context menu's "Copy relative path" item;
/// pointer-only, not bound to a key.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct CopyRelativePath;

/// Open a fresh tmux window rooted at the selected row's directory (a file's
/// parent, a directory itself), emitting
/// [`FileTreeEvent::RevealInTerminalRequested`]
/// (`docs/spec-explorer-context-menu.md`). Dispatched by the row context
/// menu's "Reveal in terminal" item; pointer-only, not bound to a key.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct RevealInTerminal;

/// Collapse every directory (reuses [`FileTree::collapse_all`]). Dispatched
/// by the row context menu's "Collapse all" item; pointer-only, not bound to
/// a key.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct CollapseAll;

/// Start renaming the selected row inline (artboard **State C**,
/// `docs/spec-explorer-file-ops.md`). Bound to `F2` in `main.rs`, scoped to
/// [`FILE_TREE_KEY_CONTEXT`]. A no-op with nothing selected.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct StartRename;

/// Start creating a new file inline under the selected row's target
/// directory (artboard **State D**, `docs/spec-explorer-file-ops.md`, #676):
/// a directory targets itself, a file targets its parent — see
/// [`FileTree::create_target_dir`]. Dispatched by the row context menu's
/// "New File…" item; pointer-only, not bound to a key.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct NewFile;

/// Same as [`NewFile`] but creates a directory. Dispatched by the row
/// context menu's "New Folder…" item; pointer-only, not bound to a key.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct NewFolder;

/// Delete the selected row, gated behind the destructive confirm dialog
/// (the `#420` pattern, artboard **State D**). Dispatched by the row context
/// menu's "Delete" item; pointer-only, not bound to a key.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct DeleteSelected;

// ── Constants ─────────────────────────────────────────────────────────────────

/// The GPUI key context the tree establishes around its root, so the
/// navigation actions above are scoped to the focused tree and never steal a
/// keystroke from the terminal panel (agent-first).
pub const FILE_TREE_KEY_CONTEXT: &str = "FileTree";

/// The open signal the tree emits when the user selects a file — the clean
/// interface the editor surface (#187) consumes via `cx.subscribe`. Carries the
/// file's path relative to the worktree root (the same key space as
/// [`WorktreeModel`] entries); the editor resolves it against the daemon root
/// when it issues its read request. Only files emit this — selecting a directory
/// toggles its expansion instead.
#[derive(Debug, Clone, PartialEq)]
pub enum FileTreeEvent {
    /// A file was selected; open it. `path` is root-relative.
    OpenFile { path: String },
    /// The header/root-row "reveal active file" action fired
    /// (`docs/spec-explorer-parity.md`). `workspace.rs`'s existing
    /// `file_tree` subscription handles this by calling the already-present
    /// `reveal_open_file_in_tree`, which owns the active-file path — a no-op
    /// when no file is open. No new protocol, no new cross-crate coupling:
    /// the panel just re-triggers the reveal path the editor-load flow
    /// already drives.
    RevealActiveRequested,
    /// The row context menu's "Reveal in terminal" action fired
    /// (`docs/spec-explorer-context-menu.md`). `dir` is the target's absolute
    /// directory (a file's parent, a directory itself). `workspace.rs`'s
    /// existing `file_tree` subscription routes this to
    /// `SessionView::open_terminal_at`, which enqueues a structural
    /// `new-window -c <dir>` on the existing tmux command channel — no
    /// send-keys into a running pane, no new protocol message.
    RevealInTerminalRequested { dir: String },
    /// The inline rename editor (State C) committed: rename `from` (the
    /// row's original root-relative path) to `to` (root-relative, same
    /// parent — the plain `<parent>/<new-name>` join). `workspace.rs`'s
    /// existing `file_tree` subscription turns this into a
    /// `ClientMessage::RenamePath` on the `file_op_tx` bridge — the same
    /// shape as `OpenFile` above. The tree never sends the request itself:
    /// it has no protocol channel of its own (`docs/spec-explorer-file-ops.md`).
    RenameRequested { from: String, to: String },
    /// The context menu's "New File…" / "New Folder…" inline editor
    /// committed (artboard **State D**, #676): create a `kind` entry at
    /// `path` (root-relative). `workspace.rs`'s existing `file_tree`
    /// subscription turns this into a `ClientMessage::CreateFile` /
    /// `CreateDir` on `file_op_tx` — the same shape as `RenameRequested`
    /// above. The tree never sends the request itself.
    CreateRequested { path: String, kind: EntryKind },
    /// The row context menu's "Delete" item, after the destructive confirm
    /// dialog (the `#420` pattern) was confirmed (artboard **State D**,
    /// #676). `workspace.rs`'s existing `file_tree` subscription turns this
    /// into a `ClientMessage::DeletePath` on `file_op_tx`. Never batched —
    /// one event per confirmed delete.
    DeleteRequested { path: String },
}

/// Which placeholder the panel shows instead of the tree
/// (`docs/spec-explorer-parity.md`) — see [`FileTree::empty_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmptyState {
    /// No snapshot has arrived yet: startup, connecting, or a non-repo root
    /// the daemon has not resolved (`model.root()` is `None`).
    Loading,
    /// A snapshot arrived and the root has no entries (`model.root()` is
    /// `Some`, `model.is_empty()` is `true`).
    EmptyRoot,
}

/// One rendered row: an entry's path, kind, nesting depth, and decoration
/// (git status + diagnostic severity, rolled up onto directories; the
/// `ignored` flag straight from the model). Built by [`FileTree::visible_rows`]
/// and held in [`FileTree::row_cache`]; the cache is always a wholesale
/// replacement of a fresh build (never mutated in place), so it can never
/// drift from the snapshot.
#[derive(Debug, PartialEq)]
struct Row {
    path: String,
    kind: EntryKind,
    depth: usize,
    ignored: bool,
    /// `None` means clean — no descendant (or, for a file, the file itself)
    /// carries a git status.
    git_status: Option<GitRollupStatus>,
    /// `None` means no descendant (or the file itself) carries a diagnostic.
    severity: Option<DiagnosticSeverity>,
    /// `true` for the single transient row [`insert_create_row`] inserts for
    /// the active create editor (artboard **State D**, #676) — not a real
    /// model entry. `false` for every row [`FileTree::visible_rows`] builds
    /// from the model. [`FileTree::render_row`] checks this to swap in
    /// [`FileTree::render_create_row`].
    is_pending_create: bool,
    /// The matched character positions from the active filter query
    /// (`docs/spec-explorer-search.md`, Phase 31), re-based onto this row's
    /// own displayed leaf name ([`leaf_matched_indices`]) — empty with no
    /// active filter, and always empty on a directory row (an ancestor of a
    /// match renders unemphasized; only a matched file carries indices).
    /// [`FileTree::render_row`] splits the name on these for the State B
    /// match-emphasis highlight.
    matched_indices: Vec<usize>,
}

/// A directory's or file's rolled-up git status, ordered by the roll-up's
/// rendering precedence (`docs/spec-explorer-panel.md`: `conflicted > changed
/// > untracked > clean`). A deleted tracked file rolls up under `changed`
/// (the same affordance as modified/added/renamed, per that spec) — #480 only
/// adds the ancestor roll-up, not a new lane. Declared low-to-high so
/// `Option<GitRollupStatus>`'s derived `Ord` (`None` sorts below every variant,
/// standing in for "clean") lets [`Rollup::merge`] pick the worst of two with a
/// plain `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum GitRollupStatus {
    Untracked,
    Changed,
    Conflicted,
}

impl GitRollupStatus {
    /// Classify one path's raw index/worktree status into the roll-up's
    /// three-way precedence. `None` (clean on both sides) is never actually
    /// sent by the daemon (`WorktreeModel::apply_git_update`'s doc), but is
    /// handled rather than assumed away. A deletion is a non-`Unmodified`
    /// status, so it classifies as `Changed` via the catch-all.
    fn from_status(status: GitEntryStatus) -> Option<Self> {
        if status.index == GitStatusCode::Unmerged || status.worktree == GitStatusCode::Unmerged {
            Some(Self::Conflicted)
        } else if status.index == GitStatusCode::Untracked
            || status.worktree == GitStatusCode::Untracked
        {
            Some(Self::Untracked)
        } else if status.index != GitStatusCode::Unmodified
            || status.worktree != GitStatusCode::Unmodified
        {
            Some(Self::Changed)
        } else {
            None
        }
    }

    /// Single-letter badge rendered after a row's name.
    fn badge(self) -> &'static str {
        match self {
            Self::Conflicted => "C",
            Self::Changed => "M",
            Self::Untracked => "U",
        }
    }
}

/// Rank a [`DiagnosticSeverity`] for roll-up comparison: the wire enum is
/// declared in LSP's own severity order, not the roll-up's "worst wins"
/// precedence (`Error > Warning > Information > Hint`), so it has no useful
/// `Ord` of its own.
fn severity_rank(severity: DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Error => 3,
        DiagnosticSeverity::Warning => 2,
        DiagnosticSeverity::Information => 1,
        DiagnosticSeverity::Hint => 0,
    }
}

/// The worse (higher-precedence) of two severities.
fn worse_severity(a: DiagnosticSeverity, b: DiagnosticSeverity) -> DiagnosticSeverity {
    if severity_rank(b) > severity_rank(a) {
        b
    } else {
        a
    }
}

/// The worse of two optional severities; `None` is clean and loses to any
/// `Some`.
fn max_severity(
    a: Option<DiagnosticSeverity>,
    b: Option<DiagnosticSeverity>,
) -> Option<DiagnosticSeverity> {
    match (a, b) {
        (None, None) => None,
        (Some(s), None) | (None, Some(s)) => Some(s),
        (Some(x), Some(y)) => Some(worse_severity(x, y)),
    }
}

/// The worst severity among every server's diagnostics for one path, or
/// `None` when the path currently has none.
fn own_severity(model: &WorktreeModel, path: &str) -> Option<DiagnosticSeverity> {
    model
        .diagnostics(path)?
        .values()
        .flatten()
        .map(|d| d.severity)
        .reduce(worse_severity)
}

/// One path's rolled-up decoration: for a file, just its own git status and
/// diagnostic severity; for a directory, the worst among every descendant
/// (accumulated by [`compute_rollup`]'s single pass).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Rollup {
    git_status: Option<GitRollupStatus>,
    severity: Option<DiagnosticSeverity>,
}

impl Rollup {
    /// Fold `other` in, keeping the worse of each dimension.
    fn merge(&mut self, other: Rollup) {
        self.git_status = self.git_status.max(other.git_status);
        self.severity = max_severity(self.severity, other.severity);
    }
}

/// Compute every path's rolled-up git status + diagnostic severity in a
/// single pass over the model's full entry set — deliberately *not* the
/// collapse-filtered visible set, since a collapsed directory must still
/// surface a hidden descendant's status. A trailing pass folds in git
/// statuses whose path has left the tree (deleted tracked files, #480): they
/// have no row of their own, so their status rolls up onto every surviving
/// ancestor directory instead.
///
/// Tracks currently open ancestor directories by their `dir/` prefix, mirroring
/// [`FileTree::visible_rows`]'s `skip_prefix` — *not* by depth. Depth alone is
/// unsound: because entries are keyed by raw path string in a `BTreeMap`, a
/// same-depth sibling whose name is the directory's name plus a byte less than
/// `/` (0x2F) — most commonly a `.ext` sibling, e.g. `src.rs` next to `src` —
/// sorts *between* the directory and its own children (`"src" < "src.rs" <
/// "src/main.rs"`), so a pop keyed on depth drops the directory off the stack
/// before its real descendants arrive. Popping is instead deferred until the
/// current path has moved lexically *past* the ancestor's whole prefix range
/// (`path > prefix` as well as failing `starts_with`) — a path that merely
/// sorts before the range (like `src.rs`) does not evict its ancestor, and
/// each entry only folds into an ancestor whose prefix it actually
/// `starts_with`, so a non-descendant seen while an ancestor is still open
/// (again, `src.rs`) is not mistakenly merged into it.
fn compute_rollup(model: &WorktreeModel) -> HashMap<String, Rollup> {
    let mut result: HashMap<String, Rollup> = HashMap::new();
    // Shallowest-first stack of ancestor directories whose subtree may still
    // contain upcoming entries, paired with their `dir/` prefix.
    let mut open_dirs: Vec<(String, String)> = Vec::new();

    for (path, entry) in model.entries() {
        while let Some((prefix, _)) = open_dirs.last() {
            if path.starts_with(prefix.as_str()) || path.as_str() < prefix.as_str() {
                break;
            }
            open_dirs.pop();
        }

        let own = Rollup {
            git_status: model
                .git_status(path)
                .and_then(GitRollupStatus::from_status),
            severity: own_severity(model, path),
        };

        for (prefix, ancestor) in &open_dirs {
            if path.starts_with(prefix.as_str()) {
                result.entry(ancestor.clone()).or_default().merge(own);
            }
        }
        result.entry(path.clone()).or_default().merge(own);

        if entry.kind == EntryKind::Dir {
            open_dirs.push((format!("{path}/"), path.clone()));
        }
    }

    // Deleted tracked files have no tree entry anymore (the worktree update
    // removed it), so the pass above never sees them — but the model still
    // holds their git status (a `Deleted` code, classified as `Changed`). Roll
    // each tree-absent status path up onto its surviving ancestor directories
    // so the deletion stays visible in the explorer (#480). No severity here:
    // diagnostics are keyed to live files, and a gone path contributes none.
    for (path, status) in model.git_statuses() {
        if model.get(path).is_some() {
            continue;
        }
        let Some(git_status) = GitRollupStatus::from_status(*status) else {
            continue;
        };
        let own = Rollup {
            git_status: Some(git_status),
            severity: None,
        };
        for ancestor in ancestor_dirs(path) {
            if model.get(ancestor).is_some() {
                result.entry(ancestor.to_owned()).or_default().merge(own);
            }
        }
    }

    result
}

/// Every ancestor directory of `path`, shallowest first — the directories a
/// "reveal" must expand for `path`'s own row to become visible. `a/b/c.rs`
/// yields `["a", "a/b"]`; a top-level path (no separator) yields none.
fn ancestor_dirs(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(|(i, _)| &path[..i])
}

/// Re-base `matched_indices` — [`fuzzy_match`]'s **character** positions into
/// the whole `path` it matched against — onto `path`'s own displayed leaf
/// name ([`FileTree::display_name`]), for [`FileTree::render_row`]'s
/// emphasis span-splitting (`docs/spec-explorer-search.md`, Phase 31).
///
/// The filter bar matches a file's full root-relative path (so a query can
/// reach into an ancestor segment, e.g. `"app main"` matching
/// `crates/app/src/main.rs`), but a row only ever displays its own leaf
/// segment — ancestor segments render on separate ancestor rows, with no
/// emphasis of their own (the artboard's State B). An index landing before
/// the leaf's start (matched inside an ancestor segment this row does not
/// itself render) is dropped rather than mis-rendered; every other index is
/// shifted left by the leaf's start so it indexes correctly into the leaf
/// alone.
fn leaf_matched_indices(path: &str, matched_indices: &[usize]) -> Vec<usize> {
    let leaf_start = path
        .rfind('/')
        .map_or(0, |byte_index| path[..=byte_index].chars().count());
    matched_indices
        .iter()
        .copied()
        .filter(|&index| index >= leaf_start)
        .map(|index| index - leaf_start)
        .collect()
}

/// Split `name`'s characters into consecutive `(text, matched)` runs from
/// `matched_indices` (ascending, [`leaf_matched_indices`]-rebased character
/// positions) — [`FileTree::render_row`]'s span-splitting for the State B
/// match-emphasis highlight, mirroring `results_panel.rs`'s
/// `highlight_segments`. An empty `matched_indices` (no active filter, or an
/// ancestor-of-a-match directory row) returns `name` as a single unmatched
/// span.
fn emphasis_segments(name: &str, matched_indices: &[usize]) -> Vec<(String, bool)> {
    if matched_indices.is_empty() {
        return vec![(name.to_owned(), false)];
    }

    let mut segments = Vec::new();
    let mut current = String::new();
    let mut current_matched = false;
    for (index, ch) in name.chars().enumerate() {
        let matched = matched_indices.contains(&index);
        if current.is_empty() {
            current_matched = matched;
        } else if matched != current_matched {
            segments.push((std::mem::take(&mut current), current_matched));
            current_matched = matched;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        segments.push((current, current_matched));
    }
    segments
}

/// Join a worktree root with a root-relative path into an absolute path —
/// pure string math, no filesystem access. Trailing-slash-safe: `root` may or
/// may not already carry a trailing `/` (the daemon-sent root and the
/// filesystem root `"/"` both need to join without doubling the separator).
/// A top-level `rel` (no `/` of its own) still joins correctly, since its
/// parent is the root itself.
fn absolute_path(root: &str, rel: &str) -> String {
    if root.ends_with('/') {
        format!("{root}{rel}")
    } else {
        format!("{root}/{rel}")
    }
}

/// The absolute directory the "Reveal in terminal" context-menu action opens
/// a new tmux window at: a directory's own absolute path, or a file's parent
/// directory (the root itself for a top-level file). Pure string math on the
/// row's own root-relative `path` — a file's parent is the substring before
/// its last `/`, empty at a top-level row — joined with `root` via
/// [`absolute_path`]; independent of tree expansion state, so it needs no
/// visible-row lookup.
fn reveal_in_terminal_dir(root: &str, kind: &EntryKind, path: &str) -> String {
    match kind {
        EntryKind::Dir => absolute_path(root, path),
        EntryKind::File => match path.rsplit_once('/') {
            Some((parent, _)) => absolute_path(root, parent),
            None => root.to_string(),
        },
    }
}

/// Index of `path` within `rows`, or `None` if it is not currently visible
/// (e.g. its parent was collapsed after it was selected).
fn row_index(rows: &[Row], path: &str) -> Option<usize> {
    rows.iter().position(|r| r.path == path)
}

/// The nearest ancestor directory of `rows[index]`: the closest preceding row
/// with a strictly smaller depth. `None` at a top-level row (depth `0`).
fn parent_path(rows: &[Row], index: usize) -> Option<&str> {
    let depth = rows[index].depth;
    if depth == 0 {
        return None;
    }
    rows[..index]
        .iter()
        .rev()
        .find(|r| r.depth < depth)
        .map(|r| r.path.as_str())
}

/// The first child row of `rows[index]`, if it is an expanded, non-empty
/// directory. Since [`FileTree::visible_rows`] lists an expanded directory's
/// children immediately after it, the first child (if any) is simply the next
/// row, one level deeper.
fn first_child_path(rows: &[Row], index: usize) -> Option<&str> {
    let row = &rows[index];
    if row.kind != EntryKind::Dir {
        return None;
    }
    let next = rows.get(index + 1)?;
    (next.depth == row.depth + 1).then_some(next.path.as_str())
}

/// The row selected after moving down one from `selected` (or the first row
/// when nothing was selected). Clamped at the last row.
fn selection_after_down(rows: &[Row], selected: Option<&str>) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let next = match selected.and_then(|p| row_index(rows, p)) {
        Some(i) => (i + 1).min(rows.len() - 1),
        None => 0,
    };
    Some(rows[next].path.clone())
}

/// The row selected after moving up one from `selected` (or the first row
/// when nothing was selected). Clamped at the first row.
fn selection_after_up(rows: &[Row], selected: Option<&str>) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let next = match selected.and_then(|p| row_index(rows, p)) {
        Some(i) => i.saturating_sub(1),
        None => 0,
    };
    Some(rows[next].path.clone())
}

/// Build the `to` path for an inline rename: `path`'s parent directory (its
/// portion before the last `/`, empty at a top-level row — the same
/// string-math `reveal_in_terminal_dir` uses for a file's parent) joined with
/// `new_name`. Inline rename never changes the parent — that's drag & drop's
/// job (`docs/spec-explorer-file-ops.md`) — so this always lands `to` beside
/// `path`.
fn rename_target(path: &str, new_name: &str) -> String {
    match path.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{new_name}"),
        None => new_name.to_owned(),
    }
}

/// Join a target directory (root-relative, empty for the worktree root) with
/// a new entry's typed `name` — the create editor's counterpart to
/// [`rename_target`], generalized to a possibly-empty (root) parent.
fn join_dir(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

/// Short, user-facing text for a [`FileOpError`] the rename editor re-opens
/// on. Only [`FileOpError::AlreadyExists`] and [`FileOpError::InvalidPath`]
/// ever reach the editor (`FileTree::apply_file_op_result`'s guard); the other
/// variants are listed for completeness so this stays exhaustive.
fn describe_file_op_error(error: FileOpError) -> &'static str {
    match error {
        FileOpError::AlreadyExists => "A file or folder with this name already exists",
        FileOpError::InvalidPath => "Invalid name",
        FileOpError::NotFound => "The original file was not found",
        FileOpError::PermissionDenied => "Permission denied",
        FileOpError::Io => "Rename failed",
    }
}

/// The destructive delete confirm dialog's message for `name` (a row's
/// display name) of `kind` — names the path, and warns "and its contents"
/// for a directory (the `#420` pattern, artboard **State D**,
/// `docs/spec-explorer-file-ops.md`).
fn describe_delete(name: &str, kind: &EntryKind) -> String {
    match kind {
        EntryKind::Dir => format!("Delete \"{name}\" and its contents? This cannot be undone."),
        EntryKind::File => format!("Delete \"{name}\"? This cannot be undone."),
    }
}

/// The root-relative directory a drag-drop resolves onto (`docs/spec-explorer-file-ops.md`):
/// the dropped-on row's own path when it is a directory, or its parent when
/// it is a file (empty at a top-level file — the worktree root). Mirrors
/// [`FileTree::create_target_dir`]'s dir-vs-file resolution, generalized to
/// any row rather than only the selection.
fn drop_target_dir<'a>(target_kind: &EntryKind, target_path: &'a str) -> &'a str {
    match target_kind {
        EntryKind::Dir => target_path,
        EntryKind::File => target_path
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent),
    }
}

/// Whether `target_dir` is `dragged_path` itself or a path-separator-bounded
/// descendant of it — refuses dropping a directory into its own subtree
/// (`docs/spec-explorer-file-ops.md`). Only ever true for a dragged
/// directory: a file has no subtree to drop into.
fn drops_into_own_subtree(dragged_path: &str, dragged_kind: &EntryKind, target_dir: &str) -> bool {
    *dragged_kind == EntryKind::Dir
        && (target_dir == dragged_path
            || target_dir
                .strip_prefix(dragged_path)
                .is_some_and(|rest| rest.starts_with('/')))
}

/// Resolve a drag-drop of `dragged_path` (`dragged_kind`) onto a row
/// (`target_kind`, `target_path`) into a `RenamePath` pair, or `None` when
/// the client-side guard refuses it — a no-op move (the resolved target
/// directory equals the dragged item's current parent) or a directory
/// dropped into itself or a descendant (`docs/spec-explorer-file-ops.md`).
/// `to` joins the resolved target directory with the dragged entry's own
/// basename ([`join_dir`], the same join [`FileTree::commit_create`] uses).
/// Pure and side-effect free — refused/sent is entirely determined by these
/// inputs, so both guards are unit-testable without a `Window`/`Context`.
fn resolve_drop(
    dragged_path: &str,
    dragged_kind: &EntryKind,
    target_kind: &EntryKind,
    target_path: &str,
) -> Option<(String, String)> {
    let target_dir = drop_target_dir(target_kind, target_path);
    let current_parent = dragged_path.rsplit_once('/').map_or("", |(p, _)| p);
    if current_parent == target_dir
        || drops_into_own_subtree(dragged_path, dragged_kind, target_dir)
    {
        return None;
    }
    let basename = FileTree::display_name(dragged_path);
    Some((dragged_path.to_owned(), join_dir(target_dir, basename)))
}

/// The payload a dragged row carries (`docs/spec-explorer-file-ops.md`): its
/// root-relative `path` and `kind`, read by the drop target's `can_drop` /
/// `on_drop` handlers via gpui's `on_drag`/`on_drop`. `Clone` — gpui's drag
/// preview constructor and `drag_over` highlight both receive `&DraggedRow`
/// and may need an owned copy.
#[derive(Clone)]
struct DraggedRow {
    path: String,
    kind: EntryKind,
}

/// The floating preview that follows the cursor while a [`DraggedRow`] is in
/// flight — the row's display name in a themed pill, the fluent
/// `on_drag` constructor's required `Entity<W>`. Mirrors `gpui-component`'s
/// own drag preview (`dock/tab_panel.rs`'s `DragPanel`); theme tokens only,
/// never a hardcoded hex.
struct DragPreview {
    name: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("file-tree-drag-preview")
            .px_2()
            .py_1()
            .rounded(ROW_RADIUS)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .text_sm()
            .text_color(cx.theme().foreground)
            .child(self.name.clone())
    }
}

/// Insert a synthetic transient row for the active create editor into
/// `rows` (an already-built plain visible-row list): the first child of
/// `create.parent` (or the very first row for the worktree root,
/// `create.parent` empty) — "a transient inline input row under the target
/// directory" (`docs/spec-explorer-file-ops.md`). A parent no longer present
/// in `rows` (collapsed again, or removed from the model between the editor
/// opening and this render) falls back to appending at the end, rather than
/// silently dropping the editor's row.
fn insert_create_row(rows: &mut Vec<Row>, create: &CreateEditor) {
    let (index, depth) = if create.parent.is_empty() {
        (0, 0)
    } else {
        match row_index(rows, &create.parent) {
            Some(i) => (i + 1, rows[i].depth + 1),
            None => (rows.len(), 0),
        }
    };
    rows.insert(
        index,
        Row {
            path: String::new(),
            kind: create.kind.clone(),
            depth,
            ignored: false,
            git_status: None,
            severity: None,
            is_pending_create: true,
            matched_indices: Vec::new(),
        },
    );
}

/// Per-tree inline rename editor state (artboard **State C**,
/// `docs/spec-explorer-file-ops.md`). [`FileTree::render_row`] swaps the
/// target row's name label for `input` while [`FileTree::rename`] holds one
/// of these; only one row renames at a time.
struct RenameEditor {
    /// The path being renamed (root-relative) — matched against each row in
    /// `render_row`, and echoed as `RenameRequested::from` on commit.
    path: String,
    /// Seeded to the current name at open (or the just-typed name on an
    /// error re-open) — a `gpui-component` `InputState`, reused verbatim,
    /// never forked.
    input: Entity<InputState>,
    /// Set by an `AlreadyExists` / `InvalidPath` `FileOpResult` reply: shown
    /// inline beside the (re-seeded) input, which keeps the just-typed name
    /// rather than reverting to the original.
    error: Option<String>,
}

/// Per-tree inline create editor state (artboard **State D**,
/// `docs/spec-explorer-file-ops.md`, #676): the "New File…" / "New Folder…"
/// context-menu items open one of these, seeding a fresh, blank-named
/// `gpui-component` `InputState` — reusing the same mechanism as
/// [`RenameEditor`], just with a target *directory* instead of an existing
/// row's own path (there is nothing to rename yet). [`FileTree::render_row`]
/// renders the transient row [`insert_create_row`] inserts into the cache;
/// only one entry can be created at a time.
struct CreateEditor {
    /// The target directory (root-relative; empty for the worktree root) the
    /// new entry is created under — [`FileTree::create_target_dir`]'s result
    /// at the moment the editor opened.
    parent: String,
    /// Whether committing sends `CreateFile` or `CreateDir`.
    kind: EntryKind,
    /// Seeded blank at open (or the just-typed name on an error re-open) —
    /// reused `InputState`, never forked.
    input: Entity<InputState>,
    /// Set by an `AlreadyExists` / `InvalidPath` `FileOpResult` reply: shown
    /// inline beside the (re-seeded) input.
    error: Option<String>,
}

/// The navigable file-tree view.
///
/// Owns the [`WorktreeModel`] (the client mirror it renders) plus the small
/// amount of view-local UI state the model deliberately does not hold: which
/// directories are collapsed and which entry is selected. The model stays pure
/// data; this view is its first rendered consumer.
pub struct FileTree {
    model: WorktreeModel,
    /// Directories the user has collapsed (their subtrees are hidden). A
    /// directory absent from this set is expanded — the tree starts fully
    /// expanded, matching how a fresh snapshot reads.
    collapsed: HashSet<String>,
    /// The currently selected entry's path, or `None` when nothing is selected.
    selected: Option<String>,
    /// The multi-select set (artboard **State B**'s discrete flat-surface
    /// fill, `docs/spec-explorer-search.md`, Phase 31, #680): every path
    /// `Ctrl`/`Cmd+Click`, `Shift+Click`, or `Shift+Up`/`Shift+Down` has added,
    /// alongside — never replacing — the single `selected` cursor above. A
    /// plain click clears it ([`FileTree::click_dir`] /
    /// [`FileTree::click_file`], "standard tree behavior"); `render_row`
    /// renders a member row with the flat `secondary` fill (no accent bar)
    /// unless it is also the cursor row, which keeps the accent-bar
    /// treatment instead. Pruned the same lazy way `selected` is: a stale
    /// path (removed from the model, or collapsed out of view) simply
    /// matches no row at render/use time ([`FileTree::open_many_targets`]);
    /// `selection` itself is never actively swept.
    selection: HashSet<String>,
    scroll_handle: VirtualListScrollHandle,
    /// Lazily created on first [`Focusable::focus_handle`] call (needs an `App`
    /// the plain [`FileTree::new`] does not take, so the tree stays constructible
    /// without a GPUI context for the headless model tests below).
    focus_handle: RefCell<Option<FocusHandle>>,
    /// The precomputed decorated-row cache: the depth-annotated visible-row
    /// list, rebuilt from the model by [`FileTree::refresh_row_cache`] only
    /// when [`FileTree::cache_dirty`] is set. `render()` and the virtual
    /// list's row closure both read this field directly — neither calls
    /// [`FileTree::visible_rows`] itself, which is what running twice per
    /// paint used to freeze interaction on.
    row_cache: Vec<Row>,
    /// Set whenever something that changes the visible-row list happens —
    /// [`FileTree::model_mut`] (any model fold), [`FileTree::toggle_dir`], or a
    /// selection change — and cleared by [`FileTree::refresh_row_cache`] once
    /// it rebuilds [`FileTree::row_cache`] from the fresh state. Marking dirty
    /// inside `model_mut` itself (rather than at each of its callers) means a
    /// fold can never forget to invalidate the cache: there is no other way to
    /// mutate the model.
    cache_dirty: bool,
    /// The active inline rename editor (artboard State C), if any — `None`
    /// most of the time. `Some` while `render_row` swaps a row's name label
    /// for a seeded `InputState`.
    rename: Option<RenameEditor>,
    /// Keeps the rename input's `PressEnter` subscription alive for as long
    /// as `rename` is `Some`; dropped (replaced by `None`) alongside it.
    _rename_input_sub: Option<Subscription>,
    /// The active inline create editor (artboard **State D**, #676), if
    /// any — mirrors `rename` above, but for the "New File…"/"New Folder…"
    /// transient row instead of an existing row's own name.
    create: Option<CreateEditor>,
    /// Keeps the create input's `PressEnter` subscription alive, mirroring
    /// `_rename_input_sub`.
    _create_input_sub: Option<Subscription>,
    /// Armed by a successful create/rename `FileOpResult`
    /// (`FileTree::apply_file_op_result`): the new path to select + reveal
    /// the moment the matching `UpdateWorktree` `added` entry arrives (the
    /// spec's pending-reveal). The tree still never mutates `WorktreeModel`
    /// from a file op — only `model_mut` (fed by `UpdateWorktree`) does; this
    /// field only drives the follow-up selection once the model already has
    /// the row.
    pending_reveal: Option<String>,
    /// Whether the in-panel filter bar (artboard **State B**,
    /// `docs/spec-explorer-search.md`, #679) is open — `render()` mounts
    /// [`FileTree::render_filter_bar`] in the header→tree seam only while
    /// this is `true`. Toggled by the header's search/filter action button;
    /// `Esc` (and toggling the button off) closes it via
    /// [`FileTree::close_filter`].
    filter_active: bool,
    /// The active filter query, mirrored from `filter_input`'s value on
    /// every `InputEvent::Change` — kept as plain state (rather than reading
    /// `filter_input` at derivation time) so [`FileTree::visible_rows`] stays
    /// a pure `&self` derivation with no `cx` dependency, matching every
    /// other seam in this file. Empty is "no filter" — [`FileTree::visible_rows`]
    /// falls back to the plain collapse-aware pass unchanged.
    filter_query: String,
    /// The filter bar's `gpui-component` `InputState`, `Some` only while
    /// `filter_active` — reused verbatim (`connection_screen.rs`/`editor.rs`),
    /// never forked, mirroring [`RenameEditor::input`] / [`CreateEditor::input`].
    filter_input: Option<Entity<InputState>>,
    /// Keeps the filter input's `InputEvent::Change` subscription alive for
    /// as long as `filter_input` is `Some`; dropped (replaced by `None`)
    /// alongside it, mirroring `_rename_input_sub`.
    _filter_input_sub: Option<Subscription>,
    /// Whole-tree collapse driven by clicking the fully-left workspace-root
    /// row (`docs/spec-explorer-polish.md`, #710): a view-local flag,
    /// distinct from the real per-folder `collapsed` set and from the
    /// header's collapse-all/expand-all (`toggle_collapse_all`, which still
    /// folds every directory while leaving root-level entries visible).
    /// While `true`, [`FileTree::visible_rows`] short-circuits to an empty
    /// list — hiding every root-level file and folder — without ever
    /// reading or mutating `collapsed`; clearing it restores the exact
    /// prior tree, mirroring the filter bar's non-mutating discipline
    /// (`filter_query` above).
    root_collapsed: bool,
}

impl FileTree {
    /// Create an empty tree. Feed it daemon worktree messages via
    /// [`FileTree::model_mut`] (then [`Context::notify`]) as they arrive.
    pub fn new() -> Self {
        Self {
            model: WorktreeModel::default(),
            collapsed: HashSet::new(),
            selected: None,
            selection: HashSet::new(),
            scroll_handle: VirtualListScrollHandle::new(),
            focus_handle: RefCell::new(None),
            row_cache: Vec::new(),
            cache_dirty: true,
            rename: None,
            _rename_input_sub: None,
            create: None,
            _create_input_sub: None,
            pending_reveal: None,
            filter_active: false,
            filter_query: String::new(),
            filter_input: None,
            _filter_input_sub: None,
            root_collapsed: false,
        }
    }

    /// The mirrored worktree model, for read access (e.g. root, entry count).
    pub fn model(&self) -> &WorktreeModel {
        &self.model
    }

    /// Mutable access to the model so the daemon-message consumer can fold
    /// snapshots and updates into it. The caller must `cx.notify()` afterwards
    /// to repaint; pruning of collapse/selection state against the new tree
    /// happens lazily at render time, so no extra bookkeeping is needed here.
    ///
    /// Marks the row cache dirty unconditionally: this is the only way to
    /// mutate the model, so every fold site (snapshot / update / git / repo /
    /// diagnostics in `workspace.rs`) invalidates through this one seam
    /// without having to remember to do so itself.
    pub fn model_mut(&mut self) -> &mut WorktreeModel {
        self.cache_dirty = true;
        &mut self.model
    }

    /// The currently selected entry path, if any — the headless handle for the
    /// selection state.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Whether `path` (a directory) is currently collapsed.
    pub fn is_collapsed(&self, path: &str) -> bool {
        self.collapsed.contains(path)
    }

    /// Toggle a directory's expanded/collapsed state.
    fn toggle_dir(&mut self, path: &str) {
        if !self.collapsed.remove(path) {
            self.collapsed.insert(path.to_owned());
        }
        // Collapsing/expanding changes which paths are in the visible-row set.
        self.cache_dirty = true;
    }

    /// Handle a click on a directory row (#481): select it, then toggle its
    /// expansion. Without the selection, arrow-key navigation right after a
    /// click resumed from whatever was selected before, not the row just
    /// clicked. Clears the multi-select set (`docs/spec-explorer-search.md`'s
    /// "a plain click still sets the cursor and clears the multi-set" —
    /// standard tree behavior).
    fn click_dir(&mut self, path: &str) {
        self.selected = Some(path.to_owned());
        self.selection.clear();
        self.toggle_dir(path);
    }

    /// Handle a plain click on a file row: select it and emit the open
    /// signal — [`FileTree::render_row`]'s `on_click` no-modifier branch, and
    /// [`FileTree::open_selected`]'s single-file fallback. Clears the
    /// multi-select set, mirroring [`FileTree::click_dir`]'s "standard tree
    /// behavior" clause (`docs/spec-explorer-search.md`, Phase 31, #680).
    fn click_file(&mut self, path: &str, cx: &mut Context<Self>) {
        self.selected = Some(path.to_owned());
        self.selection.clear();
        self.cache_dirty = true;
        cx.emit(FileTreeEvent::OpenFile {
            path: path.to_owned(),
        });
    }

    /// Toggle `path`'s membership in the multi-select set (`Ctrl`/`Cmd+Click`,
    /// `docs/spec-explorer-search.md`, Phase 31, #680): removes it if already
    /// present, inserts it otherwise, and moves the cursor to `path` too — so
    /// a following `Shift+Click` or keyboard extend continues from the row
    /// just toggled. Additive to the rest of the multi-set: unlike a plain
    /// click, this never clears it.
    fn toggle_selection(&mut self, path: &str) {
        if !self.selection.remove(path) {
            self.selection.insert(path.to_owned());
        }
        self.selected = Some(path.to_owned());
        self.cache_dirty = true;
    }

    /// Select the contiguous visible range between the cursor
    /// (`self.selected`) and `path` (`Shift+Click`,
    /// `docs/spec-explorer-search.md`, Phase 31, #680): replaces the
    /// multi-set with every row between the two (inclusive) in
    /// [`FileTree::row_cache`]'s visible order, regardless of which one comes
    /// first, then moves the cursor to `path`. Falls back to selecting `path`
    /// alone when there is no cursor yet, or either endpoint is not
    /// currently visible (hidden by a collapsed ancestor or the active
    /// filter) — there is no well-defined visible range to compute then.
    ///
    /// Because this also moves the cursor, a second `Shift+Click` re-ranges
    /// from wherever the previous one landed, not a fixed original anchor —
    /// the two-field `selected`/`selection` design
    /// (`docs/spec-explorer-search.md`) keeps no separate anchor; a
    /// documented v1 simplification.
    fn range_select(&mut self, path: &str) {
        self.refresh_row_cache();
        let anchor = self.selected.clone();
        self.selected = Some(path.to_owned());
        self.cache_dirty = true;

        let range = anchor
            .as_deref()
            .and_then(|anchor| row_index(&self.row_cache, anchor))
            .zip(row_index(&self.row_cache, path));

        self.selection = match range {
            Some((a, b)) => {
                let (start, end) = (a.min(b), a.max(b));
                self.row_cache[start..=end]
                    .iter()
                    .map(|row| row.path.clone())
                    .collect()
            }
            None => HashSet::from([path.to_owned()]),
        };
    }

    /// Extend the multi-select set by one row toward the previous visible
    /// row (`ExtendSelectionUp`, `Shift+Up`, `docs/spec-explorer-search.md`,
    /// Phase 31, #680): seeds the set with the current cursor (a no-op
    /// insert on the second-and-later press, since it is already a member),
    /// then moves the cursor up and adds the new row — repeated presses grow
    /// a contiguous range. Mirrors [`FileTree::select_up`]'s
    /// clamp-at-the-first-row and empty-selection-picks-the-first-row
    /// behavior, so it is safe with nothing selected yet. Reversing
    /// direction mid-extend (an `ExtendSelectionDown` right after an
    /// `ExtendSelectionUp`) keeps *adding* toward the far edge rather than
    /// shrinking the near one back off: the multi-set has no separate anchor
    /// beyond the cursor itself, matching the two-field
    /// `selected`/`selection` design — a documented v1 simplification.
    fn extend_selection_up(&mut self) {
        self.refresh_row_cache();
        let Some(current) = self.selected.clone() else {
            self.select_first();
            return;
        };
        self.selection.insert(current.clone());
        if let Some(previous) = selection_after_up(&self.row_cache, Some(&current)) {
            self.selection.insert(previous.clone());
            self.selected = Some(previous);
        }
        self.scroll_selected_into_view();
    }

    /// Extend the multi-select set by one row toward the next visible row
    /// ([`ExtendSelectionDown`], `Shift+Down`) — mirrors
    /// [`FileTree::extend_selection_up`], see its doc for the growth and
    /// clamp behavior.
    fn extend_selection_down(&mut self) {
        self.refresh_row_cache();
        let Some(current) = self.selected.clone() else {
            self.select_first();
            return;
        };
        self.selection.insert(current.clone());
        if let Some(next) = selection_after_down(&self.row_cache, Some(&current)) {
            self.selection.insert(next.clone());
            self.selected = Some(next);
        }
        self.scroll_selected_into_view();
    }

    /// The root-relative file paths [`FileTree::open_selected`] opens as tabs
    /// when the multi-select set is non-empty — the open-many consumer that
    /// keeps the multi-selected state reachable rather than dead UI
    /// (`docs/spec-explorer-search.md`, Phase 31, #680): every path in
    /// `selection` that [`FileTree::row_cache`] still renders as a file, in
    /// visible-row order (not `HashSet` iteration order, which is
    /// unspecified) so the emitted `OpenFile` sequence is stable and matches
    /// what the user saw highlighted. A path the model no longer has, or
    /// that now names a directory, is simply absent from `row_cache` (or
    /// present with a different `kind`) and is skipped — the same lazy
    /// pruning `selected` gets; `selection` itself is never actively swept.
    fn open_many_targets(&self) -> Vec<String> {
        self.row_cache
            .iter()
            .filter(|row| row.kind == EntryKind::File && self.selection.contains(&row.path))
            .map(|row| row.path.clone())
            .collect()
    }

    /// Which placeholder the panel shows in place of the tree, or `None` when
    /// there are rows to render (`docs/spec-explorer-parity.md`). Distinguishes
    /// "no snapshot has arrived yet" from "connected, but the root is
    /// genuinely empty" — the single prior "No files" branch conflated the
    /// two, which read as an error during ordinary startup.
    fn empty_state(&self) -> Option<EmptyState> {
        if self.model.root().is_none() {
            Some(EmptyState::Loading)
        } else if self.model.is_empty() {
            Some(EmptyState::EmptyRoot)
        } else {
            None
        }
    }

    /// Whether every directory the model currently knows about is collapsed
    /// — the header collapse-toggle button's "fully collapsed" state (offers
    /// Expand once true; `docs/spec-explorer-parity.md`). Deliberately
    /// `false`, not the vacuous `true` an empty `all()` would give, when the
    /// model has no directories at all — an all-file tree never claims to be
    /// collapsed.
    fn all_dirs_collapsed(&self) -> bool {
        let mut any_dir = false;
        for entry in self.model.entries().values() {
            if entry.kind == EntryKind::Dir {
                any_dir = true;
                if !self.collapsed.contains(&entry.path) {
                    return false;
                }
            }
        }
        any_dir
    }

    /// Collapse every directory the model currently knows about (header
    /// "Collapse all", also reachable via the row context menu's `CollapseAll`
    /// action): inserts each `EntryKind::Dir` path into the existing
    /// `collapsed` set. Sets `cache_dirty` directly, mirroring `toggle_dir`'s
    /// discipline, rather than looping `toggle_dir` itself — which would
    /// re-expand a directory that was already collapsed. Distinct from the
    /// workspace-root row's whole-tree collapse
    /// ([`FileTree::toggle_root_collapsed`]): this folds every directory
    /// while leaving root-level entries visible.
    fn collapse_all(&mut self) {
        for entry in self.model.entries().values() {
            if entry.kind == EntryKind::Dir {
                self.collapsed.insert(entry.path.clone());
            }
        }
        self.cache_dirty = true;
    }

    /// Expand every directory (header "Expand all"): clears the `collapsed`
    /// set wholesale.
    fn expand_all(&mut self) {
        self.collapsed.clear();
        self.cache_dirty = true;
    }

    /// Flip between fully collapsed and fully expanded — the header
    /// collapse-toggle button's click handler.
    fn toggle_collapse_all(&mut self) {
        if self.all_dirs_collapsed() {
            self.expand_all();
        } else {
            self.collapse_all();
        }
    }

    /// Toggle the whole-tree collapse driven by clicking the fully-left
    /// workspace-root row (`docs/spec-explorer-polish.md`, #710) — distinct
    /// from [`FileTree::toggle_collapse_all`], which still folds every
    /// directory while leaving root-level entries visible. Flips
    /// `root_collapsed` and marks the cache dirty, mirroring `toggle_dir`'s
    /// discipline; never reads or writes the real `collapsed` set.
    fn toggle_root_collapsed(&mut self) {
        self.root_collapsed = !self.root_collapsed;
        self.cache_dirty = true;
    }

    /// Toggle the in-panel filter bar (artboard **State B**,
    /// `docs/spec-explorer-search.md`, #679) — the header's search/filter
    /// action button's click handler.
    fn toggle_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_active {
            self.close_filter(cx);
        } else {
            self.open_filter(window, cx);
        }
    }

    /// Open the filter bar: seeds a fresh, blank `gpui-component` `InputState`
    /// — reused verbatim, never forked — and focuses it on the next frame
    /// (mirrors [`FileTree::open_rename_editor`]'s focus mechanics, minus the
    /// select-all: a filter starts blank, there is nothing to select). A
    /// no-op while already open.
    fn open_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_active {
            return;
        }

        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files..."));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filter_query = input.read(cx).value().trim().to_owned();
                    this.cache_dirty = true;
                    cx.notify();
                }
            },
        );

        let focus_target = input.clone();
        window.on_next_frame(move |window, cx| {
            focus_target.update(cx, |state, cx| state.focus(window, cx));
        });

        self.filter_active = true;
        self.filter_input = Some(input);
        self._filter_input_sub = Some(sub);
        self.cache_dirty = true;
        cx.notify();
    }

    /// Close the filter bar (`Esc`, or toggling the header control off):
    /// clears the query and drops the input. `visible_rows` falls back to
    /// the plain collapse-aware pass the moment `filter_query` is empty
    /// again, so this restores the exact prior tree — the user's real
    /// `collapsed` set was never touched by the filtered pass
    /// (`docs/spec-explorer-search.md`). A no-op while already closed.
    fn close_filter(&mut self, cx: &mut Context<Self>) {
        if !self.filter_active {
            return;
        }
        self.filter_active = false;
        self.filter_query.clear();
        self.filter_input = None;
        self._filter_input_sub = None;
        self.cache_dirty = true;
        cx.notify();
    }

    /// The leaf (final non-empty path segment) of a root's absolute path —
    /// the workspace-root row's label before uppercasing. The filesystem
    /// root `"/"` has no real leaf name; it renders unchanged rather than as
    /// an empty label.
    fn root_leaf(root: &str) -> &str {
        root.rsplit('/')
            .find(|segment| !segment.is_empty())
            .unwrap_or(root)
    }

    /// Reveal `path` in the tree: expand every ancestor directory, select its
    /// row, and scroll it into view. [`crate::workspace::WorkspaceView`] calls
    /// this whenever the editor finishes opening or switching to a file
    /// (`docs/spec-explorer-panel.md`, #331). A `path` absent from the model —
    /// not (yet) streamed by the daemon, or a stale signal — is a no-op:
    /// nothing to expand, select, or scroll to.
    ///
    /// Mirrors [`FileTree::toggle_dir`]'s dirty-marking discipline: expansion
    /// or selection changes mark [`FileTree::cache_dirty`], same as a click.
    /// The cache is then refreshed immediately (not deferred to the next
    /// render) so the row index used to scroll is current.
    pub fn reveal(&mut self, path: &str) {
        if self.model.get(path).is_none() {
            return;
        }

        for ancestor in ancestor_dirs(path) {
            if self.collapsed.remove(ancestor) {
                self.cache_dirty = true;
            }
        }
        if self.selected.as_deref() != Some(path) {
            self.selected = Some(path.to_owned());
            self.cache_dirty = true;
        }

        self.scroll_selected_into_view();
    }

    /// After an `UpdateWorktree` folds `added_paths` into the model, reveal
    /// the pending create/rename target if it just arrived (the
    /// pending-reveal affordance, `docs/spec-explorer-file-ops.md`): a
    /// successful create/rename arms [`FileTree::pending_reveal`]
    /// (`FileTree::apply_file_op_result`); this selects + reveals that row
    /// the moment the push-only recompute actually adds it, then clears the
    /// marker. A no-op when nothing is pending or `added_paths` doesn't
    /// contain it — the model itself is never touched here, only read via
    /// `reveal`. Called from `workspace.rs`'s `apply_worktree_message`,
    /// after the model fold.
    pub(crate) fn apply_pending_reveal(&mut self, added_paths: &[String]) {
        let Some(pending) = &self.pending_reveal else {
            return;
        };
        if added_paths.iter().any(|path| path == pending) {
            let pending = self.pending_reveal.take().expect("checked Some above");
            self.reveal(&pending);
        }
    }

    /// Start renaming the selected row inline ([`StartRename`] / `F2`): a
    /// no-op with nothing selected or a selection the model no longer has.
    fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        if self.model.get(&path).is_none() {
            return;
        }
        let name = Self::display_name(&path).to_owned();
        self.open_rename_editor(path, name, None, window, cx);
    }

    /// Open (or re-open) the inline rename editor for `path`, seeded with
    /// `seed_name` and an optional inline `error` (set on an error re-open,
    /// `docs/spec-explorer-file-ops.md`). Focuses the fresh `InputState` and
    /// best-effort selects the name portion before the last `.` (the
    /// extension left unselected, the standard IDE affordance) via the
    /// input's own public `MoveToStart` / `SelectToNextWordEnd` actions —
    /// deferred to [`Window::on_next_frame`] since [`Window::dispatch_action`]
    /// resolves the focused node against the *last rendered* frame, and this
    /// row has not painted with the new input yet at call time. Reused
    /// `InputState`, never forked (`docs/spec-explorer-file-ops.md`).
    fn open_rename_editor(
        &mut self,
        path: String,
        seed_name: String,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(seed_name));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, _input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.commit_rename(window, cx);
                }
            },
        );

        let focus_target = input.clone();
        window.on_next_frame(move |window, cx| {
            focus_target.update(cx, |state, cx| state.focus(window, cx));
            window.dispatch_action(Box::new(MoveToStart), cx);
            window.dispatch_action(Box::new(SelectToNextWordEnd), cx);
        });

        self.rename = Some(RenameEditor { path, input, error });
        self._rename_input_sub = Some(sub);
        self.cache_dirty = true;
        cx.notify();
    }

    /// Commit the active rename editor's typed value (`Enter`): closes the
    /// editor immediately (optimistic — `docs/spec-explorer-file-ops.md`'s
    /// "the reply drives UX only" contract), then emits
    /// [`FileTreeEvent::RenameRequested`] for `workspace.rs` to forward as a
    /// `RenamePath`. A no-op send (editor still closes) for a blank name, a
    /// name containing `/` (would silently reparent — inline rename only
    /// ever targets the same parent, drag & drop is the move affordance), or
    /// a name identical to the current one. `FileTree::apply_file_op_result`
    /// reconstructs the editor from the `FileOpResult` echo alone on an
    /// error reply, so no state needs to survive the close here.
    fn commit_rename(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.rename.take() else {
            return;
        };
        self._rename_input_sub = None;
        self.cache_dirty = true;

        let typed = editor.input.read(cx).value().trim().to_owned();
        if !typed.is_empty() && !typed.contains('/') {
            let to = rename_target(&editor.path, &typed);
            if to != editor.path {
                cx.emit(FileTreeEvent::RenameRequested {
                    from: editor.path,
                    to,
                });
            }
        }
        cx.notify();
    }

    /// Cancel the active rename editor (`Escape`): closes it with no send.
    fn cancel_rename(&mut self) {
        if self.rename.take().is_some() {
            self._rename_input_sub = None;
            self.cache_dirty = true;
        }
    }

    /// The root-relative directory a create targets ([`NewFile`] /
    /// [`NewFolder`]): the selected row's own path when it is a directory,
    /// its parent when it is a file (matching [`reveal_in_terminal_dir`]'s
    /// target resolution, but root-relative — the model's own key space —
    /// instead of absolute), or the worktree root (`String::new()`) with
    /// nothing selected or a selection the model no longer has.
    fn create_target_dir(&self) -> String {
        let Some(selected) = self.selected.as_deref() else {
            return String::new();
        };
        match self.model.get(selected) {
            Some(entry) if entry.kind == EntryKind::Dir => selected.to_owned(),
            Some(_) => selected
                .rsplit_once('/')
                .map(|(parent, _)| parent.to_owned())
                .unwrap_or_default(),
            None => String::new(),
        }
    }

    /// Start creating a new `kind` entry inline under the selected row's
    /// target directory (artboard **State D**, [`NewFile`] / [`NewFolder`]):
    /// cancels any active rename first — only one inline editor is ever open
    /// at a time — then opens a blank create editor targeting
    /// [`FileTree::create_target_dir`]. A no-op before any snapshot has
    /// arrived (`model.root()` is `None`) — there is no tree to create into.
    fn start_create(&mut self, kind: EntryKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.root().is_none() {
            return;
        }
        self.cancel_rename();
        let parent = self.create_target_dir();
        self.open_create_editor(parent, kind, String::new(), None, window, cx);
    }

    /// Open (or re-open) the inline create editor targeting `parent`, seeded
    /// with `seed_name` and an optional inline `error` (set on an error
    /// re-open). Expands `parent` if it was collapsed, so the transient row
    /// [`insert_create_row`] adds is actually visible. Mirrors
    /// [`FileTree::open_rename_editor`]'s focus/subscribe mechanics — reused
    /// `InputState`, never forked — but seeds no selection: a blank or
    /// just-typed name has nothing worth pre-selecting the way an existing
    /// name's extension does.
    fn open_create_editor(
        &mut self,
        parent: String,
        kind: EntryKind,
        seed_name: String,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !parent.is_empty() && self.collapsed.remove(&parent) {
            self.cache_dirty = true;
        }

        let input = cx.new(|cx| InputState::new(window, cx).default_value(seed_name));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, _input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.commit_create(window, cx);
                }
            },
        );

        let focus_target = input.clone();
        window.on_next_frame(move |window, cx| {
            focus_target.update(cx, |state, cx| state.focus(window, cx));
        });

        self.create = Some(CreateEditor {
            parent,
            kind,
            input,
            error,
        });
        self._create_input_sub = Some(sub);
        self.cache_dirty = true;
        cx.notify();
    }

    /// Commit the active create editor's typed value (`Enter`): closes the
    /// editor immediately (optimistic, mirroring
    /// [`FileTree::commit_rename`]), then emits
    /// [`FileTreeEvent::CreateRequested`] for `workspace.rs` to forward as a
    /// `CreateFile` / `CreateDir`. A no-op send (editor still closes) for a
    /// blank name or a name containing `/` (a single path segment at a time
    /// — nesting under the target directory is all this affordance covers;
    /// deeper structure is created one level at a time, same as any file
    /// manager).
    fn commit_create(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.create.take() else {
            return;
        };
        self._create_input_sub = None;
        self.cache_dirty = true;

        let typed = editor.input.read(cx).value().trim().to_owned();
        if !typed.is_empty() && !typed.contains('/') {
            let path = join_dir(&editor.parent, &typed);
            cx.emit(FileTreeEvent::CreateRequested {
                path,
                kind: editor.kind,
            });
        }
        cx.notify();
    }

    /// Cancel the active create editor (`Escape`): closes it with no send.
    fn cancel_create(&mut self) {
        if self.create.take().is_some() {
            self._create_input_sub = None;
            self.cache_dirty = true;
        }
    }

    /// Re-open the create editor after an `AlreadyExists` / `InvalidPath`
    /// `FileOpResult` reply for a create request that targeted `path`
    /// (root-relative): re-derives the target directory and typed name from
    /// `path` alone, mirroring the rename arm's reply-echo reconstruction —
    /// no local state needs to survive the optimistic close.
    fn reopen_create_editor(
        &mut self,
        kind: EntryKind,
        path: &str,
        error: FileOpError,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parent = path
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_owned())
            .unwrap_or_default();
        let seed_name = Self::display_name(path).to_owned();
        let message = describe_file_op_error(error).to_owned();
        self.open_create_editor(parent, kind, seed_name, Some(message), window, cx);
    }

    /// Open the destructive delete confirm dialog for the selected row (the
    /// `#420` pattern used by `SourceControlPanel::confirm_discard` and the
    /// editor's dirty-close dialog, artboard **State D**, [`DeleteSelected`]):
    /// confirming emits [`FileTreeEvent::DeleteRequested`] for
    /// `workspace.rs` to forward as a `DeletePath`; cancelling leaves the
    /// entry untouched. Never batched — one dialog, one path. A no-op with
    /// nothing selected or a selection the model no longer has.
    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        let Some(entry) = self.model.get(&path) else {
            return;
        };
        let name = Self::display_name(&path).to_owned();
        let kind = entry.kind.clone();
        let entity = cx.entity();

        window.open_alert_dialog(cx, move |alert: AlertDialog, _, _| {
            let entity = entity.clone();
            let path = path.clone();
            alert
                .title("Delete")
                .description(SharedString::from(describe_delete(&name, &kind)))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Delete")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true)
                        .on_ok(move |_, _window, cx| {
                            entity.update(cx, |_this, cx| {
                                cx.emit(FileTreeEvent::DeleteRequested { path: path.clone() });
                            });
                            true
                        }),
                )
        });
    }

    /// Route a `FileOpResult` reply to UX transitions only — never mutates
    /// `WorktreeModel` (the single writer is `UpdateWorktree`,
    /// `docs/spec-explorer-file-ops.md`'s single-writer rule). A successful
    /// create/rename arms [`FileTree::pending_reveal`] with the new path. An
    /// `AlreadyExists` / `InvalidPath` reply to a `Rename` re-opens the
    /// editor for `from` (the file, unmoved — the op failed) seeded with
    /// `to`'s basename (the name the user just typed) and the error text; a
    /// same-shaped `CreateFile` / `CreateDir` failure re-opens the create
    /// editor via [`FileTree::apply_create_result`]. Both reconstruct their
    /// editor entirely from the reply's own echo — no local state had to
    /// survive the optimistic close. `Delete` carries no UX transition of
    /// its own here: the confirm dialog already dismissed on click, and the
    /// row's disappearance arrives through the ordinary `UpdateWorktree`
    /// push, same as every other op's tree-structure change.
    pub(crate) fn apply_file_op_result(
        &mut self,
        op: FileOp,
        ok: bool,
        error: Option<FileOpError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match op {
            FileOp::Rename { from, to } => {
                if ok {
                    self.pending_reveal = Some(to);
                } else if matches!(
                    error,
                    Some(FileOpError::AlreadyExists) | Some(FileOpError::InvalidPath)
                ) {
                    let seed_name = Self::display_name(&to).to_owned();
                    let message =
                        describe_file_op_error(error.expect("matched Some above")).to_owned();
                    self.open_rename_editor(from, seed_name, Some(message), window, cx);
                }
            }
            FileOp::CreateFile { path } => {
                self.apply_create_result(EntryKind::File, path, ok, error, window, cx);
            }
            FileOp::CreateDir { path } => {
                self.apply_create_result(EntryKind::Dir, path, ok, error, window, cx);
            }
            FileOp::Delete { .. } => {}
        }
    }

    /// The `FileOp::CreateFile` / `FileOp::CreateDir` arm of
    /// [`FileTree::apply_file_op_result`] (artboard **State D**): a success
    /// arms [`FileTree::pending_reveal`]; an `AlreadyExists` / `InvalidPath`
    /// reply re-opens the create editor via
    /// [`FileTree::reopen_create_editor`] — the same "legible error, typed
    /// name preserved" contract the rename arm gives (the milestone QA
    /// gate's "a name collision is refused with a legible error").
    fn apply_create_result(
        &mut self,
        kind: EntryKind,
        path: String,
        ok: bool,
        error: Option<FileOpError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ok {
            self.pending_reveal = Some(path);
        } else if matches!(
            error,
            Some(FileOpError::AlreadyExists) | Some(FileOpError::InvalidPath)
        ) {
            self.reopen_create_editor(kind, &path, error.expect("matched Some above"), window, cx);
        }
    }

    /// Apply a drop of `dragged` onto a row (`target_kind`, `target_path`):
    /// [`resolve_drop`] resolves the target and refuses the two client-side
    /// guard cases; a resolved drop emits [`FileTreeEvent::RenameRequested`]
    /// for `workspace.rs` to forward as a `RenamePath`, the same one message
    /// inline rename uses — the client only decides `to`
    /// (`docs/spec-explorer-file-ops.md`). The tree never sends the request
    /// itself and never mutates `WorktreeModel` here; the row moves once the
    /// daemon's `UpdateWorktree` push arrives, same as every other file op.
    fn handle_drop(
        &mut self,
        dragged: &DraggedRow,
        target_kind: &EntryKind,
        target_path: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some((from, to)) =
            resolve_drop(&dragged.path, &dragged.kind, target_kind, target_path)
        {
            cx.emit(FileTreeEvent::RenameRequested { from, to });
        }
    }

    /// Scroll the selected row into view. The tail shared by [`FileTree::reveal`]
    /// and every keyboard-navigation method below (#431): any selection movement
    /// must keep the selected row on screen — arrow keys used to walk it
    /// straight off. Refreshes the row cache first (a collapse/expand step may
    /// have invalidated it) so the row index used to scroll is current; with no
    /// selection, or one not in the visible set, there is nothing to scroll to.
    fn scroll_selected_into_view(&mut self) {
        self.refresh_row_cache();
        let Some(selected) = self.selected.as_deref() else {
            return;
        };
        if let Some(ix) = row_index(&self.row_cache, selected) {
            self.scroll_handle
                .scroll_to_item(ix, ScrollStrategy::Nearest);
        }
    }

    /// Move the selection to the previous visible row ([`SelectUp`]) and
    /// scroll it into view.
    fn select_up(&mut self) {
        self.refresh_row_cache();
        self.selected = selection_after_up(&self.row_cache, self.selected.as_deref());
        self.scroll_selected_into_view();
    }

    /// Move the selection to the next visible row ([`SelectDown`]) and scroll
    /// it into view.
    fn select_down(&mut self) {
        self.refresh_row_cache();
        self.selected = selection_after_down(&self.row_cache, self.selected.as_deref());
        self.scroll_selected_into_view();
    }

    /// Select the first visible row ([`SelectFirst`]) and scroll it into view.
    fn select_first(&mut self) {
        self.refresh_row_cache();
        self.selected = self.row_cache.first().map(|row| row.path.clone());
        self.scroll_selected_into_view();
    }

    /// Select the last visible row ([`SelectLast`]) and scroll it into view.
    fn select_last(&mut self) {
        self.refresh_row_cache();
        self.selected = self.row_cache.last().map(|row| row.path.clone());
        self.scroll_selected_into_view();
    }

    /// Collapse the selected directory if it is expanded; otherwise select
    /// its parent ([`CollapseOrSelectParent`]), scrolling the resulting
    /// selection into view. A no-op at a top-level row with nothing to
    /// collapse. Selects the first row when nothing was selected yet, matching
    /// [`FileTree::select_down`]/[`FileTree::select_up`].
    fn collapse_or_select_parent(&mut self) {
        self.refresh_row_cache();
        let Some(selected) = self.selected.clone() else {
            self.select_first();
            return;
        };
        let Some(index) = row_index(&self.row_cache, &selected) else {
            return;
        };
        let expanded =
            self.row_cache[index].kind == EntryKind::Dir && !self.collapsed.contains(&selected);
        if expanded {
            self.toggle_dir(&selected);
        } else if let Some(parent) = parent_path(&self.row_cache, index).map(str::to_owned) {
            self.selected = Some(parent);
        }
        self.scroll_selected_into_view();
    }

    /// Expand the selected directory if it is collapsed; otherwise select its
    /// first child ([`ExpandOrSelectChild`]), scrolling the resulting
    /// selection into view. A no-op on a file or an empty, already-expanded
    /// directory. Selects the first row when nothing was selected yet,
    /// matching [`FileTree::select_down`]/[`FileTree::select_up`].
    fn expand_or_select_child(&mut self) {
        self.refresh_row_cache();
        let Some(selected) = self.selected.clone() else {
            self.select_first();
            return;
        };
        let Some(index) = row_index(&self.row_cache, &selected) else {
            return;
        };
        let collapsed =
            self.row_cache[index].kind == EntryKind::Dir && self.collapsed.contains(&selected);
        if collapsed {
            self.toggle_dir(&selected);
        } else if let Some(child) = first_child_path(&self.row_cache, index).map(str::to_owned) {
            self.selected = Some(child);
        }
        self.scroll_selected_into_view();
    }

    /// Open the selected file, or toggle the selected directory
    /// ([`OpenSelected`]) — the keyboard equivalent of [`FileTree::render_row`]'s
    /// `on_click`. With a non-empty multi-select set, activates it instead
    /// (artboard **State B**'s open-many consumer,
    /// `docs/spec-explorer-search.md`, Phase 31, #680): emits
    /// [`FileTreeEvent::OpenFile`] once per [`FileTree::open_many_targets`],
    /// opening every selected file as an editor tab through the existing
    /// path. A multi-set holding only directories yields no targets and
    /// falls through to the single-cursor behavior below.
    fn open_selected(&mut self, cx: &mut Context<Self>) {
        self.refresh_row_cache();
        let targets = self.open_many_targets();
        if !targets.is_empty() {
            for path in targets {
                cx.emit(FileTreeEvent::OpenFile { path });
            }
            return;
        }
        let Some(selected) = self.selected.clone() else {
            return;
        };
        let Some(index) = row_index(&self.row_cache, &selected) else {
            return;
        };
        if self.row_cache[index].kind == EntryKind::Dir {
            self.toggle_dir(&selected);
        } else {
            cx.emit(FileTreeEvent::OpenFile { path: selected });
        }
    }

    /// Rebuild [`FileTree::row_cache`] from the model when [`FileTree::cache_dirty`]
    /// is set; a no-op otherwise. The single path that calls
    /// [`FileTree::visible_rows`] — `render()` calls this once per paint instead
    /// of deriving the visible-row list itself, and the virtual list's row
    /// closure only ever reads the resulting [`FileTree::row_cache`]. When a
    /// create editor is active, [`insert_create_row`] adds its transient row
    /// on top of the model-derived list (artboard **State D**, #676) — the
    /// model itself is never touched by a create, only this render-time cache.
    fn refresh_row_cache(&mut self) {
        if self.cache_dirty {
            let mut rows = self.visible_rows();
            if let Some(create) = &self.create {
                insert_create_row(&mut rows, create);
            }
            self.row_cache = rows;
            self.cache_dirty = false;
        }
    }

    /// Build the flattened, depth-annotated, decorated list of currently
    /// *visible* rows from the model's flat path map: an empty list while
    /// the workspace-root row's whole-tree collapse
    /// (`docs/spec-explorer-polish.md`, #710) is active — checked first, and
    /// short-circuiting before the model is ever walked — otherwise the
    /// plain collapse-aware pass with no active filter query, or the
    /// filtered narrowing pass (`docs/spec-explorer-search.md`, Phase 31)
    /// once one is set — see [`FileTree::visible_rows_unfiltered`] /
    /// [`FileTree::visible_rows_filtered`].
    fn visible_rows(&self) -> Vec<Row> {
        if self.root_collapsed {
            return Vec::new();
        }
        if self.filter_query.is_empty() {
            self.visible_rows_unfiltered()
        } else {
            self.visible_rows_filtered(&self.filter_query)
        }
    }

    /// The plain collapse-aware visible-row pass — unchanged by the filter
    /// bar (`docs/spec-explorer-search.md`: "when no query is active the
    /// derivation is exactly today's collapse-aware pass").
    ///
    /// The model keys entries by their root-relative path in a `BTreeMap`, so
    /// iteration is already lexicographically ordered — which, for slash-
    /// separated paths, places every entry directly after its parent and groups
    /// a directory's whole subtree together. That lets a single pass hide
    /// subtrees: when a collapsed directory is seen, its descendants (every path
    /// under `dir/`) are skipped until iteration leaves that prefix.
    ///
    /// Decoration is looked up from [`compute_rollup`], which walks the whole
    /// model (not this collapse-filtered pass) — a collapsed directory's row
    /// still needs its hidden descendants' rolled-up status.
    fn visible_rows_unfiltered(&self) -> Vec<Row> {
        let rollup = compute_rollup(&self.model);
        let mut rows = Vec::new();
        // The prefix (`dir/`) of the shallowest collapsed directory currently
        // being skipped. While set, any path starting with it is a hidden
        // descendant; the first path that does not is past the subtree.
        let mut skip_prefix: Option<String> = None;

        for (path, entry) in self.model.entries() {
            if let Some(prefix) = &skip_prefix {
                if path.starts_with(prefix.as_str()) {
                    continue;
                }
                skip_prefix = None;
            }

            // Depth is the number of path separators: a top-level entry has
            // none, `src/main.rs` has one, and so on.
            let depth = path.bytes().filter(|&b| b == b'/').count();
            let decoration = rollup.get(path).copied().unwrap_or_default();
            rows.push(Row {
                path: path.clone(),
                kind: entry.kind.clone(),
                depth,
                ignored: entry.ignored,
                git_status: decoration.git_status,
                severity: decoration.severity,
                is_pending_create: false,
                matched_indices: Vec::new(),
            });

            if entry.kind == EntryKind::Dir && self.collapsed.contains(path) {
                // Hide this directory's subtree: everything under `path/`.
                skip_prefix = Some(format!("{path}/"));
            }
        }

        rows
    }

    /// The filtered visible-row pass (`docs/spec-explorer-search.md`, Phase
    /// 31): matches every **file** in the full `entries()` set against
    /// `query` via the fuzzy substrate ([`fuzzy_match`]), then walks the
    /// model once more keeping only matched files and their ancestor
    /// directories.
    ///
    /// This pass never reads `self.collapsed` at all, so a match's ancestors
    /// are force-expanded by construction regardless of their real collapse
    /// state, and the real `collapsed` set is neither read nor mutated —
    /// clearing the query falls back to [`FileTree::visible_rows_unfiltered`],
    /// which restores the exact prior tree
    /// (`docs/spec-explorer-search.md`'s "force-expansion is scoped to the
    /// filtered pass").
    fn visible_rows_filtered(&self, query: &str) -> Vec<Row> {
        let rollup = compute_rollup(&self.model);

        // First pass: every ancestor directory of a matched file, so the
        // second pass below (which walks the model in path order) knows
        // whether to keep a directory row *before* it ever reaches that
        // directory's own matching descendant.
        let mut ancestors: HashSet<&str> = HashSet::new();
        for (path, entry) in self.model.entries() {
            if entry.kind == EntryKind::File && fuzzy_match(query, path).is_some() {
                ancestors.extend(ancestor_dirs(path));
            }
        }

        self.model
            .entries()
            .iter()
            .filter_map(|(path, entry)| {
                let matched_indices = match entry.kind {
                    EntryKind::File => {
                        leaf_matched_indices(path, &fuzzy_match(query, path)?.matched_indices)
                    }
                    EntryKind::Dir if ancestors.contains(path.as_str()) => Vec::new(),
                    EntryKind::Dir => return None,
                };

                let depth = path.bytes().filter(|&b| b == b'/').count();
                let decoration = rollup.get(path).copied().unwrap_or_default();
                Some(Row {
                    path: path.clone(),
                    kind: entry.kind.clone(),
                    depth,
                    ignored: entry.ignored,
                    git_status: decoration.git_status,
                    severity: decoration.severity,
                    is_pending_create: false,
                    matched_indices,
                })
            })
            .collect()
    }

    /// Display name for a row: the final path segment (the model holds full
    /// root-relative paths; the tree shows the leaf, with depth carrying the
    /// hierarchy).
    fn display_name(path: &str) -> &str {
        path.rsplit('/').next().unwrap_or(path)
    }

    /// The in-panel header band above the tree
    /// (`docs/spec-explorer-redesign.md`): an uppercase, bold, muted
    /// `EXPLORER` label at the artboard's 11px label style, flush against
    /// the panel surface (no distinct elevated tint — the artboard's header
    /// sits transparent over the panel, only a bottom hairline separates
    /// it) with a right-aligned action row: collapse-all / expand-all (a
    /// toggle reflecting `all_dirs_collapsed`), reveal-active, and the
    /// search/filter toggle over `filter_active` (artboard **State B**,
    /// `docs/spec-explorer-search.md`, #679) — Phase 27's reserved action
    /// slot, now live. Still consciously omits the artboard's *new file*
    /// glyph (Phase 30's context-menu "New File…"/"New Folder…" already
    /// cover that capability; see the spec's prior decisions).
    ///
    /// Every action renders as a real `IconName` glyph via `Button::icon(...)`
    /// — the shipping `rift` binary embeds gpui-component's SVG icon assets
    /// through the `RiftAssets` delegating source (`main.rs`, issue #597), so
    /// the icons resolve in the product build, not only under the dev-only
    /// `gallery` feature. Each button is `compact()`, giving it
    /// gpui-component's fixed `min_w_5` (20px) icon-button footprint.
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let all_collapsed = self.all_dirs_collapsed();

        let collapse_toggle = Button::new("file-tree-collapse-toggle")
            .ghost()
            .xsmall()
            .compact()
            .icon(if all_collapsed {
                IconName::Plus
            } else {
                IconName::Minus
            })
            .tooltip(if all_collapsed {
                "Expand all"
            } else {
                "Collapse all"
            })
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.toggle_collapse_all();
                cx.notify();
            }));

        let reveal_active = Button::new("file-tree-reveal-active")
            .ghost()
            .xsmall()
            .compact()
            .icon(IconName::Frame)
            .tooltip("Reveal active file")
            .on_click(cx.listener(|_this, _event, _window, cx| {
                cx.emit(FileTreeEvent::RevealActiveRequested);
            }));

        let filter_toggle = Button::new("file-tree-filter-toggle")
            .ghost()
            .xsmall()
            .compact()
            .icon(IconName::Search)
            .selected(self.filter_active)
            .tooltip(if self.filter_active {
                "Close filter"
            } else {
                "Filter files"
            })
            .on_click(cx.listener(|this, _event, window, cx| {
                this.toggle_filter(window, cx);
            }));

        h_flex()
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .h(HEADER_HEIGHT)
            .pl(HEADER_PADDING_LEFT)
            .pr(HEADER_PADDING_RIGHT)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_size(px(11.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("EXPLORER"),
            )
            .child(
                h_flex()
                    .gap(HEADER_ACTION_GAP)
                    .child(filter_toggle)
                    .child(collapse_toggle)
                    .child(reveal_active),
            )
    }

    /// A quiet, centered, muted placeholder filling the tree body in place
    /// of the row list — shared by the loading/empty-root states
    /// (`docs/spec-explorer-parity.md`) and the filter bar's "No matches"
    /// state (`docs/spec-explorer-search.md`, #679).
    fn render_placeholder(message: &'static str, cx: &Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.0))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(message)
            .into_any_element()
    }

    /// The in-panel filter bar (artboard **State B**,
    /// `docs/spec-explorer-search.md`, #679): a `gpui-component` `Input` in
    /// the header→tree seam Phase 27 reserved, reusing the shipped
    /// `InputState`+`Input` pattern (`connection_screen.rs`/`editor.rs`).
    /// Mounted in [`Render::render`] only while `filter_active`; `None` on
    /// `filter_input` at that point is unreachable (`open_filter` always
    /// sets both together) but handled as an empty row rather than
    /// `.expect()`-ing an invariant an unrelated future refactor could
    /// silently break.
    fn render_filter_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(input) = self.filter_input.as_ref() else {
            return div().into_any_element();
        };

        div()
            .flex_shrink_0()
            .px(HEADER_PADDING_LEFT)
            .py(px(6.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Input::new(input).small().w_full().cleanable(true).prefix(
                    Icon::new(IconName::Search)
                        .size(px(12.0))
                        .text_color(cx.theme().muted_foreground),
                ),
            )
            .into_any_element()
    }

    /// The workspace-root row (`RIFT` in the design) below the header
    /// (`docs/spec-explorer-polish.md`, #710 — REVISES the chevron-driven
    /// root row `docs/spec-explorer-redesign.md` shipped): the leaf of
    /// `model.root()`'s absolute path, uppercased and bold at the artboard's
    /// 12px label style, rendered fully left-aligned — no chevron, no
    /// reserved icon/chevron slot, as the project root — at the row's
    /// existing height and background tint (shared with
    /// [`FileTree::render_row`]'s [`ROW_HEIGHT`]), which gives the row a
    /// subtle band against the panel surface. Clicking it drives the
    /// whole-tree collapse ([`FileTree::toggle_root_collapsed`]), distinct
    /// from the header's collapse-all/expand-all
    /// ([`FileTree::toggle_collapse_all`]). Neutral — no label, not
    /// clickable — while `root()` is `None`: no snapshot has arrived yet, so
    /// there is nothing to name or toggle.
    fn render_root_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.model.root() else {
            return h_flex()
                .flex_shrink_0()
                .items_center()
                .h(ROW_HEIGHT)
                .px(ROOT_ROW_PADDING_X)
                .bg(cx.theme().background)
                .into_any_element();
        };

        let label = Self::root_leaf(root).to_uppercase();

        h_flex()
            .id("file-tree-root-row")
            .flex_shrink_0()
            .items_center()
            .h(ROW_HEIGHT)
            .px(ROOT_ROW_PADDING_X)
            .bg(cx.theme().background)
            .text_size(px(12.0))
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().list_hover))
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .text_color(cx.theme().foreground)
                    .child(label),
            )
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.toggle_root_collapsed();
                cx.notify();
            }))
            .into_any_element()
    }

    /// Whether `row`'s rolled-up decoration (git letter + diagnostic dot)
    /// should render. Files, and a currently *collapsed* directory, always
    /// show it; an *expanded* directory shows nothing rolled-up — its visible
    /// descendants already carry their own, so an ancestor badge would be
    /// redundant (`docs/spec-explorer-parity.md`). Render-time gate only:
    /// `compute_rollup` still folds the status onto every directory
    /// regardless of its collapse state, so toggling collapse (which already
    /// dirties the row cache) flips this on the very next render.
    fn row_shows_decoration(&self, row: &Row) -> bool {
        row.kind != EntryKind::Dir || self.collapsed.contains(&row.path)
    }

    /// Horizontal indent for a row at `depth`, from the "Explorer —
    /// Redesign" artboard's indent lanes: an 8px base (depth 0) plus 16px
    /// per level, yielding 8/24/40/56 for depths 0-3 — replacing the shipped
    /// flat `depth * 14px` (no base).
    fn row_indent(depth: usize) -> Pixels {
        INDENT_BASE + px(depth as f32 * INDENT_PER_LEVEL)
    }

    /// The left offsets of the indent guide lines a row at `depth` renders —
    /// one per ancestor lane the row is nested under, 1:1 with
    /// [`FileTree::row_indent`]'s lanes for level `0..depth` (a row's own
    /// lane, where its icon sits, gets no guide — only the lanes it is
    /// nested *inside*). A pure function, so the alignment to Phase 27's
    /// indent geometry is unit-testable without a render pass
    /// (`docs/spec-explorer-polish.md`, #711).
    fn indent_guide_positions(depth: usize) -> Vec<Pixels> {
        (0..depth).map(Self::row_indent).collect()
    }

    /// Render `depth` thin vertical guide lines in the row's leading indent
    /// region, one per nesting level, at [`FileTree::indent_guide_positions`]
    /// (`docs/spec-explorer-polish.md`, #711 — Zed-style legibility for a run
    /// of nested/expanded levels). Each line spans the full row height and is
    /// tinted the theme's `border` role — a subtle, muted role rather than a
    /// second accent — so it reads as a faint lane divider and re-tints on a
    /// theme switch (never a hardcoded hex). Absolutely positioned: callers
    /// must set `.relative()` on the row container so these children
    /// position against the row's own bounds, not the row's `pl(indent)`
    /// content offset.
    fn render_indent_guides(depth: usize, cx: &Context<Self>) -> Vec<Div> {
        let color = cx.theme().border;
        Self::indent_guide_positions(depth)
            .into_iter()
            .map(|left| {
                div()
                    .absolute()
                    .left(left)
                    .top_0()
                    .h_full()
                    .w(INDENT_GUIDE_WIDTH)
                    .bg(color)
            })
            .collect()
    }

    /// Render one row as an interactive element. Clicking a directory selects
    /// it and toggles its expansion; clicking a file selects it and emits the
    /// open signal.
    ///
    /// The row container is `w_full` (`docs/spec-explorer-polish.md`, #711 —
    /// REVISES the shipped content-width row): the hover/selected/multi-select
    /// background surface now spans the full panel edge-to-edge, while the
    /// slots below keep their per-depth indent via `pl(indent)`. The row also
    /// carries [`FileTree::render_indent_guides`] as absolutely positioned
    /// children, one thin lane-divider line per ancestor depth.
    ///
    /// Slot order, left to right, every slot `flex_shrink_0` so names and the
    /// trailing cluster column-align across rows and depths
    /// (`docs/spec-explorer-polish.md`, #710 — REVISES the chevron slot
    /// `docs/spec-explorer-redesign.md` shipped): reserved icon slot (the
    /// mapped file-type glyph, or the open/closed folder glyph
    /// [`file_icons::folder_icon_for`] — the sole disclosure affordance for a
    /// directory row, `crate::file_icons`) -> name (the only flexible slot)
    /// -> diagnostic dot -> right-aligned git-status letter. The chevron
    /// twisty and its reserved slot, and the blank chevron spacer files used
    /// to reserve, are gone — every row shifts left by that reclaimed width.
    fn render_row(&self, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let is_dir = row.kind == EntryKind::Dir;
        let is_expanded = is_dir && !self.collapsed.contains(&row.path);
        let is_selected = self.selected.as_deref() == Some(row.path.as_str());
        let indent = Self::row_indent(row.depth);

        // Reserved icon slot: the mapped file-type glyph for a file, or the
        // open/closed folder glyph for a directory (`crate::file_icons`),
        // tinted via the mapped theme-token role — never a hardcoded hex.
        let icon_entry = if is_dir {
            file_icons::folder_icon_for(is_expanded)
        } else {
            file_icons::file_icon_for(Self::display_name(&row.path))
        };
        let icon_tint = icon_entry.tint.resolve(cx.theme());
        let icon_glyph = match icon_entry.glyph {
            Glyph::Svg(path) => Icon::empty().path(path).text_color(icon_tint),
            Glyph::Chrome(chrome) => Icon::new(chrome.icon_name()).text_color(icon_tint),
        };
        let icon_slot = div()
            .w(ICON_SLOT_WIDTH)
            .h(ICON_SLOT_WIDTH)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .child(icon_glyph.size(ICON_SLOT_WIDTH));

        // Inline rename (artboard State C): while this row is the active
        // rename target, its name slot is the seeded input instead of the
        // static label — every other slot (icon, indent, row height) stays
        // identical so the row doesn't jump while editing.
        if let Some(editor) = self
            .rename
            .as_ref()
            .filter(|editor| editor.path == row.path)
        {
            return self
                .render_rename_row(row, indent, icon_slot, editor, cx)
                .into_any_element();
        }

        // Inline create (artboard State D, #676): the transient "New
        // File…"/"New Folder…" row [`insert_create_row`] adds to the cache
        // renders the same way — reusing the identical mechanism, just with
        // no existing path to match against (there is only ever one, flagged
        // by `Row::is_pending_create`).
        if row.is_pending_create {
            if let Some(editor) = self.create.as_ref() {
                return self
                    .render_create_row(row.depth, indent, icon_slot, editor, cx)
                    .into_any_element();
            }
        }

        let name = Self::display_name(&row.path).to_owned();
        let path = row.path.clone();
        let preview_name = SharedString::from(name.clone());

        // Rolled-up decoration (git letter + diagnostic dot) is suppressed on
        // an *expanded* directory: its visible descendants already carry
        // their own, so an ancestor badge would be redundant noise (design:
        // collapsed `app` shows `M`, expanded `crates`/`terminal`/`src` do
        // not). Files, and a currently collapsed directory, always show it.
        let show_decoration = self.row_shows_decoration(row);

        // Diagnostic-severity indicator: a small colored dot, always
        // reserving its slot width so the trailing git-letter lane
        // column-aligns even on a clean row (an empty, uncolored dot rather
        // than a spacer div — the artboard's re-spacing).
        let severity_dot = div()
            .size(DIAGNOSTIC_DOT_SIZE)
            .flex_shrink_0()
            .rounded_full()
            .when_some(row.severity.filter(|_| show_decoration), |el, severity| {
                let color = match severity {
                    DiagnosticSeverity::Error => cx.theme().danger,
                    DiagnosticSeverity::Warning => cx.theme().warning,
                    DiagnosticSeverity::Information => cx.theme().info,
                    DiagnosticSeverity::Hint => cx.theme().muted_foreground,
                };
                el.bg(color)
            });

        // Git-status color: tints the name itself, mirroring the roll-up
        // precedence (`conflicted > changed > untracked`).
        let git_color = row.git_status.map(|status| match status {
            GitRollupStatus::Conflicted => cx.theme().danger,
            GitRollupStatus::Changed => cx.theme().warning,
            GitRollupStatus::Untracked => cx.theme().success,
        });
        // Match emphasis (artboard **State B**, `docs/spec-explorer-search.md`,
        // #679): with an active filter, `row.matched_indices` splits `name`
        // into alternating spans ([`emphasis_segments`]) and tints the matched
        // runs with the accent-tint theme token — never a hardcoded hex,
        // mirroring `results_panel.rs`'s search-match highlight. Empty
        // `matched_indices` (no filter, or an ancestor-of-a-match directory
        // row) yields a single unmatched span, identical to the plain `name`
        // child this replaces.
        // No bold on selection (#729): a bolded name widens the glyph run and
        // shifts the row's trailing slots rightward relative to its
        // unselected siblings — a visible layout jump. Selected and
        // unselected rows now share identical text metrics; legibility comes
        // from the brighter `secondary_active` fill + inset accent bar below
        // instead of weight.
        let name_el = h_flex()
            .flex_1()
            .when_some(git_color, |el, color| el.text_color(color))
            .children(
                emphasis_segments(&name, &row.matched_indices)
                    .into_iter()
                    .map(|(text, matched)| {
                        let mut span = div().flex_none().child(text);
                        if matched {
                            span = span
                                .text_color(cx.theme().accent_foreground)
                                .bg(cx.theme().accent.opacity(0.25))
                                .rounded(px(2.0));
                        }
                        span
                    }),
            );

        // Git-status letter: a single glyph in a fixed-width, right-aligned,
        // centered trailing lane, colored the same as the name tint.
        // `name_el`'s `flex_1` fills the row's remaining width, so this
        // fixed-width slot always lands at the same trailing offset —
        // letters column-align across rows regardless of name length or
        // indent depth. Always reserves its width, same discipline as the
        // diagnostic dot above.
        let git_letter = div()
            .w(GIT_LETTER_SLOT_WIDTH)
            .flex_shrink_0()
            .text_xs()
            .text_center()
            .when_some(row.git_status.filter(|_| show_decoration), |el, status| {
                el.text_color(match status {
                    GitRollupStatus::Conflicted => cx.theme().danger,
                    GitRollupStatus::Changed => cx.theme().warning,
                    GitRollupStatus::Untracked => cx.theme().success,
                })
                .child(status.badge())
            });

        // The row's element id is its path: unique within the tree (the model
        // keys entries by path), so it makes a stable per-row id without a
        // running index that would shift as rows scroll in and out.
        let mut root = div()
            .id(gpui::SharedString::from(row.path.clone()))
            .relative()
            .w_full()
            .flex()
            .items_center()
            .h(ROW_HEIGHT)
            .py(ROW_BLOCK_PADDING_Y)
            .pl(indent)
            .pr(px(8.0))
            .gap(ROW_SLOT_GAP)
            .rounded(ROW_RADIUS)
            .text_sm()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().list_hover))
            .children(Self::render_indent_guides(row.depth, cx))
            // Right mouse-down selects the row so the context menu's unit
            // actions below target it. This listener is attached to the row
            // itself, so it paints (and registers) before `ContextMenuExt`'s
            // own right-click listener, which is registered on the wrapping
            // `ContextMenu` element's *paint*, after this row's paint runs —
            // the selection lands before the menu's deferred build reads it.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let path = path.clone();
                    move |this, _event: &MouseDownEvent, _window, cx| {
                        this.selected = Some(path.clone());
                        this.cache_dirty = true;
                        cx.notify();
                    }
                }),
            )
            // Ignored entries (not yet shown by default — #309) render dimmed
            // rather than hidden, once the daemon starts sending them.
            .when(row.ignored, |el| el.opacity(0.55))
            .child(icon_slot)
            .child(name_el)
            .child(severity_dot)
            .child(git_letter);

        // Selection fill: the cursor row keeps Phase 27's inset accent bar +
        // active-surface tint; a multi-selected row that is *not* the cursor
        // gets the artboard's discrete flat-surface fill instead (a neutral
        // `secondary` theme role, distinct from `list_active`/`accent` — no
        // accent bar, `docs/spec-explorer-search.md`, Phase 31, #680). The
        // cursor's own treatment always wins when a row is both.
        //
        // The cursor fill is `secondary_active` rather than `list_active`
        // (#729): a step brighter than both `list_active` (hover/drag-over)
        // and the multi-select `secondary` fill above, compensating for the
        // now-removed bold so the selected row still reads as legibly
        // distinct.
        if is_selected {
            root = root
                .bg(cx.theme().secondary_active)
                .border_l_2()
                .border_color(cx.theme().accent)
                .text_color(cx.theme().foreground);
        } else if self.selection.contains(&row.path) {
            root = root.bg(cx.theme().secondary);
        }

        // Drag & drop move (`docs/spec-explorer-file-ops.md`, #677): every
        // row is both a drag source (`on_drag`, a themed floating preview
        // via `DragPreview`) and a drop target (`on_drop`, resolved through
        // `FileTree::handle_drop` -> `resolve_drop`). `can_drop` mirrors
        // `resolve_drop`'s own guard (a no-op same-parent move, or a
        // directory dropped into itself or a descendant) so a refused target
        // never highlights (`drag_over`) either — the same check `on_drop`
        // re-applies before ever emitting `RenameRequested`, so a
        // highlighted target is always one that would actually send.
        let target_kind = row.kind.clone();
        let drag_payload = DraggedRow {
            path: path.clone(),
            kind: target_kind.clone(),
        };
        let can_drop_kind = target_kind.clone();
        let can_drop_path = path.clone();
        let drop_kind = target_kind.clone();
        let drop_path = path.clone();
        root = root
            .on_drag(drag_payload, move |_drag, _point, _window, cx| {
                cx.new(|_| DragPreview {
                    name: preview_name.clone(),
                })
            })
            .drag_over::<DraggedRow>(|style, _drag, _window, cx| style.bg(cx.theme().list_active))
            .can_drop(move |drag: &dyn std::any::Any, _window, _cx| {
                drag.downcast_ref::<DraggedRow>().is_some_and(|dragged| {
                    resolve_drop(&dragged.path, &dragged.kind, &can_drop_kind, &can_drop_path)
                        .is_some()
                })
            })
            .on_drop(cx.listener(move |this, drag: &DraggedRow, _window, cx| {
                this.handle_drop(drag, &drop_kind, &drop_path, cx);
            }));

        // Row context menu (`docs/spec-explorer-context-menu.md`): reuses
        // `gpui-component`'s `ContextMenuExt`/`PopupMenu` — the same widget
        // `editor.rs` already ships its right-click menu with. Label-only (no
        // `IconName`: the product binary does not embed `gpui-component`'s
        // icon assets, see the module doc). Lists the client-capable top
        // group in artboard order, then the write group (artboard **State
        // D**, `docs/spec-explorer-file-ops.md`, #676) behind a separator:
        // *New File…* / *New Folder…* open the create editor targeting this
        // row (`FileTree::start_create`); *Rename* reuses the shipped
        // `StartRename` action (State C), same as "Open" reuses
        // `OpenSelected`; *Delete* opens the destructive confirm dialog.
        //
        // Discrete multi-select (`docs/spec-explorer-search.md`, Phase 31,
        // #680): `event.modifiers()` distinguishes a plain click from
        // `Ctrl`/`Cmd+Click` (toggle) and `Shift+Click` (range) before
        // falling back to the plain dir-toggle / file-open behavior above.
        root.on_click(cx.listener(move |this, event: &ClickEvent, _window, cx| {
            let modifiers = event.modifiers();
            if modifiers.secondary() {
                this.toggle_selection(&path);
            } else if modifiers.shift {
                this.range_select(&path);
            } else if is_dir {
                this.click_dir(&path);
            } else {
                // The open signal the editor surface consumes. Selecting a file
                // is the only thing that touches anything outside this view — and
                // it touches nothing but this event; no tmux pane/window state.
                this.click_file(&path, cx);
            }
            cx.notify();
        }))
        .context_menu(|menu: PopupMenu, _window, _cx| {
            menu.menu("Open", Box::new(OpenSelected))
                .menu("Reveal in tree", Box::new(RevealInTree))
                .menu("Copy path", Box::new(CopyAbsolutePath))
                .menu("Copy relative path", Box::new(CopyRelativePath))
                .menu("Reveal in terminal", Box::new(RevealInTerminal))
                .menu("Collapse all", Box::new(CollapseAll))
                .separator()
                .menu("New File...", Box::new(NewFile))
                .menu("New Folder...", Box::new(NewFolder))
                .menu("Rename", Box::new(StartRename))
                .menu("Delete", Box::new(DeleteSelected))
        })
        .into_any_element()
    }

    /// The rename-active rendering of one row (artboard State C): icon slot
    /// unchanged (no chevron, `docs/spec-explorer-polish.md`, #710), the name
    /// slot replaced by `editor.input`, and — on an error re-open — the
    /// inline message in place of the diagnostic-dot / git-letter trailing
    /// lane. No click / context-menu handlers: the row is not selectable or
    /// openable while its name is being edited. Shares [`FileTree::render_row`]'s
    /// indent guides (`docs/spec-explorer-polish.md`, #711) so the leading
    /// region reads identically while a row is mid-rename.
    fn render_rename_row(
        &self,
        row: &Row,
        indent: Pixels,
        icon_slot: Div,
        editor: &RenameEditor,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let trailing = editor.error.as_ref().map(|message| {
            div()
                .flex_none()
                .max_w(px(120.0))
                .truncate()
                .text_xs()
                .text_color(cx.theme().danger)
                .child(SharedString::from(message.clone()))
        });

        div()
            .id(SharedString::from(format!("{}-rename", row.path)))
            .relative()
            .flex()
            .items_center()
            .h(ROW_HEIGHT)
            .py(ROW_BLOCK_PADDING_Y)
            .pl(indent)
            .pr(px(8.0))
            .gap(ROW_SLOT_GAP)
            .rounded(ROW_RADIUS)
            .text_sm()
            .children(Self::render_indent_guides(row.depth, cx))
            .child(icon_slot)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&editor.input).small()),
            )
            .children(trailing)
    }

    /// The rendering of the transient create row [`insert_create_row`] adds
    /// to the cache (artboard **State D**, #676): icon slot unchanged (no
    /// chevron, `docs/spec-explorer-polish.md`, #710 — the icon reflects
    /// `editor.kind` via the synthetic row's own `kind`), the name slot is
    /// `editor.input`, and — on an error re-open — the inline message in the
    /// trailing lane. Mirrors [`FileTree::render_rename_row`]; no click /
    /// context-menu handlers, matching that row's "not yet a real entry"
    /// affordance. Shares [`FileTree::render_row`]'s indent guides
    /// (`docs/spec-explorer-polish.md`, #711).
    fn render_create_row(
        &self,
        depth: usize,
        indent: Pixels,
        icon_slot: Div,
        editor: &CreateEditor,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let trailing = editor.error.as_ref().map(|message| {
            div()
                .flex_none()
                .max_w(px(120.0))
                .truncate()
                .text_xs()
                .text_color(cx.theme().danger)
                .child(SharedString::from(message.clone()))
        });

        div()
            .id("file-tree-create-row")
            .relative()
            .flex()
            .items_center()
            .h(ROW_HEIGHT)
            .py(ROW_BLOCK_PADDING_Y)
            .pl(indent)
            .pr(px(8.0))
            .gap(ROW_SLOT_GAP)
            .rounded(ROW_RADIUS)
            .text_sm()
            .children(Self::render_indent_guides(depth, cx))
            .child(icon_slot)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&editor.input).small()),
            )
            .children(trailing)
    }
}

impl Default for FileTree {
    fn default() -> Self {
        Self::new()
    }
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl Focusable for FileTree {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.focus_handle
            .borrow_mut()
            .get_or_insert_with(|| cx.focus_handle())
            .clone()
    }
}

impl EventEmitter<PanelEvent> for FileTree {}

impl Panel for FileTree {
    fn panel_name(&self) -> &'static str {
        FILE_TREE_PANEL_NAME
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Explorer")
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

impl Render for FileTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Rebuild the row cache once for this paint if the model, a collapse,
        // or a selection changed since the last render (see `cache_dirty`'s
        // doc); a no-op otherwise. Both the size vector below and the virtual
        // list's row closure read `row_cache` from here on — the freeze fix:
        // this used to run `visible_rows()` once here and again inside the
        // closure, doubling the tree walk on every single paint.
        self.refresh_row_cache();

        // Empty state: distinguish "no snapshot yet" from "connected, empty
        // root" (`docs/spec-explorer-parity.md`) — a single conflated "No
        // files" branch used to read as an error during ordinary startup.
        // Restyled to the redesign's rhythm (`docs/spec-explorer-redesign.md`):
        // still quiet, centered, muted, and carrying no action surface — just
        // more generous breathing room than the shipped 8px, matching the
        // redesign's less cramped density. A third case, checked once a
        // snapshot exists: an active, non-empty filter query the row cache
        // has zero matches for (`docs/spec-explorer-search.md`, #679) shows
        // the same quiet placeholder rather than an empty tree body that
        // could read as an error.
        let content = if let Some(state) = self.empty_state() {
            let message = match state {
                EmptyState::Loading => "Loading\u{2026}",
                EmptyState::EmptyRoot => "Empty folder",
            };
            Self::render_placeholder(message, cx)
        } else if self.filter_active && !self.filter_query.is_empty() && self.row_cache.is_empty() {
            Self::render_placeholder("No matches", cx)
        } else {
            // One uniform row height per item — the size vector the virtual
            // list measures against. Width is ignored for a vertical list.
            let item_sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
                self.row_cache
                    .iter()
                    .map(|_| Size::new(px(0.0), ROW_HEIGHT))
                    .collect(),
            );

            div()
                .size_full()
                .child(
                    v_virtual_list(
                        cx.entity().clone(),
                        "file-tree",
                        item_sizes,
                        move |this, visible_range, _window, cx| {
                            // Read the cache built above — the virtual list only
                            // asks for the rows currently on screen, so a huge tree
                            // still paints a bounded number of elements, but no
                            // tree walk happens here: `row_cache` is already fresh.
                            let this: &Self = this;
                            visible_range
                                .filter_map(|ix| {
                                    this.row_cache.get(ix).map(|row| this.render_row(row, cx))
                                })
                                .map(IntoElement::into_any_element)
                                .collect::<Vec<_>>()
                        },
                    )
                    .track_scroll(&self.scroll_handle),
                )
                .into_any_element()
        };

        // Root: establishes the tree's key context and focus tracking so the
        // navigation actions below fire only while the tree is focused — the
        // same scoping pattern as the editor's `EDITOR_KEY_CONTEXT` (never
        // steals a keystroke the terminal panel would otherwise receive,
        // since GPUI dispatches actions along the currently *focused*
        // element's context chain, and the terminal panel is a focus-tracked
        // sibling, not an ancestor, of this one). `flex_col` stacks the header
        // band and workspace-root row (both `flex_shrink_0`, fixed height)
        // above the scrollable content, which claims the remaining space via
        // `flex_1` (`min_h_0` so the virtual list's own sizing cannot push the
        // fixed rows off — the same shape `problems_panel.rs`'s summary bar
        // uses above its list).
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context(FILE_TREE_KEY_CONTEXT)
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(|this, _: &SelectUp, _window, cx| {
                this.select_up();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectDown, _window, cx| {
                this.select_down();
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &CollapseOrSelectParent, _window, cx| {
                    this.collapse_or_select_parent();
                    cx.notify();
                }),
            )
            .on_action(cx.listener(|this, _: &ExpandOrSelectChild, _window, cx| {
                this.expand_or_select_child();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSelected, _window, cx| {
                this.open_selected(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _window, cx| {
                this.select_first();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SelectLast, _window, cx| {
                this.select_last();
                cx.notify();
            }))
            // Discrete multi-select keyboard extension (`Shift+Up`/
            // `Shift+Down`, `docs/spec-explorer-search.md`, Phase 31, #680).
            .on_action(cx.listener(|this, _: &ExtendSelectionUp, _window, cx| {
                this.extend_selection_up();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionDown, _window, cx| {
                this.extend_selection_down();
                cx.notify();
            }))
            // Row context-menu actions (`docs/spec-explorer-context-menu.md`):
            // dispatched by the `PopupMenu` built in `render_row`, handled
            // here so they stay inside `FILE_TREE_KEY_CONTEXT` like every
            // other tree action — no key binding is added in `main.rs`. Each
            // targets `self.selected` (set by the row's right mouse-down
            // listener); a missing selection or root is a no-op, matching the
            // tree's "render / act only on what the model carries" discipline.
            .on_action(cx.listener(|this, _: &RevealInTree, _window, cx| {
                if let Some(selected) = this.selected.clone() {
                    this.reveal(&selected);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CopyAbsolutePath, _window, cx| {
                if let (Some(root), Some(selected)) =
                    (this.model.root().map(str::to_owned), this.selected.clone())
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(absolute_path(
                        &root, &selected,
                    )));
                }
            }))
            .on_action(cx.listener(|this, _: &CopyRelativePath, _window, cx| {
                if let Some(selected) = this.selected.clone() {
                    cx.write_to_clipboard(ClipboardItem::new_string(selected));
                }
            }))
            .on_action(cx.listener(|this, _: &RevealInTerminal, _window, cx| {
                if let (Some(root), Some(selected)) =
                    (this.model.root().map(str::to_owned), this.selected.clone())
                {
                    if let Some(entry) = this.model.get(&selected) {
                        let dir = reveal_in_terminal_dir(&root, &entry.kind, &selected);
                        cx.emit(FileTreeEvent::RevealInTerminalRequested { dir });
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &CollapseAll, _window, cx| {
                this.collapse_all();
                cx.notify();
            }))
            // Inline rename (artboard State C, `docs/spec-explorer-file-ops.md`):
            // `F2` (bound in `main.rs`) opens the editor for the selected row.
            .on_action(cx.listener(|this, _: &StartRename, window, cx| {
                this.start_rename(window, cx);
            }))
            // Context-menu write group (artboard State D,
            // `docs/spec-explorer-file-ops.md`, #676): `NewFile`/`NewFolder`
            // open the create editor under the selected row's target
            // directory; `DeleteSelected` opens the destructive confirm
            // dialog. Dispatched only by the row context menu — no key
            // binding in `main.rs`, matching `RevealInTree`/`CollapseAll`.
            .on_action(cx.listener(|this, _: &NewFile, window, cx| {
                this.start_create(EntryKind::File, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NewFolder, window, cx| {
                this.start_create(EntryKind::Dir, window, cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteSelected, window, cx| {
                this.confirm_delete(window, cx);
            }))
            // `Escape` while the rename, create, or filter input has focus:
            // `InputState::escape` propagates the action (it is not in
            // `clean_on_escape` mode), so it bubbles here to close whichever
            // editor is open with no send — the filter bar clears+closes
            // (`docs/spec-explorer-search.md`, #679). A no-op (and
            // re-propagated) when none is active, so an ancestor gets a
            // chance at a plain Escape too.
            .on_action(cx.listener(|this, _: &Escape, _window, cx| {
                if this.rename.is_some() {
                    this.cancel_rename();
                    cx.notify();
                } else if this.create.is_some() {
                    this.cancel_create();
                    cx.notify();
                } else if this.filter_active {
                    this.close_filter(cx);
                } else {
                    cx.propagate();
                }
            }))
            .child(self.render_header(cx))
            .when(self.filter_active, |el| {
                el.child(self.render_filter_bar(cx))
            })
            .child(self.render_root_row(cx))
            .child(div().flex_1().min_h_0().child(content))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_protocol::WorktreeEntry;
    use std::time::SystemTime;

    fn entry(path: &str, kind: EntryKind) -> WorktreeEntry {
        WorktreeEntry {
            path: path.to_owned(),
            kind,
            ignored: false,
            mtime: SystemTime::UNIX_EPOCH,
        }
    }

    fn file(path: &str) -> WorktreeEntry {
        entry(path, EntryKind::File)
    }

    fn dir(path: &str) -> WorktreeEntry {
        entry(path, EntryKind::Dir)
    }

    /// Seed a tree's model directly with a single complete snapshot.
    fn seed(entries: Vec<WorktreeEntry>) -> FileTree {
        let mut tree = FileTree::new();
        tree.model_mut()
            .apply_snapshot_chunk("/proj".into(), entries, true);
        tree
    }

    #[test]
    fn test_visible_rows_annotate_depth_from_path() {
        let tree = seed(vec![
            dir("src"),
            file("src/main.rs"),
            file("src/lib.rs"),
            file("README.md"),
        ]);

        let rows = tree.visible_rows();
        // BTreeMap order: README.md, src, src/lib.rs, src/main.rs.
        let by_path: Vec<(&str, usize)> = rows.iter().map(|r| (r.path.as_str(), r.depth)).collect();
        assert_eq!(
            by_path,
            vec![
                ("README.md", 0),
                ("src", 0),
                ("src/lib.rs", 1),
                ("src/main.rs", 1),
            ]
        );
    }

    #[test]
    fn test_collapsing_a_dir_hides_its_whole_subtree() {
        let mut tree = seed(vec![
            dir("src"),
            dir("src/net"),
            file("src/net/tcp.rs"),
            file("src/main.rs"),
            file("top.rs"),
        ]);

        // Expanded: every entry is visible.
        assert_eq!(tree.visible_rows().len(), 5);

        // Collapse `src`: src stays, but everything under `src/` disappears.
        tree.toggle_dir("src");
        let rows = tree.visible_rows();
        let visible: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(visible, vec!["src", "top.rs"]);
        assert!(tree.is_collapsed("src"));

        // Re-expand: the subtree returns in full.
        tree.toggle_dir("src");
        assert_eq!(tree.visible_rows().len(), 5);
        assert!(!tree.is_collapsed("src"));
    }

    #[test]
    fn test_collapse_is_prefix_exact_not_substring() {
        // `src` collapsed must not accidentally hide a sibling like `src2` whose
        // path shares the `src` text prefix but not the `src/` path prefix.
        let mut tree = seed(vec![
            dir("src"),
            file("src/a.rs"),
            dir("src2"),
            file("src2/b.rs"),
        ]);

        tree.toggle_dir("src");
        let rows = tree.visible_rows();
        let visible: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(visible, vec!["src", "src2", "src2/b.rs"]);
    }

    #[test]
    fn test_nested_collapse_skips_only_the_outer_subtree_once() {
        // A collapsed outer directory hides inner directories too, even though
        // they are also collapsible — the outer skip subsumes them.
        let mut tree = seed(vec![
            dir("a"),
            dir("a/b"),
            file("a/b/c.rs"),
            file("a/d.rs"),
            file("z.rs"),
        ]);

        tree.toggle_dir("a/b");
        tree.toggle_dir("a");
        let rows = tree.visible_rows();
        let visible: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(visible, vec!["a", "z.rs"]);
    }

    #[test]
    fn test_display_name_is_the_leaf_segment() {
        assert_eq!(FileTree::display_name("src/net/tcp.rs"), "tcp.rs");
        assert_eq!(FileTree::display_name("README.md"), "README.md");
        assert_eq!(FileTree::display_name("src"), "src");
    }

    // --- row anatomy / density: reserved icon slot, re-spaced trailing
    // cluster, redesigned rhythm (#653, `docs/spec-explorer-redesign.md`) ---

    #[test]
    fn test_row_indent_follows_the_artboards_8_24_40_56_lanes() {
        assert_eq!(FileTree::row_indent(0), px(8.0));
        assert_eq!(FileTree::row_indent(1), px(24.0));
        assert_eq!(FileTree::row_indent(2), px(40.0));
        assert_eq!(FileTree::row_indent(3), px(56.0));
    }

    // --- indent guide lines (#711, `docs/spec-explorer-polish.md`) ---

    #[test]
    fn test_indent_guide_positions_with_zero_depth_is_empty() {
        // A top-level row is nested under no ancestor lane, so it renders no
        // guide line at all.
        assert_eq!(FileTree::indent_guide_positions(0), Vec::<Pixels>::new());
    }

    #[test]
    fn test_indent_guide_positions_align_1_to_1_with_the_indent_lanes() {
        // One guide per ancestor lane the row is nested under (levels
        // `0..depth`), each matching `row_indent`'s lane for that level — the
        // row's own lane (where its icon sits) gets no guide.
        assert_eq!(FileTree::indent_guide_positions(1), vec![px(8.0)]);
        assert_eq!(FileTree::indent_guide_positions(2), vec![px(8.0), px(24.0)]);
        assert_eq!(
            FileTree::indent_guide_positions(3),
            vec![px(8.0), px(24.0), px(40.0)]
        );
    }

    #[test]
    fn test_indent_guide_count_matches_depth() {
        for depth in 0..6 {
            assert_eq!(FileTree::indent_guide_positions(depth).len(), depth);
        }
    }

    #[test]
    fn test_row_density_constants_match_the_redesigned_artboard() {
        // A grep-friendly lock on the redesigned density: layout pixels, not
        // theme tokens, replacing the shipped 22px row / 14px-per-level
        // indent / no-base indent / no-radius rows.
        assert_eq!(ROW_HEIGHT, px(28.0));
        assert_eq!(ROW_BLOCK_PADDING_Y, px(4.0));
        assert_eq!(ROW_RADIUS, px(5.0));
        assert_eq!(ROW_SLOT_GAP, px(6.0));
        assert_eq!(INDENT_BASE, px(8.0));
        assert_eq!(INDENT_PER_LEVEL, 16.0);
        assert_eq!(INDENT_GUIDE_WIDTH, px(1.0));
        assert_eq!(ICON_SLOT_WIDTH, px(14.0));
        assert_eq!(DIAGNOSTIC_DOT_SIZE, px(7.0));
        assert_eq!(GIT_LETTER_SLOT_WIDTH, px(12.0));
    }

    // --- header band + action row, workspace-root row density (#654,
    // `docs/spec-explorer-redesign.md`) ---

    #[test]
    fn test_header_and_root_row_constants_match_the_redesigned_artboard() {
        // A grep-friendly lock on the redesigned chrome: layout pixels, not
        // theme tokens, replacing the shipped 28px header (which matched the
        // status line, not the artboard) and the 8px/4px header and root-row
        // padding/gap.
        assert_eq!(HEADER_HEIGHT, px(38.0));
        assert_eq!(HEADER_PADDING_LEFT, px(14.0));
        assert_eq!(HEADER_PADDING_RIGHT, px(12.0));
        assert_eq!(HEADER_ACTION_GAP, px(12.0));
        assert_eq!(ROOT_ROW_PADDING_X, px(12.0));
        // Still the row-anatomy step's slot gap, shared with `render_row`'s
        // tree rows — the root row itself has a single child since the
        // chevron-less redesign (`docs/spec-explorer-polish.md`, #710), so
        // it no longer applies the gap.
        assert_eq!(ROW_SLOT_GAP, px(6.0));
    }

    #[test]
    fn test_empty_state_with_no_snapshot_yet_is_loading() {
        // A fresh tree has never received a snapshot, so `model.root()` is
        // still `None` — startup / connecting, not a genuinely empty root.
        let tree = FileTree::new();
        assert_eq!(tree.empty_state(), Some(EmptyState::Loading));
    }

    #[test]
    fn test_empty_state_with_an_empty_root_snapshot_is_empty_root() {
        // A complete snapshot arrived (`root()` is `Some`) but it carried no
        // entries — a genuinely empty root, distinct from still loading.
        let tree = seed(vec![]);
        assert!(tree.model().root().is_some());
        assert!(tree.model().is_empty());
        assert_eq!(tree.empty_state(), Some(EmptyState::EmptyRoot));
    }

    #[test]
    fn test_empty_state_with_populated_entries_is_none() {
        let tree = seed(vec![file("README.md")]);
        assert_eq!(tree.empty_state(), None);
    }

    #[test]
    fn test_new_tree_starts_with_a_dirty_cache() {
        // Nothing has been rendered yet, so the very first `refresh_row_cache`
        // must not be skipped as "already fresh".
        let tree = FileTree::new();
        assert!(tree.cache_dirty);
        assert!(tree.row_cache.is_empty());
    }

    #[test]
    fn test_refresh_row_cache_builds_rows_and_clears_dirty() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs"), file("README.md")]);
        assert!(
            tree.cache_dirty,
            "seeding via model_mut marks the cache dirty"
        );

        tree.refresh_row_cache();

        assert!(!tree.cache_dirty);
        assert_eq!(tree.row_cache, tree.visible_rows());
    }

    #[test]
    fn test_model_mut_marks_the_cache_dirty_even_with_no_visible_change() {
        let mut tree = seed(vec![file("a.txt")]);
        tree.refresh_row_cache();
        assert!(!tree.cache_dirty);

        // `model_mut` is the only seam that can mutate the model, so it marks
        // dirty unconditionally — a fold behind it can never forget to.
        tree.model_mut();
        assert!(tree.cache_dirty);
    }

    #[test]
    fn test_refresh_row_cache_after_incremental_update_matches_a_fresh_build_no_drift() {
        let mut tree = seed(vec![file("a.txt"), file("stale.txt")]);
        tree.refresh_row_cache();

        // Fold an incremental update through the same seam `workspace.rs` uses.
        tree.model_mut()
            .apply_update(vec![file("fresh.txt")], vec![], vec!["stale.txt".into()]);
        assert!(
            tree.cache_dirty,
            "the update must have marked the cache dirty"
        );

        tree.refresh_row_cache();

        // The refreshed cache must equal an independently fresh build from the
        // now-current model — no drift from the update.
        assert_eq!(tree.row_cache, tree.visible_rows());
        let paths: Vec<&str> = tree.row_cache.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "fresh.txt"]);
    }

    #[test]
    fn test_refresh_row_cache_is_idempotent_while_clean() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs")]);
        tree.refresh_row_cache();
        let built = tree.visible_rows();

        // A second refresh with nothing marked dirty in between must leave the
        // cache exactly as it was.
        tree.refresh_row_cache();
        assert_eq!(tree.row_cache, built);
    }

    #[test]
    fn test_toggle_dir_marks_the_cache_dirty_and_refresh_reflects_the_collapse() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs"), file("top.rs")]);
        tree.refresh_row_cache();
        assert!(!tree.cache_dirty);

        tree.toggle_dir("src");
        assert!(tree.cache_dirty);

        tree.refresh_row_cache();
        let visible: Vec<&str> = tree.row_cache.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(visible, vec!["src", "top.rs"]);
    }

    #[test]
    fn test_click_dir_selects_and_toggles_the_directory() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs"), file("top.rs")]);

        tree.click_dir("src");
        assert_eq!(tree.selected(), Some("src"));
        assert!(tree.is_collapsed("src"));

        // A second click on the still-selected directory re-expands it and
        // leaves the selection unchanged.
        tree.click_dir("src");
        assert_eq!(tree.selected(), Some("src"));
        assert!(!tree.is_collapsed("src"));
    }

    #[test]
    fn test_click_dir_moves_the_selection_off_a_previously_selected_row() {
        // Regression for #481: clicking a directory used to toggle it without
        // touching the selection, so the old selection stuck around and
        // arrow keys resumed from there instead of the row just clicked.
        let mut tree = seed(vec![dir("src"), file("src/main.rs"), file("top.rs")]);
        tree.selected = Some("top.rs".into());

        tree.click_dir("src");

        assert_eq!(tree.selected(), Some("src"));
    }

    // --- header actions: collapse-all / expand-all, root leaf (#604) ---

    #[test]
    fn test_all_dirs_collapsed_is_false_on_a_freshly_seeded_expanded_tree() {
        let tree = seed(vec![dir("src"), file("src/main.rs"), file("top.rs")]);
        assert!(!tree.all_dirs_collapsed());
    }

    #[test]
    fn test_all_dirs_collapsed_is_false_when_a_tree_has_no_directories() {
        // Vacuous truth would wrongly claim "collapsed" for an all-file tree.
        let tree = seed(vec![file("a.txt"), file("b.txt")]);
        assert!(!tree.all_dirs_collapsed());
    }

    #[test]
    fn test_collapse_all_collapses_every_directory_and_marks_all_dirs_collapsed() {
        let mut tree = seed(vec![
            dir("a"),
            dir("a/b"),
            file("a/b/c.rs"),
            dir("z"),
            file("top.rs"),
        ]);
        tree.refresh_row_cache();

        tree.collapse_all();

        assert!(tree.is_collapsed("a"));
        assert!(tree.is_collapsed("a/b"));
        assert!(tree.is_collapsed("z"));
        assert!(tree.all_dirs_collapsed());
        assert!(tree.cache_dirty);
        tree.refresh_row_cache();
        let visible: Vec<&str> = tree.row_cache.iter().map(|r| r.path.as_str()).collect();
        // Top-level entries remain; nested subtrees are hidden.
        assert_eq!(visible, vec!["a", "top.rs", "z"]);
    }

    #[test]
    fn test_expand_all_clears_the_collapsed_set() {
        let mut tree = seed(vec![dir("a"), dir("a/b"), file("a/b/c.rs")]);
        tree.collapse_all();
        assert!(tree.all_dirs_collapsed());

        tree.expand_all();

        assert!(!tree.is_collapsed("a"));
        assert!(!tree.is_collapsed("a/b"));
        assert!(!tree.all_dirs_collapsed());
        assert!(tree.cache_dirty);
    }

    #[test]
    fn test_toggle_collapse_all_flips_between_fully_collapsed_and_fully_expanded() {
        let mut tree = seed(vec![dir("a"), file("a/b.rs"), dir("c")]);

        tree.toggle_collapse_all();
        assert!(tree.all_dirs_collapsed());

        tree.toggle_collapse_all();
        assert!(!tree.all_dirs_collapsed());
        assert!(!tree.is_collapsed("a"));
        assert!(!tree.is_collapsed("c"));
    }

    #[test]
    fn test_collapse_all_preserves_an_already_collapsed_directory_not_reported_by_toggle() {
        // Collapse-all must insert, not toggle: a directory collapsed before
        // the call must stay collapsed, not flip back to expanded.
        let mut tree = seed(vec![dir("a"), file("a/x.rs"), dir("b"), file("b/y.rs")]);
        tree.toggle_dir("a");
        assert!(tree.is_collapsed("a"));

        tree.collapse_all();

        assert!(tree.is_collapsed("a"));
        assert!(tree.is_collapsed("b"));
    }

    // --- workspace-root row whole-tree collapse (`docs/spec-explorer-polish.md`, #710) ---

    #[test]
    fn test_toggle_root_collapsed_hides_all_entries_then_restores_the_exact_prior_tree() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs"), file("top.rs")]);
        let before = tree.visible_rows();
        assert!(!before.is_empty());

        tree.toggle_root_collapsed();
        assert!(
            tree.visible_rows().is_empty(),
            "collapsing the root hides every root-level entry"
        );

        tree.toggle_root_collapsed();
        assert_eq!(
            tree.visible_rows(),
            before,
            "expanding the root restores a build identical to the original"
        );
    }

    #[test]
    fn test_toggle_root_collapsed_never_reads_or_mutates_the_real_collapsed_set() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs")]);
        tree.toggle_dir("src");
        assert!(tree.is_collapsed("src"));

        tree.toggle_root_collapsed();
        assert!(
            tree.is_collapsed("src"),
            "the whole-tree toggle must not touch the per-folder collapsed set"
        );

        tree.toggle_root_collapsed();
        assert!(
            tree.is_collapsed("src"),
            "src is still collapsed once the whole-tree toggle clears"
        );
    }

    #[test]
    fn test_root_collapsed_is_distinct_from_header_collapse_all() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs"), file("top.rs")]);

        // Header collapse-all folds every directory but leaves root-level
        // entries visible.
        tree.collapse_all();
        assert!(tree.all_dirs_collapsed());
        assert!(!tree.visible_rows().is_empty());

        // The whole-tree toggle hides root-level entries too, without
        // disturbing collapse-all's fully-collapsed state.
        tree.toggle_root_collapsed();
        assert!(tree.visible_rows().is_empty());
        assert!(tree.all_dirs_collapsed());
    }

    #[test]
    fn test_toggle_root_collapsed_marks_the_cache_dirty() {
        let mut tree = seed(vec![file("a.txt")]);
        tree.refresh_row_cache();
        assert!(!tree.cache_dirty);

        tree.toggle_root_collapsed();
        assert!(tree.cache_dirty);
    }

    #[test]
    fn test_root_leaf_of_a_nested_absolute_path() {
        assert_eq!(FileTree::root_leaf("/home/user/proj"), "proj");
        assert_eq!(FileTree::root_leaf("/proj"), "proj");
    }

    #[test]
    fn test_root_leaf_of_the_filesystem_root_is_unchanged() {
        assert_eq!(FileTree::root_leaf("/"), "/");
    }

    #[test]
    fn test_reveal_active_requested_variant_is_distinct_from_open_file_under_eq() {
        // The event itself carries no payload; this just locks its `PartialEq`
        // against `OpenFile` so a future refactor cannot silently merge them.
        assert_ne!(
            FileTreeEvent::RevealActiveRequested,
            FileTreeEvent::OpenFile {
                path: "a.rs".into()
            }
        );
    }

    // --- filter bar: narrowing + match emphasis (artboard State B, `docs/spec-explorer-search.md`, #679) ---

    #[test]
    fn test_leaf_matched_indices_rebases_onto_the_leaf_segment() {
        // "main" matches indices [4,5,6,7] in "src/main.rs" (the `m` of
        // `main.rs` starts at char index 4, right after "src/"); rebased
        // onto the leaf "main.rs" alone they become [0,1,2,3].
        assert_eq!(
            leaf_matched_indices("src/main.rs", &[4, 5, 6, 7]),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn test_leaf_matched_indices_drops_indices_in_an_ancestor_segment() {
        // A match landing entirely in the "src/" segment (indices before the
        // leaf's start) contributes nothing to this row's own emphasis —
        // that segment renders on a separate ancestor row, not this one.
        assert_eq!(
            leaf_matched_indices("src/main.rs", &[0, 1, 2]),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn test_leaf_matched_indices_of_a_top_level_path_is_unchanged() {
        // No `/` at all: the whole path is the leaf, so indices pass through.
        assert_eq!(leaf_matched_indices("main.rs", &[0, 1]), vec![0, 1]);
    }

    #[test]
    fn test_emphasis_segments_empty_indices_is_a_single_unmatched_span() {
        assert_eq!(
            emphasis_segments("main.rs", &[]),
            vec![("main.rs".to_owned(), false)]
        );
    }

    #[test]
    fn test_emphasis_segments_splits_matched_and_unmatched_runs() {
        // "main.rs" matched at [0,1,2,3] (`main`) leaves `.rs` unmatched.
        assert_eq!(
            emphasis_segments("main.rs", &[0, 1, 2, 3]),
            vec![("main".to_owned(), true), (".rs".to_owned(), false)]
        );
    }

    #[test]
    fn test_emphasis_segments_handles_non_contiguous_matches() {
        // Scattered match: "m" (0) and "rs" (5,6) of "main.rs".
        assert_eq!(
            emphasis_segments("main.rs", &[0, 5, 6]),
            vec![
                ("m".to_owned(), true),
                ("ain.".to_owned(), false),
                ("rs".to_owned(), true),
            ]
        );
    }

    #[test]
    fn test_visible_rows_filtered_matches_files_and_includes_ancestor_dirs() {
        let mut tree = seed(vec![
            dir("src"),
            file("src/main.rs"),
            file("src/lib.rs"),
            file("README.md"),
        ]);
        tree.filter_query = "main".to_owned();

        let rows = tree.visible_rows();
        let visible: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(visible, vec!["src", "src/main.rs"]);
    }

    #[test]
    fn test_visible_rows_filtered_never_matches_a_directory_by_its_own_name() {
        // An empty directory has no descendant file to match, so even though
        // its own name matches the query, it never appears — only files are
        // matched; a directory row is included solely as a match's ancestor.
        let mut tree = seed(vec![dir("target"), file("other.rs")]);
        tree.filter_query = "target".to_owned();

        assert!(tree.visible_rows().is_empty());
    }

    #[test]
    fn test_visible_rows_filtered_no_match_query_yields_an_empty_row_set() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs")]);
        tree.filter_query = "zzz-nope".to_owned();

        assert!(tree.visible_rows().is_empty());
    }

    #[test]
    fn test_visible_rows_filtered_force_expands_a_collapsed_ancestor_without_mutating_collapsed() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs"), file("top.rs")]);
        tree.toggle_dir("src");
        assert!(tree.is_collapsed("src"));

        tree.filter_query = "main".to_owned();
        let rows = tree.visible_rows();
        let visible: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            visible,
            vec!["src", "src/main.rs"],
            "a match inside a collapsed directory still shows, its ancestor force-expanded"
        );

        // The real collapsed set is untouched by the filtered pass.
        assert!(tree.is_collapsed("src"));
    }

    #[test]
    fn test_visible_rows_filtered_carries_leaf_relative_matched_indices_on_the_file_row_only() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs")]);
        tree.filter_query = "main".to_owned();

        let rows = tree.visible_rows();
        let dir_row = rows
            .iter()
            .find(|r| r.path == "src")
            .expect("ancestor dir row present");
        assert!(
            dir_row.matched_indices.is_empty(),
            "an ancestor directory row carries no emphasis"
        );

        let file_row = rows
            .iter()
            .find(|r| r.path == "src/main.rs")
            .expect("matched file row present");
        assert_eq!(
            file_row.matched_indices,
            vec![0, 1, 2, 3],
            "leaf-relative: main.rs's own `main` is matched"
        );
    }

    #[test]
    fn test_visible_rows_clearing_the_query_restores_the_exact_prior_tree() {
        let mut tree = seed(vec![
            dir("src"),
            dir("src/net"),
            file("src/net/tcp.rs"),
            file("src/main.rs"),
            file("top.rs"),
        ]);
        tree.toggle_dir("src/net");
        let before = tree.visible_rows();

        tree.filter_query = "tcp".to_owned();
        let filtered = tree.visible_rows();
        assert_ne!(filtered, before, "the filtered pass narrows the tree");

        tree.filter_query.clear();
        assert_eq!(
            tree.visible_rows(),
            before,
            "clearing the query restores the exact prior tree"
        );
        assert!(
            tree.is_collapsed("src/net"),
            "the real collapsed set survived the filtered pass untouched"
        );
    }

    #[gpui::test]
    fn test_open_filter_activates_and_seeds_a_blank_focused_input(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.open_filter(window, cx);
                });
            })
            .expect("open filter");

        cx.update(|cx| {
            let tree = tree.read(cx);
            assert!(tree.filter_active);
            assert!(tree.filter_query.is_empty());
            let input = tree.filter_input.as_ref().expect("filter input created");
            assert_eq!(input.read(cx).value().as_ref(), "");
        });
    }

    #[gpui::test]
    fn test_close_filter_clears_the_query_and_deactivates(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.open_filter(window, cx);
                    tree.filter_query = "main".to_owned();
                    tree.close_filter(cx);
                });
            })
            .expect("close filter");

        cx.update(|cx| {
            let tree = tree.read(cx);
            assert!(!tree.filter_active);
            assert!(tree.filter_query.is_empty());
            assert!(tree.filter_input.is_none());
        });
    }

    #[gpui::test]
    fn test_toggle_filter_opens_then_closes(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.toggle_filter(window, cx);
                    assert!(tree.filter_active);

                    tree.toggle_filter(window, cx);
                    assert!(!tree.filter_active);
                });
            })
            .expect("toggle filter");
    }

    // --- git + diagnostic decoration / ancestor roll-up (#329) ---

    use rift_protocol::{
        Diagnostic, DiagnosticSeverity, GitEntryStatus, GitStatusCode, GitStatusEntry, Position,
        Range,
    };

    fn git_entry(path: &str, index: GitStatusCode, worktree: GitStatusCode) -> GitStatusEntry {
        GitStatusEntry {
            path: path.to_owned(),
            status: GitEntryStatus { index, worktree },
        }
    }

    fn diag(severity: DiagnosticSeverity) -> Diagnostic {
        Diagnostic {
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
            severity,
            message: "test".to_owned(),
            source: None,
            code: None,
        }
    }

    #[test]
    fn test_git_rollup_status_from_status_precedence() {
        use GitStatusCode::{Modified, Unmerged, Unmodified, Untracked};

        // Clean on both sides: no decoration.
        assert_eq!(
            GitRollupStatus::from_status(GitEntryStatus {
                index: Unmodified,
                worktree: Unmodified
            }),
            None
        );
        assert_eq!(
            GitRollupStatus::from_status(GitEntryStatus {
                index: Unmodified,
                worktree: Untracked
            }),
            Some(GitRollupStatus::Untracked)
        );
        assert_eq!(
            GitRollupStatus::from_status(GitEntryStatus {
                index: Modified,
                worktree: Unmodified
            }),
            Some(GitRollupStatus::Changed)
        );
        assert_eq!(
            GitRollupStatus::from_status(GitEntryStatus {
                index: Unmerged,
                worktree: Unmodified
            }),
            Some(GitRollupStatus::Conflicted)
        );
        // Conflicted outranks a simultaneously "changed"-looking pairing.
        assert_eq!(
            GitRollupStatus::from_status(GitEntryStatus {
                index: Modified,
                worktree: Unmerged
            }),
            Some(GitRollupStatus::Conflicted)
        );
    }

    #[test]
    fn test_git_rollup_status_from_status_deleted_on_either_side_maps_to_changed() {
        use GitStatusCode::{Deleted, Unmerged, Unmodified};

        // Unstaged deletion (the common case: file removed from the worktree):
        // rolls up under the shared `changed` lane, per spec-explorer-panel.md.
        assert_eq!(
            GitRollupStatus::from_status(GitEntryStatus {
                index: Unmodified,
                worktree: Deleted
            }),
            Some(GitRollupStatus::Changed)
        );
        // Staged deletion.
        assert_eq!(
            GitRollupStatus::from_status(GitEntryStatus {
                index: Deleted,
                worktree: Unmodified
            }),
            Some(GitRollupStatus::Changed)
        );
        // A conflict still outranks a deletion on the other side.
        assert_eq!(
            GitRollupStatus::from_status(GitEntryStatus {
                index: Deleted,
                worktree: Unmerged
            }),
            Some(GitRollupStatus::Conflicted)
        );
        assert_eq!(GitRollupStatus::Changed.badge(), "M");
    }

    #[test]
    fn test_worse_severity_orders_error_above_warning_above_information_above_hint() {
        use DiagnosticSeverity::{Error, Hint, Information, Warning};

        assert_eq!(worse_severity(Error, Hint), Error);
        assert_eq!(worse_severity(Hint, Error), Error);
        assert_eq!(worse_severity(Warning, Information), Warning);
        assert_eq!(worse_severity(Information, Hint), Information);
    }

    #[test]
    fn test_max_severity_none_is_clean_and_loses_to_any_some() {
        assert_eq!(max_severity(None, None), None);
        assert_eq!(
            max_severity(Some(DiagnosticSeverity::Hint), None),
            Some(DiagnosticSeverity::Hint)
        );
        assert_eq!(
            max_severity(None, Some(DiagnosticSeverity::Error)),
            Some(DiagnosticSeverity::Error)
        );
    }

    #[test]
    fn test_compute_rollup_propagates_the_worst_status_and_severity_to_ancestors() {
        // a/
        //   b/
        //     c.rs      untracked
        //     other.rs  changed + an error diagnostic
        //   d.rs         a warning diagnostic, no git status
        // e.rs            clean
        // f.rs             conflicted, top-level (must not leak into `a`)
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk(
            "/proj".into(),
            vec![
                dir("a"),
                dir("a/b"),
                file("a/b/c.rs"),
                file("a/b/other.rs"),
                file("a/d.rs"),
                file("e.rs"),
                file("f.rs"),
            ],
            true,
        );
        model.apply_git_update(
            vec![
                git_entry(
                    "a/b/c.rs",
                    GitStatusCode::Unmodified,
                    GitStatusCode::Untracked,
                ),
                git_entry(
                    "a/b/other.rs",
                    GitStatusCode::Unmodified,
                    GitStatusCode::Modified,
                ),
                git_entry("f.rs", GitStatusCode::Unmerged, GitStatusCode::Unmerged),
            ],
            vec![],
        );
        model.apply_diagnostics(
            "a/b/other.rs".into(),
            "rust-analyzer".into(),
            vec![diag(DiagnosticSeverity::Error)],
        );
        model.apply_diagnostics(
            "a/d.rs".into(),
            "rust-analyzer".into(),
            vec![diag(DiagnosticSeverity::Warning)],
        );

        let rollup = compute_rollup(&model);
        let at = |path: &str| rollup.get(path).copied().unwrap_or_default();

        // `a` rolls up the worst of everything beneath it: `Changed` beats the
        // `Untracked` sibling file, and the `Error` beats the `Warning`.
        assert_eq!(
            at("a"),
            Rollup {
                git_status: Some(GitRollupStatus::Changed),
                severity: Some(DiagnosticSeverity::Error),
            }
        );
        // `a/b` rolls up only its own two children.
        assert_eq!(
            at("a/b"),
            Rollup {
                git_status: Some(GitRollupStatus::Changed),
                severity: Some(DiagnosticSeverity::Error),
            }
        );
        // Leaf files carry exactly their own decoration.
        assert_eq!(
            at("a/b/c.rs"),
            Rollup {
                git_status: Some(GitRollupStatus::Untracked),
                severity: None,
            }
        );
        assert_eq!(
            at("a/b/other.rs"),
            Rollup {
                git_status: Some(GitRollupStatus::Changed),
                severity: Some(DiagnosticSeverity::Error),
            }
        );
        assert_eq!(
            at("a/d.rs"),
            Rollup {
                git_status: None,
                severity: Some(DiagnosticSeverity::Warning),
            }
        );
        // Clean sibling stays clean.
        assert_eq!(at("e.rs"), Rollup::default());
        // A conflicted top-level file is its own decoration only — it must not
        // leak into `a` or `e.rs`.
        assert_eq!(
            at("f.rs"),
            Rollup {
                git_status: Some(GitRollupStatus::Conflicted),
                severity: None,
            }
        );
    }

    #[test]
    fn test_compute_rollup_not_confused_by_a_lexically_interleaved_sibling_file() {
        // BTreeMap order is by raw path string, and `.` (0x2E) sorts below `/`
        // (0x2F), so `src.rs` sorts *between* `src` and `src/main.rs`:
        // "src" < "src.rs" < "src/main.rs". A depth-only open-ancestor stack
        // mistakes `src.rs` (a same-depth sibling, not a descendant) for the
        // end of `src`'s subtree and pops it prematurely, so `src/main.rs`'s
        // error never rolls up to `src` (#329's regression).
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk(
            "/proj".into(),
            vec![dir("src"), file("src.rs"), file("src/main.rs")],
            true,
        );
        model.apply_git_update(
            vec![git_entry(
                "src/main.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Modified,
            )],
            vec![],
        );
        model.apply_diagnostics(
            "src/main.rs".into(),
            "rust-analyzer".into(),
            vec![diag(DiagnosticSeverity::Error)],
        );

        let rollup = compute_rollup(&model);

        assert_eq!(
            rollup.get("src").copied().unwrap_or_default(),
            Rollup {
                git_status: Some(GitRollupStatus::Changed),
                severity: Some(DiagnosticSeverity::Error),
            },
            "src/main.rs's status and severity must roll up to its parent `src`, \
             even with the lexically-interleaved sibling `src.rs` in between"
        );
        // The sibling file itself must stay uninvolved in `src`'s subtree.
        assert_eq!(
            rollup.get("src.rs").copied().unwrap_or_default(),
            Rollup::default()
        );
    }

    #[test]
    fn test_compute_rollup_clears_when_the_underlying_status_and_diagnostics_clear() {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk("/proj".into(), vec![dir("src"), file("src/main.rs")], true);
        model.apply_git_update(
            vec![git_entry(
                "src/main.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Modified,
            )],
            vec![],
        );
        model.apply_diagnostics(
            "src/main.rs".into(),
            "rust-analyzer".into(),
            vec![diag(DiagnosticSeverity::Error)],
        );

        let rollup = compute_rollup(&model);
        assert_eq!(
            rollup.get("src").copied().unwrap_or_default(),
            Rollup {
                git_status: Some(GitRollupStatus::Changed),
                severity: Some(DiagnosticSeverity::Error),
            }
        );

        // The file returns to clean and its diagnostic is fixed.
        model.apply_git_update(vec![], vec!["src/main.rs".into()]);
        model.apply_diagnostics("src/main.rs".into(), "rust-analyzer".into(), vec![]);

        let rollup = compute_rollup(&model);
        assert_eq!(
            rollup.get("src").copied().unwrap_or_default(),
            Rollup::default()
        );
    }

    // --- deleted tracked files: changed roll-up onto surviving ancestors (#480) ---

    #[test]
    fn test_compute_rollup_deleted_path_absent_from_tree_marks_surviving_ancestors() {
        // `a/b/gone.rs` was deleted: its tree entry is gone, but the git
        // recompute still reports it as worktree-deleted. Both surviving
        // ancestors must carry the changed roll-up; the clean sibling stays clean.
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk(
            "/proj".into(),
            vec![dir("a"), dir("a/b"), file("a/b/keep.rs")],
            true,
        );
        model.apply_git_update(
            vec![git_entry(
                "a/b/gone.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Deleted,
            )],
            vec![],
        );

        let rollup = compute_rollup(&model);
        let at = |path: &str| rollup.get(path).copied().unwrap_or_default();

        let changed = Rollup {
            git_status: Some(GitRollupStatus::Changed),
            severity: None,
        };
        assert_eq!(at("a"), changed);
        assert_eq!(at("a/b"), changed);
        assert_eq!(at("a/b/keep.rs"), Rollup::default());
        // The gone path itself gets no entry — there is no row to decorate.
        assert!(!rollup.contains_key("a/b/gone.rs"));
    }

    #[test]
    fn test_compute_rollup_deleted_subtree_skips_ancestors_that_also_left_the_tree() {
        // The whole `a/b` directory was deleted with its file: only the
        // surviving ancestor `a` is marked; no phantom `a/b` roll-up appears.
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk("/proj".into(), vec![dir("a"), file("a/keep.rs")], true);
        model.apply_git_update(
            vec![git_entry(
                "a/b/gone.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Deleted,
            )],
            vec![],
        );

        let rollup = compute_rollup(&model);
        assert_eq!(
            rollup.get("a").copied().unwrap_or_default(),
            Rollup {
                git_status: Some(GitRollupStatus::Changed),
                severity: None,
            }
        );
        assert!(!rollup.contains_key("a/b"));
    }

    #[test]
    fn test_compute_rollup_deleted_and_modified_share_the_changed_lane() {
        // `src` holds both an ordinary edit and a deletion; both classify as
        // `Changed`, so the shared ancestor carries the changed roll-up.
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk("/proj".into(), vec![dir("src"), file("src/kept.rs")], true);
        model.apply_git_update(
            vec![
                git_entry(
                    "src/kept.rs",
                    GitStatusCode::Unmodified,
                    GitStatusCode::Modified,
                ),
                git_entry(
                    "src/gone.rs",
                    GitStatusCode::Unmodified,
                    GitStatusCode::Deleted,
                ),
            ],
            vec![],
        );

        let rollup = compute_rollup(&model);
        assert_eq!(
            rollup.get("src").copied().unwrap_or_default().git_status,
            Some(GitRollupStatus::Changed)
        );
    }

    #[test]
    fn test_compute_rollup_deleted_top_level_path_without_ancestors_adds_nothing() {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk("/proj".into(), vec![file("keep.rs")], true);
        model.apply_git_update(
            vec![git_entry(
                "gone.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Deleted,
            )],
            vec![],
        );

        let rollup = compute_rollup(&model);
        assert_eq!(
            rollup.get("keep.rs").copied().unwrap_or_default(),
            Rollup::default()
        );
        assert!(!rollup.contains_key("gone.rs"));
    }

    #[test]
    fn test_compute_rollup_deleted_rollup_clears_when_the_status_clears() {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk("/proj".into(), vec![dir("src"), file("src/keep.rs")], true);
        model.apply_git_update(
            vec![git_entry(
                "src/gone.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Deleted,
            )],
            vec![],
        );
        assert_eq!(
            compute_rollup(&model)
                .get("src")
                .copied()
                .unwrap_or_default()
                .git_status,
            Some(GitRollupStatus::Changed)
        );

        // The deletion gets committed (or restored): the recompute clears it.
        model.apply_git_update(vec![], vec!["src/gone.rs".into()]);
        assert_eq!(
            compute_rollup(&model)
                .get("src")
                .copied()
                .unwrap_or_default(),
            Rollup::default()
        );
    }

    #[test]
    fn test_dir_row_carries_the_changed_rollup_after_a_tracked_file_deletion() {
        // End-to-end through the view: fold the same message sequence the
        // daemon sends on a deletion (worktree update removing the entry,
        // then the git recompute reporting it deleted) and check the parent
        // directory's rendered row carries the changed decoration.
        let mut tree = seed(vec![dir("src"), file("src/gone.rs"), file("top.rs")]);
        tree.model_mut()
            .apply_update(vec![], vec![], vec!["src/gone.rs".into()]);
        tree.model_mut().apply_git_update(
            vec![git_entry(
                "src/gone.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Deleted,
            )],
            vec![],
        );

        let rows = tree.visible_rows();
        let visible: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(visible, vec!["src", "top.rs"]);

        let src_row = rows.iter().find(|r| r.path == "src").expect("src row");
        assert_eq!(src_row.git_status, Some(GitRollupStatus::Changed));
        assert_eq!(src_row.severity, None);
    }

    #[test]
    fn test_collapsed_dir_row_carries_the_rolled_up_git_status_and_severity() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs"), file("top.rs")]);
        tree.model_mut().apply_git_update(
            vec![git_entry(
                "src/main.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Modified,
            )],
            vec![],
        );
        tree.model_mut().apply_diagnostics(
            "src/main.rs".into(),
            "rust-analyzer".into(),
            vec![diag(DiagnosticSeverity::Error)],
        );

        tree.toggle_dir("src");
        let rows = tree.visible_rows();
        let visible: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(visible, vec!["src", "top.rs"]);

        let src_row = rows.iter().find(|r| r.path == "src").expect("src row");
        assert_eq!(src_row.git_status, Some(GitRollupStatus::Changed));
        assert_eq!(src_row.severity, Some(DiagnosticSeverity::Error));

        let top_row = rows
            .iter()
            .find(|r| r.path == "top.rs")
            .expect("top.rs row");
        assert_eq!(top_row.git_status, None);
        assert_eq!(top_row.severity, None);
    }

    #[test]
    fn test_row_shows_decoration_only_on_a_collapsed_dir_not_an_expanded_one() {
        // Reuses the same seeded rollup as
        // `test_collapsed_dir_row_carries_the_rolled_up_git_status_and_severity`:
        // `src` rolls up a changed + errored descendant either way, but
        // rendering must suppress that rolled-up badge while `src` is
        // expanded (its child already shows its own) and surface it once
        // `src` is collapsed.
        let mut tree = seed(vec![dir("src"), file("src/main.rs"), file("top.rs")]);
        tree.model_mut().apply_git_update(
            vec![git_entry(
                "src/main.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Modified,
            )],
            vec![],
        );
        tree.model_mut().apply_diagnostics(
            "src/main.rs".into(),
            "rust-analyzer".into(),
            vec![diag(DiagnosticSeverity::Error)],
        );

        // Expanded: `src`'s row still carries the rolled-up decoration in the
        // cache (`compute_rollup` is unchanged), but it must not render.
        let rows = tree.visible_rows();
        let src_row = rows.iter().find(|r| r.path == "src").expect("src row");
        assert_eq!(src_row.git_status, Some(GitRollupStatus::Changed));
        assert!(!tree.row_shows_decoration(src_row));

        // The file itself always shows its own decoration.
        let file_row = rows
            .iter()
            .find(|r| r.path == "src/main.rs")
            .expect("file row");
        assert!(tree.row_shows_decoration(file_row));

        // Collapse `src`: the same rolled-up row now renders its decoration.
        tree.toggle_dir("src");
        let rows = tree.visible_rows();
        let src_row = rows.iter().find(|r| r.path == "src").expect("src row");
        assert!(tree.row_shows_decoration(src_row));
    }

    #[test]
    fn test_visible_rows_carries_the_ignored_flag_from_the_entry() {
        let mut ignored_entry = file("ignored.rs");
        ignored_entry.ignored = true;
        let tree = seed(vec![file("kept.rs"), ignored_entry]);

        let rows = tree.visible_rows();
        let kept = rows.iter().find(|r| r.path == "kept.rs").expect("kept.rs");
        let ignored = rows
            .iter()
            .find(|r| r.path == "ignored.rs")
            .expect("ignored.rs");
        assert!(!kept.ignored);
        assert!(ignored.ignored);
    }

    // --- reveal active file (#331) ---

    #[test]
    fn test_ancestor_dirs_of_a_nested_path() {
        let ancestors: Vec<&str> = ancestor_dirs("src/net/tcp.rs").collect();
        assert_eq!(ancestors, vec!["src", "src/net"]);
    }

    #[test]
    fn test_ancestor_dirs_of_a_top_level_path_is_empty() {
        let ancestors: Vec<&str> = ancestor_dirs("top.rs").collect();
        assert!(ancestors.is_empty());
    }

    // --- absolute_path (context-menu "Copy path", #671) ---

    #[test]
    fn test_absolute_path_joins_root_and_nested_relative_path() {
        assert_eq!(absolute_path("/proj", "src/main.rs"), "/proj/src/main.rs");
    }

    #[test]
    fn test_absolute_path_of_a_top_level_file_joins_with_a_single_slash() {
        assert_eq!(absolute_path("/proj", "top.rs"), "/proj/top.rs");
    }

    #[test]
    fn test_absolute_path_with_filesystem_root_does_not_double_the_slash() {
        assert_eq!(absolute_path("/", "top.rs"), "/top.rs");
        assert_eq!(absolute_path("/", "src/main.rs"), "/src/main.rs");
    }

    #[test]
    fn test_absolute_path_with_a_trailing_slash_root_does_not_double_the_slash() {
        assert_eq!(absolute_path("/proj/", "src/main.rs"), "/proj/src/main.rs");
    }

    // --- reveal_in_terminal_dir (context-menu "Reveal in terminal", #672) ---

    #[test]
    fn test_reveal_in_terminal_dir_of_a_directory_is_itself() {
        assert_eq!(
            reveal_in_terminal_dir("/proj", &EntryKind::Dir, "src"),
            "/proj/src"
        );
    }

    #[test]
    fn test_reveal_in_terminal_dir_of_a_nested_file_is_its_parent() {
        assert_eq!(
            reveal_in_terminal_dir("/proj", &EntryKind::File, "src/main.rs"),
            "/proj/src"
        );
    }

    #[test]
    fn test_reveal_in_terminal_dir_of_a_top_level_file_is_the_root() {
        assert_eq!(
            reveal_in_terminal_dir("/proj", &EntryKind::File, "top.rs"),
            "/proj"
        );
    }

    #[test]
    fn test_reveal_in_terminal_dir_with_filesystem_root_does_not_double_the_slash() {
        assert_eq!(reveal_in_terminal_dir("/", &EntryKind::File, "top.rs"), "/");
        assert_eq!(reveal_in_terminal_dir("/", &EntryKind::Dir, "src"), "/src");
    }

    #[test]
    fn test_reveal_expands_ancestors_selects_and_marks_the_cache_dirty() {
        let mut tree = seed(vec![dir("a"), dir("a/b"), file("a/b/c.rs"), file("top.rs")]);
        tree.toggle_dir("a");
        tree.toggle_dir("a/b");
        tree.refresh_row_cache();
        assert!(tree.is_collapsed("a"));
        assert!(tree.is_collapsed("a/b"));
        assert!(!tree.cache_dirty);

        tree.reveal("a/b/c.rs");

        assert!(!tree.is_collapsed("a"));
        assert!(!tree.is_collapsed("a/b"));
        assert_eq!(tree.selected(), Some("a/b/c.rs"));
        assert!(!tree.cache_dirty, "reveal refreshes the cache immediately");
        let visible: Vec<&str> = tree.row_cache.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(visible, vec!["a", "a/b", "a/b/c.rs", "top.rs"]);
    }

    #[test]
    fn test_reveal_of_a_path_absent_from_the_model_is_a_no_op() {
        let mut tree = seed(vec![dir("a"), file("a/b.rs")]);
        tree.toggle_dir("a");
        tree.refresh_row_cache();

        tree.reveal("does/not/exist.rs");

        assert!(tree.is_collapsed("a"), "unrelated collapse state untouched");
        assert_eq!(tree.selected(), None, "no selection for a missing path");
        assert!(!tree.cache_dirty, "no spurious invalidation for a no-op");
    }

    #[test]
    fn test_reveal_is_idempotent_when_already_expanded_and_selected() {
        let mut tree = seed(vec![dir("a"), file("a/b.rs")]);
        tree.reveal("a/b.rs");
        tree.refresh_row_cache();
        assert!(!tree.cache_dirty);

        tree.reveal("a/b.rs");

        assert_eq!(tree.selected(), Some("a/b.rs"));
        assert!(
            !tree.cache_dirty,
            "revealing an already-expanded, already-selected path changes nothing"
        );
    }

    // --- keyboard navigation: selection movement + expand/collapse-at-edge (#332) ---

    /// A bare row for exercising the pure navigation helpers directly, without
    /// seeding a whole model.
    fn row_at(path: &str, kind: EntryKind, depth: usize) -> Row {
        Row {
            path: path.to_owned(),
            kind,
            depth,
            ignored: false,
            git_status: None,
            severity: None,
            is_pending_create: false,
            matched_indices: Vec::new(),
        }
    }

    /// Seeded visible-row set used by the plain-function navigation tests
    /// below (mirrors `a/b/c.rs`, `a/d.rs`, `e.rs`):
    /// ```text
    /// a          depth 0, dir
    /// a/b        depth 1, dir
    /// a/b/c.rs   depth 2, file
    /// a/d.rs     depth 1, file
    /// e.rs       depth 0, file
    /// ```
    fn seeded_rows() -> Vec<Row> {
        vec![
            row_at("a", EntryKind::Dir, 0),
            row_at("a/b", EntryKind::Dir, 1),
            row_at("a/b/c.rs", EntryKind::File, 2),
            row_at("a/d.rs", EntryKind::File, 1),
            row_at("e.rs", EntryKind::File, 0),
        ]
    }

    #[test]
    fn test_selection_after_down_moves_to_the_next_row() {
        let rows = seeded_rows();
        assert_eq!(
            selection_after_down(&rows, Some("a/b")).as_deref(),
            Some("a/b/c.rs")
        );
    }

    #[test]
    fn test_selection_after_down_clamps_at_the_last_row() {
        let rows = seeded_rows();
        assert_eq!(
            selection_after_down(&rows, Some("e.rs")).as_deref(),
            Some("e.rs")
        );
    }

    #[test]
    fn test_selection_after_down_selects_the_first_row_when_nothing_was_selected() {
        let rows = seeded_rows();
        assert_eq!(selection_after_down(&rows, None).as_deref(), Some("a"));
    }

    #[test]
    fn test_selection_after_down_falls_back_to_the_first_row_when_the_selection_is_no_longer_visible(
    ) {
        // Simulates the selected path having scrolled out of the visible set
        // (e.g. an ancestor was collapsed after selection).
        let rows = seeded_rows();
        assert_eq!(
            selection_after_down(&rows, Some("gone")).as_deref(),
            Some("a")
        );
    }

    #[test]
    fn test_selection_after_up_moves_to_the_previous_row() {
        let rows = seeded_rows();
        assert_eq!(
            selection_after_up(&rows, Some("a/d.rs")).as_deref(),
            Some("a/b/c.rs")
        );
    }

    #[test]
    fn test_selection_after_up_clamps_at_the_first_row() {
        let rows = seeded_rows();
        assert_eq!(selection_after_up(&rows, Some("a")).as_deref(), Some("a"));
    }

    #[test]
    fn test_selection_after_up_selects_the_first_row_when_nothing_was_selected() {
        let rows = seeded_rows();
        assert_eq!(selection_after_up(&rows, None).as_deref(), Some("a"));
    }

    #[test]
    fn test_selection_after_down_and_up_on_an_empty_row_set_select_nothing() {
        let rows: Vec<Row> = Vec::new();
        assert_eq!(selection_after_down(&rows, None), None);
        assert_eq!(selection_after_up(&rows, Some("a")), None);
    }

    #[test]
    fn test_parent_path_is_none_at_a_top_level_row() {
        let rows = seeded_rows();
        assert_eq!(parent_path(&rows, 0), None); // "a"
        assert_eq!(parent_path(&rows, 4), None); // "e.rs"
    }

    #[test]
    fn test_parent_path_finds_the_nearest_ancestor_not_just_the_previous_row() {
        let rows = seeded_rows();
        // "a/b/c.rs" (index 2) sits directly under "a/b" (index 1).
        assert_eq!(parent_path(&rows, 2), Some("a/b"));
        // "a/d.rs" (index 3) is back up a level, its parent is "a" (index 0),
        // skipping over the deeper "a/b" sibling that precedes it.
        assert_eq!(parent_path(&rows, 3), Some("a"));
    }

    #[test]
    fn test_first_child_path_finds_the_row_immediately_after_an_expanded_dir() {
        let rows = seeded_rows();
        assert_eq!(first_child_path(&rows, 0), Some("a/b")); // "a"'s first child
        assert_eq!(first_child_path(&rows, 1), Some("a/b/c.rs")); // "a/b"'s first child
    }

    #[test]
    fn test_first_child_path_is_none_for_a_file() {
        let rows = seeded_rows();
        assert_eq!(first_child_path(&rows, 2), None); // "a/b/c.rs" is a file
        assert_eq!(first_child_path(&rows, 4), None); // "e.rs" is a file
    }

    #[test]
    fn test_first_child_path_is_none_for_an_empty_or_already_collapsed_dir() {
        // A dir whose next row is a sibling (same or shallower depth), not a
        // child — the same shape as an empty dir or one hidden by collapse.
        let rows = vec![
            row_at("empty", EntryKind::Dir, 0),
            row_at("sibling.rs", EntryKind::File, 0),
        ];
        assert_eq!(first_child_path(&rows, 0), None);
    }

    #[test]
    fn test_first_child_path_is_none_for_the_last_row() {
        let rows = seeded_rows();
        assert_eq!(first_child_path(&rows, rows.len() - 1), None);
    }

    #[test]
    fn test_select_down_and_up_move_the_tree_selection_through_visible_rows() {
        let mut tree = seed(vec![dir("a"), file("a/b.rs"), file("top.rs")]);
        // BTreeMap order: a, a/b.rs, top.rs.
        tree.select_down();
        assert_eq!(tree.selected(), Some("a"));
        tree.select_down();
        assert_eq!(tree.selected(), Some("a/b.rs"));
        tree.select_down();
        assert_eq!(tree.selected(), Some("top.rs"));
        // Clamped at the last row.
        tree.select_down();
        assert_eq!(tree.selected(), Some("top.rs"));

        tree.select_up();
        assert_eq!(tree.selected(), Some("a/b.rs"));
    }

    #[test]
    fn test_select_first_and_select_last_jump_to_the_row_set_edges() {
        let mut tree = seed(vec![dir("a"), file("a/b.rs"), file("top.rs")]);
        tree.selected = Some("a/b.rs".into());

        tree.select_last();
        assert_eq!(tree.selected(), Some("top.rs"));

        tree.select_first();
        assert_eq!(tree.selected(), Some("a"));
    }

    #[test]
    fn test_scroll_selected_into_view_refreshes_the_cache_and_tolerates_a_hidden_selection() {
        let mut tree = seed(vec![dir("a"), file("a/b.rs"), file("top.rs")]);
        tree.selected = Some("a/b.rs".into());
        // Hide the selected row: `a/b.rs` drops out of the visible set and the
        // cache is marked dirty.
        tree.toggle_dir("a");

        tree.scroll_selected_into_view();

        assert!(!tree.cache_dirty, "the helper refreshes the cache first");
        // The hidden selection stays put; there is simply no row to scroll to.
        assert_eq!(tree.selected(), Some("a/b.rs"));
    }

    #[test]
    fn test_collapse_or_select_parent_collapses_an_expanded_selected_dir() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs")]);
        tree.selected = Some("src".into());

        tree.collapse_or_select_parent();

        assert!(tree.is_collapsed("src"));
        // Collapsing keeps the selection on the directory itself.
        assert_eq!(tree.selected(), Some("src"));
    }

    #[test]
    fn test_collapse_or_select_parent_on_a_file_selects_its_parent() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs")]);
        tree.selected = Some("src/main.rs".into());

        tree.collapse_or_select_parent();

        assert_eq!(tree.selected(), Some("src"));
        assert!(
            !tree.is_collapsed("src"),
            "selecting the parent must not also collapse it"
        );
    }

    #[test]
    fn test_collapse_or_select_parent_on_an_already_collapsed_dir_selects_its_parent() {
        let mut tree = seed(vec![dir("a"), dir("a/b"), file("a/b/c.rs")]);
        tree.toggle_dir("a/b");
        tree.selected = Some("a/b".into());

        tree.collapse_or_select_parent();

        assert_eq!(tree.selected(), Some("a"));
    }

    #[test]
    fn test_collapse_or_select_parent_is_a_noop_at_a_top_level_row() {
        let mut tree = seed(vec![file("top.rs")]);
        tree.selected = Some("top.rs".into());

        tree.collapse_or_select_parent();

        // No parent to step to; selection is unchanged.
        assert_eq!(tree.selected(), Some("top.rs"));
    }

    #[test]
    fn test_collapse_or_select_parent_selects_the_first_row_when_nothing_was_selected() {
        let mut tree = seed(vec![dir("a"), file("top.rs")]);

        tree.collapse_or_select_parent();

        assert_eq!(tree.selected(), Some("a"));
    }

    #[test]
    fn test_expand_or_select_child_expands_a_collapsed_selected_dir() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs")]);
        tree.toggle_dir("src");
        tree.selected = Some("src".into());

        tree.expand_or_select_child();

        assert!(!tree.is_collapsed("src"));
        assert_eq!(tree.selected(), Some("src"));
    }

    #[test]
    fn test_expand_or_select_child_on_an_expanded_dir_selects_its_first_child() {
        let mut tree = seed(vec![dir("a"), dir("a/b"), file("a/z.rs")]);
        tree.selected = Some("a".into());

        tree.expand_or_select_child();

        assert_eq!(tree.selected(), Some("a/b"));
    }

    #[test]
    fn test_expand_or_select_child_is_a_noop_on_a_file() {
        let mut tree = seed(vec![file("top.rs")]);
        tree.selected = Some("top.rs".into());

        tree.expand_or_select_child();

        assert_eq!(tree.selected(), Some("top.rs"));
    }

    #[test]
    fn test_expand_or_select_child_is_a_noop_on_an_empty_expanded_dir() {
        let mut tree = seed(vec![dir("empty"), file("top.rs")]);
        tree.selected = Some("empty".into());

        tree.expand_or_select_child();

        assert_eq!(tree.selected(), Some("empty"));
    }

    #[test]
    fn test_expand_or_select_child_selects_the_first_row_when_nothing_was_selected() {
        let mut tree = seed(vec![dir("a"), file("top.rs")]);

        tree.expand_or_select_child();

        assert_eq!(tree.selected(), Some("a"));
    }

    // --- discrete multi-select (artboard State B flat fill, `docs/spec-explorer-search.md`, #680) ---

    #[test]
    fn test_click_dir_clears_an_existing_multi_selection() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs"), file("top.rs")]);
        tree.selection = HashSet::from(["top.rs".to_owned()]);

        tree.click_dir("src");

        assert!(tree.selection.is_empty());
    }

    #[test]
    fn test_toggle_selection_adds_then_removes_a_path() {
        let mut tree = seed(vec![file("a.rs"), file("b.rs")]);

        tree.toggle_selection("a.rs");
        assert!(tree.selection.contains("a.rs"));
        assert_eq!(tree.selected(), Some("a.rs"));

        tree.toggle_selection("a.rs");
        assert!(!tree.selection.contains("a.rs"));
    }

    #[test]
    fn test_toggle_selection_is_additive_and_moves_the_cursor() {
        let mut tree = seed(vec![file("a.rs"), file("b.rs"), file("c.rs")]);

        tree.toggle_selection("a.rs");
        tree.toggle_selection("c.rs");

        assert_eq!(
            tree.selection,
            HashSet::from(["a.rs".to_owned(), "c.rs".to_owned()])
        );
        // The cursor follows the most recently toggled path.
        assert_eq!(tree.selected(), Some("c.rs"));
    }

    #[test]
    fn test_range_select_selects_every_row_between_cursor_and_target() {
        let mut tree = seed(vec![file("a.rs"), file("b.rs"), file("c.rs"), file("d.rs")]);
        tree.selected = Some("a.rs".into());

        tree.range_select("c.rs");

        assert_eq!(
            tree.selection,
            HashSet::from(["a.rs".to_owned(), "b.rs".to_owned(), "c.rs".to_owned()])
        );
        assert_eq!(tree.selected(), Some("c.rs"));
    }

    #[test]
    fn test_range_select_with_target_above_the_cursor_still_orders_low_to_high() {
        let mut tree = seed(vec![file("a.rs"), file("b.rs"), file("c.rs")]);
        tree.selected = Some("c.rs".into());

        tree.range_select("a.rs");

        assert_eq!(
            tree.selection,
            HashSet::from(["a.rs".to_owned(), "b.rs".to_owned(), "c.rs".to_owned()])
        );
    }

    #[test]
    fn test_range_select_with_no_cursor_selects_only_the_target() {
        let mut tree = seed(vec![file("a.rs"), file("b.rs")]);

        tree.range_select("b.rs");

        assert_eq!(tree.selection, HashSet::from(["b.rs".to_owned()]));
        assert_eq!(tree.selected(), Some("b.rs"));
    }

    #[test]
    fn test_range_select_re_ranges_from_wherever_the_cursor_last_landed() {
        // A second Shift+Click re-ranges from the row the first one left the
        // cursor on (`b.rs`), not the original `a.rs` anchor — the documented
        // v1 simplification (no separate anchor field).
        let mut tree = seed(vec![file("a.rs"), file("b.rs"), file("c.rs"), file("d.rs")]);
        tree.selected = Some("a.rs".into());
        tree.range_select("b.rs");
        assert_eq!(
            tree.selection,
            HashSet::from(["a.rs".to_owned(), "b.rs".to_owned()])
        );

        tree.range_select("d.rs");

        assert_eq!(
            tree.selection,
            HashSet::from(["b.rs".to_owned(), "c.rs".to_owned(), "d.rs".to_owned()])
        );
    }

    #[test]
    fn test_extend_selection_down_grows_a_contiguous_range_from_the_cursor() {
        let mut tree = seed(vec![file("a.rs"), file("b.rs"), file("c.rs"), file("d.rs")]);
        tree.selected = Some("a.rs".into());

        tree.extend_selection_down();
        assert_eq!(tree.selected(), Some("b.rs"));
        assert_eq!(
            tree.selection,
            HashSet::from(["a.rs".to_owned(), "b.rs".to_owned()])
        );

        tree.extend_selection_down();
        assert_eq!(tree.selected(), Some("c.rs"));
        assert_eq!(
            tree.selection,
            HashSet::from(["a.rs".to_owned(), "b.rs".to_owned(), "c.rs".to_owned()])
        );
    }

    #[test]
    fn test_extend_selection_up_grows_a_contiguous_range_from_the_cursor() {
        let mut tree = seed(vec![file("a.rs"), file("b.rs"), file("c.rs")]);
        tree.selected = Some("c.rs".into());

        tree.extend_selection_up();

        assert_eq!(tree.selected(), Some("b.rs"));
        assert_eq!(
            tree.selection,
            HashSet::from(["c.rs".to_owned(), "b.rs".to_owned()])
        );
    }

    #[test]
    fn test_extend_selection_down_clamps_at_the_last_row() {
        let mut tree = seed(vec![file("a.rs"), file("b.rs")]);
        tree.selected = Some("b.rs".into());

        tree.extend_selection_down();

        assert_eq!(tree.selected(), Some("b.rs"));
        assert_eq!(tree.selection, HashSet::from(["b.rs".to_owned()]));
    }

    #[test]
    fn test_extend_selection_down_selects_the_first_row_when_nothing_was_selected() {
        let mut tree = seed(vec![file("a.rs"), file("b.rs")]);

        tree.extend_selection_down();

        assert_eq!(tree.selected(), Some("a.rs"));
        assert!(tree.selection.is_empty());
    }

    #[test]
    fn test_open_many_targets_excludes_directories() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs"), file("top.rs")]);
        tree.selection = HashSet::from(["src".to_owned(), "top.rs".to_owned()]);
        tree.refresh_row_cache();

        assert_eq!(tree.open_many_targets(), vec!["top.rs".to_owned()]);
    }

    #[test]
    fn test_open_many_targets_are_ordered_by_visible_row_not_hashset_order() {
        let mut tree = seed(vec![file("a.rs"), file("m.rs"), file("z.rs")]);
        tree.selection = HashSet::from(["z.rs".to_owned(), "a.rs".to_owned(), "m.rs".to_owned()]);
        tree.refresh_row_cache();

        assert_eq!(
            tree.open_many_targets(),
            vec!["a.rs".to_owned(), "m.rs".to_owned(), "z.rs".to_owned()]
        );
    }

    #[test]
    fn test_open_many_targets_prunes_paths_the_model_no_longer_has() {
        // The multi-set is pruned against the visible set the same lazy way
        // `selected` is: `selection` itself is never actively swept, but a
        // stale path simply matches no row here (`docs/spec-explorer-search.md`).
        let mut tree = seed(vec![file("a.rs"), file("b.rs")]);
        tree.selection = HashSet::from(["a.rs".to_owned(), "b.rs".to_owned()]);
        tree.refresh_row_cache();
        assert_eq!(
            tree.open_many_targets(),
            vec!["a.rs".to_owned(), "b.rs".to_owned()]
        );

        tree.model_mut()
            .apply_snapshot_chunk("/proj".into(), vec![file("a.rs")], true);
        tree.refresh_row_cache();

        assert_eq!(tree.open_many_targets(), vec!["a.rs".to_owned()]);
        assert!(
            tree.selection.contains("b.rs"),
            "selection itself is not actively pruned"
        );
    }

    #[gpui::test]
    fn test_click_file_sets_cursor_clears_multiset_and_emits_open_file(
        cx: &mut gpui::TestAppContext,
    ) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, _window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![file("a.rs"), file("b.rs")],
                        true,
                    );
                    tree.selection = HashSet::from(["a.rs".to_owned()]);
                    tree.click_file("b.rs", cx);
                });
            })
            .expect("click file");

        cx.update(|cx| {
            let tree = tree.read(cx);
            assert_eq!(tree.selected(), Some("b.rs"));
            assert!(tree.selection.is_empty());
            assert_eq!(
                events.borrow().as_slice(),
                [FileTreeEvent::OpenFile {
                    path: "b.rs".into()
                }]
            );
        });
    }

    #[gpui::test]
    fn test_open_selected_with_a_multiselection_emits_open_file_per_selected_file(
        cx: &mut gpui::TestAppContext,
    ) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, _window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![file("a.rs"), file("b.rs"), file("c.rs")],
                        true,
                    );
                    tree.selection = HashSet::from(["a.rs".to_owned(), "c.rs".to_owned()]);
                    tree.open_selected(cx);
                });
            })
            .expect("open selected");

        assert_eq!(
            events.borrow().as_slice(),
            [
                FileTreeEvent::OpenFile {
                    path: "a.rs".into()
                },
                FileTreeEvent::OpenFile {
                    path: "c.rs".into()
                },
            ]
        );
    }

    #[gpui::test]
    fn test_open_selected_with_an_empty_multiselection_falls_back_to_the_cursor(
        cx: &mut gpui::TestAppContext,
    ) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, _window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut()
                        .apply_snapshot_chunk("/proj".into(), vec![file("a.rs")], true);
                    tree.selected = Some("a.rs".into());
                    tree.open_selected(cx);
                });
            })
            .expect("open selected");

        assert_eq!(
            events.borrow().as_slice(),
            [FileTreeEvent::OpenFile {
                path: "a.rs".into()
            }]
        );
    }

    // --- rename_target / describe_file_op_error (pure helpers) -------------

    #[test]
    fn test_rename_target_joins_new_name_under_the_original_parent() {
        assert_eq!(rename_target("src/main.rs", "lib.rs"), "src/lib.rs");
    }

    #[test]
    fn test_rename_target_top_level_path_has_no_parent_prefix() {
        assert_eq!(rename_target("README.md", "readme.md"), "readme.md");
    }

    #[test]
    fn test_join_dir_under_a_nested_parent() {
        assert_eq!(join_dir("src", "lib.rs"), "src/lib.rs");
    }

    #[test]
    fn test_join_dir_at_the_root_has_no_parent_prefix() {
        assert_eq!(join_dir("", "README.md"), "README.md");
    }

    #[test]
    fn test_describe_delete_of_a_directory_warns_about_its_contents() {
        assert_eq!(
            describe_delete("src", &EntryKind::Dir),
            "Delete \"src\" and its contents? This cannot be undone."
        );
    }

    #[test]
    fn test_describe_delete_of_a_file_omits_the_contents_warning() {
        let message = describe_delete("main.rs", &EntryKind::File);
        assert_eq!(message, "Delete \"main.rs\"? This cannot be undone.");
        assert!(!message.contains("contents"));
    }

    #[test]
    fn test_describe_file_op_error_is_distinct_per_variant() {
        let messages = [
            describe_file_op_error(FileOpError::AlreadyExists),
            describe_file_op_error(FileOpError::InvalidPath),
            describe_file_op_error(FileOpError::NotFound),
            describe_file_op_error(FileOpError::PermissionDenied),
            describe_file_op_error(FileOpError::Io),
        ];
        let unique: HashSet<&str> = messages.iter().copied().collect();
        assert_eq!(
            unique.len(),
            messages.len(),
            "every reason reads distinctly"
        );
    }

    // --- resolve_drop / drop_target_dir / drops_into_own_subtree (drag & drop, #677) ---

    #[test]
    fn test_drop_target_dir_of_a_directory_is_itself() {
        assert_eq!(drop_target_dir(&EntryKind::Dir, "src"), "src");
    }

    #[test]
    fn test_drop_target_dir_of_a_nested_file_is_its_parent() {
        assert_eq!(drop_target_dir(&EntryKind::File, "src/main.rs"), "src");
    }

    #[test]
    fn test_drop_target_dir_of_a_top_level_file_is_the_root() {
        assert_eq!(drop_target_dir(&EntryKind::File, "README.md"), "");
    }

    #[test]
    fn test_drops_into_own_subtree_is_true_for_a_directory_dropped_on_itself() {
        assert!(drops_into_own_subtree("src", &EntryKind::Dir, "src"));
    }

    #[test]
    fn test_drops_into_own_subtree_is_true_for_a_descendant_directory() {
        assert!(drops_into_own_subtree("src", &EntryKind::Dir, "src/net"));
    }

    #[test]
    fn test_drops_into_own_subtree_is_false_for_a_sibling_directory() {
        assert!(!drops_into_own_subtree("src", &EntryKind::Dir, "lib"));
    }

    #[test]
    fn test_drops_into_own_subtree_is_false_for_a_path_sharing_only_a_text_prefix() {
        // `src2` shares the `src` text prefix but is not a `src/`-bounded
        // descendant — the same prefix-vs-substring distinction the
        // collapse-set guards against.
        assert!(!drops_into_own_subtree("src", &EntryKind::Dir, "src2"));
    }

    #[test]
    fn test_drops_into_own_subtree_is_false_for_a_dragged_file() {
        // A file has no subtree; dropping it "onto itself" is caught by the
        // no-op guard instead, not this one.
        assert!(!drops_into_own_subtree(
            "src/main.rs",
            &EntryKind::File,
            "src/main.rs"
        ));
    }

    #[test]
    fn test_resolve_drop_onto_a_directory_moves_the_dragged_file_there() {
        assert_eq!(
            resolve_drop("top.rs", &EntryKind::File, &EntryKind::Dir, "src"),
            Some(("top.rs".to_owned(), "src/top.rs".to_owned()))
        );
    }

    #[test]
    fn test_resolve_drop_onto_a_file_resolves_to_its_parent_directory() {
        assert_eq!(
            resolve_drop("top.rs", &EntryKind::File, &EntryKind::File, "src/main.rs"),
            Some(("top.rs".to_owned(), "src/top.rs".to_owned()))
        );
    }

    #[test]
    fn test_resolve_drop_refuses_a_same_parent_noop_move() {
        // `src/lib.rs` dropped on `src` itself: the resolved target
        // directory is already its current parent.
        assert_eq!(
            resolve_drop("src/lib.rs", &EntryKind::File, &EntryKind::Dir, "src"),
            None
        );
    }

    #[test]
    fn test_resolve_drop_refuses_a_directory_dropped_into_its_own_descendant() {
        assert_eq!(
            resolve_drop("src", &EntryKind::Dir, &EntryKind::Dir, "src/net"),
            None
        );
    }

    #[test]
    fn test_resolve_drop_refuses_a_directory_dropped_onto_itself() {
        assert_eq!(
            resolve_drop("src", &EntryKind::Dir, &EntryKind::Dir, "src"),
            None
        );
    }

    #[test]
    fn test_resolve_drop_moves_a_directory_to_a_sibling_directory() {
        assert_eq!(
            resolve_drop("src", &EntryKind::Dir, &EntryKind::Dir, "lib"),
            Some(("src".to_owned(), "lib/src".to_owned()))
        );
    }

    #[test]
    fn test_resolve_drop_onto_the_worktree_root_from_a_nested_file() {
        assert_eq!(
            resolve_drop("src/main.rs", &EntryKind::File, &EntryKind::File, "top.rs"),
            Some(("src/main.rs".to_owned(), "main.rs".to_owned()))
        );
    }

    // --- create_target_dir (pure derivation over model + selection) --------

    #[test]
    fn test_create_target_dir_of_a_selected_directory_is_itself() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs")]);
        tree.selected = Some("src".into());
        assert_eq!(tree.create_target_dir(), "src");
    }

    #[test]
    fn test_create_target_dir_of_a_selected_file_is_its_parent() {
        let mut tree = seed(vec![dir("src"), file("src/main.rs")]);
        tree.selected = Some("src/main.rs".into());
        assert_eq!(tree.create_target_dir(), "src");
    }

    #[test]
    fn test_create_target_dir_of_a_selected_top_level_file_is_the_root() {
        let mut tree = seed(vec![file("README.md")]);
        tree.selected = Some("README.md".into());
        assert_eq!(tree.create_target_dir(), "");
    }

    #[test]
    fn test_create_target_dir_with_nothing_selected_is_the_root() {
        let tree = seed(vec![dir("src"), file("README.md")]);
        assert_eq!(tree.create_target_dir(), "");
    }

    #[test]
    fn test_create_target_dir_of_a_stale_selection_is_the_root() {
        let mut tree = seed(vec![file("a.rs")]);
        tree.selected = Some("gone.rs".into());
        assert_eq!(tree.create_target_dir(), "");
    }

    // --- inline rename editor (artboard State C, `docs/spec-explorer-file-ops.md`, #675) ---

    /// A windowed `FileTree` (mirrors `source_control.rs`'s `open_panel`):
    /// the rename editor needs a live `Window`/`Context` to construct its
    /// `InputState` and focus it, unlike the headless model tests above.
    fn open_tree(
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<FileTree>, gpui::WindowHandle<gpui_component::Root>) {
        let mut tree = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let ft = cx.new(|_cx| FileTree::new());
                tree = Some(ft.clone());
                cx.new(|cx| gpui_component::Root::new(ft, window, cx))
            })
            .expect("open window")
        });
        (tree.expect("tree constructed in window"), window)
    }

    /// Subscribe a `Vec<FileTreeEvent>` sink to `tree` (mirrors
    /// `editor.rs`'s `test_apply_references_response_opens_the_results_panel`
    /// event-sink pattern).
    fn subscribe_events(
        tree: &Entity<FileTree>,
        cx: &mut gpui::TestAppContext,
    ) -> Rc<RefCell<Vec<FileTreeEvent>>> {
        let events: Rc<RefCell<Vec<FileTreeEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        cx.update(|cx| {
            cx.subscribe(tree, move |_tree, event: &FileTreeEvent, _cx| {
                sink.borrow_mut().push(event.clone());
            })
            .detach();
        });
        events
    }

    #[gpui::test]
    fn test_start_rename_with_selection_opens_editor_seeded_to_display_name(
        cx: &mut gpui::TestAppContext,
    ) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![file("src/main.rs")],
                        true,
                    );
                    tree.selected = Some("src/main.rs".into());
                    tree.start_rename(window, cx);
                });
            })
            .expect("start rename");

        cx.update(|cx| {
            let tree = tree.read(cx);
            let editor = tree.rename.as_ref().expect("rename editor opened");
            assert_eq!(editor.path, "src/main.rs");
            assert_eq!(editor.input.read(cx).value().as_ref(), "main.rs");
            assert!(editor.error.is_none());
        });
    }

    #[gpui::test]
    fn test_start_rename_with_no_selection_is_noop(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.start_rename(window, cx);
                });
            })
            .expect("start rename");

        cx.update(|cx| {
            assert!(tree.read(cx).rename.is_none());
        });
    }

    #[gpui::test]
    fn test_commit_rename_emits_rename_requested_and_closes_editor(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![file("src/main.rs")],
                        true,
                    );
                    tree.selected = Some("src/main.rs".into());
                    tree.start_rename(window, cx);
                    tree.rename
                        .as_ref()
                        .expect("editor open")
                        .input
                        .update(cx, |input, cx| {
                            input.set_value("lib.rs", window, cx);
                        });
                    tree.commit_rename(window, cx);
                });
            })
            .expect("commit rename");

        cx.update(|cx| {
            assert!(
                tree.read(cx).rename.is_none(),
                "the editor closes immediately on commit"
            );
        });
        assert_eq!(
            events.borrow().as_slice(),
            [FileTreeEvent::RenameRequested {
                from: "src/main.rs".into(),
                to: "src/lib.rs".into(),
            }]
        );
    }

    #[gpui::test]
    fn test_commit_rename_with_blank_name_sends_nothing(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![file("src/main.rs")],
                        true,
                    );
                    tree.selected = Some("src/main.rs".into());
                    tree.start_rename(window, cx);
                    tree.rename
                        .as_ref()
                        .expect("editor open")
                        .input
                        .update(cx, |input, cx| {
                            input.set_value("   ", window, cx);
                        });
                    tree.commit_rename(window, cx);
                });
            })
            .expect("commit rename");

        assert!(
            events.borrow().is_empty(),
            "a blank name sends no RenamePath"
        );
    }

    #[gpui::test]
    fn test_commit_rename_with_slash_in_name_sends_nothing(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![file("src/main.rs")],
                        true,
                    );
                    tree.selected = Some("src/main.rs".into());
                    tree.start_rename(window, cx);
                    tree.rename
                        .as_ref()
                        .expect("editor open")
                        .input
                        .update(cx, |input, cx| {
                            input.set_value("nested/lib.rs", window, cx);
                        });
                    tree.commit_rename(window, cx);
                });
            })
            .expect("commit rename");

        assert!(
            events.borrow().is_empty(),
            "a typed `/` would silently reparent the file; inline rename never sends it"
        );
    }

    #[gpui::test]
    fn test_commit_rename_with_unchanged_name_sends_nothing(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![file("src/main.rs")],
                        true,
                    );
                    tree.selected = Some("src/main.rs".into());
                    tree.start_rename(window, cx);
                    // Left as the seeded value ("main.rs") — commit with no edit.
                    tree.commit_rename(window, cx);
                });
            })
            .expect("commit rename");

        assert!(
            events.borrow().is_empty(),
            "committing the unchanged name is a no-op send"
        );
    }

    #[gpui::test]
    fn test_cancel_rename_closes_editor_with_no_event(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![file("src/main.rs")],
                        true,
                    );
                    tree.selected = Some("src/main.rs".into());
                    tree.start_rename(window, cx);
                    tree.cancel_rename();
                    cx.notify();
                });
            })
            .expect("cancel rename");

        cx.update(|cx| {
            assert!(tree.read(cx).rename.is_none());
        });
        assert!(events.borrow().is_empty());
    }

    #[gpui::test]
    fn test_apply_file_op_result_ok_rename_arms_pending_reveal(cx: &mut gpui::TestAppContext) {
        // The editor already closed optimistically on `commit_rename` — by
        // the time its `FileOpResult` reply arrives, there is nothing left
        // to close. This mirrors the real flow: `Enter` sends and closes,
        // the daemon reply comes back later.
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![file("src/main.rs")],
                        true,
                    );
                    tree.selected = Some("src/main.rs".into());
                    tree.start_rename(window, cx);
                    tree.rename
                        .as_ref()
                        .expect("editor open")
                        .input
                        .update(cx, |input, cx| {
                            input.set_value("lib.rs", window, cx);
                        });
                    tree.commit_rename(window, cx);
                    assert!(tree.rename.is_none(), "closed optimistically on commit");

                    tree.apply_file_op_result(
                        FileOp::Rename {
                            from: "src/main.rs".into(),
                            to: "src/lib.rs".into(),
                        },
                        true,
                        None,
                        window,
                        cx,
                    );
                });
            })
            .expect("apply file op result");

        cx.update(|cx| {
            let tree = tree.read(cx);
            assert!(
                tree.rename.is_none(),
                "no editor is open once its own successful reply arrives"
            );
            assert_eq!(tree.pending_reveal.as_deref(), Some("src/lib.rs"));
        });
    }

    #[gpui::test]
    fn test_apply_file_op_result_already_exists_reopens_editor_with_error(
        cx: &mut gpui::TestAppContext,
    ) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![file("src/main.rs")],
                        true,
                    );
                    tree.selected = Some("src/main.rs".into());
                    tree.start_rename(window, cx);
                    tree.rename
                        .as_ref()
                        .expect("editor open")
                        .input
                        .update(cx, |input, cx| {
                            input.set_value("lib.rs", window, cx);
                        });
                    tree.commit_rename(window, cx);
                    assert!(tree.rename.is_none(), "closed optimistically on commit");
                    tree.apply_file_op_result(
                        FileOp::Rename {
                            from: "src/main.rs".into(),
                            to: "src/lib.rs".into(),
                        },
                        false,
                        Some(FileOpError::AlreadyExists),
                        window,
                        cx,
                    );
                });
            })
            .expect("apply file op result");

        cx.update(|cx| {
            let tree = tree.read(cx);
            let editor = tree
                .rename
                .as_ref()
                .expect("an AlreadyExists reply re-opens the editor");
            assert_eq!(
                editor.path, "src/main.rs",
                "targets the original, unmoved file"
            );
            assert_eq!(
                editor.input.read(cx).value().as_ref(),
                "lib.rs",
                "the just-typed name is preserved, not the original"
            );
            assert!(editor.error.is_some(), "the error is shown inline");
        });
    }

    #[gpui::test]
    fn test_apply_file_op_result_not_found_does_not_reopen_the_editor(
        cx: &mut gpui::TestAppContext,
    ) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.apply_file_op_result(
                        FileOp::Rename {
                            from: "src/main.rs".into(),
                            to: "src/lib.rs".into(),
                        },
                        false,
                        Some(FileOpError::NotFound),
                        window,
                        cx,
                    );
                });
            })
            .expect("apply file op result");

        cx.update(|cx| {
            assert!(
                tree.read(cx).rename.is_none(),
                "only AlreadyExists/InvalidPath re-open the editor"
            );
        });
    }

    // --- drag & drop move (`FileTree::handle_drop` wiring, #677) -----------

    #[gpui::test]
    fn test_handle_drop_onto_a_directory_emits_rename_requested(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, _window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![dir("src"), file("top.rs")],
                        true,
                    );
                    let dragged = DraggedRow {
                        path: "top.rs".into(),
                        kind: EntryKind::File,
                    };
                    tree.handle_drop(&dragged, &EntryKind::Dir, "src", cx);
                });
            })
            .expect("handle drop");

        assert_eq!(
            events.borrow().as_slice(),
            [FileTreeEvent::RenameRequested {
                from: "top.rs".into(),
                to: "src/top.rs".into(),
            }]
        );
    }

    #[gpui::test]
    fn test_handle_drop_same_parent_noop_sends_nothing(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, _window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![dir("src"), file("src/main.rs")],
                        true,
                    );
                    let dragged = DraggedRow {
                        path: "src/main.rs".into(),
                        kind: EntryKind::File,
                    };
                    // Dropped back on its own parent directory.
                    tree.handle_drop(&dragged, &EntryKind::Dir, "src", cx);
                });
            })
            .expect("handle drop");

        assert!(events.borrow().is_empty(), "a same-parent drop is a no-op");
    }

    #[gpui::test]
    fn test_handle_drop_directory_into_its_own_descendant_sends_nothing(
        cx: &mut gpui::TestAppContext,
    ) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, _window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![dir("src"), dir("src/net")],
                        true,
                    );
                    let dragged = DraggedRow {
                        path: "src".into(),
                        kind: EntryKind::Dir,
                    };
                    tree.handle_drop(&dragged, &EntryKind::Dir, "src/net", cx);
                });
            })
            .expect("handle drop");

        assert!(
            events.borrow().is_empty(),
            "a directory dropped into its own descendant is refused"
        );
    }

    // --- context-menu write group: inline create editor (artboard State D,
    // `docs/spec-explorer-file-ops.md`, #676) ---

    #[gpui::test]
    fn test_start_create_with_a_directory_selected_opens_editor_under_itself(
        cx: &mut gpui::TestAppContext,
    ) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![dir("src"), file("src/main.rs")],
                        true,
                    );
                    tree.selected = Some("src".into());
                    tree.start_create(EntryKind::File, window, cx);
                });
            })
            .expect("start create");

        cx.update(|cx| {
            let tree = tree.read(cx);
            let editor = tree.create.as_ref().expect("create editor opened");
            assert_eq!(editor.parent, "src");
            assert_eq!(editor.kind, EntryKind::File);
            assert_eq!(editor.input.read(cx).value().as_ref(), "");
            assert!(editor.error.is_none());

            let row = tree
                .row_cache
                .iter()
                .find(|r| r.is_pending_create)
                .expect("transient row inserted");
            assert_eq!(row.depth, 1, "one level deeper than its parent \"src\"");
        });
    }

    #[gpui::test]
    fn test_start_create_with_a_file_selected_targets_its_parent(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![dir("src"), file("src/main.rs")],
                        true,
                    );
                    tree.selected = Some("src/main.rs".into());
                    tree.start_create(EntryKind::Dir, window, cx);
                });
            })
            .expect("start create");

        cx.update(|cx| {
            let tree = tree.read(cx);
            let editor = tree.create.as_ref().expect("create editor opened");
            assert_eq!(editor.parent, "src");
            assert_eq!(editor.kind, EntryKind::Dir);
        });
    }

    #[gpui::test]
    fn test_start_create_with_nothing_selected_targets_the_root(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut()
                        .apply_snapshot_chunk("/proj".into(), vec![file("a.rs")], true);
                    tree.start_create(EntryKind::File, window, cx);
                });
            })
            .expect("start create");

        cx.update(|cx| {
            let tree = tree.read(cx);
            let editor = tree.create.as_ref().expect("create editor opened");
            assert_eq!(editor.parent, "");

            let index = tree
                .row_cache
                .iter()
                .position(|r| r.is_pending_create)
                .expect("transient row inserted");
            assert_eq!(index, 0, "the root's transient row leads the list");
            assert_eq!(tree.row_cache[index].depth, 0);
        });
    }

    #[gpui::test]
    fn test_start_create_expands_a_collapsed_target_directory(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![dir("src"), file("src/main.rs")],
                        true,
                    );
                    tree.toggle_dir("src");
                    assert!(tree.is_collapsed("src"));
                    tree.selected = Some("src".into());
                    tree.start_create(EntryKind::File, window, cx);
                });
            })
            .expect("start create");

        cx.update(|cx| {
            assert!(
                !tree.read(cx).is_collapsed("src"),
                "the target expands so the transient row is visible"
            );
        });
    }

    #[gpui::test]
    fn test_start_create_cancels_an_active_rename(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![file("src/main.rs")],
                        true,
                    );
                    tree.selected = Some("src/main.rs".into());
                    tree.start_rename(window, cx);
                    assert!(tree.rename.is_some());
                    tree.start_create(EntryKind::File, window, cx);
                });
            })
            .expect("start create");

        cx.update(|cx| {
            let tree = tree.read(cx);
            assert!(tree.rename.is_none(), "only one inline editor at a time");
            assert!(tree.create.is_some());
        });
    }

    #[gpui::test]
    fn test_start_create_before_any_snapshot_is_a_noop(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.start_create(EntryKind::File, window, cx);
                });
            })
            .expect("start create");

        cx.update(|cx| {
            assert!(tree.read(cx).create.is_none());
        });
    }

    #[gpui::test]
    fn test_commit_create_emits_create_requested_and_closes_editor(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut()
                        .apply_snapshot_chunk("/proj".into(), vec![dir("src")], true);
                    tree.selected = Some("src".into());
                    tree.start_create(EntryKind::File, window, cx);
                    tree.create
                        .as_ref()
                        .expect("editor open")
                        .input
                        .update(cx, |input, cx| {
                            input.set_value("new.rs", window, cx);
                        });
                    tree.commit_create(window, cx);
                });
            })
            .expect("commit create");

        cx.update(|cx| {
            assert!(
                tree.read(cx).create.is_none(),
                "the editor closes immediately on commit"
            );
        });
        assert_eq!(
            events.borrow().as_slice(),
            [FileTreeEvent::CreateRequested {
                path: "src/new.rs".into(),
                kind: EntryKind::File,
            }]
        );
    }

    #[gpui::test]
    fn test_commit_create_dir_at_the_root_emits_create_requested_with_dir_kind(
        cx: &mut gpui::TestAppContext,
    ) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut()
                        .apply_snapshot_chunk("/proj".into(), vec![file("a.rs")], true);
                    tree.start_create(EntryKind::Dir, window, cx);
                    tree.create
                        .as_ref()
                        .expect("editor open")
                        .input
                        .update(cx, |input, cx| {
                            input.set_value("newdir", window, cx);
                        });
                    tree.commit_create(window, cx);
                });
            })
            .expect("commit create");

        assert_eq!(
            events.borrow().as_slice(),
            [FileTreeEvent::CreateRequested {
                path: "newdir".into(),
                kind: EntryKind::Dir,
            }]
        );
    }

    #[gpui::test]
    fn test_commit_create_with_blank_name_sends_nothing(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut()
                        .apply_snapshot_chunk("/proj".into(), vec![dir("src")], true);
                    tree.selected = Some("src".into());
                    tree.start_create(EntryKind::File, window, cx);
                    tree.create
                        .as_ref()
                        .expect("editor open")
                        .input
                        .update(cx, |input, cx| {
                            input.set_value("   ", window, cx);
                        });
                    tree.commit_create(window, cx);
                });
            })
            .expect("commit create");

        assert!(
            events.borrow().is_empty(),
            "a blank name sends no CreateFile/CreateDir"
        );
    }

    #[gpui::test]
    fn test_commit_create_with_slash_in_name_sends_nothing(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut()
                        .apply_snapshot_chunk("/proj".into(), vec![dir("src")], true);
                    tree.selected = Some("src".into());
                    tree.start_create(EntryKind::File, window, cx);
                    tree.create
                        .as_ref()
                        .expect("editor open")
                        .input
                        .update(cx, |input, cx| {
                            input.set_value("nested/new.rs", window, cx);
                        });
                    tree.commit_create(window, cx);
                });
            })
            .expect("commit create");

        assert!(
            events.borrow().is_empty(),
            "a typed `/` is refused client-side; nesting is a later slice"
        );
    }

    #[gpui::test]
    fn test_cancel_create_closes_editor_with_no_event(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        let events = subscribe_events(&tree, cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.model_mut()
                        .apply_snapshot_chunk("/proj".into(), vec![dir("src")], true);
                    tree.selected = Some("src".into());
                    tree.start_create(EntryKind::File, window, cx);
                    tree.cancel_create();
                    cx.notify();
                });
            })
            .expect("cancel create");

        cx.update(|cx| {
            assert!(tree.read(cx).create.is_none());
        });
        assert!(events.borrow().is_empty());
    }

    #[gpui::test]
    fn test_apply_file_op_result_ok_create_file_arms_pending_reveal(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.apply_file_op_result(
                        FileOp::CreateFile {
                            path: "src/new.rs".into(),
                        },
                        true,
                        None,
                        window,
                        cx,
                    );
                });
            })
            .expect("apply file op result");

        cx.update(|cx| {
            let tree = tree.read(cx);
            assert!(tree.create.is_none());
            assert_eq!(tree.pending_reveal.as_deref(), Some("src/new.rs"));
        });
    }

    #[gpui::test]
    fn test_apply_file_op_result_already_exists_create_dir_reopens_editor_with_error(
        cx: &mut gpui::TestAppContext,
    ) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.apply_file_op_result(
                        FileOp::CreateDir {
                            path: "src/newdir".into(),
                        },
                        false,
                        Some(FileOpError::AlreadyExists),
                        window,
                        cx,
                    );
                });
            })
            .expect("apply file op result");

        cx.update(|cx| {
            let tree = tree.read(cx);
            let editor = tree
                .create
                .as_ref()
                .expect("an AlreadyExists reply re-opens the create editor");
            assert_eq!(editor.parent, "src");
            assert_eq!(editor.kind, EntryKind::Dir);
            assert_eq!(editor.input.read(cx).value().as_ref(), "newdir");
            assert!(editor.error.is_some());
        });
    }

    #[gpui::test]
    fn test_apply_file_op_result_not_found_does_not_reopen_the_create_editor(
        cx: &mut gpui::TestAppContext,
    ) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    tree.apply_file_op_result(
                        FileOp::CreateFile {
                            path: "src/new.rs".into(),
                        },
                        false,
                        Some(FileOpError::NotFound),
                        window,
                        cx,
                    );
                });
            })
            .expect("apply file op result");

        cx.update(|cx| {
            assert!(tree.read(cx).create.is_none());
        });
    }

    // --- context-menu write group: destructive delete (artboard State D,
    // `docs/spec-explorer-file-ops.md`, #676) ---

    // Both tests below use `cx.update_window` rather than `window.update`:
    // the latter (`WindowHandle<Root>::update`) leases the `Root` entity for
    // the whole closure, and `has_active_dialog` reads that same `Root`
    // internally — nesting the two double-leases and panics (mirrors the
    // pattern `editor.rs`'s `test_closing_a_clean_tab_...` documents).

    #[gpui::test]
    fn test_confirm_delete_with_nothing_selected_opens_no_dialog(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        cx.update_window(window.into(), |_, window, cx| {
            tree.update(cx, |tree, cx| {
                tree.confirm_delete(window, cx);
            });
            assert!(!window.has_active_dialog(cx));
        })
        .expect("confirm delete");
    }

    #[gpui::test]
    fn test_confirm_delete_with_a_selection_opens_the_confirm_dialog(
        cx: &mut gpui::TestAppContext,
    ) {
        let (tree, window) = open_tree(cx);
        cx.update_window(window.into(), |_, window, cx| {
            tree.update(cx, |tree, cx| {
                tree.model_mut().apply_snapshot_chunk(
                    "/proj".into(),
                    vec![file("src/main.rs")],
                    true,
                );
                tree.selected = Some("src/main.rs".into());
                tree.confirm_delete(window, cx);
            });
            assert!(window.has_active_dialog(cx));
        })
        .expect("confirm delete");
    }

    // --- pending-reveal (`docs/spec-explorer-file-ops.md`, #675) -----------

    #[test]
    fn test_apply_pending_reveal_selects_and_reveals_the_matching_path() {
        let mut tree = seed(vec![dir("src"), file("src/lib.rs")]);
        tree.pending_reveal = Some("src/lib.rs".into());

        tree.apply_pending_reveal(&["src/lib.rs".to_owned()]);

        assert_eq!(tree.selected(), Some("src/lib.rs"));
        assert!(
            tree.pending_reveal.is_none(),
            "the marker clears once the row is revealed"
        );
    }

    #[test]
    fn test_apply_pending_reveal_ignores_unrelated_added_paths() {
        let mut tree = seed(vec![file("a.rs"), file("b.rs")]);
        tree.pending_reveal = Some("b.rs".into());

        tree.apply_pending_reveal(&["a.rs".to_owned()]);

        assert_eq!(tree.selected(), None);
        assert_eq!(
            tree.pending_reveal.as_deref(),
            Some("b.rs"),
            "still armed — the pending path hasn't arrived yet"
        );
    }

    #[test]
    fn test_apply_pending_reveal_with_nothing_pending_is_a_noop() {
        let mut tree = seed(vec![file("a.rs")]);

        tree.apply_pending_reveal(&["a.rs".to_owned()]);

        assert_eq!(tree.selected(), None);
    }
}
