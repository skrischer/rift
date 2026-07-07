//! Outline panel: a dockable, virtualized tree of the active editor tab's
//! document symbols (`docs/spec-editor-chrome.md` §3, issue #530).
//!
//! Reads [`crate::editor::EditorView::active_document_symbols`] — the same
//! flattened, depth-tagged cache the breadcrumb resolves the enclosing
//! symbol against (#527) — and renders one row per symbol, indented by its
//! `depth`. A pure read: no new protocol, no symbol authoring.
//!
//! # Selection follows cursor
//!
//! [`innermost_symbol_index`] reuses [`crate::editor::enclosing_symbol_chain`]
//! (the same logic the breadcrumb's enclosing-symbol chain is built from) to
//! find the deepest symbol enclosing the active tab's cursor, and highlights
//! that row — one source of truth for "what symbol is the cursor inside",
//! shared with the breadcrumb.
//!
//! # Live updates
//!
//! [`OutlinePanel`] observes the [`crate::editor::EditorView`] entity it is
//! handed at construction: every notify (a document-symbol response landing,
//! a cursor move, a tab switch/open/close) marks the cache dirty, mirroring
//! [`crate::problems_panel::ProblemsPanel`]'s observe-then-rebuild-once-per-
//! paint pattern (`row_cache`/`cache_dirty`, only rebuilt in `render`).
//!
//! # Jump-to-location
//!
//! Clicking a row emits [`OutlinePanelEvent::OpenLocation`], the same shape
//! the problems panel and file tree already use — the workspace subscribes
//! and routes it to [`crate::editor::EditorView::open_at_range`]. The click
//! handler itself only calls [`OutlinePanel::jump_target`], a pure(-ish)
//! lookup directly testable without simulating a pointer event (mirrors
//! `crate::file_tree::FileTree::click_dir`).

use std::rc::Rc;

use gpui::{
    div, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    Render, SharedString, Size, Styled as _, Subscription, Window,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{v_virtual_list, ActiveTheme as _, VirtualListScrollHandle};
use rift_protocol::{DocumentSymbolEntry, Range, SymbolKind};

use crate::editor::{enclosing_symbol_chain, EditorView};

/// Emitted when the user selects an outline row — the open-file-at-position
/// signal, routed by the workspace to
/// [`crate::editor::EditorView::open_at_range`] (mirrors
/// `crate::problems_panel::ProblemsPanelEvent::OpenLocation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutlinePanelEvent {
    /// A symbol row was selected; jump to its `selection_range` in the
    /// active tab's file.
    OpenLocation { path: String, range: Range },
}

/// Stable, distinct dock-panel identity for the outline panel
/// (`Panel::panel_name`). Once shipped this must not change — it is the
/// persisted panel identifier.
pub const OUTLINE_PANEL_NAME: &str = "outline";

/// Fixed row height for every rendered symbol row, matching the problems
/// panel and file tree's uniform-height virtual lists.
const ROW_HEIGHT: Pixels = px(22.0);

/// Horizontal indent applied per `depth` level, matching the file tree's own
/// per-level indent (`crate::file_tree`'s private `INDENT_PER_LEVEL`).
const INDENT_PER_LEVEL: f32 = 14.0;

/// Width of the kind-glyph lane preceding each symbol's name (`docs/spec-
/// editor-chrome.md` §3: "kind glyph lanes").
const GLYPH_LANE_WIDTH: Pixels = px(18.0);

/// One flattened outline row, derived from a [`DocumentSymbolEntry`].
/// Rebuilt fresh from the editor's symbol cache by
/// [`OutlinePanel::refresh_cache`] — never stored across a symbol-tree
/// change, mirroring the problems panel's `ProblemRow`.
#[derive(Debug, Clone, PartialEq)]
struct OutlineRow {
    name: SharedString,
    kind: SymbolKind,
    depth: u32,
    /// The symbol's `selection_range` (`docs/protocol.md`) — the sub-span to
    /// reveal/select on a jump, not the whole enclosing `range`.
    range: Range,
}

/// Coarse category for a [`SymbolKind`], driving the kind-glyph's color —
/// four theme-token buckets, mirroring the problems panel's severity-color
/// precedent (no per-kind hex).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymbolCategory {
    Type,
    Function,
    Value,
    Container,
}

/// Bucket every [`SymbolKind`] variant into a [`SymbolCategory`] — exhaustive
/// match (no `_` arm), so a new `SymbolKind` variant fails to compile here
/// instead of silently falling into the wrong color.
fn symbol_category(kind: SymbolKind) -> SymbolCategory {
    match kind {
        SymbolKind::Class
        | SymbolKind::Struct
        | SymbolKind::Interface
        | SymbolKind::Enum
        | SymbolKind::TypeParameter => SymbolCategory::Type,
        SymbolKind::Function
        | SymbolKind::Method
        | SymbolKind::Constructor
        | SymbolKind::Operator => SymbolCategory::Function,
        SymbolKind::File
        | SymbolKind::Module
        | SymbolKind::Namespace
        | SymbolKind::Package
        | SymbolKind::Event => SymbolCategory::Container,
        SymbolKind::Property
        | SymbolKind::Field
        | SymbolKind::Variable
        | SymbolKind::Constant
        | SymbolKind::String
        | SymbolKind::Number
        | SymbolKind::Boolean
        | SymbolKind::Array
        | SymbolKind::Object
        | SymbolKind::Key
        | SymbolKind::Null
        | SymbolKind::EnumMember => SymbolCategory::Value,
    }
}

/// The kind-glyph lane's theme color for `category` — theme tokens only
/// (constitution: no hardcoded hex).
fn category_color(category: SymbolCategory, cx: &Context<OutlinePanel>) -> Hsla {
    match category {
        SymbolCategory::Type => cx.theme().info,
        SymbolCategory::Function => cx.theme().accent,
        SymbolCategory::Value => cx.theme().success,
        SymbolCategory::Container => cx.theme().muted_foreground,
    }
}

/// The single-letter glyph rendered in a row's kind lane. Every `SymbolKind`
/// maps to a distinct letter (guarded by
/// `test_symbol_glyph_is_distinct_per_kind`) so two different kinds are never
/// visually indistinguishable even before color is taken into account.
fn symbol_glyph(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::File => "F",
        SymbolKind::Module => "M",
        SymbolKind::Namespace => "N",
        SymbolKind::Package => "P",
        SymbolKind::Class => "C",
        SymbolKind::Method => "m",
        SymbolKind::Property => "p",
        SymbolKind::Field => "d",
        SymbolKind::Constructor => "c",
        SymbolKind::Enum => "E",
        SymbolKind::Interface => "I",
        SymbolKind::Function => "f",
        SymbolKind::Variable => "v",
        SymbolKind::Constant => "k",
        SymbolKind::String => "s",
        SymbolKind::Number => "n",
        SymbolKind::Boolean => "b",
        SymbolKind::Array => "a",
        SymbolKind::Object => "o",
        SymbolKind::Key => "y",
        SymbolKind::Null => "0",
        SymbolKind::EnumMember => "e",
        SymbolKind::Struct => "S",
        SymbolKind::Event => "!",
        SymbolKind::Operator => "+",
        SymbolKind::TypeParameter => "T",
    }
}

/// Flatten the editor's cached symbol tree into the virtual list's row
/// sequence — one row per entry, in the daemon's pre-order (`depth`-tagged)
/// list order (`docs/protocol.md`).
fn build_rows(symbols: &[DocumentSymbolEntry]) -> Vec<OutlineRow> {
    symbols
        .iter()
        .map(|s| OutlineRow {
            name: SharedString::from(s.name.clone()),
            kind: s.kind,
            depth: s.depth,
            range: s.selection_range,
        })
        .collect()
}

/// The index into `symbols` of the innermost symbol enclosing `(line,
/// character)` — the outline's "selection follows cursor" signal
/// (`docs/spec-editor-chrome.md` §3). Reuses
/// [`enclosing_symbol_chain`] (already depth-sorted, outermost first) rather
/// than re-deriving the containment check, so the outline and the breadcrumb
/// agree on what "the enclosing symbol" means. `None` when the cursor is
/// inside no symbol.
fn innermost_symbol_index(
    symbols: &[DocumentSymbolEntry],
    line: u32,
    character: u32,
) -> Option<usize> {
    let chain = enclosing_symbol_chain(symbols, line, character);
    let innermost = *chain.last()?;
    symbols.iter().position(|s| std::ptr::eq(s, innermost))
}

/// The outline panel view: a virtualized, depth-indented read of the active
/// editor tab's cached document-symbol tree.
pub struct OutlinePanel {
    editor: Entity<EditorView>,
    focus_handle: FocusHandle,
    scroll_handle: VirtualListScrollHandle,
    /// Flattened rows as of the last [`OutlinePanel::refresh_cache`] call —
    /// stale whenever [`OutlinePanel::cache_dirty`] is set; `render` always
    /// refreshes before reading either this or `selected_row`.
    row_cache: Vec<OutlineRow>,
    /// The `row_cache` index of the symbol enclosing the active tab's cursor
    /// (innermost first), or `None` when the cursor is inside no symbol —
    /// computed alongside `row_cache` so a scroll never re-walks the symbol
    /// tree (`docs/spec-editor-chrome.md`'s "Virtualization polish"
    /// precedent, `crate::problems_panel`).
    selected_row: Option<usize>,
    /// Set by the editor observer below on every notify (a document-symbol
    /// response, a cursor move, a tab switch/open/close among them); cleared
    /// by [`OutlinePanel::refresh_cache`] once it rebuilds `row_cache` and
    /// `selected_row` from the fresh editor state.
    cache_dirty: bool,
    /// Repaints this panel whenever the observed editor notifies — the same
    /// "live" wiring `crate::problems_panel::ProblemsPanel` uses for the file
    /// tree.
    _observe_editor: Subscription,
}

impl OutlinePanel {
    /// Build an outline panel that mirrors `editor`'s active-tab symbol
    /// cache.
    pub fn new(editor: Entity<EditorView>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&editor, |this, _editor, cx| {
            this.cache_dirty = true;
            cx.notify();
        });
        Self {
            editor,
            focus_handle: cx.focus_handle(),
            scroll_handle: VirtualListScrollHandle::new(),
            row_cache: Vec::new(),
            selected_row: None,
            cache_dirty: true,
            _observe_editor: observe,
        }
    }

    /// Rebuild `row_cache` and `selected_row` from the editor's active-tab
    /// symbol cache and cursor when [`OutlinePanel::cache_dirty`] is set; a
    /// no-op otherwise. `render` calls this once per paint, before the
    /// item-size vector and the virtual list's row closure both read
    /// `row_cache` — so a large symbol tree is flattened once per editor
    /// change, not once per visible-range query during a scroll.
    fn refresh_cache(&mut self, cx: &App) {
        if !self.cache_dirty {
            return;
        }
        let editor = self.editor.read(cx);
        let symbols = editor.active_document_symbols();
        self.row_cache = build_rows(symbols);
        self.selected_row = editor
            .cursor_position(cx)
            .and_then(|(line, character)| innermost_symbol_index(symbols, line, character));
        self.cache_dirty = false;
    }

    /// The `(path, range)` a click on row `ix` of `row_cache` would jump to,
    /// or `None` when `ix` is out of range or no file is open. Pure decision
    /// behind the row's mouse-down handler, directly testable without
    /// simulating a pointer event (mirrors
    /// `crate::file_tree::FileTree::click_dir`).
    fn jump_target(&self, ix: usize, cx: &App) -> Option<(String, Range)> {
        let path = self.editor.read(cx).open_path()?.to_owned();
        let row = self.row_cache.get(ix)?;
        Some((path, row.range))
    }

    /// Render one row: the kind-glyph lane, indented by `depth`, then the
    /// mono symbol name — highlighted when `ix` is the cursor's enclosing
    /// symbol (`docs/spec-editor-chrome.md` §3).
    fn render_row(
        row: &OutlineRow,
        ix: usize,
        is_selected: bool,
        mono_font: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let indent = px(row.depth as f32 * INDENT_PER_LEVEL);
        let glyph_color = category_color(symbol_category(row.kind), cx);

        let mut root = div()
            .flex()
            .items_center()
            .h(ROW_HEIGHT)
            .pl(indent)
            .pr(px(8.0))
            .gap(px(6.0))
            .text_sm()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().list_hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                    if let Some((path, range)) = this.jump_target(ix, cx) {
                        cx.emit(OutlinePanelEvent::OpenLocation { path, range });
                    }
                }),
            )
            .child(
                div()
                    .w(GLYPH_LANE_WIDTH)
                    .flex_shrink_0()
                    .text_color(glyph_color)
                    .font_family(mono_font.clone())
                    .child(symbol_glyph(row.kind)),
            )
            .child(
                div()
                    .flex_1()
                    .font_family(mono_font)
                    .child(row.name.clone()),
            );

        if is_selected {
            root = root
                .bg(cx.theme().list_active)
                .text_color(cx.theme().foreground);
        }

        root
    }
}

impl Focusable for OutlinePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for OutlinePanel {}
impl EventEmitter<OutlinePanelEvent> for OutlinePanel {}

impl Panel for OutlinePanel {
    fn panel_name(&self) -> &'static str {
        OUTLINE_PANEL_NAME
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Outline")
    }
}

impl Render for OutlinePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Rebuild the caches once for this paint if the editor changed since
        // the last one; a no-op otherwise. Both the size vector below and the
        // virtual list's row closure read `row_cache`/`selected_row` from
        // here on — see the module-level "Live updates" doc.
        self.refresh_cache(cx);

        if self.editor.read(cx).open_path().is_none() {
            return div()
                .size_full()
                .p(px(8.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No file open")
                .into_any_element();
        }

        if self.row_cache.is_empty() {
            return div()
                .size_full()
                .p(px(8.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No symbols")
                .into_any_element();
        }

        let item_sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
            self.row_cache
                .iter()
                .map(|_| Size::new(px(0.0), ROW_HEIGHT))
                .collect(),
        );
        let mono_font = cx.theme().mono_font_family.clone();
        let selected_row = self.selected_row;

        div()
            .size_full()
            .child(
                v_virtual_list(
                    cx.entity().clone(),
                    "outline-list",
                    item_sizes,
                    move |this, visible_range, _window, cx| {
                        let this: &Self = this;
                        let mono_font = mono_font.clone();
                        visible_range
                            .filter_map(|ix| {
                                this.row_cache.get(ix).map(|row| {
                                    Self::render_row(
                                        row,
                                        ix,
                                        Some(ix) == selected_row,
                                        mono_font.clone(),
                                        cx,
                                    )
                                })
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
    use gpui::{AppContext as _, TestAppContext};
    use rift_protocol::{ClientMessage, Position};
    use std::time::SystemTime;

    fn sym(name: &str, depth: u32, start: (u32, u32), end: (u32, u32)) -> DocumentSymbolEntry {
        let range = Range {
            start: Position {
                line: start.0,
                character: start.1,
            },
            end: Position {
                line: end.0,
                character: end.1,
            },
        };
        DocumentSymbolEntry {
            name: name.to_owned(),
            kind: SymbolKind::Function,
            range,
            selection_range: range,
            depth,
        }
    }

    // --- pure helpers ---

    #[test]
    fn test_build_rows_uses_selection_range_not_range() {
        let mut entry = sym("fn render", 1, (2, 4), (8, 5));
        entry.selection_range = Range {
            start: Position {
                line: 2,
                character: 7,
            },
            end: Position {
                line: 2,
                character: 13,
            },
        };
        let rows = build_rows(std::slice::from_ref(&entry));
        assert_eq!(rows[0].range, entry.selection_range);
        assert_ne!(rows[0].range, entry.range);
    }

    #[test]
    fn test_build_rows_preserves_list_order_and_depth() {
        let symbols = vec![
            sym("impl View", 0, (0, 0), (10, 1)),
            sym("fn render", 1, (1, 4), (8, 5)),
        ];
        let rows = build_rows(&symbols);
        assert_eq!(
            rows.iter().map(|r| r.name.as_ref()).collect::<Vec<_>>(),
            vec!["impl View", "fn render"]
        );
        assert_eq!(rows.iter().map(|r| r.depth).collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn test_innermost_symbol_index_picks_the_deepest_enclosing_symbol() {
        let symbols = vec![
            sym("impl View", 0, (0, 0), (10, 1)),
            sym("fn render", 1, (2, 4), (8, 5)),
            sym("|event|", 2, (4, 8), (6, 9)),
        ];
        assert_eq!(innermost_symbol_index(&symbols, 5, 0), Some(2));
        assert_eq!(innermost_symbol_index(&symbols, 3, 0), Some(1));
        assert_eq!(innermost_symbol_index(&symbols, 0, 0), Some(0));
    }

    #[test]
    fn test_innermost_symbol_index_is_none_outside_every_symbol() {
        let symbols = vec![sym("fn render", 0, (2, 4), (8, 5))];
        assert_eq!(innermost_symbol_index(&symbols, 100, 0), None);
        assert_eq!(innermost_symbol_index(&[], 0, 0), None);
    }

    #[test]
    fn test_symbol_glyph_is_distinct_per_kind() {
        let kinds = [
            SymbolKind::File,
            SymbolKind::Module,
            SymbolKind::Namespace,
            SymbolKind::Package,
            SymbolKind::Class,
            SymbolKind::Method,
            SymbolKind::Property,
            SymbolKind::Field,
            SymbolKind::Constructor,
            SymbolKind::Enum,
            SymbolKind::Interface,
            SymbolKind::Function,
            SymbolKind::Variable,
            SymbolKind::Constant,
            SymbolKind::String,
            SymbolKind::Number,
            SymbolKind::Boolean,
            SymbolKind::Array,
            SymbolKind::Object,
            SymbolKind::Key,
            SymbolKind::Null,
            SymbolKind::EnumMember,
            SymbolKind::Struct,
            SymbolKind::Event,
            SymbolKind::Operator,
            SymbolKind::TypeParameter,
        ];
        let mut glyphs: Vec<&str> = kinds.iter().map(|k| symbol_glyph(*k)).collect();
        let before = glyphs.len();
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(
            glyphs.len(),
            before,
            "every SymbolKind must render a distinct glyph"
        );
    }

    #[test]
    fn test_symbol_category_groups_related_kinds() {
        assert_eq!(symbol_category(SymbolKind::Struct), SymbolCategory::Type);
        assert_eq!(
            symbol_category(SymbolKind::Method),
            SymbolCategory::Function
        );
        assert_eq!(symbol_category(SymbolKind::Constant), SymbolCategory::Value);
        assert_eq!(
            symbol_category(SymbolKind::Module),
            SymbolCategory::Container
        );
    }

    // --- GPUI-level: cache reactivity, selection-follows-cursor, jump target ---

    /// Build an `EditorView` inside a fresh window (mirrors
    /// `crate::editor`'s own private `build_test_editor_full`, which this
    /// module cannot reach — `EditorView::new` is the public constructor
    /// both use).
    fn build_test_editor(
        cx: &mut TestAppContext,
    ) -> (
        Entity<EditorView>,
        gpui::WindowHandle<gpui_component::Root>,
        flume::Receiver<ClientMessage>,
    ) {
        let (open_file_tx, _open_file_rx) = flume::unbounded();
        let (save_file_tx, _save_file_rx) = flume::unbounded();
        let (buffer_change_tx, _buffer_change_rx) = flume::unbounded();
        let (nav_tx, nav_rx) = flume::unbounded();

        let mut editor: Option<Entity<EditorView>> = None;
        let window = cx.update(|cx| {
            gpui_component::init(cx);
            cx.open_window(Default::default(), |window, cx| {
                editor = Some(cx.new(|cx| {
                    EditorView::new(
                        open_file_tx,
                        save_file_tx,
                        buffer_change_tx,
                        nav_tx,
                        window,
                        cx,
                    )
                }));
                cx.new(|cx| gpui_component::Root::new(editor.clone().unwrap(), window, cx))
            })
            .unwrap()
        });
        (
            editor.expect("editor constructed inside the window callback"),
            window,
            nav_rx,
        )
    }

    /// Open `path` in `editor` and answer the `DocumentSymbolRequest` the
    /// load dispatches with `symbols` — the minimal public-API path to a
    /// loaded tab with a populated symbol cache (`EditorView` exposes no
    /// test-only symbol setter; `tabs`/`symbols` are private to `editor.rs`).
    fn open_with_symbols(
        editor: &Entity<EditorView>,
        window: gpui::WindowHandle<gpui_component::Root>,
        nav_rx: &flume::Receiver<ClientMessage>,
        cx: &mut TestAppContext,
        path: &str,
        symbols: Vec<DocumentSymbolEntry>,
    ) {
        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.begin_open(path.to_owned(), false, window, cx);
                    // 60 blank lines: `set_cursor_position` clamps to the
                    // last valid line, so a single-line buffer would silently
                    // clamp every test cursor move here back to line 0 —
                    // wide enough content lets the fixture symbol ranges
                    // (well under line 60) exercise real line numbers.
                    editor.load(
                        path.to_owned(),
                        "\n".repeat(60),
                        SystemTime::now(),
                        window,
                        cx,
                    );
                });
            })
            .unwrap();

        let ClientMessage::DocumentSymbolRequest { id, .. } = nav_rx
            .try_recv()
            .expect("load() dispatches a DocumentSymbolRequest")
        else {
            panic!("expected a DocumentSymbolRequest");
        };

        window
            .update(cx, |_, _window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.apply_document_symbol_response(id, symbols, cx);
                });
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_refresh_cache_builds_rows_and_tracks_selection_across_cursor_moves(
        cx: &mut TestAppContext,
    ) {
        let (editor, window, nav_rx) = build_test_editor(cx);
        let symbols = vec![
            sym("impl View", 0, (0, 0), (10, 1)),
            sym("fn render", 1, (2, 4), (8, 5)),
        ];
        open_with_symbols(&editor, window, &nav_rx, cx, "a.rs", symbols);

        let panel = cx.update(|cx| cx.new(|cx| OutlinePanel::new(editor.clone(), cx)));

        cx.update(|cx| {
            panel.update(cx, |panel, cx| panel.refresh_cache(cx));
            let rows = &panel.read(cx).row_cache;
            assert_eq!(rows.len(), 2, "one row per document symbol");
            assert_eq!(rows[0].name.as_ref(), "impl View");
            assert_eq!(rows[1].name.as_ref(), "fn render");
        });

        // Move the cursor into the nested `fn render` and confirm the
        // selection follows it to the deeper row (#530 acceptance).
        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.open_at_range(
                        "a.rs".to_owned(),
                        Range {
                            start: Position {
                                line: 3,
                                character: 0,
                            },
                            end: Position {
                                line: 3,
                                character: 0,
                            },
                        },
                        window,
                        cx,
                    );
                });
            })
            .unwrap();

        cx.update(|cx| {
            panel.update(cx, |panel, cx| panel.refresh_cache(cx));
            assert_eq!(
                panel.read(cx).selected_row,
                Some(1),
                "cursor inside `fn render` selects the deeper row"
            );
        });

        // Move the cursor outside every symbol; selection clears.
        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.open_at_range(
                        "a.rs".to_owned(),
                        Range {
                            start: Position {
                                line: 50,
                                character: 0,
                            },
                            end: Position {
                                line: 50,
                                character: 0,
                            },
                        },
                        window,
                        cx,
                    );
                });
            })
            .unwrap();

        cx.update(|cx| {
            panel.update(cx, |panel, cx| panel.refresh_cache(cx));
            assert_eq!(
                panel.read(cx).selected_row,
                None,
                "cursor outside every symbol clears the selection"
            );
        });
    }

    #[gpui::test]
    fn test_observing_the_editor_marks_the_cache_dirty(cx: &mut TestAppContext) {
        let (editor, window, nav_rx) = build_test_editor(cx);
        let panel = cx.update(|cx| cx.new(|cx| OutlinePanel::new(editor.clone(), cx)));

        cx.update(|cx| {
            panel.update(cx, |panel, cx| panel.refresh_cache(cx));
            assert!(!panel.read(cx).cache_dirty);
        });

        let symbols = vec![sym("fn render", 0, (0, 0), (5, 1))];
        open_with_symbols(&editor, window, &nav_rx, cx, "a.rs", symbols);

        cx.update(|cx| {
            assert!(
                panel.read(cx).cache_dirty,
                "a DocumentSymbolResponse landing on the observed editor must mark the cache dirty"
            );
            panel.update(cx, |panel, cx| panel.refresh_cache(cx));
            assert_eq!(panel.read(cx).row_cache.len(), 1);
        });
    }

    #[gpui::test]
    fn test_jump_target_returns_the_open_paths_selection_range(cx: &mut TestAppContext) {
        let (editor, window, nav_rx) = build_test_editor(cx);
        let symbols = vec![sym("fn render", 0, (2, 4), (8, 5))];
        open_with_symbols(&editor, window, &nav_rx, cx, "a.rs", symbols);

        let panel = cx.update(|cx| cx.new(|cx| OutlinePanel::new(editor.clone(), cx)));

        cx.update(|cx| {
            panel.update(cx, |panel, cx| panel.refresh_cache(cx));
            let target = panel.read(cx).jump_target(0, cx);
            assert_eq!(
                target,
                Some((
                    "a.rs".to_owned(),
                    Range {
                        start: Position {
                            line: 2,
                            character: 4
                        },
                        end: Position {
                            line: 8,
                            character: 5
                        },
                    }
                ))
            );
        });
    }

    #[gpui::test]
    fn test_jump_target_is_none_when_no_file_is_open(cx: &mut TestAppContext) {
        let (editor, _window, _nav_rx) = build_test_editor(cx);
        let panel = cx.update(|cx| cx.new(|cx| OutlinePanel::new(editor.clone(), cx)));

        cx.update(|cx| {
            panel.update(cx, |panel, cx| panel.refresh_cache(cx));
            assert_eq!(panel.read(cx).jump_target(0, cx), None);
        });
    }

    #[gpui::test]
    fn test_jump_target_is_none_for_an_out_of_range_index(cx: &mut TestAppContext) {
        let (editor, window, nav_rx) = build_test_editor(cx);
        let symbols = vec![sym("fn render", 0, (2, 4), (8, 5))];
        open_with_symbols(&editor, window, &nav_rx, cx, "a.rs", symbols);

        let panel = cx.update(|cx| cx.new(|cx| OutlinePanel::new(editor.clone(), cx)));

        cx.update(|cx| {
            panel.update(cx, |panel, cx| panel.refresh_cache(cx));
            assert_eq!(panel.read(cx).jump_target(5, cx), None);
        });
    }
}
