# Spec: Session start-directory (spawn panes in the project root)

> Status: COMPLETED
> Created: 2026-07-09
> Completed: 2026-07-10

Give every tmux session rift creates a project-root default working directory, so
its first pane and every new window and split land in the project — not the SSH
login directory (`$HOME`) — without navigating there by hand. Roadmap Phase 34
(single-root; the per-session dynamic root is Phase 35).

## Outcome

- [ ] A session rift creates starts in the project root: its first pane, and every
      new window (the tab `+` button, prefix `c`) and every split, has cwd = the
      project root, not `$HOME` — verified live on the dev channel.
- [ ] The realization is **daemon-local**: the session's default working directory
      is set once at `new-session`, and windows/panes inherit it. The app's
      tmux-command emission (`crates/terminal/src/session_view.rs`) and the
      new-window / split-window strings are unchanged.
- [ ] The root used is the daemon's existing single watched root (the `--root`
      value the daemon already holds); how the reactive layer (file tree / git /
      LSP) is rooted is unchanged. No per-session or dynamic root here.
- [ ] Attaching a pre-existing session whose default directory is not the project
      root re-roots it (via `attach-session -c <root>`), so its new windows/panes
      also land in the project root — not `$HOME`.

## Scope

### In scope

- Thread the daemon's watched root to the tmux control-mode attach and append
  `-c <root>` to `new-session -A -s <session>` in `Attach::spawn`, so a freshly
  created session's default working directory is the project root. The seam is
  **`serve_connection`** (`crates/daemon/src/lib.rs`), which spawns `terminal_task`
  and is shared by both entry points — the real path is
  `serve_uds` / `serve` → `serve_connection` → `terminal_task` → `open_attach` →
  `Attach::spawn`. Concretely:
  - `crates/daemon/src/lib.rs`: `serve_connection` gains a `root: Option<PathBuf>`
    parameter and passes it into the `terminal::terminal_task(...)` spawn.
    `serve_uds` and the stdio `serve` currently *consume* `worktree_root` into
    `watch_worktree` / `watch_lsp`; they retain a clone to hand each connection's
    `serve_connection` call. Every `serve_connection` call site — the two
    production ones and the many test ones — takes the new argument (mechanical).
  - `crates/daemon/src/terminal.rs`: `terminal_task` → `open_attach` →
    `Attach::spawn` gain the `root: Option<PathBuf>`; `Attach::spawn` appends
    `-c <root>` when present. The root is a separate argv entry (not a shell
    string), so it is injection-safe by construction — no quoting helper needed
    (unlike `crates/ssh/src/launch.rs`).
- Extract the argv construction from `Attach::spawn` into a pure, testable helper
  (e.g. `fn spawn_args(session, server_socket, root: Option<&Path>) -> Vec<String>`),
  mirroring the `parse_serve_uds_args` / `watched_root` extraction from
  `archive/spec-daemon-project-root.md`, and unit-test it: with a root the argv
  contains `-c <root>` after `new-session -A -s <session>`; with no root, `-c` is
  absent. (Extraction for testability — a function, not a trait; no
  premature-abstraction concern.)
- Update the `terminal_task` spawn sites for the new parameter — the production
  seam (`serve_connection`) and the test helpers (`terminal.rs`'s `spawn_task` and
  the `terminal_task` spawns in `lib.rs`'s tests). No existing test asserts the
  `new-session` argv (the `terminal.rs` tests are real-tmux integration tests), so
  this is a compile-level update, not a behavioral-test rewrite.
- Re-rooting a pre-existing session on attach (accepted at the gate). The concrete
  command (`attach-session -c <root>` issued from the already-attached control
  client, or a validated equivalent) must be **validated against real tmux** — not
  assumed — before it is committed to; its effect on an existing session's cwd is
  the thing under test. Plus its own command-construction test.

### Out of scope

- **Per-session / dynamic project root** — the watched root and the session root
  stay the single daemon `--root`; making the root follow the active session is
  Phase 35 (`docs/spec-per-session-project-root.md`).
- **The `@root` session-user-option coupling** and any session→root storage —
  Phase 35.
- **The legacy app-side `tmux -CC new-session` fallback** (`crates/app/src/main.rs`,
  around the `new-session -A -s {session}` fallback) — retired by issue #285; this
  phase targets the daemon control-mode path that all UI session creation already
  funnels through (`ClientMessage::Attach` → `Attach::spawn`).
- **Explicit per-call `-c` on split-window / new-window** — unnecessary under the
  set-once approach (see Prior decisions) and would override tmux's idiomatic
  "inherit the session directory" for splits.
- The daemon's watched-root resolution itself (`RIFT_PROJECT_ROOT` / `--root`) —
  already shipped by `archive/spec-daemon-project-root.md`; reused as-is.

## Constraints

- Since tmux 1.9 (`default-path` removed) a `new-window` / `split-window` with no
  `-c` uses the **session's** default working directory, not the active pane's
  `pane_current_path`. Setting that directory once at session creation therefore
  makes every later window and split inherit the project root — the basis of the
  set-once approach.
- `-c` on `new-session -A` applies only when the session is **created**; when `-A`
  attaches an already-existing session, `-c` is a no-op. Re-rooting a pre-existing
  session needs a separate `attach-session -c <root>` (the gate decision).
- In `--serve-uds` mode the daemon refuses to start without `--root` (issue #502),
  so a root is present in production. The stdio `serve` path also carries a
  `worktree_root: Option<PathBuf>`; threading through the shared `serve_connection`
  seam means stdio sessions get `-c <root>` too when a root is present — the
  consistent behavior, not something to suppress. `root` stays `Option` (and `-c`
  is omitted when `None`) for the **test** call sites, which build connections
  with no root.
- The daemon builds tmux argv as a `Vec`, not a shell string — `-c <root>` is a
  plain arg, inherently injection-safe. No `shell_single_quote` needed here.
- Best-effort side channel: a bad root must degrade (tmux may reject `-c`), never
  abort the daemon (`docs/constitution.md`: binaries use `anyhow`, degrade + log).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| **Set-once via the session default directory**, not per-call `-c` on split/new-window | tmux (≥1.9) inherits the session's default working directory for `new-window`/`split-window` when `-c` is omitted; setting it once at `new-session -c <root>` makes every window/pane land in the project root. Keeps the change daemon-local — `session_view.rs` and the app's tmux-command strings stay untouched, respecting the crate boundary. Roadmap-flagged open decision, resolved here by the tmux constraint. | 2026-07-09 |
| Realize the change **daemon-side** at `Attach::spawn`, threading the root through the shared `serve_connection` seam: `serve_uds` / `serve` → `serve_connection` → `terminal_task` → `open_attach` → `Attach::spawn` | All session creation (UI new-session prompt, switch, `+`, prefix `c`) funnels through `ClientMessage::Attach` → the daemon's `new-session -A`; the daemon already holds the watched root, and `serve_connection` is the single seam both entry points spawn `terminal_task` through. One chokepoint covers every path. | 2026-07-09 |
| Root source = the daemon's existing single `--root` (single-root scope) | No per-session root in this phase; dynamizing the root is Phase 35. Reuses the shipped `archive/spec-daemon-project-root.md` resolution unchanged. | 2026-07-09 |
| `-c <root>` passed as a separate argv entry (no quoting helper) | The daemon spawns tmux via `Command::args`, not a shell; a path arg cannot inject. Contrast `launch.rs`, which builds a shell string and must single-quote. | 2026-07-09 |
| **Re-root a pre-existing session on attach** (via `attach-session -c <root>` or a validated equivalent), so a session whose default dir is not the project root (created outside rift in `$HOME`, or persisted from before this change) also lands its new windows/panes in the project root | Resolved at the spec-acceptance gate: completes the "never land in `$HOME`" outcome for the whole session set and avoids a one-time session-recreate migration for the live dogfooding `rift` session; consistent with the single-root "session = project" model. Caveat: overrides a deliberately-chosen session dir (acceptable under single-root; Phase 35 makes it per-session). | 2026-07-09 |

## Prior art

From `docs/prior-art.md` → "Session ↔ project root coupling — prior-art index
(Phases 34–35)":

- **Start-directory for new panes / windows / sessions** — tmux `new-session -c`
  and `attach-session -c` (session default dir, inherited by windows/panes); the
  `#{pane_current_path}` inherit pattern; `workmux` pane `-c` config. Verdict:
  **reuse** the tmux-native `-c`; avoid the removed `default-path`.
- **"session = project" naming + create-with-dir convention** — `sesh`,
  `tmux-sessionizer`, `tmuxinator` / `smug`. Verdict: **reference** the
  session-dir convention; rift creates sessions itself via `new-session -A`, not
  as an external session-spawner.

## Human prerequisites

- None. The project root is already provided by the shipped `--root` /
  `RIFT_PROJECT_ROOT` mechanism; this phase only threads that value into the
  `new-session` invocation.

## Tracking

The decomposition into steps lives as GitHub issues, one per implementable step,
grouped under the milestone. This spec owns the design; the issues own progress.

- Milestone: created at the spec-acceptance gate
- Issues: created from this spec after merge (one per implementable step)

Each issue references this spec path in its body.

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace
      excluding `rift-app`).
- [ ] Unit (`crates/daemon`): the extracted `spawn_args` helper, with a root,
      builds `new-session -A -s <session> -c <root>` (argv contains `-c` then the
      root); with no root, `-c` is absent. Plus the re-root command-construction
      test.
- [ ] The `terminal_task` / `serve_connection` call sites compile with the new
      `root` parameter (production seam + test helpers).
- [ ] Behavioral (dev-channel QA gate): create a fresh rift session; open a new
      window (`+` and prefix `c`) and a split; `pwd` in each is the project root,
      not `$HOME`. Attaching a session whose default dir was `$HOME` re-roots its
      new windows to the project root.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `-c` interacts unexpectedly with `-A` when the session already exists (silently ignored) | Documented in Constraints/Prior decisions: `-c` applies on create only; the re-root path (if approved) covers pre-existing sessions. Command-construction unit test pins the argv. |
| A stale session persisted from before this change keeps its `$HOME` default dir | Same class as the 3.5 stale-daemon migration: a one-time re-create (or the re-root decision) fixes it; documented at the QA gate. |
| tmux rejects a bad/nonexistent `-c` path | Not realistic in production (the root is the already-validated watched root). If it occurred, `new-session` fails and surfaces as this attach ending (`TerminalExit`); the daemon process survives — only this one attach ends, not the daemon. |

## Decision log

- 2026-07-09: Spec drafted for roadmap Phase 34 from the "Session ↔ project root
  coupling" seed. The set-once-vs-per-call open decision the roadmap flagged is
  resolved to **set-once** by the tmux ≥1.9 constraint (windows/splits inherit the
  session default dir), which also keeps the change daemon-local. One genuinely
  open decision carried to the acceptance gate: re-rooting a pre-existing session.
- 2026-07-09: Spec review (VERDICT REQUEST_CHANGES → addressed): corrected the
  plumbing path — `terminal_task` is spawned by the shared `serve_connection` seam
  (`lib.rs:979`), not directly by `serve_uds`, so the root threads
  `serve` / `serve_uds` → `serve_connection` → `terminal_task` → `open_attach` →
  `Attach::spawn` and the change spans `lib.rs` (the seam + its call sites) and
  `terminal.rs`. Clarified that the stdio `serve` path also gets `-c` (only test
  call sites are rootless); specified the `spawn_args` helper extraction for the
  unit test; reworded the "update tests" scope (no argv-asserting test exists — it
  is a compile-level call-site update) and the bad-`-c` risk wording; flagged that
  the re-root command must be validated against real tmux if the gate approves it.
  The design itself (set-once daemon-side, tmux inheritance, session_view.rs
  untouched, scope vs Phase 35) was confirmed sound.
- 2026-07-09: Spec-acceptance gate. Resolved the open decision — **re-root a
  pre-existing session on attach** (`attach-session -c <root>`), to complete the
  never-`$HOME` outcome and avoid a session-recreate migration for the live
  dogfooding session. Human prerequisites: none. Status flipped `DRAFT` → `READY`
  in the same PR; milestone `Phase 340` created at acceptance.
- 2026-07-09: Issue #726 implementation. Threading `root: Option<PathBuf>` through
  `serve_connection` (already at 7 parameters) pushed it to 8, past clippy's
  `too_many_arguments` default threshold; annotated it
  `#[allow(clippy::too_many_arguments)]`, the same pattern already used at
  `crates/terminal/src/pane_view.rs` and `crates/app/src/editor.rs`, rather than
  bundling the connection's wiring into a struct — an unrelated refactor out of
  scope for this step.
- 2026-07-09: Issue #727 implementation — re-root command validated against a
  real tmux 3.4 server (`tmux -L rift-reroot-test`, killed after each run).
  Findings, driving a real `-C` control-mode child via its stdin/stdout (a bare
  CLI `new-window` was *not* representative: run from a client with no
  attached session, tmux falls back to that command client's own invocation
  cwd rather than the session default dir — server_client_get_cwd's
  command-client branch — so only a genuinely attached control client
  exercises the session-inherits-default-dir path the spec's Constraints
  section relies on):
  - `attach-session -c <root>`, sent with **no `-t`**, over the control-mode
    connection already attached to the target session, sets that session's
    `#{session_path}` to `root`; a subsequent `new-window` from the same
    client then lands in `root`. Omitting `-t` targets the client's own
    current session, so the session name is never embedded in the command
    line — no quoting/injection surface for it at all. (`-t <session>` also
    works, targeting explicitly, but is unnecessary here.)
  - Re-issuing it for a session already at `root` (the freshly-created-session
    case) is a harmless no-op behaviorally: tmux applies the same path, and
    the `%session-changed` it emits carries this attach's own unchanged
    session id, which the existing `Event::SessionChanged` handler already
    treats as `switched = false` (no spurious layout re-query from that event
    specifically; the `%window-add`/`%session-window-changed` also observed
    on the wire are already-handled structural-change events, coalesced by
    the existing `layout_dirty` logic — no new handling needed).
  - The command string is parsed by tmux's own control-mode lexer (unlike
    `spawn_args`, which is real process argv), so `root` must be quoted:
    confirmed empirically that an **unquoted** root containing a space
    (`/tmp/rift reroot project`) makes tmux reply
    `parse error: command attach-session: too many arguments (need at most 0)`
    and leaves the session unrerooted, while the same path **single-quoted**
    (tmux-lexer style, embedded `'` escaped as `'\''`) re-roots correctly —
    verified for both a plain space and an embedded single quote. Implemented
    as a small `quote_tmux_arg` duplicated from
    `crates/terminal/src/tmux_quote.rs` rather than shared, since the daemon
    and `terminal` crates stay independent and this is the daemon's one
    command line with a dynamic, unbounded value.
  - No version caveat found within this validation (tmux 3.4, the version
    available in this environment); `-c` on `attach-session` is documented
    back through tmux's stable manpage history with the same "sets the
    session working directory (used for new windows)" wording, consistent
    with the spec's Constraints section.
  - Review follow-up: `quote_tmux_arg`'s in-line escaping cannot neutralize a
    `\n`/`\r` in `root`, since `Attach::send_command` frames each command as
    one control-mode line before tmux's lexer runs — `reroot_command` now
    guards this and returns `None` (skip the re-root, warn) rather than ever
    sending a split, partially-unquoted command, preserving the spec's
    per-attach containment guarantee (a bad `-c` ends only this attach, never
    the daemon or the tmux server).
