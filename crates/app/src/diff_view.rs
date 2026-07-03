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

    /// Open `path`'s diff: arm the loading state and send the request onto
    /// the protocol. Called by the workspace on
    /// [`crate::source_control::SourceControlEvent::OpenDiff`].
    pub fn open_diff(&mut self, path: String, cx: &mut Context<Self>) {
        if let Err(e) = self.request_diff_tx.try_send(path.clone()) {
            debug!(error = %e, %path, "failed to enqueue diff request");
        }
        self.path = Some(path);
        self.state = DiffViewState::Loading;
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
}
