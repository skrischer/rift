# Spec: Clone-a-repository into a new session

> Status: READY
> Created: 2026-07-10

A "Clone from URL" path in the new-session / root-picker flow: the operator
enters a git URL, the daemon clones it (by shelling out to the host's `git`)
into the browsed parent as `<parent>/<name>`, and a session is created rooted at
the checkout (`@root` stamped) — binding clone → session=project in one step.
Closes the cold-start gap: connecting to a parent like `/workspace` with nothing
cloned yet no longer forces a throwaway shell session to run `git clone` by hand.
The daemon is already on the remote, so the clone runs with the host's own git
credentials — no credential forwarding.

## Outcome

- [ ] From the root picker, entering a git URL clones the repo on the **remote**
      (the daemon, shelling out to the host's `git`) into `<parent>/<name>` and,
      on success, creates a session rooted at the checkout with `@root` stamped —
      the reactive layer (file tree / git / diagnostics) comes up on the freshly
      cloned tree with no manual `git clone` and no throwaway parent session.
- [ ] The clone runs entirely daemon-side with the host's own git credentials
      (no token is sent from the client, no credential forwarding); a **public**
      repo clones with no configuration, and a **private** repo clones against the
      host's ambient git auth — the daemon's `git clone` inherits the host's
      credential helpers / `GIT_AUTH_TOKEN` exactly as a terminal `git clone`
      would.
- [ ] A failed clone (bad URL, auth failure, target already exists, network
      error) surfaces a clear error in the picker and creates **no** session and
      **no** partial directory — never a half-clone left on disk.
- [ ] `protocol` gains a clone request/reply channel; `PROTOCOL_VERSION` bumps
      `10 → 11` and the fingerprint test is re-pinned.
- [ ] The daemon stays a self-contained static musl binary **and pure-Rust /
      C-free**: the clone shells out to the host's `git`, so the daemon embeds no
      HTTPS/TLS stack — no `reqwest`/`rustls`/`aws-lc-rs`, no `libgit2`/OpenSSL,
      and the musl build needs no C cross-compiler. The one accepted tradeoff is a
      runtime dependency on `git` being present on the target host. `cargo deny
      check licenses` passes with no clone-specific transitive crates.

## Scope

### In scope

- **`crates/protocol`**: a request/reply clone channel modeled on the existing
  browse channel (`QueryDirEntries` → `DirEntriesReply`, the query-reply
  data-or-error shape, not the `ok`-ack `FileOpResult` shape):
  `ClientMessage::CloneRepo { url, parent, name }` →
  `DaemonMessage::CloneResult { path, error: Option<CloneError> }`, with a
  `CloneError` enum (invalid URL, auth failed, target exists, network/transport,
  other) mirroring `DirBrowseError`. `PROTOCOL_VERSION` bumps to the next free
  value (10 → 11 unless another protocol-touching phase merges first); re-pin the
  fingerprint test. **Shipped in #827 — the wire contract is mechanism-agnostic
  (it carries a URL and a resolved path, not a transport), so the shell-out
  reimplementation leaves it unchanged.**
- **`crates/daemon`**: the `clone` module. It reuses the browse channel's
  **message** shape (one request → one data-or-error reply) but NOT its dispatch:
  the browse listing is awaited **inline** in the per-connection
  `serve_connection` loop (`lib.rs:1211`), correct only because a listing is
  sub-second. A clone is unbounded (seconds to minutes), so it must **not** block
  that loop — else the connection's terminal output and all further inbound
  messages stall for the clone's duration and a hung clone cannot be interrupted.
  The clone therefore runs as a **detached task**: the dispatch returns
  immediately, the task runs `git clone -- <url> <target>` as a child process
  (via `tokio::process::Command`), and posts one `CloneResult` on the connection
  when the child exits. Cancellation kills the child process (the task tracks the
  child handle; on `should_interrupt` it sends `start_kill` and reaps it), so a
  wrong/hung clone is abortable without wedging the connection. The module
  resolves `<parent>/<name>` under the same path-resolution/validation as browse,
  validates the URL's scheme up front (so a bogus string is not handed to `git`
  as an ambiguous argument) and passes it after a `--` separator, and refuses when
  the target already exists (no clobber). **No partial tree on failure**: `git
  clone` removes its own target on a failed clone, and the task additionally
  removes `<target>` on any non-success exit or on interrupt-kill, so a partial
  tree never survives. Auth is left to the host's git configuration (credential
  helpers / `GIT_AUTH_TOKEN` in the daemon's environment) — the same resolution a
  terminal `git clone` on the host uses.
- **Daemon dependency posture (reverting the gix-transport addition)**: the
  daemon does **not** gain any git HTTP-transport dependency. The `gix` clone
  features added in the first implementation
  (`blocking-http-transport-reqwest-rust-tls` + `worktree-mutation`) and their
  transitive tree (`reqwest`, `rustls`, `aws-lc-rs`, `webpki-root-certs`) are
  **removed**, along with the `deny.toml` `CDLA-Permissive-2.0` allowance they
  required and the direct `gix` dependency `crates/daemon/Cargo.toml` gained for
  the clone. `gix` stays in the workspace for the **local-read** features it
  already served (`status`, `dirwalk`, `revision`, `sha1`,
  `max-performance-safe`, `blob-diff` — all pure-Rust, no network, no TLS). Net:
  the daemon's musl build returns to needing **no C cross-compiler**.
- **`crates/app`**: the root picker (Frame C) **clone mode** — a git-URL field +
  target parent (default = the current browse path) + name (default = the repo
  basename from the URL — strip a trailing `/` and `.git`, and handle scp-like
  `git@host:org/repo.git` — editable) + a Clone action; an in-progress state while
  the clone runs; on `CloneResult` success, drive the existing create-with-root
  path (`Attach { session, root: Some(<checkout>) }`, the same path the
  browse-and-pick Create uses); on error, show it inline. The visual contract is
  the Paper `rift` file, "Session management" **Frame C** (root-picker anatomy)
  extended with the clone mode. **Shipped in #829/#835 — mechanism-agnostic, so
  the shell-out reimplementation leaves it unchanged.**
- **Docs**: `docs/protocol.md` documents the clone channel; `CLAUDE.md` /
  `AGENTS.md` container-workflow note mentions clone-to-start as the cold-start
  path (and that the target host must have `git`).

### Out of scope

- **Streamed clone progress** (objects/bytes/percent). v1 is coarse: an
  in-progress state in the picker, then success/error. Parsing `git clone`'s
  `--progress` stream is a later enhancement (would add progress messages to the
  channel).
- **Ref / branch / PR-slug selection** at clone time (DevPod's `@ref`). v1 clones
  the default branch; branch work happens in-session afterward.
- **Client-side credential forwarding or an auth UI.** The daemon uses the host's
  ambient git credentials; rift sends no token. A repo the host cannot
  authenticate to fails with `CloneError::AuthFailed` — the operator fixes
  credentials on the host (out of scope to manage them from rift).
- **A "new empty folder" (mkdir) affordance.** Considered in sparring, dropped:
  an empty dir still needs a clone/scaffold step, so clone-from-URL is the whole
  cold-start answer; mkdir adds surface without closing the gap.
- Any change to the shipped `@root` / session-create machinery (Phases 34–36) —
  reused unchanged; this only adds the clone that precedes the create.
- The daemon's `file_ops` write path (Phase 30) — clone is a distinct top-level
  op (a whole-repo network fetch), not an in-tree file op.

## Constraints

- **musl-static self-containment AND pure-Rust / no-C (`docs/constitution.md`).**
  The daemon targets `x86_64-unknown-linux-musl` and must stay a self-contained
  static binary built from **pure Rust with no C dependencies** — the reason
  `git2`/`libgit2` were ruled out. An embedded HTTPS git transport cannot satisfy
  this: `gix` over `rustls` pulls a C crypto backend (`aws-lc-rs`, or `ring` —
  both compile C and need a musl C cross-compiler), and the only fully-pure-Rust
  provider (`rustls-rustcrypto`) is explicitly not production-grade. Cloning
  therefore **shells out to the host's `git`**: the daemon stays pure-Rust/C-free
  and the musl build needs no C toolchain. The accepted tradeoff is a **runtime
  dependency on `git` on the target host** — a low, industry-standard assumption
  (VS Code, JetBrains, Zed, and every tmux agent-orchestrator already require the
  `git` binary), and a dev/agent host that runs coding agents in tmux always has
  it.
- **`gix` is retained for local reads only.** `gix::prepare_clone` exists at the
  pinned `0.84`, but enabling its network/HTTP-transport/checkout features is
  exactly what pulls the C crypto tree. The workspace keeps `gix` with
  `default-features = false` and its existing local-read features (no network),
  and the clone does not use `gix` at all.
- **Message shape follows browse; dispatch does not.** The clone reuses browse's
  one-request → one-reply message shape, but a clone is unbounded, so — unlike
  browse's inline await in the per-connection dispatch loop (`lib.rs:1211`) — it
  runs detached and posts `CloneResult` on completion, or it would freeze the
  connection's terminal and message flow (see the `crates/daemon` scope).
  `protocol` additions are a deliberate API change (`PROTOCOL_VERSION` bump +
  fingerprint re-pin), per `docs/constitution.md`.
- **No clobber, no partial tree.** The daemon refuses a target that already
  exists and must not leave a half-cloned directory on failure. `git clone`
  cleans up its own target on a failed clone; the daemon additionally removes
  `<target>` on any non-success exit or interrupt-kill, so a partial tree never
  survives (a daemon kill mid-clone that skips the cleanup is the one accepted v1
  edge — `git clone` will itself have removed most of it).
- **Remote-native auth is a differentiator, not a gap.** Because the daemon runs
  on the target, its `git clone` uses the target's own credentials — the homelab
  devenv already provisions `GIT_AUTH_TOKEN`, and the host's git credential
  helpers, `gh` credential helper, and SSH agent all apply transparently. This is
  strictly simpler than the embedded-transport path (which had to hand-wire a bare
  `GIT_AUTH_TOKEN` into the URL because gix's config-credential path did not honor
  it): a subprocess `git clone` inherits all of it for free. Still no client-sent
  token.
- **Design phase not enabled.** `docs/design.md` does not exist, so no formal
  `/loopkit:design` step runs; the Frame C artboard in the Paper `rift` file is
  the visual contract, and the clone-mode extension is authored against it during
  implementation / visual-QA (as Phase 36's Frame C itself was).

## Prior art

- **Clone-a-repository into a session — prior-art index (Phase 42)** in
  `docs/prior-art.md` — VS Code "Git: Clone" / DevPod `devpod up <url>` / Gitpod
  (URL → parent → open, adopted and rebound to session=project); the git-clone
  execution concern (the prior-art's own note flagged "fall back to system `git`"
  as the decision to make at plan time — now the chosen path); the remote-native
  credential model (differentiation — no forwarding).
- **Zed** (`crates/fs`, `crates/project`, `crates/git_ui`) — the canonical
  Rust+GPUI precedent: clone shells out to `git clone` via `std::process::Command`
  (PR #35606), and Zed deliberately **removed** `git2`/libgit2 (PR #53453, "~30k
  lines of vendored C and the associated build complexity") — network ops go
  through the `git` CLI, only local reads (diff) use pure-Rust (`imara-diff`).
  Exactly the split this spec adopts.
- `docs/archive/spec-session-root-picker.md` (Phase 36) — the root picker
  (Frame C) + create-with-root (`Attach { root: Some(...) }`) path this extends;
  the clone mode is a sibling of its browse-and-pick mode.
- `docs/archive/spec-per-session-project-root.md` (Phase 35) — the `@root` stamp
  the created session carries; unchanged here.

## Human prerequisites

- **`git` present on the daemon host / container** — the clone shells out to it.
  The VPS host and the devenv container must have `git` on `PATH`. (Confirmed
  present on both; a dev/agent host running coding agents in tmux always has it.)
  The daemon reports a clear error if it is missing (see Risks), rather than
  failing opaquely.
- Behavioural QA of **private**-repo clone needs the target host/container to
  hold git credentials the daemon's `git clone` can use — a configured credential
  helper or `GIT_AUTH_TOKEN` in the daemon's environment. The homelab devenv
  provisions `GIT_AUTH_TOKEN` (present); no NEW provisioning is required.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Clone runs **daemon-side by shelling out to the host's `git`** (`git clone`), NOT gix-in-process | The embedded-HTTPS path (`gix` + `rustls`) pulls a C crypto backend (`aws-lc-rs`/`ring`), violating the constitution's pure-Rust/no-C rule and breaking the musl build without a C cross-compiler; there is no production-grade pure-Rust TLS. Shelling out keeps the daemon pure-Rust/C-free and matches the ecosystem (Zed deleted libgit2 and shells out; VS Code/JetBrains/orchestrators require system `git`). **Reverses the original gix-in-process decision.** | 2026-07-10 |
| Accepted tradeoff: a **runtime dependency on `git` on the target host** | Low, industry-standard assumption; a dev/agent host running tmux agents always has `git`. The daemon detects its absence and reports a clear error. | 2026-07-10 |
| The daemon embeds **no git HTTP transport**: revert `gix`'s clone features (`blocking-http-transport-reqwest-rust-tls` + `worktree-mutation`) and their `reqwest`/`rustls`/`aws-lc-rs`/`webpki-root-certs` tree, the `deny.toml` CDLA allowance, and the direct daemon `gix` dep | Those are the only source of the C dependency; `gix` stays for local reads (status/diff) which are pure-Rust and network-free | 2026-07-10 |
| New **request/reply clone channel** (`CloneRepo` → `CloneResult`), query-reply data-or-error shape; `PROTOCOL_VERSION` bump (10 → 11); mechanism-agnostic wire | Mirrors the browse channel; the wire carries a URL + resolved path, not a transport, so the shell-out reimplementation reuses it unchanged (shipped in #827) | 2026-07-10 |
| Clone dispatch is a **detached task**, NOT inline like browse; cancellation kills the `git` child process on `should_interrupt` | A clone is unbounded; awaiting it inline in `serve_connection` (`lib.rs:1211`, as browse does) would stall the connection's terminal + inbound messages for the clone's duration and make a hung clone un-cancellable | 2026-07-10 |
| No partial tree: `git clone`'s own target cleanup + a defensive `<target>` removal on any non-success exit or interrupt-kill (not gix Drop) | With no gix in the clone path, cleanup is explicit; `git clone` already removes its target on failure, and the belt-and-suspenders removal covers the interrupt-kill case | 2026-07-10 |
| **No client-sent credentials / no forwarding**; the daemon's `git clone` uses the host's ambient git credentials | The remote-native differentiator — the daemon is already on the target with its own creds (devenv `GIT_AUTH_TOKEN` + credential helpers, inherited by the subprocess for free); avoids an auth UI and a security surface | 2026-07-10 |
| v1 is **coarse progress** (in-progress → success/error), **URL-only** (default branch), **no mkdir** | Proportional first cut; streamed progress (`git clone --progress`), ref selection, and empty-folder scaffolding are deferred enhancements that add surface without closing the cold-start gap | 2026-07-10 |
| No clobber; **no partial tree** left on failure | A half-clone masquerading as a project would corrupt the reactive layer; clone materializes at the final path only on success | 2026-07-10 |
| Clone surface is a **mode inside Frame C** — a `Browse ⇄ Clone` toggle in the existing root picker, NOT a distinct entry point | Resolved at the original spec-acceptance gate via an exploratory Paper sketch: at cold-start the picker is already open so clone is one toggle away, it keeps a single surface with minimal new chrome, and it extends the existing Frame C artboard rather than adding a second surface to maintain | 2026-07-10 |

## Tracking

The decomposition into steps lives as GitHub issues, one per implementable step,
under the milestone. This spec owns the design; the issues own progress.

- Milestone: [Phase 42 — Clone-a-repository into a new session](https://github.com/skrischer/rift/milestone/61)
- Issues: `crates/protocol` (#827, shipped), `crates/app` Frame C clone mode
  (#829, shipped), and the daemon clone execution — the daemon step is
  **re-opened as a new issue** for the shell-out reimplementation (the first
  daemon implementation, #828/#836, shipped the gix-in-process transport this
  revision reverses).

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace
      excluding `rift-app`); `app-check` compiles `rift-app`; the daemon musl
      build succeeds **with no C cross-compiler** (pure-Rust, C-free).
- [ ] `cargo deny check licenses` passes; the `reqwest`/`rustls`/`aws-lc-rs`/
      `webpki-root-certs` subtree and the `CDLA-Permissive-2.0` allowance are
      **removed** from the tree and `deny.toml`.
- [ ] `protocol`: `PROTOCOL_VERSION == 11`; the fingerprint test passes re-pinned;
      `CloneRepo`/`CloneResult`/`CloneError` round-trip serde (valid + error).
      (Shipped in #827; unchanged by this revision.)
- [ ] Daemon clone tests (a local `file://` / bare-repo fixture to stay offline in
      CI, cloned via the host `git`): a `file://` URL clones into `<parent>/<name>`
      and the checkout is present; an existing target is refused with
      `CloneError::TargetExists`; a bogus URL yields `CloneError::InvalidUrl` and
      leaves no directory behind; an unreachable/failed clone leaves no directory
      behind.
- [ ] The clone dispatch is **non-blocking**: a clone in progress does not stall
      the connection — a live terminal in the same session keeps producing output
      during the clone (dev-channel QA), and the daemon-side clone task is
      interruptible (kills the `git` child on `should_interrupt`; unit/integration
      over that path).
- [ ] Behavioural (dev-channel QA): from the root picker, cloning a **public**
      repo into `/workspace` creates a session rooted at the checkout and the file
      tree / git come up on it, with no manual `git clone`; a **private** repo
      clones against the devenv's host git credentials; a bad URL shows an inline
      error and creates no session.
- [ ] The daemon binary embeds **no HTTPS/TLS stack** (`reqwest`/`rustls`/
      `aws-lc-rs` absent from `Cargo.lock`) and **no** `libgit2`/OpenSSL; the musl
      release build needs no C cross-compiler; `git` is invoked as an external host
      binary.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `git` is absent on the target host/container | The daemon detects a missing `git` (spawn `NotFound`) and returns a clear `CloneError` (VS Code-style "git not found"), rather than failing opaquely; documented as a host prerequisite. A dev/agent host running tmux agents has `git`. |
| `git clone` error classification onto the wire `CloneError` | Map the child's exit status + stderr onto `CloneError` by recognizable substrings (authentication vs. could-not-resolve-host/network vs. already-exists vs. other), best-effort defaulting to `CloneError::Other` — the same "stable human-readable surface, no guessing" approach the first implementation used for gix's error `Display`. |
| An interrupt (connection drop) leaves a `git` child running or a partial dir | The task holds the child handle and `start_kill`s + reaps it on `should_interrupt`; the target dir is removed on any non-success/interrupt exit. |
| A slow clone with only a coarse in-progress state feels unresponsive | Acceptable for v1 (proportional); the channel is shaped so streamed progress (`git clone --progress`) can be added later without breaking the request/reply base. |

## Decision log

- 2026-07-10: Spec drafted from the Phase-42 seed. (Original approach — clone
  daemon-side via `gix` pure-Rust with a rustls HTTP transport; a new
  `CloneRepo`/`CloneResult` protocol channel v10 → 11 modeled on the browse
  channel; Frame C root-picker clone mode; the created session reuses the shipped
  `@root`/create-with-root path.) One open decision (clone surface: Frame C mode
  vs distinct entry) carried to the acceptance gate.
- 2026-07-10 (spec review): folded in the review's findings pre-gate. BLOCKING
  (B1) — clone must NOT mirror browse's inline dispatch: an unbounded clone
  awaited inline in `serve_connection` (`lib.rs:1211`) would freeze the
  connection's terminal + messages, so the clone runs as a **detached task** and
  cancels via a `should_interrupt` flag. Non-blocking findings folded in
  (no-partial-tree, URL→basename parse, `10 → 11` next-free-at-merge).
- 2026-07-10 (spec-acceptance gate): open decision resolved — the clone surface
  is a **mode inside Frame C** (`Browse ⇄ Clone` toggle in the root picker), not a
  distinct entry point. Status DRAFT → READY.
- 2026-07-10 (issue #829, `crates/app`): the Frame C clone mode implemented and
  merged (#835) — `PickerMode` `Browse ⇄ Clone` toggle, URL/parent/name inputs,
  `start_clone`/`apply_clone_result`, exact-match `pending_clone` correlation on
  the daemon-echoed `<parent>/<name>`, reusing the existing browse transport
  bridge and the create-with-root `Attach { root: Some(...) }` path.
  Mechanism-agnostic — unaffected by the transport reversal below.
- 2026-07-10 (issue #828, daemon clone via gix — SHIPPED then REVERSED): the
  gix-in-process transport was implemented and merged (#836): `prepare_clone` →
  `fetch_then_checkout` → `main_worktree` with the named gix 0.84 features
  (`blocking-http-transport-reqwest-rust-tls` + `worktree-mutation`), URL
  pre-validation, ambient-`GIT_AUTH_TOKEN` URL embedding, detached dispatch with
  `should_interrupt`, and gix-Drop no-partial-tree cleanup. **Defect found
  post-merge:** the rustls transport pulls **`aws-lc-rs`** (C/BoringSSL crypto),
  which needs an `x86_64-linux-musl-gcc` cross-compiler — the local daemon musl
  build fails (`ToolNotFound: x86_64-linux-musl-gcc`); CI passed only because it
  installs `musl-tools`. This **violates the constitution's pure-Rust / no-C
  daemon rule** (the same rule that ruled out `git2`/`libgit2`).
- 2026-07-10 (re-plan — transport reversal): the spec's founding premise
  ("cloning must be pure-Rust: `gix` + rustls") was **false** — `rustls`'
  crypto backends (`aws-lc-rs`, `ring`) are both C, and the only pure-Rust
  provider (`rustls-rustcrypto`) carries an explicit "DO NOT USE IN PRODUCTION"
  warning; `gitoxide`'s own mature CLI clones over curl+OpenSSL and treats its
  pure-Rust HTTPS transport as "less mature". Websearch prior-art (folded into
  `docs/prior-art.md`): **Zed** shells out to `git clone` (`std::process::Command`,
  PR #35606) and deleted `git2`/libgit2 (PR #53453) to escape ~30k lines of
  vendored C; **VS Code / JetBrains** require system `git`; **every tmux
  agent-orchestrator** (Claude Squad, Arbor, …) drives `git` via the CLI. The
  mainstream Rust pattern is: shell out to `git` for network ops, use `gix` only
  for local reads. **Decision: revert the gix-in-process HTTPS transport and
  reimplement the daemon clone by shelling out to the host's `git clone`** —
  restoring the pure-Rust/C-free daemon, getting host credential-helper auth for
  free, at the accepted cost of a `git`-on-host runtime dependency. The protocol
  channel (#827) and the app clone UI (#829/#835) are mechanism-agnostic and stay
  as shipped; only the daemon clone module + its dependency posture change. The
  daemon step is re-opened as a new issue for the shell-out reimplementation.
