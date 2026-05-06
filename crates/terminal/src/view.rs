use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{CursorShape, Processor};
use gpui::*;

use crate::colors;
use crate::grid::{self, DamageSnapshot, GridSelection};
use crate::keyboard;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TermSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

#[derive(Clone)]
struct Listener;

impl EventListener for Listener {
    fn send_event(&self, _event: Event) {}
}

pub struct TerminalHandle {
    pub pty_tx: flume::Sender<Vec<u8>>,
    pub input_rx: flume::Receiver<Vec<u8>>,
    pub size_changed_rx: flume::Receiver<TermSize>,
}

struct CachedBgSpan {
    x: Pixels,
    y: Pixels,
    width: Pixels,
    height: Pixels,
    color: Hsla,
}

struct CachedRow {
    bg_spans: Vec<CachedBgSpan>,
    shaped_line: ShapedLine,
    origin: Point<Pixels>,
}

struct RowCacheState {
    entries: Vec<Option<CachedRow>>,
    grid_cols: usize,
    grid_rows: usize,
    cell_size: Size<Pixels>,
    grid_origin: Point<Pixels>,
    prev_cursor_visible: bool,
    prev_cursor_row: usize,
    prev_selection: Option<GridSelection>,
    prev_display_offset: usize,
}

impl RowCacheState {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            grid_cols: 0,
            grid_rows: 0,
            cell_size: size(px(0.0), px(0.0)),
            grid_origin: point(px(0.0), px(0.0)),
            prev_cursor_visible: true,
            prev_cursor_row: 0,
            prev_selection: None,
            prev_display_offset: 0,
        }
    }
}

pub struct TerminalView {
    terminal: Arc<Mutex<Term<Listener>>>,
    parser: Arc<Mutex<Processor>>,
    pty_rx: flume::Receiver<Vec<u8>>,
    input_tx: flume::Sender<Vec<u8>>,
    size_changed_tx: flume::Sender<TermSize>,
    focus_handle: FocusHandle,
    cell_size: Size<Pixels>,
    grid_size: TermSize,
    selection: Option<GridSelection>,
    selecting: bool,
    blink_epoch: Instant,
    row_cache: Arc<Mutex<RowCacheState>>,
}

impl TerminalView {
    pub fn new(cx: &mut Context<Self>) -> (Self, TerminalHandle) {
        let grid_size = TermSize { cols: 80, rows: 24 };
        let config = Config::default();
        let terminal = Term::new(config, &grid_size, Listener);

        let (pty_tx, pty_rx) = flume::unbounded();
        let (input_tx, input_rx) = flume::unbounded();
        let (size_changed_tx, size_changed_rx) = flume::unbounded();

        let view = Self {
            terminal: Arc::new(Mutex::new(terminal)),
            parser: Arc::new(Mutex::new(Processor::new())),
            pty_rx,
            input_tx,
            size_changed_tx,
            focus_handle: cx.focus_handle(),
            cell_size: size(px(0.0), px(0.0)),
            grid_size,
            selection: None,
            selecting: false,
            blink_epoch: Instant::now(),
            row_cache: Arc::new(Mutex::new(RowCacheState::new())),
        };

        let handle = TerminalHandle {
            pty_tx,
            input_rx,
            size_changed_rx,
        };

        (view, handle)
    }

    fn process_incoming(&mut self) {
        while let Ok(data) = self.pty_rx.try_recv() {
            let mut term = self.terminal.lock().expect("term lock poisoned");
            let mut parser = self.parser.lock().expect("parser lock poisoned");
            let offset_before = term.grid().display_offset();
            parser.advance(&mut *term, &data);
            if offset_before > 0 && term.grid().display_offset() == 0 {
                term.scroll_display(alacritty_terminal::grid::Scroll::Delta(
                    offset_before as i32,
                ));
            }
        }
    }

    fn pixel_to_grid(&self, pos: Point<Pixels>) -> (usize, usize) {
        let cache = self.row_cache.lock().expect("cache lock poisoned");
        let cell_size = cache.cell_size;
        let origin = cache.grid_origin;
        let grid_cols = cache.grid_cols;
        let grid_rows = cache.grid_rows;
        drop(cache);

        if cell_size.width <= px(0.0) || cell_size.height <= px(0.0) {
            return (0, 0);
        }

        let local_x = pos.x - origin.x;
        let local_y = pos.y - origin.y;
        let col = (local_x / cell_size.width).floor().max(0.0) as usize;
        let row = (local_y / cell_size.height).floor().max(0.0) as usize;
        (
            col.min(grid_cols.saturating_sub(1)),
            row.min(grid_rows.saturating_sub(1)),
        )
    }

    fn selected_text(&self) -> Option<String> {
        let sel = self.selection.as_ref()?;
        let (sr, sc, er, ec) = sel.normalize();
        let term = self.terminal.lock().expect("term lock poisoned");
        let grid = term.grid();
        let columns = grid.columns();
        let display_offset = grid.display_offset();
        let mut result = String::new();

        for row in sr..=er {
            if row >= grid.screen_lines() {
                break;
            }
            let c0 = if row == sr { sc } else { 0 };
            let c1 = if row == er {
                ec
            } else {
                columns.saturating_sub(1)
            };
            let line_idx = Line(row as i32 - display_offset as i32);
            let mut line = String::new();
            for col in c0..=c1 {
                if col >= columns {
                    break;
                }
                let cell = &grid[line_idx][Column(col)];
                if cell
                    .flags
                    .contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER)
                {
                    continue;
                }
                line.push(if cell.c == '\0' { ' ' } else { cell.c });
            }
            if row < er {
                result.push_str(line.trim_end());
                result.push('\n');
            } else {
                result.push_str(&line);
            }
        }

        let trimmed = result.trim_end().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.process_incoming();

        {
            let cache = self.row_cache.lock().expect("cache lock poisoned");
            self.cell_size = cache.cell_size;
            if cache.grid_cols > 0 && cache.grid_rows > 0 {
                self.grid_size = TermSize {
                    cols: cache.grid_cols,
                    rows: cache.grid_rows,
                };
            }
        }

        let cursor_visible = (self.blink_epoch.elapsed().as_millis() / 500).is_multiple_of(2);

        window.request_animation_frame();

        let terminal = self.terminal.clone();
        let cell_size = self.cell_size;
        let grid_size = self.grid_size;
        let selection = self.selection.clone();
        let size_changed_tx = self.size_changed_tx.clone();
        let row_cache = self.row_cache.clone();

        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus_handle(cx))
            .size_full()
            .bg(RgbaExt::to_hsla(colors::BACKGROUND))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _window, cx| {
                let ks = &event.keystroke;

                if ks.modifiers.control && ks.modifiers.shift && ks.key.as_str() == "c" {
                    if let Some(text) = this.selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    return;
                }

                if ks.modifiers.control && ks.modifiers.shift && ks.key.as_str() == "v" {
                    if let Some(item) = cx.read_from_clipboard() {
                        if let Some(text) = item.text() {
                            let _ = this.input_tx.try_send(text.as_bytes().to_vec());
                        }
                    }
                    return;
                }

                let mode = {
                    let term = this.terminal.lock().expect("term lock poisoned");
                    *term.mode()
                };

                if let Some(bytes) = keyboard::encode_keystroke(ks, mode) {
                    {
                        let mut term = this.terminal.lock().expect("term lock poisoned");
                        if term.grid().display_offset() > 0 {
                            term.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
                        }
                    }
                    this.selection = None;
                    this.blink_epoch = Instant::now();
                    let _ = this.input_tx.try_send(bytes);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    let (col, row) = this.pixel_to_grid(event.position);
                    this.selection = Some(GridSelection {
                        start_row: row,
                        start_col: col,
                        end_row: row,
                        end_col: col,
                    });
                    this.selecting = true;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if this.selecting && event.pressed_button == Some(MouseButton::Left) {
                    let (col, row) = this.pixel_to_grid(event.position);
                    if let Some(ref mut sel) = this.selection {
                        sel.end_row = row;
                        sel.end_col = col;
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.selecting = false;
                    if let Some(ref sel) = this.selection {
                        if sel.start_row == sel.end_row && sel.start_col == sel.end_col {
                            this.selection = None;
                        }
                    }
                    if let Some(text) = this.selected_text() {
                        cx.write_to_primary(ClipboardItem::new_string(text));
                    }
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                let delta = match event.delta {
                    ScrollDelta::Lines(lines) => lines.y.round() as i32,
                    ScrollDelta::Pixels(pixels) => {
                        let lh: f32 = {
                            let cache = this.row_cache.lock().expect("cache lock poisoned");
                            cache.cell_size.height.into()
                        };
                        if lh > 0.0 {
                            (pixels.y / px(lh)).round() as i32
                        } else {
                            0
                        }
                    }
                };
                if delta != 0 {
                    let mut term = this.terminal.lock().expect("term lock poisoned");
                    term.scroll_display(alacritty_terminal::grid::Scroll::Delta(delta));
                    cx.notify();
                }
            }))
            .child(TerminalElement {
                terminal,
                cell_size,
                grid_size,
                selection,
                cursor_visible,
                size_changed_tx,
                row_cache,
            })
    }
}

struct TerminalElement {
    terminal: Arc<Mutex<Term<Listener>>>,
    cell_size: Size<Pixels>,
    grid_size: TermSize,
    selection: Option<GridSelection>,
    cursor_visible: bool,
    size_changed_tx: flume::Sender<TermSize>,
    row_cache: Arc<Mutex<RowCacheState>>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct TerminalPrepaintState {
    bg_rects: Vec<PaintQuad>,
    block_cursor: Option<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    line_cursor: Option<PaintQuad>,
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = TerminalPrepaintState;

    fn id(&self) -> Option<ElementId> {
        Some("terminal-grid".into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let font_size = px(14.0);
        let text_system = window.text_system();
        let font = Font {
            family: "JetBrains Mono".into(),
            features: Default::default(),
            fallbacks: Default::default(),
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
        };
        let font_id = text_system.resolve_font(&font);
        let cell_width = text_system
            .advance(font_id, font_size, 'M')
            .map(|s| s.width)
            .unwrap_or(px(8.4));
        let line_height = font_size * 1.4;
        self.cell_size = size(cell_width, line_height);

        let cols = (bounds.size.width / cell_width).floor() as usize;
        let rows = (bounds.size.height / line_height).floor() as usize;
        let new_size = TermSize {
            cols: cols.max(1),
            rows: rows.max(1),
        };

        let mut term = self.terminal.lock().expect("term lock poisoned");

        if new_size != self.grid_size {
            self.grid_size = new_size;
            term.resize(new_size);
            let _ = self.size_changed_tx.try_send(new_size);
        }

        let damage = grid::take_damage(&mut *term);

        let cursor_point = term.grid().cursor.point;
        let cursor_row = cursor_point.line.0 as usize;
        let cursor_col = cursor_point.column.0;
        let cursor_style = term.cursor_style();
        let cursor_shape = cursor_style.shape;
        let show_cursor =
            cursor_shape != CursorShape::Hidden && (self.cursor_visible || !cursor_style.blinking);

        let display_offset = term.grid().display_offset();

        let mut cache = self.row_cache.lock().expect("cache lock poisoned");

        if cache.grid_cols != new_size.cols || cache.grid_rows != new_size.rows {
            cache.entries.clear();
            cache.entries.resize_with(new_size.rows, || None);
            cache.grid_cols = new_size.cols;
            cache.grid_rows = new_size.rows;
        }

        cache.cell_size = self.cell_size;
        cache.grid_origin = bounds.origin;

        let mut dirty_rows: Vec<usize> = match &damage {
            DamageSnapshot::Full => (0..new_size.rows).collect(),
            DamageSnapshot::Partial(spans) => {
                let mut r: Vec<usize> = spans.iter().map(|s| s.row).collect();
                r.sort_unstable();
                r.dedup();
                r
            }
        };

        if display_offset != cache.prev_display_offset {
            dirty_rows = (0..new_size.rows).collect();
            cache.prev_display_offset = display_offset;
        }

        if self.selection != cache.prev_selection {
            dirty_rows = (0..new_size.rows).collect();
        }

        if self.cursor_visible != cache.prev_cursor_visible {
            let add = |rows: &mut Vec<usize>, r: usize| {
                if !rows.contains(&r) {
                    rows.push(r);
                }
            };
            add(&mut dirty_rows, cursor_row);
            add(&mut dirty_rows, cache.prev_cursor_row);
        }

        if cursor_row != cache.prev_cursor_row {
            let add = |rows: &mut Vec<usize>, r: usize| {
                if !rows.contains(&r) {
                    rows.push(r);
                }
            };
            add(&mut dirty_rows, cursor_row);
            add(&mut dirty_rows, cache.prev_cursor_row);
        }

        cache.prev_cursor_visible = self.cursor_visible;
        cache.prev_cursor_row = cursor_row;
        cache.prev_selection = self.selection.clone();

        for &row_idx in &dirty_rows {
            if row_idx >= new_size.rows {
                continue;
            }

            let cells =
                grid::extract_row_cells(&*term, row_idx, display_offset, self.selection.as_ref());
            let y = bounds.origin.y + line_height * row_idx as f32;
            let mut bg_spans = Vec::new();
            let mut line_text = String::with_capacity(new_size.cols);
            let mut runs: Vec<TextRun> = Vec::new();

            for cell in &cells {
                if cell.wide_spacer {
                    continue;
                }

                let at_block_cursor = row_idx == cursor_row
                    && cell.col == cursor_col
                    && cursor_shape == CursorShape::Block
                    && show_cursor;

                let (fg, bg) = if at_block_cursor {
                    (colors::BACKGROUND, colors::FOREGROUND)
                } else if cell.selected {
                    (
                        cell.fg,
                        gpui::Rgba {
                            r: 88.0 / 255.0,
                            g: 91.0 / 255.0,
                            b: 112.0 / 255.0,
                            a: 1.0,
                        },
                    )
                } else {
                    (cell.fg, cell.bg)
                };

                let fg_hsla = RgbaExt::to_hsla(fg);
                let bg_hsla = RgbaExt::to_hsla(bg);

                let bg_is_default = !at_block_cursor && !cell.selected && bg == colors::BACKGROUND;
                if !bg_is_default {
                    let x = bounds.origin.x + cell_width * cell.col as f32;
                    let w = if cell.wide {
                        cell_width * 2.0
                    } else {
                        cell_width
                    };
                    bg_spans.push(CachedBgSpan {
                        x,
                        y,
                        width: w,
                        height: line_height,
                        color: bg_hsla,
                    });
                }

                line_text.push(cell.ch);
                let char_len = cell.ch.len_utf8();

                let weight = if cell.bold {
                    FontWeight::BOLD
                } else {
                    FontWeight::NORMAL
                };
                let fstyle = if cell.italic {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                };

                let run = TextRun {
                    len: char_len,
                    font: Font {
                        family: font.family.clone(),
                        features: Default::default(),
                        fallbacks: Default::default(),
                        weight,
                        style: fstyle,
                    },
                    color: fg_hsla,
                    background_color: None,
                    underline: if cell.underline {
                        Some(UnderlineStyle {
                            color: Some(fg_hsla),
                            thickness: px(1.0),
                            wavy: false,
                        })
                    } else {
                        None
                    },
                    strikethrough: if cell.strikethrough {
                        Some(StrikethroughStyle {
                            color: Some(fg_hsla),
                            thickness: px(1.0),
                        })
                    } else {
                        None
                    },
                };

                if let Some(last) = runs.last_mut() {
                    if last.font == run.font
                        && last.color == run.color
                        && last.underline == run.underline
                        && last.strikethrough == run.strikethrough
                    {
                        last.len += char_len;
                    } else {
                        runs.push(run);
                    }
                } else {
                    runs.push(run);
                }
            }

            let origin = point(bounds.origin.x, y);
            let shaped = if !line_text.is_empty() && !runs.is_empty() {
                text_system.shape_line(line_text.into(), font_size, &runs, None)
            } else {
                text_system.shape_line(
                    " ".into(),
                    font_size,
                    &[TextRun {
                        len: 1,
                        font: font.clone(),
                        color: RgbaExt::to_hsla(colors::FOREGROUND),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                )
            };

            cache.entries[row_idx] = Some(CachedRow {
                bg_spans,
                shaped_line: shaped,
                origin,
            });
        }

        let mut bg_rects = Vec::new();
        let mut lines = Vec::with_capacity(new_size.rows);

        for row_idx in 0..new_size.rows {
            if let Some(cached) = &cache.entries[row_idx] {
                for span in &cached.bg_spans {
                    bg_rects.push(fill(
                        Bounds::new(point(span.x, span.y), size(span.width, span.height)),
                        span.color,
                    ));
                }
                lines.push((cached.origin, cached.shaped_line.clone()));
            }
        }

        let (block_cursor, line_cursor) = if show_cursor && cursor_row < new_size.rows {
            let cx = bounds.origin.x + cell_width * cursor_col as f32;
            let cy = bounds.origin.y + line_height * cursor_row as f32;
            let cursor_color = RgbaExt::to_hsla(colors::FOREGROUND);

            match cursor_shape {
                CursorShape::Block => (
                    Some(fill(
                        Bounds::new(point(cx, cy), size(cell_width, line_height)),
                        cursor_color,
                    )),
                    None,
                ),
                CursorShape::Underline => (
                    None,
                    Some(fill(
                        Bounds::new(
                            point(cx, cy + line_height - px(2.0)),
                            size(cell_width, px(2.0)),
                        ),
                        cursor_color,
                    )),
                ),
                CursorShape::Beam => (
                    None,
                    Some(fill(
                        Bounds::new(point(cx, cy), size(px(2.0), line_height)),
                        cursor_color,
                    )),
                ),
                CursorShape::HollowBlock => (
                    None,
                    Some(outline(
                        Bounds::new(point(cx, cy), size(cell_width, line_height)),
                        cursor_color,
                        BorderStyle::default(),
                    )),
                ),
                CursorShape::Hidden => (None, None),
            }
        } else {
            (None, None)
        };

        drop(cache);
        drop(term);

        TerminalPrepaintState {
            bg_rects,
            block_cursor,
            lines,
            line_cursor,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for quad in prepaint.bg_rects.drain(..) {
            window.paint_quad(quad);
        }

        if let Some(cursor) = prepaint.block_cursor.take() {
            window.paint_quad(cursor);
        }

        for (origin, line) in prepaint.lines.drain(..) {
            let _ = line.paint(origin, self.cell_size.height, window, cx);
        }

        if let Some(cursor) = prepaint.line_cursor.take() {
            window.paint_quad(cursor);
        }
    }
}

struct RgbaExt;

impl RgbaExt {
    fn to_hsla(rgba: gpui::Rgba) -> gpui::Hsla {
        gpui::rgba(
            ((rgba.r * 255.0) as u32) << 24
                | ((rgba.g * 255.0) as u32) << 16
                | ((rgba.b * 255.0) as u32) << 8
                | ((rgba.a * 255.0) as u32),
        )
        .into()
    }
}
