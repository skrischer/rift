# Spec: Terminal output timestamps (on-demand)

> Created: 2026-07-30

Stamp PTY-byte arrival time per scrollback line at the pane and surface it
on demand, agent-agnostic (no output parsing). Roadmap Phase 50.

## Outcome

- [ ] Each live scrollback line the client witnesses since attach carries the wall-clock arrival time of the output that produced it, held in a rift-owned per-pane structure (not in alacritty's cells).
- [ ] The timestamp is surfaced on demand at the pane (per line), not once per window — the differentiator over iTerm2-under-tmux, which sees the multiplexer as one command and can only stamp once per window.
- [ ] The surface (chosen at the spec-acceptance gate) reveals a line's arrival time without parsing any output — the time comes purely from when the `%output` bytes arrived at the client.
- [ ] Timestamps are suppressed on the alternate screen (full-screen TUIs) and absent for the pre-attach `capture-pane` history block (which predates the client's attach); those rows show no timestamp rather than a wrong one.
- [ ] The per-pane timestamp structure is bounded to the scrollback history limit and evicts in lockstep with alacritty's ring buffer — no unbounded growth over a long session.

## Scope

### In scope

- Capture the arrival wall-clock time at the per-pane PTY read boundary (`crates/terminal/src/pane_view.rs:329` batch receive → `:347` `p.advance`), and associate it with the live scrollback line(s) that batch produces.
- A rift-owned monotonic line-identity scheme: a per-pane "total lines ever scrolled into history" counter, reconstructed by diffing `history_size()` / cursor line before vs. after each `advance` (alacritty exposes no line-produced or eviction callback). Timestamps key off this synthetic id and are translated to a viewport row at render/hover time by inverting the composite-scroll mapping (`pane_view.rs:1561-1608`).
- Lockstep eviction: drop timestamp entries for lines alacritty silently evicted, inferred from the same counter / `history_size` diff, bounded to the alacritty history limit.
- The v1 on-demand surface (gate-resolved): a **per-line hover reveal** — reusing `pixel_to_grid` (`:887`) + `on_mouse_move` (`:1817`) to find the row under the mouse, invert the composite mapping to the logical live line, and reveal that line's arrival time. The toggle gutter and idle-marker surfaces are deferred follow-ups that reuse the same backend.
- Alt-screen gating (`term.mode().contains(TermMode::ALT_SCREEN)`, `pane_view.rs:715`) so timestamps never show over a full-screen TUI; the primary-scrollback map persists across alt-screen toggles.
- Build the composite-scroll row→line inversion inline for v1. The Phase-49 scrollback scrollbar reads the same composite offset, so a shared helper is the eventual home — but factor it out only when that second consumer actually lands (Phase 49's issues are unmerged), not speculatively now (constitution: no premature abstraction, extract at 2+ implementations).

### Out of scope

- Timestamps for the pre-attach `capture-pane` history block — those lines predate attach and carry no arrival time; tmux's capture does not provide per-line time. The feature is inherently "timestamps for output witnessed since attach."
- Any output parsing / content interpretation to derive time — strictly forbidden (constitution: no parsing of agent output). The only time source is byte-arrival.
- Transport-lag correction via the tmux `%extended-output <age>` token (parsed and dropped today at `crates/tmux-core/src/parser.rs:71-74`). v1 stamps pure client-arrival time; subtracting the age to recover the host emit time is a possible later refinement, not this phase.
- Persisting timestamps across restarts — the map is in-memory, live-session only.
- Sub-millisecond precision or a configurable time format beyond a sensible default.
- Building more than one surface. The gate picks the v1 surface; the others are deferred follow-ups (the timestamp backend is shared, so a later surface reuses it).

## Constraints

- **The stamping boundary is the client PTY read loop, per pane** (`pane_view.rs:329`/`:347`) — this is rift's differentiator: it sits at the `%output`-per-pane seam, where iTerm2-under-tmux cannot. Stamp at batch receive.
- **No per-line metadata channel exists.** `CellRenderInfo` (the render unit, `pane_view.rs:1374`) has no timestamp field and is owned by the external `termy_terminal_ui` crate — timestamps must live in a parallel rift-owned per-pane structure keyed by the synthetic line id, never attached to a cell. Do NOT reuse the OSC-133 `CommandLifecycle` channel (`:251`) — that is shell-integration-driven and would couple to output semantics.
- **alacritty gives no line-produced or eviction event.** The `EventListener` surfaces only `ClipboardStore` / `Bell` (`handle_term_event`, `:975-986`). (`Term::damage()` / `TermDamage` is viewport render-diffing — it resets per frame and carries no stable scrollback identity or eviction signal, so it is not a usable anchor either.) Line identity and eviction must be inferred from `history_size()` / cursor-line diffs across each `advance` — the single hardest problem, proven by a spike before the surface is built.
- **Line addressing is viewport-relative** (`Line(row - display_offset)`); a given text's address shifts as new lines scroll in. The synthetic monotonic id is the only stable anchor; it is inverted to a viewport row at render time via the composite mapping.
- **Stamp policy:** a line is stamped with the arrival time of the batch during whose `advance` the line was completed (a line straddling two batches takes the completing batch's time). Bounded, deterministic; documented so hover/gutter reads are consistent.
- Agent-agnostic, client-only: no daemon, protocol, or `crates/protocol` change — the byte-arrival signal is already client-side. No new dependency (wall-clock via the existing time facility).

## Prior art

- [QA-seeded phases — prior-art index (Phases 48–56)](prior-art.md#qa-seeded-phases--prior-art-index-phases-4856) — the Phase 50 row: adopt iTerm2's View > Show Timestamps toggle + per-line-stamp semantics; the differentiation is that iTerm2 and peers stamp only once per window under tmux (they see the multiplexer as a single command), while rift is the control-mode client receiving `%output` per pane and stamps PTY-byte arrival per scrollback line at the pane. Demand is confirmed across Warp / Waveterm / Windows Terminal / Ghostty / Tabby feature requests (linked in the row).

## Human prerequisites

- none — the feature is entirely client-side over the byte-arrival signal the app already receives.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Timestamps live in a rift-owned per-pane parallel structure keyed by a synthetic monotonic line id, not on cells | `CellRenderInfo` has no timestamp field and is external; alacritty addresses lines viewport-relative with no stable id | 2026-07-30 |
| Line identity + eviction inferred from `history_size()` / cursor diffs across each `advance` (no alacritty callback exists); proven by a spike before the surface | It is the only viable anchor; the scoping named it the hardest problem, so it is de-risked first | 2026-07-30 |
| Coverage is the live region only; the pre-attach `capture-pane` block shows no timestamp | Those lines predate attach and have no arrival time; a wrong time is worse than none | 2026-07-30 |
| v1 stamps pure client-arrival time; the tmux `%extended-output <age>` lag-correction is deferred | Simplicity; age-correction is an accuracy refinement, not core; the token is already parsed so a later phase can add it | 2026-07-30 |
| Stamp a line with the arrival time of the batch that completed it (straddle → completing batch) | Deterministic, bounded, one clear policy for consistent hover/gutter reads | 2026-07-30 |
| Client-only; no protocol / daemon change | Byte-arrival is already a client signal; the constitution admits PTY-byte timing as agent-agnostic | 2026-07-30 |
| Build the composite row→line inversion inline for v1; factor a shared helper only when the Phase-49 scrollbar lands as the second consumer | Phase 49's issues are unmerged; a shared helper now would be single-use (constitution: extract at 2+ implementations) | 2026-07-30 |
| The v1 on-demand surface is a **per-line hover reveal** — the arrival time appears only for the scrollback line under the mouse; the toggle gutter and idle-markers are deferred follow-ups reusing the shared backend | Accepted at the gate. Cheapest v1, purest "on demand", reuses the existing `pixel_to_grid` + `on_mouse_move` hover hit-testing, zero clutter | 2026-08-01 |

## Tracking

- Milestone: created at the spec-acceptance gate
- Issues: created from this spec once merged — a **spike** (prove line-id + eviction inference against alacritty's grid), the **timestamp backend** (stamping at the PTY boundary + the bounded per-pane map + lockstep eviction), and the **chosen surface** (hover / gutter / idle-marker + the row→line lookup). Dependency edges in the issue bodies (surface depends on the backend, backend depends on the spike).

## Verification

- [ ] `just ci` equivalent green for terminal / app (fmt + clippy `-D warnings` + tests); the crates compile warm-target clean.
- [ ] Unit test: feeding batches with known arrival times and newline patterns yields the expected per-line timestamps; a line straddling two batches takes the completing batch's time.
- [ ] Unit test: the per-pane timestamp map is bounded to the history limit and evicts oldest entries in lockstep as new lines scroll in (no unbounded growth).
- [ ] QA: the chosen surface reveals a plausible arrival time per live line at the pane; two panes emitting at different times show different per-line stamps (the per-pane differentiator).
- [ ] QA: alt-screen (open `htop` / `vim`) shows no timestamps; on exit the primary scrollback still has its stamps.
- [ ] QA: scrolled-back pre-attach history rows show no timestamp (not a wrong one).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Line-identity inference from `history_size`/cursor diffs is wrong under wraps / clears / partial lines | De-risk with the spike first; unit-test batch→line mapping with wrap/clear/partial fixtures before building any surface |
| The parallel map leaks because eviction has no signal | Bound to the history limit and evict from the same counter diff; a QA item asserts bounded memory over a long session |
| Timestamps drift the render hot path | The map lookup is O(1) per visible row at render/hover; stamping is a cheap diff per batch on the existing blocking advance thread |
| Surface collides with the Phase-49 scrollbar overlay in the same margin | Share the composite mapping helper; coordinate the gutter/scrollbar layout if both land (QA the terminal with both) |
| Wall-clock unavailable in the render/advance context | Capture arrival time at the PTY read boundary (`:329`), not at render; the read loop is a normal async context |

## Decision log

- 2026-07-30: Scoped from a `crates/terminal/pane_view.rs` + control-mode read (Explore agent). The stamping boundary is the per-pane PTY read loop (`:329`/`:347`); no per-line metadata or eviction callback exists, so timestamps need a rift-owned monotonic line-id inferred from `history_size`/cursor diffs, covering the live region only (the pre-attach capture block predates attach). Client-only, no protocol change. The single open item — the on-demand surface (hover / gutter / idle-marker) — carried to the gate; the hard line-identity problem is de-risked by a spike before the surface.
- 2026-08-01: Spec-acceptance gate (PR #931) — accepted. Review returned APPROVE (verified the stamping boundary, the absence of any per-line/eviction callback including `Term::damage()`, alt-screen gating, and the pre-attach block's un-stampability; no simpler approach missed) with non-blocking nits, addressed: the Phase-49 row→line helper is built inline for v1 (factored only when the scrollbar lands), and a note that `damage()` is unusable was added. Gate decision: the v1 surface is a **per-line hover reveal**; the toggle gutter and idle-markers are deferred follow-ups on the shared backend.
- 2026-08-06: Spike (#932, `crates/terminal/src/line_id.rs`) confirms the approach with one adjustment. `history_size() + cursor_line` is exact and precise for id assignment right up to the point history saturates — verified against a real `Term`: once `history_size()` reaches alacritty's scrollback cap **and** the cursor is pinned at the last screen line (continuous output, the common no-scroll-region case), *both* getters go flat and carry zero further growth signal, even though alacritty is still silently evicting/birthing one line per scrolled row. No combination of `Term`'s public getters observes this. Adjustment: once saturated, fall back to a structural `\n`-byte count of the batch that produced the update (mirrors the existing `\n`-counting in `pane_view::parse_capture_to_rows`), combined via `max()` with the precise window-delta signal rather than picked instead of it — exact for newline-terminated output, and only *undercounts* (never leaks; the hard window cap still applies every call) for an auto-wrapped, non-newline-terminated line completing while already saturated. Eviction is keyed specifically off a `history_size()` decrease (unambiguous: only an explicit scrollback purge `ESC[3J` or a full reset causes it) rather than off a `cursor_line` decrease, which is often benign (a plain clear + cursor-home, or a cursor-up escape) and must not evict a row that is still there. Resize is left out of `LineIdTracker::update` entirely — alacritty's resize can shrink `history_size()` either by reclaiming a row into a growing viewport (no eviction) or by evicting it outright, indistinguishable from the getters alone — callers reset the tracker on resize instead, mirroring the existing invalidate-on-resize pattern for `PaneView::history_block`/`paint_cache`. All four required scenarios (plain newlines, wrapped long lines, screen clears, partial lines straddling batches) plus bounded post-saturation eviction are unit-tested against a real `alacritty_terminal::Term`.
- 2026-08-06: Adversarial review of PR #976 found the first cut of the saturation adjustment above was itself broken: it picked *either* the precise window-delta *or* the `\n`-fallback (mutually exclusive branches), so a single batch that both consumed remaining scrollback room and scrolled further past the cap in the same `advance()` call — reachable any time one coalesced `%output` batch (a build log, a large `cat`) exceeds the room remaining at the moment it lands, not an exotic edge case — took the precise branch (since it did grow the window) and silently dropped every birth past the cap. `next_id` permanently under-counted, the hard cap never caught up, and `id_for_row` went on returning stale ids reused for different, newer content: a real, non-recovering row→id desync, not the narrow auto-wrap-only undercount the first write-up claimed. Fixed by combining both signals unconditionally (`max`, not `if`/`else if`) whenever a batch ends saturated, so every birth is counted regardless of when in the batch the cap was crossed; added a regression test driving a single multi-newline batch across the cap with content-encoded birth indices as an oracle (fails on the pre-fix code, passes after) plus a re-convergence check, and a synthetic formula-level test pinning the exact "mixed" (partial room + past-cap) arithmetic.
