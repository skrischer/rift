# Spec: tmux status-line config mirroring

> Status: DRAFT
> Created: 2026-06-05
> Completed: —

Render the user's tmux status-line configuration (`status-left`, `status-right`, `status-style`, window-status formats) in rift's native statusbar, so a user's `.tmux.conf` status setup is honored instead of being silently dropped under control mode.

## Outcome

- [ ] rift reads the active tmux status-line options (`status`, `status-left`, `status-right`, `status-style`, `status-left-style`, `status-right-style`, `status-left-length`, `status-right-length`) from the running server
- [ ] A tmux format-string interpreter renders a documented subset of the format DSL into native GPUI text: literal text, the common single-letter shortcuts (`#H`, `#S`, `#I`, `#W`, `#D`, `#P`, `#T`, `#F`), `#{...}` variable expansion for the variables rift already tracks, inline style tags `#[fg=…,bg=…,<attr>]`, and the `#{?cond,then,else}` conditional
- [ ] tmux color tokens (`colour0`-`colour255`, named colors, `#rrggbb`, `default`) resolve to `Hsla`, mapping `default` onto the active `cx.theme()` foreground/background
- [ ] Mirroring is opt-in; with it off, the native statusbar (Phase 2d fields) renders unchanged — the two modes are mutually exclusive and never stack
- [ ] Unsupported format tokens degrade gracefully (rendered literally or skipped, logged once) rather than panicking or blanking the bar
- [ ] The statusbar refreshes on tmux's status interval / on `%output`-driven status changes, not on a busy poll

## Scope

### In scope

- Fetching `status-*` options via a control-mode command (`show-options -g`) and on change
- A tmux format-string interpreter covering the subset listed in Outcome (variables rift can resolve, inline styles, one level of conditional, left/right alignment split)
- Mapping tmux's color and attribute syntax to rift's `ThemeColor`/`Hsla` and GPUI text styling
- A toggle to choose between the native statusbar (default) and the mirrored tmux status line
- Graceful fallback for tokens outside the supported subset

### Out of scope

- Full fidelity with the entire tmux format DSL (every `#{...}` expansion, `#{T:…}`/`#{s:…}` transforms, nested conditionals, `#{e:…}` math) — only the documented subset
- Multi-line `status-format[N]` / the tmux 2.9+ multi-row status — single status row only
- Per-window status styling (`window-status-*`) — the tab bar already owns window presentation (Phase 2e)
- Writing tmux options back / a config editor — read-only mirroring
- Driving rift's own data fields from this path — Phase 2d's native fields and this mirror are separate, exclusive render modes

## Constraints

- Control mode (`tmux -CC`) suppresses tmux's own status-line rendering by design; rift is the only renderer. tmux still maintains the status options, so they are queryable but never drawn by tmux. This spec closes the gap that those options are currently ignored.
- Options and format strings are obtained over the existing control-mode command channel; no second tmux connection.
- tmux 3.4+ (already a hard requirement from Phase 2a).
- Agent-agnostic: tmux is the substrate, not an agent. This mirrors tmux config, never any agent's output — no agent-specific code rule still holds.
- Color resolution goes through `cx.theme()` so the mirrored bar still respects the active rift theme where tmux says `default`.
- Sibling to [spec-tmux-keytable-mirroring.md](spec-tmux-keytable-mirroring.md): same pattern of surfacing a tmux config primitive (key tables there, status line here) that control mode otherwise hides.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Native statusbar (Phase 2d) is source of truth by default | rift owns its chrome; the mirror is an opt-in compatibility mode, not the default | 2026-06-05 |
| Support a documented subset of the tmux format DSL, not full fidelity | The full DSL is a large interpreter; a subset covers the common `status-left/right` setups and keeps scope bounded | 2026-06-05 |
| Read-only mirroring | rift reflects tmux config; it does not become a tmux config editor | 2026-06-05 |

## Tracking

The decomposition into steps lives as GitHub issues, not in this file — one issue per step, grouped under a milestone. This spec owns the design; the issues own progress.

- Milestone: (created when this spec moves to `READY`)
- Issues: created from this spec once it is `READY` (one per implementable step)

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Format-interpreter unit tests cover valid and malformed input: each supported variable, an inline style tag, a `#{?…}` conditional, a left/right split, an unknown token (graceful fallback)
- [ ] A non-trivial real `.tmux.conf` `status-right` (e.g. `#[fg=green]#H #[fg=yellow]#S`) renders with correct text and colors in the native bar
- [ ] tmux color forms (`colourN`, named, `#rrggbb`, `default`) all resolve, `default` following the active theme
- [ ] Toggling mirroring off restores the Phase 2d native statusbar with no leftover state
- [ ] No busy-poll: statusbar updates are driven by tmux status changes, not a fixed-rate timer faster than tmux's interval

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| tmux format DSL surface is very large | Ship a documented subset; anything outside it falls back literally and is logged once. Expand the subset only on real demand. |
| Color/attribute syntax variety (`colourN`, named, hex, `default`, `bright`, `bold`) | Centralize parsing in one tested function; map `default` to theme tokens; treat unknown attributes as no-op. |
| Re-parsing the format on every status tick is wasteful | Parse to an intermediate representation once per config change; only re-resolve variable values on tick. |
| Mirrored bar and native fields could visually collide if both render | Make the modes mutually exclusive at the render branch; never compose them. |
| Users expect full tmux fidelity | Document the supported subset explicitly; the native statusbar remains the recommended default. |

## Decision log

- 2026-06-05: Spec created. Spun out of the Phase 2d statusbar discussion: under `tmux -CC` the user's `status-*` config is queryable but never rendered, so it is currently ignored. Phase 2d's native statusbar stays the default source of truth; this spec adds an opt-in mode that mirrors the tmux status-line config instead. Sibling to the tmux key-table mirroring DRAFT (same "surface a hidden tmux config primitive" pattern).
