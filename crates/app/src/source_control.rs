//! Source-control panel: the STAGED/CHANGES review surface docked into the
//! right dock (`docs/spec-source-control-write.md`, issue #546).
//!
//! The panel is driven by the same [`WorktreeModel`] the file tree already
//! mirrors — the daemon's `UpdateGitStatus`/`RepoState` stream is folded there
//! by `WorkspaceView`, and this panel only reads it (no re-derivation, no new
//! read protocol). It holds the tree's `Entity` and observes it for repaint;
//! it never mutates the model.
//!
//! Two sections are derived from the EXISTING `GitStatusEntry.index`/`.worktree`
//! split (a path with an index-side change is STAGED; a path with a
//! worktree-side change is a CHANGE; a staged-then-edited path shows in both):
//!
//! - **STAGED CHANGES** — per-row unstage, section-level unstage-all.
//! - **CHANGES** — per-row stage + discard, section-level stage-all.
//!
//! A commit textarea (mono, multi-line) plus a primary Commit button with a
//! live `N staged` suffix tops the panel. Every write is an explicit user
//! action sent over `git_op_tx` as a [`ClientMessage`] — `StageFile`,
//! `UnstageFile`, `DiscardFile`, `Commit`. The daemon replies `ok`/`error` and
//! the resulting state converges through the existing push recompute, so the
//! panel repaints via its tree observer without a manual refresh. Discard is
//! destructive: it is gated behind the #420 confirm dialog and never batched.
//!
//! Agent-agnostic: nothing here inspects agent output; the panel reacts only to
//! git facts the daemon computed with gix.

use flume::Sender;
use gpui::{
    div, px, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window,
};
use gpui_component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_component::dialog::{AlertDialog, DialogButtonProps};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _,
};
use rift_protocol::{ClientMessage, GitStatusCode};

use crate::file_tree::FileTree;
use crate::worktree::WorktreeModel;

/// Stable, distinct dock-panel identity for the source-control panel
/// (`Panel::panel_name`). Once shipped this must not change — it is the
/// persisted panel identifier.
pub const SOURCE_CONTROL_PANEL_NAME: &str = "source-control";

/// Fixed row height for every list row (file row and section header alike).
const ROW_HEIGHT: Pixels = px(22.0);

/// Width of the single-letter status lane in front of each file row.
const LETTER_LANE_WIDTH: Pixels = px(14.0);

/// The open-diff signal the panel emits when the user selects a changed file —
/// the clean interface the diff view (#338) subscribes to. `path` is
/// root-relative, the same key space as [`WorktreeModel`] entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceControlEvent {
    /// A changed file was selected; open its diff.
    OpenDiff { path: String },
}

/// Which of the two panel sections a row belongs to — fixes the per-row and
/// section-level action set (unstage in STAGED; stage + discard in CHANGES).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Staged,
    Changes,
}

impl Section {
    /// Stable slug used to key row/button element ids and hover groups so the
    /// same path in both sections never collides.
    fn tag(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Changes => "changes",
        }
    }
}

/// One file row in a section: the path plus the status code for the section's
/// own side (the index code in STAGED, the worktree code in CHANGES).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScmEntry {
    pub path: String,
    pub code: GitStatusCode,
}

/// The panel's two-section split, derived from the model's git statuses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScmSections {
    /// Paths with an index-side change — the STAGED CHANGES section.
    pub staged: Vec<ScmEntry>,
    /// Paths with a worktree-side change — the CHANGES section.
    pub changes: Vec<ScmEntry>,
}

impl ScmSections {
    /// Whether both sections are empty (a clean tree).
    fn is_empty(&self) -> bool {
        self.staged.is_empty() && self.changes.is_empty()
    }
}

/// Split the model's git statuses into the STAGED and CHANGES sections from the
/// EXISTING index/worktree codes — the read-side split already carried by
/// [`rift_protocol::GitEntryStatus`] since the git-status phase. A path with a
/// non-`Unmodified` index side lands in STAGED (its index code); a path with a
/// non-`Unmodified` worktree side lands in CHANGES (its worktree code); a
/// staged-then-edited path appears in both, mirroring `git status`'s `XY`
/// short form. Iteration follows the source `BTreeMap`'s path order, so both
/// sections come out path-sorted. Pure derivation — headless-testable, no GPUI
/// — so the model stays the single source of truth.
pub fn scm_sections(model: &WorktreeModel) -> ScmSections {
    let mut sections = ScmSections::default();
    for (path, status) in model.git_statuses() {
        if status.index != GitStatusCode::Unmodified {
            sections.staged.push(ScmEntry {
                path: path.clone(),
                code: status.index,
            });
        }
        if status.worktree != GitStatusCode::Unmodified {
            sections.changes.push(ScmEntry {
                path: path.clone(),
                code: status.worktree,
            });
        }
    }
    sections
}

/// The single-letter badge for a status code, following git's short-status
/// letters (`?` for untracked so it never collides with `U` = unmerged).
fn status_letter(code: GitStatusCode) -> &'static str {
    match code {
        GitStatusCode::Modified => "M",
        GitStatusCode::TypeChange => "T",
        GitStatusCode::Added => "A",
        GitStatusCode::Deleted => "D",
        GitStatusCode::Renamed => "R",
        GitStatusCode::Copied => "C",
        GitStatusCode::Unmerged => "U",
        GitStatusCode::Untracked => "?",
        GitStatusCode::Unmodified => "",
    }
}

/// Theme-token color for a status letter: additions/untracked read as
/// success, deletions/conflicts as danger, edits as warning (mirroring the
/// file tree's git decoration palette). Theme tokens only, never a hardcoded
/// hex.
fn status_color(code: GitStatusCode, cx: &App) -> Hsla {
    match code {
        GitStatusCode::Added | GitStatusCode::Copied | GitStatusCode::Untracked => {
            cx.theme().success
        }
        GitStatusCode::Deleted | GitStatusCode::Unmerged => cx.theme().danger,
        GitStatusCode::Modified | GitStatusCode::TypeChange | GitStatusCode::Renamed => {
            cx.theme().warning
        }
        GitStatusCode::Unmodified => cx.theme().muted_foreground,
    }
}

/// Split a root-relative path into `(file_name, parent_dir)` for the two-tone
/// path column — the basename in the foreground, the directory muted after it.
fn split_name_dir(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (name, dir),
        None => (path, ""),
    }
}

/// The source-control panel: the STAGED/CHANGES review surface with a commit
/// box, per-row and section-level write actions, and a live view of the daemon
/// git status.
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
    /// Write-op sender: each user action becomes one [`ClientMessage`]
    /// (`StageFile`/`UnstageFile`/`DiscardFile`/`Commit`) the tokio side
    /// forwards onto the protocol verbatim. The daemon's `ok`/`error` reply is
    /// not echoed into panel state — the resulting git change arrives through
    /// the push recompute, the protocol's one source of truth for git state.
    git_op_tx: Sender<ClientMessage>,
    /// The commit-message textarea (mono, multi-line). Observed for `Change` so
    /// the Commit button's enabled state tracks the message live.
    commit_input: Entity<InputState>,
    /// The currently selected changed file's path, or `None`. Cleared by the
    /// tree observer (see [`Self::new`]) the moment `path` leaves the changed
    /// set — a commit or discard must not leave a stale selection highlighting
    /// a row that no longer exists (#489).
    selected: Option<String>,
    focus_handle: FocusHandle,
    _observe_tree: Subscription,
    _observe_input: Subscription,
}

impl SourceControlPanel {
    /// Create the panel around the workspace's existing file-tree entity (the
    /// shared [`WorktreeModel`] this panel reads, never its own copy) and the
    /// write-op sender the workspace bridges onto the daemon protocol.
    pub fn new(
        file_tree: Entity<FileTree>,
        git_op_tx: Sender<ClientMessage>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let commit_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(3)
                .placeholder("Commit message")
        });
        // Repaint on every keystroke so the Commit button's disabled state
        // (empty message => disabled) tracks the textarea live.
        let observe_input = cx.subscribe(&commit_input, |_this, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let observe_tree = cx.observe(&file_tree, |this, tree, cx| {
            // A status fold (commit, discard, stage/unstage) may have dropped
            // the selected path from the changed set entirely; a stale
            // selection would otherwise linger with nothing left to show for
            // it (#489). Any path still present is left alone here — section
            // membership changes don't affect selection.
            if let Some(selected) = &this.selected {
                if !tree.read(cx).model().git_statuses().contains_key(selected) {
                    this.selected = None;
                }
            }
            cx.notify();
        });
        Self {
            file_tree,
            git_op_tx,
            commit_input,
            selected: None,
            focus_handle: cx.focus_handle(),
            _observe_tree: observe_tree,
            _observe_input: observe_input,
        }
    }

    /// The currently selected path, if any — the headless handle for the
    /// selection state.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Enqueue one write op for the tokio bridge. A full/closed channel only
    /// means the daemon session is down; the op is dropped and the next
    /// recompute reflects reality, so a debug log is enough.
    fn send_op(&self, op: ClientMessage) {
        if let Err(e) = self.git_op_tx.try_send(op) {
            tracing::debug!(error = %e, "failed to enqueue git op");
        }
    }

    /// Stage every worktree-changed path (the CHANGES section), recomputed
    /// fresh at click time so the set is never stale.
    fn stage_all(&self, cx: &Context<Self>) {
        for entry in scm_sections(self.file_tree.read(cx).model()).changes {
            self.send_op(ClientMessage::StageFile { path: entry.path });
        }
    }

    /// Unstage every staged path (the STAGED section), recomputed fresh at
    /// click time.
    fn unstage_all(&self, cx: &Context<Self>) {
        for entry in scm_sections(self.file_tree.read(cx).model()).staged {
            self.send_op(ClientMessage::UnstageFile { path: entry.path });
        }
    }

    /// Commit the staged set with the textarea's message, then clear the
    /// textarea. No-op when the message is blank or nothing is staged — the two
    /// states the daemon would reject, prevented client-side so the button
    /// press only ever fires a commit the daemon will accept.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.commit_input.read(cx).value().trim().to_owned();
        if message.is_empty() {
            return;
        }
        if scm_sections(self.file_tree.read(cx).model())
            .staged
            .is_empty()
        {
            return;
        }
        self.send_op(ClientMessage::Commit { message });
        self.commit_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        cx.notify();
    }

    /// Open the destructive-discard confirm dialog (#420 pattern, mirroring
    /// [`crate::editor::EditorView`]'s dirty-close dialog). Confirming sends
    /// one [`ClientMessage::DiscardFile`]; cancelling leaves the file
    /// untouched. Never batched — one dialog, one file.
    fn confirm_discard(&self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let git_op_tx = self.git_op_tx.clone();
        window.open_alert_dialog(cx, move |alert: AlertDialog, _, _| {
            let git_op_tx = git_op_tx.clone();
            let path = path.clone();
            alert
                .title("Discard Changes")
                .description(SharedString::from(format!(
                    "Discard all changes to \"{name}\"? This cannot be undone."
                )))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Discard")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true)
                        .on_ok(move |_, _window, _cx| {
                            if let Err(e) = git_op_tx
                                .try_send(ClientMessage::DiscardFile { path: path.clone() })
                            {
                                tracing::debug!(error = %e, "failed to enqueue discard");
                            }
                            true
                        }),
                )
        });
    }

    /// The commit box: mono multi-line textarea, a primary Commit button with a
    /// leading check icon (never a glyph in a string), and a live `N staged`
    /// suffix. The button is disabled unless a non-empty message meets a
    /// non-empty staged set.
    fn render_commit_box(&self, staged_count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let can_commit = staged_count > 0 && !self.commit_input.read(cx).value().trim().is_empty();
        v_flex()
            .flex_none()
            .p(px(8.0))
            .gap(px(6.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(Input::new(&self.commit_input).font_family(cx.theme().mono_font_family.clone()))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        Button::new("scm-commit")
                            .primary()
                            .small()
                            .icon(IconName::Check)
                            .label("Commit")
                            .disabled(!can_commit)
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.commit(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{staged_count} staged")),
                    ),
            )
    }

    /// One section header: an uppercased muted title, a count pill, and the
    /// section-level stage-all / unstage-all icon button.
    fn render_section_header(
        &self,
        section: Section,
        count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (title, icon, tooltip, action_id) = match section {
            Section::Staged => (
                "Staged Changes",
                IconName::Minus,
                "Unstage all",
                "scm-unstage-all",
            ),
            Section::Changes => ("Changes", IconName::Plus, "Stage all", "scm-stage-all"),
        };
        let action = Button::new(action_id)
            .ghost()
            .xsmall()
            .icon(icon)
            .tooltip(tooltip)
            .on_click(cx.listener(move |this, _event, _window, cx| match section {
                Section::Staged => this.unstage_all(cx),
                Section::Changes => this.stage_all(cx),
            }));
        h_flex()
            .items_center()
            .justify_between()
            .h(ROW_HEIGHT)
            .px(px(8.0))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(title.to_uppercase()),
                    )
                    .child(
                        div()
                            .px(px(6.0))
                            .rounded_full()
                            .bg(cx.theme().muted)
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(count.to_string()),
                    ),
            )
            .child(action)
    }

    /// The per-row hover actions: unstage in STAGED; stage + discard in
    /// CHANGES. Returned as a bare container so the caller can attach the
    /// row-scoped hover reveal.
    fn render_row_actions(
        &self,
        section: Section,
        path: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut actions = h_flex().flex_none().items_center().gap(px(2.0));
        match section {
            Section::Staged => {
                let unstage_path = path.to_owned();
                actions = actions.child(
                    Button::new(SharedString::from(format!("scm-unstage-{path}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Minus)
                        .tooltip("Unstage")
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            cx.stop_propagation();
                            this.send_op(ClientMessage::UnstageFile {
                                path: unstage_path.clone(),
                            });
                        })),
                );
            }
            Section::Changes => {
                let stage_path = path.to_owned();
                let discard_path = path.to_owned();
                actions = actions
                    .child(
                        Button::new(SharedString::from(format!("scm-stage-{path}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip("Stage")
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                cx.stop_propagation();
                                this.send_op(ClientMessage::StageFile {
                                    path: stage_path.clone(),
                                });
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("scm-discard-{path}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Undo)
                            .tooltip("Discard")
                            .on_click(cx.listener(move |this, _event, window, cx| {
                                cx.stop_propagation();
                                this.confirm_discard(discard_path.clone(), window, cx);
                            })),
                    );
            }
        }
        actions
    }

    /// One changed-file row: the status letter lane, the two-tone path column
    /// (clicking it selects the row and opens its diff), and the hover-revealed
    /// section actions. Selecting only emits [`SourceControlEvent::OpenDiff`] —
    /// no git write, no tmux state.
    fn render_row(
        &self,
        section: Section,
        entry: ScmEntry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ScmEntry { path, code } = entry;
        let is_selected = self.selected.as_deref() == Some(path.as_str());
        let (name, dir) = split_name_dir(&path);
        let name = name.to_owned();
        let dir = dir.to_owned();
        let tag = section.tag();
        let group = SharedString::from(format!("scm-row-{tag}-{path}"));

        let letter_lane = div()
            .flex_none()
            .w(LETTER_LANE_WIDTH)
            .text_center()
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .text_color(status_color(code, cx))
            .child(status_letter(code));

        let click_path = path.clone();
        let mut name_column = h_flex()
            .id(SharedString::from(format!("scm-name-{tag}-{path}")))
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .child(letter_lane)
            .child(div().flex_none().child(name));
        if !dir.is_empty() {
            name_column = name_column.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(dir),
            );
        }
        let name_column = name_column.on_click(cx.listener(move |this, _event, _window, cx| {
            this.selected = Some(click_path.clone());
            cx.emit(SourceControlEvent::OpenDiff {
                path: click_path.clone(),
            });
            cx.notify();
        }));

        let actions = self
            .render_row_actions(section, &path, cx)
            .opacity(0.0)
            .group_hover(group.clone(), |style| style.opacity(1.0));

        let mut row = h_flex()
            .group(group)
            .h(ROW_HEIGHT)
            .px(px(8.0))
            .items_center()
            .gap(px(4.0))
            .text_sm()
            .hover(|style| style.bg(cx.theme().list_hover))
            .child(name_column)
            .child(actions);
        if is_selected {
            row = row
                .bg(cx.theme().list_active)
                .text_color(cx.theme().foreground);
        }
        row
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
        let sections = scm_sections(self.file_tree.read(cx).model());
        let staged_count = sections.staged.len();

        let content = if sections.is_empty() {
            div()
                .flex_1()
                .p(px(8.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No changes")
                .into_any_element()
        } else {
            let mut list = v_flex().w_full();
            if !sections.staged.is_empty() {
                list = list.child(self.render_section_header(
                    Section::Staged,
                    sections.staged.len(),
                    cx,
                ));
                for entry in sections.staged {
                    list = list.child(self.render_row(Section::Staged, entry, cx));
                }
            }
            if !sections.changes.is_empty() {
                list = list.child(self.render_section_header(
                    Section::Changes,
                    sections.changes.len(),
                    cx,
                ));
                for entry in sections.changes {
                    list = list.child(self.render_row(Section::Changes, entry, cx));
                }
            }
            // A change set taller than the panel scrolls (#436): the themed
            // scrollbar wrapper owns the scroll state, keyed on this call site.
            div()
                .flex_1()
                .min_h_0()
                .child(list.overflow_y_scrollbar())
                .into_any_element()
        };

        v_flex()
            .size_full()
            .child(self.render_commit_box(staged_count, cx))
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext, WindowHandle};
    use gpui_component::Root;
    use rift_protocol::{AheadBehind, GitStatusEntry, WorktreeEntry};

    fn status(index: GitStatusCode, worktree: GitStatusCode) -> rift_protocol::GitEntryStatus {
        rift_protocol::GitEntryStatus { index, worktree }
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

    // --- scm_sections derivation -------------------------------------------

    fn seed_model() -> WorktreeModel {
        let mut model = WorktreeModel::default();
        model.apply_snapshot_chunk(
            "/proj".into(),
            vec![
                file("staged.rs"),
                file("both.rs"),
                file("dirty.rs"),
                file("loose.rs"),
            ],
            true,
        );
        model.apply_git_update(
            vec![
                // Staged only: index changed, worktree clean.
                git_entry("staged.rs", GitStatusCode::Added, GitStatusCode::Unmodified),
                // Staged then further edited: shows in BOTH sections.
                git_entry("both.rs", GitStatusCode::Added, GitStatusCode::Modified),
                // Unstaged edit: worktree only.
                git_entry(
                    "dirty.rs",
                    GitStatusCode::Unmodified,
                    GitStatusCode::Modified,
                ),
                // Untracked: worktree only.
                git_entry(
                    "loose.rs",
                    GitStatusCode::Unmodified,
                    GitStatusCode::Untracked,
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
            0,
            0,
        );
        model
    }

    #[test]
    fn test_scm_sections_splits_by_index_and_worktree_side() {
        let sections = scm_sections(&seed_model());

        assert_eq!(
            sections.staged,
            vec![
                ScmEntry {
                    path: "both.rs".to_owned(),
                    code: GitStatusCode::Added,
                },
                ScmEntry {
                    path: "staged.rs".to_owned(),
                    code: GitStatusCode::Added,
                },
            ],
            "STAGED lists exactly the index-side changes, path-sorted, with the index code"
        );
        assert_eq!(
            sections.changes,
            vec![
                ScmEntry {
                    path: "both.rs".to_owned(),
                    code: GitStatusCode::Modified,
                },
                ScmEntry {
                    path: "dirty.rs".to_owned(),
                    code: GitStatusCode::Modified,
                },
                ScmEntry {
                    path: "loose.rs".to_owned(),
                    code: GitStatusCode::Untracked,
                },
            ],
            "CHANGES lists exactly the worktree-side changes with the worktree code"
        );
    }

    #[test]
    fn test_scm_sections_staged_then_edited_appears_in_both() {
        let sections = scm_sections(&seed_model());
        assert!(sections.staged.iter().any(|e| e.path == "both.rs"));
        assert!(sections.changes.iter().any(|e| e.path == "both.rs"));
    }

    #[test]
    fn test_scm_sections_empty_model_yields_no_sections() {
        let sections = scm_sections(&WorktreeModel::default());
        assert!(sections.is_empty());
    }

    #[test]
    fn test_scm_sections_committing_drops_the_file_from_both_sections() {
        // Mirrors the acceptance criterion: a status tick (`cleared`) drops a
        // committed file from both sections on the next re-derivation.
        let mut model = seed_model();
        assert!(scm_sections(&model)
            .staged
            .iter()
            .any(|e| e.path == "staged.rs"));

        model.apply_git_update(vec![], vec!["staged.rs".into()]);

        let sections = scm_sections(&model);
        assert!(!sections.staged.iter().any(|e| e.path == "staged.rs"));
        assert!(!sections.changes.iter().any(|e| e.path == "staged.rs"));
    }

    // --- status letter / color ---------------------------------------------

    #[test]
    fn test_status_letter_uses_git_short_codes() {
        assert_eq!(status_letter(GitStatusCode::Modified), "M");
        assert_eq!(status_letter(GitStatusCode::Added), "A");
        assert_eq!(status_letter(GitStatusCode::Deleted), "D");
        assert_eq!(status_letter(GitStatusCode::Renamed), "R");
        assert_eq!(status_letter(GitStatusCode::Untracked), "?");
        assert_eq!(status_letter(GitStatusCode::Unmerged), "U");
    }

    #[test]
    fn test_split_name_dir_separates_basename_from_parent() {
        assert_eq!(split_name_dir("src/app/main.rs"), ("main.rs", "src/app"));
        assert_eq!(split_name_dir("top.rs"), ("top.rs", ""));
    }

    // --- panel construction + write ops ------------------------------------

    fn open_panel(
        cx: &mut TestAppContext,
    ) -> (
        Entity<FileTree>,
        Entity<SourceControlPanel>,
        flume::Receiver<ClientMessage>,
        WindowHandle<Root>,
    ) {
        let (tx, rx) = flume::unbounded();
        let mut file_tree = None;
        let mut panel = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                let ft = cx.new(|_cx| FileTree::new());
                let scm = cx.new(|cx| SourceControlPanel::new(ft.clone(), tx.clone(), window, cx));
                file_tree = Some(ft);
                panel = Some(scm.clone());
                cx.new(|cx| Root::new(scm, window, cx))
            })
            .expect("open window")
        });
        (
            file_tree.expect("file tree constructed in window"),
            panel.expect("panel constructed in window"),
            rx,
            window,
        )
    }

    fn seed_tree(file_tree: &Entity<FileTree>, cx: &mut TestAppContext) {
        cx.update(|cx| {
            file_tree.update(cx, |tree, _cx| {
                tree.model_mut().apply_snapshot_chunk(
                    "/proj".into(),
                    vec![file("staged.rs"), file("dirty.rs"), file("loose.rs")],
                    true,
                );
                tree.model_mut().apply_git_update(
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
                    ],
                    vec![],
                );
            });
        });
    }

    #[gpui::test]
    fn test_stage_all_sends_stage_for_each_changed_file(cx: &mut TestAppContext) {
        let (file_tree, panel, rx, _window) = open_panel(cx);
        seed_tree(&file_tree, cx);

        cx.update(|cx| {
            panel.update(cx, |panel, cx| panel.stage_all(cx));
        });

        let mut sent: Vec<String> = rx
            .drain()
            .map(|msg| match msg {
                ClientMessage::StageFile { path } => path,
                other => panic!("expected StageFile, got {other:?}"),
            })
            .collect();
        sent.sort();
        assert_eq!(sent, vec!["dirty.rs".to_owned(), "loose.rs".to_owned()]);
    }

    #[gpui::test]
    fn test_unstage_all_sends_unstage_for_each_staged_file(cx: &mut TestAppContext) {
        let (file_tree, panel, rx, _window) = open_panel(cx);
        seed_tree(&file_tree, cx);

        cx.update(|cx| {
            panel.update(cx, |panel, cx| panel.unstage_all(cx));
        });

        let sent: Vec<ClientMessage> = rx.drain().collect();
        assert_eq!(
            sent,
            vec![ClientMessage::UnstageFile {
                path: "staged.rs".to_owned()
            }]
        );
    }

    #[gpui::test]
    fn test_commit_sends_commit_and_clears_message_when_staged(cx: &mut TestAppContext) {
        let (file_tree, panel, rx, window) = open_panel(cx);
        seed_tree(&file_tree, cx);

        window
            .update(cx, |_root, window, cx| {
                panel.update(cx, |panel, cx| {
                    panel.commit_input.update(cx, |input, cx| {
                        input.set_value("feat: land it", window, cx);
                    });
                    panel.commit(window, cx);
                });
            })
            .expect("commit update");

        let sent: Vec<ClientMessage> = rx.drain().collect();
        assert_eq!(
            sent,
            vec![ClientMessage::Commit {
                message: "feat: land it".to_owned()
            }]
        );
        cx.update(|cx| {
            assert_eq!(
                panel.read(cx).commit_input.read(cx).value().as_ref(),
                "",
                "the textarea is cleared after a commit is sent"
            );
        });
    }

    #[gpui::test]
    fn test_commit_with_empty_message_sends_nothing(cx: &mut TestAppContext) {
        let (file_tree, panel, rx, window) = open_panel(cx);
        seed_tree(&file_tree, cx);

        window
            .update(cx, |_root, window, cx| {
                panel.update(cx, |panel, cx| panel.commit(window, cx));
            })
            .expect("commit update");

        assert!(
            rx.drain().next().is_none(),
            "a blank message must not send a Commit the daemon would reject"
        );
    }

    #[gpui::test]
    fn test_commit_with_nothing_staged_sends_nothing(cx: &mut TestAppContext) {
        let (file_tree, panel, rx, window) = open_panel(cx);
        // Only an unstaged edit — nothing in the index tree.
        cx.update(|cx| {
            file_tree.update(cx, |tree, _cx| {
                tree.model_mut()
                    .apply_snapshot_chunk("/proj".into(), vec![file("dirty.rs")], true);
                tree.model_mut().apply_git_update(
                    vec![git_entry(
                        "dirty.rs",
                        GitStatusCode::Unmodified,
                        GitStatusCode::Modified,
                    )],
                    vec![],
                );
            });
        });

        window
            .update(cx, |_root, window, cx| {
                panel.update(cx, |panel, cx| {
                    panel.commit_input.update(cx, |input, cx| {
                        input.set_value("feat: nothing staged", window, cx);
                    });
                    panel.commit(window, cx);
                });
            })
            .expect("commit update");

        assert!(
            rx.drain().next().is_none(),
            "a nothing-staged index must not send a Commit the daemon would reject"
        );
    }

    // --- selection lifecycle (#489) ----------------------------------------

    #[gpui::test]
    fn test_selected_path_leaving_the_changed_set_clears_the_selection(cx: &mut TestAppContext) {
        let (file_tree, panel, _rx, _window) = open_panel(cx);
        cx.update(|cx| {
            file_tree.update(cx, |tree, _cx| {
                tree.model_mut()
                    .apply_snapshot_chunk("/proj".into(), vec![file("dirty.rs")], true);
                tree.model_mut().apply_git_update(
                    vec![git_entry(
                        "dirty.rs",
                        GitStatusCode::Unmodified,
                        GitStatusCode::Modified,
                    )],
                    vec![],
                );
            });
        });

        cx.update(|cx| {
            panel.update(cx, |panel, _cx| {
                panel.selected = Some("dirty.rs".to_owned())
            });
        });
        cx.update(|cx| {
            assert_eq!(panel.read(cx).selected(), Some("dirty.rs"));
        });

        // The commit lands: dirty.rs leaves the changed set entirely.
        cx.update(|cx| {
            file_tree.update(cx, |tree, cx| {
                tree.model_mut()
                    .apply_git_update(vec![], vec!["dirty.rs".into()]);
                cx.notify();
            });
        });

        // `cx.observe`'s callback runs on effect flush at the end of the
        // *outermost* `cx.update` call, not mid-closure — so the clear is only
        // observable from a separate top-level call after the one that
        // triggered it (mirrors `ProblemsPanel`'s equivalent live-update test).
        cx.update(|cx| {
            assert_eq!(
                panel.read(cx).selected(),
                None,
                "the path leaving the changed set clears the stale selection"
            );
        });
    }

    #[gpui::test]
    fn test_selected_path_still_changed_keeps_the_selection(cx: &mut TestAppContext) {
        let (file_tree, panel, _rx, _window) = open_panel(cx);
        cx.update(|cx| {
            file_tree.update(cx, |tree, _cx| {
                tree.model_mut().apply_snapshot_chunk(
                    "/proj".into(),
                    vec![file("dirty.rs"), file("other.rs")],
                    true,
                );
                tree.model_mut().apply_git_update(
                    vec![
                        git_entry(
                            "dirty.rs",
                            GitStatusCode::Unmodified,
                            GitStatusCode::Modified,
                        ),
                        git_entry(
                            "other.rs",
                            GitStatusCode::Unmodified,
                            GitStatusCode::Untracked,
                        ),
                    ],
                    vec![],
                );
            });
        });

        cx.update(|cx| {
            panel.update(cx, |panel, _cx| {
                panel.selected = Some("dirty.rs".to_owned())
            });
        });

        // A status tick that clears a *different* path leaves the selection
        // untouched.
        cx.update(|cx| {
            file_tree.update(cx, |tree, cx| {
                tree.model_mut()
                    .apply_git_update(vec![], vec!["other.rs".into()]);
                cx.notify();
            });
        });

        cx.update(|cx| {
            assert_eq!(
                panel.read(cx).selected(),
                Some("dirty.rs"),
                "a tick that doesn't touch the selected path leaves it selected"
            );
        });
    }
}
