use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, Processor};
use gpui::*;
use gpui_component::dialog::AlertDialog;
use gpui_component::notification::Notification;
use gpui_component::WindowExt;
use termy_terminal_ui::{
    add_span_damage_compute_us, encode_mouse_report, find_link_in_line, CellRenderInfo,
    CommandLifecycle, CommandPhase, OscEvent, OscInterceptor, TerminalCursorStyle, TerminalGrid,
    TerminalGridPaintCacheHandle, TerminalGridPaintDamage, TerminalMouseButton,
    TerminalMouseEventKind, TerminalMouseMode, TerminalMouseModifiers, TerminalMousePosition,
};
use tracing::{debug, error};

use crate::colors;
use crate::error::TerminalError;
use crate::keyboard;
use crate::keytable::{self, KeyTable, PrefixOptions};
use crate::prefix::{PrefixAction, PrefixEngine};
use crate::{CaptureRequest, PaneInput, TermSize};

pub fn statusbar_height() -> Pixels {
    px(28.0)
}

#[derive(Clone)]
struct Listener {
    event_tx: flume::Sender<Event>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if matches!(event, Event::ClipboardStore(..) | Event::Bell) {
            let _ = self.event_tx.try_send(event);
        }
    }
}

/// Per-pane activity classification derived agent-agnostically from structural
/// process state (the tmux foreground-process flag, the client alternate-screen
/// mode, the OSC-133 phase) plus the terminal bell. Precedence:
/// attention > busy > free (`docs/spec-pane-activity-v2.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneActivity {
    /// At a shell prompt or otherwise not running a command.
    Free,
    /// A foreground command is running — stays busy across the command's silent
    /// phases (a running agent reads busy whether working or thinking).
    Busy,
    /// The pane rang the terminal bell and the user has not acknowledged it.
    Attention,
}

/// Whether a pane is busy (a foreground command is running), from its structural
/// inputs. Authority is ordered: the tmux foreground-process flag first, then a
/// client-side structural fallback where that flag is unavailable (legacy path).
///
/// - `is_shell == Some(true)` — the foreground process is the login shell: free.
/// - `is_shell == Some(false)` — a command is running: busy.
/// - `is_shell == None` — the flag is unavailable; fall back to the client
///   structural signals: busy if a full-screen TUI is on the alternate screen
///   or the OSC-133 phase is `Executing`.
///
/// GPUI-free and byte-flow-free so it is unit-testable in isolation
/// (`docs/spec-pane-activity-v2.md`).
fn classify_busy(is_shell: Option<bool>, alt_screen: bool, osc_executing: bool) -> bool {
    match is_shell {
        Some(true) => false,
        Some(false) => true,
        None => alt_screen || osc_executing,
    }
}

/// Mutable per-pane activity signals folded into a [`PaneActivity`]. Kept free
/// of GPUI so the state machine is unit-testable in isolation. Busy/free is a
/// pure function of the structural inputs (see [`classify_busy`]); this tracker
/// holds the pane's foreground-process flag and overlays the terminal bell as
/// attention (`docs/spec-pane-activity-v2.md`).
#[derive(Debug, Default)]
struct ActivityTracker {
    /// The pane's tmux foreground-process flag (#510), pushed down from the
    /// snapshot via [`Self::set_foreground_shell`]: `Some(false)` a command is
    /// running, `Some(true)` at the shell, `None` unavailable (legacy path).
    is_shell: Option<bool>,
    /// Unacknowledged terminal-bell attention. Set by [`Self::on_bell`], cleared
    /// by [`Self::acknowledge`].
    attention: bool,
    /// Whether this pane's window is the session's active window, pushed down
    /// from the window layer via [`Self::set_window_active`]. Gates bell raises:
    /// a bell in the window the user is looking at never raises attention
    /// (`docs/spec-pane-activity-v2.md`).
    window_active: bool,
}

impl ActivityTracker {
    /// Record the pane's tmux foreground-process flag — the authoritative
    /// busy/free signal (`None` on the legacy path, where the client fallback
    /// applies).
    fn set_foreground_shell(&mut self, is_shell: Option<bool>) {
        self.is_shell = is_shell;
    }

    /// Raise unacknowledged attention (the pane rang the terminal bell).
    /// Suppressed while the pane's window is active — the user is already
    /// looking at it (`docs/spec-pane-activity-v2.md`).
    fn on_bell(&mut self) {
        if !self.window_active {
            self.attention = true;
        }
    }

    /// Clear attention back to the underlying busy/free state.
    fn acknowledge(&mut self) {
        self.attention = false;
    }

    /// Record whether this pane's window is the session's active window.
    /// Activation acknowledges any pending attention (viewing the window is
    /// the acknowledgement); while active, bell raises are suppressed at the
    /// source in [`Self::on_bell`].
    fn set_window_active(&mut self, active: bool) {
        self.window_active = active;
        if active {
            self.attention = false;
        }
    }

    /// Classify the pane's activity from its structural inputs, overlaying an
    /// unacknowledged bell as attention.
    fn state(&self, alt_screen: bool, osc_executing: bool) -> PaneActivity {
        if self.attention {
            return PaneActivity::Attention;
        }
        self.underlying(alt_screen, osc_executing)
    }

    /// The pane's busy/free state ignoring any unacknowledged bell — the state
    /// the active window surfaces, so a bell arriving there never flashes
    /// attention (`docs/spec-pane-activity-v2.md`).
    fn underlying(&self, alt_screen: bool, osc_executing: bool) -> PaneActivity {
        if classify_busy(self.is_shell, alt_screen, osc_executing) {
            PaneActivity::Busy
        } else {
            PaneActivity::Free
        }
    }
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

struct HoveredLink {
    row: usize,
    start_col: usize,
    end_col: usize,
    target: String,
}

pub struct PaneView {
    pane_id: String,
    terminal: Arc<Mutex<Term<Listener>>>,
    input_tx: flume::Sender<PaneInput>,
    size_changed_tx: flume::Sender<TermSize>,
    capture_request_tx: flume::Sender<CaptureRequest>,
    /// Parsed pre-attach history rendered as a static block above the live
    /// `Term`'s own scrollback. Captured once on the first scroll past the top
    /// of the live scrollback; invalidated (and re-fetched) only on resize.
    history_block: Option<Vec<Vec<CellRenderInfo>>>,
    /// How many rows of the pre-attach block are scrolled into view above the
    /// live region. `> 0` implies the live `Term` is pinned fully scrolled up.
    history_scroll: usize,
    /// A capture is in flight; suppress duplicate requests.
    history_pending: bool,
    /// The scroll-up remainder that triggered the in-flight capture, applied to
    /// `history_scroll` once the block arrives so the gesture is not lost.
    history_pending_scroll: usize,
    /// `cols` at capture time; the block is sized to this width and goes stale
    /// when the grid width changes.
    history_capture_cols: usize,
    focus_handle: FocusHandle,
    cell_size: Size<Pixels>,
    grid_size: TermSize,
    selection: Option<GridSelection>,
    selecting: bool,
    cursor_blink_visible: bool,
    paint_cache: TerminalGridPaintCacheHandle,
    working_directory: Option<String>,
    current_command: Option<String>,
    command_lifecycle: CommandLifecycle,
    mouse_mode_active: bool,
    hovered_link: Option<HoveredLink>,
    prev_selection: Option<GridSelection>,
    tmux_size: Option<TermSize>,
    content_origin: Point<Pixels>,
    /// Whole-client render font size, pushed down by `SessionView`. Drives cell
    /// metrics; the grid auto-detects the change via its paint style key.
    font_size: Pixels,
    /// Reports a font-zoom delta (`+1`/`-1`) to `SessionView` when the focused
    /// pane intercepts a zoom shortcut.
    font_zoom_tx: flume::Sender<i32>,
    /// Dispatches a resolved tmux key-table binding through the single
    /// command seam (the same channel `SessionView`'s chrome uses).
    tmux_command_tx: flume::Sender<String>,
    /// The mirrored `list-keys`/`show-options` lookup, pushed down from
    /// `SessionView` and refreshed in place via [`Self::set_key_table`]
    /// (`docs/spec-tmux-keytable-mirroring.md`).
    key_table: Arc<KeyTable>,
    prefix_options: PrefixOptions,
    /// Prefix chord capture/repeat state for this pane's `on_key_down`.
    prefix_engine: PrefixEngine,
    /// Agent-agnostic activity signals (the tmux foreground-process flag, the
    /// alternate-screen mode, the OSC-133 phase, the terminal bell) folded into
    /// a [`PaneActivity`] via [`Self::activity`].
    activity: ActivityTracker,
}

impl PaneView {
    /// - `tmux_command_tx` — dispatches resolved tmux key-table bindings
    ///   through the single command seam (`SessionView`'s chrome channel). A
    ///   binding-mutating dispatch's follow-up key-table refresh is issued
    ///   server-side, on the same seam, after the mutating command lands —
    ///   not requested from here (`spawn_command_bridge` in `crates/app`).
    /// - `key_table`/`prefix_options` — the mirrored `list-keys`/
    ///   `show-options` lookup, pushed down from `SessionView`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cx: &mut Context<Self>,
        pty_rx: flume::Receiver<Vec<u8>>,
        input_tx: flume::Sender<PaneInput>,
        size_changed_tx: flume::Sender<TermSize>,
        capture_request_tx: flume::Sender<CaptureRequest>,
        font_zoom_tx: flume::Sender<i32>,
        tmux_command_tx: flume::Sender<String>,
        key_table: Arc<KeyTable>,
        prefix_options: PrefixOptions,
    ) -> Self {
        let grid_size = TermSize { cols: 80, rows: 24 };
        let config = Config::default();
        let (term_event_tx, term_event_rx) = flume::unbounded();
        let listener = Listener {
            event_tx: term_event_tx,
        };
        let terminal = Arc::new(Mutex::new(Term::new(config, &grid_size, listener)));

        {
            let terminal = terminal.clone();
            cx.spawn(async move |this, cx| {
                let mut osc = OscInterceptor::new();
                let mut parser: Processor = Processor::new();
                debug!("PTY read loop started");
                loop {
                    let Ok(data) = pty_rx.recv_async().await else {
                        debug!("PTY stream closed");
                        break;
                    };
                    let mut chunks = vec![data];
                    while let Ok(more) = pty_rx.try_recv() {
                        chunks.push(more);
                    }
                    let term_ref = terminal.clone();
                    let mut p = parser;
                    let mut o = osc;
                    let parse_result = smol::unblock(move || {
                        let mut term = term_ref.lock().map_err(|_| TerminalError::LockPoisoned)?;
                        let mut events = Vec::new();
                        for chunk in &chunks {
                            let (filtered, chunk_events) = o.process(chunk);
                            events.extend(chunk_events);
                            if !filtered.is_empty() {
                                p.advance(&mut *term, &filtered);
                            }
                        }
                        Ok::<_, TerminalError>((p, o, events))
                    })
                    .await;
                    let (p_ret, o_ret, all_osc_events) = match parse_result {
                        Ok(val) => val,
                        Err(e) => {
                            error!(%e, "PTY parse loop aborting");
                            break;
                        }
                    };
                    parser = p_ret;
                    osc = o_ret;
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
                // The loop ending means this pane's PTY channel closed (the pane
                // was dropped from the snapshot, e.g. via `exit`). That is a
                // single-pane teardown, not a session end. The whole tmux
                // session ending surfaces as the visible
                // `ConnectionStatus::Disconnected` state on the session's
                // connection-status loop — never an app quit (#476).
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

        Self {
            pane_id: String::new(),
            terminal,
            input_tx,
            size_changed_tx,
            capture_request_tx,
            history_block: None,
            history_scroll: 0,
            history_pending: false,
            history_pending_scroll: 0,
            history_capture_cols: 0,
            focus_handle: cx.focus_handle(),
            cell_size: size(px(0.0), px(0.0)),
            grid_size,
            selection: None,
            selecting: false,
            cursor_blink_visible: true,
            paint_cache: TerminalGridPaintCacheHandle::default(),
            working_directory: None,
            current_command: None,
            command_lifecycle: CommandLifecycle::default(),
            mouse_mode_active: false,
            hovered_link: None,
            prev_selection: None,
            tmux_size: None,
            content_origin: Point::default(),
            font_size: px(14.0),
            font_zoom_tx,
            tmux_command_tx,
            key_table,
            prefix_options,
            prefix_engine: PrefixEngine::new(),
            activity: ActivityTracker::default(),
        }
    }

    pub fn grid_size(&self) -> TermSize {
        self.grid_size
    }

    pub fn working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }

    pub fn set_pane_id(&mut self, id: String) {
        self.pane_id = id;
    }

    pub fn set_working_directory(&mut self, path: String) {
        self.working_directory = Some(path);
    }

    pub fn current_command(&self) -> Option<&str> {
        self.current_command.as_deref()
    }

    /// Whether this pane is mid-capture of a tmux prefix chord — drives the
    /// statusbar pending-prefix indicator in `SessionView`.
    pub fn prefix_pending(&self) -> bool {
        self.prefix_engine.pending()
    }

    pub fn set_current_command(&mut self, command: String) {
        self.current_command = Some(command);
    }

    /// Apply a refreshed mirrored key-table lookup and prefix/repeat options
    /// (`SessionView` re-parsed a `list-keys`/`show-options` reply). The
    /// caller is responsible for `cx.notify()`.
    pub fn set_key_table(&mut self, key_table: Arc<KeyTable>, prefix_options: PrefixOptions) {
        self.key_table = key_table;
        self.prefix_options = prefix_options;
    }

    /// Handle a resolved bound command from the prefix engine: classify it
    /// (`keytable::classify_command`) and dispatch, hint, or confirm per the
    /// command-interception taxonomy (`docs/spec-tmux-keytable-mirroring.md`).
    fn handle_dispatch(&mut self, command: String, window: &mut Window, cx: &mut Context<Self>) {
        match keytable::classify_command(&command) {
            keytable::DispatchDecision::Dispatch(cmd) => self.dispatch_tmux_command(cmd),
            keytable::DispatchDecision::Hint(hint) => {
                window.push_notification(Notification::info(hint), cx);
            }
            keytable::DispatchDecision::Confirm { prompt, wrapped } => {
                self.open_confirm_dialog(prompt, wrapped, window, cx);
            }
            keytable::DispatchDecision::SwitchTable(table) => {
                // A mirror of the engine's own table state, never a server
                // dispatch (#484). Notify here because the confirm-dialog
                // path also lands here, outside the key-down notify.
                self.prefix_engine.switch_table(&table);
                cx.notify();
            }
        }
    }

    /// Send a resolved command through the single command seam. A binding-
    /// mutating dispatch's key-table refresh is issued server-side, on the
    /// same seam, strictly after the mutating command lands
    /// (`spawn_command_bridge` in `crates/app`) — requesting it from here
    /// instead would race the mutation across two independent
    /// channels/tasks with no ordering guarantee between them.
    fn dispatch_tmux_command(&self, command: String) {
        let _ = self.tmux_command_tx.try_send(command);
    }

    /// Render a native confirm dialog for a bound `confirm-before` command:
    /// `wrapped` dispatches only if the user confirms — a control client
    /// cannot render tmux's own confirmation, so this is what makes stock
    /// chords like `prefix x` (kill-pane) actually work instead of silently
    /// no-opping. On confirm, `wrapped` is routed back through
    /// `handle_dispatch` for re-classification rather than dispatched
    /// unconditionally — a bound `confirm-before` can itself wrap a
    /// pane-mode or other intercepted command (e.g. `confirm-before -p
    /// "copy?" copy-mode`), which must still resolve to a hint instead of
    /// being shoved onto the shared pane.
    fn open_confirm_dialog(
        &self,
        prompt: String,
        wrapped: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        window.open_alert_dialog(cx, move |alert: AlertDialog, _, _| {
            let wrapped = wrapped.clone();
            let entity = entity.clone();
            alert
                .title("tmux confirmation")
                .description(SharedString::from(prompt.clone()))
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    entity.update(cx, |view, cx| {
                        view.handle_dispatch(wrapped.clone(), window, cx);
                    });
                    true
                })
        });
    }

    /// Apply a new whole-client font size. The caller is responsible for
    /// `cx.notify()`. Cell metrics recompute on the next render; the grid
    /// repaints fully because its paint style key embeds the font size.
    pub fn set_font_size(&mut self, size: Pixels) {
        if size == self.font_size {
            return;
        }
        self.font_size = size;
        self.paint_cache.clear();
    }

    pub fn set_tmux_size(&mut self, cols: u16, rows: u16) {
        let new_size = TermSize {
            cols: cols as usize,
            rows: rows as usize,
        };
        self.tmux_size = Some(new_size);
        if new_size != self.grid_size {
            self.grid_size = new_size;
            {
                let mut term = self.terminal.lock().expect("term lock poisoned");
                term.resize(new_size);
            }
            self.paint_cache.clear();
            self.invalidate_history();
        }
    }

    /// Drop the parsed pre-attach block and reset scroll into it. The block was
    /// sized to the previous `cols`/history boundary; a resize reflows tmux's
    /// history, so it must be re-captured on the next scroll past the top.
    fn invalidate_history(&mut self) {
        self.history_block = None;
        self.history_scroll = 0;
        self.history_pending = false;
        self.history_pending_scroll = 0;
    }

    /// Composite scroll across the live `Term`'s own scrollback (post-attach) and
    /// the static pre-attach block above it. Positive `delta` scrolls up.
    fn handle_scroll(&mut self, delta: i32, cx: &mut Context<Self>) {
        if delta == 0 {
            return;
        }
        let (history_size, display_offset, alt_screen) = {
            let term = self.terminal.lock().expect("term lock poisoned");
            (
                term.grid().history_size(),
                term.grid().display_offset(),
                term.mode().contains(TermMode::ALT_SCREEN),
            )
        };

        if delta > 0 {
            let delta = delta as usize;
            let room_in_live = history_size.saturating_sub(display_offset);
            let live_step = room_in_live.min(delta);
            if live_step > 0 {
                let mut term = self.terminal.lock().expect("term lock poisoned");
                term.scroll_display(alacritty_terminal::grid::Scroll::Delta(live_step as i32));
            }
            let remainder = delta - live_step;
            // No pre-attach history while the live `Term` is on the alternate
            // screen (vim/less/htop): capture would return alt-screen content,
            // not history. This matches native terminals.
            if remainder > 0 && !alt_screen {
                match self.history_block.as_ref().map(Vec::len) {
                    Some(block_rows) => {
                        self.history_scroll = (self.history_scroll + remainder).min(block_rows);
                    }
                    None => self.request_history(history_size, remainder),
                }
            }
        } else {
            let down = (-delta) as usize;
            let from_history = down.min(self.history_scroll);
            self.history_scroll -= from_history;
            let live_down = down - from_history;
            if live_down > 0 {
                let mut term = self.terminal.lock().expect("term lock poisoned");
                term.scroll_display(alacritty_terminal::grid::Scroll::Delta(-(live_down as i32)));
            }
        }

        // The history composite repaints the whole viewport; drop the cached row
        // ops so a stale frame is never reused across a scroll.
        self.paint_cache.clear();
        cx.notify();
    }

    /// Ask the SSH thread for the pre-attach history: everything above the lines
    /// the live `Term` already holds. tmux line `-(history_size + 1)` is the
    /// newest pre-attach line; `-` is the oldest. `-J` is off so captured lines
    /// map 1:1 to grid rows.
    fn request_history(&mut self, history_size: usize, remainder: usize) {
        if self.history_pending || self.pane_id.is_empty() {
            return;
        }
        let end_row = format!("-{}", history_size + 1);
        if self
            .capture_request_tx
            .try_send(CaptureRequest {
                pane_id: self.pane_id.clone(),
                start_row: "-".to_string(),
                end_row,
                join_wraps: false,
            })
            .is_ok()
        {
            self.history_pending = true;
            self.history_pending_scroll = remainder;
            self.history_capture_cols = self.grid_size.cols;
        }
    }

    /// Receive a captured pre-attach payload. An empty payload (capture error or
    /// no pre-attach history) just clears the in-flight flag so scrolling never
    /// wedges and a later attempt can retry. The VTE replay is CPU-bound, so it
    /// runs off the UI thread (`smol::unblock`) like the live PTY parse loop.
    pub fn apply_history(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.history_pending = false;
        if bytes.is_empty() {
            return;
        }
        let cols = self.history_capture_cols.max(1);
        let pending_scroll = self.history_pending_scroll;
        cx.spawn(async move |this, cx| {
            let rows = smol::unblock(move || parse_capture_to_rows(&bytes, cols)).await;
            if rows.is_empty() {
                return;
            }
            let _ = cx.update(|cx| {
                this.update(cx, |view, cx| {
                    view.history_scroll = pending_scroll.min(rows.len());
                    view.history_block = Some(rows);
                    view.paint_cache.clear();
                    cx.notify();
                })
            });
        })
        .detach();
    }

    fn send_input(&self, bytes: Vec<u8>) {
        let _ = self.input_tx.try_send(PaneInput {
            pane_id: self.pane_id.clone(),
            bytes,
        });
    }

    /// Paste clipboard text into the pane, honoring bracketed-paste mode and
    /// normalizing line endings (see `keyboard::encode_paste`).
    fn paste_text(&self, text: &str) {
        let mode = {
            let term = self.terminal.lock().expect("term lock poisoned");
            *term.mode()
        };
        self.send_input(keyboard::encode_paste(text, mode));
    }

    pub fn command_lifecycle(&self) -> &CommandLifecycle {
        &self.command_lifecycle
    }

    /// The pane's structural busy/free inputs read live: alternate-screen mode
    /// (a full-screen foreground TUI) from the client `Term`, and whether the
    /// OSC-133 phase is `Executing`. Fed to the [`ActivityTracker`], which
    /// prefers the tmux foreground-process flag and falls back to these only on
    /// the legacy path (`docs/spec-pane-activity-v2.md`).
    fn structural_activity_inputs(&self) -> (bool, bool) {
        let alt_screen = {
            let term = self.terminal.lock().expect("term lock poisoned");
            term.mode().contains(TermMode::ALT_SCREEN)
        };
        let osc_executing = self.command_lifecycle.phase == CommandPhase::Executing;
        (alt_screen, osc_executing)
    }

    /// This pane's current activity state (attention > busy > free), derived
    /// agent-agnostically from the tmux foreground-process flag (with an
    /// alternate-screen / OSC-133 fallback on the legacy path) and the terminal
    /// bell (`docs/spec-pane-activity-v2.md`).
    pub fn activity(&self) -> PaneActivity {
        let (alt_screen, osc_executing) = self.structural_activity_inputs();
        self.activity.state(alt_screen, osc_executing)
    }

    /// This pane's underlying busy/free state with any unacknowledged bell
    /// suppressed — what the active window surfaces, so a bell arriving between
    /// snapshots never flashes its tab (`docs/spec-pane-activity-v2.md`).
    pub fn underlying_activity(&self) -> PaneActivity {
        let (alt_screen, osc_executing) = self.structural_activity_inputs();
        self.activity.underlying(alt_screen, osc_executing)
    }

    /// Clear this pane's unacknowledged bell attention back to its underlying
    /// busy/free state. Called by the window layer when the user selects this
    /// pane's window locally (tab click, Alt+1..9), so the badge clears without
    /// waiting for the confirming snapshot (`docs/spec-pane-activity-indicators.md`).
    pub fn acknowledge_attention(&mut self) {
        self.activity.acknowledge();
    }

    /// Record whether this pane's window is the session's active window, from
    /// the snapshot's `is_active` flag. While active, a bell never raises
    /// attention; the activation edge acknowledges any pending attention
    /// (`docs/spec-pane-activity-indicators.md`).
    pub fn set_window_active(&mut self, active: bool) {
        self.activity.set_window_active(active);
    }

    /// Record this pane's tmux foreground-process flag (#510) from the snapshot,
    /// the authoritative busy/free signal: `Some(false)` a command is running
    /// (busy), `Some(true)` at the shell (free), `None` unavailable (the client
    /// structural fallback applies). Pushed down alongside `set_window_active`
    /// (`docs/spec-pane-activity-v2.md`).
    pub fn set_foreground_shell(&mut self, is_shell: Option<bool>) {
        self.activity.set_foreground_shell(is_shell);
    }

    fn pixel_to_grid(&self, pos: Point<Pixels>) -> (usize, usize) {
        if self.cell_size.width <= px(0.0) || self.cell_size.height <= px(0.0) {
            return (0, 0);
        }

        let local_x = pos.x - self.content_origin.x;
        let local_y = pos.y - self.content_origin.y;
        let col = (local_x / self.cell_size.width).floor().max(0.0) as usize;
        let row = (local_y / self.cell_size.height).floor().max(0.0) as usize;
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
        match event {
            Event::ClipboardStore(_, text) => {
                debug!(len = text.len(), "OSC 52: clipboard store");
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            Event::Bell => {
                debug!("terminal bell: raising pane attention");
                self.activity.on_bell();
            }
            _ => {}
        }
    }

    fn detect_link_at(&self, row: usize, col: usize) -> Option<HoveredLink> {
        let term = self.terminal.lock().expect("term lock poisoned");
        let grid = term.grid();
        let display_offset = grid.display_offset();
        let line = Line(row as i32 - display_offset as i32);
        let columns = grid.columns();

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
            self.send_input(bytes);
        }
        true
    }
}

impl Focusable for PaneView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub fn measure_cell_size(window: &mut Window, font_size: Pixels) -> Size<Pixels> {
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

/// Parse a `capture-pane` payload (`-J` off, `-e` on) into styled rows by
/// replaying it through a scratch `Term`. The scratch screen is sized to the
/// logical line count with generous scrollback, so any line that still wraps
/// (e.g. a capture taken at a wider width before a resize invalidates it) lands
/// in the scratch history rather than being lost — the real produced row count
/// is then read back from the grid, not assumed from the requested range or the
/// `\n` count (which would silently drop wrapped history).
fn parse_capture_to_rows(payload: &[u8], cols: usize) -> Vec<Vec<CellRenderInfo>> {
    if payload.is_empty() || cols == 0 {
        return Vec::new();
    }

    let line_count = payload.iter().filter(|byte| **byte == b'\n').count() + 1;
    let size = TermSize {
        cols,
        rows: line_count.max(1),
    };
    let config = Config {
        scrolling_history: line_count + cols,
        ..Config::default()
    };
    let (event_tx, _event_rx) = flume::unbounded();
    let listener = Listener { event_tx };
    let mut term = Term::new(config, &size, listener);
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, payload);

    let grid = term.grid();
    let hist = grid.history_size();
    let last_line = grid.cursor.point.line.0.max(0) as usize;
    // Render row `r` maps to grid line `r - hist`, so row 0 is the oldest
    // scrollback line and the final content line is at `hist + last_line`.
    let total = hist + last_line + 1;

    (0..total)
        .map(|row| extract_row_cells(&term, row, hist, None))
        .collect()
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

impl Render for PaneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_size = self.font_size;
        let cell_size = measure_cell_size(window, font_size);
        self.cell_size = cell_size;

        let new_size = if let Some(ts) = self.tmux_size {
            ts
        } else {
            let viewport = window.viewport_size();
            let cols = (viewport.width / cell_size.width).floor() as usize;
            let rows = ((viewport.height - statusbar_height()) / cell_size.height).floor() as usize;
            TermSize {
                cols: cols.max(1),
                rows: rows.max(1),
            }
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
            if self.tmux_size.is_none() {
                let _ = self.size_changed_tx.try_send(new_size);
            }
            self.paint_cache.clear();
            // Reflow stales the pre-attach block (sized to the old width); drop it
            // so the next top-scroll re-captures. Inlined rather than calling
            // `invalidate_history` because the `term` lock guard is still alive.
            self.history_block = None;
            self.history_scroll = 0;
            self.history_pending = false;
            self.history_pending_scroll = 0;
        }

        let damage_start = std::time::Instant::now();
        let mut paint_damage = map_damage(&mut term);
        add_span_damage_compute_us(damage_start.elapsed().as_micros() as u64);

        if self.selection != self.prev_selection {
            paint_damage = TerminalGridPaintDamage::Full;
            self.prev_selection = self.selection.clone();
        }

        let mode = *term.mode();

        let is_focused = self.focus_handle.is_focused(window);

        let cursor_point = term.grid().cursor.point;
        let cursor_row = cursor_point.line.0 as usize;
        let cursor_col = cursor_point.column.0;
        let cursor_style = term.cursor_style();
        let cursor_shape = if mode.contains(TermMode::SHOW_CURSOR) {
            cursor_style.shape
        } else {
            CursorShape::Hidden
        };
        let show_cursor = is_focused
            && cursor_shape != CursorShape::Hidden
            && self.history_scroll == 0
            && (self.cursor_blink_visible || !cursor_style.blinking);
        let mouse_now = mode.intersects(
            TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
        );
        if mouse_now != self.mouse_mode_active {
            debug!(enabled = mouse_now, "mouse mode changed");
            self.mouse_mode_active = mouse_now;
        }

        let display_offset = term.grid().display_offset();

        // When scrolled into the pre-attach block, its bottom `history_rows` rows
        // occupy the top of the viewport and the live `Term` (pinned fully
        // scrolled up) fills the rest. `history_scroll == 0` is the unchanged
        // pure-live path.
        let history_rows = self
            .history_block
            .as_ref()
            .map_or(0, |block| self.history_scroll.min(block.len()))
            .min(new_size.rows);

        let mut grid_rows: Vec<Arc<Vec<CellRenderInfo>>> = Vec::with_capacity(new_size.rows);
        if let (true, Some(block)) = (history_rows > 0, self.history_block.as_ref()) {
            let first = block.len() - self.history_scroll.min(block.len());
            for (row_idx, block_row) in block[first..first + history_rows].iter().enumerate() {
                let mut cells = block_row.clone();
                for cell in &mut cells {
                    cell.row = row_idx;
                }
                grid_rows.push(Arc::new(cells));
            }
        }
        for row_idx in history_rows..new_size.rows {
            let mut row_cells = extract_row_cells(
                &term,
                row_idx - history_rows,
                display_offset,
                self.selection.as_ref(),
            );
            // Live rows render after the history block, so shift their grid-relative
            // index to the viewport row they actually occupy (a no-op when
            // `history_rows == 0`).
            for cell in &mut row_cells {
                cell.row = row_idx;
            }
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

        let entity = cx.entity().clone();
        let bounds_observer = canvas(
            move |bounds: Bounds<Pixels>, _window: &mut Window, cx: &mut App| {
                entity.update(cx, |view: &mut Self, _cx| {
                    view.content_origin = bounds.origin;
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let mut terminal_area = div()
            .id("terminal")
            .key_context(crate::TERMINAL_KEY_CONTEXT)
            .track_focus(&self.focus_handle(cx))
            .flex_1()
            .bg(bg_hsla);

        if self.hovered_link.is_some() {
            terminal_area = terminal_area.cursor(gpui::CursorStyle::PointingHand);
        }

        terminal_area
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
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
                            this.paste_text(&text);
                        }
                    }
                    return;
                }

                // Whole-client font zoom. Ctrl++ (key "+", or "=" on QWERTY where
                // Ctrl+Shift+= yields "+") zooms in; Ctrl+- zooms out. The delta
                // goes to `SessionView`, the source of truth for font size.
                if ks.modifiers.control {
                    let delta = match ks.key.as_str() {
                        "+" | "=" => 1,
                        "-" => -1,
                        _ => 0,
                    };
                    if delta != 0 {
                        let _ = this.font_zoom_tx.try_send(delta);
                        return;
                    }
                }

                // tmux key-table mirroring: after the rift-native early
                // returns above, before PTY fallthrough below (constitution
                // precedence: rift-native -> tmux tables -> typing). Keys
                // with no tmux representation (bare modifiers, unmapped
                // names) skip the engine entirely and fall through unchanged.
                if let Some(tmux_key) = keytable::keystroke_to_tmux_key(ks) {
                    let action = this.prefix_engine.handle_key(
                        &tmux_key,
                        &this.key_table,
                        &this.prefix_options,
                        Instant::now(),
                    );
                    match action {
                        PrefixAction::Dispatch(command) => {
                            this.handle_dispatch(command, window, cx);
                            cx.notify();
                            return;
                        }
                        PrefixAction::Consume => {
                            cx.notify();
                            return;
                        }
                        PrefixAction::PassThrough => {}
                    }
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
                    // Typing snaps back to the live bottom, leaving the pre-attach
                    // block too.
                    this.history_scroll = 0;
                    this.selection = None;
                    this.cursor_blink_visible = true;
                    this.send_input(bytes);
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
                            this.send_input(bytes);
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
                                this.paste_text(&text);
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
                                this.paste_text(&text);
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
                            this.send_input(bytes);
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
                        this.handle_scroll(delta, cx);
                    }
                }
            }))
            .child(bounds_observer)
            .child(grid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_text(row: &[CellRenderInfo]) -> String {
        row.iter().map(|cell| cell.char).collect::<String>()
    }

    #[::core::prelude::v1::test]
    fn test_parse_capture_empty_payload_yields_no_rows() {
        assert!(parse_capture_to_rows(b"", 80).is_empty());
        assert!(parse_capture_to_rows(b"abc", 0).is_empty());
    }

    #[::core::prelude::v1::test]
    fn test_parse_capture_maps_each_line_to_one_row() {
        let rows = parse_capture_to_rows(b"ab\r\ncd", 4);
        assert_eq!(rows.len(), 2);
        assert_eq!(row_text(&rows[0]).trim_end(), "ab");
        assert_eq!(row_text(&rows[1]).trim_end(), "cd");
        // Each row is padded to the capture width.
        assert_eq!(rows[0].len(), 4);
    }

    #[::core::prelude::v1::test]
    fn test_parse_capture_wrapped_line_expands_to_multiple_rows() {
        // A line longer than `cols` wraps into several grid rows. Sizing by the
        // real (wrapped) row count rather than the `\n` count is what keeps the
        // upper history from being silently dropped (the discarded first seam's
        // latent bug).
        let rows = parse_capture_to_rows(b"abcde\r\nfg", 3);
        assert_eq!(rows.len(), 3);
        assert_eq!(row_text(&rows[0]).trim_end(), "abc");
        assert_eq!(row_text(&rows[1]).trim_end(), "de");
        assert_eq!(row_text(&rows[2]).trim_end(), "fg");
    }

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

    #[::core::prelude::v1::test]
    fn test_activity_default_is_free() {
        // No foreground-process flag and no structural signal: free.
        let tracker = ActivityTracker::default();
        assert_eq!(tracker.state(false, false), PaneActivity::Free);
    }

    #[::core::prelude::v1::test]
    fn test_classify_busy_shell_true_is_always_free() {
        // A shell prompt reads free regardless of any client structural signal.
        for &alt in &[false, true] {
            for &osc in &[false, true] {
                assert!(!classify_busy(Some(true), alt, osc), "alt={alt} osc={osc}");
            }
        }
    }

    #[::core::prelude::v1::test]
    fn test_classify_busy_shell_false_is_always_busy() {
        // A running foreground command reads busy regardless of alt-screen/OSC.
        for &alt in &[false, true] {
            for &osc in &[false, true] {
                assert!(classify_busy(Some(false), alt, osc), "alt={alt} osc={osc}");
            }
        }
    }

    #[::core::prelude::v1::test]
    fn test_classify_busy_legacy_none_follows_structural_fallback() {
        // Legacy path (no is_shell): busy iff a full-screen TUI (alt-screen) or
        // an executing OSC-133 command is present.
        assert!(!classify_busy(None, false, false));
        assert!(classify_busy(None, true, false));
        assert!(classify_busy(None, false, true));
        assert!(classify_busy(None, true, true));
    }

    #[::core::prelude::v1::test]
    fn test_activity_all_input_combinations_match_classifier() {
        // Every (is_shell, alt_screen, osc_executing, attention) combination
        // folds to: an unacknowledged bell overlays attention, otherwise the
        // pure classifier decides busy/free.
        for is_shell in [Some(true), Some(false), None] {
            for &alt in &[false, true] {
                for &osc in &[false, true] {
                    for &attention in &[false, true] {
                        let mut tracker = ActivityTracker::default();
                        tracker.set_foreground_shell(is_shell);
                        if attention {
                            tracker.on_bell();
                        }
                        let expected = if attention {
                            PaneActivity::Attention
                        } else if classify_busy(is_shell, alt, osc) {
                            PaneActivity::Busy
                        } else {
                            PaneActivity::Free
                        };
                        assert_eq!(
                            tracker.state(alt, osc),
                            expected,
                            "is_shell={is_shell:?} alt={alt} osc={osc} attention={attention}"
                        );
                    }
                }
            }
        }
    }

    #[::core::prelude::v1::test]
    fn test_activity_silent_but_running_stays_busy() {
        // Regression: a foreground command that emits no output stays busy. With
        // byte-flow removed there is no recency decay, so it never ages to free
        // (the old 1500 ms window no longer applies).
        let mut tracker = ActivityTracker::default();
        tracker.set_foreground_shell(Some(false));
        assert_eq!(tracker.state(false, false), PaneActivity::Busy);
    }

    #[::core::prelude::v1::test]
    fn test_activity_byte_burst_never_sets_busy_on_free_shell() {
        // Regression: a shell at its prompt has no output-driven path to busy —
        // a redraw burst (mouse reports, a window-select repaint) cannot flip it,
        // because the derivation reads only the structural inputs, never bytes.
        let mut tracker = ActivityTracker::default();
        tracker.set_foreground_shell(Some(true));
        assert_eq!(tracker.state(false, false), PaneActivity::Free);
    }

    #[::core::prelude::v1::test]
    fn test_activity_bare_shell_reads_free() {
        // Regression: a never-ran / freshly-split / exited-to-prompt pane reads
        // free, and stays free under any incidental client structural noise.
        let mut tracker = ActivityTracker::default();
        tracker.set_foreground_shell(Some(true));
        assert_eq!(tracker.state(false, false), PaneActivity::Free);
        assert_eq!(tracker.state(true, true), PaneActivity::Free);
    }

    #[::core::prelude::v1::test]
    fn test_activity_bell_raises_attention_and_acknowledge_clears() {
        let mut tracker = ActivityTracker::default();
        tracker.set_foreground_shell(Some(false));

        tracker.on_bell();
        // Attention overlays the underlying busy state.
        assert_eq!(tracker.state(false, false), PaneActivity::Attention);

        tracker.acknowledge();
        // Cleared back to the underlying structural state (busy: a command runs).
        assert_eq!(tracker.state(false, false), PaneActivity::Busy);
    }

    #[::core::prelude::v1::test]
    fn test_activity_underlying_ignores_unacknowledged_bell() {
        // The active window reads `underlying`: an unacknowledged bell surfaces
        // attention via `state`, but `underlying` reports the busy/free beneath
        // it so a bell in the active window never flashes its tab.
        let mut tracker = ActivityTracker::default();
        tracker.set_foreground_shell(Some(false));
        tracker.on_bell();

        assert_eq!(tracker.state(false, false), PaneActivity::Attention);
        assert_eq!(tracker.underlying(false, false), PaneActivity::Busy);
    }

    #[::core::prelude::v1::test]
    fn test_activity_bell_while_window_active_raises_no_attention() {
        // A bell in the active window is suppressed at raise time: the user is
        // already looking at it, so no stale attention flag survives leaving
        // the window.
        let mut tracker = ActivityTracker::default();
        tracker.set_foreground_shell(Some(true));
        tracker.set_window_active(true);

        tracker.on_bell();
        assert_eq!(tracker.state(false, false), PaneActivity::Free);

        // Leaving the window afterwards must not resurface the suppressed bell.
        tracker.set_window_active(false);
        assert_eq!(tracker.state(false, false), PaneActivity::Free);
    }

    #[::core::prelude::v1::test]
    fn test_activity_window_activation_clears_pending_attention() {
        // A bell in a background window raises attention; activating the
        // window acknowledges it (viewing is the acknowledgement), and a later
        // deactivation does not re-raise it.
        let mut tracker = ActivityTracker::default();
        tracker.set_foreground_shell(Some(true));

        tracker.on_bell();
        assert_eq!(tracker.state(false, false), PaneActivity::Attention);

        tracker.set_window_active(true);
        assert_eq!(tracker.state(false, false), PaneActivity::Free);

        tracker.set_window_active(false);
        assert_eq!(tracker.state(false, false), PaneActivity::Free);
    }
}
