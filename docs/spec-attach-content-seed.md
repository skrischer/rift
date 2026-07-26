# Spec: seed visible pane content on attach

> Status: READY
> Created: 2026-07-26
> Completed: —

When the daemon attaches a control-mode child to a tmux session, seed each pane's
CURRENT visible screen from the server (`capture-pane`) so the pane shows its real
content immediately — instead of staying blank until the pane produces new output.
This closes #897 (switching to an already-running session leaves the pane
un-repainted) and the same gap on first-connect to a pre-populated session.

## Why (root cause, spike-confirmed)

tmux control mode **never pushes a pane's pre-existing screen to a freshly-attached
control client** — it streams `%output` only for output produced AFTER attach.
Verified against live tmux 3.4 (spikes in #897): a fresh attach to an idle session
with on-screen content yields only `%session-changed`, no `%output`; no
`refresh-client` variant (same-size `-C`, bare, `-S`) resends it; even a resize only
redraws the current line. The content is retrievable solely via `capture-pane`
(which returns it, alt-screen included). "Resize fixes it" in the app works only
because `SIGWINCH` makes the pane's own program redraw itself (post-attach output),
not a tmux redraw. A freshly CREATED session appears to work today only because its
shell prints its first prompt AFTER attach (the daemon's `test_attach_*` "initial
draw" is exactly this post-attach prompt) — so the defect is specific to content
that predates the attach.

## Outcome

- [ ] On attach to a session whose panes already hold content, each pane's current
      visible screen is delivered to the client and rendered immediately — no
      manual resize, no waiting for new output.
- [ ] The same holds on an in-app session switch (a switch re-attaches a fresh
      control child, `spec-session-switch.md`), including when both sessions share
      the client grid size — closing #897.
- [ ] A pane on the alternate screen (a full-screen agent TUI) renders its
      alternate-screen content correctly after the seed, not as normal-buffer text.
- [ ] The seed composes cleanly with the live `%output` stream: no duplicated or
      corrupted rows when the pane produces output right after attach.
- [ ] Agent-agnostic: the seed derives only from `capture-pane` + pane format
      queries over the control stream; no agent detection, no output parsing.

## Scope

### In scope

- `daemon`: after the attach's `LAYOUT_QUERY` resolves, issue one
  `capture-pane -p -e -t %<pane>` per pane in the attached session (visible screen
  only, no `-S`/scrollback). Reuse the existing command-emission + `%begin/%end`
  correlation, but route the reply through a **distinct in-flight set** (e.g.
  `seed_captures`) whose replies emit `DaemonMessage::PaneOutput` — **not**
  `self.captures`, whose replies emit `PaneCapture` and land in the client's
  SCROLLBACK, not the live screen (`terminal.rs`: `captures` insert ~:688, reply
  ~:874, `PaneCapture`). Delivering the seed on `PaneOutput` (the same channel
  `%output` uses) makes the client's `Term` render it with zero new protocol
  surface and no client change.
- `daemon`: make the seed render correctly by querying the pane's screen state in
  the same round-trip and framing the captured rows accordingly —
  `#{alternate_on}`, `#{cursor_x}`, `#{cursor_y}` (one `display-message`/format read
  per pane, or folded into the capture correlation): a normal-screen pane is framed
  `ESC[2J ESC[H` + rows; an alternate-screen pane is additionally wrapped with the
  alt-screen enter (`ESC[?1049h`) so the client `Term` switches buffers before the
  rows land; the cursor is restored last (`ESC[<y+1>;<x+1>H`).
- Ordering: the seed is emitted once per attach, before or interleaved with the
  first live `%output`; because tmux sends no pre-existing content, the seed and the
  subsequent delta stream do not overlap (spike-confirmed) — normal terminal
  semantics compose them.

### Out of scope

- Scrollback / pre-attach history — already handled by the on-demand
  `CaptureRequest` path (`spec-*`); this seeds only the VISIBLE screen.
- Any client-side (`terminal`/`app`) change: the fix is daemon-side content
  delivery on the existing `PaneOutput` path; the render layer already renders
  whatever bytes reach the `Term`. (If review finds a render-layer gap, it splits to
  its own issue — not assumed here.)
- A new protocol message: the seed reuses `PaneOutput`; no `protocol` change.
- tmux versions < 3.4 (the project's hard floor, `docs/tmux-reference.md`).

## Constraints

- Control-mode contract (`docs/architecture.md`): everything via `%begin/%end`-guarded
  commands over the single control stream; `capture-pane`/`display-message` only,
  never a rendered chooser. `#{...}` format args MUST be single-quoted on the command
  line (`docs/tmux-reference.md` pitfall 9).
- Agent-agnostic (`docs/constitution.md`): the seed is a raw screen snapshot; no
  detection or parsing of what runs in the pane.
- `capture-pane` payloads are octal-escaped and split on `%output` boundaries like
  any control output — reuse the existing `unescape`/UTF-8-buffering path
  (`docs/tmux-reference.md` pitfalls 1, 8), do not add a parallel decoder.
- No `.unwrap()` in library code; crate boundaries via `lib.rs`; a parser/framing
  helper is tested with valid + malformed capture input (`docs/constitution.md`).
- Bounded: one capture + one format read per pane, once per attach — proportional to
  pane count, not to output volume.

## Prior art

- `docs/prior-art.md` → tmux Control Mode (Category 3 #1) + **iTerm2 tmux
  integration** (Category 3 #5, `TmuxController`/`TmuxGateway`, GPL-2.0, architectural
  reference only): iTerm2's async pane materialisation seeds a pane's initial content
  via `capture-pane` on attach for exactly this reason — control mode does not replay
  the screen. Reference only; the mechanism here is derived and spike-hardened, no
  code is copied.
- `docs/prior-art.md` → **WezTerm `termwiz::tmux_cc`** (control-mode parser
  reference — `%output`/`%begin`/`%end` framing, `unvis` octal decode) and
  **smtg-ai/claude-squad `session/tmux/`** for `capture-pane` usage. NB
  claude-squad's *content hashing* of `capture-pane` is on the AVOID list
  (agent-specific) — only the capture mechanism is borrowed, never output
  interpretation.

## Human prerequisites

None — runs against the existing SSH host + tmux 3.4 server; no secrets, accounts,
or external provisioning.

## Tracking

- Milestone: created at acceptance.
- Issues: one per implementable step, each referencing this spec path. #897 is
  repointed here as the implementing bug (off `spec-session-switch.md`).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Daemon integration test (real tmux, mirroring `test_attach_*`): create a
      session, draw a marker, WAIT so it is idle pre-attach, attach → assert a
      `PaneOutput` carrying the marker arrives without any resize or new keystroke
      (the regression that today's tests miss because they only cover the
      post-attach prompt draw)
- [ ] Framing helper unit tests: normal-screen and alternate-screen captures frame
      to the expected byte sequence (valid + malformed capture input), including a
      full-width last row / bottom-row case that locks the no-scroll invariant (a
      naive replay of a full bottom row must not push the grid up a line)
- [ ] Behavioural (dev channel, #897): switch A→B→A between two same-size sessions,
      each running content (one an alt-screen agent TUI) — the target pane always
      shows its current content immediately; the TUI renders as its alt-screen, not
      as scrolled text
- [ ] First-connect to a pre-populated running session shows content immediately

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Seed duplicates rows if tmux DID send some pre-existing output | Spike-confirmed tmux sends none; the seed is the at-attach snapshot and live `%output` are subsequent deltas — normal terminal semantics compose them |
| Alt-screen pane rendered as normal text | Query `#{alternate_on}`; wrap the seed in `ESC[?1049h` so the `Term` switches buffers first |
| Cursor lands at the wrong cell after the seed | Query `#{cursor_x}`/`#{cursor_y}`, restore with a final `ESC[H`-family sequence |
| Capture round-trips flood a large session | One capture + one format read per pane, once per attach; bounded by pane count |
| A wide/unicode capture splits mid-escape across `%output` chunks | Reuse the existing UTF-8-buffering + `unescape` path, not a parallel decoder |

## Decision log

- 2026-07-26: Spec created from the #897 spike. Root cause: tmux control mode never
  replays pre-existing screen content to a fresh control client (only post-attach
  `%output`); the fix is a daemon-side `capture-pane` seed on attach. Supersedes the
  `spec-session-switch.md` 2026-07-24 "same-size `refresh-client -C` no-op"
  hypothesis, which the spike REFUTED as the fix mechanism (no `refresh-client`
  variant resends content). The size-dependence it observed is a real but secondary
  symptom (a resize makes the pane's own program redraw); the primary cause is the
  absent initial-content push.
- 2026-07-26: Seed reuses the existing `PaneOutput` channel + `capture-pane`
  scrollback plumbing rather than a new protocol message — the client `Term` renders
  seed bytes identically to `%output`, so no `protocol` change and no client change
  (constraint: minimal API surface, `docs/constitution.md`).
- 2026-07-26: Alt-screen handled via `#{alternate_on}` + `ESC[?1049h` wrapping
  rather than attempting to reconstruct app state — `capture-pane -p -e` returns the
  visible (alternate) cells; wrapping switches the client buffer so they render in
  the right plane. Spike-confirmed `#{alternate_on}` is queryable and `-e` preserves
  the styling.
- 2026-07-26 (implementation, #897): the seed fires once, off the attach-time
  **layout snapshot** reply (`!snapshot_sent`), reusing the panes the layout query
  already returned — no extra pane enumeration. Panes split AFTER attach arrive as
  `LayoutUpdate`s and are NOT seeded: a freshly split pane draws its own prompt as
  post-attach `%output`, so only pre-existing content needs the seed. Each pane
  pairs two correlated commands — `display-message -p -t %<pane>
  '#{alternate_on}\t#{cursor_x}\t#{cursor_y}'` and `capture-pane -p -e -t %<pane>`
  — tracked in a per-pane `SeedCapture` (mirroring `KeyTableQuery`'s two-leg
  pairing) whose reply arm is checked BEFORE `captures`, so a seed capture is never
  emitted as a scrollback `PaneCapture`. Framing (`frame_seed`, pure + unit-tested):
  optional `ESC[?1049h`, then `ESC[2J ESC[H`, rows CRLF-joined with the LAST row
  left unterminated (the no-scroll invariant for a full-width bottom row), then a
  1-based cursor restore from the 0-based tmux coordinates. A malformed/errored
  format reply degrades to `SeedFrame::default` (normal screen, home) rather than
  dropping the pane.
