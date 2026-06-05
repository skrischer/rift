# Spec: tmux key-table mirroring

> Status: DRAFT
> Created: 2026-06-04
> Completed: —

Make configured tmux keybindings work while focus is in a rift pane. Today input is sent via `send-keys -t <pane> -H <hex>`, which injects raw bytes into the pane PTY and *bypasses tmux's key tables entirely* (see the "tmux control-mode interaction model" section in `architecture.md`). As a result the prefix chord and every `bind-key` (window/pane management, copy-mode entry, custom bindings) are inert in rift. This spec restores them by mirroring tmux's key tables client-side.

This is intentionally split out of `spec-terminal-interaction-fixes.md` because it is an order of magnitude larger than the dogfooding fixes (prefix state machine, `list-keys` parsing, per-mode tables) and must not block them.

## Outcome

What is true when this work is done (to be refined before READY):

- [ ] Pressing the configured tmux prefix followed by a bound key runs the bound tmux command, matching a native client
- [ ] Bindings are read from the live tmux config (`list-keys`), not hardcoded — a user's custom bindings work with zero rift changes
- [ ] Per-mode key tables are respected (at least `prefix` and `root`; copy-mode tables depend on the copy-mode design)
- [ ] Keys with no binding fall through to the existing `encode_keystroke` → `send-keys -H` path unchanged

## Scope

### In scope (provisional)

- Query tmux key tables via `list-keys` (over the framed `send_command` channel) and build a lookup
- A prefix state machine: intercept the prefix chord before `encode_keystroke`, capture the next chord, resolve the bound command
- Dispatch the resolved command as a control command (feasible with zero termy changes — `send_command` already exists)
- Re-query / invalidate the table when the config changes (mechanism TBD)

### Out of scope (provisional)

- copy-mode/choose-mode *rendering* — not delivered to control clients; scrollback is handled separately via `capture-pane` (see `spec-terminal-interaction-fixes.md`)
- Rebinding or editing tmux config from rift
- Conflict detection between tmux bindings and rift-native GUI shortcuts (revisit once the table is known)

## Constraints

- Input/command emission must stay behind the single `TmuxClient` seam (Phase 3 transport swap stays single-seam).
- `list-keys` output parsing must handle the documented control-mode quirks (framing, escaping) — reuse termy's command-response path.
- No termy changes anticipated; if any are needed, contribute upstream rather than fork.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Mirror via `list-keys`, not a hardcoded table | A user's custom bindings must work with zero rift changes; reading the live config is the only agent-/config-agnostic approach | 2026-06-04 |
| Split out of the interaction-fixes spec | Order-of-magnitude larger (prefix state machine, per-mode tables); would sink the small-fix batch | 2026-06-04 |
| Feasible with zero termy changes | Prefix interception sits in rift's `on_key_down` before `encode_keystroke`; bound commands dispatch via existing `send_command` | 2026-06-04 |

## Tracking

No milestone or issues until this spec is promoted to READY.

## Verification

To be defined when promoted. At minimum: prefix + bound key triggers the command; custom user bindings work; unbound keys still reach the PTY.

## Decision log

- 2026-06-04: Stub created when splitting key-table mirroring out of `spec-terminal-interaction-fixes.md`. Remains DRAFT until the interaction fixes land and the copy-mode/mode-table interplay is settled.
