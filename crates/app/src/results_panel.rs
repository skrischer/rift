//! Results panel: a dockable list of LSP navigation results — find-references
//! and multi-target go-to-definition — in the right dock
//! (`docs/spec-editor-chrome.md` §3, issue #529).
//!
//! # Two consumers, one surface
//!
//! Both overlay consumers the transient inline jump-list used to serve now feed
//! this panel: a [`rift_protocol::DaemonMessage::ReferencesResponse`] and a
//! multi-target [`rift_protocol::DaemonMessage::DefinitionResponse`]. The editor
//! emits [`crate::editor::EditorEvent::ShowResults`] with the result
//! [`ResultsKind`], the searched symbol (for the search-context chip and the
//! per-match highlight), and the [`NavLocation`] list; the workspace routes it
//! to [`ResultsPanel::set_results`] and shows the panel. The jump-list overlay
//! is removed entirely — this panel is the single mechanism for both surfaces.
//!
//! # Anatomy
//!
//! Per the design §3: a mode header naming the kind ("References" /
//! "Definitions") with a close affordance, a search-context chip, an
//! "N results · M files" summary, then file groups (each a header with a count
//! badge) followed by their match rows — line number + the source line with the
//! searched symbol highlighted. One row carries the active accent.
//!
//! # Jump-to-location
//!
//! Clicking a match row emits [`ResultsPanelEvent::OpenLocation`] carrying the
//! full [`NavLocation`] (so the editor preserves the out-of-root read-only
//! carve-out), and marks that row active — the panel stays open so the user can
//! walk the whole result set. The close affordance emits
//! [`ResultsPanelEvent::Close`]; both are routed by the workspace.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, px, App, Context, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    Render, SharedString, Size, Styled as _, Window,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{v_virtual_list, ActiveTheme as _, VirtualListScrollHandle};
use rift_protocol::NavLocation;

/// Which navigation family produced the results — drives the mode header title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultsKind {
    /// Find-references results.
    References,
    /// Multi-target go-to-definition results (e.g. Rust trait impls).
    Definitions,
}

impl ResultsKind {
    /// The mode-header title for this kind (`docs/spec-editor-chrome.md` §3).
    fn title(self) -> &'static str {
        match self {
            ResultsKind::References => "References",
            ResultsKind::Definitions => "Definitions",
        }
    }
}

/// Emitted by the panel for the workspace to route: a match row was activated
/// (jump to it, keeping the panel open) or the panel was closed via its header
/// affordance.
#[derive(Debug, Clone, PartialEq)]
pub enum ResultsPanelEvent {
    /// A match row was selected; jump to `location` in the editor.
    OpenLocation { location: NavLocation },
    /// The close affordance was clicked; the workspace hides the panel.
    Close,
}

/// Stable, distinct dock-panel identity (`Panel::panel_name`). Once shipped this
/// must not change — it is the persisted panel identifier.
pub const RESULTS_PANEL_NAME: &str = "results";

/// Fixed height of a file-group header row.
const GROUP_HEIGHT: Pixels = px(24.0);

/// Fixed height of a single match row.
const ROW_HEIGHT: Pixels = px(22.0);

/// Left inset applied to a match row's line-number lane, indenting matches under
/// their file-group header.
const MATCH_INDENT: Pixels = px(14.0);

/// Width of the line-number lane preceding each match's source line.
const LINE_LANE_WIDTH: Pixels = px(44.0);

/// The transient result set the panel is currently showing.
struct ResultsData {
    kind: ResultsKind,
    /// The searched symbol, for the header chip and per-match highlight. `None`
    /// when the editor could not resolve a token at the request cursor.
    symbol: Option<SharedString>,
    locations: Vec<NavLocation>,
}

/// One flattened list row: a file-group header or a match under it. Rebuilt from
/// the locations by [`group_rows`] whenever the result set changes.
#[derive(Debug, Clone, PartialEq)]
enum DisplayRow {
    /// A file-group header: the file's path and how many matches it holds.
    Group { path: SharedString, count: usize },
    /// A match row: an index into [`ResultsData::locations`].
    Match { index: usize },
}

/// The results panel view.
pub struct ResultsPanel {
    focus_handle: FocusHandle,
    scroll_handle: VirtualListScrollHandle,
    data: Option<ResultsData>,
    /// Flattened group/match rows for the virtual list — cached from
    /// [`ResultsPanel::set_results`] so a scroll never re-groups.
    rows: Vec<DisplayRow>,
    /// Distinct file count in the current result set (the "M files" summand).
    file_count: usize,
    /// The active row: an index into [`ResultsData::locations`], or `None` when
    /// empty. Set to the first match on a new result set and to whichever match
    /// the user last clicked.
    selected: Option<usize>,
}

impl ResultsPanel {
    /// Build an empty results panel.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            scroll_handle: VirtualListScrollHandle::new(),
            data: None,
            rows: Vec::new(),
            file_count: 0,
            selected: None,
        }
    }

    /// Replace the panel's result set (`docs/spec-editor-chrome.md` §3). Groups
    /// the locations by file, caches the flattened rows, and marks the first
    /// match active. A notify repaints the panel.
    pub fn set_results(
        &mut self,
        kind: ResultsKind,
        symbol: Option<SharedString>,
        locations: Vec<NavLocation>,
        cx: &mut Context<Self>,
    ) {
        let (rows, file_count) = group_rows(&locations);
        self.selected = (!locations.is_empty()).then_some(0);
        self.rows = rows;
        self.file_count = file_count;
        self.data = Some(ResultsData {
            kind,
            symbol,
            locations,
        });
        cx.notify();
    }

    /// Clear the panel back to its empty state (on close). A notify repaints.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.data = None;
        self.rows.clear();
        self.file_count = 0;
        self.selected = None;
        cx.notify();
    }

    /// Mark the match at `index` (into the current locations) active and emit
    /// the jump signal for the workspace to route. A no-op when `index` is out
    /// of range or no data is loaded.
    fn select_and_jump(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(location) = self
            .data
            .as_ref()
            .and_then(|d| d.locations.get(index))
            .cloned()
        else {
            return;
        };
        self.selected = Some(index);
        cx.emit(ResultsPanelEvent::OpenLocation { location });
        cx.notify();
    }

    /// Render a file-group header: the file name, its parent directory muted,
    /// and a count badge.
    fn render_group(
        path: &SharedString,
        count: usize,
        mono_font: SharedString,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let (dir, name) = split_path(path);
        div()
            .flex()
            .items_center()
            .h(GROUP_HEIGHT)
            .px(px(8.0))
            .gap(px(6.0))
            .text_xs()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(6.0))
                    .flex_1()
                    .overflow_hidden()
                    .font_family(mono_font)
                    .child(div().text_color(cx.theme().foreground).child(name))
                    .when(!dir.is_empty(), |el| {
                        el.child(div().text_color(cx.theme().muted_foreground).child(dir))
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .px(px(6.0))
                    .rounded(px(6.0))
                    .bg(cx.theme().muted)
                    .text_color(cx.theme().muted_foreground)
                    .child(count.to_string()),
            )
    }

    /// Render a match row: the line number lane, then the source line with the
    /// searched symbol highlighted. Carries the active accent when selected.
    fn render_match(
        &self,
        location_index: usize,
        mono_font: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let data = self.data.as_ref();
        let location = data.and_then(|d| d.locations.get(location_index));
        let symbol = data.and_then(|d| d.symbol.as_deref());
        let is_selected = self.selected == Some(location_index);

        let line_no = location
            .map(|l| l.range.start.line + 1)
            .unwrap_or_default()
            .to_string();
        let preview = location
            .and_then(|l| l.line_preview.as_deref())
            .unwrap_or("");
        let segments = highlight_segments(preview, symbol);

        let mut root = div()
            .flex()
            .items_center()
            .h(ROW_HEIGHT)
            .pl(MATCH_INDENT)
            .pr(px(8.0))
            .gap(px(6.0))
            .text_xs()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().list_hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                    this.select_and_jump(location_index, cx);
                }),
            )
            .child(
                div()
                    .w(LINE_LANE_WIDTH)
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .font_family(mono_font.clone())
                    .child(line_no),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .font_family(mono_font)
                    .children(segments.into_iter().map(|(text, is_match)| {
                        let mut span = div().child(text);
                        if is_match {
                            span = span
                                .text_color(cx.theme().accent_foreground)
                                .bg(cx.theme().accent.opacity(0.25));
                        }
                        span
                    })),
            );

        if is_selected {
            root = root
                .bg(cx.theme().list_active)
                .border_l_2()
                .border_color(cx.theme().accent);
        }

        root
    }
}

impl Focusable for ResultsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for ResultsPanel {}
impl EventEmitter<ResultsPanelEvent> for ResultsPanel {}

impl Panel for ResultsPanel {
    fn panel_name(&self) -> &'static str {
        RESULTS_PANEL_NAME
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .data
            .as_ref()
            .map(|d| d.kind.title())
            .unwrap_or("Results");
        SharedString::from(title)
    }
}

impl Render for ResultsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(data) = self.data.as_ref() else {
            return div()
                .size_full()
                .p(px(8.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No results")
                .into_any_element();
        };

        let result_count = data.locations.len();
        let summary = summary_text(result_count, self.file_count);
        let chip = data.symbol.clone();
        let mode = data.kind.title();
        let mono_font = cx.theme().mono_font_family.clone();

        // Mode header: title + close affordance, then the chip and summary line.
        let header = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .px(px(8.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child(mode),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .size(px(18.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .text_color(cx.theme().muted_foreground)
                            .hover(|s| s.bg(cx.theme().list_hover))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_this, _event: &MouseDownEvent, _window, cx| {
                                    cx.emit(ResultsPanelEvent::Close);
                                }),
                            )
                            // U+00D7 multiplication sign — a close glyph, not an
                            // emoji.
                            .child("\u{00D7}"),
                    ),
            )
            .when_some(chip, |el, symbol| {
                el.child(
                    div()
                        .self_start()
                        .px(px(6.0))
                        .py(px(1.0))
                        .rounded(px(4.0))
                        .bg(cx.theme().muted)
                        .text_xs()
                        .text_color(cx.theme().foreground)
                        .font_family(mono_font.clone())
                        .child(symbol),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary),
            );

        let item_sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
            self.rows
                .iter()
                .map(|row| match row {
                    DisplayRow::Group { .. } => Size::new(px(0.0), GROUP_HEIGHT),
                    DisplayRow::Match { .. } => Size::new(px(0.0), ROW_HEIGHT),
                })
                .collect(),
        );

        let list = v_virtual_list(
            cx.entity().clone(),
            "results-list",
            item_sizes,
            move |this, visible_range, _window, cx| {
                let mono_font = mono_font.clone();
                visible_range
                    .filter_map(|ix| {
                        this.rows.get(ix).map(|row| match row {
                            DisplayRow::Group { path, count } => {
                                Self::render_group(path, *count, mono_font.clone(), cx)
                                    .into_any_element()
                            }
                            DisplayRow::Match { index } => this
                                .render_match(*index, mono_font.clone(), cx)
                                .into_any_element(),
                        })
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&self.scroll_handle);

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(div().flex_1().min_h(px(0.0)).child(list))
            .into_any_element()
    }
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

/// Group `locations` by file in first-appearance order, returning the flattened
/// group/match rows and the distinct file count. Each file yields one
/// [`DisplayRow::Group`] header followed by a [`DisplayRow::Match`] per location
/// in that file, preserving the daemon's location order within a file.
fn group_rows(locations: &[NavLocation]) -> (Vec<DisplayRow>, usize) {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, loc) in locations.iter().enumerate() {
        match groups.iter_mut().find(|(path, _)| *path == loc.path) {
            Some((_, indices)) => indices.push(i),
            None => groups.push((loc.path.clone(), vec![i])),
        }
    }
    let file_count = groups.len();
    let mut rows = Vec::with_capacity(locations.len() + file_count);
    for (path, indices) in groups {
        rows.push(DisplayRow::Group {
            path: SharedString::from(path),
            count: indices.len(),
        });
        for index in indices {
            rows.push(DisplayRow::Match { index });
        }
    }
    (rows, file_count)
}

/// Split a slash-separated path into its `(parent, file_name)` display parts.
/// The parent keeps its trailing separator dropped; a path with no separator
/// yields an empty parent.
fn split_path(path: &str) -> (SharedString, SharedString) {
    match path.rfind('/') {
        Some(slash) => (
            SharedString::from(path[..slash].to_owned()),
            SharedString::from(path[slash + 1..].to_owned()),
        ),
        None => (SharedString::default(), SharedString::from(path.to_owned())),
    }
}

/// The "N results · M files" summary line (`docs/spec-editor-chrome.md` §3).
/// The `·` is U+00B7 (middle dot), not an emoji.
fn summary_text(result_count: usize, file_count: usize) -> String {
    let results_word = if result_count == 1 {
        "result"
    } else {
        "results"
    };
    let files_word = if file_count == 1 { "file" } else { "files" };
    format!("{result_count} {results_word} \u{00B7} {file_count} {files_word}")
}

/// Split `preview` into `(text, is_match)` segments, highlighting every
/// occurrence of `symbol`. Returns a single non-match segment when `symbol` is
/// `None`, empty, or absent from the preview. `symbol` matches are literal
/// (case-sensitive) substrings; because a match is a byte-exact substring, every
/// returned slice lands on a UTF-8 boundary.
fn highlight_segments(preview: &str, symbol: Option<&str>) -> Vec<(String, bool)> {
    let Some(symbol) = symbol.filter(|s| !s.is_empty()) else {
        return vec![(preview.to_owned(), false)];
    };
    let mut segments = Vec::new();
    let mut rest = preview;
    while let Some(pos) = rest.find(symbol) {
        if pos > 0 {
            segments.push((rest[..pos].to_owned(), false));
        }
        let end = pos + symbol.len();
        segments.push((rest[pos..end].to_owned(), true));
        rest = &rest[end..];
    }
    if !rest.is_empty() {
        segments.push((rest.to_owned(), false));
    }
    if segments.is_empty() {
        segments.push((preview.to_owned(), false));
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, Entity, TestAppContext};
    use rift_protocol::{Position, Range};

    fn loc(path: &str, line: u32, preview: &str) -> NavLocation {
        NavLocation {
            path: path.to_owned(),
            range: Range {
                start: Position { line, character: 0 },
                end: Position { line, character: 0 },
            },
            out_of_root: false,
            line_preview: Some(preview.to_owned()),
        }
    }

    // --- group_rows ---

    #[test]
    fn test_group_rows_groups_by_file_in_first_appearance_order() {
        let locations = vec![
            loc("a.rs", 1, "one"),
            loc("b.rs", 2, "two"),
            loc("a.rs", 3, "three"),
        ];
        let (rows, file_count) = group_rows(&locations);
        assert_eq!(file_count, 2, "two distinct files");
        assert_eq!(
            rows,
            vec![
                DisplayRow::Group {
                    path: "a.rs".into(),
                    count: 2
                },
                DisplayRow::Match { index: 0 },
                DisplayRow::Match { index: 2 },
                DisplayRow::Group {
                    path: "b.rs".into(),
                    count: 1
                },
                DisplayRow::Match { index: 1 },
            ]
        );
    }

    #[test]
    fn test_group_rows_empty_is_empty() {
        let (rows, file_count) = group_rows(&[]);
        assert!(rows.is_empty());
        assert_eq!(file_count, 0);
    }

    // --- split_path ---

    #[test]
    fn test_split_path_separates_parent_and_name() {
        assert_eq!(
            split_path("crates/app/src/editor.rs"),
            ("crates/app/src".into(), "editor.rs".into())
        );
    }

    #[test]
    fn test_split_path_no_separator_has_empty_parent() {
        assert_eq!(split_path("editor.rs"), ("".into(), "editor.rs".into()));
    }

    // --- summary_text ---

    #[test]
    fn test_summary_text_pluralizes_each_count_independently() {
        assert_eq!(summary_text(1, 1), "1 result \u{00B7} 1 file");
        assert_eq!(summary_text(3, 1), "3 results \u{00B7} 1 file");
        assert_eq!(summary_text(0, 0), "0 results \u{00B7} 0 files");
    }

    // --- highlight_segments (valid + malformed) ---

    #[test]
    fn test_highlight_segments_marks_every_symbol_occurrence() {
        let segments = highlight_segments("foo bar foo", Some("foo"));
        assert_eq!(
            segments,
            vec![
                ("foo".to_owned(), true),
                (" bar ".to_owned(), false),
                ("foo".to_owned(), true),
            ]
        );
    }

    #[test]
    fn test_highlight_segments_none_or_empty_symbol_is_one_plain_segment() {
        assert_eq!(
            highlight_segments("let x = 1;", None),
            vec![("let x = 1;".to_owned(), false)]
        );
        assert_eq!(
            highlight_segments("let x = 1;", Some("")),
            vec![("let x = 1;".to_owned(), false)]
        );
    }

    #[test]
    fn test_highlight_segments_absent_symbol_is_one_plain_segment() {
        assert_eq!(
            highlight_segments("let x = 1;", Some("zzz")),
            vec![("let x = 1;".to_owned(), false)]
        );
    }

    #[test]
    fn test_highlight_segments_multibyte_preview_stays_on_char_boundaries() {
        // A multibyte line preview with the symbol after non-ASCII text must not
        // panic on a byte-index slice (find returns byte offsets).
        let segments = highlight_segments("// café then foo", Some("foo"));
        assert_eq!(
            segments,
            vec![
                ("// café then ".to_owned(), false),
                ("foo".to_owned(), true)
            ]
        );
    }

    // --- panel behavior (GPUI entity) ---

    fn build_panel(cx: &mut TestAppContext) -> Entity<ResultsPanel> {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.new(ResultsPanel::new)
        })
    }

    #[gpui::test]
    fn test_set_results_caches_rows_and_selects_the_first_match(cx: &mut TestAppContext) {
        let panel = build_panel(cx);
        cx.update(|cx| {
            panel.update(cx, |panel, cx| {
                panel.set_results(
                    ResultsKind::References,
                    Some("foo".into()),
                    vec![loc("a.rs", 1, "foo one"), loc("a.rs", 2, "foo two")],
                    cx,
                );
            });
            let panel = panel.read(cx);
            assert_eq!(panel.file_count, 1);
            assert_eq!(panel.selected, Some(0), "first match is active by default");
            assert_eq!(panel.rows.len(), 3, "one group header + two matches");
        });
    }

    #[gpui::test]
    fn test_clear_resets_to_empty(cx: &mut TestAppContext) {
        let panel = build_panel(cx);
        cx.update(|cx| {
            panel.update(cx, |panel, cx| {
                panel.set_results(
                    ResultsKind::Definitions,
                    None,
                    vec![loc("a.rs", 1, "x")],
                    cx,
                );
                panel.clear(cx);
            });
            let panel = panel.read(cx);
            assert!(panel.data.is_none());
            assert!(panel.rows.is_empty());
            assert_eq!(panel.selected, None);
        });
    }

    #[gpui::test]
    fn test_select_and_jump_emits_location_and_marks_it_active(cx: &mut TestAppContext) {
        let panel = build_panel(cx);
        let emitted: Rc<std::cell::RefCell<Vec<NavLocation>>> = Rc::new(Default::default());
        cx.update(|cx| {
            let sink = emitted.clone();
            cx.subscribe(&panel, move |_panel, event: &ResultsPanelEvent, _cx| {
                if let ResultsPanelEvent::OpenLocation { location } = event {
                    sink.borrow_mut().push(location.clone());
                }
            })
            .detach();
            panel.update(cx, |panel, cx| {
                panel.set_results(
                    ResultsKind::References,
                    Some("foo".into()),
                    vec![loc("a.rs", 1, "foo one"), loc("b.rs", 5, "foo two")],
                    cx,
                );
                panel.select_and_jump(1, cx);
                assert_eq!(panel.selected, Some(1), "the clicked match is now active");
            });
        });
        assert_eq!(emitted.borrow().len(), 1);
        assert_eq!(emitted.borrow()[0].path, "b.rs");
    }

    #[gpui::test]
    fn test_select_and_jump_out_of_range_is_a_noop(cx: &mut TestAppContext) {
        let panel = build_panel(cx);
        cx.update(|cx| {
            panel.update(cx, |panel, cx| {
                panel.set_results(ResultsKind::References, None, vec![loc("a.rs", 1, "x")], cx);
                panel.select_and_jump(9, cx);
                assert_eq!(
                    panel.selected,
                    Some(0),
                    "selection unchanged for a bad index"
                );
            });
        });
    }
}
