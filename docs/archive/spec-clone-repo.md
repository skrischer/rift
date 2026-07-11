# Spec: Clone-a-repository into a new session

> Status: COMPLETED (2026-07-11)
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
      repo clones with no configuration. A **private** repo clones against the
      host's ambient git auth **when the host has a git credential helper
      configured** — the daemon's `git clone` inherits the host's credential
      helper / SSH agent / `gh` setup exactly as a terminal `git clone` would. A
      bare `GIT_AUTH_TOKEN` env var alone is NOT consumed by plain `git` (see the
      Remote-native auth constraint); the host must have a helper that resolves
      it.
- [ ] A failed clone (bad URL, auth failure, target already exists, network
      error) surfaces a clear error in the picker and creates **no** session and
      **no** partial directory — never a half-clone left on disk. If the host has
      no `git`, the picker shows a distinct actionable "git is not installed on the
      host" message (a `CloneError::GitUnavailable` variant), not a generic
      failure.
- [ ] `protocol` gains a clone request/reply channel (shipped in #827,
      `PROTOCOL_VERSION` 10 → 11); this revision adds one variant
      (`CloneError::GitUnavailable`), bumping `PROTOCOL_VERSION` 11 → 12 (next free
      at merge) and re-pinning the fingerprint test.
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
  other) mirroring `DirBrowseError`. **Shipped in #827** (`PROTOCOL_VERSION`
  10 → 11) — the wire contract is otherwise mechanism-agnostic (it carries a URL
  and a resolved path, not a transport), so the shell-out reimplementation reuses
  it. **This revision makes one additive change**: a `CloneError::GitUnavailable`
  variant so the git-absent case surfaces as a distinct, actionable picker
  message rather than a generic `Other`; `PROTOCOL_VERSION` bumps 11 → 12 (next
  free at merge) and the fingerprint test is re-pinned.
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
  when the child exits. Cancellation kills the child process: the task holds the
  child handle and `select!`s the child's exit against the interrupt signal (a
  `watch`/`Notify` — `should_interrupt` is no longer a cooperatively-polled flag
  inside gix, so the task must actively watch it alongside `child.wait()`),
  sending `start_kill` and reaping the child on interrupt, so a wrong/hung clone
  is abortable without wedging the connection. The module resolves
  `<parent>/<name>` under the same path-resolution/validation as browse, validates
  the URL's scheme up front — a **gix-free** check (a scheme allow-list plus
  scp-shorthand detection, since the daemon no longer carries a direct `gix` dep
  to borrow `gix::url::parse` from; its remaining job, now that `--` neutralizes
  option injection, is rejecting local-path-looking strings) — passes it after a
  `--` separator, and refuses when the target already exists (no clobber). The
  child is spawned with `LC_ALL=C` so `git`'s stderr is locale-stable for error
  classification. A spawn `NotFound` (no `git` on the host) maps to the new
  `CloneError::GitUnavailable`; the child's exit status + stderr map onto the
  other `CloneError` variants (auth / network / target-exists / other).
  **No partial tree on failure**: `git
  clone` removes its own target on a failed clone, and the task additionally
  removes `<target>` on any non-success exit or on interrupt-kill, so a partial
  tree never survives. Auth is left to the host's git configuration (a configured
  credential helper — a bare `GIT_AUTH_TOKEN` needs a helper to consume it, see
  the Remote-native auth constraint) — the same resolution a terminal `git clone`
  on the host uses.
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
  extended with the clone mode. **Shipped in #829/#835** — the shell-out
  reimplementation leaves it unchanged except one line: `describe_clone_error`
  gains the `CloneError::GitUnavailable` case (an actionable "git is not installed
  on the host" message).
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
  on the target, its `git clone` uses the target's own credentials — a configured
  git credential helper, the `gh` credential helper, `insteadOf` rules, and the
  SSH agent all apply transparently to the subprocess for free (strictly simpler
  than the embedded-transport path, which had to hand-wire a token into the URL).
  **One precise caveat:** plain `git` does **not** read a bare `GIT_AUTH_TOKEN`
  env var — there is no such standard git variable; the host must have a
  credential helper (or `insteadOf` / `.git-credentials`) configured to resolve
  its ambient credentials for `git`. The spec's retained #828 finding recorded
  exactly this (the gix impl had to embed the bare token because git's
  config-credential path did not honor it); the shell-out path faces the same
  constraint, now met by host git configuration rather than by rift. Still no
  client-sent token.
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
  hold git credentials the daemon's `git clone` can use — a **configured git
  credential helper** (e.g. `gh auth setup-git`, a `credential.helper`, or an
  `insteadOf` rule that injects the token), **not** merely a bare `GIT_AUTH_TOKEN`
  env var (plain `git` does not consume that on its own). The homelab devenv
  provisions `GIT_AUTH_TOKEN`; confirm the container's git is configured with a
  helper that resolves it — if the prior gix impl relied on rift embedding the
  bare token, this is the one behavioural item to re-verify so private-repo clone
  does not silently regress. Public-repo clone — the primary cold-start case —
  needs no credentials.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Clone runs **daemon-side by shelling out to the host's `git`** (`git clone`), NOT gix-in-process | The embedded-HTTPS path (`gix` + `rustls`) pulls a C crypto backend (`aws-lc-rs`/`ring`), violating the constitution's pure-Rust/no-C rule and breaking the musl build without a C cross-compiler; there is no production-grade pure-Rust TLS. Shelling out keeps the daemon pure-Rust/C-free and matches the ecosystem (Zed deleted libgit2 and shells out; VS Code/JetBrains/orchestrators require system `git`). **Reverses the original gix-in-process decision.** | 2026-07-10 |
| Accepted tradeoff: a **runtime dependency on `git` on the target host** | Low, industry-standard assumption; a dev/agent host running tmux agents always has `git`. The daemon detects its absence and reports a clear error. | 2026-07-10 |
| The daemon embeds **no git HTTP transport**: revert `gix`'s clone features (`blocking-http-transport-reqwest-rust-tls` + `worktree-mutation`) and their `reqwest`/`rustls`/`aws-lc-rs`/`webpki-root-certs` tree, the `deny.toml` CDLA allowance, and the direct daemon `gix` dep | Those are the only source of the C dependency; `gix` stays for local reads (status/diff) which are pure-Rust and network-free | 2026-07-10 |
| New **request/reply clone channel** (`CloneRepo` → `CloneResult`), query-reply data-or-error shape; `PROTOCOL_VERSION` bump (10 → 11); mechanism-agnostic wire | Mirrors the browse channel; the wire carries a URL + resolved path, not a transport, so the shell-out reimplementation reuses it (shipped in #827) — adding only one variant (below) | 2026-07-10 |
| Add a **`CloneError::GitUnavailable`** variant for the git-absent case (`PROTOCOL_VERSION` 11 → 12, fingerprint re-pin; one `describe_clone_error` line in the app) | Shelling out to host `git` introduces a git-absent failure mode; a distinct actionable "git not installed on the host" message (Zed-style clarity) beats a generic `Other`. A single additive variant is the proportionate cost — no message payload, no new channel; decided at the re-plan spec-acceptance gate | 2026-07-10 |
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
- [ ] `protocol`: the additive `CloneError::GitUnavailable` variant lands;
      `PROTOCOL_VERSION` bumps to 12 (next free at merge); the fingerprint test
      passes re-pinned; `CloneRepo`/`CloneResult`/`CloneError` round-trip serde
      (valid + each error variant, incl. `GitUnavailable`). (The channel itself
      shipped in #827.)
- [ ] Daemon clone tests (a local `file://` / bare-repo fixture to stay offline in
      CI, cloned via the host `git`): a `file://` URL clones into `<parent>/<name>`
      and the checkout is present; an existing target is refused with
      `CloneError::TargetExists`; a bogus URL yields `CloneError::InvalidUrl` and
      leaves no directory behind; an unreachable/failed clone leaves no directory
      behind; a missing `git` binary surfaces as `CloneError::GitUnavailable` with
      no panic and no directory left behind.
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
| `git` is absent on the target host/container | The daemon detects a missing `git` (spawn `NotFound`) and returns the new `CloneError::GitUnavailable`, which the picker renders as an actionable "git is not installed on the host" message (not a generic failure) — the one additive protocol change this revision makes. Also logged. A dev/agent host running tmux agents has `git`. |
| `git clone` error classification onto the wire `CloneError` | Map the child's exit status + stderr onto `CloneError` by recognizable substrings (authentication vs. could-not-resolve-host/network vs. already-exists vs. other), with `LC_ALL=C` set on the child so the substrings are locale-stable; best-effort defaulting to `CloneError::Other` — the same "stable surface, no guessing" approach the first implementation used for gix's error `Display`. |
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
- 2026-07-10 (re-plan spec review): folded in the review's findings pre-gate.
  BLOCKING B1 — `docs/prior-art.md`'s "Notes — ADOPT" clause still said "gix for
  the clone", contradicting the flipped verdict cell; flipped it to "shell out to
  the host's `git`". BLOCKING B2 — the private-repo auth wording overclaimed: plain
  `git` does not consume a bare `GIT_AUTH_TOKEN` env var (no such standard git
  variable), and the spec's own retained #828 finding records exactly that; the
  Outcome / Remote-native-auth constraint / Human-prerequisites now state that
  private-repo auth requires a host **credential helper** (`gh auth setup-git` /
  `credential.helper` / `insteadOf`), flagged as the one behavioural item to
  re-verify so private-repo clone does not silently regress (public-repo
  cold-start unaffected). Non-blocking, also folded in: `LC_ALL=C` on the `git`
  child for locale-stable stderr classification; a missing `git` surfaces as
  `CloneError::Other` on the wire (frozen enum, no message payload — clear only in
  the daemon log); the up-front URL validation is now gix-free (allow-list +
  scp-shorthand, no `gix::url::parse`); `should_interrupt` is watched via a
  `select!` on `child.wait()` since it is no longer a gix-internal polled flag.
- 2026-07-10 (re-plan spec-acceptance gate): one decision resolved — the
  git-absent case gets a **distinct `CloneError::GitUnavailable` variant** (over
  the review's non-blocking "keep it `Other`" suggestion), so the picker shows an
  actionable "git is not installed on the host" message (Zed-style clarity) rather
  than a generic failure. This makes the protocol a small additive change
  (`PROTOCOL_VERSION` 11 → 12, fingerprint re-pin) plus one `describe_clone_error`
  line in the app — the reimplementation issue now spans protocol + app + daemon,
  one coherent change. Auth/URL/network/target-exists failure modes were already
  actionable via their existing variants. Human prerequisites (`git` on the host;
  a credential helper for private-repo clone) confirmed as the delivery / QA
  handover; the operator verifies the devenv's git + credential-helper config
  before private-repo QA (public-repo cold-start unaffected). Spec accepted.
- 2026-07-10 (issues #841 + #839): daemon clone reimplemented as host `git
  clone` shell-out (gix transport + reqwest/rustls/aws-lc-rs reverted; C-free
  musl confirmed via Cargo.lock — the four crates the gix HTTP-transport
  feature pulled, `reqwest`/`aws-lc-rs`/`aws-lc-sys`/`webpki-root-certs`, are
  fully absent; one unrelated pre-existing `rustls` entry remains, pulled by
  the app crate's `gpui-component-assets`→`zed-reqwest` dependency via `ring`,
  unreachable from `rift-daemon`'s own dependency tree); `CloneError::GitUnavailable`
  added (`PROTOCOL_VERSION` 11→12, fingerprint re-pinned — unchanged
  numerically, since the fingerprint's extraction covers only the
  `ClientMessage`/`DaemonMessage` enum bodies, and neither's literal text
  changed by adding a `CloneError` variant). App clone-reply correlation made
  `~`-tolerant (issue #839's fix: `main.rs`/`workspace.rs` now check
  `pending_clone` via `root_picker::browse_reply_matches`, the same tolerant
  comparison `pending_browse` already used, instead of an exact-match check)
  + `name` validated client-side before send (`root_picker::invalid_clone_name`,
  mirroring the daemon's own `clone::validate_name`), using the reply's
  resolved path as the session root.
