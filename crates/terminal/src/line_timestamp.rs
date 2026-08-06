//! Bounded per-pane map from a [`crate::LineIdTracker`] line id to the
//! wall-clock arrival time of the PTY batch that completed that line
//! (`docs/spec-terminal-timestamps.md`).
//!
//! **Stamping boundary.** The caller captures the arrival `Instant` at the
//! PTY-batch-receive boundary (`pane_view.rs`'s read loop, right after a
//! batch is pulled off the channel and *before* it is handed to
//! `smol::unblock`'s blocking `advance` closure) — never inside that
//! closure, whose background thread pool may schedule it late and record
//! when the thread happened to run rather than when the bytes arrived.
//! [`LineTimestamps::stamp_chunk`] takes that `Instant` as a plain
//! parameter, so the "clock" is injectable by construction (tests below use
//! synthetic `Instant`s, no `Clock` trait needed).
//!
//! **Stamp policy — completion, not birth.** [`crate::LineIdTracker`] mints
//! a row's id the moment the cursor *arrives* at it, so a still-empty row
//! already has an id — but a line should be timestamped when it is
//! *completed* (the batch whose newline pushed the cursor past it), not
//! when it was born. A line whose bytes straddle two batches must take the
//! *completing* batch's time. [`LineTimestamps::stamp_chunk`] implements
//! this by stamping, per call, every id that just became *sealed*: every id
//! minted before this call plus every id minted during it, **except** the
//! newest one (the cursor's new, still in-progress row) — the half-open
//! range `[before_next - 1, after_next - 1)`. Zero growth (a partial line,
//! no newline yet) seals nothing; the in-progress row stays unstamped until
//! a later batch completes it.
//!
//! **Eviction.** After every [`LineTimestamps::stamp_chunk`] call, every id
//! below the tracker's current `oldest_id` is dropped — in lockstep with
//! [`crate::LineIdTracker`]'s own bound, so this map never exceeds the
//! tracker's live window (`history_cap + screen_lines`).
//! [`LineTimestamps::clear`] additionally drops everything on a
//! [`crate::LineIdTracker::reset`] (resize): ids restart from 0 there, so
//! stale entries would otherwise misattribute an old timestamp to unrelated
//! new content.
//!
//! **Alt screen.** alacritty's alternate-screen buffer is a *separate* grid
//! with its own, unrelated `history_size()`/cursor line (always
//! `history_size() == 0`, no scrollback). Feeding that into the
//! primary-screen tracker would look like a scrollback purge and evict the
//! whole map. [`LineTimestamps::stamp_chunk`] takes an `alt_screen` flag
//! and, when set, skips the tracker update and any stamping entirely for
//! that chunk — the primary map is simply left untouched and resumes
//! correctly once the alternate screen closes.
//!
//! **Post-saturation drift (accepted, not mitigated here).** Once a pane's
//! scrollback saturates, [`crate::LineIdTracker`] undercounts births for
//! auto-wrapped lines with no trailing newline byte (`line_id` module docs,
//! spec decision log 2026-08-06); a precise fix would need to simulate the
//! cursor against arbitrary escape sequences, i.e. reimplement a VTE, which
//! is out of scope. This map stamps and evicts by whatever id the tracker
//! reports, with no independent correction, so it inherits that drift
//! as-is. Accepted for v1 rather than mitigated: the map stays bounded and
//! internally consistent regardless (drift only shifts *which* stale ids
//! get evicted a little early or late, it never leaks or grows unbounded);
//! the on-demand hover surface (#934) matters most for *recent* lines,
//! least likely to have crossed a long session's saturation wall; and
//! re-anchoring away the drift would need its own ground truth for "true"
//! row identity, the same unavailable-callback problem `line_id.rs` already
//! found no solution for short of a VTE reimplementation.

use std::collections::HashMap;
use std::time::Instant;

use crate::LineIdTracker;

/// A bounded per-pane map from a [`LineIdTracker`] line id to the
/// wall-clock arrival time of the PTY batch that completed that line. See
/// the module docs for the stamping/eviction policy.
#[derive(Debug, Default)]
pub struct LineTimestamps {
    map: HashMap<u64, Instant>,
}

impl LineTimestamps {
    pub fn new() -> Self {
        Self::default()
    }

    /// The single seam the PTY read loop drives once per processed chunk:
    /// feeds `tracker` with this chunk's post-`advance()` grid state
    /// (skipped entirely while `alt_screen`, see module docs), stamps every
    /// id newly *sealed* by that update with `arrival`, and evicts in
    /// lockstep with the tracker's own bound.
    ///
    /// - `history_size` / `screen_lines` / `cursor_line`: read from
    ///   `term.grid()` immediately after this chunk's `advance()` call, the
    ///   same values [`LineIdTracker::update`] expects.
    /// - `bytes_fed`: exactly the byte slice handed to that `advance()`
    ///   call (the OSC-filtered bytes).
    /// - `arrival`: the batch's arrival time, captured by the caller before
    ///   the blocking `advance` closure — see module docs.
    #[allow(clippy::too_many_arguments)]
    pub fn stamp_chunk(
        &mut self,
        tracker: &mut LineIdTracker,
        alt_screen: bool,
        history_size: usize,
        screen_lines: usize,
        cursor_line: usize,
        bytes_fed: &[u8],
        arrival: Instant,
    ) {
        if alt_screen {
            return;
        }
        let before_next = next_id(tracker);
        tracker.update(history_size, screen_lines, cursor_line, bytes_fed);
        let after_next = next_id(tracker);
        let oldest = tracker.oldest_id().unwrap_or(after_next);

        self.stamp_range(
            before_next.saturating_sub(1),
            after_next.saturating_sub(1),
            oldest,
            arrival,
        );
        self.evict_below(oldest);
    }

    /// Stamps every id in `[first_id, next_id)` with `arrival`, skipping
    /// any id below `oldest_id` (already evicted from the tracker by the
    /// time this call landed).
    fn stamp_range(&mut self, first_id: u64, next_id: u64, oldest_id: u64, arrival: Instant) {
        let mut id = first_id.max(oldest_id);
        while id < next_id {
            self.map.insert(id, arrival);
            id += 1;
        }
    }

    /// Drops every tracked id below `oldest_id`, mirroring
    /// [`LineIdTracker`]'s own eviction.
    fn evict_below(&mut self, oldest_id: u64) {
        self.map.retain(|id, _| *id >= oldest_id);
    }

    /// Clears every entry. Called when the owning [`LineIdTracker`] resets
    /// (resize) — ids restart from 0 there, so stale entries would
    /// otherwise misattribute an old timestamp to unrelated new content.
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// The arrival time stamped for `id`, or `None` if `id` predates the
    /// map (pre-attach history, which never carries a
    /// [`LineIdTracker`] id at all), was evicted, or is still in progress
    /// (not yet sealed by a completing batch — see module docs).
    pub fn get(&self, id: u64) -> Option<Instant> {
        self.map.get(&id).copied()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

/// The tracker's total-ids-ever-minted counter, derived from its public
/// `oldest_id`/`live_len` getters (`oldest_id + live_len == next_id`; see
/// `LineIdTracker`'s own internal invariant) since it does not expose
/// `next_id` directly.
fn next_id(tracker: &LineIdTracker) -> u64 {
    tracker.oldest_id().unwrap_or(0) + tracker.live_len()
}

#[cfg(test)]
mod tests {
    use super::{next_id, LineTimestamps};
    use crate::LineIdTracker;
    use alacritty_terminal::event::{Event, EventListener};
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::Processor;
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    struct NullListener;
    impl EventListener for NullListener {
        fn send_event(&self, _event: Event) {}
    }

    #[derive(Debug, Clone, Copy)]
    struct Size {
        cols: usize,
        rows: usize,
    }
    impl Dimensions for Size {
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

    /// Drives a real alacritty `Term` through `batches`, calling
    /// [`LineTimestamps::stamp_chunk`] after each one exactly like
    /// `pane_view.rs`'s PTY read loop does (one `advance()` + one
    /// `stamp_chunk` per chunk), each batch carrying its own arrival time.
    fn drive(
        rows: usize,
        cols: usize,
        history_cap: usize,
        batches: &[(&[u8], Instant)],
    ) -> (LineIdTracker, LineTimestamps) {
        let size = Size { cols, rows };
        let config = Config {
            scrolling_history: history_cap,
            ..Config::default()
        };
        let mut term = Term::new(config, &size, NullListener);
        let mut parser: Processor = Processor::new();
        let mut tracker = LineIdTracker::new(history_cap);
        let mut timestamps = LineTimestamps::new();

        for (bytes, arrival) in batches {
            parser.advance(&mut term, bytes);
            let alt_screen = term
                .mode()
                .contains(alacritty_terminal::term::TermMode::ALT_SCREEN);
            let (history_size, screen_lines, cursor_line) = {
                let grid = term.grid();
                (
                    grid.history_size(),
                    grid.screen_lines(),
                    grid.cursor.point.line.0.max(0) as usize,
                )
            };
            timestamps.stamp_chunk(
                &mut tracker,
                alt_screen,
                history_size,
                screen_lines,
                cursor_line,
                bytes,
                *arrival,
            );
        }

        (tracker, timestamps)
    }

    #[::core::prelude::v1::test]
    fn test_stamp_chunk_completed_line_stamped_and_new_current_row_unstamped() {
        let t0 = Instant::now();
        // One batch: a full line, so the "echo hi" row is sealed by the
        // very same batch that produced it (no straddle); the newly
        // opened row 1 (the cursor's fresh row) is not sealed yet.
        let (tracker, timestamps) = drive(24, 80, 100, &[(b"echo hi\n", t0)]);
        let sealed = tracker.id_for_row(0).expect("row 0 should have an id");
        let in_progress = tracker.id_for_row(1).expect("row 1 should have an id");
        assert_eq!(timestamps.get(sealed), Some(t0));
        assert_eq!(timestamps.get(in_progress), None);
    }

    #[::core::prelude::v1::test]
    fn test_stamp_chunk_straddling_line_takes_completing_batch_arrival() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);
        // Batch 1 writes a partial line (no newline yet) at t0; batch 2
        // completes it with a lone newline at t1. The line must carry t1,
        // not t0 - the "completing batch" policy the spec requires.
        let (tracker, timestamps) = drive(24, 80, 100, &[(b"echo hi", t0), (b"\n", t1)]);
        let id = tracker.id_for_row(0).expect("row 0 should have an id");
        assert_eq!(timestamps.get(id), Some(t1));
    }

    #[::core::prelude::v1::test]
    fn test_stamp_chunk_multi_newline_batch_seals_all_interior_rows() {
        let t0 = Instant::now();
        let (tracker, timestamps) = drive(24, 80, 100, &[(b"a\nb\nc\n", t0)]);
        for row in 0..3 {
            let id = tracker.id_for_row(row).unwrap_or_else(|| {
                panic!("row {row} should have an id");
            });
            assert_eq!(timestamps.get(id), Some(t0), "row {row} should be sealed");
        }
        // Row 3 (the new cursor row) is still in progress.
        let id = tracker.id_for_row(3).expect("row 3 should have an id");
        assert_eq!(timestamps.get(id), None);
    }

    #[::core::prelude::v1::test]
    fn test_stamp_chunk_alt_screen_active_skips_tracking_and_stamping() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);
        let size = Size { cols: 80, rows: 24 };
        let config = Config {
            scrolling_history: 100,
            ..Config::default()
        };
        let mut term = Term::new(config, &size, NullListener);
        let mut parser: Processor = Processor::new();
        let mut tracker = LineIdTracker::new(100);
        let mut timestamps = LineTimestamps::new();

        // Seed one sealed primary line at t0.
        parser.advance(&mut term, b"prompt\n");
        {
            let grid = term.grid();
            timestamps.stamp_chunk(
                &mut tracker,
                false,
                grid.history_size(),
                grid.screen_lines(),
                grid.cursor.point.line.0.max(0) as usize,
                b"prompt\n",
                t0,
            );
        }
        let before_next = next_id(&tracker);
        let before_len = timestamps.len();

        // Enter the alternate screen and produce output there; the caller
        // reports `alt_screen = true` (mirroring `TermMode::ALT_SCREEN`
        // after this advance) and the tracker/map must not move at all.
        parser.advance(&mut term, b"\x1b[?1049h\x1b[Hfullscreen ui");
        assert!(term
            .mode()
            .contains(alacritty_terminal::term::TermMode::ALT_SCREEN));
        {
            let grid = term.grid();
            timestamps.stamp_chunk(
                &mut tracker,
                true,
                grid.history_size(),
                grid.screen_lines(),
                grid.cursor.point.line.0.max(0) as usize,
                b"\x1b[?1049h\x1b[Hfullscreen ui",
                t1,
            );
        }

        assert_eq!(next_id(&tracker), before_next);
        assert_eq!(timestamps.len(), before_len);
        let id = tracker
            .id_for_row(0)
            .expect("row 0 should still have an id");
        assert_eq!(timestamps.get(id), Some(t0));
    }

    #[::core::prelude::v1::test]
    fn test_stamp_chunk_bounded_map_evicts_in_lockstep_with_tracker() {
        let t0 = Instant::now();
        let history_cap = 5;
        let rows = 3;
        // Feed far more lines than history_cap + rows can retain.
        let mut batches: Vec<(&[u8], Instant)> = Vec::new();
        for _ in 0..50 {
            batches.push((b"line\n" as &[u8], t0));
        }
        let (tracker, timestamps) = drive(rows, 80, history_cap, &batches);

        let window_cap = history_cap as u64 + rows as u64;
        assert!(
            timestamps.len() as u64 <= window_cap,
            "map grew past the tracker's own bound: {} > {}",
            timestamps.len(),
            window_cap
        );
        // Every id evicted from the tracker must also be gone from the map.
        let oldest = tracker.oldest_id().expect("tracker should be non-empty");
        assert_eq!(timestamps.get(oldest.saturating_sub(1)), None);
    }

    #[::core::prelude::v1::test]
    fn test_clear_drops_every_entry() {
        let t0 = Instant::now();
        let (tracker, mut timestamps) = drive(24, 80, 100, &[(b"a\nb\n", t0)]);
        let id = tracker.id_for_row(0).expect("row 0 should have an id");
        assert_eq!(timestamps.get(id), Some(t0));
        timestamps.clear();
        assert_eq!(timestamps.get(id), None);
        assert_eq!(timestamps.len(), 0);
    }

    #[::core::prelude::v1::test]
    fn test_stamp_range_and_evict_below_bound_map_to_oldest_id() {
        let t0 = Instant::now();
        let mut timestamps = LineTimestamps::new();
        timestamps.stamp_range(0, 5, 0, t0);
        assert_eq!(timestamps.len(), 5);
        timestamps.evict_below(3);
        assert_eq!(timestamps.len(), 2);
        assert_eq!(timestamps.get(2), None);
        assert_eq!(timestamps.get(3), Some(t0));
        assert_eq!(timestamps.get(4), Some(t0));
    }
}
