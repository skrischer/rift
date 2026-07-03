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
//! Bounded to **navigate + open + decorate** (the spec's v1 tree scope):
//! selecting a file emits [`FileTreeEvent::OpenFile`] carrying its root-relative
//! path — the clean signal the editor surface (#187) subscribes to. Rows carry
//! git status and diagnostic severity from the model, rolled up onto ancestor
//! directories (`compute_rollup`, #329) so a collapsed folder still surfaces a
//! modified/errored descendant. No rich operations (create/rename/delete/move)
//! live here — that stays a later explorer-panel sub-spec. Selecting changes no
//! tmux pane/window state — this is a pure GUI surface, agent-agnostic by
//! construction (it only ever reads file paths, kinds, git status, diagnostics,
//! and the `ignored` flag; it never inspects pane processes or file contents).
//!
//! Implements `gpui-component`'s `Panel` trait directly (`docs/spec-ide-shell.md`,
//! issue #323), so it can be mounted as a dock panel once the shell adopts
//! `DockArea` (#324).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    div, px, App, Context, EventEmitter, FluentBuilder as _, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, SharedString, Size,
    StatefulInteractiveElement as _, Styled as _, Window,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{v_virtual_list, ActiveTheme as _, VirtualListScrollHandle};
use rift_protocol::{DiagnosticSeverity, EntryKind, GitEntryStatus, GitStatusCode};

use crate::worktree::WorktreeModel;

/// Stable, distinct dock-panel identity for the file tree (`Panel::panel_name`).
/// Once shipped this must not change — it is the persisted panel identifier.
pub const FILE_TREE_PANEL_NAME: &str = "explorer";

/// Fixed row height for every tree entry. The virtual list needs a height per
/// item; a uniform row keeps the size vector trivial to build and the scroll
/// math exact.
const ROW_HEIGHT: Pixels = px(22.0);

/// Horizontal indent applied per nesting level, so depth reads visually.
const INDENT_PER_LEVEL: f32 = 14.0;

/// The open signal the tree emits when the user selects a file — the clean
/// interface the editor surface (#187) consumes via `cx.subscribe`. Carries the
/// file's path relative to the worktree root (the same key space as
/// [`WorktreeModel`] entries); the editor resolves it against the daemon root
/// when it issues its read request. Only files emit this — selecting a directory
/// toggles its expansion instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileTreeEvent {
    /// A file was selected; open it. `path` is root-relative.
    OpenFile { path: String },
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
}

/// A directory's or file's rolled-up git status, ordered by the roll-up's
/// rendering precedence (`docs/spec-explorer-panel.md`: `conflicted > changed
/// > untracked > clean`). Declared low-to-high so `Option<GitRollupStatus>`'s
/// derived `Ord` (`None` sorts below every variant, standing in for "clean")
/// lets [`Rollup::merge`] pick the worst of two with a plain `max`.
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
    /// handled rather than assumed away.
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
/// surface a hidden descendant's status.
///
/// Relies on the same contiguous-subtree property of the sorted path map that
/// [`FileTree::visible_rows`] does: because entries are keyed in a
/// `BTreeMap`, a directory's descendants are exactly the run of paths sharing
/// its `dir/` prefix that immediately follows it. That lets a stack of
/// currently open ancestor directories be maintained by depth alone — an
/// entry no deeper than the shallowest open ancestor means that ancestor's
/// subtree has ended — and each entry's own decoration folds into every
/// still-open ancestor as it is visited, with no per-row descendant walk.
fn compute_rollup(model: &WorktreeModel) -> HashMap<String, Rollup> {
    let mut result: HashMap<String, Rollup> = HashMap::new();
    // Shallowest-first stack of ancestor directories whose subtree is still
    // being iterated, paired with their depth.
    let mut open_dirs: Vec<(usize, String)> = Vec::new();

    for (path, entry) in model.entries() {
        let depth = path.bytes().filter(|&b| b == b'/').count();
        while open_dirs
            .last()
            .is_some_and(|(open_depth, _)| *open_depth >= depth)
        {
            open_dirs.pop();
        }

        let own = Rollup {
            git_status: model
                .git_status(path)
                .and_then(GitRollupStatus::from_status),
            severity: own_severity(model, path),
        };

        for (_, ancestor) in &open_dirs {
            result.entry(ancestor.clone()).or_default().merge(own);
        }
        result.entry(path.clone()).or_default().merge(own);

        if entry.kind == EntryKind::Dir {
            open_dirs.push((depth, path.clone()));
        }
    }

    result
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

    /// Rebuild [`FileTree::row_cache`] from the model when [`FileTree::cache_dirty`]
    /// is set; a no-op otherwise. The single path that calls
    /// [`FileTree::visible_rows`] — `render()` calls this once per paint instead
    /// of deriving the visible-row list itself, and the virtual list's row
    /// closure only ever reads the resulting [`FileTree::row_cache`].
    fn refresh_row_cache(&mut self) {
        if self.cache_dirty {
            self.row_cache = self.visible_rows();
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

    /// Render one row as an interactive element. Clicking a directory toggles
    /// its expansion; clicking a file selects it and emits the open signal.
    fn render_row(&self, row: &Row, cx: &mut Context<Self>) -> impl IntoElement {
        let is_dir = row.kind == EntryKind::Dir;
        let is_selected = self.selected.as_deref() == Some(row.path.as_str());
        let indent = px(row.depth as f32 * INDENT_PER_LEVEL);

        // Directory disclosure glyph (text, not an icon: the product binary does
        // not embed gpui-component's SVG icon assets, so a glyph renders reliably
        // either way). A file gets a blank spacer of the same width so names
        // align across kinds.
        let twisty = if is_dir {
            if self.collapsed.contains(&row.path) {
                "\u{203a}" // single right-pointing angle quotation mark
            } else {
                "\u{2304}" // down arrowhead
            }
        } else {
            " "
        };

        let name = Self::display_name(&row.path).to_owned();
        let path = row.path.clone();

        // Diagnostic-severity indicator: a small colored dot, or an empty
        // same-size spacer when the row (or, for a directory, everything
        // beneath it) is clean — keeping every row's layout aligned.
        let severity_dot = row.severity.map(|severity| {
            let color = match severity {
                DiagnosticSeverity::Error => cx.theme().danger,
                DiagnosticSeverity::Warning => cx.theme().warning,
                DiagnosticSeverity::Information => cx.theme().info,
                DiagnosticSeverity::Hint => cx.theme().muted_foreground,
            };
            div()
                .size(px(6.0))
                .flex_shrink_0()
                .rounded(px(3.0))
                .bg(color)
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
            .when_some(git_color, |el, color| el.text_color(color))
            .child(name);

        // Git-status badge: a single letter after the name, colored the same
        // as the name tint.
        let git_badge = row.git_status.map(|status| {
            div()
                .text_xs()
                .text_color(match status {
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
            .pl(indent)
            .pr(px(8.0))
            .gap(px(4.0))
            .text_sm()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().list_hover))
            // Ignored entries (not yet shown by default — #309) render dimmed
            // rather than hidden, once the daemon starts sending them.
            .when(row.ignored, |el| el.opacity(0.55))
            .child(div().w(px(12.0)).flex_shrink_0().child(twisty.to_string()))
            .child(div().w(px(8.0)).flex_shrink_0().children(severity_dot))
            .child(name_el)
            .children(git_badge);

        if is_selected {
            root = root
                .bg(cx.theme().list_active)
                .text_color(cx.theme().foreground);
        }

        root.on_click(cx.listener(move |this, _event, _window, cx| {
            if is_dir {
                this.toggle_dir(&path);
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

        // Empty state: no snapshot yet (or an empty root). Keep it quiet — the
        // panel is a passive mirror, not an action surface.
        if self.row_cache.is_empty() {
            return div()
                .size_full()
                .p(px(8.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No files")
                .into_any_element();
        }

        // One uniform row height per item — the size vector the virtual list
        // measures against. Width is ignored for a vertical list.
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
}
