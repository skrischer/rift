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
- The chosen on-demand surface (gate decision) — one of: a per-line hover reveal (reusing `pixel_to_grid` `:887` + `on_mouse_move` `:1817` hover hit-testing), a toggleable timestamp gutter column (iTerm2-style, always-on when enabled), or idle-markers at output-pause boundaries — plus the row→line lookup helper it needs.
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
| **OPEN — resolved at the spec-acceptance gate:** the on-demand surface — per-line hover reveal vs. a toggleable iTerm2-style gutter column vs. idle-markers at output-pause boundaries (the QA session leaned "idle-marker + hover") | A UX call the constitution/prior-art does not settle; the backend (line-id + stamping) is shared across all three, so the surface is the only open variable | 2026-07-30 |

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
