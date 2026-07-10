//! Problems panel: a dockable, project-wide diagnostics list
//! (`docs/spec-problems-panel.md`, issues #342, #343).
//!
//! Reads [`crate::worktree::WorktreeModel::all_diagnostics`] — the same
//! project-wide `path -> server -> Vec<Diagnostic>` map the file tree already
//! mirrors and the editor's inline markers already consume (#178, #189) — and
//! renders it grouped by file, sorted by severity then location, with per-file
//! and aggregate error/warning counts. A pure read: no new protocol, no
//! diagnostic authoring.
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
//!
//! # Virtualization polish (#344)
//!
//! The observer only marks [`ProblemsPanel::cache_dirty`]; the grouped,
//! sorted, flattened row list is rebuilt by [`ProblemsPanel::refresh_cache`]
//! at most once per paint, mirroring [`crate::file_tree::FileTree`]'s
//! `row_cache`/`cache_dirty` pattern. Without it, `render` and the virtual
//! list's visible-range closure would each re-group/sort/flatten the full
//! diagnostics set independently — on every scroll frame, not just every
//! model change — defeating the point of virtualizing a large set.
//!
//! # Jump-to-location (#343)
//!
//! Clicking a diagnostic row emits [`ProblemsPanelEvent::OpenLocation`], the
//! same shape as the file tree's `FileTreeEvent::OpenFile` — a clean signal
//! the workspace subscribes to and routes to
//! [`crate::editor::EditorView::open_at_range`], the thin public wrapper
//! around the editor's existing LSP-nav jump machinery. No new navigation
//! mechanism; file headers are not clickable, only individual diagnostics.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    Render, SharedString, Size, Styled as _, Subscription, Window,
};
use gpui_component::button::Button;
use gpui_component::dock::{Panel, PanelControl, PanelEvent};
use gpui_component::{v_virtual_list, ActiveTheme as _, VirtualListScrollHandle};
use rift_protocol::{Diagnostic, DiagnosticSeverity, Range};

use crate::workspace::{solo_button, SoloDiagnostics};

use crate::file_tree::FileTree;

/// Emitted when the user selects a diagnostic row — the open-file-at-position
/// signal `docs/spec-problems-panel.md` calls for, routed by the workspace to
/// [`crate::editor::EditorView::open_at_range`] (#343).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemsPanelEvent {
    /// A diagnostic was selected; jump to its file + range. `path` is
    /// root-relative, matching `FileTreeEvent::OpenFile`.
    OpenLocation { path: String, range: Range },
}

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
/// each render — never stored, mirroring the file tree's `Row`. An `Entry`
/// carries its owning file's path (not just the diagnostic) so a click can
/// emit [`ProblemsPanelEvent::OpenLocation`] (#343).
#[derive(Debug, PartialEq)]
enum ProblemRow {
    Header {
        path: String,
        counts: SeverityCounts,
    },
    Entry {
        path: String,
        diagnostic: Diagnostic,
    },
}

/// The problems panel view: a virtualized, grouped, sorted read of the file
/// tree's mirrored [`crate::worktree::WorktreeModel`] diagnostics.
pub struct ProblemsPanel {
    file_tree: Entity<FileTree>,
    focus_handle: FocusHandle,
    scroll_handle: VirtualListScrollHandle,
    /// Grouped/sorted/counted summary as of the last [`ProblemsPanel::refresh_cache`]
    /// call; [`ProblemsPanel::row_cache`] is flattened from it. Stale whenever
    /// [`ProblemsPanel::cache_dirty`] is set — `render` always refreshes
    /// before reading either.
    cached_summary: ProblemsSummary,
    /// Flattened rows the virtual list's visible-range closure reads
    /// directly, rebuilt by [`ProblemsPanel::refresh_cache`] only when
    /// [`ProblemsPanel::cache_dirty`] is set (see the module-level
    /// "Virtualization polish" doc).
    row_cache: Vec<ProblemRow>,
    /// Set by the file-tree observer on every notify (a `Diagnostics` fold
    /// among them); cleared by [`ProblemsPanel::refresh_cache`] once it
    /// rebuilds [`ProblemsPanel::cached_summary`] and
    /// [`ProblemsPanel::row_cache`] from the fresh model state.
    cache_dirty: bool,
    /// Repaints this panel whenever the observed file tree notifies (the
    /// workspace's daemon-stream bridge already calls `cx.notify()` on the
    /// tree after every `Diagnostics` fold) — the "live" requirement, with no
    /// new stream.
    _observe_model: Subscription,
}

impl ProblemsPanel {
    /// Build a problems panel that mirrors `file_tree`'s model.
    pub fn new(file_tree: Entity<FileTree>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&file_tree, |this, _tree, cx| {
            this.cache_dirty = true;
            cx.notify();
        });
        Self {
            file_tree,
            focus_handle: cx.focus_handle(),
            scroll_handle: VirtualListScrollHandle::new(),
            cached_summary: ProblemsSummary::default(),
            row_cache: Vec::new(),
            cache_dirty: true,
            _observe_model: observe,
        }
    }

    /// The current grouped/sorted/counted summary, freshly computed from the
    /// model — the headless handle used by tests. Always live (unlike
    /// [`ProblemsPanel::cached_summary`]): callers that don't render a full
    /// paint per check (e.g. tests) still see the latest model state.
    pub fn summary(&self, cx: &App) -> ProblemsSummary {
        ProblemsSummary::from_diagnostics(self.file_tree.read(cx).model().all_diagnostics())
    }

    /// Rebuild [`ProblemsPanel::cached_summary`] and
    /// [`ProblemsPanel::row_cache`] from the model when
    /// [`ProblemsPanel::cache_dirty`] is set; a no-op otherwise. `render`
    /// calls this once per paint, before the item-size vector and the virtual
    /// list's row closure both read the caches — so a large diagnostics set
    /// is grouped/sorted/flattened once per model change, not once per
    /// visible-range query during a scroll.
    fn refresh_cache(&mut self, cx: &App) {
        if self.cache_dirty {
            self.cached_summary = self.summary(cx);
            self.row_cache = Self::rows(&self.cached_summary);
            self.cache_dirty = false;
        }
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
                .chain(group.diagnostics.iter().cloned().map(|diagnostic| {
                    ProblemRow::Entry {
                        path: group.path.clone(),
                        diagnostic,
                    }
                }))
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
            ProblemRow::Entry { path, diagnostic } => {
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

                let jump_path = path.clone();
                let range = diagnostic.range;

                div()
                    .h(ROW_HEIGHT)
                    .flex()
                    .items_center()
                    .pl(px(20.0))
                    .pr(px(8.0))
                    .gap(px(6.0))
                    .text_sm()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().list_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_this, _event: &MouseDownEvent, _window, cx| {
                            cx.emit(ProblemsPanelEvent::OpenLocation {
                                path: jump_path.clone(),
                                range,
                            });
                        }),
                    )
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
impl EventEmitter<ProblemsPanelEvent> for ProblemsPanel {}

impl Panel for ProblemsPanel {
    fn panel_name(&self) -> &'static str {
        PROBLEMS_PANEL_NAME
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Problems")
    }

    // gpui-component's own native zoom disabled (issue #820, superseding
    // #716): its `ToggleZoom` -> `PanelEvent` path would flip `TabPanel.
    // zoomed` + `DockArea.zoom_view` independently of the rift-owned
    // visible set (`docs/spec-workspace-visibility-rail.md`, "Single source
    // of truth for solo"). `toolbar_buttons` below replaces it with a header
    // button that solos this area through that set instead.
    fn zoomable(&self, _cx: &App) -> Option<PanelControl> {
        None
    }

    fn toolbar_buttons(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Vec<Button>> {
        Some(vec![solo_button(|_, window, cx| {
            window.dispatch_action(Box::new(SoloDiagnostics), cx);
        })])
    }
}

impl Render for ProblemsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Rebuild the caches once for this paint if the model changed since
        // the last one; a no-op otherwise. Both the size vector below and the
        // virtual list's row closure read `row_cache` from here on — see the
        // module-level "Virtualization polish" doc.
        self.refresh_cache(cx);

        if self.cached_summary.groups.is_empty() {
            return div()
                .size_full()
                .p(px(8.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No problems")
                .into_any_element();
        }

        let item_sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
            self.row_cache
                .iter()
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
                    .child(counts_line(&self.cached_summary.totals)),
            )
            .child(
                div().flex_1().min_h_0().child(
                    v_virtual_list(
                        cx.entity().clone(),
                        "problems-list",
                        item_sizes,
                        move |this, visible_range, _window, cx| {
                            // Read the cache built above — the virtual list
                            // only asks for the rows currently on screen, so a
                            // large diagnostics set still paints a bounded
                            // number of elements, but no re-group/sort/
                            // flatten happens here: `row_cache` is already
                            // fresh.
                            let this: &Self = this;
                            visible_range
                                .filter_map(|ix| {
                                    this.row_cache.get(ix).map(|row| Self::render_row(row, cx))
                                })
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
    use gpui::{AppContext as _, TestAppContext};
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
    fn test_rows_threads_the_owning_file_path_onto_each_entry() {
        // The jump-to-location click handler (#343) needs each entry row to
        // carry its owning file's path (not just the diagnostic), since a
        // `Diagnostic` alone carries no path. `rows()` must thread it through.
        let map = map_of(vec![(
            "a.rs",
            "rust-analyzer",
            vec![diag(1, 0, DiagnosticSeverity::Error, "e1")],
        )]);
        let summary = ProblemsSummary::from_diagnostics(&map);

        let rows = ProblemsPanel::rows(&summary);
        assert_eq!(rows.len(), 2, "one header row + one entry row");
        match &rows[1] {
            ProblemRow::Entry { path, diagnostic } => {
                assert_eq!(path, "a.rs");
                assert_eq!(diagnostic.message, "e1");
            }
            other => panic!("expected an entry row, got {other:?}"),
        }
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

    /// Live updates + cache polish (#344): a `Diagnostics` fold onto the
    /// observed file tree marks [`ProblemsPanel::cache_dirty`], and
    /// [`ProblemsPanel::refresh_cache`] then reflects the change in
    /// [`ProblemsPanel::row_cache`] — an error introduced adds rows, and
    /// clearing it drops the file's rows entirely. Headless: constructs the
    /// panel directly against a `FileTree` entity, no window.
    #[gpui::test]
    fn test_refresh_cache_reflects_a_diagnostic_added_then_cleared(cx: &mut TestAppContext) {
        // `cx.observe`'s callback runs on effect flush, which happens when the
        // *outermost* `cx.update` call returns (`App::finish_update`) — not
        // mid-closure on a nested `Entity::update`. So the notify-driven
        // dirty flag is only observable from a separate top-level `cx.update`
        // call after the one that triggered it.
        let (file_tree, panel) = cx.update(|cx| {
            let file_tree = cx.new(|_cx| FileTree::new());
            let panel = cx.new(|cx| ProblemsPanel::new(file_tree.clone(), cx));
            (file_tree, panel)
        });

        cx.update(|cx| {
            assert!(
                panel.read(cx).cache_dirty,
                "a freshly constructed panel starts dirty"
            );
            panel.update(cx, |panel, cx| panel.refresh_cache(cx));
            assert!(
                panel.read(cx).row_cache.is_empty(),
                "no diagnostics folded yet"
            );
        });

        cx.update(|cx| {
            file_tree.update(cx, |tree, cx| {
                tree.model_mut()
                    .apply_snapshot_chunk("/proj".into(), Vec::new(), true);
                tree.model_mut().apply_diagnostics(
                    "a.rs".into(),
                    "rust-analyzer".into(),
                    vec![diag(1, 0, DiagnosticSeverity::Error, "mismatched types")],
                );
                cx.notify();
            });
        });

        cx.update(|cx| {
            assert!(
                panel.read(cx).cache_dirty,
                "observing the file tree's notify marks the cache dirty"
            );
            panel.update(cx, |panel, cx| panel.refresh_cache(cx));
            assert!(!panel.read(cx).cache_dirty);
            assert_eq!(
                panel.read(cx).row_cache.len(),
                2,
                "one header row + one entry row for a.rs"
            );
            assert_eq!(panel.read(cx).cached_summary.totals.errors, 1);
        });

        cx.update(|cx| {
            file_tree.update(cx, |tree, cx| {
                tree.model_mut()
                    .apply_diagnostics("a.rs".into(), "rust-analyzer".into(), vec![]);
                cx.notify();
            });
        });

        cx.update(|cx| {
            panel.update(cx, |panel, cx| panel.refresh_cache(cx));
            assert!(
                panel.read(cx).row_cache.is_empty(),
                "a.rs cleared its only diagnostic and drops out of the cached rows"
            );
            assert_eq!(panel.read(cx).cached_summary.totals.errors, 0);
        });
    }
}
