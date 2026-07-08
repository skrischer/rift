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
//! **State C**, `docs/spec-explorer-file-ops.md`, #675) and the context-menu
//! write group (artboard **State D**, #676): selecting a file emits
//! [`FileTreeEvent::OpenFile`] carrying its root-relative path — the clean
//! signal the editor surface (#187) subscribes to. Rows carry git status
//! and diagnostic severity from the model, rolled up onto ancestor directories
//! (`compute_rollup`, #329) so a collapsed folder still surfaces a
//! modified/errored descendant; a deleted tracked file, whose own row is gone,
//! rolls its status up onto surviving ancestors the same way (#480). Move
//! (drag & drop) stays a later slice of the same phase — see
//! `docs/spec-explorer-file-ops.md`. Selecting changes no tmux pane/window
//! state — this is a pure GUI surface, agent-agnostic by construction (it only
//! ever reads file paths, kinds, git status, diagnostics, and the `ignored`
//! flag; it never inspects pane processes or file contents). A rename,
//! create, or delete is user intent over the filesystem, sent as a
//! [`FileTreeEvent::RenameRequested`] / [`FileTreeEvent::CreateRequested`] /
//! [`FileTreeEvent::DeleteRequested`] for `workspace.rs` to forward — no
//! different in kind from any other write. *New File…* / *New Folder…* reuse
//! the State-C inline-editor mechanism for a transient, not-yet-real row;
//! *Delete* is gated behind the `#420` destructive confirm-dialog pattern,
//! never batched.
//!
//! Implements `gpui-component`'s `Panel` trait directly (`docs/spec-ide-shell.md`,
//! issue #323), so it can be mounted as a dock panel once the shell adopts
//! `DockArea` (#324).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, px, AnyElement, App, AppContext as _, ClipboardItem, Context, Div, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, ParentElement as _, Pixels, Render, ScrollStrategy, SharedString, Size,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window,
};
use gpui_component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_component::dialog::{AlertDialog, DialogButtonProps};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::input::{
    Escape, Input, InputEvent, InputState, MoveToStart, SelectToNextWordEnd,
};
use gpui_component::menu::{ContextMenuExt as _, PopupMenu};
use gpui_component::{
    h_flex, v_virtual_list, ActiveTheme as _, Icon, IconName, Sizable as _,
    VirtualListScrollHandle, WindowExt as _,
};
use rift_protocol::{
    DiagnosticSeverity, EntryKind, FileOp, FileOpError, GitEntryStatus, GitStatusCode,
};

use crate::file_icons::{self, Glyph};
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

/// Gap between every row slot (chevron, icon, name, diagnostic dot, git
/// letter), from the artboard's row density.
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

/// Fixed width of the reserved icon slot, between the chevron and the name —
/// sized to the artboard's icon glyph (`docs/spec-explorer-redesign.md`).
/// Renders the mapped file-type / folder glyph ([`file_icons`]); unchanged
/// from Phase 27 so no row re-layout follows (`docs/spec-explorer-icons.md`).
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
}

impl FileTree {
    /// Create an empty tree. Feed it daemon worktree messages via
    /// [`FileTree::model_mut`] (then [`Context::notify`]) as they arrive.
    pub fn new() -> Self {
        Self {
            model: WorktreeModel::default(),
            collapsed: HashSet::new(),
            selected: None,
            scroll_handle: VirtualListScrollHandle::new(),
            focus_handle: RefCell::new(None),
            row_cache: Vec::new(),
            cache_dirty: true,
            rename: None,
            _rename_input_sub: None,
            create: None,
            _create_input_sub: None,
            pending_reveal: None,
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
    /// clicked.
    fn click_dir(&mut self, path: &str) {
        self.selected = Some(path.to_owned());
        self.toggle_dir(path);
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
    /// — the header button's and root-row chevron's "fully collapsed" state
    /// (offers Expand once true; `docs/spec-explorer-parity.md`). Deliberately
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
    /// "Collapse all" / root-row chevron while expanded): inserts each
    /// `EntryKind::Dir` path into the existing `collapsed` set. Sets
    /// `cache_dirty` directly, mirroring `toggle_dir`'s discipline, rather
    /// than looping `toggle_dir` itself — which would re-expand a directory
    /// that was already collapsed.
    fn collapse_all(&mut self) {
        for entry in self.model.entries().values() {
            if entry.kind == EntryKind::Dir {
                self.collapsed.insert(entry.path.clone());
            }
        }
        self.cache_dirty = true;
    }

    /// Expand every directory (header "Expand all" / root-row chevron while
    /// collapsed): clears the `collapsed` set wholesale.
    fn expand_all(&mut self) {
        self.collapsed.clear();
        self.cache_dirty = true;
    }

    /// Flip between fully collapsed and fully expanded. The header button
    /// and the workspace-root chevron are two entry points into the same
    /// toggle.
    fn toggle_collapse_all(&mut self) {
        if self.all_dirs_collapsed() {
            self.expand_all();
        } else {
            self.collapse_all();
        }
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
    /// `on_click`.
    fn open_selected(&mut self, cx: &mut Context<Self>) {
        self.refresh_row_cache();
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
    /// *visible* rows from the model's flat path map.
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
    fn visible_rows(&self) -> Vec<Row> {
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
            });

            if entry.kind == EntryKind::Dir && self.collapsed.contains(path) {
                // Hide this directory's subtree: everything under `path/`.
                skip_prefix = Some(format!("{path}/"));
            }
        }

        rows
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
    /// it) with a right-aligned action row. Ships exactly the two actions
    /// that map to a real client capability — collapse-all / expand-all (a
    /// toggle reflecting `all_dirs_collapsed`) and reveal-active — and
    /// consciously omits the artboard's *search/filter* (Phase 31) and *new
    /// file* (Phase 30) glyphs (no client capability yet; see the spec's
    /// prior decisions).
    ///
    /// Both actions render as real `IconName` glyphs via `Button::icon(...)`
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
                    .child(collapse_toggle)
                    .child(reveal_active),
            )
    }

    /// The workspace-root row (`RIFT` in the design) below the header
    /// (`docs/spec-explorer-redesign.md`): the leaf of `model.root()`'s
    /// absolute path, uppercased and bold at the artboard's 12px label
    /// style, with a disclosure chevron that mirrors and drives the
    /// collapse-all/expand-all state, re-densified to the artboard's row
    /// height, padding, and slot gap (shared with [`FileTree::render_row`]'s
    /// [`ROW_HEIGHT`] / [`ROW_SLOT_GAP`]) and its own measured background
    /// tint, which gives the row a subtle band against the panel surface.
    /// Neutral — no label, no chevron, not clickable — while `root()` is
    /// `None`: no snapshot has arrived yet, so there is nothing to name or
    /// toggle.
    fn render_root_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.model.root() else {
            return h_flex()
                .flex_shrink_0()
                .items_center()
                .h(ROW_HEIGHT)
                .px(ROOT_ROW_PADDING_X)
                .gap(ROW_SLOT_GAP)
                .bg(cx.theme().background)
                .child(div().w(px(12.0)).flex_shrink_0())
                .into_any_element();
        };

        let chevron = if self.all_dirs_collapsed() {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        };
        let label = Self::root_leaf(root).to_uppercase();

        h_flex()
            .id("file-tree-root-row")
            .flex_shrink_0()
            .items_center()
            .h(ROW_HEIGHT)
            .px(ROOT_ROW_PADDING_X)
            .gap(ROW_SLOT_GAP)
            .bg(cx.theme().background)
            .text_size(px(12.0))
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().list_hover))
            .child(
                div().w(px(12.0)).flex_shrink_0().child(
                    Icon::new(chevron)
                        .size(px(12.0))
                        .text_color(cx.theme().muted_foreground),
                ),
            )
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .text_color(cx.theme().foreground)
                    .child(label),
            )
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.toggle_collapse_all();
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

    /// Render one row as an interactive element. Clicking a directory selects
    /// it and toggles its expansion; clicking a file selects it and emits the
    /// open signal.
    ///
    /// Slot order, left to right, every slot `flex_shrink_0` so names and the
    /// trailing cluster column-align across rows and depths
    /// (`docs/spec-explorer-redesign.md`): chevron -> reserved icon slot
    /// (mapped file-type / folder glyph, `crate::file_icons`) -> name (the
    /// only flexible slot) -> diagnostic dot -> right-aligned git-status
    /// letter.
    fn render_row(&self, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let is_dir = row.kind == EntryKind::Dir;
        let is_expanded = is_dir && !self.collapsed.contains(&row.path);
        let is_selected = self.selected.as_deref() == Some(row.path.as_str());
        let indent = Self::row_indent(row.depth);

        // Directory disclosure chevron (right collapsed / down expanded); a
        // file gets a same-width blank spacer so names align across kinds
        // (`docs/spec-explorer-icons.md`).
        let twisty = div().w(px(12.0)).flex_shrink_0().when(is_dir, |el| {
            let chevron = if is_expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            };
            el.child(
                Icon::new(chevron)
                    .size(px(12.0))
                    .text_color(cx.theme().muted_foreground),
            )
        });

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
        // static label — every other slot (chevron, icon, indent, row
        // height) stays identical so the row doesn't jump while editing.
        if let Some(editor) = self
            .rename
            .as_ref()
            .filter(|editor| editor.path == row.path)
        {
            return self
                .render_rename_row(row, indent, twisty, icon_slot, editor, cx)
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
                    .render_create_row(indent, twisty, icon_slot, editor, cx)
                    .into_any_element();
            }
        }

        let name = Self::display_name(&row.path).to_owned();
        let path = row.path.clone();

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
        let name_el = div()
            .flex_1()
            .when(is_selected, |el| el.font_weight(FontWeight::BOLD))
            .when_some(git_color, |el, color| el.text_color(color))
            .child(name);

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
            .child(twisty)
            .child(icon_slot)
            .child(name_el)
            .child(severity_dot)
            .child(git_letter);

        if is_selected {
            root = root
                .bg(cx.theme().list_active)
                .border_l_2()
                .border_color(cx.theme().accent)
                .text_color(cx.theme().foreground);
        }

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
        // Drag & drop (move) is a later slice of the same phase.
        root.on_click(cx.listener(move |this, _event, _window, cx| {
            if is_dir {
                this.click_dir(&path);
            } else {
                this.selected = Some(path.clone());
                this.cache_dirty = true;
                // The open signal the editor surface consumes. Selecting a file
                // is the only thing that touches anything outside this view — and
                // it touches nothing but this event; no tmux pane/window state.
                cx.emit(FileTreeEvent::OpenFile { path: path.clone() });
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

    /// The rename-active rendering of one row (artboard State C): chevron +
    /// icon slot unchanged, the name slot replaced by `editor.input`, and — on
    /// an error re-open — the inline message in place of the diagnostic-dot /
    /// git-letter trailing lane. No click / context-menu handlers: the row is
    /// not selectable or openable while its name is being edited.
    fn render_rename_row(
        &self,
        row: &Row,
        indent: Pixels,
        twisty: Div,
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
            .flex()
            .items_center()
            .h(ROW_HEIGHT)
            .py(ROW_BLOCK_PADDING_Y)
            .pl(indent)
            .pr(px(8.0))
            .gap(ROW_SLOT_GAP)
            .rounded(ROW_RADIUS)
            .text_sm()
            .child(twisty)
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
    /// to the cache (artboard **State D**, #676): chevron + icon slot
    /// unchanged (the icon reflects `editor.kind` via the synthetic row's own
    /// `kind`), the name slot is `editor.input`, and — on an error re-open —
    /// the inline message in the trailing lane. Mirrors
    /// [`FileTree::render_rename_row`]; no click / context-menu handlers,
    /// matching that row's "not yet a real entry" affordance.
    fn render_create_row(
        &self,
        indent: Pixels,
        twisty: Div,
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
            .flex()
            .items_center()
            .h(ROW_HEIGHT)
            .py(ROW_BLOCK_PADDING_Y)
            .pl(indent)
            .pr(px(8.0))
            .gap(ROW_SLOT_GAP)
            .rounded(ROW_RADIUS)
            .text_sm()
            .child(twisty)
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
        // redesign's less cramped density.
        let content = if let Some(state) = self.empty_state() {
            let message = match state {
                EmptyState::Loading => "Loading\u{2026}",
                EmptyState::EmptyRoot => "Empty folder",
            };
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
            // `Escape` while the rename or create input has focus:
            // `InputState::escape` propagates the action (it is not in
            // `clean_on_escape` mode), so it bubbles here to close whichever
            // editor is open with no send. A no-op (and re-propagated) when
            // neither is active, so an ancestor gets a chance at a plain
            // Escape too.
            .on_action(cx.listener(|this, _: &Escape, _window, cx| {
                if this.rename.is_some() {
                    this.cancel_rename();
                    cx.notify();
                } else if this.create.is_some() {
                    this.cancel_create();
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            .child(self.render_header(cx))
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
        // The root row shares the row-anatomy step's slot gap rather than
        // introducing its own — the artboard measures the same 6px.
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

    #[gpui::test]
    fn test_confirm_delete_with_nothing_selected_opens_no_dialog(cx: &mut gpui::TestAppContext) {
        let (tree, window) = open_tree(cx);
        window
            .update(cx, |_, window, cx| {
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
        window
            .update(cx, |_, window, cx| {
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
