//! Prefix state machine: the tmux key-table mirroring dispatch layer.
//!
//! Consumes the `KeyTable`/`PrefixOptions` built by [`crate::keytable`] and
//! turns a stream of normalized tmux key names into dispatch decisions —
//! whether a keystroke starts a prefix chord, resolves a bound command
//! (`prefix` table, falling back to `root` on a miss — tmux's native
//! unbound-after-prefix retry), continues a `bind -r` repeat window, or falls
//! through untouched. [`PrefixEngine::handle_key`] is a pure function of
//! (state, key, table, options, now) -> action, so it never touches GPUI or
//! the command seam directly and is exhaustively unit-testable; the caller
//! (`pane_view::PaneView::on_key_down`) owns mapping a GPUI keystroke to a
//! tmux key name and sending a `Dispatch` command through the single command
//! seam.

use std::time::{Duration, Instant};

use crate::keytable::{Binding, KeyTable, PrefixOptions};

const PREFIX_TABLE: &str = "prefix";
const ROOT_TABLE: &str = "root";

/// What the caller should do with the keystroke that produced this action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefixAction {
    /// The key was consumed by the state machine (prefix captured, or an
    /// unbound-after-prefix chord discarded per native semantics) — never
    /// forward it to `encode_keystroke`.
    Consume,
    /// Run this tmux command through the single command seam.
    Dispatch(String),
    /// No key-table entry claims this key; fall through to the existing
    /// `encode_keystroke` typing path unchanged.
    PassThrough,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Idle,
    /// Saw the prefix key; the next key resolves against `prefix` (falling
    /// back to `root`) or is discarded.
    Pending,
    /// Just dispatched a `bind -r` binding from `table`; a same-table key
    /// within `repeat-time` of `since` repeats without needing the prefix
    /// again (tmux's `repeat-time` semantics).
    Repeat { table: &'static str, since: Instant },
}

/// Per-pane prefix chord state. One instance lives on the focused
/// `PaneView`, so a chord that starts and finishes while the same pane holds
/// GPUI focus resolves correctly; focus moving mid-chord abandons the
/// capture in the old pane (unspecified by the spec — an accepted edge case).
#[derive(Debug, Default)]
pub struct PrefixEngine {
    state: State,
}

impl PrefixEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a prefix chord is being captured — drives the statusbar
    /// pending-prefix indicator. `false` during a repeat window: that state
    /// is a continuation, not a fresh capture.
    pub fn pending(&self) -> bool {
        matches!(self.state, State::Pending)
    }

    /// Apply a bound `switch-client -T` to the engine's own table state (the
    /// [`crate::keytable::DispatchDecision::SwitchTable`] classification,
    /// #484): `prefix` arms the capture exactly as if the prefix key had been
    /// pressed, any other mirrored table (`root`) returns to idle. Replaces
    /// whatever state is current — including an open repeat window — just as
    /// tmux replaces the client's key table.
    pub fn switch_table(&mut self, table: &str) {
        self.state = if table == PREFIX_TABLE {
            State::Pending
        } else {
            State::Idle
        };
    }

    /// Resolve one normalized tmux key name (from
    /// [`crate::keytable::keystroke_to_tmux_key`]) against the mirrored
    /// tables, advancing the state machine.
    pub fn handle_key(
        &mut self,
        key: &str,
        table: &KeyTable,
        options: &PrefixOptions,
        now: Instant,
    ) -> PrefixAction {
        match self.state {
            State::Repeat {
                table: repeat_table,
                since,
            } => {
                if now.saturating_duration_since(since) < Duration::from_millis(options.repeat_time)
                {
                    self.continue_repeat(repeat_table, key, table, options, now)
                } else {
                    self.state = State::Idle;
                    self.handle_idle(key, table, options, now)
                }
            }
            State::Pending => {
                self.state = State::Idle;
                self.resolve_after_prefix(key, table, now)
            }
            State::Idle => self.handle_idle(key, table, options, now),
        }
    }

    /// No chord in progress: the prefix key starts a capture, otherwise the
    /// key is checked against `root` (`bind -n`) before falling through.
    fn handle_idle(
        &mut self,
        key: &str,
        table: &KeyTable,
        options: &PrefixOptions,
        now: Instant,
    ) -> PrefixAction {
        if is_prefix_key(key, options) {
            self.state = State::Pending;
            return PrefixAction::Consume;
        }
        match table.get(ROOT_TABLE, key) {
            Some(binding) => self.dispatch(ROOT_TABLE, binding, now),
            None => PrefixAction::PassThrough,
        }
    }

    /// The key right after a captured prefix: resolve in `prefix`, retrying
    /// `root` on a miss (native unbound-after-prefix semantics) before
    /// discarding — a prefix-table chord is never forwarded as typing. This
    /// is also how `Escape` cancels a pending capture: it is simply unbound
    /// in both tables, so it discards like any other unbound chord key
    /// rather than needing a hardcoded cancel key.
    fn resolve_after_prefix(&mut self, key: &str, table: &KeyTable, now: Instant) -> PrefixAction {
        if let Some(binding) = table.get(PREFIX_TABLE, key) {
            return self.dispatch(PREFIX_TABLE, binding, now);
        }
        match table.get(ROOT_TABLE, key) {
            Some(binding) => self.dispatch(ROOT_TABLE, binding, now),
            None => PrefixAction::Consume,
        }
    }

    /// A key while inside a `repeat-time` window: a hit in the same table
    /// repeats (resetting the window on another `-r` binding); a miss ends
    /// the repeat and re-evaluates the key exactly as if it had arrived
    /// fresh — it is never swallowed just because a repeat window happened
    /// to be open.
    fn continue_repeat(
        &mut self,
        repeat_table: &'static str,
        key: &str,
        table: &KeyTable,
        options: &PrefixOptions,
        now: Instant,
    ) -> PrefixAction {
        if let Some(binding) = table.get(repeat_table, key) {
            return self.dispatch(repeat_table, binding, now);
        }
        self.state = State::Idle;
        self.handle_idle(key, table, options, now)
    }

    fn dispatch(&mut self, table: &'static str, binding: &Binding, now: Instant) -> PrefixAction {
        self.state = if binding.repeat {
            State::Repeat { table, since: now }
        } else {
            State::Idle
        };
        PrefixAction::Dispatch(binding.command.clone())
    }
}

fn is_prefix_key(key: &str, options: &PrefixOptions) -> bool {
    options.prefix.as_deref() == Some(key) || options.prefix2.as_deref() == Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keytable::{parse_list_keys, parse_options};

    // Mirrors the real-tmux shape used by the keytable.rs fixtures: a stock
    // `send-prefix` binding, one plain prefix binding, one repeatable prefix
    // binding, a `bind -n` root binding, and a sticky-prefix table switch —
    // enough to exercise every transition in this module.
    const FIXTURE: &str = "\
bind-key    -T prefix C-b     send-prefix
bind-key    -T prefix c       new-window
bind-key -r -T prefix Left    resize-pane -L 5
bind-key    -T root  M-Left   select-pane -L
bind-key    -T root  C-Space  switch-client -T prefix
";

    fn table() -> KeyTable {
        parse_list_keys(FIXTURE)
    }

    fn default_options() -> PrefixOptions {
        parse_options("")
    }

    #[test]
    fn test_prefix_then_bound_key_dispatches() {
        let table = table();
        let options = default_options();
        let mut engine = PrefixEngine::new();
        let now = Instant::now();

        assert_eq!(
            engine.handle_key("C-b", &table, &options, now),
            PrefixAction::Consume
        );
        assert!(engine.pending());

        assert_eq!(
            engine.handle_key("c", &table, &options, now),
            PrefixAction::Dispatch("new-window".to_string())
        );
        assert!(!engine.pending());
    }

    #[test]
    fn test_send_prefix_dispatches_generically() {
        let table = table();
        let options = default_options();
        let mut engine = PrefixEngine::new();
        let now = Instant::now();

        engine.handle_key("C-b", &table, &options, now);
        assert_eq!(
            engine.handle_key("C-b", &table, &options, now),
            PrefixAction::Dispatch("send-prefix".to_string())
        );
    }

    #[test]
    fn test_unbound_prefix_key_retries_root_binding() {
        let table = table();
        let options = default_options();
        let mut engine = PrefixEngine::new();
        let now = Instant::now();

        engine.handle_key("C-b", &table, &options, now);
        // Not bound in `prefix`, but bound in `root`.
        assert_eq!(
            engine.handle_key("M-Left", &table, &options, now),
            PrefixAction::Dispatch("select-pane -L".to_string())
        );
    }

    #[test]
    fn test_unbound_after_prefix_with_no_root_match_is_discarded() {
        let table = table();
        let options = default_options();
        let mut engine = PrefixEngine::new();
        let now = Instant::now();

        engine.handle_key("C-b", &table, &options, now);
        assert_eq!(
            engine.handle_key("q", &table, &options, now),
            PrefixAction::Consume
        );
        assert!(!engine.pending());
    }

    #[test]
    fn test_escape_cancels_pending_prefix() {
        let table = table();
        let options = default_options();
        let mut engine = PrefixEngine::new();
        let now = Instant::now();

        engine.handle_key("C-b", &table, &options, now);
        assert!(engine.pending());
        assert_eq!(
            engine.handle_key("Escape", &table, &options, now),
            PrefixAction::Consume
        );
        assert!(!engine.pending());
    }

    #[test]
    fn test_root_binding_dispatches_without_prefix() {
        let table = table();
        let options = default_options();
        let mut engine = PrefixEngine::new();
        let now = Instant::now();

        assert_eq!(
            engine.handle_key("M-Left", &table, &options, now),
            PrefixAction::Dispatch("select-pane -L".to_string())
        );
        assert!(!engine.pending());
    }

    #[test]
    fn test_unbound_root_key_passes_through() {
        let table = table();
        let options = default_options();
        let mut engine = PrefixEngine::new();
        let now = Instant::now();

        assert_eq!(
            engine.handle_key("z", &table, &options, now),
            PrefixAction::PassThrough
        );
    }

    #[test]
    fn test_repeat_binding_repeats_within_window_and_stops_after() {
        let table = table();
        let options = default_options(); // repeat_time defaults to 500ms
        let mut engine = PrefixEngine::new();
        let base = Instant::now();

        engine.handle_key("C-b", &table, &options, base);
        assert_eq!(
            engine.handle_key("Left", &table, &options, base),
            PrefixAction::Dispatch("resize-pane -L 5".to_string())
        );

        // Repeats within the window without the prefix.
        assert_eq!(
            engine.handle_key("Left", &table, &options, base + Duration::from_millis(100)),
            PrefixAction::Dispatch("resize-pane -L 5".to_string())
        );

        // After the window elapses the same key types normally (no `root`
        // binding for plain `Left` in this fixture).
        assert_eq!(
            engine.handle_key("Left", &table, &options, base + Duration::from_millis(700)),
            PrefixAction::PassThrough
        );
    }

    #[test]
    fn test_repeat_miss_falls_back_to_idle_handling() {
        let table = table();
        let options = default_options();
        let mut engine = PrefixEngine::new();
        let base = Instant::now();

        engine.handle_key("C-b", &table, &options, base);
        engine.handle_key("Left", &table, &options, base);
        // Still inside the repeat window, but `M-Left` is not in `prefix` —
        // ends the repeat and resolves as a fresh root-table hit.
        assert_eq!(
            engine.handle_key("M-Left", &table, &options, base + Duration::from_millis(50)),
            PrefixAction::Dispatch("select-pane -L".to_string())
        );
    }

    #[test]
    fn test_custom_prefix_from_options() {
        let table = table();
        let options = parse_options("prefix C-a\n");
        let mut engine = PrefixEngine::new();
        let now = Instant::now();

        // Stock C-b no longer triggers capture (and has no root binding).
        assert_eq!(
            engine.handle_key("C-b", &table, &options, now),
            PrefixAction::PassThrough
        );
        assert_eq!(
            engine.handle_key("C-a", &table, &options, now),
            PrefixAction::Consume
        );
        assert!(engine.pending());
    }

    #[test]
    fn test_prefix2_also_triggers_capture() {
        let table = table();
        let options = parse_options("prefix C-a\nprefix2 C-b\n");
        let mut engine = PrefixEngine::new();
        let now = Instant::now();

        assert_eq!(
            engine.handle_key("C-b", &table, &options, now),
            PrefixAction::Consume
        );
        assert!(engine.pending());
    }

    #[test]
    fn test_switch_table_binding_arms_prefix_capture() {
        use crate::keytable::{classify_command, DispatchDecision};

        let table = table();
        let options = default_options();
        let mut engine = PrefixEngine::new();
        let now = Instant::now();

        // The sticky-prefix root binding resolves to its raw command...
        let action = engine.handle_key("C-Space", &table, &options, now);
        let PrefixAction::Dispatch(command) = action else {
            panic!("expected Dispatch, got {action:?}");
        };
        // ...which classifies as a local table switch, not a server dispatch
        // (#484)...
        let DispatchDecision::SwitchTable(target) = classify_command(&command) else {
            panic!("expected SwitchTable for {command:?}");
        };
        engine.switch_table(&target);

        // ...so the engine is in the prefix table: the next key resolves
        // there without the prefix key ever being pressed.
        assert!(engine.pending());
        assert_eq!(
            engine.handle_key("c", &table, &options, now),
            PrefixAction::Dispatch("new-window".to_string())
        );
        assert!(!engine.pending());
    }

    #[test]
    fn test_switch_table_root_returns_engine_to_idle() {
        let mut engine = PrefixEngine::new();
        engine.switch_table("prefix");
        assert!(engine.pending());
        engine.switch_table("root");
        assert!(!engine.pending());
    }

    #[test]
    fn test_switch_table_replaces_open_repeat_window() {
        let table = table();
        let options = default_options();
        let mut engine = PrefixEngine::new();
        let base = Instant::now();

        engine.handle_key("C-b", &table, &options, base);
        engine.handle_key("Left", &table, &options, base);
        engine.switch_table("prefix");

        // Still inside repeat-time, but the switch replaced the window: the
        // key resolves in the prefix table as a fresh chord.
        assert_eq!(
            engine.handle_key("c", &table, &options, base + Duration::from_millis(50)),
            PrefixAction::Dispatch("new-window".to_string())
        );
    }

    #[test]
    fn test_empty_table_prefix_still_captured_and_discarded() {
        let table = KeyTable::default();
        let options = PrefixOptions::default();
        let mut engine = PrefixEngine::new();
        let now = Instant::now();

        assert_eq!(
            engine.handle_key("C-b", &table, &options, now),
            PrefixAction::Consume
        );
        assert_eq!(
            engine.handle_key("c", &table, &options, now),
            PrefixAction::Consume
        );
        assert!(!engine.pending());
    }
}
