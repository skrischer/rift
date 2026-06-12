# Spec: Phase 8 — tmux status-line config mirroring

> Status: DRAFT
> Created: 2026-06-05 (refreshed 2026-06-12 by /loopkit:plan — Phase 8 planning cycle)
> Completed: —

Render the user's tmux status-line configuration (`status-left`, `status-right`, styles) in rift's native statusbar, so a `.tmux.conf` status setup is honored instead of being silently dropped under control mode. The refresh replaces the originally planned client-side format interpreter with **server-side expansion: tmux itself evaluates the format strings** (`display-message -p`) with full DSL fidelity, and rift parses only the style runs (`#[fg=…,bg=…,attrs]`) and color tokens in the expanded output into GPUI-styled text.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] rift reads the active tmux status options (`status`, `status-left`, `status-right`, `status-left-style`, `status-right-style`, `status-style`, `status-left-length`, `status-right-length`, `status-interval`) from the running server via `show-options` over the single command seam.
- [ ] Format strings are **expanded by tmux server-side** (full format-DSL fidelity — variables, `#{?…}` conditionals, `#{E:…}`/`#{T:…}` transforms all behave exactly as tmux defines them, because tmux evaluates them); rift never re-implements the format DSL.
- [ ] The expanded output's **style runs** (`#[fg=…,bg=…,<attrs>]`) and color tokens (`colour0`–`colour255`, named colors, `#rrggbb`, `default`) are parsed client-side and resolve to `Hsla`/GPUI text styling, with `default` mapping onto the active `cx.theme()` foreground/background.
- [ ] Mirroring is **opt-in**; with it off, the native statusbar (Phase 2d fields) renders unchanged — the two modes are mutually exclusive at the render branch and never stack.
- [ ] Malformed or unknown style/color tokens degrade gracefully (skipped or rendered literally, logged once) — never a panic, never a blanked bar.
- [ ] The mirrored bar refreshes on tmux's own `status-interval` cadence plus on relevant state-change notifications — no busy poll faster than tmux's configured interval.
- [ ] Agent-agnostic: this mirrors tmux config, never any process's output; the expanded strings come from rift's own tmux queries on the command channel.

## Scope

### In scope

- **Option discovery** (`show-options`, session-resolved) for the `status-*` set above, reusing the query/refresh machinery the key-table spec establishes (Phase 7 — `show-options` discovery, refresh triggers).
- **Server-side expansion**: fetch the expanded `status-left`/`status-right` via `display-message -p` (targeted at the rift client's own tmux client context, so client-scoped variables resolve correctly) over the single command seam, re-fetched on the `status-interval` tick and on change triggers.
- **Style-run parser**: split the expanded string into styled runs at `#[…]` boundaries; parse tmux color and attribute syntax into `Hsla` + GPUI text attributes; one tested function for color resolution. Fixtures valid and malformed (constitution parser rule).
- **Render mode + toggle**: an exclusive render branch in the statusbar — native Phase 2d fields (default) or the mirrored tmux status line (opt-in via env var, consistent with rift's existing `RIFT_*` configuration style); toggling off leaves no residual state.
- **Refresh wiring**: `status-interval` timer (tmux's own cadence) plus re-fetch on option changes (the Phase 7 refresh-trigger pattern: `set-option` dispatches touching `status-*`).

### Out of scope

- **A client-side format-DSL interpreter** — superseded by server-side expansion; rift parses style runs only. (This deliberately replaces the DRAFT's "documented subset" plan — see Prior decisions.)
- **Multi-line `status-format[N]` / multi-row status** — single status row only.
- **Per-window status styling** (`window-status-*`) — the tab bar owns window presentation (Phase 2e).
- **Writing tmux options back / a config editor** — read-only mirroring.
- **Driving rift's own data fields from this path** — Phase 2d native fields and this mirror are separate, exclusive render modes.

## Human prerequisites

None. Runs against the user's existing tmux config; no secrets, accounts, or provisioning.

## Constraints

- Control mode suppresses tmux's own status rendering by design; tmux still evaluates and maintains the options, which is exactly what server-side expansion exploits — tmux remains the format engine ("tmux is the engine"), rift is the renderer.
- **Single seam, request/response form**: `show-options` and `display-message -p` go over the framed command channel — today termy's `send_command`, after Phase 6 the daemon tmux-command path (the same request/response seam requirement the key-table spec records). Seam-agnostic by design.
- **Sequencing**: the roadmap queue places Phase 8 behind Phase 7 (cross-milestone edge on the prior phase's last issue, #212); the option-discovery and refresh-trigger machinery lands there first and is reused, not duplicated.
- **Expansion context matters**: client-scoped format variables (`#{client_*}`) must expand against the rift client's own tmux client; the first issue validates targeting (`display-message -p -t`) including a conditional and a client variable before the rest proceeds.
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
| **Server-side format expansion via `display-message -p`; rift parses only style runs** — supersedes the DRAFT's client-side "documented subset" interpreter | Constraint-determined (2026-06-12 refresh): tmux already evaluates its own format DSL with full fidelity; re-implementing a subset client-side is a large interpreter, a permanent fidelity gap, and over-engineering against the minimal-solution rule. Style tags pass through expansion untouched, so the client-side surface shrinks to one tested style/color parser. Cost: a cheap command round-trip per `status-interval` tick. | 2026-06-12 |
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
- [ ] Style-run parser fixtures, valid and malformed: multiple `#[…]` runs, every color form (`colourN`, named, `#rrggbb`, `default`), attribute combinations, unknown tokens (graceful skip+log), unterminated tags
- [ ] Expansion fidelity (first-issue validation): a format containing a variable, a `#{?…,…,…}` conditional, and a client-scoped variable expands correctly for the rift client's own tmux client context
- [ ] A non-trivial real `status-right` (e.g. `#[fg=green]#H #[fg=yellow]#S %H:%M`) renders with correct text, colors, and live clock updates at `status-interval` cadence
- [ ] `default` color follows the active rift theme; switching themes re-resolves it
- [ ] Toggling mirroring off restores the Phase 2d native statusbar with no leftover state; toggling on never composes the two
- [ ] No busy poll: fetch cadence equals tmux's `status-interval` (plus change triggers), verified by command-log inspection
- [ ] A `grep` confirms no pane-content parsing and no agent detection in the status path
- [ ] Milestone QA (dev channel): the user's real `.tmux.conf` status line renders faithfully side-by-side with a native tmux client

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Expansion context is wrong (client variables resolve against the wrong tmux client) | First-issue validation pins `display-message -p -t` targeting before the rest proceeds; the Phase 6 per-client attach gives each rift client its own tmux client context. |
| Style/color syntax variety (`colourN`, named, hex, `default`, attribute lists) | One centralized, fixture-tested parse function; unknown attributes are no-ops; malformed tags degrade to literal text, logged once. |
| Per-tick `display-message` round-trips feel heavy | The string is short and the cadence is tmux's own `status-interval` (default 15 s); change triggers avoid stale bars between ticks. If profiling ever disagrees, batching into one command per tick is the lever. |
| Mirrored bar and native fields could visually collide | Exclusive render branch (prior decision); never composed. |
| Users expect the mirror to be the default | Documented: native is default, mirror is opt-in compatibility — the DRAFT's 2026-06-05 product decision stands. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-06-05: Spec created. Spun out of the Phase 2d statusbar discussion: under `tmux -CC` the user's `status-*` config is queryable but never rendered, so it is currently ignored. Phase 2d's native statusbar stays the default source of truth; this spec adds an opt-in mode that mirrors the tmux status-line config instead. Sibling to the tmux key-table mirroring DRAFT (same "surface a hidden tmux config primitive" pattern).
- 2026-06-12: Refreshed by `/loopkit:plan` (loop mode — roadmap Phase 8). The central change: the client-side format-DSL interpreter ("documented subset") is **superseded by server-side expansion** — tmux evaluates its own formats via `display-message -p` with full fidelity, and rift's client-side surface shrinks to a style-run/color parser. Recorded constraint-determined: reuse of the Phase 7 `show-options`/refresh machinery and request/response seam, the env-var toggle, expansion-context targeting validated in the first issue, and the queue edge behind #212. The 2026-06-05 product decisions (native-default + opt-in mirror, read-only, exclusive modes) stand unchanged. No genuinely-open decisions remain for the gate — it is acceptance + prerequisites only.
