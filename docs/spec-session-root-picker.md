# Spec: New-session remote root picker

> Status: DRAFT
> Created: 2026-07-09
> Completed: —

Creating a tmux session picks its project root by browsing the remote filesystem
first: a new daemon-side directory-listing capability backs a folder picker that
resolves the root, the session name defaults to the folder basename, and the
session is created rooted there — `new-session -c <root>` (Phase 34) with `@root`
stamped to the picked root (Phase 35) instead of the baked default. Roadmap
Phase 36; completes "session = project" by letting the human choose the project
at session-creation time. Depends on Phase 35 (the `@root` stamp + per-root
context) and reuses Phase 33's post-connect picker and Phase 32's session strip.

> Visual contract: Paper file `rift` — the **"Session flows"** artboard (all
> launch → cockpit routes, incl. the phase-36 root picker as S3) and the
> **"rift — Session management"** artboard's **Frame C** (root-picker anatomy:
> remote breadcrumb, folder rows with git flag, name field defaulting to the
> folder, Create) plus **Frame B**'s superseded zero-sessions note (with no
> sessions, connecting opens the root picker directly).

## Outcome

- [ ] The `protocol` message set gains a **directory-browse channel** — a
      `QueryDirEntries { path }` request and a `DirEntriesReply` reply carrying the
      resolved absolute path, its parent, and the child directory entries — as a
      deliberate, reviewed API extension: `PROTOCOL_VERSION` bumps `9 → 10`, the
      fingerprint is re-pinned, and `docs/protocol.md` documents the channel. The
      message contract is fully specified below; the implementer only wires it.
- [ ] The **daemon serves directory listings** for an absolute path on the host
      via `std::fs::read_dir` on `spawn_blocking`, resolving an empty/relative
      request to `$HOME` and returning child **directories** (each flagged
      `is_git_repo`) — **daemon-side, never client SFTP**, and — unlike the
      Phase-30 write path — **not confined to a worktree root** (its purpose is to
      choose a new root; the daemon already reads the whole host filesystem as the
      SSH user, so this adds no new privilege). A missing / permission-denied path
      returns a typed error reply; the daemon never aborts.
- [ ] Creating a session opens the **remote root picker**: the user browses the
      host (breadcrumb + directory rows, async per level), picks a folder, and the
      session name field is **pre-filled with the folder basename** (editable). On
      confirm the session is created rooted at the picked path — `new-session -A -s
      <name> -c <picked>` (Phase 34) with `set -t <name> @root <picked>` (Phase 35)
      — and the cockpit attaches to it, its reactive layer rooted there.
- [ ] The root picker is the entry for **every "new session" path**: the
      post-connect picker's "+ New session…" (Phase 33) and the in-cockpit session
      strip's "+" (Phase 32) both open it, and — superseding the zero-sessions
      empty-state screen — connecting to a host with **no sessions** opens the root
      picker directly (the session list shows only when sessions exist).
      `RIFT_SESSION` stays the picker-skipping fast-path.
- [ ] The browse never blocks the UI: each directory level is an async round-trip
      to the daemon with a loading state; a denied / missing directory surfaces a
      legible error in the picker without tearing it down.
- [ ] The flow stays agent-agnostic (a filesystem read, no pane-content parsing);
      `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` pass;
      CI `app-check` compiles the app.

## Scope

### In scope

The **directory-browse protocol channel**, its **daemon-side handler**, and the
**client root-picker surface** wired into the new-session entry points. The
binding visual reference is the Paper `rift` file — "Session flows" (routes) and
"Session management" Frame C (root-picker anatomy) + Frame B (superseded
zero-sessions note).

**Protocol message contract (fully specified — the implementer only wires it).**

Add to `ClientMessage`:

- `QueryDirEntries { path: String }` — request the child directories of `path`, an
  **absolute** host path. An empty string resolves to the daemon user's `$HOME`
  (the picker's start level). Leading `~/` expands to `$HOME`. One
  `DirEntriesReply` reply.

Add to `DaemonMessage`:

- `DirEntriesReply { path: String, parent: Option<String>, entries: Vec<DirEntry>, error: Option<DirBrowseError> }`
  — the reply to every `QueryDirEntries`. `path` is the resolved absolute
  directory that was listed (so the client can correlate and render the
  breadcrumb even when it sent `""`); `parent` is its parent directory or `None`
  at the filesystem root; `entries` are its child directories, name-sorted; on
  failure `entries` is empty and `error` is set (`error` omitted on success via
  `skip_serializing_if = "Option::is_none"`).

Add the two supporting types:

- `DirEntry { name: String, is_git_repo: bool }` — one child **directory**
  (files are omitted: a project root is a directory). `is_git_repo` is a cheap
  `<name>/.git` existence check (no git read), letting the picker flag repos
  (**OPEN**: whether to include it / the current branch — see Prior decisions).
- `DirBrowseError` — a `#[serde(rename_all = "snake_case")]` typed reason
  (mirroring `FileOpError`): `NotFound`, `PermissionDenied`, `NotADirectory`,
  `Io`.

Bump `PROTOCOL_VERSION` `9 → 10`, re-pin `PROTOCOL_FINGERPRINT` (the failing
fingerprint test prints the new value), add serde round-trip tests for every new
variant (valid + unknown-tag rejection, as the existing tests do), and extend
`docs/protocol.md` with a "Directory-browse channel" section and a version-10
history line.

**Daemon handler (`std::fs`, daemon-side, unconfined read).** A new
`crates/daemon/src/browse.rs` module in the shape of `file_ops::reply` — an
`async fn reply(msg) -> DaemonMessage` that resolves `path` (`""`/`~` → `$HOME`
via the daemon's environment), runs `std::fs::read_dir` on
`tokio::task::spawn_blocking` (disk-bound), keeps entries where
`file_type().is_dir()` (following the symlink for the type check), stamps
`is_git_repo` from a `<child>/.git` existence probe, sorts by name, and maps an
`io::ErrorKind` to `DirBrowseError` (`NotFound` / `PermissionDenied` →the match;
a non-directory target → `NotADirectory`; else `Io`). Wire the new request into
`serve_connection`'s per-connection reply dispatch beside the file-op arm
(`browse::reply(msg).await` → `encode_frame` → write back to the requesting
socket). **No `State` / context borrow** — the browse is rootless, so unlike
`file_ops::reply` it takes no `state` and does no `buffer::resolve` confinement
(recorded as a Prior decision: the confinement carve-out is intentional).

**Client root picker (`app` / `terminal`).** A remote folder-browser surface —
the Frame-C anatomy — presented as a **modal/panel over the existing Phase-33
pre-cockpit picker container and, in-cockpit, over the workspace** (not a new
`Shell` state): a header, a breadcrumb of the resolved path, a name-sorted list
of directory rows (folder glyph, name, a git flag when `is_git_repo`), and a
footer with the session-name input (seeded with the selected folder's basename,
editable) and a Create button. Selecting a row issues `QueryDirEntries { path:
<row> }` and descends; the breadcrumb segments and a parent/".." affordance issue
`QueryDirEntries` for an ancestor; each pending level shows a loading state and an
`error` reply renders inline without closing the picker. Reuse the
Connection-screen / Phase-32 theme tokens and `gpui-component` `InputState` for
the name field; never fork them.

**New-session entry points + create-with-root (`app`).** Route every "new
session" affordance through the root picker:

- The Phase-33 post-connect picker's **"+ New session…"** footer opens the root
  picker instead of a bare name prompt.
- The Phase-32 in-cockpit **session-strip "+"** opens the root picker.
- **Zero-sessions**: when the post-connect `QuerySessionList` returns an empty
  list, open the root picker **directly** (superseding the empty-state screen);
  the session list renders only when the host has ≥1 session.

On Create, the app creates the session at the picked root by threading it into the
existing spawn: `new-session -A -s <name> -c <picked>` (the Phase-34
`spawn_args` `-c`) and stamps `set -t <name> @root <picked>` (the Phase-35 stamp),
so the picked root — not `RIFT_PROJECT_ROOT` — becomes the new session's `@root`;
then it `Attach`es, and the Phase-35 daemon re-root makes the reactive layer
follow. The name defaults to the folder basename and is editable before Create.

### Out of scope — each its own phase or deliberately deferred

- **Re-rooting an existing session** to a different folder after creation (a
  "change project root" affordance on a live session). Phase 36 picks the root at
  **creation**; changing it later is follow-on. `@root` is written once, at create.
- **The simultaneous multi-worktree / multi-context UI** (vision Scenario 2) —
  still deferred (Phase 35's boundary); Phase 36 adds only the create-time root
  choice, not rendering multiple projects at once.
- **A file opener / picker** — the browse lists **directories only** and selects a
  root; opening individual files through it is not this phase.
- **Fuzzy project quick-open** over the whole host (a file-finder-style jump to a
  known project by typing). v1 is hierarchical browse (+ optional recents); a
  fuzzy index is a possible follow-on (cf. Phase 31's file quick-open).
- **A durable project registry / workspace files** (Zed-style `.zed` project
  files). The durable per-session root lives in tmux `@root` (Phase 35); Phase 36
  adds at most a lightweight recents mapping (**OPEN** — see Prior decisions), not
  a bespoke project-file format.

## Constraints

- **`protocol` is a deliberate API surface** (constitution): the request, the
  reply, and the two supporting types are an intentional, reviewed extension;
  `PROTOCOL_VERSION` bumps `9 → 10` and the fingerprint test re-pins, so the
  message-set change cannot merge without the bump (CI-enforced). The value is
  the next free number at merge time (Phase 35, milestone #53, is protocol-free,
  so `9` stands until this lands).
- **Daemon owns the filesystem — the browse runs daemon-side, never client SFTP**
  (the Phase-30 foundation contract; the daemon watches, reads, and writes the
  remote tree). The client sends intent (`QueryDirEntries`); the daemon executes
  with `std::fs::read_dir`. No SFTP layer, no second transport. Mirrors Zed's
  remote server (prior-art index).
- **The browse read is NOT root-confined** — deliberately unlike the Phase-30
  write path. Its purpose is to pick a *new* root, so it accepts an absolute path
  and reads any directory the daemon user can, with no `buffer::resolve`
  confinement and no context/`State` borrow. This is not a privilege escalation:
  the daemon already runs as the SSH user and reads the whole host filesystem (it
  watches arbitrary roots). Recorded as a Prior decision.
- **Async, non-blocking browse** (constitution: async for I/O): each level is a
  daemon round-trip; the picker shows a per-level loading state and never blocks
  the render thread. A slow / large directory must not freeze the UI.
- **Depends on Phase 35** (`@root` stamp + per-root context, milestone #53) and
  **Phase 34** (`-c` on `new-session`), and **reuses** Phase 33's post-connect
  picker container + entry-point model and Phase 32's session strip. Phase 36
  writes `@root` = the picked root using Phase 35's stamp; it does not re-open the
  stamp or the re-root.
- **Best-effort daemon** (constitution; no `.unwrap()` in libs): a missing /
  denied / non-directory browse target returns a typed `DirBrowseError` reply and
  logs; it never aborts the daemon. The daemon is a binary — `anyhow` + a
  hand-written `Display` for the browse error, matching `buffer.rs` / `file_ops`.
- **Agent-agnostic** (constitution): the browse is a filesystem read; no pane
  content is parsed, no agent detected. The picked root is a filesystem signal,
  consistent with the two-signals rule.
- **Theme tokens only** (Catppuccin Mocha via `gpui-component`): the picker's
  surfaces (breadcrumb, rows, git flag, name input, Create button, error) use
  existing theme roles, never hardcoded hex; layout stays plain constants.
- **Reuse `gpui-component` / `gpui` primitives, never fork them**: `InputState`
  for the name field, the existing list/scroll primitives for the rows.
- No new dependency (`std::fs` / `tokio::fs`; git-repo flag is a `.git` existence
  check, not a `gix` read); crate boundaries via `lib.rs`; English; no emojis.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| **The browse is a new daemon-side `protocol` dir-listing channel (`QueryDirEntries` → `DirEntriesReply`), executed with `std::fs::read_dir`, never client SFTP** | The daemon owns the remote filesystem (it watches, reads, saves, mutates the git index). Directory browsing is the same read capability; a client SFTP layer would be a second transport for a job the daemon is positioned to do — mirroring Zed's remote server (`docs/prior-art.md`, "Session ↔ project root coupling — prior-art index", Phase 36 rows) and the Phase-30 file-op precedent. | 2026-07-09 |
| **The browse handler is NOT root-confined** — it accepts an absolute path, does no `buffer::resolve`, holds no context/`State`, and reads anywhere the daemon user can | Its purpose is to choose a *new* project root, so confining it to an existing worktree root would defeat it. This is not new privilege: the daemon already runs as the SSH user and reads the whole host filesystem. The Phase-30 write confinement is a write-path concept and does not apply to this read. | 2026-07-09 |
| **The picker supersedes the zero-sessions empty-state screen** — with no sessions on the host, connecting opens the root picker directly; the session list shows only when sessions exist; `RIFT_SESSION` stays the picker-skipping fast-path | "No sessions → you must create one → creating means picking a root", so a distinct empty-list screen is redundant (idea sparring, this session; the "Session flows" artboard Path A and Frame B's superseded note encode it). The Phase-33 zero-sessions edge is re-pointed at the root picker. | 2026-07-09 |
| **The session name defaults to the folder basename, editable before Create** | "session = project" — the name falls out of the chosen folder, removing the "name it first" friction; still editable for when the basename is not the wanted session name. Matches the `sesh` / `tmux-sessionizer` folder-basename convention (`docs/prior-art.md`, Phase 36 rows). | 2026-07-09 |
| **The picked root is applied via the existing Phase-34 `-c` + Phase-35 `@root` stamp — the picker only supplies the value** | Phase 34 already threads `-c <root>` into `new-session` and Phase 35 stamps `@root` (defaulting it to `RIFT_PROJECT_ROOT`). Phase 36 replaces that default with the picked path; the create/attach/re-root machinery is unchanged, keeping Phase 36 additive over 34/35 rather than re-opening them. | 2026-07-09 |
| **The root picker is a modal/panel over the Phase-33 picker container and the in-cockpit workspace — not a new `Shell` state** | Phase 33 already introduced the pre-cockpit picker `Shell` state; the root picker is a mode within it (post-connect) and a modal over the workspace (in-cockpit strip "+"). Reusing those containers avoids a fourth top-level state and a second session-creation UI. | 2026-07-09 |
| **Browse lists directories only, name-sorted, async per level** | A project root is a directory, so files are noise in the list; per-level round-trips keep each response small and the UI responsive (constitution: async for I/O; never block the render thread). | 2026-07-09 |
| **`is_git_repo` flag / current branch — OPEN, resolved at the spec-acceptance gate** | The "Session flows" mockup shows a git branch badge, but a branch read is a per-directory `gix`/git cost the cheap `.git`-existence flag avoids. Whether to ship the `is_git_repo` flag only, add the branch, or omit git metadata is a scope/UX call neither precedent nor constraint settles. `OPEN — resolved at the spec-acceptance gate`. | 2026-07-09 |
| **Recents (start level + recently-picked roots) — OPEN, resolved at the spec-acceptance gate** | The picker could start at `$HOME` only, or seed from a phase-9-store recents mapping of recently-picked roots (Phase 35 keeps a lightweight session→root recents). Whether recents ship in Phase 36 or defer is a scope call. `OPEN — resolved at the spec-acceptance gate`. | 2026-07-09 |
| **Per-session-root display in the session list/picker (`SessionEntry.root`) — OPEN, resolved at the spec-acceptance gate** | Phase 35 deferred showing each session's project path, noting it "lands with the picker phase" — it needs `SESSION_LIST_QUERY` to read `#{@root}` and `SessionEntry` to gain `root` (folded into this phase's protocol bump). Whether to include it now or keep Phase 36 to browse+create is a scope call. `OPEN — resolved at the spec-acceptance gate`. | 2026-07-09 |

## Prior art

From `docs/prior-art.md` → "Session ↔ project root coupling — prior-art index
(Phases 34–36)", Phase 36 rows:

- **Remote directory browsing (pick a project root on the host)** — Zed remote
  model (the daemon owns the fs and enumerates directories server-side; clients
  are thin proxies) + rift's own Phase-30 daemon file-op precedent (`std::fs` on
  the remote host via new `protocol` messages, not client SFTP); `yazi`'s
  russh-SFTP provider is reference-only (rift's daemon is already on the host).
  Verdict: **reference / reuse own pattern** — a new `protocol` dir-listing
  request/reply executed daemon-side like the Phase-30 file ops.
- **Folder picker → session (name = basename, git-aware, recents)** —
  `joshmedeski/sesh` + `*/tmux-sessionizer` (folder / git repo → session in its
  dir, basename as the name) + Zed "Open Folder" / `recent_project_workspaces`.
  Verdict: **reference (pattern)** — the folder-basename default and a phase-9
  recents list.

Reused rift-local contracts: `crates/daemon/src/file_ops.rs` (the
`reply(msg) -> DaemonMessage`, `spawn_blocking`, `io::ErrorKind` → typed error
shape — Phase 30); `spec-per-session-project-root.md` (the `@root` stamp + per-root
re-root — Phase 35); `spec-post-connect-picker.md` (the pre-cockpit picker
container + entry-point model — Phase 33); the Phase-34 `spawn_args` `-c`.

## Human prerequisites

None. The daemon already runs on the remote host with filesystem read access (it
watches and reads the tree); directory browsing is that same capability. No new
secret, account, or provisioning. The `PROTOCOL_VERSION` bump is client+daemon
lockstep, already the project's deploy discipline (the daemon binary is
redeployed per session). No new dependency.

## Foundation impact (ratified in this spec's PR)

Per the roadmap's recorded Phase-36 impact (never edited from the roadmap):

- `protocol` gains the directory-browse channel (`QueryDirEntries` +
  `DirEntriesReply` + `DirEntry` + `DirBrowseError`), `PROTOCOL_VERSION` `9 → 10`,
  documented in `docs/protocol.md` — the daemon's first filesystem **browse** read.
- `docs/architecture.md`: a one-line note that the daemon exposes an
  **unconfined directory-browse read** (distinct from the root-confined Phase-30
  write path), so the browse capability's trust boundary is on record. The
  per-session context substrate itself is already Phase 35's foundation change;
  Phase 36 adds no further architecture change.

## Tracking

The decomposition into steps lives as GitHub issues, one per implementable step,
grouped under the milestone. This spec owns the design; the issues own progress.

- Milestone: created at the spec-acceptance gate. `Depends on milestone: #53`
  (Phase 350 / Phase 35 — the `@root` stamp + per-root re-root this phase writes
  and relies on).
- Issues: created from this spec after merge (one per implementable step), ordered
  so the protocol + daemon browse capability lands before the client picker, and
  the create-with-root wiring depends on the Phase-35 `@root` stamp.

Each issue references this spec path. A PR may only merge if it closes an issue
that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes; `cargo test --workspace`
      passes; CI `app-check` compiles the app.
- [ ] `PROTOCOL_VERSION` is `10`; the fingerprint test passes with the re-pinned
      value; `docs/protocol.md` documents the directory-browse channel and a
      version-10 history line. Serde round-trip tests cover every new variant
      (valid + unknown-tag rejection); `DirEntriesReply` omits `error` on success.
- [ ] Daemon browse tests (mirroring `file_ops` tests, over a `tempfile` tree):
      `QueryDirEntries` on a directory returns its child directories name-sorted
      (files excluded), with `is_git_repo` true for a child containing `.git`;
      an empty/`~` request resolves to `$HOME`; a missing path replies `NotFound`,
      a non-directory replies `NotADirectory`, an unreadable dir replies
      `PermissionDenied` — and the daemon does not abort on any of them.
- [ ] Root picker (client): browsing descends level-by-level over
      `QueryDirEntries` with a loading state; the breadcrumb reflects the resolved
      path; selecting a folder pre-fills the name field with its basename; an error
      reply renders inline without closing the picker. Asserted headlessly over the
      picker state where tests reach it; the visual treatment is the QA gate.
- [ ] Create-with-root: confirming creates `new-session -A -s <name> -c <picked>`
      and stamps `@root <picked>` (the picked root, not `RIFT_PROJECT_ROOT`), then
      attaches; a headless assertion confirms the create command carries the picked
      `-c` and the `@root` stamp value.
- [ ] Entry points: the post-connect picker's "+ New session…" and the in-cockpit
      strip's "+" both open the root picker; a host with **no sessions** opens the
      root picker directly (no empty-state list screen); `RIFT_SESSION` still
      attaches directly with no picker.
- [ ] `grep` confirms no agent detection, no client-side SFTP, no new dependency;
      the daemon browse logic is confined to `crates/daemon/src/browse.rs` and is
      rootless (no `buffer::resolve`, no `State` borrow).
- [ ] Milestone QA (dev channel): with no sessions, connecting opens the root
      picker; browse to a project folder (a git repo shows its flag), accept the
      basename-defaulted name, Create — a new session is created rooted there and
      the cockpit's file tree / git / LSP reflect that project (Phase-35 re-root).
      Repeat from the in-cockpit strip "+". The surfaces read like Frame C.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| An unconfined browse read is a wider surface than the confined write path | It is a **read** the daemon user already has (it reads the whole host fs to watch roots); no write, no new privilege. Recorded as a Prior decision and noted in `architecture.md`; the write path stays confined. |
| A huge directory (`$HOME` with thousands of entries) stalls the listing / render | The read runs on `spawn_blocking` (off the render thread); the reply carries directories only (files excluded), name-sorted; the list uses the existing virtualized/scroll primitives. Per-level round-trips keep each response bounded to one directory. |
| Browse latency makes the picker feel unresponsive | Every level is async with a loading state; the picker never blocks. Selecting a still-cached ancestor via the breadcrumb re-queries but renders immediately from the last reply if unchanged (client-side, not a protocol concern). |
| A denied / vanished directory mid-browse | The daemon returns a typed `DirBrowseError`; the picker renders it inline and keeps the last good level, never tearing down. |
| Phase 35's `@root` stamp is not yet merged when the create-with-root issue runs | The milestone carries `Depends on milestone: #53` and the create issue depends on the Phase-35 stamp issue; the loop parks it until Phase 35 lands. The protocol + daemon browse + picker-UI issues do not need Phase 35 and can proceed. |
| The message-set change merges without the version bump | The fingerprint test fails on any message-set change until `PROTOCOL_VERSION` bumps and re-pins — CI-enforced, the same gate every prior protocol change passed. |
| Scope creep into a project registry / multi-context UI | Explicitly out of scope: durable root stays in `@root`, recents (if in scope) is a lightweight phase-9 mapping, Scenario 2 stays deferred. |

## Decision log

- 2026-07-09: Spec created from `/loopkit:plan` (roadmap Phase 36 — new-session
  remote root picker), the "project picker" Phase 35 explicitly deferred
  ("choosing / saving an arbitrary root when creating a rift session … a follow-on
  phase"). Grounded in the Phase-30 file-op protocol/daemon precedent (the
  `reply(msg) -> DaemonMessage` + `spawn_blocking` + typed-error shape, applied to
  a **read**), the Phase-35 `@root` stamp + per-root re-root, and the Phase-33
  post-connect picker + entry-point model. Key design points settled: the browse
  is a new daemon-side dir-listing channel (`PROTOCOL_VERSION 9 → 10`), **not**
  root-confined (its job is to pick a root; the daemon already reads the host fs);
  the root is applied through the existing Phase-34 `-c` + Phase-35 `@root` stamp
  (picker only supplies the value); the picker supersedes the zero-sessions
  empty-state screen; name defaults to the folder basename. Three open decisions
  carried to the acceptance gate, each with a proposed answer: git flag/branch
  richness, recents in scope, and per-session-root display (`SessionEntry.root`,
  which Phase 35 pointed here). Visual contract: the Paper "Session flows" +
  "Session management" Frame C artboards authored in this session's design sparring.
