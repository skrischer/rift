//! Virtualized unified diff view (`docs/spec-source-control.md`, issue #338).
//!
//! Renders the [`rift_protocol::DaemonMessage::FileDiff`] streamed for the path
//! the source-control panel's [`crate::source_control::SourceControlEvent::OpenDiff`]
//! selects: a single-column unified diff with add/remove/context styling from
//! theme tokens, or a placeholder for the binary/too-large sentinels
//! ([`rift_protocol::FileDiffPayload`]). Mirrors the editor's `OpenFile ->
//! FileContent` request/reply pattern (`crate::workspace`) — `open_diff` sends
//! a path on the `request_diff_tx` channel the workspace wires to
//! `ClientMessage::RequestDiff`, and `apply_file_diff` renders the daemon's
//! reply. Path-keyed, like the buffer channel: at most one diff is ever
//! inflight, so a reply is only applied while it still matches the currently
//! open path (a stale reply for an already-abandoned selection is dropped).
//!
//! [`flatten_hunks`] and [`DiffViewState::from_payload`] are pure, GPUI-free
//! functions — the hunk-to-rows flattening (line-number bookkeping per
//! [`DiffLineKind`]) and the sentinel mapping are unit-tested headless,
//! mirroring [`crate::problems_panel::ProblemsSummary::from_diagnostics`].
//! Rows are flattened once per reply (not re-derived per virtual-list frame,
//! unlike [`crate::problems_panel::ProblemsPanel`]'s smaller diagnostics set) —
//! the spec's size ceiling (~20k changed lines) makes a per-frame re-flatten
//! wasteful for the view this panel renders.
//!
//! Read-only, agent-agnostic: this view only requests and displays a computed
//! diff; it performs no git write operations and inspects no agent output.

use std::rc::Rc;

use flume::Sender;
use gpui::{
    div, px, AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement,
    ParentElement as _, Pixels, Render, SharedString, Size, Styled as _, Window,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{v_virtual_list, ActiveTheme as _, VirtualListScrollHandle};
use rift_protocol::{DiffHunk, DiffLineKind, FileDiffPayload};
use tracing::debug;

/// Stable, distinct dock-panel identity for the diff view (`Panel::panel_name`).
/// Once shipped this must not change — it is the persisted panel identifier.
pub const DIFF_VIEW_PANEL_NAME: &str = "diff-view";

/// Fixed row height for every diff row (hunk header and line alike) — a
/// uniform height keeps the virtual list's size vector trivial, mirroring
/// `ProblemsPanel::ROW_HEIGHT`.
const ROW_HEIGHT: Pixels = px(20.0);

/// Height of the fixed path header above the scrollable diff.
const HEADER_HEIGHT: Pixels = px(28.0);

/// Width of each line-number column.
const LINE_NUMBER_WIDTH: Pixels = px(44.0);

/// One flattened row of the virtualized diff list: either a hunk's `@@ ... @@`
/// header or one of its lines, addressed against both the old (HEAD) and new
/// (worktree) line numbering. Derived once from the streamed hunks by
/// [`flatten_hunks`] and held in [`DiffViewState::Hunks`] — never re-derived
/// per render frame.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffRow {
    HunkHeader {
        old_start: u32,
        old_len: u32,
        new_start: u32,
        new_len: u32,
    },
    Line {
        kind: DiffLineKind,
        /// The line's 1-based position on the old (HEAD) side; `None` for an
        /// added line, which has no old-side position.
        old_line: Option<u32>,
        /// The line's 1-based position on the new (worktree) side; `None` for
        /// a removed line, which has no new-side position.
        new_line: Option<u32>,
        content: String,
    },
}

/// Flatten a [`FileDiffPayload::Hunks`]' hunks into the virtual list's row
/// sequence, computing each line's old/new line number by walking the hunk's
/// `old_start`/`new_start` forward: context and remove lines advance the old
/// counter, context and add lines advance the new counter — mirroring
/// unified-diff's own line-counting rule. Pure and GPUI-free so it is
/// unit-testable headless.
fn flatten_hunks(hunks: Vec<DiffHunk>) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for hunk in hunks {
        rows.push(DiffRow::HunkHeader {
            old_start: hunk.old_start,
            old_len: hunk.old_len,
            new_start: hunk.new_start,
            new_len: hunk.new_len,
        });

        let mut old_line = hunk.old_start;
        let mut new_line = hunk.new_start;
        for line in hunk.lines {
            let (old_num, new_num) = match line.kind {
                DiffLineKind::Context => {
                    let nums = (Some(old_line), Some(new_line));
                    old_line += 1;
                    new_line += 1;
                    nums
                }
                DiffLineKind::Remove => {
                    let nums = (Some(old_line), None);
                    old_line += 1;
                    nums
                }
                DiffLineKind::Add => {
                    let nums = (None, Some(new_line));
                    new_line += 1;
                    nums
                }
            };
            rows.push(DiffRow::Line {
                kind: line.kind,
                old_line: old_num,
                new_line: new_num,
                content: line.content,
            });
        }
    }
    rows
}

/// The diff view's current display state for the open path. `Empty` (no file
/// selected yet) and `Loading` (request sent, reply not yet in) render a
/// placeholder identically to the sentinels — never a partial/garbled render.
#[derive(Debug, Clone, PartialEq)]
enum DiffViewState {
    Empty,
    Loading,
    Hunks(Vec<DiffRow>),
    Binary,
    TooLarge,
}

impl DiffViewState {
    /// Map a daemon [`FileDiffPayload`] reply onto the view's display state.
    /// Pure and GPUI-free so the binary/too-large sentinel handling is
    /// unit-testable headless, alongside [`flatten_hunks`].
    fn from_payload(payload: FileDiffPayload) -> Self {
        match payload {
            FileDiffPayload::Hunks { hunks } => Self::Hunks(flatten_hunks(hunks)),
            FileDiffPayload::Binary => Self::Binary,
            FileDiffPayload::TooLarge => Self::TooLarge,
        }
    }
}

/// The action to take when a git-status tick's `changed`/`cleared` paths
/// (`docs/spec-source-control.md`'s refresh semantics, issue #339) are checked
/// against the diff view's currently open path: `Refresh` re-requests the
/// diff (the open file is still in the changed set, so its on-disk content
/// may have moved since the last reply); `Close` empties the view (the open
/// file left the changed set entirely — e.g. a commit landed); `None` when
/// neither list mentions the open path. Pure and GPUI-free so the decision is
/// unit-testable headless, mirroring [`flatten_hunks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitUpdateAction {
    None,
    Refresh,
    Close,
}

/// Decide [`GitUpdateAction`] for `open_path` given one `UpdateGitStatus`
/// tick's `changed`/`cleared` path lists. `Close` takes priority over
/// `Refresh` if a path were ever listed in both (the daemon's
/// `git_delta_messages` never does this — `changed` and `cleared` are
/// disjoint by construction — but `Close` is the safer default if it did).
fn git_update_action(open_path: &str, changed: &[String], cleared: &[String]) -> GitUpdateAction {
    if cleared.iter().any(|path| path == open_path) {
        GitUpdateAction::Close
    } else if changed.iter().any(|path| path == open_path) {
        GitUpdateAction::Refresh
    } else {
        GitUpdateAction::None
    }
}

/// The diff view: a virtualized, read-only unified diff of the currently
/// selected changed file, streamed from the daemon on request.
pub struct DiffView {
    /// The path currently open (selected in the source-control panel), if any.
    path: Option<String>,
    state: DiffViewState,
    /// Diff pull requests: a root-relative path the workspace forwards onto
    /// the protocol as `ClientMessage::RequestDiff` (mirrors
    /// `WorkspaceChannels::open_file_tx`).
    request_diff_tx: Sender<String>,
    focus_handle: FocusHandle,
    scroll_handle: VirtualListScrollHandle,
}

impl DiffView {
    pub fn new(request_diff_tx: Sender<String>, cx: &mut Context<Self>) -> Self {
        Self {
            path: None,
            state: DiffViewState::Empty,
            request_diff_tx,
            focus_handle: cx.focus_handle(),
            scroll_handle: VirtualListScrollHandle::new(),
        }
    }

    /// Open `path`'s diff: send the request onto the protocol and arm the
    /// loading state — but only when `path` differs from the currently open
    /// one. Re-requesting the already-open path (a git-status refresh tick,
    /// or the user reselecting the same row) keeps the rendered diff visible
    /// until the reply swaps it in (#487) — no "Loading diff..." flash for
    /// content that is most likely unchanged. Called by the workspace on
    /// [`crate::source_control::SourceControlEvent::OpenDiff`] and by
    /// [`Self::apply_git_update`]'s refresh path.
    pub fn open_diff(&mut self, path: String, cx: &mut Context<Self>) {
        if let Err(e) = self.request_diff_tx.try_send(path.clone()) {
            debug!(error = %e, %path, "failed to enqueue diff request");
        }
        if self.path.as_deref() != Some(path.as_str()) {
            self.path = Some(path);
            self.state = DiffViewState::Loading;
        }
        cx.notify();
    }

    /// Apply a `FileDiff` reply. A reply for a path that is no longer open
    /// (the user selected a different file before this one arrived) is
    /// dropped — mirrors `EditorView::load`'s stale-reply guard.
    pub fn apply_file_diff(&mut self, path: String, diff: FileDiffPayload, cx: &mut Context<Self>) {
        if self.path.as_deref() != Some(path.as_str()) {
            return;
        }
        self.state = DiffViewState::from_payload(diff);
        cx.notify();
    }

    /// React to an `UpdateGitStatus` tick for the currently open diff's path
    /// (`docs/spec-source-control.md`'s refresh semantics, #339): re-requests
    /// the diff when the open path is still in `changed` (its content may
    /// have moved since the last reply), closes the view when the open path
    /// left the changed set entirely (`cleared` — e.g. a commit landed), and
    /// is a no-op otherwise — including when no diff is open. Called by the
    /// workspace after it folds an `UpdateGitStatus` onto the file tree's
    /// model, alongside the existing re-request-on-reselection path
    /// (`open_diff`, driven by `SourceControlEvent::OpenDiff`).
    pub fn apply_git_update(
        &mut self,
        changed: &[String],
        cleared: &[String],
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.path.clone() else {
            return;
        };
        match git_update_action(&path, changed, cleared) {
            GitUpdateAction::Refresh => self.open_diff(path, cx),
            GitUpdateAction::Close => self.close(cx),
            GitUpdateAction::None => {}
        }
    }

    /// React to an `UpdateWorktree` tick for the currently open diff's path
    /// (#488): a content-only edit to an already-tracked file leaves its git
    /// status unchanged (`M` stays `M`), so `git_delta_messages` (daemon
    /// `crates/daemon/src/lib.rs`) never emits an `UpdateGitStatus` for it and
    /// `apply_git_update` alone never re-requests — an agent iterating on an
    /// open file's content leaves the diff stale. `UpdateWorktree` fires on
    /// every disk write regardless of git status (file-watch, not git-status,
    /// driven), so re-requesting here catches the content-only case that
    /// `apply_git_update` misses. `open_diff`'s same-path branch keeps the
    /// rendered diff visible until the reply swaps it in — no loading flash.
    /// A no-op when no diff is open or `changed` does not mention it.
    pub fn apply_content_update(&mut self, changed: &[String], cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if changed.iter().any(|changed_path| changed_path == &path) {
            self.open_diff(path, cx);
        }
    }

    /// Empty the view and forget the open path: the file it was showing left
    /// the changed set (e.g. a commit landed), so there is nothing left to
    /// review. No request is sent — mirrors `open_diff`'s state assignment in
    /// reverse, with no diff left to pull.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.path = None;
        self.state = DiffViewState::Empty;
        cx.notify();
    }

    fn placeholder(text: impl Into<SharedString>, cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .p(px(8.0))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text.into())
            .into_any_element()
    }

    /// Render one row: a hunk's `@@ ... @@` header, or one line with its
    /// old/new line numbers, a +/-/space marker, and add/remove/context
    /// styling from theme tokens (`success`/`danger`, mirroring the file
    /// tree's git-status decoration — no diff-specific tokens invented).
    fn render_row(row: &DiffRow, cx: &mut Context<Self>) -> AnyElement {
        match row {
            DiffRow::HunkHeader {
                old_start,
                old_len,
                new_start,
                new_len,
            } => div()
                .h(ROW_HEIGHT)
                .flex()
                .items_center()
                .px(px(8.0))
                .bg(cx.theme().muted)
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .font_family(cx.theme().mono_font_family.clone())
                .child(format!(
                    "@@ -{old_start},{old_len} +{new_start},{new_len} @@"
                ))
                .into_any_element(),
            DiffRow::Line {
                kind,
                old_line,
                new_line,
                content,
            } => {
                let (bg, marker): (Hsla, &str) = match kind {
                    DiffLineKind::Add => (cx.theme().success.opacity(0.14), "+"),
                    DiffLineKind::Remove => (cx.theme().danger.opacity(0.14), "-"),
                    DiffLineKind::Context => (cx.theme().background, " "),
                };
                let old_col = old_line.map(|n| n.to_string()).unwrap_or_default();
                let new_col = new_line.map(|n| n.to_string()).unwrap_or_default();

                div()
                    .h(ROW_HEIGHT)
                    .flex()
                    .items_center()
                    .bg(bg)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .child(
                        div()
                            .w(LINE_NUMBER_WIDTH)
                            .flex_shrink_0()
                            .px(px(4.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(old_col),
                    )
                    .child(
                        div()
                            .w(LINE_NUMBER_WIDTH)
                            .flex_shrink_0()
                            .px(px(4.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(new_col),
                    )
                    .child(
                        div()
                            .w(px(14.0))
                            .flex_shrink_0()
                            .text_color(cx.theme().foreground)
                            .child(marker),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_color(cx.theme().foreground)
                            .child(content.clone()),
                    )
                    .into_any_element()
            }
        }
    }
}

impl Focusable for DiffView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for DiffView {}

impl Panel for DiffView {
    fn panel_name(&self) -> &'static str {
        DIFF_VIEW_PANEL_NAME
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Diff")
    }
}

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(path) = self.path.clone() else {
            return Self::placeholder("Select a changed file to view its diff", cx);
        };

        let rows = match &self.state {
            DiffViewState::Empty => {
                return Self::placeholder("Select a changed file to view its diff", cx)
            }
            DiffViewState::Loading => return Self::placeholder("Loading diff...", cx),
            DiffViewState::Binary => return Self::placeholder("Binary file - diff not shown", cx),
            DiffViewState::TooLarge => return Self::placeholder("Diff too large to display", cx),
            DiffViewState::Hunks(rows) if rows.is_empty() => {
                return Self::placeholder("No changes", cx)
            }
            DiffViewState::Hunks(rows) => rows,
        };

        let item_sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
            rows.iter()
                .map(|_| Size::new(px(0.0), ROW_HEIGHT))
                .collect(),
        );

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(HEADER_HEIGHT)
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .px(px(8.0))
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(path),
            )
            .child(
                div().flex_1().min_h_0().child(
                    v_virtual_list(
                        cx.entity().clone(),
                        "diff-view-list",
                        item_sizes,
                        move |this, visible_range, _window, cx| {
                            let DiffViewState::Hunks(rows) = &this.state else {
                                return Vec::new();
                            };
                            visible_range
                                .filter_map(|ix| rows.get(ix).map(|row| Self::render_row(row, cx)))
                                .collect::<Vec<_>>()
                        },
                    )
                    .track_scroll(&self.scroll_handle),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};
    use rift_protocol::DiffLine;

    fn line(kind: DiffLineKind, content: &str) -> DiffLine {
        DiffLine {
            kind,
            content: content.to_owned(),
        }
    }

    // --- flatten_hunks ---

    #[test]
    fn test_flatten_single_hunk_numbers_context_add_remove_lines_correctly() {
        // 3 old lines (1..3), 3 new lines (1..3): line 2 removed and replaced
        // by a new "b2" line, mirroring a typical one-line edit.
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_len: 3,
            new_start: 1,
            new_len: 3,
            lines: vec![
                line(DiffLineKind::Context, "a1"),
                line(DiffLineKind::Remove, "a2"),
                line(DiffLineKind::Add, "b2"),
                line(DiffLineKind::Context, "a3"),
            ],
        }];

        let rows = flatten_hunks(hunks);
        assert_eq!(
            rows,
            vec![
                DiffRow::HunkHeader {
                    old_start: 1,
                    old_len: 3,
                    new_start: 1,
                    new_len: 3,
                },
                DiffRow::Line {
                    kind: DiffLineKind::Context,
                    old_line: Some(1),
                    new_line: Some(1),
                    content: "a1".into(),
                },
                DiffRow::Line {
                    kind: DiffLineKind::Remove,
                    old_line: Some(2),
                    new_line: None,
                    content: "a2".into(),
                },
                DiffRow::Line {
                    kind: DiffLineKind::Add,
                    old_line: None,
                    new_line: Some(2),
                    content: "b2".into(),
                },
                DiffRow::Line {
                    kind: DiffLineKind::Context,
                    old_line: Some(3),
                    new_line: Some(3),
                    content: "a3".into(),
                },
            ]
        );
    }

    #[test]
    fn test_flatten_multiple_hunks_each_restart_from_their_own_start() {
        let hunks = vec![
            DiffHunk {
                old_start: 1,
                old_len: 1,
                new_start: 1,
                new_len: 1,
                lines: vec![line(DiffLineKind::Context, "a1")],
            },
            DiffHunk {
                old_start: 50,
                old_len: 1,
                new_start: 50,
                new_len: 1,
                lines: vec![line(DiffLineKind::Context, "a50")],
            },
        ];

        let rows = flatten_hunks(hunks);
        assert_eq!(rows.len(), 4, "2 headers + 2 lines");
        assert_eq!(
            rows[1],
            DiffRow::Line {
                kind: DiffLineKind::Context,
                old_line: Some(1),
                new_line: Some(1),
                content: "a1".into(),
            }
        );
        assert_eq!(
            rows[3],
            DiffRow::Line {
                kind: DiffLineKind::Context,
                old_line: Some(50),
                new_line: Some(50),
                content: "a50".into(),
            }
        );
    }

    #[test]
    fn test_flatten_added_file_lines_carry_no_old_line_number() {
        let hunks = vec![DiffHunk {
            old_start: 0,
            old_len: 0,
            new_start: 1,
            new_len: 2,
            lines: vec![
                line(DiffLineKind::Add, "new1"),
                line(DiffLineKind::Add, "new2"),
            ],
        }];

        let rows = flatten_hunks(hunks);
        for row in &rows[1..] {
            match row {
                DiffRow::Line {
                    old_line, new_line, ..
                } => {
                    assert_eq!(*old_line, None);
                    assert!(new_line.is_some());
                }
                other => panic!("expected a line row, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_flatten_empty_hunks_yields_no_rows() {
        assert!(flatten_hunks(vec![]).is_empty());
    }

    // --- DiffViewState::from_payload ---

    #[test]
    fn test_from_payload_hunks_flattens_into_rows() {
        let payload = FileDiffPayload::Hunks {
            hunks: vec![DiffHunk {
                old_start: 1,
                old_len: 1,
                new_start: 1,
                new_len: 1,
                lines: vec![line(DiffLineKind::Context, "a1")],
            }],
        };
        match DiffViewState::from_payload(payload) {
            DiffViewState::Hunks(rows) => assert_eq!(rows.len(), 2),
            other => panic!("expected Hunks state, got {other:?}"),
        }
    }

    #[test]
    fn test_from_payload_empty_hunks_yields_empty_rows_not_a_sentinel() {
        // Empty `hunks` means "identical to HEAD", not a sentinel — the render
        // path shows "No changes" for this, distinct from binary/too-large.
        match DiffViewState::from_payload(FileDiffPayload::Hunks { hunks: vec![] }) {
            DiffViewState::Hunks(rows) => assert!(rows.is_empty()),
            other => panic!("expected an empty Hunks state, got {other:?}"),
        }
    }

    #[test]
    fn test_from_payload_binary_and_too_large_map_to_their_sentinels() {
        assert_eq!(
            DiffViewState::from_payload(FileDiffPayload::Binary),
            DiffViewState::Binary
        );
        assert_eq!(
            DiffViewState::from_payload(FileDiffPayload::TooLarge),
            DiffViewState::TooLarge
        );
    }

    // --- git_update_action ---

    #[test]
    fn test_git_update_action_refreshes_when_open_path_is_still_changed() {
        assert_eq!(
            git_update_action("a.rs", &["a.rs".to_owned(), "b.rs".to_owned()], &[]),
            GitUpdateAction::Refresh
        );
    }

    #[test]
    fn test_git_update_action_closes_when_open_path_left_the_changed_set() {
        assert_eq!(
            git_update_action("a.rs", &[], &["a.rs".to_owned()]),
            GitUpdateAction::Close
        );
    }

    #[test]
    fn test_git_update_action_close_wins_when_open_path_is_in_both_lists() {
        // Not a shape the daemon actually produces (`changed`/`cleared` are
        // disjoint by construction), but `Close` is the safer default if it
        // ever happened.
        assert_eq!(
            git_update_action("a.rs", &["a.rs".to_owned()], &["a.rs".to_owned()]),
            GitUpdateAction::Close
        );
    }

    #[test]
    fn test_git_update_action_none_when_open_path_is_not_mentioned() {
        assert_eq!(
            git_update_action("a.rs", &["b.rs".to_owned()], &["c.rs".to_owned()]),
            GitUpdateAction::None
        );
    }

    #[test]
    fn test_git_update_action_none_when_both_lists_are_empty() {
        assert_eq!(git_update_action("a.rs", &[], &[]), GitUpdateAction::None);
    }

    // --- DiffView::apply_git_update / close ---

    #[gpui::test]
    fn test_apply_git_update_refreshes_the_still_open_path(cx: &mut TestAppContext) {
        let (tx, rx) = flume::unbounded();
        let diff_view = cx.update(|cx| cx.new(|cx| DiffView::new(tx, cx)));

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| view.open_diff("a.rs".to_owned(), cx));
        });
        assert_eq!(
            rx.try_recv().expect("open_diff sends the initial request"),
            "a.rs"
        );

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_git_update(&["a.rs".to_owned()], &[], cx);
            });
        });

        assert_eq!(
            rx.try_recv()
                .expect("a change tick for the open path re-requests its diff"),
            "a.rs"
        );
        cx.update(|cx| {
            assert_eq!(diff_view.read(cx).path.as_deref(), Some("a.rs"));
        });
    }

    #[gpui::test]
    fn test_apply_git_update_closes_the_path_that_left_the_changed_set(cx: &mut TestAppContext) {
        let (tx, rx) = flume::unbounded();
        let diff_view = cx.update(|cx| cx.new(|cx| DiffView::new(tx, cx)));

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| view.open_diff("a.rs".to_owned(), cx));
        });
        rx.try_recv().expect("open_diff sends the initial request");

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_git_update(&[], &["a.rs".to_owned()], cx);
            });
        });

        assert!(
            rx.try_recv().is_err(),
            "leaving the changed set sends no new diff request"
        );
        cx.update(|cx| {
            let view = diff_view.read(cx);
            assert!(view.path.is_none());
            assert_eq!(view.state, DiffViewState::Empty);
        });
    }

    #[gpui::test]
    fn test_apply_git_update_is_a_no_op_when_no_diff_is_open(cx: &mut TestAppContext) {
        let (tx, rx) = flume::unbounded();
        let diff_view = cx.update(|cx| cx.new(|cx| DiffView::new(tx, cx)));

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_git_update(&["a.rs".to_owned()], &["b.rs".to_owned()], cx);
            });
        });

        assert!(
            rx.try_recv().is_err(),
            "no diff open means no request is sent"
        );
        cx.update(|cx| {
            assert!(diff_view.read(cx).path.is_none());
        });
    }

    // --- DiffView::apply_content_update (#488) ---

    #[gpui::test]
    fn test_apply_content_update_refreshes_the_open_path_on_a_content_only_change(
        cx: &mut TestAppContext,
    ) {
        let (tx, rx) = flume::unbounded();
        let diff_view = cx.update(|cx| cx.new(|cx| DiffView::new(tx, cx)));

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| view.open_diff("a.rs".to_owned(), cx));
        });
        assert_eq!(
            rx.try_recv().expect("open_diff sends the initial request"),
            "a.rs"
        );

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_content_update(&["a.rs".to_owned()], cx);
            });
        });

        assert_eq!(
            rx.try_recv()
                .expect("a content-only tick for the open path re-requests its diff"),
            "a.rs"
        );
        cx.update(|cx| {
            assert_eq!(diff_view.read(cx).path.as_deref(), Some("a.rs"));
        });
    }

    #[gpui::test]
    fn test_apply_content_update_ignores_a_tick_that_does_not_mention_the_open_path(
        cx: &mut TestAppContext,
    ) {
        let (tx, rx) = flume::unbounded();
        let diff_view = cx.update(|cx| cx.new(|cx| DiffView::new(tx, cx)));

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| view.open_diff("a.rs".to_owned(), cx));
        });
        rx.try_recv().expect("open_diff sends the initial request");

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_content_update(&["b.rs".to_owned()], cx);
            });
        });

        assert!(
            rx.try_recv().is_err(),
            "a tick for an unrelated path sends no new diff request"
        );
        cx.update(|cx| {
            assert_eq!(diff_view.read(cx).path.as_deref(), Some("a.rs"));
        });
    }

    #[gpui::test]
    fn test_apply_content_update_is_a_no_op_when_no_diff_is_open(cx: &mut TestAppContext) {
        let (tx, rx) = flume::unbounded();
        let diff_view = cx.update(|cx| cx.new(|cx| DiffView::new(tx, cx)));

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_content_update(&["a.rs".to_owned()], cx);
            });
        });

        assert!(
            rx.try_recv().is_err(),
            "no diff open means no request is sent"
        );
        cx.update(|cx| {
            assert!(diff_view.read(cx).path.is_none());
        });
    }

    // --- DiffView::open_diff refresh semantics (#487) ---

    fn one_context_line_payload() -> FileDiffPayload {
        FileDiffPayload::Hunks {
            hunks: vec![DiffHunk {
                old_start: 1,
                old_len: 1,
                new_start: 1,
                new_len: 1,
                lines: vec![line(DiffLineKind::Context, "a1")],
            }],
        }
    }

    #[gpui::test]
    fn test_open_diff_same_path_keeps_rendered_diff_until_reply(cx: &mut TestAppContext) {
        let (tx, rx) = flume::unbounded();
        let diff_view = cx.update(|cx| cx.new(|cx| DiffView::new(tx, cx)));

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.open_diff("a.rs".to_owned(), cx);
                view.apply_file_diff("a.rs".to_owned(), one_context_line_payload(), cx);
            });
        });
        rx.try_recv().expect("open_diff sends the initial request");

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| view.open_diff("a.rs".to_owned(), cx));
        });

        assert_eq!(
            rx.try_recv().expect("re-opening the same path re-requests"),
            "a.rs"
        );
        cx.update(|cx| {
            assert!(
                matches!(diff_view.read(cx).state, DiffViewState::Hunks(_)),
                "the rendered diff stays visible instead of flashing Loading"
            );
        });
    }

    #[gpui::test]
    fn test_open_diff_different_path_arms_loading(cx: &mut TestAppContext) {
        let (tx, rx) = flume::unbounded();
        let diff_view = cx.update(|cx| cx.new(|cx| DiffView::new(tx, cx)));

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.open_diff("a.rs".to_owned(), cx);
                view.apply_file_diff("a.rs".to_owned(), one_context_line_payload(), cx);
                view.open_diff("b.rs".to_owned(), cx);
            });
        });

        rx.try_recv().expect("open_diff sends the initial request");
        assert_eq!(
            rx.try_recv().expect("opening a new path sends its request"),
            "b.rs"
        );
        cx.update(|cx| {
            let view = diff_view.read(cx);
            assert_eq!(view.path.as_deref(), Some("b.rs"));
            assert_eq!(
                view.state,
                DiffViewState::Loading,
                "a different path must not show the previous file's diff"
            );
        });
    }

    #[gpui::test]
    fn test_apply_git_update_refresh_keeps_rendered_diff_then_reply_swaps_it(
        cx: &mut TestAppContext,
    ) {
        let (tx, rx) = flume::unbounded();
        let diff_view = cx.update(|cx| cx.new(|cx| DiffView::new(tx, cx)));

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.open_diff("a.rs".to_owned(), cx);
                view.apply_file_diff("a.rs".to_owned(), one_context_line_payload(), cx);
                view.apply_git_update(&["a.rs".to_owned()], &[], cx);
            });
        });

        rx.try_recv().expect("open_diff sends the initial request");
        rx.try_recv()
            .expect("the refresh tick re-requests the diff");
        cx.update(|cx| {
            assert!(
                matches!(diff_view.read(cx).state, DiffViewState::Hunks(_)),
                "the refresh tick keeps the rendered diff visible"
            );
        });

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_file_diff("a.rs".to_owned(), FileDiffPayload::Binary, cx);
            });
        });
        cx.update(|cx| {
            assert_eq!(
                diff_view.read(cx).state,
                DiffViewState::Binary,
                "the replacement reply still swaps the view"
            );
        });
    }

    #[gpui::test]
    fn test_apply_git_update_ignores_a_tick_that_does_not_mention_the_open_path(
        cx: &mut TestAppContext,
    ) {
        let (tx, rx) = flume::unbounded();
        let diff_view = cx.update(|cx| cx.new(|cx| DiffView::new(tx, cx)));

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| view.open_diff("a.rs".to_owned(), cx));
        });
        rx.try_recv().expect("open_diff sends the initial request");

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_git_update(&["other.rs".to_owned()], &["another.rs".to_owned()], cx);
            });
        });

        assert!(
            rx.try_recv().is_err(),
            "an update naming unrelated paths sends no new request"
        );
        cx.update(|cx| {
            assert_eq!(diff_view.read(cx).path.as_deref(), Some("a.rs"));
        });
    }
}
