//! Problems panel: a dockable, project-wide diagnostics list
//! (`docs/spec-problems-panel.md`, issue #342).
//!
//! Reads [`crate::worktree::WorktreeModel::all_diagnostics`] — the same
//! project-wide `path -> server -> Vec<Diagnostic>` map the file tree already
//! mirrors and the editor's inline markers already consume (#178, #189) — and
//! renders it grouped by file, sorted by severity then location, with per-file
//! and aggregate error/warning counts. A pure read: no new protocol, no new
//! stream, no diagnostic authoring.
//!
//! [`ProblemsSummary::from_diagnostics`] is the pure grouping/sorting/counting
//! logic, independent of GPUI so it is unit-testable without a window. The
//! shared [`DiagnosticSeverity`] derives no `Ord` (adding one would be a
//! protocol change), so [`severity_ordinal`] maps it to a local ordinal here.
//!
//! Live updates ride the file tree's own `cx.notify()`: [`ProblemsPanel`]
//! observes the [`FileTree`] entity it is handed at construction, so every
//! fold the workspace already performs (`Diagnostics` -> `apply_diagnostics`)
//! repaints this panel too — no new bridge from the daemon stream.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement,
    ParentElement as _, Pixels, Render, SharedString, Size, Styled as _, Subscription, Window,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{v_virtual_list, ActiveTheme as _, VirtualListScrollHandle};
use rift_protocol::{Diagnostic, DiagnosticSeverity};

use crate::file_tree::FileTree;

/// Stable, distinct dock-panel identity for the problems panel
/// (`Panel::panel_name`). Once shipped this must not change — it is the
/// persisted panel identifier.
pub const PROBLEMS_PANEL_NAME: &str = "problems";

/// Fixed row height for every rendered row (file header or diagnostic entry).
/// A uniform height keeps the virtual list's size vector trivial, mirroring
/// the file tree's `ROW_HEIGHT`.
const ROW_HEIGHT: Pixels = px(22.0);

/// Height of the fixed aggregate-count line above the scrollable list.
const SUMMARY_HEIGHT: Pixels = px(28.0);

/// Local severity ordinal: `DiagnosticSeverity` derives no `Ord` (adding one
/// would be a protocol change — see the spec constraints), so sorting maps
/// each severity to this scale, matching the enum's declared order
/// (Error > Warning > Information > Hint).
fn severity_ordinal(severity: DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Error => 0,
        DiagnosticSeverity::Warning => 1,
        DiagnosticSeverity::Information => 2,
        DiagnosticSeverity::Hint => 3,
    }
}

/// Per-severity diagnostic counts, in `DiagnosticSeverity` order.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SeverityCounts {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    pub hints: usize,
}

impl SeverityCounts {
    fn from_diagnostics(items: &[Diagnostic]) -> Self {
        let mut counts = Self::default();
        for item in items {
            match item.severity {
                DiagnosticSeverity::Error => counts.errors += 1,
                DiagnosticSeverity::Warning => counts.warnings += 1,
                DiagnosticSeverity::Information => counts.infos += 1,
                DiagnosticSeverity::Hint => counts.hints += 1,
            }
        }
        counts
    }

    fn add(&mut self, other: &Self) {
        self.errors += other.errors;
        self.warnings += other.warnings;
        self.infos += other.infos;
        self.hints += other.hints;
    }

    /// The ordinal of the worst severity present — the sort key that surfaces
    /// files with errors before files with only warnings, and so on. Only
    /// meaningful for a non-empty group (every group in [`ProblemsSummary`]
    /// backs at least one diagnostic, since the model never holds an empty
    /// per-file entry — see `WorktreeModel::apply_diagnostics`).
    fn worst_ordinal(&self) -> u8 {
        if self.errors > 0 {
            0
        } else if self.warnings > 0 {
            1
        } else if self.infos > 0 {
            2
        } else {
            3
        }
    }
}

/// One file's diagnostics, flattened across servers, sorted by severity then
/// location, plus that file's per-severity counts.
#[derive(Debug, Clone, PartialEq)]
pub struct ProblemGroup {
    pub path: String,
    pub diagnostics: Vec<Diagnostic>,
    pub counts: SeverityCounts,
}

/// The project-wide problems view: every file's group, ordered so files with
/// errors surface first, plus the aggregate counts across every group.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProblemsSummary {
    pub groups: Vec<ProblemGroup>,
    pub totals: SeverityCounts,
}

impl ProblemsSummary {
    /// Build the grouped, sorted, counted summary from the model's
    /// project-wide diagnostics map. Pure function — no GPUI — so the
    /// grouping/sorting/counting logic is unit-testable without a window.
    ///
    /// Diagnostics are independent of the file tree's entries (the model keys
    /// them separately, `WorktreeModel::apply_diagnostics`), so a path with no
    /// tree entry still produces a group here.
    pub fn from_diagnostics(
        diagnostics: &BTreeMap<String, BTreeMap<String, Vec<Diagnostic>>>,
    ) -> Self {
        let mut groups: Vec<ProblemGroup> = diagnostics
            .iter()
            .map(|(path, by_server)| {
                let mut items: Vec<Diagnostic> = by_server.values().flatten().cloned().collect();
                items.sort_by(|a, b| {
                    severity_ordinal(a.severity)
                        .cmp(&severity_ordinal(b.severity))
                        .then_with(|| a.range.start.line.cmp(&b.range.start.line))
                        .then_with(|| a.range.start.character.cmp(&b.range.start.character))
                });
                let counts = SeverityCounts::from_diagnostics(&items);
                ProblemGroup {
                    path: path.clone(),
                    diagnostics: items,
                    counts,
                }
            })
            .collect();

        groups.sort_by(|a, b| {
            a.counts
                .worst_ordinal()
                .cmp(&b.counts.worst_ordinal())
                .then_with(|| a.path.cmp(&b.path))
        });

        let mut totals = SeverityCounts::default();
        for group in &groups {
            totals.add(&group.counts);
        }

        Self { groups, totals }
    }
}

/// `count noun`/`count nouns` — the small pluralization the "N errors, M
/// warnings" aggregate/per-file lines need.
fn pluralize(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// The "N errors, M warnings" line shared by the aggregate summary and each
/// file header.
fn counts_line(counts: &SeverityCounts) -> String {
    format!(
        "{}, {}",
        pluralize(counts.errors, "error"),
        pluralize(counts.warnings, "warning")
    )
}

/// One flattened row of the virtualized list: either a file's header (path +
/// per-file counts) or one of its diagnostics. Derived fresh from the model
/// each render — never stored, mirroring the file tree's `Row`.
enum ProblemRow {
    Header {
        path: String,
        counts: SeverityCounts,
    },
    Entry {
        diagnostic: Diagnostic,
    },
}

/// The problems panel view: a virtualized, grouped, sorted read of the file
/// tree's mirrored [`crate::worktree::WorktreeModel`] diagnostics.
pub struct ProblemsPanel {
    file_tree: Entity<FileTree>,
    focus_handle: FocusHandle,
    scroll_handle: VirtualListScrollHandle,
    /// Repaints this panel whenever the observed file tree notifies (the
    /// workspace's daemon-stream bridge already calls `cx.notify()` on the
    /// tree after every `Diagnostics` fold) — the "live" requirement, with no
    /// new stream.
    _observe_model: Subscription,
}

impl ProblemsPanel {
    /// Build a problems panel that mirrors `file_tree`'s model.
    pub fn new(file_tree: Entity<FileTree>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&file_tree, |_this, _tree, cx| cx.notify());
        Self {
            file_tree,
            focus_handle: cx.focus_handle(),
            scroll_handle: VirtualListScrollHandle::new(),
            _observe_model: observe,
        }
    }

    /// The current grouped/sorted/counted summary — the headless handle used
    /// by tests and by `Render`.
    pub fn summary(&self, cx: &App) -> ProblemsSummary {
        ProblemsSummary::from_diagnostics(self.file_tree.read(cx).model().all_diagnostics())
    }

    /// Flatten a summary's groups into the virtual list's row sequence.
    fn rows(summary: &ProblemsSummary) -> Vec<ProblemRow> {
        summary
            .groups
            .iter()
            .flat_map(|group| {
                std::iter::once(ProblemRow::Header {
                    path: group.path.clone(),
                    counts: group.counts,
                })
                .chain(
                    group
                        .diagnostics
                        .iter()
                        .cloned()
                        .map(|diagnostic| ProblemRow::Entry { diagnostic }),
                )
            })
            .collect()
    }

    fn severity_color(severity: DiagnosticSeverity, cx: &Context<Self>) -> Hsla {
        match severity {
            DiagnosticSeverity::Error => cx.theme().danger,
            DiagnosticSeverity::Warning => cx.theme().warning,
            DiagnosticSeverity::Information => cx.theme().info,
            DiagnosticSeverity::Hint => cx.theme().muted_foreground,
        }
    }

    /// Render one row: a file header (path + per-file "N errors, M warnings")
    /// or a diagnostic entry (severity dot, `line:col`, message with optional
    /// source/code).
    fn render_row(row: &ProblemRow, cx: &mut Context<Self>) -> impl IntoElement {
        match row {
            ProblemRow::Header { path, counts } => div()
                .h(ROW_HEIGHT)
                .flex()
                .items_center()
                .px(px(8.0))
                .gap(px(8.0))
                .text_sm()
                .bg(cx.theme().muted)
                .child(div().text_color(cx.theme().foreground).child(path.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(counts_line(counts)),
                )
                .into_any_element(),
            ProblemRow::Entry { diagnostic } => {
                let color = Self::severity_color(diagnostic.severity, cx);
                let line = diagnostic.range.start.line + 1;
                let character = diagnostic.range.start.character + 1;

                let mut label = String::new();
                if let Some(source) = &diagnostic.source {
                    label.push_str(&format!("[{source}] "));
                }
                label.push_str(&diagnostic.message);
                if let Some(code) = &diagnostic.code {
                    label.push_str(&format!(" ({code})"));
                }

                div()
                    .h(ROW_HEIGHT)
                    .flex()
                    .items_center()
                    .pl(px(20.0))
                    .pr(px(8.0))
                    .gap(px(6.0))
                    .text_sm()
                    .child(
                        div()
                            .w(px(8.0))
                            .flex_shrink_0()
                            .text_color(color)
                            .child("\u{25cf}"),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{line}:{character}")),
                    )
                    .child(div().text_color(cx.theme().foreground).child(label))
                    .into_any_element()
            }
        }
    }
}

impl Focusable for ProblemsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for ProblemsPanel {}

impl Panel for ProblemsPanel {
    fn panel_name(&self) -> &'static str {
        PROBLEMS_PANEL_NAME
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Problems")
    }
}

impl Render for ProblemsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let summary = self.summary(cx);

        if summary.groups.is_empty() {
            return div()
                .size_full()
                .p(px(8.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No problems")
                .into_any_element();
        }

        let rows = Self::rows(&summary);
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
                    .h(SUMMARY_HEIGHT)
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .px(px(8.0))
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(counts_line(&summary.totals)),
            )
            .child(
                div().flex_1().min_h_0().child(
                    v_virtual_list(
                        cx.entity().clone(),
                        "problems-list",
                        item_sizes,
                        move |this, visible_range, _window, cx| {
                            let summary = this.summary(cx);
                            let rows = Self::rows(&summary);
                            visible_range
                                .filter_map(|ix| rows.get(ix).map(|row| Self::render_row(row, cx)))
                                .map(IntoElement::into_any_element)
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
    use rift_protocol::{Position, Range};

    fn diag(line: u32, character: u32, severity: DiagnosticSeverity, message: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position { line, character },
                end: Position {
                    line,
                    character: character + 1,
                },
            },
            severity,
            message: message.to_owned(),
            source: None,
            code: None,
        }
    }

    fn map_of(
        entries: Vec<(&str, &str, Vec<Diagnostic>)>,
    ) -> BTreeMap<String, BTreeMap<String, Vec<Diagnostic>>> {
        let mut map: BTreeMap<String, BTreeMap<String, Vec<Diagnostic>>> = BTreeMap::new();
        for (path, server, items) in entries {
            map.entry(path.to_owned())
                .or_default()
                .insert(server.to_owned(), items);
        }
        map
    }

    #[test]
    fn test_empty_map_yields_empty_summary() {
        let summary = ProblemsSummary::from_diagnostics(&BTreeMap::new());
        assert!(summary.groups.is_empty());
        assert_eq!(summary.totals, SeverityCounts::default());
    }

    #[test]
    fn test_group_flattens_diagnostics_across_servers_for_one_file() {
        let map = map_of(vec![
            (
                "a.rs",
                "rust-analyzer",
                vec![diag(1, 0, DiagnosticSeverity::Error, "mismatched types")],
            ),
            (
                "a.rs",
                "clippy",
                vec![diag(3, 0, DiagnosticSeverity::Warning, "needless clone")],
            ),
        ]);

        let summary = ProblemsSummary::from_diagnostics(&map);
        assert_eq!(summary.groups.len(), 1);
        assert_eq!(summary.groups[0].path, "a.rs");
        assert_eq!(summary.groups[0].diagnostics.len(), 2);
    }

    #[test]
    fn test_entries_within_a_file_sort_by_severity_then_location() {
        let map = map_of(vec![(
            "a.rs",
            "rust-analyzer",
            vec![
                diag(5, 0, DiagnosticSeverity::Warning, "unused variable"),
                diag(1, 2, DiagnosticSeverity::Error, "second error"),
                diag(1, 0, DiagnosticSeverity::Error, "first error"),
                diag(2, 0, DiagnosticSeverity::Hint, "a hint"),
            ],
        )]);

        let summary = ProblemsSummary::from_diagnostics(&map);
        let messages: Vec<&str> = summary.groups[0]
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(
            messages,
            vec!["first error", "second error", "unused variable", "a hint"]
        );
    }

    #[test]
    fn test_files_with_errors_surface_before_files_without() {
        let map = map_of(vec![
            (
                "warn_only.rs",
                "rust-analyzer",
                vec![diag(1, 0, DiagnosticSeverity::Warning, "unused import")],
            ),
            (
                "has_error.rs",
                "rust-analyzer",
                vec![diag(1, 0, DiagnosticSeverity::Error, "mismatched types")],
            ),
        ]);

        let summary = ProblemsSummary::from_diagnostics(&map);
        let paths: Vec<&str> = summary.groups.iter().map(|g| g.path.as_str()).collect();
        assert_eq!(paths, vec!["has_error.rs", "warn_only.rs"]);
    }

    #[test]
    fn test_files_with_equal_worst_severity_tie_break_by_path() {
        let map = map_of(vec![
            (
                "z.rs",
                "rust-analyzer",
                vec![diag(1, 0, DiagnosticSeverity::Error, "e")],
            ),
            (
                "a.rs",
                "rust-analyzer",
                vec![diag(1, 0, DiagnosticSeverity::Error, "e")],
            ),
        ]);

        let summary = ProblemsSummary::from_diagnostics(&map);
        let paths: Vec<&str> = summary.groups.iter().map(|g| g.path.as_str()).collect();
        assert_eq!(paths, vec!["a.rs", "z.rs"]);
    }

    #[test]
    fn test_per_file_and_aggregate_counts_are_correct() {
        let map = map_of(vec![
            (
                "a.rs",
                "rust-analyzer",
                vec![
                    diag(1, 0, DiagnosticSeverity::Error, "e1"),
                    diag(2, 0, DiagnosticSeverity::Error, "e2"),
                    diag(3, 0, DiagnosticSeverity::Warning, "w1"),
                ],
            ),
            (
                "b.rs",
                "clippy",
                vec![
                    diag(1, 0, DiagnosticSeverity::Warning, "w2"),
                    diag(2, 0, DiagnosticSeverity::Hint, "h1"),
                ],
            ),
        ]);

        let summary = ProblemsSummary::from_diagnostics(&map);
        let a = summary
            .groups
            .iter()
            .find(|g| g.path == "a.rs")
            .expect("a.rs present");
        assert_eq!(a.counts.errors, 2);
        assert_eq!(a.counts.warnings, 1);

        let b = summary
            .groups
            .iter()
            .find(|g| g.path == "b.rs")
            .expect("b.rs present");
        assert_eq!(b.counts.warnings, 1);
        assert_eq!(b.counts.hints, 1);

        assert_eq!(summary.totals.errors, 2);
        assert_eq!(summary.totals.warnings, 2);
        assert_eq!(summary.totals.hints, 1);
    }

    #[test]
    fn test_diagnostic_for_a_path_with_no_tree_entry_still_lists() {
        // The model keys diagnostics independently of tree entries
        // (`WorktreeModel::apply_diagnostics`): a path present only in the
        // diagnostics map (no matching tree entry) must still produce a group.
        let map = map_of(vec![(
            "generated/out.rs",
            "rust-analyzer",
            vec![diag(
                1,
                0,
                DiagnosticSeverity::Error,
                "generated file error",
            )],
        )]);

        let summary = ProblemsSummary::from_diagnostics(&map);
        assert_eq!(summary.groups.len(), 1);
        assert_eq!(summary.groups[0].path, "generated/out.rs");
    }

    #[test]
    fn test_counts_line_pluralizes_singular_counts() {
        assert_eq!(
            counts_line(&SeverityCounts {
                errors: 1,
                warnings: 0,
                infos: 0,
                hints: 0,
            }),
            "1 error, 0 warnings"
        );
        assert_eq!(
            counts_line(&SeverityCounts {
                errors: 2,
                warnings: 1,
                infos: 0,
                hints: 0,
            }),
            "2 errors, 1 warning"
        );
    }
}
