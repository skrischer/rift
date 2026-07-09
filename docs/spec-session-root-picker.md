# Spec: New-session remote root picker

> Status: READY
> Created: 2026-07-09
> Completed: —

Creating a tmux session picks its project root by browsing the remote filesystem
first: a new daemon-side directory-listing capability backs a folder picker that
resolves the root, the session name defaults to the folder basename, and the
session is created rooted there — `new-session -c <root>` (Phase 34) with `@root`
stamped to the picked root (Phase 35) instead of the baked default, the picked
root carried to the daemon on a new `root` field of `Attach`. Roadmap Phase 36;
completes "session = project" by letting the human choose the project at
session-creation time. Depends on Phase 35 (the `@root` stamp + per-root context)
and reuses Phase 33's post-connect picker and Phase 32's session strip.

> Visual contract: Paper file `rift` — the **"Session flows"** artboard (all
> launch → cockpit routes, incl. the phase-36 root picker as S3) and the
> **"rift — Session management"** artboard's **Frame C** (root-picker anatomy:
> remote breadcrumb, folder rows with git flag, name field defaulting to the
> folder, Create) plus **Frame B**'s superseded zero-sessions note (with no
> sessions, connecting opens the root picker directly).

## Outcome

- [ ] The `protocol` message set gains a **directory-browse channel** — a
      `QueryDirEntries { path }` request and a `DirEntriesReply` reply carrying the
      resolved absolute path, its parent, and the child directory entries — a
      `root: Option<String>` field on `Attach` (the create-with-root transport),
      and a `root` field on `SessionEntry` (each session's project path) — as a
      deliberate, reviewed API extension: `PROTOCOL_VERSION` bumps `9 → 10`, the
      fingerprint is re-pinned, and `docs/protocol.md` documents all three. The
      message contract is fully specified below; the implementer only wires it.
- [ ] The **daemon serves directory listings** for an absolute path on the host
      via `std::fs::read_dir` on `spawn_blocking`, resolving an empty/`~` request to
      `$HOME` and returning child **directories** (symlinked dirs followed and
      included, each flagged `is_git_repo` with its current git branch) —
      **daemon-side, never client SFTP**,
      and — unlike the Phase-30 write path — **not confined to a worktree root**
      (its purpose is to choose a new root; the capability is a directory
      *enumeration* over reads the SSH user already has, so it adds no new
      privilege). A missing / permission-denied / non-directory path returns a
      typed error reply; the daemon never aborts.
- [ ] Creating a session opens the **remote root picker**: the user browses the
      host (breadcrumb + directory rows, async per level), picks a folder, and the
      session name field is **pre-filled with the folder basename** (editable). On
      confirm the picked root travels to the daemon on `Attach { session, root:
      Some(picked) }`; the daemon creates the session rooted there — `new-session
      -A -s <name> -c <picked>` (Phase 34) with `set -t <name> @root <picked>`
      (Phase 35) — and attaches, its reactive layer rooted there. The picker
      guarantees a fresh session name so a create never silently attaches an
      existing session.
- [ ] The root picker is the entry for **every "new session" path**: the
      post-connect picker's "+ New session…" (Phase 33) and the in-cockpit session
      strip's "+" (Phase 32) both open it (both send `Attach { root: Some(picked) }`
      — no separate in-cockpit path), and — superseding the zero-sessions
      empty-state screen — connecting to a host with **no sessions** opens the root
      picker directly (the session list shows only when sessions exist).
      `RIFT_SESSION` stays the picker-skipping fast-path.
- [ ] The picker starts from a **recents list of recently-picked roots** (the
      phase-9 store), falling back to `$HOME`; a successful create records the
      picked root. The session list / picker show each session's project path from
      `SessionEntry.root` (read from `#{@root}`).
- [ ] The browse never blocks the UI: each directory level is an async round-trip
      to the daemon with a loading state; a denied / missing directory surfaces a
      legible error in the picker without tearing it down.
- [ ] The flow stays agent-agnostic (a filesystem read, no pane-content parsing);
      `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` pass;
      CI `app-check` compiles the app.

## Scope

### In scope

The **directory-browse protocol channel**, the **`Attach.root` create-with-root
transport**, the **daemon-side browse handler**, and the **client root-picker
surface** wired into the new-session entry points. The binding visual reference is
the Paper `rift` file — "Session flows" (routes) and "Session management" Frame C
(root-picker anatomy) + Frame B (superseded zero-sessions note).

**Protocol message contract (fully specified — the implementer only wires it).**

Add to `ClientMessage`:

- `QueryDirEntries { path: String }` — request the child directories of `path`, an
  **absolute** host path. An empty string resolves to the daemon user's `$HOME`
  (the picker's start level). Leading `~/` expands to `$HOME`. One
  `DirEntriesReply` reply.
- **Extend `ClientMessage::Attach { session }`** with a `root: Option<String>`
  field — the create-with-root transport. Today `Attach` carries only a session
  name (`crates/protocol/src/lib.rs`), and the daemon spawns `new-session` with its
  **own configured** root (`terminal.rs`), so a session cannot be created at a
  *picked* root without this field. `None` on every existing caller (reconnect,
  switch, pick-existing) → unchanged behavior (Phase-35 resolves `@root` /
  `session_path`); `Some(<picked>)` when creating at a picked root, where the
  daemon threads it into the `new-session -c` and the Phase-35 `@root` stamp (the
  create sequence is in the client section below). The field carries
  `#[serde(default, skip_serializing_if = "Option::is_none")]`, so a `root`-less
  payload still deserializes and a `None` attach serializes without a `root` key —
  matching the pinned `test_attach_roundtrip_carries_session_name` exact-JSON
  assertion and the `Option`-field precedent (`GitOpResult::error`,
  `Diagnostic::source`). The field rides this bump.

Add to `DaemonMessage`:

- `DirEntriesReply { path: String, parent: Option<String>, entries: Vec<DirEntry>, error: Option<DirBrowseError> }`
  — the reply to every `QueryDirEntries`. `path` is the resolved absolute
  directory that was listed (so the client can correlate and render the
  breadcrumb even when it sent `""`); `parent` is its parent directory or `None`
  at the filesystem root; `entries` are its child directories, name-sorted; on
  failure `entries` is empty and `error` is set (`error` omitted on success via
  `skip_serializing_if = "Option::is_none"`). This is a **query-reply** shape
  (success = `error.is_none()`), deliberately not the `ok: bool` op-result shape of
  `FileOpResult` — a query returns data or an error, not an ack.

Add the two supporting types:

- `DirEntry { name: String, is_git_repo: bool, git_branch: Option<String> }` — one
  child **directory** (files are omitted: a project root is a directory; dotfile
  directories are **included**). `is_git_repo` is a cheap `<name>/.git` existence
  check; `git_branch` is the repo's current branch parsed from `<name>/.git/HEAD`
  (`ref: refs/heads/<branch>` → `<branch>`; a detached HEAD or non-repo → `None`) —
  a plain file read, **not** a `gix` call (resolved "flag + branch" at the
  acceptance gate). `git_branch` carries
  `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- `DirBrowseError` — a `#[serde(rename_all = "snake_case")]` typed reason
  (mirroring `FileOpError`): `NotFound`, `PermissionDenied`, `NotADirectory`,
  `Io`.

Extend `SessionEntry` (`crates/protocol/src/lib.rs`) with `root: Option<String>`
(`#[serde(default, skip_serializing_if = "Option::is_none")]`) and the daemon's
`SESSION_LIST_QUERY` (`crates/daemon/src/terminal.rs`) to read `#{@root}` alongside
the existing session fields, so `parse_session_line` fills each entry's root and the
list / picker render each session's project path. Reading `#{@root}` depends on the
Phase-35 stamp, so this rides the milestone-#53 dependency.

Bump `PROTOCOL_VERSION` `9 → 10`, re-pin `PROTOCOL_FINGERPRINT` (the failing
fingerprint test prints the new value) — the bump covers the directory-browse
channel, the new `Attach.root` field, and the `SessionEntry.root` field — add serde
round-trip tests for every new/changed variant (valid + unknown-tag rejection, as
the existing tests do), and extend `docs/protocol.md` with a "Directory-browse
channel" section, the `Attach.root` + `SessionEntry.root` additions, and a
version-10 history line.

**Daemon browse handler (`std::fs`, daemon-side, unconfined read).** A new
`crates/daemon/src/browse.rs` module in the shape of `file_ops::reply` — an
`async fn reply(msg) -> DaemonMessage` (takes **no `state`** — the browse is
rootless, so unlike `file_ops::reply` it does no `buffer::resolve` confinement and
holds no `State`/context borrow; the confinement carve-out is intentional, see
Prior decisions). It:
1. **Resolves the path**: `""` / `~` / `~/…` → `$HOME` via `std::env::var("HOME")`;
   if `HOME` is unset (a stripped daemon env), degrade to `/` (best-effort, never
   abort). Otherwise the path is used as the absolute target.
2. **Checks the target up front** with `fs::metadata(path)` (mirroring `file_ops`'
   deterministic up-front checks rather than trusting `io::ErrorKind`): a missing
   target → `NotFound`; an existing non-directory target → `NotADirectory`. This
   avoids depending on `ErrorKind::NotADirectory` (stable only since Rust 1.83).
3. **Lists** via `std::fs::read_dir` on `tokio::task::spawn_blocking` (disk-bound),
   keeping entries whose **`fs::metadata` (symlink-following) `is_dir()`** holds —
   so a symlinked project directory is **included** (`DirEntry::file_type()` does
   not follow symlinks, so it is not used for the dir test); stamps `is_git_repo`
   from a `<child>/.git` existence probe and `git_branch` from a `<child>/.git/HEAD`
   read (`ref: refs/heads/<b>` → `<b>`; a detached HEAD or non-repo → `None`), a
   plain file read (no `gix`); sorts by name.
4. Maps any remaining `io::ErrorKind` to `DirBrowseError` (`PermissionDenied` → the
   match; else `Io`).

Wire the new request into `serve_connection`'s per-connection reply dispatch beside
the file-op arm (`browse::reply(msg).await` → `encode_frame` → write back to the
requesting socket).

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
the name field; never fork them. The picker's start level is seeded from a
**recents list of recently-picked roots** (the phase-9 window-state store, extended
with a roots list; a successful create records the picked root), falling back to
`$HOME` when empty. Each session row / list entry shows its project path from
`SessionEntry.root`.

**New-session entry points + create-with-root (`app`).** Route every "new
session" affordance through the root picker:

- The Phase-33 post-connect picker's **"+ New session…"** footer opens the root
  picker instead of a bare name prompt.
- The Phase-32 in-cockpit **session-strip "+"** opens the root picker.
- **Zero-sessions**: when the post-connect `QuerySessionList` returns an empty
  list, open the root picker **directly** (superseding the empty-state screen);
  the session list renders only when the host has ≥1 session.

On Create, the app sends `Attach { session: <name>, root: Some(<picked>) }` — the
**single transport for both entry points** (no separate in-cockpit `TmuxCommand`
create path). The daemon's attach handler (`open_attach` / `Attach::spawn`,
`crates/daemon/src/terminal.rs`) threads `root` when `Some` into `spawn_args`'s
`new-session -c <picked>` and the Phase-35 `@root` stamp (`set -t <name> @root
<picked>`), so the **picked** root — not `RIFT_PROJECT_ROOT` — becomes the new
session's `@root`; `None` preserves today's behavior (reconnect / switch /
pick-existing, Phase-35 resolving `@root`/`session_path`). Because `new-session -A`
**attaches** an existing session of the same name (ignoring `-c`), the picker
**guarantees a fresh name**: it checks the live `QuerySessionList` and
disambiguates a colliding basename (e.g. `rift` → `rift-2`) before Create — the
**client-side fresh-name check is the guarantee** that a create does not land in an
unrelated existing session. The daemon's `Some`-only `@root` stamp only spares the
`None` path (reconnect / switch / pick-existing never re-stamp); it does not itself
cover a `Some` + same-name race, which the disambiguation prevents (the residual
list-check-to-Attach TOCTOU on the shared daemon is negligible and recoverable).
The name defaults to the folder basename and is editable before Create.

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
  adds a lightweight recents mapping of recently-picked roots (the phase-9 store),
  not a bespoke project-file format.
- **A hide-dotfiles toggle** — v1 lists all directories including dotfiles; a
  toggle is a cheap follow-on polish (see Prior decisions).

## Constraints

- **`protocol` is a deliberate API surface** (constitution): the request, the
  reply, the two supporting types, and the `Attach.root` field are an intentional,
  reviewed extension; `PROTOCOL_VERSION` bumps `9 → 10` and the fingerprint test
  re-pins, so the message-set change cannot merge without the bump (CI-enforced).
  The value is the next free number at merge time (Phase 35, milestone #53, is
  protocol-free, so `9` stands until this lands).
- **Daemon owns the filesystem — the browse runs daemon-side, never client SFTP**
  (the Phase-30 foundation contract; the daemon watches, reads, and writes the
  remote tree). The client sends intent (`QueryDirEntries`); the daemon executes
  with `std::fs::read_dir`. No SFTP layer, no second transport. Mirrors Zed's
  remote server (prior-art index).
- **The browse read is NOT root-confined** — deliberately unlike the Phase-30
  write path. Its purpose is to pick a *new* root, so it accepts an absolute path
  and reads any directory the daemon user can, with no `buffer::resolve`
  confinement and no context/`State` borrow. This is not a privilege escalation:
  arbitrary absolute-path reads are already exposed (the `OpenFile` out-of-root
  read carve-out, `crates/daemon/src/lib.rs`) and the SSH user runs a shell in
  every tmux pane (it can `ls` anything); the one genuinely new capability is
  directory **enumeration** (discovery). Recorded as a Prior decision.
- **Async, non-blocking browse** (constitution: async for I/O): each level is a
  daemon round-trip; the picker shows a per-level loading state and never blocks
  the render thread. A slow / large directory must not freeze the UI.
- **Depends on Phase 35** (`@root` stamp + per-root context, milestone #53) and
  **Phase 34** (`-c` on `new-session`), and **reuses** Phase 33's post-connect
  picker container + entry-point model and Phase 32's session strip. Phase 36
  writes `@root` = the picked root using Phase 35's stamp (extended to take the
  per-attach `root`); it does not re-open the stamp or the re-root.
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
- No new dependency (`std::fs` / `tokio::fs`; the git-repo flag is a `.git`
  existence check and the branch a `.git/HEAD` file-read parse, not a `gix` read);
  crate boundaries via `lib.rs`; English; no emojis.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| **The browse is a new daemon-side `protocol` dir-listing channel (`QueryDirEntries` → `DirEntriesReply`), executed with `std::fs::read_dir`, never client SFTP** | The daemon owns the remote filesystem (it watches, reads, saves, mutates the git index). Directory browsing is the same read capability; a client SFTP layer would be a second transport for a job the daemon is positioned to do — mirroring Zed's remote server (`docs/prior-art.md`, "Session ↔ project root coupling — prior-art index", Phase 36 rows) and the Phase-30 file-op precedent. | 2026-07-09 |
| **The browse handler is NOT root-confined** — it accepts an absolute path, does no `buffer::resolve`, holds no context/`State`, and reads anywhere the daemon user can | Its purpose is to choose a *new* project root, so confining it to an existing worktree root would defeat it. The one genuinely-new capability is directory **enumeration** (discovery): reading arbitrary absolute paths is already exposed (the `OpenFile` out-of-root read carve-out, `crates/daemon/src/lib.rs`) and the SSH user runs a shell in every pane (it can `ls` anything). So this is not a privilege escalation — a discovery convenience over reads the user already has. The Phase-30 write confinement is a write-path concept and does not apply. | 2026-07-09 |
| **The picked root reaches the daemon via a new `root: Option<String>` on `Attach`; the daemon threads it into the Phase-34 `-c` and the Phase-35 `@root` stamp** | `Attach` today carries only a session name and the daemon spawns `new-session` with its *own configured* root, so a create-at-picked-root needs a wire channel — the minimal one is an optional `root` on the existing attach-or-create path (not a separate `CreateSession` message, which would duplicate the attach flow). `None` = unchanged; `Some` = create at the picked root. Phase 36 stays additive over 34/35: it reuses their `-c` and `@root` mechanics, only supplying a per-attach value. | 2026-07-09 |
| **The picker guarantees a fresh session name (disambiguates against the live list) before a create; the daemon stamps `@root` only when `Attach.root` is `Some`** | `new-session -A -s <name>` **attaches** an existing session of that name and ignores `-c`, so a colliding basename would silently land in — and, without the guard, re-stamp the `@root` of — an unrelated project. The **client-side disambiguation is the guarantee** (the picker holds the live `QuerySessionList` and renames `rift` → `rift-2`); the `Some`-only stamp only spares the `None` path (reconnect / switch / pick-existing never re-stamp), not a `Some` + colliding-name race — which the disambiguation, not the stamp, prevents. The residual `Some` + same-name shared-daemon TOCTOU window is negligible and recoverable for a personal tool. | 2026-07-09 |
| **The picker supersedes the zero-sessions empty-state screen** — with no sessions on the host, connecting opens the root picker directly; the session list shows only when sessions exist; `RIFT_SESSION` stays the picker-skipping fast-path | "No sessions → you must create one → creating means picking a root", so a distinct empty-list screen is redundant (idea sparring, this session; the "Session flows" artboard Path A and Frame B's superseded note encode it). The Phase-33 zero-sessions edge is re-pointed at the root picker. | 2026-07-09 |
| **The session name defaults to the folder basename, editable before Create** | "session = project" — the name falls out of the chosen folder, removing the "name it first" friction; still editable for when the basename is not the wanted session name. Matches the `sesh` / `tmux-sessionizer` folder-basename convention (`docs/prior-art.md`, Phase 36 rows). | 2026-07-09 |
| **The root picker is a modal/panel over the Phase-33 picker container and the in-cockpit workspace — not a new `Shell` state** | Phase 33 already introduced the pre-cockpit picker `Shell` state; the root picker is a mode within it (post-connect) and a modal over the workspace (in-cockpit strip "+"). Reusing those containers avoids a fourth top-level state and a second session-creation UI. | 2026-07-09 |
| **Browse lists directories only, name-sorted, symlinked dirs followed/included, async per level** | A project root is a directory, so files are noise; following symlinks includes a symlinked project dir (the usual picker expectation); per-level round-trips keep each response small and the UI responsive (constitution: async for I/O; never block the render thread). | 2026-07-09 |
| **v1 lists all child directories including dotfiles; a hide-dotfiles toggle is deferred** | Simplicity — no filter state in v1; a dotfile-heavy `$HOME` is the user's own tree, and hiding it risks concealing a wanted `.dotfiles` / `.local/...` root. A hide toggle is a cheap follow-on polish, not a v1 scope item. | 2026-07-09 |
| **Show the `is_git_repo` flag AND the current branch** (resolved at the acceptance gate) | The human chose the fuller signal (the "Session flows" mockup's `⑂ <branch>` badge). The branch is parsed from `<dir>/.git/HEAD` — a plain file read, not a `gix` dependency — so a repo row shows its branch (a detached HEAD / non-repo shows none). The per-directory `.git/HEAD` read runs on `spawn_blocking` with the listing. | 2026-07-09 |
| **Include a recents list of recently-picked roots** (resolved at the acceptance gate) | The picker starts from a phase-9-store list of recently-picked roots (extending the existing recents store, app-side, no protocol change), falling back to `$HOME`; a successful create records the picked root, so returning to a project does not require re-browsing. | 2026-07-09 |
| **Include per-session-root display (`SessionEntry.root`)** (resolved at the acceptance gate) | Phase 35 pointed here and the protocol is bumped regardless. `SESSION_LIST_QUERY` reads `#{@root}` and `SessionEntry` gains `root: Option<String>`, so the list / picker show each session's project path. Reading `#{@root}` depends on the Phase-35 stamp (milestone #53). | 2026-07-09 |
| **Issues #766 (protocol) and #767 (daemon browse handler) land in one PR, not two** | A protocol-only PR cannot stay workspace-green: `QueryDirEntries` is a new `ClientMessage` variant, and the daemon's exhaustive dispatch matches (`serve_connection`, `handle_client_message`) must acknowledge it somehow — the constitution forbids `todo!()`/`unimplemented!()` in merged code, and issue #766's own acceptance requires `cargo clippy --workspace -- -D warnings` / `cargo test --workspace` to pass. A real (non-stub) handler for the new variant IS #767's deliverable, so the two issues are one atomic compilable unit; `Attach.root`/`SessionEntry.root` ride along as behavior-identical `None`/`..`-ignored stubs (their real wiring stays #769/#770). | 2026-07-09 |

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
`reply(msg) -> DaemonMessage`, `spawn_blocking`, up-front existence check +
`io::ErrorKind` → typed error shape — Phase 30); `crates/daemon/src/lib.rs` (the
`serve_connection` reply dispatch and the `OpenFile` out-of-root read carve-out);
`spec-per-session-project-root.md` (the `@root` stamp + per-root re-root — Phase
35); `spec-post-connect-picker.md` (the pre-cockpit picker container + entry-point
model + `Attach` — Phase 33); the Phase-34 `spawn_args` `-c`.

## Human prerequisites

None. The daemon already runs on the remote host with filesystem read access (it
watches and reads the tree); directory browsing is that same capability. No new
secret, account, or provisioning. The `PROTOCOL_VERSION` bump is client+daemon
lockstep, already the project's deploy discipline (the daemon binary is
redeployed per session). No new dependency.

## Foundation impact (ratified in this spec's PR)

Per the roadmap's recorded Phase-36 impact (never edited from the roadmap):

- `protocol` gains the directory-browse channel (`QueryDirEntries` +
  `DirEntriesReply` + `DirEntry` + `DirBrowseError`), a `root: Option<String>`
  field on `Attach` (the create-with-root transport), and a `root` field on
  `SessionEntry` (per-session project path), `PROTOCOL_VERSION` `9 → 10`,
  documented in `docs/protocol.md` — the daemon's first filesystem **browse** read.
- `docs/architecture.md`: a one-line note that the daemon exposes an
  **unconfined directory-browse read** (distinct from the root-confined Phase-30
  write path; a discovery convenience over the already-exposed `OpenFile` read and
  the shell in every pane), so the browse capability's trust boundary is on record.
  The per-session context substrate itself is already Phase 35's foundation change;
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
      value; `docs/protocol.md` documents the directory-browse channel, the
      `Attach.root` field, and a version-10 history line. Serde round-trip tests
      cover every new/changed variant (valid + unknown-tag rejection);
      `DirEntriesReply` omits `error` on success, `Attach` round-trips with `root`
      both `None` and `Some`, `SessionEntry` round-trips with and without `root`,
      and `DirEntry` carries `git_branch` (present / absent).
- [ ] Daemon browse tests (mirroring `file_ops` tests, over a `tempfile` tree):
      `QueryDirEntries` on a directory returns its child directories name-sorted
      (files excluded), including a **symlinked** directory and **dotfile**
      directories, with `is_git_repo` true and `git_branch` set from `.git/HEAD`
      for a child containing `.git` (detached HEAD / non-repo → `git_branch`
      `None`); an empty/`~` request resolves to `$HOME` (and degrades to `/` when `HOME` is
      unset); a missing path replies `NotFound` and a non-directory replies
      `NotADirectory` via the up-front `metadata` check; an unreadable dir replies
      `PermissionDenied` (best-effort — skipped when the suite runs as root); the
      daemon never aborts on any of them.
- [ ] Root picker (client): browsing descends level-by-level over
      `QueryDirEntries` with a loading state; the breadcrumb reflects the resolved
      path; selecting a folder pre-fills the name field with its basename; an error
      reply renders inline without closing the picker. Asserted headlessly over the
      picker state where tests reach it; the visual treatment is the QA gate.
- [ ] Create-with-root: confirming sends `Attach { session, root: Some(picked) }`;
      the daemon spawns `new-session -A -s <name> -c <picked>` and stamps `@root
      <picked>` (the picked root, not `RIFT_PROJECT_ROOT`), then attaches. Headless
      assertions: `Attach.root` round-trips the picked path; `spawn_args` with a
      `Some` root emits `-c <picked>` (and omits it / uses the default for `None`);
      the picker disambiguates a name that collides with a live-list session before
      Create; the `@root` stamp fires only for `Some`.
- [ ] Entry points: the post-connect picker's "+ New session…" and the in-cockpit
      strip's "+" both open the root picker; a host with **no sessions** opens the
      root picker directly (no empty-state list screen); `RIFT_SESSION` still
      attaches directly with no picker.
- [ ] Session-list root + recents: `SESSION_LIST_QUERY` parses `#{@root}` into
      `SessionEntry.root` and the list / picker render each session's project path;
      the picker's start level comes from the phase-9 recents-of-roots store
      (falling back to `$HOME`), and a successful create records the picked root.
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
| An unconfined browse read is a wider surface than the confined write path | It is a **read** the user already has: arbitrary absolute reads via the `OpenFile` carve-out and a shell in every pane. The one new bit is directory enumeration (discovery); no write, no new privilege. Recorded as a Prior decision and noted in `architecture.md`; the write path stays confined. |
| A huge directory (`$HOME` with thousands of entries) stalls the listing / render | The read runs on `spawn_blocking` (off the render thread); the reply carries directories only (files excluded), name-sorted; the list uses the existing virtualized/scroll primitives. Per-level round-trips keep each response bounded to one directory. |
| Browse latency makes the picker feel unresponsive | Every level is async with a loading state; the picker never blocks. Selecting a still-cached ancestor via the breadcrumb re-queries but renders immediately from the last reply if unchanged (client-side, not a protocol concern). |
| A denied / vanished directory mid-browse | The daemon returns a typed `DirBrowseError`; the picker renders it inline and keeps the last good level, never tearing down. |
| Phase 35's `@root` stamp is not yet merged when the create-with-root issue runs | The milestone carries `Depends on milestone: #53` and the create issue depends on the Phase-35 stamp issue; the loop parks it until Phase 35 lands. The protocol + daemon browse + picker-UI issues do not need Phase 35 and can proceed. |
| `new-session -A` with a colliding basename attaches an existing session and re-stamps its `@root` (cross-project surprise) | The client-side fresh-name check against the live `QuerySessionList` (disambiguates `name` → `name-2`) before Create is the guarantee a create does not land in an unrelated session; the daemon's `Some`-only stamp only spares the `None` path. A `Some` + same-name race is a negligible, recoverable shared-daemon TOCTOU. |
| The message-set change merges without the version bump | The fingerprint test fails on any message-set change until `PROTOCOL_VERSION` bumps and re-pins — CI-enforced, the same gate every prior protocol change passed. |
| Scope creep into a project registry / multi-context UI | Explicitly out of scope: durable root stays in `@root`, recents (if in scope) is a lightweight phase-9 mapping, Scenario 2 stays deferred. |

## Decision log

- 2026-07-09: Spec created from `/loopkit:plan` (roadmap Phase 36 — new-session
  remote root picker), the "project picker" Phase 35 explicitly deferred
  ("choosing / saving an arbitrary root when creating a rift session … a follow-on
  phase"). Grounded in the Phase-30 file-op protocol/daemon precedent (the
  `reply(msg) -> DaemonMessage` + `spawn_blocking` + up-front-check + typed-error
  shape, applied to a **read**), the Phase-35 `@root` stamp + per-root re-root, and
  the Phase-33 post-connect picker + entry-point model. Settled: the browse is a
  new daemon-side dir-listing channel (`PROTOCOL_VERSION 9 → 10`), **not**
  root-confined; the root is applied through Phase-34 `-c` + Phase-35 `@root`; the
  picker supersedes the zero-sessions empty-state; name defaults to the folder
  basename. Three open decisions carried to the acceptance gate, each with a
  proposed answer: git flag/branch richness, recents in scope, and per-session-root
  display (`SessionEntry.root`). Visual contract: the Paper "Session flows" +
  "Session management" Frame C artboards from this session's design sparring.
- 2026-07-09: Fresh-context spec review (VERDICT REQUEST_CHANGES → addressed). The
  reviewer verified the protocol/daemon/reuse claims TRUE (`PROTOCOL_VERSION` 9,
  the `file_ops` shape, `@root` absent from the codebase so `SessionEntry.root` is a
  real addition) and the three OPEN decisions as genuinely open. Blocking B1: the
  original "picker only supplies the value, machinery unchanged" claim was false —
  `Attach` carries only a session name and the daemon spawns `new-session` with its
  *configured* root, so the picked root had no wire channel, and the two entry
  points differ (in-cockpit has a control child, the pre-attach picker does not).
  Resolved by adding a `root: Option<String>` field to `Attach` (the single
  transport for both entry points, folded into this bump), specifying the daemon
  threading and the basename-collision guard (fresh-name disambiguation + `Some`-only
  stamp). Non-blocking addressed: symlink contract corrected (follow via
  `fs::metadata`, include symlinked dirs); `$HOME` source named (`std::env::var`
  with a `/` degrade); deterministic up-front `metadata` check for
  `NotFound`/`NotADirectory` (not `ErrorKind::NotADirectory`); the unconfined-read
  rationale corrected (the `OpenFile` carve-out + shell-parity, enumeration is the
  new bit); dotfiles listed in v1 (toggle deferred); the query-reply shape noted; and
  symlink/dotfile/collision + best-effort-`PermissionDenied` verification added.
- 2026-07-09: Spec-acceptance gate. The three open decisions resolved toward the
  fuller scope: **git flag + current branch** (parsed from `.git/HEAD`, no `gix`),
  **recents included** (a phase-9-store list of recently-picked roots, app-side),
  and **`SessionEntry.root` included** (`SESSION_LIST_QUERY` reads `#{@root}`, the
  list / picker show each session's path — riding the milestone-#53 dependency).
  Human prerequisites: none. Status `DRAFT` → `READY`; milestone `Phase 360` created
  at acceptance with `Depends on milestone: #53`.
- 2026-07-10: Issue #770 implemented (per-session root display, no protocol
  change — `SessionEntry.root` already existed from #766). `SESSION_LIST_QUERY`/
  `SESSION_LIST_FORMAT` gained `#{@root}` positioned right before `session_name`
  (not after it): `session_name` is the one truly free-form, user-renamable
  field, so it keeps the "most-arbitrary-field-last" slot per
  [`ROOT_QUERY`]'s existing convention (`#{@root}` before `#{session_path}`) —
  `parse_session_line`'s `splitn` grew from 4 to 5 fields accordingly.
  App-side, `SessionListItem`/`SessionRow` both gained `root: Option<String>`,
  threaded from the daemon's `SessionEntry.root` with no re-derivation. Render
  placement: the Phase-32 title-bar strip chip (fixed 24px single-line height)
  shows the root as a small muted truncated label appended after the name on
  the SAME line (mirroring the existing attached-dot/windows-caption
  horizontal-secondary pattern) rather than growing chip height; the Phase-33
  post-connect picker's vertical row (more space) shows it as a muted line
  below the name. Both omit the label entirely for `root: None`.
