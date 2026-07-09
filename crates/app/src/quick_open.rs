//! Jump-to-file quick-open modal (`docs/spec-explorer-search.md`, issue
//! #681): a `Root` dialog overlay modeled on [`crate::command_palette`],
//! listing every file in the streamed worktree (`WorktreeModel::entries()`)
//! ranked by the [`crate::fuzzy_match`] substrate. Opened by
//! [`OpenQuickOpen`] (bound to `Ctrl+Shift+O` / `Cmd+Shift+O` in `main.rs`,
//! unscoped like [`crate::command_palette::OpenCommandPalette`], so the
//! shortcut reaches quick-open regardless of which surface currently has
//! focus, including the terminal).
//!
//! Confirming a row emits [`crate::file_tree::FileTreeEvent::OpenFile`]
//! through the shared `file_tree` entity — the exact event a tree click
//! already emits — so `workspace.rs`'s existing subscription opens the file,
//! and the existing post-load reveal (`WorkspaceView::reveal_open_file_in_tree`,
//! fired for every completed file load regardless of origin) selects and
//! scrolls to it in the tree. No new open path, no new protocol.

use gpui::{
    px, App, AppContext as _, Context, Entity, ParentElement as _, Styled as _, Task, Window,
};
use gpui_component::label::Label;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::{IndexPath, WindowExt as _};
use rift_protocol::EntryKind;

use crate::file_tree::{FileTree, FileTreeEvent};
use crate::fuzzy_match::fuzzy_match;
use crate::worktree::WorktreeModel;

/// Every file in `model` (directories excluded) whose path fuzzy-matches
/// `query`, ranked by [`fuzzy_match`]'s score descending. `sort_by_key` is a
/// stable sort, so ties keep `entries()`'s `BTreeMap` (path-ascending) order.
/// An empty query matches (and lists) every file, all scoring `0`
/// (`fuzzy_match`'s own empty-query contract) — quick-open's "list every
/// file" state.
fn filter_files(model: &WorktreeModel, query: &str) -> Vec<String> {
    let mut matches: Vec<(String, u32)> = model
        .entries()
        .iter()
        .filter(|(_, entry)| entry.kind == EntryKind::File)
        .filter_map(|(path, _)| fuzzy_match(query, path).map(|m| (path.clone(), m.score)))
        .collect();
    matches.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
    matches.into_iter().map(|(path, _)| path).collect()
}

/// Open the jump-to-file quick-open. Bound with no key-context scope in
/// `main.rs` (mirroring `OpenCommandPalette`), so the shortcut opens
/// quick-open from anywhere, including while the terminal is focused.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = rift, no_json)]
pub struct OpenQuickOpen;

/// Quick-open's `ListDelegate`: renders the files currently matching the
/// query as a plain `gpui_component::list::ListItem` (its root-relative
/// path). Holds the shared `file_tree` entity both to read the corpus
/// (`WorktreeModel::entries()`) and, on confirm, to emit the open request
/// through it.
struct QuickOpenDelegate {
    file_tree: Entity<FileTree>,
    /// Root-relative paths of the files currently shown, ranked by
    /// [`filter_files`]. Rebuilt on every query change by `perform_search`;
    /// starts empty until [`QuickOpen::open`] seeds it.
    matches: Vec<String>,
    selected_index: Option<IndexPath>,
}

impl ListDelegate for QuickOpenDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.matches.len()
    }

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.matches = filter_files(self.file_tree.read(cx).model(), query);
        cx.notify();
        Task::ready(())
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let path = self.matches.get(ix.row)?;
        let selected = Some(ix) == self.selected_index;
        Some(
            ListItem::new(ix)
                .selected(selected)
                .child(Label::new(path.clone())),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_index = ix;
        cx.notify();
    }

    /// Close the dialog, then emit `FileTreeEvent::OpenFile` on the shared
    /// `file_tree` entity — `workspace.rs`'s existing subscription to that
    /// entity opens the file exactly as a tree click would (`docs/spec-
    /// explorer-search.md`: "no new open path"). Order mirrors
    /// `CommandPaletteDelegate::confirm` (#434): closing first restores the
    /// pre-quick-open focus synchronously before anything downstream runs.
    fn confirm(
        &mut self,
        _secondary: bool,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        let path = self
            .selected_index
            .and_then(|ix| self.matches.get(ix.row))
            .cloned();

        window.close_dialog(cx);

        if let Some(path) = path {
            self.file_tree.update(cx, |_tree, cx| {
                cx.emit(FileTreeEvent::OpenFile { path });
            });
        }
    }
}

/// The width of the quick-open dialog.
const QUICK_OPEN_WIDTH: f32 = 480.0;

/// The max height of quick-open's file list before it scrolls.
const QUICK_OPEN_LIST_MAX_HEIGHT: f32 = 360.0;

/// Owns quick-open's [`ListState`] entity for the workspace's lifetime, so
/// reopening it reuses the same list rather than rebuilding the corpus scan
/// from scratch each time.
pub struct QuickOpen {
    list: Entity<ListState<QuickOpenDelegate>>,
}

impl QuickOpen {
    pub fn new(file_tree: Entity<FileTree>, window: &mut Window, cx: &mut App) -> Self {
        let delegate = QuickOpenDelegate {
            file_tree,
            matches: Vec::new(),
            selected_index: None,
        };
        let list = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));
        Self { list }
    }

    /// Open quick-open as a `Root` dialog. Resets the state left over from
    /// the previous time it was opened (mirroring
    /// `CommandPalette::open`'s #434 fix), so it always starts with an empty
    /// query listing every file and the first row selected, so Enter on a
    /// freshly opened quick-open jumps to the top match.
    pub fn open(&self, window: &mut Window, cx: &mut App) {
        self.list.update(cx, |list, cx| {
            list.set_query("", window, cx);
            let file_tree = list.delegate().file_tree.clone();
            list.delegate_mut().matches = filter_files(file_tree.read(cx).model(), "");
            list.set_selected_index(Some(IndexPath::default()), window, cx);
            list.scroll_to_selected_item(window, cx);
        });

        let list = self.list.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title("Go to File")
                .close_button(false)
                .w(px(QUICK_OPEN_WIDTH))
                .child(
                    List::new(&list)
                        .search_placeholder("Go to file...")
                        .max_h(px(QUICK_OPEN_LIST_MAX_HEIGHT)),
                )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::TestAppContext;
    use gpui_component::Root;
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

    fn model_with(entries: Vec<WorktreeEntry>) -> WorktreeModel {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk("/proj".into(), entries, true);
        model
    }

    #[test]
    fn test_filter_files_empty_query_lists_every_file_excluding_directories() {
        let model = model_with(vec![dir("src"), file("src/main.rs"), file("README.md")]);

        let mut matches = filter_files(&model, "");
        matches.sort();

        assert_eq!(matches, vec!["README.md", "src/main.rs"]);
    }

    #[test]
    fn test_filter_files_ranks_matches_by_score_descending() {
        let model = model_with(vec![file("main.rs"), file("m_a_i_n.rs")]);

        let matches = filter_files(&model, "main");

        assert_eq!(matches, vec!["main.rs", "m_a_i_n.rs"]);
    }

    #[test]
    fn test_filter_files_no_match_returns_empty() {
        let model = model_with(vec![file("main.rs")]);

        assert!(filter_files(&model, "zzz-no-such-file").is_empty());
    }

    #[test]
    fn test_filter_files_never_returns_a_directory() {
        let model = model_with(vec![dir("target"), file("target/debug")]);

        // "target" fuzzy-matches the directory's own path too, but only the
        // file entry may ever appear in quick-open's flat file list.
        let matches = filter_files(&model, "target");

        assert_eq!(matches, vec!["target/debug"]);
    }

    /// Bare render stub hosting quick-open's dialog in a `Root`-wrapped test
    /// window, mirroring `command_palette.rs`'s `palette_window` harness.
    struct StubView;

    impl gpui::Render for StubView {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
        }
    }

    fn quick_open_window(
        cx: &mut TestAppContext,
    ) -> (QuickOpen, Entity<FileTree>, gpui::WindowHandle<Root>) {
        let mut quick_open: Option<QuickOpen> = None;
        let mut file_tree: Option<Entity<FileTree>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let tree = cx.new(|_| {
                    let mut tree = FileTree::new();
                    tree.model_mut().apply_snapshot_chunk(
                        "/proj".into(),
                        vec![file("main.rs"), file("lib.rs")],
                        true,
                    );
                    tree
                });
                quick_open = Some(QuickOpen::new(tree.clone(), window, cx));
                file_tree = Some(tree);
                let stub = cx.new(|_| StubView);
                cx.new(|cx| Root::new(stub, window, cx))
            })
            .unwrap()
        });
        (
            quick_open.expect("quick-open constructed inside the window callback"),
            file_tree.expect("file tree constructed inside the window callback"),
            window,
        )
    }

    /// `open` always starts from a full, unfiltered file list with the first
    /// row selected, regardless of state a previous session left behind
    /// (mirrors `command_palette.rs`'s #434 regression test).
    #[gpui::test]
    fn test_open_lists_every_file_and_selects_the_first_row(cx: &mut TestAppContext) {
        let (quick_open, _file_tree, window) = quick_open_window(cx);

        cx.update_window(window.into(), |_, window, cx| {
            quick_open.list.update(cx, |list, cx| {
                list.set_query("zzz-stale-query", window, cx);
                list.delegate_mut().matches = Vec::new();
                list.set_selected_index(None, window, cx);
            });

            quick_open.open(window, cx);

            let list = quick_open.list.read(cx);
            let mut matches = list.delegate().matches.clone();
            matches.sort();
            assert_eq!(matches, vec!["lib.rs", "main.rs"]);
            assert_eq!(
                list.selected_index(),
                Some(IndexPath::default()),
                "reopening selects the first row so Enter jumps to the top match"
            );
        })
        .unwrap();
    }

    /// Confirming a row closes the dialog and emits `FileTreeEvent::OpenFile`
    /// on the shared `file_tree` entity — the existing open path a tree
    /// click already drives, not a new one.
    #[gpui::test]
    fn test_confirm_closes_the_dialog_and_emits_open_file_on_the_shared_file_tree(
        cx: &mut TestAppContext,
    ) {
        let (quick_open, file_tree, window) = quick_open_window(cx);

        let opened = std::rc::Rc::new(std::cell::RefCell::new(None));
        let opened_write = opened.clone();
        cx.update_window(window.into(), |_, window, cx| {
            cx.subscribe(&file_tree, move |_tree, event: &FileTreeEvent, _cx| {
                if let FileTreeEvent::OpenFile { path } = event {
                    *opened_write.borrow_mut() = Some(path.clone());
                }
            })
            .detach();

            quick_open.open(window, cx);
            assert!(window.has_active_dialog(cx), "open sets an active dialog");

            quick_open.list.update(cx, |list, cx| {
                list.set_selected_index(Some(IndexPath::default()), window, cx);
                list.delegate_mut().confirm(false, window, cx);
            });

            assert!(
                !window.has_active_dialog(cx),
                "confirm closes the quick-open dialog"
            );
        })
        .unwrap();

        assert_eq!(
            opened.borrow().as_deref(),
            Some("lib.rs"),
            "confirm emits OpenFile for the selected (first, lib.rs before main.rs) match"
        );
    }
}
