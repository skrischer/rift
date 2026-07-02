//! File-tree render: the navigable explorer panel built from the client
//! worktree model (`docs/spec-editor.md`, the file-tree render debut; #186).
//!
//! The [`WorktreeModel`] is a flat `path -> entry` map (snapshot as source of
//! truth). This view derives a depth-annotated, collapse-aware *visible row*
//! list from it on demand, renders that list virtualized via `gpui-component`'s
//! [`v_virtual_list`] (so a directory with thousands of entries paints only the
//! rows on screen), and lets the user expand/collapse directories and select a
//! file.
//!
//! Bounded to **navigate + open** (the spec's v1 tree scope): selecting a file
//! emits [`FileTreeEvent::OpenFile`] carrying its root-relative path — the clean
//! signal the editor surface (#187) subscribes to. No rich operations
//! (create/rename/delete/move) and no git/diagnostics decoration live here; the
//! model carries that data, but the tree leaves it for a later explorer-panel
//! sub-spec. Selecting changes no tmux pane/window state — this is a pure GUI
//! surface, agent-agnostic by construction (it only ever reads file paths and
//! kinds; it never inspects pane processes or file contents).
//!
//! Implements `gpui-component`'s `Panel` trait directly (`docs/spec-ide-shell.md`,
//! issue #323), so it can be mounted as a dock panel once the shell adopts
//! `DockArea` (#324).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use gpui::{
    div, px, App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, SharedString, Size,
    StatefulInteractiveElement as _, Styled as _, Window,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{v_virtual_list, ActiveTheme as _, VirtualListScrollHandle};
use rift_protocol::EntryKind;

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

/// One rendered row: an entry path, its kind, and its nesting depth. Derived
/// fresh from the model each render — never stored, so it can never drift from
/// the snapshot.
struct Row {
    path: String,
    kind: EntryKind,
    depth: usize,
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
    pub fn model_mut(&mut self) -> &mut WorktreeModel {
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
    }

    /// Build the flattened, depth-annotated list of currently *visible* rows
    /// from the model's flat path map.
    ///
    /// The model keys entries by their root-relative path in a `BTreeMap`, so
    /// iteration is already lexicographically ordered — which, for slash-
    /// separated paths, places every entry directly after its parent and groups
    /// a directory's whole subtree together. That lets a single pass hide
    /// subtrees: when a collapsed directory is seen, its descendants (every path
    /// under `dir/`) are skipped until iteration leaves that prefix.
    fn visible_rows(&self) -> Vec<Row> {
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
            rows.push(Row {
                path: path.clone(),
                kind: entry.kind.clone(),
                depth,
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
            .child(div().w(px(12.0)).flex_shrink_0().child(twisty.to_string()))
            .child(name);

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
        let rows = self.visible_rows();

        // Empty state: no snapshot yet (or an empty root). Keep it quiet — the
        // panel is a passive mirror, not an action surface.
        if rows.is_empty() {
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
            rows.iter()
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
                        // Re-derive the visible rows for the painted range. The
                        // virtual list only asks for the rows currently on
                        // screen, so a huge tree paints a bounded number of
                        // elements regardless of size.
                        let rows = this.visible_rows();
                        visible_range
                            .filter_map(|ix| rows.get(ix).map(|row| this.render_row(row, cx)))
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
}
