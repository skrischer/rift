# Spec: Dogfooding fixes (living)

> Status: LIVING
> Created: 2026-06-09
> Completed: — (never; this spec is LIVING, see "Why this spec is LIVING")

A rolling backlog of small, self-contained UX/interaction defects found while using
rift. One design anchor for many tiny fixes that each do not warrant their own spec —
the successor to the now-archived `spec-terminal-interaction-fixes.md`, generalised
beyond the terminal surface.

## Why this spec is LIVING

`LIVING` is a new status marker (added to `handover-conventions.md` alongside this
spec): a spec that is a **rolling backlog and is never `COMPLETED` or archived**. The
standard lifecycle "all verification met -> `COMPLETED` -> `archive/`" does not apply,
because there is no terminal "done" — the backlog refills as dogfooding surfaces new
papercuts.

The design (this file) persists. The individual fixes live as issues, exactly like any
other spec: each issue references this spec path, each `fix:` PR closes its issue and so
passes the planning gate. When an issue closes, its history stays on GitHub; the spec is
unchanged. The spec is the durable *admission policy* for the backlog, not a checklist
that completes.

## What qualifies as an entry

An entry (one issue under this spec) must be **all** of:

- **A defect, not a feature.** It restores intended/existing behaviour or sharpens a GUI
  affordance — it does not add new product scope. New features get their own spec.
- **Small and self-contained.** One concern, well under the ~400-line PR ceiling
  (`CLAUDE.md`), no cross-cutting design surface. If a fix needs a state machine, a
  protocol change, a new crate, or a multi-PR sequence, it has outgrown this bucket.
- **Surfaced by use.** Found by actually running rift (dogfooding), not speculative.
- **Agent-agnostic.** No detection or special-casing of which CLI agent runs
  (`CLAUDE.md`). A papercut in how rift reacts to a universal signal (keystroke, PTY
  byte, file event) qualifies; "make Claude Code's prompt box behave" does not.

The **graduation rule:** if, on investigation, a fix turns out to carry a real design
surface, it leaves this spec and gets its own. Precedent: tmux key-table mirroring was
split out of `spec-terminal-interaction-fixes.md` for exactly this reason (prefix state
machine, `list-keys` parsing, per-mode tables). This bucket exists to *ship the small
fixes without blocking on the large ones* — keep it honest by exporting anything that
grows.

## Design framing: affordance vs defect

Carried from the archived terminal-interaction-fixes spec. Every entry states which of
two categories it is in, because that determines whether the change is the correct
design or a workaround:

1. **A GUI affordance** — rift renders something natively/better than the underlying
   layer (e.g. GPU scrollback, font zoom, border-drag resize). Inheriting the text-mode
   original would be *failing at being a GUI*. Not a workaround.
2. **A genuine defect in an existing path** — an input/output signal that should already
   flow correctly but does not. The fix repairs the existing seam; it adds no new design.

The first entry (Tab forwarding, below) is category 2.

## Outcome

Because this spec is `LIVING`, this is **not** a completing checklist (no `COMPLETED`
state exists for it). It is the standing bar every entry's issue clears before it
closes — applied per entry, not ticked off once:

- The reported papercut no longer reproduces when dogfooding rift.
- The change is small, self-contained, and agent-agnostic (admission criteria above).
- `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` stay green.
- A defect-category entry adds no new design surface; an affordance-category entry routes
  through the existing narrow seam for its surface, not a parallel mechanism.

## Scope

### In scope

- Small, self-contained, dogfooding-surfaced interaction/UX defects across any rift
  crate that meet the admission criteria.
- The first entry: **Tab keypress not forwarded to the PTY** (see Tracking).

### Out of scope

- New features or product scope (own spec).
- Any fix that grows a real design surface — it graduates to its own spec (graduation
  rule above). Notably tmux key-table mirroring (`spec-tmux-keytable-mirroring.md`) owns
  all *bound*-key behaviour; this bucket only covers raw/unbound-key papercuts that fall
  through to the existing forwarding path.
- Cross-cutting refactors. Those are `refactor:` work, exempt from the planning gate and
  not a papercut.

## Constraints

- Each entry obeys the project rules in `CLAUDE.md`: no agent-specific code, no `.unwrap()`
  in library code, no `clone()` to satisfy the borrow checker, ~400-line PR ceiling.
- Surface-specific constraints are owned by that surface's architecture/spec, not
  restated here. An entry touching terminal input, for instance, still routes through the
  single tmux/input seam (`TmuxClient` today) per `architecture.md` — it does not reach
  into `alacritty_terminal::Term` internals.
- Entries are typed `fix:` (occasionally `perf:`/`refactor:` when that is honestly the
  change). A `fix:` PR closes its issue and so must trace to this spec via the planning
  gate; `refactor:`/`perf:` are gate-exempt but still reference the spec for traceability.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Status is `LIVING`; the spec is never `COMPLETED` or archived | A rolling backlog has no terminal done-state; archiving it would orphan every future entry. The design (admission policy) is durable while individual fixes churn as issues. | 2026-06-09 |
| Entries are grouped by a `papercut` GitHub label, not a milestone | A milestone groups a *phase* and closes when its issues do (`handover-conventions.md`); an open-ended backlog never closes, so a milestone misrepresents it. The planning gate only *warns* on a missing milestone (it does not block), so a label-only entry merges cleanly. | 2026-06-09 |
| This is the successor to `spec-terminal-interaction-fixes.md`, generalised beyond the terminal | That spec proved the batch-spec pattern (one design anchor, N small issues) but was scoped to terminal interaction and went `COMPLETED`. Dogfooding papercuts are not all terminal-bound, and the value is a *standing* bucket, not a one-off batch. | 2026-06-09 |
| A fix that grows a real design surface graduates to its own spec | Keeps the bucket honest: it ships small fixes fast precisely by exporting anything that needs design (precedent: key-table mirroring split off the terminal-interaction batch). | 2026-06-09 |

## Tracking

The decomposition lives as GitHub issues — one issue per papercut — grouped by the
`papercut` label, not a milestone (see Prior decisions). This spec owns the admission
policy; the issues own the individual fixes and their progress.

- Grouping: [`papercut` label](https://github.com/skrischer/rift/labels/papercut)
- First entry (resolved 2026-06-10, #116): **Tab keypress not forwarded to the PTY** —
  pressing Tab in a pane did not reach the shell/agent, so shell completion
  (`cd /path/<Tab>`) and Claude Code's slash/prompt suggestions were dead. Root cause,
  **confirmed during the fix**: gpui-component's `Root` view binds `tab`/`shift-tab` to
  focus navigation in the `"Root"` context, which wraps every pane and consumes the
  keystroke before it reaches the pane's `on_key_down` (GPUI dispatches matched bindings
  ahead of key-down listeners). Category 2 (defect in an existing path); the fix shadows
  those bindings with `NoAction` in the deeper `"Terminal"` context so Tab falls through
  to the existing `encode_keystroke` path. See the Decision log for the full mechanism.

Each issue references this spec path in its body. A `fix:` PR may only merge if it closes
an issue that traces back here (planning gate).

## Verification

There is no whole-spec verification — `LIVING` specs do not reach `COMPLETED`.
Verification is **per entry**, owned by each issue's Acceptance checklist, and always
includes:

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] The specific papercut no longer reproduces (stated as a concrete behavioural check
      in the issue, verified by dogfooding)

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The bucket becomes a dumping ground for half-features ("scope creep by a thousand papercuts") | The admission criteria and graduation rule are the gate; the spec PR review and each issue's framing reject entries that carry a real design surface |
| A `LIVING` spec that never archives accrues stale/abandoned entries | Closed issues are the history; the live backlog is exactly the open `papercut` issues. The spec body stays generic (policy, not a per-entry list), so it does not rot |
| Confusion with `spec-tmux-keytable-mirroring.md` over who owns key behaviour | Explicit boundary (Out of scope): keytable-mirroring owns *bound* keys; this bucket only covers raw/unbound-key papercuts on the existing fall-through path |

## Decision log

- 2026-06-09: Spec created as a `LIVING` batch spec — the standing successor to the
  archived `spec-terminal-interaction-fixes.md`, generalised beyond the terminal. Added
  the `LIVING` status marker and an archive-exemption carve-out to
  `handover-conventions.md` in the same PR. First entry: Tab keypress not forwarded to
  the PTY (category 2 — defect in the existing key-forwarding path, upstream of
  `encode_keystroke`).
- 2026-06-10: **Tab forwarding** resolved (#116, PR #137). Confirmed root cause:
  gpui-component's `Root` view binds `tab`/`shift-tab` to focus navigation in the
  `"Root"` context (an ancestor of every pane), pre-empting the pane's `on_key_down`
  because GPUI dispatches matched key *bindings* before falling through to key-down
  listeners. Fix: shadow both with `NoAction` in the deeper `"Terminal"` context —
  deepest context wins, and a `NoAction` whose `meta == None` yields an empty binding
  set, so the keystroke falls through to the existing `encode_keystroke` path
  (`\t` / `\x1b[Z`). Scoped to `"Terminal"` so Tab still navigates focus in dialogs and
  forms. The context string was extracted to a shared `rift_terminal::TERMINAL_KEY_CONTEXT`
  const, referenced by both the binding and the pane's `key_context`, so a rename cannot
  silently break forwarding across the crate boundary.
- 2026-06-10: **alt+N out-of-range window creation** resolved (#120, PR #139). Category 1
  (completing an existing GUI affordance). When N exceeds the window count, the
  `SelectWindow` handler now sends a single `new-window` via `tmux_command_tx` (tmux
  auto-selects it), mirroring the `+` tab button; N far beyond the count still creates
  exactly one window, not N-M.
- 2026-06-10: **Ctrl+Backspace word deletion** resolved (#121, PR #142). Category 2
  (defect in an existing path). The backspace encoder ignored the Ctrl modifier and
  always emitted DEL (`0x7f`). Decision: emit ESC+DEL (`\x1b\x7f`, readline's
  `backward-kill-word`) rather than `0x17` (`unix-word-rubout`, whitespace-delimited).
  `\x1b\x7f` shares the alphanumeric word boundary of the already-working Ctrl+Left/Right
  (`\x1b[1;5D`) and is the sequence Alt+Backspace already emits, so `cd /pfad/zu` loses
  one path segment per press rather than the whole path. Plain Backspace stays `0x7f`.
- 2026-07-06: **daemon watches $HOME with no `--root`** resolved (#502, PR #557).
  Category 2 (defect in an existing path). Without `--root`, `watched_root` fell back to
  `std::env::current_dir()` — over SSH the launch directory is the login shell's `$HOME`,
  so an unconfigured daemon silently scanned the whole home directory instead of the
  intended project. Decision: remove the fallback; `watched_root` now errors with a
  clear message when the flag is absent, since every sanctioned launch path
  (`crates/ssh/src/launch.rs` via `RIFT_PROJECT_ROOT`, the justfile's default) already
  resolves and passes an explicit root.
- 2026-07-09: **Editor tabs middle-click close + close icon; lighter editor background**
  resolved (#730). Category 1 for the close icon (completing the existing tab-chrome
  affordance — `SessionView`'s window tabs already use `IconName::Close` and
  middle-click, the editor tabs had not caught up); category 2 for the background (the
  surface was pinned to the darkest base token instead of an elevated one). Middle-click
  is wired on the `Tab` itself via `on_mouse_down(MouseButton::Middle, ...)` routed
  through the existing `close_tab` dirty-confirm path, mirroring
  `crates/terminal/src/session_view.rs`'s window-tab convention exactly (left-click still
  activates, the close icon still closes on left-click). Background token: `secondary`,
  already the token this same render function uses for the editor's own immediate chrome
  (the breadcrumb bar, the minimap strip) — the surface now reads as one cohesive,
  elevated panel distinct from the surrounding dock/sidebar chrome, which stays on
  `background`. `muted`/`accent` were tried first (one Catppuccin surface step above
  `background`, vs. `secondary`'s two) but rejected: under Catppuccin Mocha `muted` and
  `accent` resolve to the *identical* hex, and the current-line highlight already washes
  `accent` over the surface at low alpha — an identical surface color flattened that
  highlight to invisibility, caught by a CI test failure
  (`test_editor_surface_background_is_a_subtle_step_lighter_than_base`) before merge.
  `secondary` differs from `accent`, so the highlight stays visible.
- 2026-08-01: **Explorer row font too large / not adjustable** resolved (#908). Category 2
  (defect in an existing path — no explorer-specific font-size setting exists yet; the
  terminal "Font size" slider does not reach the tree, and that global control is the
  seeded roadmap Phase 54, out of scope here). `ROW_HEIGHT` 28px -> 24px and the ambient
  `text_sm` (14px) row label replaced by a new named `ROW_TEXT_SIZE` constant at 12px, a
  denser default matching typical IDE explorers. Root row (`ROOT_ROW_TEXT_SIZE`) and
  header (`HEADER_TEXT_SIZE`) nudged down in step, 12px -> 11px and 11px -> 10px, keeping
  the same ascending header < root < row order the shipped 11/12/14 triple had. Both
  density-lock tests in `crates/app/src/file_tree.rs` updated to the new values.
- 2026-08-01: **Pane header close (X) button** resolved (#907). Category 1 (completing
  the existing pane-header action row — split-right/split-down/zoom already emit tmux
  commands over the shared seam, close had not caught up). The lightweight confirm did
  not need a new dialog component: it reuses the session strip's existing inline
  kill-confirm pattern (`SessionKillConfirm` / `render_session_strip`, #685) rather than
  building a dialog subsystem — a new `PaneKillConfirm` state (keyed by tmux pane id
  instead of session id) swaps the header's action row for an inline "Kill?" +
  confirm/cancel row, armed by `start_pane_kill_confirm` and committed/aborted by
  `confirm_pane_kill`/`cancel_pane_kill`, mirroring the session methods exactly. The
  shell/non-shell branch reuses `is_shell` (#510, already read for the header's type
  glyph): a shell foreground sends `kill-pane` immediately (nothing in-flight to lose,
  matching split/zoom's no-confirm precedent); any other foreground process arms the
  confirm instead, so a stray click can never kill a running process outright.
- 2026-08-01: **AltGr+Plus/Minus triggering font zoom** resolved (#903). Category 2
  (defect in an existing path). The font-zoom guard in
  `crates/terminal/src/pane_view.rs`'s `on_key_down` checked only
  `ks.modifiers.control`, so AltGr — reported as Ctrl+Alt on Windows/Linux
  (`keyboard.rs`'s `encode_keystroke_impl`) — matched the `"+"`/`"="`/`"-"` zoom arm
  and `return`ed before the terminal's AltGr passthrough could send the composed
  character (e.g. German AltGr+` -> `~`, physically the `+`/`~` key). Fix: extracted
  the guard into a pure `font_zoom_delta(&Keystroke) -> i32` that also requires
  `!ks.modifiers.alt`, so it is unit-testable without a GPUI window. The downstream
  sink (`SessionView::apply_font_zoom`) was already correct and untouched; Ctrl+`+`/
  Ctrl+`=`/Ctrl+`-` (no Alt) still zoom.
- 2026-08-01: **Pane resize seam cursor + corner (2-axis) resize** resolved (#906).
  Category 1 (completing the existing border-drag affordance — `resize_handle`
  already dragged the seam, the cursor never changed to say so). `resize_handle`
  now sets `CursorStyle::ResizeLeftRight`/`ResizeUpDown` matching its axis. For the
  corner: `render_layout`'s per-seam loop now calls a new `render_seam`, which checks
  whether the leading or trailing neighbor at that seam is itself a `LayoutNode::Split`
  on the opposite axis (a new `layout::opposite_axis_children` helper) — i.e. whether
  the seam also crosses that neighbor's own internal boundary/boundaries (leading
  preferred, matching the seam's existing "leading child" resize-target convention;
  trailing as fallback so a plain-pane/split-neighbor split, e.g. split right then
  split the new right pane down, is covered too). When it does, the seam renders as a
  mirror of the neighbor's own row/column proportions (same `flex_basis(relative(p))`
  math as the neighbor's real row/column wrappers, so segment boundaries land at the
  same pixels) with a small `nwse`-cursor corner hitzone between segments. `BorderDrag`
  gained a `start: Point<Pixels>` (previously single-axis `Pixels`) and an optional
  `corner: Option<CornerDrag>` so a corner drag emits two independent incremental
  `resize-pane` commands (one per axis) from the same mouse-down origin, reusing the
  existing per-axis delta/`resize_direction` plumbing rather than a new absolute-size
  mechanism.
- 2026-08-01: **Session strip overflow pushing window controls off-screen** resolved
  (#905). Category 2 (defect in an existing path). `render_session_strip` built a plain
  `h_flex()` with no width constraint, and the title bar's left group
  (`title_bar::render`) hosted it with no `min_w_0`/`max_w` either, so with enough
  sessions the strip's unbounded content width pushed "+ New session" and the title
  bar's right-side connection/settings/window controls off-screen. Fix: `flex_1` +
  `min_w_0` on the left group (`title_bar.rs`) and again on the strip's own top-level
  container let it shrink to whatever space remains after the brand, instead of
  forcing its content width onto the row; the chip row itself becomes the
  horizontally scrollable region past that point (`overflow_x_scroll` +
  `track_scroll`, with an overlay `Scrollbar::horizontal` at `ScrollbarShow::Hover` as
  the discoverable affordance, mirroring `SessionPicker`'s vertical scrollbar, #804).
  "+ New session" sits outside the scrollable region as a `flex_none` sibling so it
  stays reachable regardless of session count, never shrinking and never scrolled
  past.
- 2026-08-01: **Explorer has no context menu / create action on empty background**
  resolved (#904). Category 2 (defect in an existing path). `.context_menu(...)` was
  attached only inside `render_row`, so right-clicking empty background space showed
  nothing, and on an empty root — no rows at all — `NewFile`/`NewFolder` (pointer-only,
  no keybinding) were unreachable. Fix scoped to the `EmptyState::EmptyRoot` render
  path (`FileTree::render_empty_root_placeholder`), not the whole `Render::render`
  container the issue's investigation pointed at: `gpui-component`'s `ContextMenuExt`
  registers its right-click listener via `window.on_mouse_event` gated only on
  `hitbox.is_hovered(window)` (a pure spatial check, no propagation-stopping) — a
  `.context_menu()` on any ancestor that also contains rows would still fire (its
  hitbox spans the whole area, including every row) on *every* row right-click,
  alongside the row's own menu, opening both at once. The empty-root placeholder has
  zero rows to collide with, so it is the only container safe to wrap directly.
  `FileTree::create_target_dir` already resolves to the worktree root whenever
  `self.selected` is `None` or the model can no longer find it (guaranteed on an empty
  model), so no extra selection-clearing was needed to target the root.
