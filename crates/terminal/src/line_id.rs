//! Monotonic scrollback line identity, inferred without an alacritty
//! callback (`docs/spec-terminal-timestamps.md`).
//!
//! alacritty exposes no "line produced" / "line evicted" event
//! (`EventListener` only surfaces `ClipboardStore`/`Bell`, and `Term::damage()`
//! is per-frame viewport diffing with no stable identity or eviction signal).
//! [`LineIdTracker`] reconstructs both from the two numbers alacritty's grid
//! *does* expose after every `advance()`: `history_size()` and the cursor's
//! grid line.
//!
//! Every row currently addressable — from the oldest retained history line
//! (`Line(-history_size)`) through the cursor's own line — occupies one slot
//! of an ever-growing, born-in-order tape; `history_size() + cursor_line + 1`
//! is the tape's current window size. Ids are handed out in tape order, so
//! the window collapses to two counters, `oldest_id` and `next_id`; a row's
//! id is `oldest_id + absolute_row` (`absolute_row` 0 at the oldest retained
//! row, the same addressing `pane_view::extract_row_cells` already uses).
//!
//! **Spike finding — the saturation wall:** once `history_size()` reaches
//! alacritty's scrollback cap, further scrolling rotates its ring buffer in
//! place and `history_size()` stays flat; if the cursor is also pinned at
//! the last screen line (continuous output, no scroll region — the common
//! case), `cursor_line` is flat too. Verified empirically against a real
//! `Term`: in that steady state the two getters carry **no further growth
//! signal at all**, even though alacritty keeps evicting/birthing a line per
//! scrolled row.
//!
//! The initial version of this module only applied a `\n`-count fallback
//! when the *whole* batch produced zero window growth, on the assumption
//! that a batch that grows the window at all must be precise. That
//! assumption is wrong: a single batch that *crosses* the cap partway
//! through both grows `history_size()` (so it takes the precise,
//! window-based path) **and** keeps scrolling past the cap in the same
//! `advance()` call, silently rotating away rows with no further getter
//! signal. Those extra births were never minted, so `next_id`
//! permanently under-counted, the step-3 hard cap never advanced
//! `oldest_id` to compensate, and `id_for_row` went on returning a stale id
//! for a row alacritty had already evicted — a real, non-recovering
//! row→id desync, reachable any time one coalesced `%output` batch (a build
//! log, a large `cat`) exceeds the scrollback room remaining at the moment
//! it lands. **The fix:** compute both signals unconditionally whenever a
//! batch ends saturated — `window_growth` (the precise, capped delta) and a
//! structural `\n`-byte count (a C0 control byte, never a UTF-8
//! continuation/CSI parameter byte, mirroring the existing `\n`-count in
//! `parse_capture_to_rows`) — and take their maximum. Neither signal ever
//! *overcounts* the true birth count (`window_growth` only undercounts past
//! the cap; the `\n` count only undercounts wrap-only rows with no trailing
//! LF), so the maximum is the best available lower bound and is exact for
//! the overwhelmingly common case of newline-terminated output, including
//! batches that straddle the cap. It still undercounts (never leaks — the
//! hard cap in step 3 of [`LineIdTracker::update`] applies every call) only
//! for an auto-wrapped, non-newline-terminated line completing while
//! already saturated.
//!
//! **Eviction vs. a harmless cursor move:** `history_size()` only ever
//! *decreases* via an explicit scrollback purge (`ESC[3J`) or a full reset
//! (`RIS`) — never as a side effect of ordinary scrolling. A `cursor_line`
//! decrease alone (screen clear + home, or a cursor-up escape) is not: the
//! row is still there and gets overwritten, reusing its id. `update`
//! therefore keys eviction off a `history_size()` decrease only.
//!
//! **Scope:** resize is a separate event with its own ambiguity (alacritty
//! can shrink `history_size()` by shifting a row into a growing viewport
//! with no eviction at all, or by evicting it outright — indistinguishable
//! from the getters alone). Callers reset the tracker on resize instead,
//! mirroring the existing invalidate-on-resize pattern for
//! `PaneView::history_block` / `paint_cache`.

/// Assigns a monotonic synthetic id to every row addressable in an
/// alacritty grid (scrollback + on-screen, up to and including the cursor's
/// row), and evicts ids in lockstep once the tracked window exceeds
/// alacritty's own retained-row capacity. See the module docs for the
/// inference strategy.
#[derive(Debug, Clone)]
pub struct LineIdTracker {
    /// alacritty's configured scrollback cap (`Config::scrolling_history`
    /// the `Term` was constructed with).
    history_cap: usize,
    /// One past the most recently minted id; the total count of ids ever
    /// handed out.
    next_id: u64,
    /// The oldest id still considered live; ids below this have been
    /// evicted.
    oldest_id: u64,
    /// `history_size()` as observed on the previous `update` call, used to
    /// detect an explicit scrollback purge (the only way `history_size()`
    /// legitimately decreases outside of a resize).
    last_history_size: usize,
}

impl LineIdTracker {
    /// `history_cap` must match the `scrolling_history` the owning `Term`
    /// was constructed with — it is the only external input this tracker
    /// needs (everything else is read back from the grid each `update`).
    pub fn new(history_cap: usize) -> Self {
        Self {
            history_cap,
            next_id: 0,
            oldest_id: 0,
            last_history_size: 0,
        }
    }

    /// Resets to a fresh, empty tracker. Callers invoke this on resize
    /// rather than feeding resize state through [`Self::update`] — see the
    /// module docs for why resize is deliberately out of scope here.
    pub fn reset(&mut self) {
        self.next_id = 0;
        self.oldest_id = 0;
        self.last_history_size = 0;
    }

    /// Samples the grid state after one `advance()` call (or chunk thereof)
    /// and updates the tracked id window.
    ///
    /// - `history_size` / `screen_lines`: `term.grid().history_size()` /
    ///   `term.grid().screen_lines()` (or `Dimensions::screen_lines`), read
    ///   right after the `advance()` call this update follows.
    /// - `cursor_line`: `term.grid().cursor.point.line.0.max(0) as usize` —
    ///   the cursor's row is always non-negative (it is always within the
    ///   viewport), mirroring the cast `pane_view::parse_capture_to_rows`
    ///   already performs.
    /// - `bytes_fed`: exactly the byte slice handed to the `advance()` call
    ///   this update follows (the OSC-filtered bytes, matching
    ///   `pane_view.rs`'s PTY read loop) — consulted only as a saturation
    ///   fallback, see the module docs.
    pub fn update(
        &mut self,
        history_size: usize,
        screen_lines: usize,
        cursor_line: usize,
        bytes_fed: &[u8],
    ) {
        // 1. Explicit purge: history_size only shrinks via an explicit
        //    scrollback clear (ESC[3J) or a full reset (RIS), never as a
        //    side effect of ordinary scrolling. Evict exactly what alacritty
        //    just dropped.
        if history_size < self.last_history_size {
            let dropped = (self.last_history_size - history_size) as u64;
            self.oldest_id = self.oldest_id.saturating_add(dropped).min(self.next_id);
        }
        self.last_history_size = history_size;

        // 2. Growth: how many rows are newly addressable relative to the
        //    window we currently track.
        let target_window = history_size as u64 + cursor_line as u64 + 1;
        let current_window = self.next_id - self.oldest_id;

        // Precise path: while there is still room below the cap,
        // history_size/cursor_line diffs exactly account for every
        // newly-produced row (plain newlines, wraps, and the common
        // screen-clear-then-retype case all take this path). This alone is
        // NOT a safe upper bound once a single batch also *crosses* the cap
        // (see "The saturation wall" in the module docs): history_size()
        // stops growing partway through the batch, silently rotating away
        // any further scrolls, so `window_growth` alone undercounts by
        // exactly the number of rows lost after saturation was reached.
        let window_growth = target_window.saturating_sub(current_window);

        let growth = if history_size >= self.history_cap {
            // Saturated by the end of this batch - either it was already
            // flat (the pre-existing single-line-at-a-time case, where
            // `window_growth` is 0) or it crossed the cap mid-batch (where
            // `window_growth` undercounts). `\n` (a C0 control byte, never
            // a UTF-8 continuation or CSI parameter byte - structural, not
            // content interpretation, mirroring the existing `\n`-count in
            // `parse_capture_to_rows`) completes exactly one row per
            // occurrence regardless of saturation, so it is an independent,
            // exact lower bound for newline-driven output. Neither signal
            // ever overcounts the true birth count, so the combined,
            // correct growth is their maximum - this is what fixes the
            // permanent row-id desync a saturation-crossing batch used to
            // cause (next_id under-advanced, so the step-3 cap below never
            // caught up and stale ids got reused for new content).
            let newline_count = bytes_fed.iter().filter(|byte| **byte == b'\n').count() as u64;
            window_growth.max(newline_count)
        } else {
            // Below the cap, `window_growth` is exact on its own: no batch
            // this call could possibly have overflowed silently.
            window_growth
        };
        self.next_id += growth;

        // 3. Hard cap, applied unconditionally: never retain more ids than
        //    alacritty itself retains rows (its history cap plus whatever
        //    is currently on screen). This is the safety net for the
        //    saturation fallback above and the sole source of truth for
        //    "no leak".
        let window_cap = self.history_cap as u64 + screen_lines as u64;
        if self.next_id - self.oldest_id > window_cap {
            self.oldest_id = self.next_id - window_cap;
        }
    }

    /// The id of the row at `absolute_row` (0 = the oldest row currently
    /// retained, i.e. `Line(-history_size)`; `history_size` = the first
    /// on-screen row `Line(0)`; `history_size + cursor_line` = the cursor's
    /// row) — the same addressing `pane_view::extract_row_cells` uses.
    /// `None` if the row is outside the currently-tracked window (already
    /// evicted, or not yet born).
    pub fn id_for_row(&self, absolute_row: usize) -> Option<u64> {
        let id = self.oldest_id.checked_add(absolute_row as u64)?;
        (id < self.next_id).then_some(id)
    }

    /// The id of the most recently born row (the cursor's row), or `None`
    /// before the first [`Self::update`] call.
    pub fn newest_id(&self) -> Option<u64> {
        (self.next_id > self.oldest_id).then(|| self.next_id - 1)
    }

    /// The id of the oldest still-tracked row, or `None` if nothing has
    /// been tracked yet.
    pub fn oldest_id(&self) -> Option<u64> {
        (self.next_id > self.oldest_id).then_some(self.oldest_id)
    }

    /// The number of ids currently retained (bounded to `history_cap +
    /// screen_lines` after every `update`).
    pub fn live_len(&self) -> u64 {
        self.next_id - self.oldest_id
    }
}

#[cfg(test)]
mod tests {
    use super::LineIdTracker;
    use alacritty_terminal::event::{Event, EventListener};
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::Line;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::Processor;

    #[derive(Clone)]
    struct NullListener;
    impl EventListener for NullListener {
        fn send_event(&self, _event: Event) {}
    }

    /// Minimal `Dimensions` impl for building scratch `Term`s in tests,
    /// mirroring `crate::TermSize`.
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

    /// Drives a real alacritty `Term` with `batches`, calling `update` after
    /// each one exactly like `pane_view.rs`'s PTY read loop does, and
    /// returns the tracker plus the final grid snapshot (history_size,
    /// screen_lines, cursor_line) for assertions.
    fn drive(
        rows: usize,
        cols: usize,
        history_cap: usize,
        batches: &[&[u8]],
    ) -> (LineIdTracker, Term<NullListener>) {
        let size = Size { cols, rows };
        let config = Config {
            scrolling_history: history_cap,
            ..Config::default()
        };
        let mut term = Term::new(config, &size, NullListener);
        let mut parser: Processor = Processor::new();
        let mut tracker = LineIdTracker::new(history_cap);

        for batch in batches {
            parser.advance(&mut term, batch);
            let grid = term.grid();
            tracker.update(
                grid.history_size(),
                grid.screen_lines(),
                grid.cursor.point.line.0.max(0) as usize,
                batch,
            );
        }

        (tracker, term)
    }

    /// Reads back the text of the row at `absolute_row` (same addressing as
    /// [`LineIdTracker::id_for_row`]), trimmed of trailing blank cells - the
    /// content-encoded oracle the saturation-crossing test uses to verify
    /// `id_for_row` against ground truth instead of hand-derived numbers.
    fn row_text(term: &Term<NullListener>, absolute_row: usize) -> String {
        let history_size = term.grid().history_size() as i32;
        let line = Line(absolute_row as i32 - history_size);
        let row = &term.grid()[line];
        let text: String = row.into_iter().map(|cell| cell.c).collect();
        text.trim_end().to_string()
    }

    #[::core::prelude::v1::test]
    fn test_new_tracker_starts_empty() {
        let tracker = LineIdTracker::new(1000);
        assert_eq!(tracker.live_len(), 0);
        assert_eq!(tracker.oldest_id(), None);
        assert_eq!(tracker.newest_id(), None);
        assert_eq!(tracker.id_for_row(0), None);
    }

    #[::core::prelude::v1::test]
    fn test_plain_newlines_ids_increase_monotonically_and_stay_stable() {
        let (tracker, term) = drive(
            5,
            10,
            100,
            &[b"aaa\r\n", b"bbb\r\n", b"ccc\r\n", b"ddd\r\n"],
        );
        // 4 newlines from an empty screen: cursor lands on row 4 (0-indexed),
        // nothing scrolled into history yet (rows fit in the 5-row screen).
        assert_eq!(term.grid().history_size(), 0);
        assert_eq!(term.grid().cursor.point.line.0, 4);
        // ids 0..=4 minted: one per completed line (0..3) plus the cursor's
        // own (not yet written) row 4.
        assert_eq!(tracker.live_len(), 5);
        assert_eq!(tracker.newest_id(), Some(4));
        // Row addressing: absolute_row 0 is "aaa", row 3 is "ddd".
        assert_eq!(tracker.id_for_row(0), Some(0));
        assert_eq!(tracker.id_for_row(3), Some(3));

        // Feed one more line, pushing "aaa" (row 0) into history via a
        // scroll. Its id must be unchanged even though its address shifted
        // from viewport row 0 to history row -1 (absolute_row 0 still).
        let (tracker2, term2) = drive(
            5,
            10,
            100,
            &[b"aaa\r\n", b"bbb\r\n", b"ccc\r\n", b"ddd\r\n", b"eee\r\n"],
        );
        assert_eq!(term2.grid().history_size(), 1);
        assert_eq!(
            tracker2.id_for_row(0),
            Some(0),
            "the scrolled line keeps its id"
        );
        assert_eq!(tracker2.newest_id(), Some(5));
    }

    #[::core::prelude::v1::test]
    fn test_wrapped_long_line_mints_one_id_per_wrapped_row() {
        // 10 columns; write 25 chars with no newline -> "0123456789" wraps,
        // "ABCDEFGHIJ" wraps, "KLMNO" stays partial on the cursor's row.
        let (tracker, term) = drive(5, 10, 100, &[b"0123456789ABCDEFGHIJKLMNO"]);
        assert_eq!(term.grid().history_size(), 0);
        assert_eq!(term.grid().cursor.point.line.0, 2);
        // Rows born: "0123456789", "ABCDEFGHIJ", and the partial "KLMNO..."
        // row the cursor now sits on - 3 ids total.
        assert_eq!(tracker.live_len(), 3);
        assert_eq!(tracker.newest_id(), Some(2));
    }

    #[::core::prelude::v1::test]
    fn test_partial_line_completing_in_a_later_batch_keeps_same_id() {
        // Wide enough (20 cols) that "partial line" (12 chars) never wraps,
        // isolating the straddle behavior from the wrap behavior.
        let (tracker_before, term_before) = drive(5, 20, 100, &[b"partial"]);
        let id_before = tracker_before.newest_id();
        assert_eq!(term_before.grid().cursor.point.line.0, 0);

        let (tracker_after, term_after) = drive(5, 20, 100, &[b"partial", b" line\r\n"]);
        assert_eq!(term_after.grid().cursor.point.line.0, 1);
        // Completing the line moved the cursor to a *new* row, but the
        // partial row itself (absolute_row 0) must still resolve to the
        // same id it was assigned when the first, incomplete batch arrived.
        assert_eq!(tracker_after.id_for_row(0), id_before);
    }

    #[::core::prelude::v1::test]
    fn test_screen_clear_keeps_ids_monotonic_and_bounded() {
        let (tracker, term) = drive(
            5,
            10,
            100,
            &[
                b"aaa\r\nbbb\r\nccc\r\nddd\r\n",
                b"\x1b[H\x1b[2J", // ED2 (erase display) + cursor home
                b"post-clear\r\n",
            ],
        );
        assert_eq!(term.grid().cursor.point.line.0, 1);
        // No panic, ids stay bounded to what alacritty actually retains.
        let window_cap = 100 + term.grid().screen_lines() as u64;
        assert!(tracker.live_len() <= window_cap);
        assert!(tracker.newest_id().is_some());
    }

    #[::core::prelude::v1::test]
    fn test_scrollback_clear_evicts_ids_immediately() {
        let (tracker, term) = drive(
            3,
            10,
            100,
            &[b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\n", b"\x1b[3J"],
        );
        assert_eq!(term.grid().history_size(), 0, "ESC[3J purges scrollback");
        // Everything that used to be in history is gone; only what remains
        // addressable on screen (screen_lines rows) stays live.
        assert_eq!(tracker.live_len(), term.grid().screen_lines() as u64);
    }

    #[::core::prelude::v1::test]
    fn test_eviction_bounded_once_history_full_no_leak() {
        let history_cap = 20;
        let rows = 3;
        let mut batches = Vec::new();
        for i in 0..500 {
            batches.push(format!("line {i}\r\n"));
        }
        let batch_refs: Vec<&[u8]> = batches.iter().map(|s| s.as_bytes()).collect();
        let (tracker, term) = drive(rows, 10, history_cap, &batch_refs);

        let window_cap = history_cap as u64 + rows as u64;
        assert_eq!(term.grid().history_size(), history_cap, "history saturated");
        // The saturation fallback must still be minting/evicting ids in
        // lockstep - not merely freezing once the grid getters flatline.
        assert_eq!(
            tracker.live_len(),
            window_cap,
            "window stays exactly at the bound, no leak"
        );
        assert!(
            tracker.newest_id().unwrap_or(0) > window_cap,
            "ids kept advancing past saturation instead of freezing"
        );
    }

    /// The bug this test was written to catch: a *single* batch that both
    /// grows history_size() toward the cap AND keeps scrolling past it in
    /// the same `advance()` call. The window-delta signal alone accounts
    /// for only the room-filling portion; the rest must come from the
    /// `\n`-count fallback combined into the *same* call, not picked
    /// instead of it. Verified against a real `Term`, using each line's own
    /// embedded birth index as ground truth rather than hand-derived ids.
    #[::core::prelude::v1::test]
    fn test_saturation_crossing_batch_counts_every_birth_and_reconverges() {
        let rows = 3;
        let cols = 20;
        let cap = 20;
        let window_cap = cap as u64 + rows as u64;

        // Below the cap, so growth here is precise by construction; gives a
        // known birth-id offset for the S-labeled lines that follow,
        // sidestepping the fresh-tracker's-first-call degenerate case.
        let baseline: Vec<u8> = (0..5)
            .flat_map(|n| format!("B{n}\r\n").into_bytes())
            .collect();
        let (baseline_tracker, _) = drive(rows, cols, cap, &[&baseline]);
        // The baseline's newest id is a pending, not-yet-written row (the
        // cursor's current position) - the straddle batch's first line
        // (`S0`) is written into that SAME row, not a fresh one, so it is
        // the birth id for "S0" (not "S0"'s id plus one).
        let birth_offset = baseline_tracker.newest_id().expect("baseline minted ids");

        // ONE batch, 40 newlines: only 17 lines of room remain after the
        // baseline (cap 20 - history 3), so this straddles the cap
        // partway through the same `advance()` call.
        let straddle_count = 40u64;
        let straddle: Vec<u8> = (0..straddle_count)
            .flat_map(|n| format!("S{n}\r\n").into_bytes())
            .collect();
        let (tracker, term) = drive(rows, cols, cap, &[&baseline, &straddle]);

        assert_eq!(
            term.grid().history_size(),
            cap,
            "history saturated mid-batch"
        );
        // All 40 births must be counted, not just the ones that grew
        // history_size() before it hit the wall - the newest id is the
        // pending row after "S39" (birth_offset + one id per newline).
        assert_eq!(tracker.newest_id(), Some(birth_offset + straddle_count));
        assert_eq!(
            tracker.live_len(),
            window_cap,
            "window fills exactly to the bound, no leak"
        );

        // Content-encoded oracle: every retained, written row's own embedded
        // birth index must match what id_for_row reports for it - not
        // merely a count, but a per-row correctness check that catches
        // silent reuse/aliasing. The single newest row (id `birth_offset +
        // straddle_count`) is the pending row after "S39" - not yet written
        // to, so it carries no label and is skipped.
        let mut checked = 0;
        for row in 0..window_cap as usize {
            let label = row_text(&term, row);
            let Some(n) = label.strip_prefix('S').and_then(|s| s.parse::<u64>().ok()) else {
                continue;
            };
            assert_eq!(
                tracker.id_for_row(row),
                Some(birth_offset + n),
                "row {row} (\"{label}\") must resolve to its true birth id"
            );
            checked += 1;
        }
        assert_eq!(
            checked,
            window_cap as usize - 1,
            "every retained row except the single pending (unwritten) one is S-labeled"
        );

        // Re-convergence: a still-live row must keep the SAME id as more
        // ordinary lines push it further through the (still-saturated)
        // window - row -> id stability across continued scroll, not just a
        // one-time snapshot right after the straddle.
        let id_before = tracker.id_for_row(10);
        let label_before = row_text(&term, 10);

        let more: Vec<u8> = (40..43u64)
            .flat_map(|n| format!("S{n}\r\n").into_bytes())
            .collect();
        let (tracker2, term2) = drive(rows, cols, cap, &[&baseline, &straddle, &more]);
        assert_eq!(
            tracker2.live_len(),
            window_cap,
            "still bounded after reconverging"
        );
        // 3 more saturated lines shift every live row back by exactly 3.
        assert_eq!(
            tracker2.id_for_row(7),
            id_before,
            "the same content keeps its id as it scrolls further"
        );
        assert_eq!(row_text(&term2, 7), label_before);
    }

    /// Formula-level companion to the real-`Term` straddling test above:
    /// pins the exact arithmetic for a single `update()` call that both
    /// consumes remaining room and scrolls past the cap, independent of
    /// alacritty's own wrap/scroll-region behavior.
    #[::core::prelude::v1::test]
    fn test_update_mixed_growth_and_saturation_counts_every_birth_in_one_call() {
        let history_cap = 10;
        let screen_lines = 5;
        let mut tracker = LineIdTracker::new(history_cap);

        // Baseline below the cap: window size 8, 7 lines of room left.
        tracker.update(3, screen_lines, 4, b"");
        assert_eq!(tracker.live_len(), 8);

        // 12 newlines fed in one call; only 7 fit before history_size()
        // saturates at the cap, so the window-delta alone would report 7.
        let batch = b"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n";
        tracker.update(history_cap, screen_lines, 4, batch);

        assert_eq!(
            tracker.live_len(),
            history_cap as u64 + screen_lines as u64,
            "window fills exactly to the bound, no leak"
        );
        assert_eq!(
            tracker.newest_id(),
            Some(8 + 12 - 1),
            "all 12 births counted, not just the 7 that grew history_size()"
        );
    }

    #[::core::prelude::v1::test]
    fn test_id_for_row_outside_window_returns_none() {
        let (tracker, _term) = drive(5, 10, 100, &[b"aaa\r\nbbb\r\n"]);
        assert_eq!(tracker.id_for_row(1_000_000), None);
    }

    #[::core::prelude::v1::test]
    fn test_reset_returns_tracker_to_empty_state() {
        let mut tracker = LineIdTracker::new(100);
        tracker.update(0, 5, 3, b"abcd\r\n");
        assert!(tracker.live_len() > 0);
        tracker.reset();
        assert_eq!(tracker.live_len(), 0);
        assert_eq!(tracker.oldest_id(), None);
    }

    #[::core::prelude::v1::test]
    fn test_update_benign_cursor_decrease_does_not_evict() {
        let mut tracker = LineIdTracker::new(100);
        tracker.update(0, 24, 10, b""); // grow to window size 11 (ids 0..10)
        assert_eq!(tracker.live_len(), 11);
        // Cursor moved up (e.g. a cursor-up escape) with history unchanged:
        // must not evict - the rows are still there, just not at the peak.
        tracker.update(0, 24, 2, b"");
        assert_eq!(
            tracker.live_len(),
            11,
            "no eviction from a cursor-only decrease"
        );
        assert_eq!(tracker.newest_id(), Some(10));
    }

    #[::core::prelude::v1::test]
    fn test_update_malformed_empty_batch_is_a_no_op() {
        let mut tracker = LineIdTracker::new(100);
        tracker.update(0, 24, 0, b"");
        assert_eq!(tracker.live_len(), 1);
        tracker.update(0, 24, 0, b"");
        assert_eq!(
            tracker.live_len(),
            1,
            "re-sampling identical state mints nothing"
        );
    }
}
