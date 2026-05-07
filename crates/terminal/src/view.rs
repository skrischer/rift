use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, Processor};
use gpui::*;
use termy_terminal_ui::{
    add_span_damage_compute_us, encode_mouse_report, find_link_in_line,
    terminal_ui_render_metrics_snapshot, CellRenderInfo, CommandLifecycle, OscEvent,
    OscInterceptor, TerminalCursorStyle, TerminalGrid, TerminalGridPaintCacheHandle,
    TerminalGridPaintDamage, TerminalMouseButton, TerminalMouseEventKind, TerminalMouseMode,
    TerminalMouseModifiers, TerminalMousePosition, TerminalUiRenderMetricsSnapshot,
};

use tracing::{debug, info};

use crate::colors;
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
struct Listener {
    event_tx: flume::Sender<Event>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if matches!(event, Event::ClipboardStore(..)) {
            let _ = self.event_tx.try_send(event);
        }
    }
}

pub struct TerminalHandle {
    pub pty_tx: flume::Sender<Vec<u8>>,
    pub input_rx: flume::Receiver<Vec<u8>>,
    pub size_changed_rx: flume::Receiver<TermSize>,
}

#[derive(Clone, Debug, PartialEq)]
struct GridSelection {
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
}

impl GridSelection {
    fn normalize(&self) -> (usize, usize, usize, usize) {
        if self.start_row < self.end_row
            || (self.start_row == self.end_row && self.start_col <= self.end_col)
        {
            (self.start_row, self.start_col, self.end_row, self.end_col)
        } else {
            (self.end_row, self.end_col, self.start_row, self.start_col)
        }
    }

    fn contains(&self, row: usize, col: usize) -> bool {
        let (sr, sc, er, ec) = self.normalize();

        if row < sr || row > er {
            return false;
        }

        if sr == er {
            return col >= sc && col <= ec;
        }

        if row == sr {
            return col >= sc;
        }

        if row == er {
            return col <= ec;
        }

        true
    }
}

pub struct TerminalView {
    terminal: Arc<Mutex<Term<Listener>>>,
    input_tx: flume::Sender<Vec<u8>>,
    size_changed_tx: flume::Sender<TermSize>,
    focus_handle: FocusHandle,
    cell_size: Size<Pixels>,
    grid_size: TermSize,
    selection: Option<GridSelection>,
    selecting: bool,
    cursor_blink_visible: bool,
    paint_cache: TerminalGridPaintCacheHandle,
    working_directory: Option<String>,
    command_lifecycle: CommandLifecycle,
    mouse_mode_active: bool,
    hovered_link: Option<HoveredLink>,
    prev_selection: Option<GridSelection>,
    ssh_label: SharedString,
}

struct HoveredLink {
    row: usize,
    start_col: usize,
    end_col: usize,
    target: String,
}

impl TerminalView {
    pub fn new(cx: &mut Context<Self>) -> (Self, TerminalHandle) {
        let grid_size = TermSize { cols: 80, rows: 24 };
        info!(
            cols = grid_size.cols,
            rows = grid_size.rows,
            "terminal view created"
        );
        let config = Config::default();
        let (term_event_tx, term_event_rx) = flume::unbounded();
        let listener = Listener {
            event_tx: term_event_tx,
        };
        let terminal = Arc::new(Mutex::new(Term::new(config, &grid_size, listener)));
        let parser: Arc<Mutex<Processor>> = Arc::new(Mutex::new(Processor::new()));

        let (pty_tx, pty_rx) = flume::unbounded::<Vec<u8>>();
        let (input_tx, input_rx) = flume::unbounded();
        let (size_changed_tx, size_changed_rx) = flume::unbounded();

        {
            let terminal = terminal.clone();
            let parser = parser.clone();
            cx.spawn(async move |this, cx| {
                let mut osc = OscInterceptor::new();
                debug!("PTY read loop started");
                loop {
                    let Ok(data) = pty_rx.recv_async().await else {
                        debug!("PTY stream closed");
                        break;
                    };
                    let mut all_osc_events: Vec<OscEvent> = Vec::new();
                    {
                        let mut term = terminal.lock().expect("term lock poisoned");
                        let mut p = parser.lock().expect("parser lock poisoned");
                        let (filtered, events) = osc.process(&data);
                        all_osc_events.extend(events);
                        if !filtered.is_empty() {
                            p.advance(&mut *term, &filtered);
                        }
                        while let Ok(more) = pty_rx.try_recv() {
                            let (filtered, events) = osc.process(&more);
                            all_osc_events.extend(events);
                            if !filtered.is_empty() {
                                p.advance(&mut *term, &filtered);
                            }
                        }
                    }
                    let mut term_events: Vec<Event> = Vec::new();
                    while let Ok(event) = term_event_rx.try_recv() {
                        term_events.push(event);
                    }
                    let result = cx.update(|cx| {
                        this.update(cx, |view, cx| {
                            for event in all_osc_events {
                                view.handle_osc_event(event);
                            }
                            for event in term_events {
                                view.handle_term_event(event, cx);
                            }
                            cx.notify();
                        })
                    });
                    if result.is_err() {
                        break;
                    }
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        }

        {
            cx.spawn(async move |this, cx| loop {
                smol::Timer::after(Duration::from_millis(500)).await;
                let result = cx.update(|cx| {
                    this.update(cx, |view, cx| {
                        let term = view.terminal.lock().expect("term lock poisoned");
                        let blinking = term.cursor_style().blinking;
                        drop(term);
                        if blinking {
                            view.cursor_blink_visible = !view.cursor_blink_visible;
                            cx.notify();
                        } else if !view.cursor_blink_visible {
                            view.cursor_blink_visible = true;
                            cx.notify();
                        }
                    })
                });
                if result.is_err() {
                    break;
                }
            })
            .detach();
        }

        let ssh_user = std::env::var("RIFT_SSH_USER").unwrap_or_default();
        let ssh_host = std::env::var("RIFT_SSH_HOST").unwrap_or_else(|_| "localhost".into());
        let ssh_label: SharedString = if ssh_user.is_empty() {
            ssh_host.into()
        } else {
            format!("{}@{}", ssh_user, ssh_host).into()
        };

        let view = Self {
            terminal,
            input_tx,
            size_changed_tx,
            focus_handle: cx.focus_handle(),
            cell_size: size(px(0.0), px(0.0)),
            grid_size,
            selection: None,
            selecting: false,
            cursor_blink_visible: true,
            paint_cache: TerminalGridPaintCacheHandle::default(),
            working_directory: None,
            command_lifecycle: CommandLifecycle::default(),
            mouse_mode_active: false,
            hovered_link: None,
            prev_selection: None,
            ssh_label,
        };

        let handle = TerminalHandle {
            pty_tx,
            input_rx,
            size_changed_rx,
        };

        (view, handle)
    }

    fn pixel_to_grid(&self, pos: Point<Pixels>) -> (usize, usize) {
        if self.cell_size.width <= px(0.0) || self.cell_size.height <= px(0.0) {
            return (0, 0);
        }

        let col = (pos.x / self.cell_size.width).floor().max(0.0) as usize;
        let row = (pos.y / self.cell_size.height).floor().max(0.0) as usize;
        (
            col.min(self.grid_size.cols.saturating_sub(1)),
            row.min(self.grid_size.rows.saturating_sub(1)),
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
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
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

    pub fn working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }

    pub fn command_lifecycle(&self) -> &CommandLifecycle {
        &self.command_lifecycle
    }

    pub fn render_metrics(&self) -> TerminalUiRenderMetricsSnapshot {
        terminal_ui_render_metrics_snapshot()
    }

    fn handle_osc_event(&mut self, event: OscEvent) {
        match event {
            OscEvent::WorkingDirectory(path) => {
                debug!(%path, "OSC 7: working directory changed");
                self.working_directory = Some(path);
            }
            OscEvent::ShellPromptStart => {
                debug!("OSC 133;A: shell prompt start");
                self.command_lifecycle.prompt_start();
            }
            OscEvent::ShellCommandStart => {
                debug!("OSC 133;B: command input start");
                self.command_lifecycle.command_start();
            }
            OscEvent::ShellCommandExecuting => {
                debug!("OSC 133;C: command executing");
                self.command_lifecycle.command_executing();
            }
            OscEvent::ShellCommandFinished(code) => {
                debug!(?code, "OSC 133;D: command finished");
                self.command_lifecycle.command_finished(code);
            }
            _ => {}
        }
    }

    fn handle_term_event(&mut self, event: Event, cx: &mut Context<Self>) {
        if let Event::ClipboardStore(_, text) = event {
            debug!(len = text.len(), "OSC 52: clipboard store");
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn detect_link_at(&self, row: usize, col: usize) -> Option<HoveredLink> {
        let term = self.terminal.lock().expect("term lock poisoned");
        let grid = term.grid();
        let display_offset = grid.display_offset();
        let line = Line(row as i32 - display_offset as i32);
        let columns = grid.columns();

        // OSC 8: check if cell has an explicit hyperlink
        if col < columns {
            if let Some(hyperlink) = grid[line][Column(col)].hyperlink() {
                let uri = hyperlink.uri().to_owned();
                let mut start = col;
                let mut end = col;
                while start > 0 {
                    if let Some(h) = grid[line][Column(start - 1)].hyperlink() {
                        if h.uri() == hyperlink.uri() {
                            start -= 1;
                            continue;
                        }
                    }
                    break;
                }
                while end + 1 < columns {
                    if let Some(h) = grid[line][Column(end + 1)].hyperlink() {
                        if h.uri() == hyperlink.uri() {
                            end += 1;
                            continue;
                        }
                    }
                    break;
                }
                return Some(HoveredLink {
                    row,
                    start_col: start,
                    end_col: end,
                    target: uri,
                });
            }
        }

        // Regex fallback: extract row text and use find_link_in_line
        let chars: Vec<char> = (0..columns)
            .map(|c| {
                let cell = &grid[line][Column(c)];
                if cell.c == '\0' {
                    ' '
                } else {
                    cell.c
                }
            })
            .collect();

        find_link_in_line(&chars, col).map(|detected| HoveredLink {
            row,
            start_col: detected.start_col,
            end_col: detected.end_col,
            target: detected.target,
        })
    }

    fn open_link(url: &str) {
        debug!(%url, "opening link");
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", url])
                .spawn();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(url).spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(url).spawn();
        }
    }

    fn mouse_mode_from_term(mode: TermMode) -> TerminalMouseMode {
        TerminalMouseMode {
            enabled: mode.intersects(
                TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
            ),
            report_click: mode.contains(TermMode::MOUSE_REPORT_CLICK),
            report_drag: mode.contains(TermMode::MOUSE_DRAG),
            report_motion: mode.contains(TermMode::MOUSE_MOTION),
            sgr_encoding: mode.contains(TermMode::SGR_MOUSE),
            utf8_encoding: mode.contains(TermMode::UTF8_MOUSE),
        }
    }

    fn try_forward_mouse(
        &self,
        event_kind: TerminalMouseEventKind,
        position: Point<Pixels>,
        modifiers: &Modifiers,
    ) -> bool {
        let mode = {
            let term = self.terminal.lock().expect("term lock poisoned");
            *term.mode()
        };
        let mouse_mode = Self::mouse_mode_from_term(mode);
        if !mouse_mode.enabled {
            return false;
        }
        let (col, row) = self.pixel_to_grid(position);
        let mods = TerminalMouseModifiers {
            shift: modifiers.shift,
            alt: modifiers.alt,
            control: modifiers.control,
        };
        if let Some(bytes) = encode_mouse_report(
            mouse_mode,
            event_kind,
            TerminalMousePosition { col, row },
            mods,
        ) {
            let _ = self.input_tx.try_send(bytes);
        }
        true
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn measure_cell_size(window: &mut Window, font_size: Pixels) -> Size<Pixels> {
    let text_system = window.text_system();
    let font = Font {
        family: "JetBrainsMono Nerd Font Mono".into(),
        features: FontFeatures::default(),
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
    size(cell_width, line_height)
}

fn extract_row_cells(
    term: &Term<Listener>,
    row: usize,
    display_offset: usize,
    selection: Option<&GridSelection>,
) -> Vec<CellRenderInfo> {
    let grid = term.grid();
    let columns = grid.columns();
    let line = Line(row as i32 - display_offset as i32);
    let default_bg = Hsla::from(colors::BACKGROUND);
    let mut cells = Vec::with_capacity(columns);

    for col in 0..columns {
        let cell = &grid[line][Column(col)];
        let flags = cell.flags;

        if flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }

        let inverse = flags.contains(Flags::INVERSE);
        let raw_fg = Hsla::from(colors::to_gpui_color(cell.fg));
        let raw_bg = Hsla::from(colors::to_gpui_color(cell.bg));

        let (mut fg, bg) = if inverse {
            (raw_bg, raw_fg)
        } else {
            (raw_fg, raw_bg)
        };

        if flags.contains(Flags::DIM) {
            fg.a *= 0.5;
        }

        let ch = if cell.c == '\0' { ' ' } else { cell.c };
        let selected = selection.is_some_and(|sel| sel.contains(row, col));

        cells.push(CellRenderInfo {
            col,
            row,
            char: ch,
            fg,
            bg,
            uses_terminal_default_bg: !inverse && bg == default_bg,
            bold: flags.contains(Flags::BOLD),
            render_text: true,
            selected,
            search_current: false,
            search_match: false,
        });
    }

    cells
}

fn map_damage(term: &mut Term<Listener>) -> TerminalGridPaintDamage {
    let display_offset = term.grid().display_offset();
    let damage = match term.damage() {
        TermDamage::Full => {
            term.reset_damage();
            return TerminalGridPaintDamage::Full;
        }
        TermDamage::Partial(iter) => {
            let mut iter = iter.peekable();
            if display_offset != 0 {
                let has_damage = iter.peek().is_some();
                term.reset_damage();
                return if has_damage {
                    TerminalGridPaintDamage::Full
                } else {
                    TerminalGridPaintDamage::None
                };
            }
            let ranges: Vec<(usize, usize, usize)> = iter
                .map(|bounds| (bounds.line, bounds.left, bounds.right))
                .collect();
            if ranges.is_empty() {
                TerminalGridPaintDamage::None
            } else {
                TerminalGridPaintDamage::RowRanges(ranges.into())
            }
        }
    };
    term.reset_damage();
    damage
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_size = px(14.0);
        let cell_size = measure_cell_size(window, font_size);
        self.cell_size = cell_size;

        let statusbar_h = px(28.0);
        let viewport = window.viewport_size();
        let cols = (viewport.width / cell_size.width).floor() as usize;
        let rows = ((viewport.height - statusbar_h) / cell_size.height).floor() as usize;
        let new_size = TermSize {
            cols: cols.max(1),
            rows: rows.max(1),
        };

        let mut term = self.terminal.lock().expect("term lock poisoned");

        if new_size != self.grid_size {
            debug!(
                cols = new_size.cols,
                rows = new_size.rows,
                "terminal resized"
            );
            self.grid_size = new_size;
            term.resize(new_size);
            let _ = self.size_changed_tx.try_send(new_size);
            self.paint_cache.clear();
        }

        let damage_start = std::time::Instant::now();
        let mut paint_damage = map_damage(&mut term);
        add_span_damage_compute_us(damage_start.elapsed().as_micros() as u64);

        if self.selection != self.prev_selection {
            paint_damage = TerminalGridPaintDamage::Full;
            self.prev_selection = self.selection.clone();
        }

        let mode = *term.mode();

        let cursor_point = term.grid().cursor.point;
        let cursor_row = cursor_point.line.0 as usize;
        let cursor_col = cursor_point.column.0;
        let cursor_style = term.cursor_style();
        let cursor_shape = if mode.contains(TermMode::SHOW_CURSOR) {
            cursor_style.shape
        } else {
            CursorShape::Hidden
        };
        let show_cursor = cursor_shape != CursorShape::Hidden
            && (self.cursor_blink_visible || !cursor_style.blinking);
        let mouse_now = mode.intersects(
            TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
        );
        if mouse_now != self.mouse_mode_active {
            debug!(enabled = mouse_now, "mouse mode changed");
            self.mouse_mode_active = mouse_now;
        }

        let display_offset = term.grid().display_offset();

        let mut grid_rows: Vec<Arc<Vec<CellRenderInfo>>> = Vec::with_capacity(new_size.rows);
        for row_idx in 0..new_size.rows {
            let row_cells =
                extract_row_cells(&term, row_idx, display_offset, self.selection.as_ref());
            grid_rows.push(Arc::new(row_cells));
        }

        let cursor_cell = if show_cursor && cursor_row < new_size.rows {
            Some((cursor_col, cursor_row))
        } else {
            None
        };

        let termy_cursor_style = match cursor_shape {
            CursorShape::Block | CursorShape::HollowBlock => TerminalCursorStyle::Block,
            _ => TerminalCursorStyle::Line,
        };

        drop(term);

        let bg_hsla = Hsla::from(colors::BACKGROUND);
        let fg_hsla = Hsla::from(colors::FOREGROUND);
        let selection_bg = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.35,
            a: 1.0,
        };

        let grid = TerminalGrid {
            cells: Arc::new(grid_rows),
            paint_cache: self.paint_cache.clone(),
            paint_damage,
            cell_size,
            cols: new_size.cols,
            rows: new_size.rows,
            clear_bg: bg_hsla,
            terminal_surface_bg: bg_hsla,
            cursor_color: fg_hsla,
            selection_bg,
            selection_fg: fg_hsla,
            search_match_bg: selection_bg,
            search_current_bg: selection_bg,
            hovered_link_range: self
                .hovered_link
                .as_ref()
                .map(|l| (l.row, l.start_col, l.end_col)),
            cursor_cell,
            cursor_visible: show_cursor,
            font_family: "JetBrainsMono Nerd Font Mono".into(),
            font_size,
            cursor_style: termy_cursor_style,
        };

        let mut terminal_area = div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus_handle(cx))
            .flex_1()
            .bg(bg_hsla);

        if self.hovered_link.is_some() {
            terminal_area = terminal_area.cursor(gpui::CursorStyle::PointingHand);
        }

        let terminal_area = terminal_area
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
                    this.cursor_blink_visible = true;
                    let _ = this.input_tx.try_send(bytes);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    if event.modifiers.control {
                        if let Some(ref link) = this.hovered_link {
                            Self::open_link(&link.target);
                            this.hovered_link = None;
                            cx.notify();
                            return;
                        }
                    }

                    if !this.try_forward_mouse(
                        TerminalMouseEventKind::Press(TerminalMouseButton::Left),
                        event.position,
                        &event.modifiers,
                    ) {
                        let (col, row) = this.pixel_to_grid(event.position);
                        this.selection = Some(GridSelection {
                            start_row: row,
                            start_col: col,
                            end_row: row,
                            end_col: col,
                        });
                        this.selecting = true;
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                let mode = {
                    let term = this.terminal.lock().expect("term lock poisoned");
                    *term.mode()
                };
                let mouse_mode = Self::mouse_mode_from_term(mode);

                if mouse_mode.enabled {
                    let button = match event.pressed_button {
                        Some(MouseButton::Left) => {
                            Some(TerminalMouseEventKind::Drag(TerminalMouseButton::Left))
                        }
                        Some(MouseButton::Right) => {
                            Some(TerminalMouseEventKind::Drag(TerminalMouseButton::Right))
                        }
                        Some(MouseButton::Middle) => {
                            Some(TerminalMouseEventKind::Drag(TerminalMouseButton::Middle))
                        }
                        None if mouse_mode.report_motion => Some(TerminalMouseEventKind::Move),
                        _ => None,
                    };
                    if let Some(event_kind) = button {
                        let (col, row) = this.pixel_to_grid(event.position);
                        let modifiers = TerminalMouseModifiers {
                            shift: event.modifiers.shift,
                            alt: event.modifiers.alt,
                            control: event.modifiers.control,
                        };
                        if let Some(bytes) = encode_mouse_report(
                            mouse_mode,
                            event_kind,
                            TerminalMousePosition { col, row },
                            modifiers,
                        ) {
                            let _ = this.input_tx.try_send(bytes);
                        }
                    }
                } else if this.selecting && event.pressed_button == Some(MouseButton::Left) {
                    let (col, row) = this.pixel_to_grid(event.position);
                    if let Some(ref mut sel) = this.selection {
                        sel.end_row = row;
                        sel.end_col = col;
                    }
                    cx.notify();
                }

                // Link detection on Ctrl+hover
                if event.modifiers.control && event.pressed_button.is_none() {
                    let (col, row) = this.pixel_to_grid(event.position);
                    let new_link = this.detect_link_at(row, col);
                    if this
                        .hovered_link
                        .as_ref()
                        .map(|l| (&l.target, l.row, l.start_col))
                        != new_link.as_ref().map(|l| (&l.target, l.row, l.start_col))
                    {
                        if let Some(ref link) = new_link {
                            debug!(
                                url = %link.target,
                                row = link.row,
                                cols = ?(link.start_col..=link.end_col),
                                "link detected"
                            );
                        }
                        this.hovered_link = new_link;
                        cx.notify();
                    }
                } else if this.hovered_link.is_some() {
                    this.hovered_link = None;
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    if !this.try_forward_mouse(
                        TerminalMouseEventKind::Release(TerminalMouseButton::Left),
                        event.position,
                        &event.modifiers,
                    ) {
                        this.selecting = false;
                        if let Some(ref sel) = this.selection {
                            if sel.start_row == sel.end_row && sel.start_col == sel.end_col {
                                this.selection = None;
                            }
                        }
                        if let Some(text) = this.selected_text() {
                            #[cfg(target_os = "linux")]
                            cx.write_to_primary(ClipboardItem::new_string(text));
                            #[cfg(not(target_os = "linux"))]
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    if !this.try_forward_mouse(
                        TerminalMouseEventKind::Press(TerminalMouseButton::Right),
                        event.position,
                        &event.modifiers,
                    ) {
                        if let Some(item) = cx.read_from_clipboard() {
                            if let Some(text) = item.text() {
                                let _ = this.input_tx.try_send(text.as_bytes().to_vec());
                            }
                        }
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
                    this.try_forward_mouse(
                        TerminalMouseEventKind::Release(TerminalMouseButton::Right),
                        event.position,
                        &event.modifiers,
                    );
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    if !this.try_forward_mouse(
                        TerminalMouseEventKind::Press(TerminalMouseButton::Middle),
                        event.position,
                        &event.modifiers,
                    ) {
                        if let Some(item) = cx.read_from_clipboard() {
                            if let Some(text) = item.text() {
                                let _ = this.input_tx.try_send(text.as_bytes().to_vec());
                            }
                        }
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseUpEvent, _window, _cx| {
                    this.try_forward_mouse(
                        TerminalMouseEventKind::Release(TerminalMouseButton::Middle),
                        event.position,
                        &event.modifiers,
                    );
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                let mode = {
                    let term = this.terminal.lock().expect("term lock poisoned");
                    *term.mode()
                };
                let mouse_mode = Self::mouse_mode_from_term(mode);

                if mouse_mode.enabled {
                    let (col, row) = this.pixel_to_grid(event.position);
                    let modifiers = TerminalMouseModifiers {
                        shift: event.modifiers.shift,
                        alt: event.modifiers.alt,
                        control: event.modifiers.control,
                    };
                    let lines = match event.delta {
                        ScrollDelta::Lines(lines) => lines.y.round() as i32,
                        ScrollDelta::Pixels(pixels) => {
                            let lh: f32 = this.cell_size.height.into();
                            if lh > 0.0 {
                                (pixels.y / px(lh)).round() as i32
                            } else {
                                0
                            }
                        }
                    };
                    let event_kind = if lines > 0 {
                        TerminalMouseEventKind::WheelUp
                    } else {
                        TerminalMouseEventKind::WheelDown
                    };
                    for _ in 0..lines.unsigned_abs() {
                        if let Some(bytes) = encode_mouse_report(
                            mouse_mode,
                            event_kind,
                            TerminalMousePosition { col, row },
                            modifiers,
                        ) {
                            let _ = this.input_tx.try_send(bytes);
                        }
                    }
                } else {
                    let delta = match event.delta {
                        ScrollDelta::Lines(lines) => lines.y.round() as i32,
                        ScrollDelta::Pixels(pixels) => {
                            let lh: f32 = this.cell_size.height.into();
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
                }
            }))
            .child(grid);

        let cwd = self.working_directory.clone().unwrap_or_default();
        let size_label = format!("{}x{}", new_size.cols, new_size.rows);
        let statusbar_bg = Hsla::from(colors::SURFACE0);
        let statusbar_border = Hsla::from(colors::SURFACE1);
        let statusbar_fg = Hsla::from(colors::SUBTEXT0);

        let statusbar = div()
            .id("statusbar")
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .w_full()
            .h(statusbar_h)
            .bg(statusbar_bg)
            .border_t_1()
            .border_color(statusbar_border)
            .text_size(font_size)
            .text_color(statusbar_fg)
            .font_family("JetBrainsMono Nerd Font Mono")
            .px(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(16.0))
                    .child(self.ssh_label.clone())
                    .children((!cwd.is_empty()).then(|| SharedString::from(cwd.clone()))),
            )
            .child(div().child(SharedString::from(size_label)));

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(bg_hsla)
            .child(terminal_area)
            .child(statusbar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn test_grid_selection_contains_single_row() {
        let sel = GridSelection {
            start_row: 5,
            start_col: 3,
            end_row: 5,
            end_col: 10,
        };

        assert!(!sel.contains(5, 2));
        assert!(sel.contains(5, 3));
        assert!(sel.contains(5, 7));
        assert!(sel.contains(5, 10));
        assert!(!sel.contains(5, 11));
        assert!(!sel.contains(4, 5));
        assert!(!sel.contains(6, 5));
    }

    #[::core::prelude::v1::test]
    fn test_grid_selection_contains_multi_row() {
        let sel = GridSelection {
            start_row: 2,
            start_col: 5,
            end_row: 4,
            end_col: 8,
        };

        assert!(!sel.contains(2, 4));
        assert!(sel.contains(2, 5));
        assert!(sel.contains(2, 79));
        assert!(sel.contains(3, 0));
        assert!(sel.contains(3, 40));
        assert!(sel.contains(3, 79));
        assert!(sel.contains(4, 0));
        assert!(sel.contains(4, 8));
        assert!(!sel.contains(4, 9));
        assert!(!sel.contains(1, 5));
        assert!(!sel.contains(5, 0));
    }

    #[::core::prelude::v1::test]
    fn test_grid_selection_contains_backward() {
        let sel = GridSelection {
            start_row: 4,
            start_col: 8,
            end_row: 2,
            end_col: 5,
        };

        assert!(!sel.contains(2, 4));
        assert!(sel.contains(2, 5));
        assert!(sel.contains(3, 0));
        assert!(sel.contains(4, 8));
        assert!(!sel.contains(4, 9));
    }

    #[::core::prelude::v1::test]
    fn test_grid_selection_normalize() {
        let forward = GridSelection {
            start_row: 2,
            start_col: 5,
            end_row: 4,
            end_col: 8,
        };
        assert_eq!(forward.normalize(), (2, 5, 4, 8));

        let backward = GridSelection {
            start_row: 4,
            start_col: 8,
            end_row: 2,
            end_col: 5,
        };
        assert_eq!(backward.normalize(), (2, 5, 4, 8));

        let same_row = GridSelection {
            start_row: 3,
            start_col: 10,
            end_row: 3,
            end_col: 2,
        };
        assert_eq!(same_row.normalize(), (3, 2, 3, 10));
    }
}
