# Spec: Per-session project root (root follows the active session)

> Status: READY
> Created: 2026-07-09
> Completed: —

Make the daemon's watched root follow the active tmux session: each session's
project root is coupled to the session via a session-scoped `@root` user option,
and a session switch re-roots the reactive layer (file tree / git / LSP) to the
new session's root — superseding the single, connect-time-baked `RIFT_PROJECT_ROOT`.
Roadmap Phase 35; codifies "tmux session = project". Depends on Phase 34 (the
`new-session` chokepoint) and the Phase 32/33 session switch/list/pick surface.

## Outcome

- [ ] Switching sessions re-roots the reactive layer: after a switch, the file
      tree, git status, and diagnostics reflect the newly active session's
      project root, not the previous one — verified live on the dev channel.
- [ ] Each session's root is coupled to the tmux session via a session-scoped
      `@root` user option, stamped by the daemon when it creates the session, and
      resolved to the session's working directory (`#{session_path}`) when `@root`
      is unset (a session created outside rift).
- [ ] The single connect-time `RIFT_PROJECT_ROOT` / `--root` no longer pins the
      daemon's watched root for its lifetime; it becomes the default root for a
      newly created session's `@root`, not a global lifetime constant.
- [ ] Two app instances attaching **different** sessions to the one shared daemon
      each see their own session's reactive layer; two instances attaching the
      **same** session share one reactive context (no duplicate language server).
- [ ] No regression to Phase 34: new panes/windows/sessions still spawn in their
      session's root.

## Scope

### In scope

- **Introduce the `@root` session-user-option coupling.** `@root` does **not**
  exist in the codebase today (this phase adds it). At the `new-session -A -s
  <session>` chokepoint (`crates/daemon/src/terminal.rs:275`, the same line Phase 34
  adds `-c` to), the daemon additionally stamps `set -t <session> @root <root>`
  using its configured root, so a rift-created session carries its project root as
  durable, session-scoped tmux metadata. **Resolving a session's root** (the read
  side): a `display-message -p` query on that session's freshly-attached control
  child returns `@root` and `#{session_path}` together (the layout-query correlated
  round-trip pattern already in `terminal.rs`); resolve to `@root` if non-empty,
  else `#{session_path}` (the session working directory Phase 34 sets via `-c`).
- **Re-root the reactive layer daemon-side on `Attach` — the seam.** On
  `ClientMessage::Attach { session }` (the existing message — connect, reconnect,
  and switch all send it; `spawn_session_switch_bridge`, `crates/app/src/main.rs`),
  today routing reaches only the per-connection terminal task; the reactive layer
  is broadcast to **all** connections over one shared bus fed by the single global
  `state` watch (`crates/daemon/src/lib.rs`). Phase 35 replaces that with
  per-context routing:
  1. The terminal task (which owns the control child) resolves the attached
     session's root via the `display-message -p` read above.
  2. It **acquires** the per-root context for that root (creating it — scan +
     watchers + LSP — on first use, or bumping the refcount of an existing one) and
     **releases** the connection's previous context (refcount--); this connection's
     reactive writer now subscribes to the acquired context's updates instead of
     the global bus.
  3. Only **after** the root is resolved and the context acquired does the daemon
     stream this connection's fresh `WorktreeSnapshot { root }` + `RepoState` +
     `Diagnostics` — a defined ordering, so the first snapshot always carries the
     new root. Delivery reuses the existing messages (the client already replaces
     its reactive view on `WorktreeSnapshot { root }`, `crates/app/src/worktree.rs`),
     so the app follows with little or no change.
- **Make the daemon's watched state root-aware** (the core refactor). Today
  `crates/daemon/src/lib.rs`'s single process-global `State` (one `worktree`, one
  `git`, one `diagnostics`, one `lsp_status`) plus its single `watch_worktree` /
  `watch_lsp` workers and the one shared broadcast bus are spawned once at
  `serve`/`serve_uds` start against the one `--root`. This becomes a **per-root,
  reference-counted context map**: `root → context { state, worktree worker, git,
  LSP, update bus }`, each connection subscribing its reactive stream to its
  acquired root's context. The single-root path stays a special case (one context),
  so the common flow is unchanged.
- **Detach open buffers on re-root (safety).** The buffer / diff / git-write /
  file-op request-response handlers confine paths to the connection's context root
  (`crates/daemon/src/lib.rs`). A re-root silently changes that root, so a buffer
  opened against the old root then saved would resolve against the new root — a
  cross-project wrong-root write. On re-root the daemon **drops the connection's
  live-buffer feed** (equivalent to `buffer_closed` for every open path), and the
  client closes/re-opens editor buffers on a project switch; the existing
  `mtime`-conflict check is the backstop against a stale relative-path save. The
  full "keep editor tabs across a project switch" UX is out of scope (deferred).
- **Reconnect / picker integration.** The reconnect re-Attach (the Phase-20/33
  current-session watch) and the Phase-33 post-connect pick both flow through
  `Attach`, so re-rooting them is automatic once `Attach` re-roots.

### Out of scope

- **A project picker** — choosing/saving an arbitrary root when creating a rift
  session (the IDE "projects" surface). Phase 35 delivers the coupling *mechanism*;
  absent a picker, rift-created sessions default their `@root` to `RIFT_PROJECT_ROOT`
  and externally-created sessions resolve to their `#{session_path}`. Choosing a
  distinct root per rift-created session is a follow-on phase.
- **The simultaneous multi-pane / multi-worktree explorer UI** (vision Scenario 2)
  — showing more than one project's reactive context at once in one window. Phase 35
  delivers "root follows the *active* session" plus the daemon-side per-root
  substrate Scenario 2 later builds on; it does not render multiple contexts at once.
- **Per-session-root *display*** in the session list/picker (showing each session's
  project path) — see the OPEN protocol-surface decision; proposed deferred so the
  phase stays protocol-free.
- Phase 34's start-directory mechanics (shipped separately; this phase depends on
  that chokepoint but does not re-open it).

## Constraints

- **`PROTOCOL_VERSION` strict equality** (`crates/protocol/src/lib.rs`, currently
  `9`): any message-set change bumps it and re-pins `PROTOCOL_FINGERPRINT`
  (fingerprint test enforced). The proposed design re-roots via the **existing**
  `Attach` and `WorktreeSnapshot { root }` messages, so it needs **no protocol
  change** — unless per-session-root display is included (the OPEN decision), which
  adds `SessionEntry.root` and forces a version bump.
- **One shared daemon per host** (reattachable single-instance, #62; the dogfooding
  stable+dev channels share it — `docs/spec-dogfooding-channels.md`). The re-root
  design must stay correct with multiple concurrent connections on that one daemon.
- **RAM-constrained host** (`docs/workflow.md`: ~1-2 GB free on the WSL dev host).
  A language server (rust-analyzer) is memory-heavy, so the design must not run a
  duplicate server for the common case of two instances on the **same** session/root
  — the decisive argument for per-root sharing over per-connection contexts.
- **No `.unwrap()` in library code**; the daemon is best-effort — a bad/again-missing
  root degrades (worktree-only / empty) and logs, never aborts (`docs/constitution.md`;
  the existing `worktree_worker` degradation path is reused per root).
- **Agent-agnostic**: `@root` is rift's own session metadata, not agent state; no
  pane content is parsed. The reactive re-root derives only from the session's root
  (filesystem signal), consistent with the two-signals rule.
- **`@root` is introduced by this phase** — it exists nowhere in the codebase today,
  and `SESSION_LIST_QUERY` (`crates/daemon/src/terminal.rs:74`) reads no per-session
  root. Both the stamp (`set -t <session> @root`) and the `display-message -p` read
  are new daemon-side additions.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| **Couple via the tmux `@root` session user option**, stamped by the daemon at `new-session`; resolve `@root` → else `#{session_path}` | Native, session-scoped, durable, and shared across the two dogfooding instances without an external project registry (`docs/prior-art.md`, "Session ↔ project root coupling"). `#{session_path}` (the session working dir Phase 34 sets via `-c`) is the natural fallback for externally-created sessions, so "session = project" holds even without an explicit stamp. | 2026-07-09 |
| **Re-root delivered over the existing `WorktreeSnapshot { root }` / `RepoState` / `Diagnostics` messages** — no new "re-root" message | These are already replace-semantics keyed by root/path (`docs/protocol.md`); a fresh snapshot with the new root *is* the re-root. The client already follows the daemon's streamed root, so the reactive-view change is minimal. | 2026-07-09 |
| **Daemon-side root resolution** — the daemon reads `@root`/`session_path` on `Attach` via a `display-message -p` round-trip on the control child; `Attach` is unchanged | Resolved at the gate. `@root` is introduced by this phase; daemon-side keeps root resolution next to the workers and the control child that already runs correlated queries, and needs **no `Attach` protocol change**. Refines the roadmap's pre-planning "resolved app-side" guess. | 2026-07-09 |
| **Per-root, reference-counted context map** (Zed `HeadlessProject`/`WorktreeStore` shape) | Resolved at the gate. (a) *single re-rootable State* thrashes when two connections attach different sessions; (b) *per-connection context* duplicates the language server for the same-session dogfooding case (2× rust-analyzer → OOM on the ~1-2 GB-free host); **(c)** shares one context per root (no duplicate LSP) yet separates distinct roots — the only option that breaks neither rift scenario (`docs/prior-art.md`). | 2026-07-09 |
| **Defer per-session-root display** — Phase 35 stays protocol-free | Resolved at the gate. Showing each session's project path in the list/picker needs `SessionEntry.root` (+ `#{@root}` in `SESSION_LIST_QUERY`, + a `PROTOCOL_VERSION` bump); it is a UX nicety separable from the re-root mechanism and lands with the picker phase, keeping the architecture refactor un-entangled from a protocol bump. | 2026-07-09 |
| **Detach a connection's open buffers on re-root**; defer cross-switch editor-tab persistence | Prevents a cross-project wrong-root write (a buffer opened in project A, saved after switching to B): the daemon drops the connection's live-buffer feed on re-root and the client closes editor buffers on a project switch, with the `mtime`-conflict check as the backstop. Keeping tabs across a project switch is a picker-era UX, out of scope here. | 2026-07-09 |
| Scope excludes a project picker and Scenario 2's multi-context UI | Roadmap-bounded: Phase 35 is the coupling mechanism; choosing/saving arbitrary per-session roots and rendering multiple contexts at once are follow-on work (`docs/constitution.md`: no premature abstraction). | 2026-07-09 |

## Prior art

From `docs/prior-art.md` → "Session ↔ project root coupling — prior-art index
(Phases 34–35)":

- **Session-scoped root storage (the coupling)** — tmux `@root` user option
  (`set -t <session> @root`, `display -p -t <session> '#{@root}'`); `#{session_path}`
  fallback. Verdict: **reuse** — native session-scoped coupling, no external registry.
- **One server holding N project-root contexts + per-context LSP/git** — Zed
  `HeadlessProject` → `WorktreeStore` (one server, multiple `Worktree` entities;
  per-worktree LSP/git). Verdict: **reference** — the target shape for the daemon's
  per-root context map; rejects a single global root re-scanned on switch.
- **Which session → which root** — Zed workspace persistence / recent projects.
  Verdict: **reference** — durable root lives in tmux `@root`; no bespoke external
  project-file format.

## Human prerequisites

- None. The default root for a newly created session's `@root` is the already-provided
  `RIFT_PROJECT_ROOT` / `RIFT_DEFAULT_PROJECT_ROOT`; no new secret or config.

## Foundation impact (architecture.md — ratified in this spec's PR)

Authored and ratified with this spec (per the roadmap's recorded impact; never
edited from the roadmap):

- Amend the **"Connection robustness contract (phase 20)"** section: the
  current-session watch (already the reconnect/pick re-Attach target) now also
  drives the daemon's **watched root** — a session switch re-roots the reactive
  context, not only the terminal attach.
- Record the shift from the implicit **single-global-root daemon** (one `State`,
  workers bound once at serve start to `--root`) to a **per-root, reference-counted
  context** the attached session selects — in the **"Target architecture (Phase 3+)"**
  daemon description (the daemon holds N per-root contexts, the Zed
  `HeadlessProject`/`WorktreeStore` shape), plus a **tech-debt-table** row retiring
  the single-global-root assumption. `--root` becomes the default root for a new
  session's `@root`, not a lifetime constant.

## Tracking

The decomposition into steps lives as GitHub issues, one per implementable step,
grouped under the milestone. This spec owns the design; the issues own progress.

- Milestone: created at the spec-acceptance gate
- Issues: created from this spec after merge (one per implementable step)

Each issue references this spec path in its body.

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace
      excluding `rift-app`).
- [ ] Unit (`crates/daemon`): resolving a session's root prefers `@root` and falls
      back to `#{session_path}`; the `@root` stamp command is well-formed
      (validated against real tmux, like the Phase-34 attach command).
- [ ] Unit (`crates/daemon`): the per-root context map acquires one context per
      distinct root and shares it across connections on the same root; a context is
      released and its workers torn down when the last connection leaves it.
- [ ] Behavioral (dev-channel QA gate): with two sessions rooted at different
      projects, switching between them re-roots the file tree, git status, and
      diagnostics to the active session's project within the reactive latency
      budget; switching back restores the first.
- [ ] Behavioral (dev-channel QA gate): two app instances on the **same** session
      share one language server (no duplicate rust-analyzer); on **different**
      sessions each sees its own reactive layer.
- [ ] Behavioral: reconnect (stream recovery) and the Phase-33 post-connect pick
      each re-root to the correct session's root, not the connect-time default.
- [ ] Behavioral: an editor buffer open in one project is detached on a switch to
      another — no cross-project save against the new root (the `mtime`-conflict
      backstop rejects a stale save).
- [ ] Behavioral: Phase-34 start-directory still holds (new windows/panes in the
      session root).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The per-root context refactor is large and central (touches `State`, workers, dispatch, connection routing) | Decompose into issues (protocol-free core: root resolution + `@root` stamp; then the context-map refactor; then the Attach re-root wiring; then reconnect/switch end-to-end). Keep each PR ~400 lines; the single-root path stays a special case (one context) so the common flow is unchanged. |
| Re-root latency / flicker on switch (tear down + rescan) | Per-root contexts are cached and reference-counted, so switching *back* to a still-referenced root is instant; a first switch to a new root pays one scan (the existing initial-scan cost). |
| Language-server churn on switch | The LSP lives in the per-root context; switching away drops the ref but the context (and its server) can linger briefly / until the last ref leaves, avoiding restart thrash on rapid back-and-forth (exact eviction policy is an implementation detail, not a new gate). |
| A resolved root is missing/invalid (bad `@root`, gone dir) | Reuse the existing per-root degradation (worktree-only / empty + log); never abort the daemon. |
| A buffer opened against the old root is saved after a switch → cross-project wrong-root write | On re-root the daemon drops the connection's live-buffer feed (buffers detach); the client closes editor buffers on a project switch; the existing `mtime`-conflict check rejects a stale relative-path save as the backstop. Full cross-switch tab persistence is deferred. |
| `@root` stamped by one instance must be seen by the other (shared daemon) | `@root` is server-side session state, visible to every control client on the tmux server — the same shared-daemon property the session list already relies on. |

## Decision log

- 2026-07-09: Spec drafted for roadmap Phase 35 from the "Session ↔ project root
  coupling" seed, grounded in a daemon architecture map. Key finding: the daemon's
  `State` + workers are a single process-global root bound at serve start
  (`crates/daemon/src/lib.rs`), shared by all connections — the assumption Phase 35
  breaks. The map also showed the re-root can ride the existing `Attach` +
  `WorktreeSnapshot { root }` messages, so the design is largely daemon-side with
  little/no app or protocol change. Three open decisions carried to the acceptance
  gate: root resolution (daemon-side vs app-side), context structure (per-root map
  vs alternatives), per-session-root display (defer vs include), each with a
  proposed answer; scope (no picker, no Scenario-2 UI) recorded as bounded.
- 2026-07-09: Spec review (VERDICT REQUEST_CHANGES → addressed). The reviewer
  verified the central premise (single-global-root daemon) and the "no protocol
  change" claim as TRUE, and the context-structure choice (c) as sound. Addressed
  three blocking gaps: (1) corrected the false "daemon already reads `@root`"
  premise — `@root` is introduced here — and specified the read mechanism (a
  `display-message -p` round-trip on the control child returning `@root` +
  `session_path`); (2) sketched the re-root seam — today `Attach` touches only the
  terminal task and the reactive layer broadcasts to all connections over one bus,
  so Phase 35 adds per-context routing with a defined resolve → acquire → snapshot
  ordering; (3) added the open-buffer detach-on-re-root safety rule to prevent a
  cross-project wrong-root write. Plus the non-blocking fixes (decision count,
  Foundation-impact placement, reconnect/pick + buffer verification lines; the
  `roadmap.md` row-35 "app-side" wording is updated to daemon-side on merge).
- 2026-07-09: Spec-acceptance gate. Resolved all three open decisions to the
  proposed answers: **(c) per-root reference-counted context map**, **daemon-side
  root resolution** (`display-message -p`, no `Attach` change), and **defer
  per-session-root display** (Phase 35 stays protocol-free). Human prerequisites:
  none. Status `DRAFT` → `READY`; milestone `Phase 350` created at acceptance.
- 2026-07-09: Issue #735 implementation. Two narrow decisions not spelled out by
  the spec's command sketch: (1) **`stamp_root_command` sends unconditionally on
  every attach** (like `reroot_command`), not only when `new-session -A` actually
  creates the session — tmux control mode gives no "did this create the session"
  signal, and Phase 34's `reroot_command` already established the unconditional +
  idempotent pattern for exactly this reason ("Phase 34 has exactly one root, so
  sending this unconditionally on every attach is idempotent"); the same
  reasoning carries to `@root` while the daemon still has one root. (2) **the
  stamp targets the session explicitly (`set -t <session> @root <root>`)**,
  unlike `reroot_command`'s no-target trick, per the spec's own literal command
  sketch — both `session` and `root` are quoted with `quote_tmux_arg` and checked
  for `\n`/`\r`. The resolve side (`ROOT_QUERY`, `resolve_session_root`) is wired
  into `Attach::spawn`'s round trip and resolves-and-logs on every attach; nothing
  downstream consumes the resolved value yet — that lands with #736/#737 (the
  per-root context map and the re-root seam).
