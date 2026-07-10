# Spec: Clone-a-repository into a new session

> Status: DRAFT
> Created: 2026-07-10
> Completed: —

A "Clone from URL" path in the new-session / root-picker flow: the operator
enters a git URL, the daemon clones it (via `gix`) into the browsed parent as
`<parent>/<name>`, and a session is created rooted at the checkout (`@root`
stamped) — binding clone → session=project in one step. Closes the cold-start
gap: connecting to a parent like `/workspace` with nothing cloned yet no longer
forces a throwaway shell session to run `git clone` by hand. The daemon is
already on the remote, so it clones with the host's own credentials — no
credential forwarding.

## Outcome

- [ ] From the root picker, entering a git URL clones the repo on the **remote**
      (the daemon, via `gix`) into `<parent>/<name>` and, on success, creates a
      session rooted at the checkout with `@root` stamped — the reactive layer
      (file tree / git / diagnostics) comes up on the freshly cloned tree with no
      manual `git clone` and no throwaway parent session.
- [ ] The clone runs entirely daemon-side with the host's own git credentials
      (no token is sent from the client, no credential forwarding); a **public**
      repo clones with no configuration. **Private**-repo auth is spike-confirmed:
      the daemon clones a private repo using the host's credentials — via gix's
      credential path, or, if a bare `GIT_AUTH_TOKEN` is not honored there, by the
      daemon wiring the ambient token into gix (still no client-sent token).
- [ ] A failed clone (bad URL, auth failure, target already exists, network
      error) surfaces a clear error in the picker and creates **no** session and
      **no** partial directory — never a half-clone left on disk.
- [ ] `protocol` gains a clone request/reply channel; `PROTOCOL_VERSION` bumps
      `10 → 11` and the fingerprint test is re-pinned.
- [ ] The daemon stays a self-contained static musl binary: cloning is pure-Rust
      (`gix` + a rustls HTTP transport), no dependency on a system `git` on the
      target and no `libgit2`/OpenSSL. `cargo deny check licenses` passes with the
      added transitive crates.

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
  fingerprint test.
- **`crates/daemon`**: a new `clone` module. It reuses the browse channel's
  **message** shape (one request → one data-or-error reply) but NOT its dispatch:
  the browse listing is awaited **inline** in the per-connection
  `serve_connection` loop (`lib.rs:1211`), correct only because a listing is
  sub-second. A clone is unbounded (seconds to minutes), so it must **not** block
  that loop — else the connection's terminal output and all further inbound
  messages stall for the clone's duration and a hung clone cannot be interrupted.
  The clone therefore runs as a **detached task**: the dispatch returns
  immediately, the task runs the clone on `spawn_blocking` (network + disk bound),
  and posts one `CloneResult` on the connection when it finishes. Cancellation
  rides gix's own cooperative interrupt — `fetch_then_checkout(progress,
  should_interrupt: &AtomicBool)` (gix 0.84) — so a wrong/hung clone is abortable
  without wedging the connection. The module resolves `<parent>/<name>` under the
  same path-resolution/validation as browse, refuses when the target already
  exists (no clobber), and executes `gix::prepare_clone(url, path)?` →
  `fetch_then_checkout` → `main_worktree` checkout. **No partial tree on failure**:
  gix's `PrepareFetch`/`PrepareCheckout` delete the directory they created on
  `Drop` unless `persist()`d, so the error path simply does not persist (no
  hand-rolled temp-then-rename); a daemon kill mid-clone skips `Drop` and may
  leave a partial dir — acceptable for v1. Auth is left to gix's
  git-config-honoring credential path.
- **gix feature enablement (named dependency change)**: the workspace `gix`
  (`Cargo.toml`, today `default-features = false`, features `["status",
  "dirwalk", "revision", "sha1", "max-performance-safe", "blob-diff"]`) gains the
  clone-capable features, named exactly (verified against gix 0.84, not deferred
  to the spike): **`blocking-http-transport-reqwest-rust-tls`** (transitively
  enables `blocking-network-client` + gix's credentials/transport) **and**
  **`worktree-mutation`** (separate — not pulled by the transport; required by
  `main_worktree`/`fetch_then_checkout` for the checkout). This pulls `reqwest`
  transitively (`rustls` is already in `Cargo.lock`) — pure-Rust, musl-clean (the
  same reason gix was chosen over `git2`/`libgit2`); `reqwest` is enabled with a
  rustls TLS backend and no default features (no OpenSSL). Must pass
  `cargo deny check licenses`. curl / OpenSSL gix transports are rejected (not
  musl-static-clean). The daemon spike confirms the musl build, not the feature
  names.
- **`crates/app`**: extend the root picker (Frame C) with a **clone mode** — a
  git-URL field + target parent (default = the current browse path) + name
  (default = the repo basename from the URL — strip a trailing `/` and `.git`,
  and handle scp-like `git@host:org/repo.git` — editable) + a Clone action;
  an in-progress state while the clone runs; on `CloneResult` success, drive the
  existing create-with-root path (`Attach { session, root: Some(<checkout>) }`,
  the same path the browse-and-pick Create uses); on error, show it inline. The
  visual contract is the Paper `rift` file, "Session management" **Frame C**
  (root-picker anatomy) extended with the clone mode.
- **Docs**: `docs/protocol.md` gains the clone channel; `CLAUDE.md` / `AGENTS.md`
  container-workflow note mentions clone-to-start as the cold-start path.

### Out of scope

- **Streamed clone progress** (objects/bytes/percent). v1 is coarse: an
  in-progress state in the picker, then success/error. A gix-progress-driven
  stream is a later enhancement (would add progress messages to the channel).
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

- **musl-static self-containment (`docs/constitution.md`).** The daemon targets
  `x86_64-unknown-linux-musl` and must stay a self-contained static binary — the
  reason `git2`/`libgit2` were ruled out. Cloning therefore must be pure-Rust:
  `gix` + a **rustls** HTTP transport (not curl/OpenSSL, not system `git`). No
  new target runtime dependency.
- **gix can clone, but the current feature set cannot.** `gix::prepare_clone`
  exists at the pinned `0.84` and does fetch + main-worktree checkout honoring
  git auth config, but the workspace enables `gix` with `default-features =
  false` and **no** network/HTTP-transport/checkout features — so clone support
  is a deliberate feature (and transitive-dependency) addition, named here so the
  implement loop may add it (workflow autonomy: deps named in the spec).
- **Message shape follows browse; dispatch does not.** The clone reuses browse's
  one-request → one-reply message shape, but a clone is unbounded, so — unlike
  browse's inline await in the per-connection dispatch loop (`lib.rs:1211`) — it
  runs detached and posts `CloneResult` on completion, or it would freeze the
  connection's terminal and message flow (see the `crates/daemon` scope).
  `protocol` additions are a deliberate API change (`PROTOCOL_VERSION` bump +
  fingerprint re-pin), per `docs/constitution.md`.
- **No clobber, no partial tree.** The daemon refuses a target that already
  exists and must not leave a half-cloned directory on failure (clone into the
  final path only on success, or clean up on error).
- **Remote-native auth is a differentiator, not a gap.** Because the daemon runs
  on the target, it uses the target's own credentials — the homelab devenv
  already provisions `GIT_AUTH_TOKEN`. Whether that env var is wired into gix's
  credential resolution (a configured git credential helper vs. a bare env var)
  is an implementation detail the daemon spike must confirm; if a bare
  `GIT_AUTH_TOKEN` is not honored by gix's config path, the daemon reads the
  ambient credential and provides it to gix (still no client-sent token).
- **Design phase not enabled.** `docs/design.md` does not exist, so no formal
  `/loopkit:design` step runs; the Frame C artboard in the Paper `rift` file is
  the visual contract, and the clone-mode extension is authored against it during
  implementation / visual-QA (as Phase 36's Frame C itself was).

## Prior art

- **Clone-a-repository into a session — prior-art index (Phase 42)** in
  `docs/prior-art.md` — VS Code "Git: Clone" / DevPod `devpod up <url>` / Gitpod
  (URL → parent → open, adopted and rebound to session=project); `gix` clone
  (reuse — already the daemon's git dependency); the remote-native credential
  model (differentiation — no forwarding).
- `docs/archive/spec-session-root-picker.md` (Phase 36) — the root picker
  (Frame C) + create-with-root (`Attach { root: Some(...) }`) path this extends;
  the clone mode is a sibling of its browse-and-pick mode.
- `docs/archive/spec-per-session-project-root.md` (Phase 35) — the `@root` stamp
  the created session carries; unchanged here.

## Human prerequisites

- none for build/test — the clone is exercised against public repos in daemon
  tests; the gix feature/dep additions are named above and land in-repo.
- Behavioural QA of **private**-repo clone needs the target host/container to
  hold git credentials the daemon can use. The homelab devenv provisions
  `GIT_AUTH_TOKEN` (present), but whether gix honors a bare env var vs. a
  configured credential helper is spike-confirmed (see Risks); no NEW provisioning
  is required beyond the token the devenv already sets.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Clone runs **daemon-side via `gix`**, pure-Rust, no system `git` | musl-static self-containment (constitution ruled out `git2`/`libgit2`); `gix` is already the daemon's git dependency and `prepare_clone` does fetch + checkout honoring auth config | 2026-07-10 |
| HTTP transport is **rustls** (`reqwest` rustls family), not curl/OpenSSL | Only a pure-Rust TLS stack is musl-static-clean; curl/OpenSSL reintroduce the native-linking problem gix was chosen to avoid | 2026-07-10 |
| gix clone features are a **named dependency addition** (network + rustls HTTP transport + worktree checkout); exact feature names pinned by the daemon spike | The current `default-features = false` gix set has no network; the spec names the dep so the implement loop may add it (workflow autonomy); must pass `cargo deny check licenses` | 2026-07-10 |
| New **request/reply clone channel** (`CloneRepo` → `CloneResult`), query-reply data-or-error shape; `PROTOCOL_VERSION` bump (10 → 11, next free at merge) | Mirrors the browse channel's message shape (`QueryDirEntries` → `DirEntriesReply`); a `protocol` addition is a deliberate API change (fingerprint re-pin) | 2026-07-10 |
| Clone dispatch is a **detached task**, NOT inline like browse; cancellation via gix's `should_interrupt` flag | A clone is unbounded; awaiting it inline in `serve_connection` (`lib.rs:1211`, as browse does) would stall the connection's terminal + inbound messages for the clone's duration and make a hung clone un-cancellable | 2026-07-10 |
| No partial tree via gix's Drop cleanup (don't `persist()` on error), not temp-then-rename | gix's `PrepareFetch`/`PrepareCheckout` remove the dir they created on Drop unless persisted, so the mechanism is already there; a daemon-kill mid-clone leaving a partial dir is an accepted v1 edge | 2026-07-10 |
| **No client-sent credentials / no forwarding**; the daemon uses the host's ambient git credentials | The remote-native differentiator — the daemon is already on the target with its own creds (devenv `GIT_AUTH_TOKEN`); avoids an auth UI and a security surface | 2026-07-10 |
| v1 is **coarse progress** (in-progress → success/error), **URL-only** (default branch), **no mkdir** | Proportional first cut; streamed progress, ref selection, and empty-folder scaffolding are deferred enhancements that add surface without closing the cold-start gap | 2026-07-10 |
| No clobber; **no partial tree** left on failure | A half-clone masquerading as a project would corrupt the reactive layer; clone materializes at the final path only on success | 2026-07-10 |
| OPEN — clone surface: a **mode inside Frame C** (Browse ⇄ Clone toggle in the existing root picker) vs a **distinct "New from URL" entry point** | resolved at the spec-acceptance gate | — |

## Tracking

The decomposition into steps lives as GitHub issues, one per implementable step,
under the milestone. This spec owns the design; the issues own progress.

- Milestone: created at the spec-acceptance gate.
- Issues: created from this spec after merge.

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace
      excluding `rift-app`); `app-check` compiles `rift-app`; the daemon musl
      build succeeds with the added gix/rustls features.
- [ ] `cargo deny check licenses` passes with the transitive `reqwest`/`rustls`
      crates added.
- [ ] `protocol`: `PROTOCOL_VERSION == 11`; the fingerprint test passes re-pinned;
      `CloneRepo`/`CloneResult`/`CloneError` round-trip serde (valid + error).
- [ ] Daemon clone tests (over the network, or a local `file://` / bare-repo
      fixture to stay offline in CI): a public/`file://` URL clones into
      `<parent>/<name>` and the checkout is present; an existing target is refused
      with `CloneError::TargetExists`; a bogus URL yields `InvalidUrl` /
      `Transport` and leaves no directory behind.
- [ ] The clone dispatch is **non-blocking**: a clone in progress does not stall
      the connection — a live terminal in the same session keeps producing output
      during the clone (dev-channel QA), and the daemon-side clone task is
      interruptible (unit/integration over the `should_interrupt` path).
- [ ] Behavioural (dev-channel QA): from the root picker, cloning a **public**
      repo into `/workspace` creates a session rooted at the checkout and the file
      tree / git come up on it, with no manual `git clone`; a **private** repo
      clones against the devenv's `GIT_AUTH_TOKEN`; a bad URL shows an inline
      error and creates no session.
- [ ] The daemon binary remains self-contained: the musl release build links no
      system `git`/`libgit2`/OpenSSL (pure-Rust rustls transport).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| gix 0.84 clone + main-worktree checkout + HTTPS auth on musl is unproven in this codebase | **Spike first**: the daemon issue starts with a minimal `gix::prepare_clone` proof (public repo, then a token-auth private repo) building for musl, before the UI is wired. It de-risks the feature set + auth path up front. |
| Bare `GIT_AUTH_TOKEN` (env var) is not honored by gix's git-config credential path | The daemon reads the ambient credential and provides it to gix (URL/credential injection) — still no client-sent token; confirmed by the private-repo QA item. |
| rustls HTTP transport pulls a heavy transitive tree (`reqwest`) | Enable `reqwest` with rustls-tls and **no default features** (no OpenSSL); verify `cargo deny` and the musl build; if `reqwest` is too heavy, gix's lighter blocking-http transport variants are the fallback (still rustls) — a spike call, not a scope change. |
| Fallback to system `git` if gix clone proves insufficient | Explicitly a **last resort** and a separate decision (it reintroduces a target dependency, against self-containment) — the spike must fail conclusively first; not taken speculatively. |
| A slow clone with only a coarse in-progress state feels unresponsive | Acceptable for v1 (proportional); the channel is shaped so streamed progress can be added later without breaking the request/reply base. |

## Decision log

- 2026-07-10: Spec drafted from the Phase-42 seed. Clone runs daemon-side via
  `gix` (pure-Rust, musl-clean), a new `CloneRepo`/`CloneResult` protocol channel
  (v10 → 11) modeled on the browse channel, and extends the Frame C root picker;
  the created session reuses the shipped `@root`/create-with-root path. Key
  finding folded in at draft: `gix 0.84` has `prepare_clone` but the workspace's
  `default-features = false` gix has no network/transport/checkout features, so
  clone is a **named** gix-feature + `reqwest`/`rustls` dependency addition
  (musl-clean; curl/OpenSSL rejected). One open decision (clone surface: Frame C
  mode vs distinct entry) carried to the acceptance gate. `docs/design.md` is not
  set up, so no formal `/loopkit:design` step; the Frame C artboard is the visual
  contract.
- 2026-07-10 (spec review): folded in the review's findings pre-gate. BLOCKING
  (B1) — clone must NOT mirror browse's inline dispatch: an unbounded clone
  awaited inline in `serve_connection` (`lib.rs:1211`) would freeze the
  connection's terminal + messages, so the clone runs as a **detached task** and
  cancels via gix's `should_interrupt` flag (new Prior-decision row +
  `crates/daemon` scope + Constraint + Verification). Non-blocking findings: exact
  gix features named (`blocking-http-transport-reqwest-rust-tls` +
  `worktree-mutation`, verified against 0.84, not spike-deferred); no-partial-tree
  via gix's Drop cleanup (no `persist()` on error), not temp-then-rename;
  private-repo auth reworded to spike-confirmed (a bare `GIT_AUTH_TOKEN` is not a
  standard gix credential source); URL→basename parse rule pinned; `10 → 11`
  hedged to next-free-at-merge.
