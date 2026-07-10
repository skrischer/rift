# Spec: tmux session management

> Status: READY
> Created: 2026-07-08
> Completed: —

Turn the phase-19 session switcher (switch + new, behind a click-to-open title-bar
popover) into a first-class management surface: every host tmux session visible at
a glance, one click to jump, plus rename, reorder, and kill — all agent-agnostic,
all client-side (no protocol or daemon change).

> Visual contract: the **"rift — Session management" Paper artboard** (Paper file
> `rift`), **Frame A** — the title-bar session strip (chips: name + window count +
> attached/current marker, "+ New session…") and its interaction states (per-chip
> rename / kill-confirm menu, inline-rename, drag-to-reorder).

## Outcome

- [ ] Every tmux session on the host is visible at a glance in the cockpit
      without a click-to-open step, and one click jumps the cockpit into any of
      them (the existing switch = re-`Attach` path); the list stays live on
      create / kill / rename (the phase-19 `SessionListReply` churn push).
- [ ] A session can be renamed in-UI (inline edit → `rename-session`); the list
      updates immediately, and when it is the attached session the indicator and
      terminal reflect the new name too (the `%session-renamed` path since
      #429/#448), with no manual refresh.
- [ ] Sessions can be reordered and the order persists across restarts (a local
      per-channel order store, the recents/window-state pattern); the order
      applies to the glanceable surface, never mutating the server list.
- [ ] A session can be killed from the UI behind a confirm affordance
      (`kill-session`); the list drops it live, and killing the attached session
      surfaces the existing `TerminalExit` path — no new teardown code.
- [ ] Creating a new named session works from the surface (the phase-19
      `new-session -A` attach-or-create path).
- [ ] All of it is agent-agnostic and derives only from the tmux control stream;
      no new protocol message, no `PROTOCOL_VERSION` bump.

## Scope

### In scope

- `terminal`: extend `SessionListItem` (crates/terminal/src/lib.rs:78-85) with the
  tmux session `id` (`$<n>`), which the app currently drops when mapping
  `SessionEntry` → `SessionListItem` (crates/app/src/main.rs:1988-1992). The id is
  the rename-stable target for rename / kill (reorder persistence keys by session
  name, not id — see Prior decisions; the id is at most the transient row identity
  during a drag); `SessionEntry.id` already exists (crates/protocol/src/lib.rs:651-658),
  so this is a client-model change only, no protocol change.
- `terminal`: rename and kill ride the existing generic `ClientMessage::TmuxCommand`
  seam (crates/daemon/src/terminal.rs:344-346 executes it fire-and-forget) — the
  same channel the pane-header split / zoom / select-pane controls already use.
  The client assembles tmux-safe, quoted commands: `rename-session -t $<id> -- <q>`
  and `kill-session -t $<id>`, where `<q>` is the new name run through a
  tmux-quoting helper (tested with spaces / quotes / unicode). No daemon change:
  the result surfaces via the churn-driven `SessionListReply` push
  (terminal.rs:461-473, :552-554) and, for a killed attached session, the existing
  `TerminalExit` path.
- `terminal`/`app`: the glanceable management surface = an always-visible session
  strip in the custom title bar's connection group (crates/app/src/title_bar.rs
  `render_connection_group` :148-164 / workspace.rs :1399-1414), REPLACING the
  phase-19 click-to-open popover (`render_session_switcher`,
  session_view.rs:1416-1557). Sessions render as chips: name + attached/current
  marker (the current chip keeps the 2px primary emphasis); click =
  `switch_to_session` (session_view.rs:867-878); a per-chip hover / right-click
  menu holds inline rename and a confirm-guarded kill; drag reorders the chips.
  "+ New session…" stays as a trailing affordance. Reuses the phase-19/21 row
  tokens (mono name 13px, muted window count, success attached dot, danger
  kill-confirm). With the host's few sessions the strip fits the connection group
  beside the window controls; it adds no dock panel.
- `app`: a client-side session-order store — a new `session_order.rs` beside
  `recents.rs` (crates/app/src/recents.rs) reusing `window_state::state_dir`, a
  per-channel `"{channel}-session-order.json"` file, the same tolerant-load /
  atomic-write pattern; keyed by session name (durable across daemon/tmux-server
  restarts, which re-mint ids). Applied as a sort over the server `sessions` list
  at render time, never written into `SessionView.sessions` (replace-semantics
  from the churn push). Unknown / new sessions fall to the tmux default order
  (name) after the stored ones. A client-initiated rename updates the order-store
  key (old → new name) in the SAME action, preserving that session's slot; only an
  external CLI rename re-slots it (self-healing: unknown names sort last).

### Out of scope

- Any protocol or daemon change. Every operation is a client-assembled `TmuxCommand`
  or a client-local store; `PROTOCOL_VERSION` stays 8.
- Parallel multi-session rendering inside ONE window (phase-19 out-of-scope;
  parallelism stays "a second app instance", one control child per client).
- Session resurrection / saved-and-restored dead sessions (zellij-style) — tmux
  does not persist a killed session; kill is terminal here.
- Per-session project roots — worktree / git / LSP state stays keyed to the
  daemon `--root`, not the session (phase-19 documented limitation).
- The post-connect session picker flow and de-hardcoding the connect-card default
  session — phase 33 (`spec-post-connect-picker.md`), which reuses this surface.

## Constraints

- No protocol / daemon change (Prior decisions): rename/kill are fire-and-forget
  `TmuxCommand`s — the codebase's established split of "reply needed → typed
  correlated message (QuerySessionList), fire-and-forget action → generic
  `TmuxCommand`" (the pane-header controls) determines this.
- Control-mode contract (docs/architecture.md): commands run over the existing
  control stream under `%begin/%end` guards; never render tmux chooser UIs
  (`choose-tree` is invisible to control clients).
- Session names are untrusted text: the rename command MUST be tmux-quoted so a
  name with spaces / quotes / `;` cannot break or inject a second command; the
  helper is unit-tested with malformed input.
- Kill is destructive: a confirm affordance guards it (no accidental one-click
  kill); this reverses the phase-19 out-of-scope exclusion deliberately, with the
  confirm as the mitigation.
- Reorder is client-side only — tmux has no session order; the store keys by name
  (a rename re-slots that session, acceptable for a rare explicit action).
- Agent-agnostic: session data derives from the control stream only; no agent
  detection.
- UI contract — all colors / typography as THEME TOKENS, never hardcoded hex
  (reference values Catppuccin Mocha, per the phase-19/21 distillation): reuse the
  phase-19 popover tokens (popover bg, border, radius 8; 30px rows; mono session
  name 13px; muted "N windows" caption; success attached dot; current row =
  surface bg + 2px primary left bar). The trailing action icons are muted, brighten
  on hover; the kill confirm uses the danger token. No emojis.
- No `.unwrap()` in library code; crate boundaries via `lib.rs`; the order store is
  GPUI-free and unit-testable (the recents.rs precedent).
- Requires the daemon transport (`TmuxCommand`/`Attach` are protocol messages) —
  the daemon is the sole terminal source (#285), so this is always available
  once a session is connected.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Rename / kill ride the existing generic `ClientMessage::TmuxCommand`; NO new protocol message, no `PROTOCOL_VERSION` bump | The daemon already executes client-sent raw tmux commands fire-and-forget (terminal.rs:344), which is how pane-header split/zoom/select-pane work; rename/kill need no reply (the `%sessions-changed`/`%session-renamed` churn already re-queries and pushes `SessionListReply`). The codebase's own pattern reserves typed correlated messages (QuerySessionList) for reply-bearing queries only. This supersedes the roadmap's pre-planning "protocol gains rename/kill messages" estimate | 2026-07-08 |
| Target rename/kill by tmux session id (`$<n>`), not name; add `id` to `SessionListItem` | `SessionEntry.id` is rename-stable (protocol lib.rs:651); a name-targeted command races a concurrent rename. The app drops the id today (main.rs:1988) — restoring it is a client-model change, not a protocol one | 2026-07-08 |
| Reorder = drag-to-order (a total user-set order), over a client-side per-channel order store (a `recents.rs` clone) keyed by session NAME, applied as a render-time sort over the server list | tmux has no session order; "reorder" means an explicit user-set order, so a total-order store (not a pin/favorite subset flag) is the honest model; the recents/window-state local-store pattern is the precedent (recents.rs:1-6). Name-keying survives a daemon restart that re-mints ids; a client-initiated rename renames the key too (slot preserved), only an external CLI rename re-slots. The store never mutates `SessionView.sessions` — replaced wholesale by the churn push (session_view.rs:822) | 2026-07-08 |
| Kill is guarded by an inline confirm affordance | Phase 19 excluded kill-from-picker as "destructive; not in v1 UI"; adding it needs a mitigation so a stray click cannot nuke a session. Killing the attached session reuses the existing `TerminalExit` path — no new teardown | 2026-07-08 |
| Glanceable surface = an always-visible session strip in the custom title bar (phase-21 connection group): chips, click = switch (current marked), rename / kill via a per-chip hover / right-click menu, drag = reorder; the phase-19 click-to-open popover is REMOVED | Spec-acceptance gate (2026-07-08): the few-session workflow (rift / rift-dev) wants glanceable-first with zero extra panel; the connection/session group already lives in the title bar, so the strip replaces the popover there rather than adding a dock panel that competes with the explorer for width. Rename/kill are compact by design (per-chip menu) | 2026-07-08 |

## Prior art

- `docs/prior-art.md` → "Session management & post-connect picker — prior-art index
  (Phases 32–33)", Phase 32 rows: iTerm2 tmux **Dashboard** (all sessions at a
  glance + rename + switch over the same `-CC` stream) as the glanceable-surface UX
  reference; `zellij` session-manager + `endoze/zellij-switcher` (per-row rename /
  kill, index quick-switch) for the operations UX; tmux `rename-session` /
  `kill-session` / `new-session` (reuse, over the existing control stream); session
  reorder is flagged **greenfield** (no tmux session order — client-side ordering
  on the window-state store precedent).

## Human prerequisites

None — everything runs against the existing SSH host and tmux server; no new
secrets, accounts, or external provisioning.

## Tracking

- Milestone: created after this spec merges (phase 32).
- Issues: one per implementable step, each referencing this spec path.

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Unit tests: the tmux-quoting helper (a name with spaces / `"` / `'` / `;` /
      leading `-` / `$` / `#` / unicode round-trips to a single safe
      `rename-session` argument after `--`); the session-order store (tolerant load
      of missing/corrupt file; atomic save; render-time sort places stored sessions
      first in order, unknown ones after by name; a client rename renames the key,
      preserving the slot)
- [ ] Behavioral (dev channel): every host session is visible without a
      click-to-open step; clicking one jumps the cockpit and the indicator to it
- [ ] Rename a session in-UI → the list, indicator, and terminal show the new name
      with no manual refresh; a name with a space survives (quoting)
- [ ] Reorder sessions (drag) → the order holds; relaunch the app → the order
      persists; rename a reordered session in-UI → it keeps its slot
- [ ] Kill a non-attached session (confirm) → it drops from the list live; kill the
      attached session → the existing `TerminalExit` path fires (no freeze)
- [ ] "+ New session…" creates and attaches a fresh named session
- [ ] No hardcoded hex in the new/touched rendering code (grep-verified)

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| A raw `rename-session` string with an unescaped name breaks the control command or injects a second command | The client assembles the command through a tmux-quoting helper unit-tested with spaces / quotes / `;`; the name is passed after `--` |
| Reorder store keyed by name goes stale when a session is renamed EXTERNALLY (tmux CLI) | A client-initiated rename renames the store key too (slot preserved); only an external rename re-slots, self-healing (unknown names sort last by name) — accepted for a rare action |
| Fire-and-forget rename/kill fails silently (duplicate/invalid name, blocked kill) — `TmuxCommand` surfaces no error/ack | Accepted for a personal tool: the inline edit snaps back with no feedback. A typed correlated `RenameSession`/`KillSession` (daemon-assembled, reply-bearing) is the upgrade path IF error feedback ever matters — not warranted now; this is the one real argument for a typed message |
| Kill fired on the wrong row | Inline confirm affordance (two-step); the id-targeted command cannot hit a renamed-away session |
| The churn push replaces `sessions` mid-interaction (e.g. during a drag) and drops the local order | Order is a render-time sort over the server list, not stored in `sessions`; a replace re-applies the same sort — the drag operates on the derived view and commits to the order store, not the model |
| A killed attached session leaves the client frozen | Reuses the existing `TerminalExit` path (protocol lib.rs:365) that pane/window exit already drives; no new teardown |

## Decision log

- 2026-07-08: Spec drafted from the phase-19 session-switch base + the phase-32
  seam map. Key finding: rename/kill need NO protocol/daemon change — they ride the
  existing `TmuxCommand` fire-and-forget seam (terminal.rs:344) and the churn-driven
  `SessionListReply` push, so this phase is client-only (`[terminal]`/`[app]`). The
  roadmap's pre-planning "protocol gains rename/kill messages" foundation note is
  corrected in this phase's step-8 roadmap-update PR (no protocol change; reorder
  client-side).
- 2026-07-08: Fresh-context review (PR #667): APPROVE, no blocking findings.
  Adopted the non-blocking refinements — reorder fixed to drag-to-order (total
  order, the store already implies it); an in-UI rename renames the order-store
  key so the slot is preserved (only external CLI renames re-slot); silent
  fire-and-forget rename/kill failure acknowledged in Risks (typed correlated
  message is the upgrade path, not warranted now); expanded quoting-helper tests
  (leading `-`, `'`, `$`, `#`); tightened the rename-indicator wording to the
  attached session.
- 2026-07-08: Spec-acceptance gate — the one open decision (glanceable-surface
  placement) resolved to an always-visible title-bar session strip replacing the
  phase-19 popover; rename/kill via a per-chip hover / right-click menu, drag to
  reorder. Human prerequisites: none. Spec accepted.
