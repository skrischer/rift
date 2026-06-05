# Spec: Terminal interaction fixes (dogfooding)

> Status: READY
> Created: 2026-06-04
> Completed: —

Closes a batch of terminal/tmux interaction defects found during dogfooding: scrollback that desyncs from a native client, no font zoom, no inter-pane resize, no pane zoom. These predate the SDD process; this spec gives them a design anchor. The largest related concern — mirroring tmux's full key-table so configured keybindings work — is intentionally split into its own spec (see Pending document updates).

## Design framing: why these are GUI affordances, not workarounds

rift runs over tmux control mode (`-CC`), which delivers structure (per-pane `%output`, `%layout-change`, pane lifecycle) at the cost of tmux's rendered interactive layer (copy-mode, key-table, status line). That trade is deliberate and is the durable reason rift can draw separate GPU panes at all — the full rationale, the rejected alternative, and the exit criteria belong in `architecture.md` (see Pending document updates), not here.

What matters for *this* spec is splitting the "lost native features" into two honest categories, because it determines whether each fix is a workaround or the correct design:

1. **Features rift replaces with something better (a GUI affordance).** Scrollback → GPU-native, mouse-driven history rendering. Pane resize → border drag. Font zoom → `Ctrl+=`/`Ctrl+-`. Pane focus/zoom → click / native shortcut. A native IDE inheriting tmux's text-rendered copy-mode and prefix-driven resize would be *failing at being a GUI*. These are not workarounds for a `-CC` limitation; they are rift being a GUI. This is iTerm2's posture too (`capture-pane`-backed history rather than forwarded copy-mode).
2. **The one feature genuinely forgone.** A scroll/copy-mode viewport *shared live* with a separate native client attached to the same session. `-CC` cannot deliver this and rift does not need it (see Out of scope).

All four Outcomes below fall into category 1.

## Outcome

What is true when this work is done:

- [ ] Scrolling a pane shows the pane's real tmux scrollback history (fetched via `capture-pane`), not only the lines streamed since attach
- [ ] `Ctrl+=` / `Ctrl+-` change the rendered font size; the resulting cols/rows are forwarded to tmux via `set_client_size`, and tmux reflows all panes (whole-client zoom)
- [ ] Dragging the border between two panes resizes them in tmux (`resize-pane`); the layout updates from the next snapshot
- [ ] A rift-native shortcut toggles pane zoom (`resize-pane -Z`); the zoomed pane fills the window and the snapshot-driven layout follows

## Scope

### In scope

- `capture-pane`-backed scrollback for the active pane: live `scroll_display` for post-attach, a one-time static pre-attach history block above it (see Scrollback architecture), including resize invalidation, alt-screen suppression, and capture-error reset. Depends on a termy capture-signature change.
- Client-side font scaling on `Ctrl+=` / `Ctrl+-`, propagated through the existing resize path
- Mouse drag handlers on the pane-split borders, emitting `resize-pane`
- Pane zoom toggle via a rift-native shortcut, emitting `resize-pane -Z`

### Out of scope

- **tmux key-table mirroring** (make all configured tmux keybindings work while focused) — own spec, see Pending document updates. This is the heavy item (prefix state machine, `list-keys` parsing, mode tables) and must not block the small fixes here.
- **Leader / prefix bindings** — depend on key-table mirroring; deferred with it. Pane zoom therefore ships on a rift-native shortcut now, and gains its natural `prefix`-based binding once mirroring lands.
- **Per-pane font zoom** — `-CC` exposes a single client size; per-pane cell counts would desync from tmux's layout. Whole-client zoom only.
- **Live cross-client scroll sync** — sharing a scroll position with a separate native client is not achievable over `-CC` (this is the one genuinely-forgone feature from the design framing above); `capture-pane` gives rift its own faithful history, not a synced viewport.

## Constraints

- tmux 3.4+ (hard requirement since Phase 2a).
- Transport is tmux control mode (`-CC`). Input today goes out via termy `send_input` → `send-keys -t <pane> -H <hex>`.
- termy exposes the history primitive `TmuxClient::capture_pane` (`crates/terminal_ui/src/tmux/client.rs:541`), today used only for attach/switch hydration (`src/terminal_view/runtime/tmux/snapshot.rs:92`). Its current signature hardcodes `capture-pane … -J -S -<n> -E -` (`payload.rs`): end fixed at `-` (last line), join-wrapped always on. A line-exact, overlap-free pre-attach capture needs an extended signature (parametrizable `-E`, optional `-J`). Extend that one seam — do not add a parallel mechanism.
- Arbitrary commands with string output are available synchronously via termy `send_command` (`%begin`/`%end` framed) — used by `resize-pane` etc.
- All tmux command/input emission must go through one narrow interface (today `TmuxClient` via the existing flume channels: `input_tx`, `size_changed_tx`, `tmux_command_tx`). Do not reach into `alacritty_terminal::Term` internals for scrollback content. This keeps the Phase 3 transport swap (`TmuxClient` → daemon protocol) a single-seam change and leaves the deferred VTE-location decision untouched.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Scrollback via `capture-pane`, not copy-mode forwarding | Two reasons, not one. (1) It is the right GUI design: rift renders its own GPU-native, mouse-driven history rather than inheriting tmux's text-rendered copy-mode (category 1 in Design framing; iTerm2 does the same). (2) Forwarding is also technically impossible over `-CC`: tmux does not deliver copy-mode/choose-mode rendering to control-mode clients (tmux Control Mode wiki), so forwarding the wheel would leave rift's viewport blank and force the shared pane into copy-mode, breaking the other client. | 2026-06-04 |
| Scrollback: one-time pre-attach block over live scroll, not a per-tick pager | A pager re-capturing per scroll tick is O(n) on a blocking thread and discards the live `Term`'s working post-attach scrollback. The only defect is the pre-attach gap; capture it once (see Scrollback architecture). | 2026-06-05 |
| Requires a termy capture-signature change (`-E` parametrizable, `-J` optional) | termy hardcodes `-J … -E -`; an overlap-free, line-exact pre-attach capture is impossible otherwise. One method on the single `TmuxClient` seam, not a parallel mechanism. Owner (parallel termy track vs. own branch) decided after this redesign. | 2026-06-05 |
| Zoom is whole-client, not per-pane | `-CC` has a single client size (`refresh-client -C`); font size is a client render property. This matches the native terminal `Ctrl+=` behaviour the user expects. | 2026-06-04 |
| Pane zoom triggers on a rift-native shortcut for now | tmux prefix bindings cannot work over `send-keys` until key-table mirroring exists; a rift-native shortcut ships the feature without blocking on that spec. | 2026-06-04 |
| Pane zoom (`resize-pane -Z`) is moved here from Phase 2d's deferral | Phase 2d (`spec-phase2d-tabbar.md`) lists pane zoom as out-of-scope/deferred. This spec takes ownership; 2d's tracking is updated to point here (see Pending document updates). | 2026-06-04 |
| tmux key-table mirroring is a separate spec | It is an order of magnitude larger than these fixes (prefix state machine, `list-keys` parse, per-mode key tables) and would otherwise sink the small-fix batch this spec exists to ship. | 2026-06-04 |

## Scrollback architecture (revised 2026-06-05 after design challenge)

An earlier "pager" design — `capture-pane` as the single source for everything above a screen-only live `Term`, re-fetched as the user scrolls — was challenged and rejected. The decisive findings:

- termy's `capture_pane` hardcodes `capture-pane … -J -S -<n> -E -` (`payload.rs`): the end is fixed at `-` (last line) and `-J` (join wrapped lines) is always on. There is no way to request a history-only range (`-E -1`) or unwrapped output, so the pager model's overlap-free premise is unbuildable without a termy change.
- With the only available range (`-E -`), a capture contains history **plus** the visible screen; compositing `history ++ live-screen` double-renders the screen. A line-count trim cannot fix it because `-J` makes captured logical lines diverge from the live grid's wrapped rows.
- Re-capturing per scroll tick is O(n) bytes + O(n) VTE re-parse on a blocking thread — visible stalls over SSH.

Revised model — **live scroll for post-attach, a one-time static block for pre-attach:**

1. Post-attach scrollback keeps using `term.scroll_display(Scroll::Delta)` on the live `Term` (the original, working behaviour). The only real defect is the pre-attach gap.
2. The pre-attach history is captured **once** (lazily, on the first scroll past the top of the live `Term`'s own scrollback), parsed into rows, and rendered as a static block **above** the live scrollback. It is the past before attach, so it never changes — no streaming seam, no per-tick work.
3. Re-capture only on resize (rare), never on scroll.

This requires a termy capture-signature change (the gating decision — owner TBD): a parametrizable `-E <end>` so the capture excludes the region the live `Term` already holds, and an optional `-J` (off) so captured lines map 1:1 to grid rows and the seam is line-exact. One extra method on `TmuxClient` — consistent with the single-seam constraint.

Edges the challenge surfaced, now in scope:

- **Wrapped lines:** with `-J` off, one captured line maps to one grid row; the capture parser sizes its scratch `Term` to the real row count, tested with a line exceeding `cols`. (The discarded first seam had a latent bug here: it sized by `\n` count, so a wrapped line silently dropped upper history.)
- **Resize / font-zoom (#40):** changing `cols` invalidates the parsed block; mark stale and re-capture.
- **Alternate screen (vim/less/htop):** no history scrollback while the live `Term` is in alt-screen — capture returns alt-screen content; this matches native terminals.
- **Capture error/timeout:** clear the in-flight flag and allow retry; never wedge scrolling.

## Implementation notes (non-binding)

Integration points surfaced during investigation, for the implementor:

- Scroll handler: `crates/terminal/src/pane_view.rs:963-1020` (`on_scroll_wheel`) — keep `term.scroll_display(Scroll::Delta)` for the post-attach region; on reaching the top of the live scrollback, fetch the pre-attach block once and render it above (see Scrollback architecture above).
- Key interception (font zoom, pane-zoom shortcut): insert in `on_key_down` at `crates/terminal/src/pane_view.rs:751-786`, before the `encode_keystroke` call (line 775), alongside the existing `Ctrl+Shift+C/V` early returns. No global GPUI key bindings compete here.
- Resize emission path: `size_changed_tx` → `crates/app/src/main.rs:215-224` → `set_client_size`. Font zoom recomputes cols/rows and reuses this.
- Pane borders: `crates/terminal/src/session_view.rs:render_layout` (~line 251-266) draws `border_r_1`/`border_b_1` with no handlers — add drag handlers that emit `resize-pane` via `tmux_command_tx`.

## Tracking

The decomposition into steps lives as GitHub issues, one per Outcome, grouped under a milestone. This spec owns the design; the issues own progress.

- Milestone: [Terminal interaction fixes](https://github.com/skrischer/rift/milestone/3)
- Issues: scrollback via capture-pane (#39), whole-client font zoom (#40), drag-to-resize panes (#41), pane zoom shortcut (#42) — one per Outcome above

Each issue references this spec path in its body. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Scrolling up in a pane reveals pre-attach history identical to `tmux capture-pane` output for that pane
- [ ] `Ctrl+=` / `Ctrl+-` visibly rescale the font and the statusbar cols×rows changes; a parallel native client attached to the same session reflows to the new size
- [ ] Dragging a pane border changes the split ratio and persists in the tmux layout (visible to a native client)
- [ ] The pane-zoom shortcut toggles a pane to fill the window and back, matching `resize-pane -Z`

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `capture-pane` over a large `history-limit` is slow / can time out on reattach | One-time pre-attach capture (not per scroll tick); bound the captured range; on error clear the in-flight flag and allow retry so scrolling never wedges |
| `-J` (join) makes captured logical lines diverge from the live grid's wrapped rows, so the history/live seam cannot be line-exact | Capture with `-J` off (needs the termy signature change); size the scratch `Term` to the real wrapped row count; test with a line exceeding `cols` |
| `cols` changes (resize / font-zoom #40) leave the parsed pre-attach block at the wrong width | Mark the block stale on size change and re-capture; it is the only re-capture trigger besides the first scroll |
| Alternate-screen apps (vim/less/htop) make `capture-pane` return alt-screen content, not history | Suppress the history block while the live `Term` is in alt-screen, matching native terminals |
| Drag-to-resize pixel→cell mapping is imprecise near borders | Convert delta using the measured cell size; snap to whole cells; let the snapshot be the source of truth for the final layout |
| Pane-zoom shortcut collides with a user's tmux/app binding | Use a rift-namespaced chord (e.g. `Ctrl+Shift+Z`); revisit once key-table mirroring can detect conflicts |
| Reaching into `Term` scrollback would couple to the deferred VTE-location decision | Hard constraint above: scrollback content flows through the seam, not `Term` internals |

## Decision log

Decisions made during implementation are appended here.

- 2026-06-04: Spec drafted from dogfooding triage. Copy-mode scroll forwarding rejected as technically impossible in `-CC` (see Prior decisions); key-table mirroring split out.
- 2026-06-05: Challenged the scrollback implementation design (dedicated challenger session over code + termy source + tmux semantics). The "pager" model (capture-pane as single source above a screen-only live `Term`, re-fetched per scroll) was **rejected**: (S1, critical) termy hardcodes `-J … -E -`, so the overlap-free `-E -1` range it relies on is unreachable without a termy change — the "no termy bump" premise was factually wrong; (S2, critical) the available `-E -` range double-renders the screen and `-J` defeats a line-count trim; (S3) the first seam draft (`parse_capture_to_rows`) had a latent bug sizing the scratch `Term` by `\n` count, silently dropping upper history on wrapped lines; (S4–S6) per-tick re-capture is O(n) on a blocking thread and the frozen-history/live-screen seam tears under streaming. Adopted the revised model (live `scroll_display` for post-attach + one-time static pre-attach block above), which needs a termy capture-signature change (`-E` parametrizable, `-J` optional). The first seam (request/response plumbing) is discarded and rebuilt from scratch under the revised model. Open: termy-change ownership (parallel termy track vs. own branch).
- 2026-06-05: Resolved the termy capture-signature ownership left open above. Added `capture_pane_range` (parametrizable `-E`, optional `-J`) to the skrischer/termy fork (`feat/capture-signature`, rev `49d3928`, layered on the subscriptions superset) and pinned rift to it; upstreamed as a separate single-responsibility PR (lassejlv/termy#322) rather than folding into the open subscriptions PR, since the two changes are orthogonal.
- 2026-06-04: Challenged the `-CC` choice (3-agent fan-out over docs/code/architecture). Outcome: `-CC` confirmed — the rejected alternative (tmux-in-one-PTY, native rendering) gives free interactive features but zero pane structure, deleting rift's reason to exist. Reframed the "lost features" into two categories (replaced-with-a-better-GUI-affordance vs. genuinely-forgone) so the fixes read as GUI design, not workarounds. Surfaced that the `-CC` decision is undocumented; expanded Pending update #1 to record it in `architecture.md` with the rejected alternative and the WezTerm-mux exit criteria.

---

## Pending document updates

Applied when this spec moved to `READY` (2026-06-04):

1. **`docs/architecture.md`** — added the section "tmux control-mode interaction model" recording the `-CC` decision, the rejected alternative (tmux-in-one-PTY = structure loss), the durable contract (consequences for keybindings/scrollback/zoom/resize), and the WezTerm-mux exit criteria.
2. **`docs/spec-phase2d-tabbar.md`** — updated the pane-zoom "Out of scope" line to note it is now owned by this spec.
3. **`docs/spec-tmux-keytable-mirroring.md`** — created as a DRAFT stub for the deferred key-table mirroring design. No milestone/issues until promoted.
4. **`docs/roadmap.md`** — added the interaction-fixes parallel track and listed key-table mirroring as planned.
