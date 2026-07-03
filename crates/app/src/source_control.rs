//! Source-control panel: the changed-file list docked into the right dock
//! (`docs/spec-source-control.md`, issue #337).
//!
//! Lists the working tree's changed files, grouped and labeled by status
//! (added/modified/deleted/renamed/untracked). The list is derived from the
//! same [`WorktreeModel`] the file tree already mirrors — the daemon's
//! `UpdateGitStatus`/`RepoState` stream is folded there by `WorkspaceView`, and
//! this panel only reads it (no re-derivation, no new protocol). It holds the
//! tree's `Entity` and observes it for repaint; it never mutates the model.
//!
//! Read-only, agent-agnostic: selecting a row only emits
//! [`SourceControlEvent::OpenDiff`] for the diff view (#338) to consume later —
//! there is no git write path here, and nothing here inspects agent output.

use gpui::{
    div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::ActiveTheme as _;
use rift_protocol::{GitEntryStatus, GitStatusCode};

use crate::file_tree::FileTree;
use crate::worktree::WorktreeModel;

/// Stable, distinct dock-panel identity for the source-control panel
/// (`Panel::panel_name`). Once shipped this must not change — it is the
/// persisted panel identifier.
pub const SOURCE_CONTROL_PANEL_NAME: &str = "source-control";

/// Fixed row height for every list row (file row and group header alike).
const ROW_HEIGHT: Pixels = px(22.0);

/// The open-diff signal the panel emits when the user selects a changed file —
/// the clean interface the diff view (#338) will subscribe to. `path` is
/// root-relative, the same key space as [`WorktreeModel`] entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceControlEvent {
    /// A changed file was selected; open its diff.
    OpenDiff { path: String },
}

/// One of the five buckets a changed file is grouped and labeled under in the
/// panel. `Ord` fixes the group display order (declaration order) independent
/// of iteration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

impl ChangeStatus {
    /// The group header label shown in the panel.
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Modified => "Modified",
            Self::Deleted => "Deleted",
            Self::Renamed => "Renamed",
            Self::Untracked => "Untracked",
        }
    }

    /// Classify one path's git status into the panel's five display groups.
    ///
    /// The worktree (unstaged) side wins when it carries a change; a path
    /// changed only in the index (staged, worktree clean) falls back to the
    /// index side — matching `git status`'s own precedence (a staged-then-
    /// further-edited file shows its current, unstaged, state). `Copied` folds
    /// into `Added` (a copy is a new path from the review's perspective);
    /// `TypeChange` and `Unmerged` (conflict) fold into `Modified` — the spec's
    /// five buckets have no dedicated slot for them, and both are, at bottom,
    /// "this existing path changed."
    fn from_git_entry_status(status: GitEntryStatus) -> Self {
        let code = if status.worktree != GitStatusCode::Unmodified {
            status.worktree
        } else {
            status.index
        };
        match code {
            GitStatusCode::Added | GitStatusCode::Copied => Self::Added,
            GitStatusCode::Deleted => Self::Deleted,
            GitStatusCode::Renamed => Self::Renamed,
            GitStatusCode::Untracked => Self::Untracked,
            GitStatusCode::Modified
            | GitStatusCode::TypeChange
            | GitStatusCode::Unmerged
            | GitStatusCode::Unmodified => Self::Modified,
        }
    }
}

/// Derive the changed-file list from the model, grouped by [`ChangeStatus`] in
/// a fixed display order and, within a group, in path order (the source
/// `BTreeMap`'s own order). Pure derivation — headless-testable, no GPUI
/// involved — so the model stays the single source of truth (no separate fold).
pub fn grouped_changed_files(model: &WorktreeModel) -> Vec<(ChangeStatus, Vec<String>)> {
    const ORDER: [ChangeStatus; 5] = [
        ChangeStatus::Added,
        ChangeStatus::Modified,
        ChangeStatus::Deleted,
        ChangeStatus::Renamed,
        ChangeStatus::Untracked,
    ];

    let mut groups: std::collections::BTreeMap<ChangeStatus, Vec<String>> =
        std::collections::BTreeMap::new();
    for (path, status) in model.git_statuses() {
        groups
            .entry(ChangeStatus::from_git_entry_status(*status))
            .or_default()
            .push(path.clone());
    }

    ORDER
        .into_iter()
        .filter_map(|status| groups.remove(&status).map(|paths| (status, paths)))
        .collect()
}

/// The source-control panel: a read-only, grouped view of the changed-file
/// list.
///
/// Bounded to **list + select** (this step's scope): selecting a file emits
/// [`SourceControlEvent::OpenDiff`], the clean signal the diff view (#338)
/// subscribes to. No git write operations live here — the agent runs git in
/// the terminal, this panel only visualizes its result.
///
/// Implements `gpui-component`'s `Panel` trait directly, mirroring
/// [`crate::file_tree::FileTree`] and [`crate::terminal_panel::TerminalPanel`],
/// so it mounts into the right dock the same way those mount into the left
/// dock / center split.
pub struct SourceControlPanel {
    /// The file tree's entity, read (never mutated) for its mirrored
    /// [`WorktreeModel`] — the single source of truth for git status, folded
    /// there by `WorkspaceView` from the daemon stream. Observed so a status
    /// fold's `cx.notify()` on the tree repaints this panel too.
    file_tree: Entity<FileTree>,
    /// The currently selected changed file's path, or `None`.
    selected: Option<String>,
    focus_handle: FocusHandle,
    _observe_tree: Subscription,
}

impl SourceControlPanel {
    /// Create the panel around the workspace's existing file-tree entity — the
    /// shared `WorktreeModel` this panel reads, never its own copy.
    pub fn new(file_tree: Entity<FileTree>, cx: &mut Context<Self>) -> Self {
        let observe_tree = cx.observe(&file_tree, |_this, _tree, cx| cx.notify());
        Self {
            file_tree,
            selected: None,
            focus_handle: cx.focus_handle(),
            _observe_tree: observe_tree,
        }
    }

    /// The currently selected path, if any — the headless handle for the
    /// selection state.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Render one group header (status label + count). Not interactive —
    /// selecting toggles nothing here, only file rows are clickable.
    fn render_group_header(
        &self,
        status: ChangeStatus,
        count: usize,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .h(ROW_HEIGHT)
            .px(px(8.0))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(format!("{} ({count})", status.label()))
    }

    /// Render one changed-file row. Clicking selects it and emits the open-diff
    /// signal the diff view (#338) consumes — the only thing selecting touches;
    /// no git write path, no tmux pane/window state.
    fn render_row(&self, path: String, cx: &mut Context<Self>) -> impl IntoElement {
        let is_selected = self.selected.as_deref() == Some(path.as_str());
        let click_path = path.clone();

        let mut row = div()
            .id(SharedString::from(format!("source-control-{path}")))
            .flex()
            .items_center()
            .h(ROW_HEIGHT)
            .pl(px(20.0))
            .pr(px(8.0))
            .text_sm()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().list_hover))
            .child(path.clone());

        if is_selected {
            row = row
                .bg(cx.theme().list_active)
                .text_color(cx.theme().foreground);
        }

        row.on_click(cx.listener(move |this, _event, _window, cx| {
            this.selected = Some(click_path.clone());
            cx.emit(SourceControlEvent::OpenDiff {
                path: click_path.clone(),
            });
            cx.notify();
        }))
    }
}

impl Focusable for SourceControlPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<SourceControlEvent> for SourceControlPanel {}
impl EventEmitter<PanelEvent> for SourceControlPanel {}

impl Panel for SourceControlPanel {
    fn panel_name(&self) -> &'static str {
        SOURCE_CONTROL_PANEL_NAME
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Source Control")
    }
}

impl gpui::Render for SourceControlPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = grouped_changed_files(self.file_tree.read(cx).model());

        if groups.is_empty() {
            return div()
                .size_full()
                .p(px(8.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No changes")
                .into_any_element();
        }

        let mut root = div().size_full().flex().flex_col();
        for (status, paths) in groups {
            root = root.child(self.render_group_header(status, paths.len(), cx));
            for path in paths {
                root = root.child(self.render_row(path, cx));
            }
        }
        root.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_protocol::{AheadBehind, GitStatusCode, GitStatusEntry, WorktreeEntry};

    fn status(index: GitStatusCode, worktree: GitStatusCode) -> GitEntryStatus {
        GitEntryStatus { index, worktree }
    }

    fn git_entry(path: &str, index: GitStatusCode, worktree: GitStatusCode) -> GitStatusEntry {
        GitStatusEntry {
            path: path.to_owned(),
            status: status(index, worktree),
        }
    }

    fn file(path: &str) -> WorktreeEntry {
        WorktreeEntry {
            path: path.to_owned(),
            kind: rift_protocol::EntryKind::File,
            ignored: false,
            mtime: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    // --- ChangeStatus::from_git_entry_status ---

    #[test]
    fn test_classify_prefers_worktree_side_when_changed() {
        // Staged then further edited: worktree side (the current, unstaged
        // state) wins over the index side.
        assert_eq!(
            ChangeStatus::from_git_entry_status(status(
                GitStatusCode::Added,
                GitStatusCode::Modified
            )),
            ChangeStatus::Modified
        );
    }

    #[test]
    fn test_classify_falls_back_to_index_side_when_worktree_clean() {
        // Staged, not further edited: worktree is clean, so the index side (the
        // staged add) determines the group.
        assert_eq!(
            ChangeStatus::from_git_entry_status(status(
                GitStatusCode::Added,
                GitStatusCode::Unmodified
            )),
            ChangeStatus::Added
        );
    }

    #[test]
    fn test_classify_untracked_is_worktree_only() {
        assert_eq!(
            ChangeStatus::from_git_entry_status(status(
                GitStatusCode::Unmodified,
                GitStatusCode::Untracked
            )),
            ChangeStatus::Untracked
        );
    }

    #[test]
    fn test_classify_deleted_and_renamed() {
        assert_eq!(
            ChangeStatus::from_git_entry_status(status(
                GitStatusCode::Unmodified,
                GitStatusCode::Deleted
            )),
            ChangeStatus::Deleted
        );
        assert_eq!(
            ChangeStatus::from_git_entry_status(status(
                GitStatusCode::Unmodified,
                GitStatusCode::Renamed
            )),
            ChangeStatus::Renamed
        );
    }

    #[test]
    fn test_classify_copied_folds_into_added() {
        assert_eq!(
            ChangeStatus::from_git_entry_status(status(
                GitStatusCode::Unmodified,
                GitStatusCode::Copied
            )),
            ChangeStatus::Added
        );
    }

    #[test]
    fn test_classify_type_change_and_unmerged_fold_into_modified() {
        assert_eq!(
            ChangeStatus::from_git_entry_status(status(
                GitStatusCode::Unmodified,
                GitStatusCode::TypeChange
            )),
            ChangeStatus::Modified
        );
        assert_eq!(
            ChangeStatus::from_git_entry_status(status(
                GitStatusCode::Unmerged,
                GitStatusCode::Unmerged
            )),
            ChangeStatus::Modified
        );
    }

    // --- grouped_changed_files ---

    fn seed_model() -> WorktreeModel {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk(
            "/proj".into(),
            vec![
                file("staged.rs"),
                file("dirty.rs"),
                file("loose.rs"),
                file("gone.rs"),
                file("moved.rs"),
            ],
            true,
        );
        model.apply_git_update(
            vec![
                git_entry("staged.rs", GitStatusCode::Added, GitStatusCode::Unmodified),
                git_entry(
                    "dirty.rs",
                    GitStatusCode::Unmodified,
                    GitStatusCode::Modified,
                ),
                git_entry(
                    "loose.rs",
                    GitStatusCode::Unmodified,
                    GitStatusCode::Untracked,
                ),
                git_entry("gone.rs", GitStatusCode::Unmodified, GitStatusCode::Deleted),
                git_entry(
                    "moved.rs",
                    GitStatusCode::Unmodified,
                    GitStatusCode::Renamed,
                ),
            ],
            vec![],
        );
        model.apply_repo_state(
            Some("main".into()),
            Some(AheadBehind {
                ahead: 0,
                behind: 0,
            }),
        );
        model
    }

    #[test]
    fn test_grouped_changed_files_lists_exactly_the_changed_set() {
        let model = seed_model();
        let groups = grouped_changed_files(&model);

        let flattened: Vec<(ChangeStatus, &str)> = groups
            .iter()
            .flat_map(|(status, paths)| paths.iter().map(move |p| (*status, p.as_str())))
            .collect();

        assert_eq!(
            flattened,
            vec![
                (ChangeStatus::Added, "staged.rs"),
                (ChangeStatus::Modified, "dirty.rs"),
                (ChangeStatus::Deleted, "gone.rs"),
                (ChangeStatus::Renamed, "moved.rs"),
                (ChangeStatus::Untracked, "loose.rs"),
            ]
        );
    }

    #[test]
    fn test_grouped_changed_files_omits_empty_groups() {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk("/proj".into(), vec![file("a.rs")], true);
        model.apply_git_update(
            vec![git_entry(
                "a.rs",
                GitStatusCode::Unmodified,
                GitStatusCode::Untracked,
            )],
            vec![],
        );

        let groups = grouped_changed_files(&model);
        assert_eq!(
            groups,
            vec![(ChangeStatus::Untracked, vec!["a.rs".to_owned()])]
        );
    }

    #[test]
    fn test_grouped_changed_files_empty_model_yields_no_groups() {
        let model = WorktreeModel::default();
        assert!(grouped_changed_files(&model).is_empty());
    }

    #[test]
    fn test_committing_a_file_removes_it_from_the_next_grouping() {
        // Mirrors the acceptance criterion: a status tick (`cleared`) drops a
        // committed file from the list on the next re-derivation.
        let mut model = seed_model();
        assert!(grouped_changed_files(&model)
            .iter()
            .any(|(_, paths)| paths.iter().any(|p| p == "staged.rs")));

        model.apply_git_update(vec![], vec!["staged.rs".into()]);

        assert!(!grouped_changed_files(&model)
            .iter()
            .any(|(_, paths)| paths.iter().any(|p| p == "staged.rs")));
    }
}
