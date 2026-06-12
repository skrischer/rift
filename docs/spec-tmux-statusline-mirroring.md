# Spec: Phase 8 — tmux status-line config mirroring

> Status: DRAFT
> Created: 2026-06-05 (refreshed 2026-06-12 by /loopkit:plan — Phase 8 planning cycle)
> Completed: —

Render the user's tmux status-line configuration (`status-left`, `status-right`, styles) in rift's native statusbar, so a `.tmux.conf` status setup is honored instead of being silently dropped under control mode. The refresh replaces the originally planned client-side format interpreter with **server-side expansion: tmux itself evaluates the format strings** (`display-message -p '#{T:…}'`) with the format engine's own fidelity — except `#()` shell-command insertions, which one-shot expansion cannot run (see Constraints) — and rift parses only the style runs (`#[fg=…,bg=…,attrs]`) and color tokens in the expanded output into GPUI-styled text. The mirror renders the **left and right segments only**; the bar's center window list is not mirrored — rift's tab bar already owns window presentation.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] rift reads the active tmux status options (`status`, `status-left`, `status-right`, `status-left-style`, `status-right-style`, `status-style`, `status-left-length`, `status-right-length`, `status-interval`) from the running server via `show-options` over the single command seam.
- [ ] Format strings are **expanded by tmux server-side** via `#{T:…}` (format expansion plus strftime, so `%H:%M` clocks work) — variables, `#{?…}` conditionals, and `#{E:…}` transforms behave exactly as tmux defines them because tmux evaluates them; rift never re-implements the format DSL. The one documented exception: **`#()` shell-command segments render empty** (one-shot expansion returns the placeholder; tmux only runs those jobs when drawing a status line, which never happens under `-CC` — verified in `spec-phase2d-tabbar.md`'s 2026-06-05 decision log).
- [ ] The expanded output's **style runs** (`#[fg=…,bg=…,<attrs>]`, including `#[default]` and tokens like `#[range=…]`/`#[list=…]` as no-ops) and color tokens (`colour0`–`colour255`, named colors, `#rrggbb`, `default`) are parsed client-side and resolve to `Hsla`/GPUI text styling. **`status-style` is the bar's base style**: it paints the bar's background/foreground and is what `default` and `#[default]` reset to; where `status-style` itself says `default`, the active `cx.theme()` colors apply.
- [ ] **Length limits are honored client-side**: tmux truncates `status-left`/`status-right` at draw time, which never happens here, so rift truncates the parsed runs to `status-left-length`/`status-right-length` (cell-width- and style-run-aware).
- [ ] Mirroring is **opt-in**; with it off, the native statusbar (Phase 2d fields) renders unchanged — the two modes are mutually exclusive at the render branch and never stack.
- [ ] Malformed or unknown style/color tokens degrade gracefully (skipped or rendered literally, logged once) — never a panic, never a blanked bar.
- [ ] The mirrored bar refreshes on tmux's own `status-interval` cadence plus on relevant state-change notifications — no busy poll faster than tmux's configured interval; **`status-interval 0` disables the timer entirely** (change triggers only, matching tmux's "no interval redraw" semantics).
- [ ] Agent-agnostic: this mirrors tmux config, never any process's output; the expanded strings come from rift's own tmux queries on the command channel.

## Scope

### In scope

- **Option discovery** (`show-options`, session-resolved) for the `status-*` set above, reusing the query/refresh machinery the key-table spec establishes (Phase 7 — `show-options` discovery, refresh triggers).
- **Server-side expansion**: fetch the expanded segments via `display-message -p '#{T:status-left}'` (and `status-right`) over the single command seam, re-fetched on the `status-interval` tick and on change triggers. **Never interpolate raw option values into a command line** (quoting/injection hazard) — always expand the option by name through `#{T:…}`. Client-scoped variables resolve against the rift client's own tmux client: commands issued over rift's own control-mode connection already run with that client current; if explicit targeting is ever needed it is `-c <target-client>` (not `-t`, which targets a pane) — the first issue pins this.
- **Style-run parser**: split the expanded string into styled runs at `#[…]` boundaries; parse tmux color and attribute syntax into `Hsla` + GPUI text attributes (`#[default]` resets to the `status-style` base; `#[range=…]`/`#[list=…]` are no-ops); handle post-expansion literal `#[` produced by `##` escaping the way tmux's own draw-time parser does; one tested function for color resolution. Fixtures valid and malformed, including multibyte UTF-8 (constitution parser rule).
- **Client-side length truncation** of the parsed runs per `status-left-length`/`status-right-length` (cell-width- and style-run-aware).
- **Render mode + toggle**: an exclusive render branch in the statusbar — native Phase 2d fields (default) or the mirrored tmux status line (opt-in via env var, consistent with rift's existing `RIFT_*` configuration style); toggling off leaves no residual state.
- **Refresh wiring**: `status-interval` timer (tmux's own cadence) plus re-fetch on option changes (the Phase 7 refresh-trigger pattern: `set-option` dispatches touching `status-*`).

### Out of scope

- **A client-side format-DSL interpreter** — superseded by server-side expansion; rift parses style runs only. (This deliberately replaces the DRAFT's "documented subset" plan — see Prior decisions.)
- **`#()` shell-command segments** — one-shot expansion cannot run them (tmux runs those jobs only when drawing, which `-CC` never does); they render empty in v1, documented as the known fidelity gap.
- **The window-list center section of the bar** — the mirror renders left+right segments only; rift's tab bar is the window list. (This subsumes the narrower `window-status-*` styling exclusion: the whole center section is not mirrored, Phase 2e owns it.)
- **Multi-line `status-format[N]` / multi-row status** — a `status` value of 2–5 mirrors only the first row; the rest degrade silently (logged once).
- **Writing tmux options back / a config editor** — read-only mirroring.
- **Driving rift's own data fields from this path** — Phase 2d native fields and this mirror are separate, exclusive render modes.

## Human prerequisites

None. Runs against the user's existing tmux config; no secrets, accounts, or provisioning.

## Constraints

- Control mode suppresses tmux's own status rendering by design; tmux still evaluates and maintains the options, which is exactly what server-side expansion exploits — tmux remains the format engine ("tmux is the engine"), rift is the renderer.
- **Single seam, request/response form**: `show-options` and `display-message -p` go over the framed command channel — today termy's `send_command`, after Phase 6 the daemon tmux-command path (the same request/response seam requirement the key-table spec records). Seam-agnostic by design.
- **Sequencing**: the roadmap queue places Phase 8 behind Phase 7 (cross-milestone edge on the prior phase's last issue, #212); the option-discovery and refresh-trigger machinery lands there first and is reused, not duplicated.
- **Expansion context matters**: client-scoped format variables (`#{client_*}`) must expand against the rift client's own tmux client. Commands on rift's own control-mode connection already run with that client current; explicit targeting, if needed, is `-c <target-client>`. The first issue validates expansion end-to-end — a variable, a `#{?…}` conditional, a client-scoped variable, strftime via `#{T:…}`, and a `#()` segment (expected empty) — before the rest proceeds.
- tmux 3.4+ (hard floor since Phase 2a).
- Color resolution goes through `cx.theme()` where tmux says `default`, so the mirrored bar respects the active rift theme.
- Re-fetch cadence is tmux's `status-interval` — parsing happens per fetch on an already-expanded short string; no cached intermediate representation is needed (the expensive part, format evaluation, lives in tmux).
- Agent-agnostic; no pane-content parsing. `thiserror` in libraries; no `.unwrap()`; no emojis in UI.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Native statusbar (Phase 2d) is source of truth by default**; the mirror is opt-in | rift owns its chrome; the mirror is a compatibility mode, not the default. | 2026-06-05 |
| **Read-only mirroring** | rift reflects tmux config; it does not become a tmux config editor. | 2026-06-05 |
| **Exclusive render modes** — native fields or mirrored bar, never composed | Visual collision is the failure mode; exclusivity at the render branch removes it. | 2026-06-05 |
| **Server-side format expansion via `display-message -p '#{T:…}'`; rift parses only style runs** — supersedes the DRAFT's client-side "documented subset" interpreter | Constraint-determined (2026-06-12 refresh): tmux already evaluates its own format DSL; re-implementing a subset client-side is a large interpreter, a permanent fidelity gap, and over-engineering against the minimal-solution rule. Style tags pass through expansion untouched, so the client-side surface shrinks to one tested style/color parser plus length truncation. Cost: a cheap command round-trip per `status-interval` tick. Known gap: `#()` segments (see Out of scope) — smaller than the subset-interpreter's gap, which could not evaluate `#()` either. | 2026-06-12 |
| **Reuse the Phase 7 query/refresh machinery** (`show-options` discovery, refresh triggers, request/response seam) | Constraint-determined: the key-table spec builds exactly this; duplicating it would violate reuse-existing-patterns. Sibling pattern — both specs surface a tmux config primitive control mode otherwise hides. | 2026-06-12 |
| **Opt-in toggle is an env var** (`RIFT_*` style) for v1 | Constraint-determined: rift's configuration today is env vars with working defaults (justfile/CLAUDE.md); a settings UI is not this spec's scope. | 2026-06-12 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 8 milestone. Created once this spec is `READY` and merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created at `READY` (Phase 8 — tmux status-line mirroring)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Style-run parser fixtures, valid and malformed: multiple `#[…]` runs, every color form (`colourN`, named, `#rrggbb`, `default`), attribute combinations, `#[default]` reset, `#[range=…]`/`#[list=…]` no-ops, post-expansion literal `#[` (from `##` escaping), multibyte UTF-8 content, unknown tokens (graceful skip+log), unterminated tags
- [ ] Expansion fidelity (first-issue validation): a format containing a variable, a `#{?…,…,…}` conditional, a client-scoped variable, strftime via `#{T:…}`, and a `#()` segment (expected empty) expands correctly for the rift client's own tmux client context
- [ ] A non-trivial real `status-right` (e.g. `#[fg=green]#H #[fg=yellow]#S %H:%M`) renders with correct text, colors, and live clock updates at `status-interval` cadence
- [ ] `status-left-length`/`status-right-length` truncate the rendered segments (cell-width-aware, styles intact); `status-style` paints the bar base and `default`/`#[default]` resolve to it
- [ ] `default` color follows the active rift theme where `status-style` itself is default; switching themes re-resolves it
- [ ] Toggling mirroring off restores the Phase 2d native statusbar with no leftover state; toggling on never composes the two
- [ ] No busy poll: fetch cadence equals tmux's `status-interval` (plus change triggers); `status-interval 0` runs no timer — verified by command-log inspection
- [ ] A `grep` confirms no pane-content parsing and no agent detection in the status path
- [ ] Milestone QA (dev channel): the user's real `.tmux.conf` **left and right status segments** render faithfully against a native tmux client (the center window list is rift's tab bar by design; `#()` segments are the documented gap)

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Expansion context is wrong (client variables resolve against the wrong tmux client) | First-issue validation pins `display-message -p -t` targeting before the rest proceeds; the Phase 6 per-client attach gives each rift client its own tmux client context. |
| Style/color syntax variety (`colourN`, named, hex, `default`, attribute lists) | One centralized, fixture-tested parse function; unknown attributes are no-ops; malformed tags degrade to literal text, logged once. |
| Per-tick `display-message` round-trips feel heavy | The string is short and the cadence is tmux's own `status-interval` (default 15 s); change triggers avoid stale bars between ticks. If profiling ever disagrees, batching into one command per tick is the lever. |
| Out-of-band config changes (a co-attached native client or a pane process running `set-option`) leave the bar stale | Accepted: rift's refresh triggers only see rift-dispatched changes; staleness is bounded by one `status-interval`. Optional future lever: a `refresh-client -B` format subscription (machinery live since Phase 2d) pushing changes, modulo the same `#()` caveat. |
| Users expect full tmux fidelity, including `#()` script segments | Documented limitation: `#()` renders empty under one-shot expansion (tmux only runs those jobs when drawing); everything else is tmux-evaluated. Native statusbar remains the recommended default; the mirror is opt-in compatibility — the DRAFT's 2026-06-05 product decision stands. |
| Mirrored bar and native fields could visually collide | Exclusive render branch (prior decision); never composed. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-06-05: Spec created. Spun out of the Phase 2d statusbar discussion: under `tmux -CC` the user's `status-*` config is queryable but never rendered, so it is currently ignored. Phase 2d's native statusbar stays the default source of truth; this spec adds an opt-in mode that mirrors the tmux status-line config instead. Sibling to the tmux key-table mirroring DRAFT (same "surface a hidden tmux config primitive" pattern).
- 2026-06-12: Refreshed by `/loopkit:plan` (loop mode — roadmap Phase 8). The central change: the client-side format-DSL interpreter ("documented subset") is **superseded by server-side expansion** — tmux evaluates its own formats via `display-message -p` and rift's client-side surface shrinks to a style-run/color parser. Recorded constraint-determined: reuse of the Phase 7 `show-options`/refresh machinery and request/response seam, the env-var toggle, expansion-context targeting validated in the first issue, and the queue edge behind #212. The 2026-06-05 product decisions (native-default + opt-in mirror, read-only, exclusive modes) stand unchanged. No genuinely-open decisions remain for the gate — it is acceptance + prerequisites only.
- 2026-06-12: Review gate (fresh-context Agent review, `NEEDS CHANGES` → addressed). All five blocking findings folded in: (1) the fidelity claim is qualified — `#()` shell segments render empty under one-shot expansion (cross-verified against `spec-phase2d-tabbar.md`'s 2026-06-05 live finding), added to the first-issue validation and the risk table; (2) client targeting corrected to `-c` (`-t` targets a pane), with the note that rift's own control-mode connection already carries the right client context; (3) fetch pinned to `#{T:…}` (format + strftime — live clocks work) and raw-option interpolation forbidden (quoting/injection); (4) the mirror is explicitly left+right segments only (no window list — the tab bar owns the center) and the QA criterion is scoped accordingly; (5) `status-style` is assigned as the bar's base style (`default`/`#[default]` resolve to it, theme applies where it is itself default) and length limits are truncated client-side (tmux truncates only at draw time). Non-blocking: `status-interval 0` = no timer; `##`-escaped literal `#[` and `#[range/list]` fixtures; out-of-band staleness named as accepted (one-interval bound, `refresh-client -B` as future lever); UTF-8 fixtures; multi-row `status` degrades to the first row.
