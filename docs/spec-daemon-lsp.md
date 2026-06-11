# Spec: Phase 3 — LSP diagnostics

> Status: READY
> Created: 2026-06-10
> Completed: —

The daemon runs language servers on the remote host, feeds them the on-disk content of files as the watched worktree changes, and streams the diagnostics they publish to the client as live, per-file updates — delivering the north-star Scenario 1 signal: type errors surfacing as the agent edits, before its turn ends. This is the third consumer of the worktree foundation, alongside git status and the future explorer panel, and the live-error signal that makes rift an *IDE*, not merely an editor surface.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] When a tracked / non-ignored source file is created or modified in the watched worktree, the daemon ensures a language server for that file's language is running (started lazily, once per language, initialized at the worktree root) and feeds it the file's on-disk content as an LSP document open / change.
- [ ] Diagnostics the server publishes are streamed to the client, and the client holds an accurate, live per-file diagnostics model — each diagnostic carries a range, severity, message, and (when the server provides them) source and code; a file's diagnostics converge to what the server reports for the on-disk state.
- [ ] When the agent fixes an error and the file is rewritten, the corresponding diagnostic clears via a follow-up update: the client's set always reflects the server's latest `publishDiagnostics` for that file (full-set-replace per `(file, server)` — an empty set clears that server's diagnostics for the file).
- [ ] Two servers attached to the same language / file (e.g. a linter plus a type-checker) aggregate: the client's diagnostics for that file carry both servers' output, keyed by server so one server clearing its set does not drop the other's.
- [ ] Language servers run on the remote (daemon-side) as child processes managed off the daemon dispatch loop; diagnostics are published to consumers via the daemon's `watch` / `broadcast` channel — no `Arc<Mutex<State>>`, and the dispatch loop never blocks on server I/O.
- [ ] Ignored paths never drive a server: a write inside `target/`, `.git/`, or a `.gitignore`d path opens / changes no document and produces no diagnostics, consistent with the worktree snapshot excluding them.

## Scope

### In scope

- **`crates/lsp/` library** (new daemon-side crate): an `async-lsp`-based client that manages language-server child processes — a `Registry` mapping a `DocumentSelector` (language → server) to running server instances (multi-server-per-language), lifecycle (lazy start at the worktree root, reuse for the session, shutdown on daemon stop), and the JSON-RPC stdio transport (provided by `async-lsp`'s `MainLoop`). `gpui`-free, musl-clean. Translates `lsp-types` diagnostics into the rift protocol diagnostic types. Registered in the workspace and in `architecture.md`'s repo structure + technology map when it lands.
- **Disk-backed document sync**: driven by the explorer worktree change stream (`spec-daemon-filetree.md`), the daemon feeds the relevant server `didOpen` (first observation of a matching file), `didChange` (full text re-read from disk on modification), and `didClose` (on removal), so diagnostics reflect on-disk state. No editor-buffer integration — rift is not the editor and owns no buffers.
- **Built-in language → server table** keyed by `DocumentSelector`, resolving the server binary on the daemon's `$PATH`; **rust-analyzer is the proving server**. The table is data, not code — adding a language is a table entry. The server must already be installed on the remote (rift does not install servers, mirroring how it does not install agents).
- **Protocol**: an additive `crates/protocol` change — a `Diagnostics` `DaemonMessage` keyed by worktree-relative path, carrying rift's own serde-clean diagnostic types (range / severity / message / source / code), with full-set-replace per `(file, server)` and a **server-id key** so multi-server sets aggregate without clobbering. Supersedes the placeholder `diagnostics` sketch in `protocol.md` (align `uri` → worktree-relative path; no `lsp-types` leakage across the boundary). The exact message cardinality (one `(file, server)` set per message vs. a batched update) and the unknown-path reconciliation are pinned in the first protocol issue, mirroring how the git-status spec defers its reconciliation. No new `ClientMessage` — push-only; the client is a pure consumer in v1.
- **Daemon wiring**: the daemon owns LSP state in its single `State`, runs the `crates/lsp` client off the dispatch loop, subscribes to the explorer change stream to drive document sync, and routes `Diagnostics` onto the client broadcast channel alongside the worktree and git updates.
- **Client-side**: the client applies `Diagnostics` onto an in-memory per-file diagnostics model. Verifiable headless via tests / logging — the consuming state, not yet a rendered panel (data-layer-only, inherited from the file-tree / git-status scope decision).
- **Single watched root** = the same worktree root the file-tree spec watches (the daemon's launch directory for v1).

### Out of scope

- **Interactive LSP request/response features** — the *pull* half of LSP. **Navigation** (hover / go-to-definition / find-references) is a **committed sibling sub-spec sequenced with the editor track**, not a permanent cut: its only consumer is rift's own editor (`vision.md` Scenario 3 — the jump lands in rift's editor, no longer remote-controlling Neovim), which has no spec yet, so building its request/response plumbing now would be premature (`CLAUDE.md` "no premature abstraction"). **completion / rename / formatting / code actions** are editing-surface features that ride the editor spec, not LSP-on-the-worktree. None of this spec's push-only protocol changes for them.
- **The rendered diagnostics panel / inline error decoration** in a GPUI explorer or editor surface — its own sub-spec, the same one that renders the deferred file-tree and git-status panels (data-layer-only, inherited). That panel consumes this client-side diagnostics model.
- **Trust-gating language-server execution** (Zed's `TrustedWorktrees`) — v1's trust boundary is the SSH connection to a host the user owns and already runs unrestricted agents on; per-worktree trust prompts are deferred hardening, not a v1 need. This **deliberately narrows the scaffolding spec's pre-recorded *trust-gated* LSP lifecycle** (`archive/spec-daemon-scaffolding.md`, "Recorded for later sub-specs"): trust-gating is deferred; every other element of that pre-decision — daemon-side, lazy per-`DocumentSelector`, multi-server registry, `async-lsp` — stands.
- **Installing / bootstrapping language servers on the remote** — rift consumes servers already on `$PATH`, exactly as it consumes already-installed agents.
- **User-configurable server tables / per-project LSP settings / `initializationOptions` tuning** — built-in defaults for v1; configuration is a later extension.
- **Live editor-buffer (unsaved) diagnostics in v1** — v1 reflects on-disk state only. Unsaved-buffer diagnostics from *third-party* editors (Neovim, Helix) stay out permanently: those are black boxes (agent/editor-as-black-box rule). Unsaved diagnostics from **rift's own editor** are the forward path, not a contradiction — that is the disk→rift-buffer shift (see the disk-backed document-model row), owned by the editor spec, not v1.
- **Server-status / lifecycle protocol messages** (e.g. "rust-analyzer indexing…") — logged daemon-side; no client-facing status surface in v1.
- **Multi-root / per-pane-CWD LSP contexts** (`vision.md` Scenario 2) — single root for v1, mirroring the file-tree / git-status single-root cut.
- **LSP / diagnostics for the terminal-streaming path** — unrelated sub-spec.

## Constraints

- **Sequences after the file-tree milestone.** This spec can reach `READY` in parallel, but **implementation** sequences after the worktree file-tree sync lands (`spec-daemon-filetree.md`): the disk-backed document sync needs the explorer's worktree change stream and root to drive `didOpen` / `didChange`. Independent of git status (no ordering relationship between the two).
- **`crates/lsp` must cross-compile to static musl and stay `gpui`-free** — it becomes a daemon dependency, and the scaffolding dep-trim (PR #99) established that a daemon dep must be `gpui`-free and musl-clean before it is added to `crates/daemon/Cargo.toml`. Verify `async-lsp` + `lsp-types` (and their transitive tree) are musl-clean in the `daemon-musl` CI job, the same gate the explorer deps pass.
- **Language servers are external child processes** the daemon spawns on the remote; spawning and stdio are async, off the dispatch loop. The servers themselves are separately-installed binaries and are not subject to rift's musl constraint.
- **Push is the source of truth**; the client never derives diagnostics itself — it only applies daemon `Diagnostics` updates. Mirrors the worktree / git / tmux snapshot discipline (`CLAUDE.md` "state flows through channels").
- **Diagnostics honor the same ignore rules as the scan** — only files the snapshot exposes (tracked + non-ignored) ever open a document; ignored paths never drive a server.
- Adding to `crates/protocol/` is a deliberate, additive API change — both sides depend on it, never on each other. **`crates/protocol` stays free of `lsp-types`**: the daemon translates server diagnostics into rift's own protocol types.
- `async-lsp` and `lsp-types` are new dependencies needing the dependency-rule sign-off per `CLAUDE.md`. Both are license-compatible (`async-lsp` MIT OR Apache-2.0; `lsp-types` MIT) and gated by `cargo deny check licenses`. There is no native-API equivalent — an LSP client is protocol work. `lsp-types` is known to be **stalled** (`prior-art.md` caveat 6); record the `tower-lsp-community/ls-types` migration watch.
- **A one-day rust-analyzer round-trip spike precedes committing to `async-lsp`** — pre-recorded in the scaffolding spec and `prior-art.md` ("validate … before committing"). If the spike fails async-lsp's latency / ergonomics bar, fall back to forking `helix-lsp` (MPL-2.0, multi-server registry already implemented).
- `thiserror` in `crates/lsp`, `anyhow` in the daemon binary; no `.unwrap()` in library code.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **LSP runs daemon-side (remote)**, not client-side | Constraint-determined: `architecture.md` "Why LSP runs on the remote" — servers need the full remote project environment (`node_modules`, `target/`, `venv/`, `$GOPATH`), which is gigabytes, platform-specific, and not in git; every remote-capable IDE (VS Code Remote, Zed, JetBrains Gateway) runs LSP on the remote. Pre-recorded in the scaffolding spec. | 2026-06-10 |
| **Disk-backed document model**: `didOpen` / `didChange` are fed from on-disk file content via the worktree watcher, not from live editor buffers; sync is full-text (`TextDocumentSyncKind::Full`) | Constraint-determined: third-party agents and editors (Neovim) are black boxes (`CLAUDE.md`, `architecture.md` "agent-agnostic"), and v1's LSP layer owns no buffers. Reactive-to-filesystem *is* the architecture ("filesystem events … trigger … LSP diagnostics"). Full-text sync follows because there are no incremental editor deltas to send — the daemon only ever has the whole new file from disk. **Forward-note**: once rift's own editor lands (`architecture.md` "The GUI is the editor"), the document source-of-truth shifts disk→rift-buffer — the editor will send `didChange` from the live buffer. v1's disk path is the current source, not a forever-invariant; the data layer must not bake in a permanent "LSP only reads disk" assumption. | 2026-06-10 |
| **v1 `didOpen` breadth is the observed / changed file set** (files created or modified in the session), not an eager whole-tree open — with the accepted consequence that an imported-but-untouched file may carry no diagnostics on servers that only diagnose *open* documents (tsserver, pyright), while workspace-wide servers (rust-analyzer) surface project errors regardless | Constraint-determined: minimal scope (`CLAUDE.md` "no premature abstraction"), and the north-star signal is precisely the *agent-edited* set ("type errors surfacing as the agent edits"). The exact per-server-class `didOpen` mechanics — when an open-doc server must be sent a file before it will publish for it — are pinned by the rust-analyzer spike and the first document-sync issue; broader eager workspace open is a later refinement, not a v1 need. | 2026-06-10 |
| **LSP client crate is `async-lsp` — the spike-confirmed primary candidate**, with forking `helix-lsp` as the pre-vetted fallback | Precedent-backed *direction* (not a closed decision, unlike the other rows): `prior-art.md` Category 7 recommends `async-lsp` (active, client-first, Tower middleware, MIT/Apache); validated by `async-lsp` + `helix-lsp` + Lapce (patterns #7 three-task transport, #8 multi-server registry); pre-recorded in the scaffolding spec. The one-day rust-analyzer spike (Constraints, Verification) is the **commitment gate** and can still fall back to a `helix-lsp` fork. That fallback is license/musl-pre-vetted — `helix-lsp` is MPL-2.0 (GPL-3.0-compatible per `prior-art.md`) and pure-Rust — so taking it does **not** reopen the protocol design, the disk-backed model, or the data-layer cut; only the `cargo deny` Verification line would then carry MPL-2.0 in the tree. | 2026-06-10 |
| **Lazy, per-language server lifecycle** started at the worktree root and reused; **multi-server-per-language via a `Registry`** | Precedent-decided: Lapce `start_lsp(…, document_selector)`, Helix `Registry` (`HashMap<Name, Vec<Id>>`), Zed `LspStore` (`prior-art.md` pattern #8). Pre-recorded in the scaffolding spec ("lazily started per `DocumentSelector` … multi-server-per-document via a registry"). | 2026-06-10 |
| **`crates/protocol` carries rift's own diagnostic types; the daemon translates from `lsp-types`** | Keeps the shared protocol dependency-light (no heavy `lsp-types` across the boundary) and serialization-agnostic (`protocol.md`: may migrate to MessagePack). Mirrors how worktree / git messages are rift types, not library types. | 2026-06-10 |
| **Diagnostics carry full-set-per-file replace semantics, keyed by server id** for aggregation | LSP `publishDiagnostics` semantics: each notification is the complete current set for a URI, replacing the previous one. Server-id keying lets a linter + type-checker coexist (pattern #8) without one server clearing the other's set. | 2026-06-10 |
| **LSP logic lives in a new `crates/lsp/` daemon-side library** (`gpui`-free, musl-clean) | Mirrors the `crates/explorer` precedent: a daemon library is independently testable via `cargo test --workspace --exclude rift-app` (the binary is not), and the musl / `gpui`-free guarantee is enforced at the crate boundary. `prior-art.md` pattern #10 ("one crate per subsystem"). | 2026-06-10 |
| **Data-layer-only**: the spec ends when the client holds a live per-file diagnostics model; the rendered panel and decorations are a separate sub-spec | Inherited from the file-tree and git-status review-gate decisions (same rationale: headless-verifiable, small PRs, parallel-dev fit). The panel sub-spec renders the tree, git status, and diagnostics together. | 2026-06-10 |
| **Single watched root**, top-level only; multi-root deferred | No premature abstraction; mirrors the file-tree / git-status single-root cut. | 2026-06-10 |
| **v1 LSP feature scope = diagnostics (server-push); navigation is a committed sibling sub-spec on the editor track** | Resolved at the review gate (`AskUserQuestion`: rift's LSP destination is diagnostics **and** navigation). Sequencing splits them: diagnostics need only the worktree watcher and are headless-verifiable now; navigation (pull: hover / go-to-definition / references) has a single consumer — rift's own editor (`vision.md` / `architecture.md` editor pivot, #153) — which has no spec yet, so its request/response plumbing is deferred to a sub-spec that sequences with the editor. Keeps this spec's clean push-only protocol and the small-PR / no-premature-abstraction discipline. | 2026-06-11 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under a Phase 3 sub-milestone. Created once this spec is `READY` and merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created at `READY` (LSP sub-milestone under Phase 3)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo deny check licenses` passes with the full `async-lsp` + `lsp-types` transitive tree resolved
- [ ] `cargo build --release -p rift-daemon --target x86_64-unknown-linux-musl` still produces a static binary with `crates/lsp` (`async-lsp` + `lsp-types`) linked
- [ ] **rust-analyzer spike**: a real rust-analyzer round-trip over `async-lsp` produces diagnostics for a fixture Rust file end-to-end — the commitment gate before the rest of the milestone proceeds
- [ ] Integration test against a fixture project driven by a **stub LSP server** (a small test binary speaking minimal LSP that emits canned `publishDiagnostics`): modifying a file to introduce an error yields a `Diagnostics` update carrying that error for the file; rewriting the file to fix it yields a follow-up update clearing it (empty set); the client model converges
- [ ] A second stub server attached to the same language aggregates: both servers' diagnostics appear for the file, and one server clearing its set leaves the other's intact (server-id keying)
- [ ] A write to an ignored path (`target/foo`, a `.gitignore`d path) opens no document and emits no `Diagnostics`
- [ ] A `grep` confirms no `Arc<Mutex<State>>` in the daemon crate and that `crates/lsp` pulls no `gpui` / `gpui-component` (inspect its resolved dependency tree)

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `async-lsp` ergonomics / latency below bar, or its musl cross-compile unproven in this toolchain | The one-day rust-analyzer spike is the go/no-go gate; the `daemon-musl` CI job builds the daemon with `async-lsp` linked. If either fails, fall back to forking `helix-lsp` (MPL-2.0) — recorded as the prior-decision fallback. |
| `didOpen` breadth: opening every matching file in a huge repo floods the server, and which files get diagnostics varies per server class (rust-analyzer publishes workspace-wide; tsserver / pyright mostly for open docs) | Decided in Prior decisions (v1 = the observed / changed set, with the imported-but-untouched consequence accepted); the spike pins the per-server-class mechanics. Bound and log; broader eager open is a later refinement. |
| A language server is not installed on the remote, or is the wrong version | Resolve on `$PATH`; if absent, log once and skip that language (no diagnostics) rather than erroring — never fatal, agent-agnostic. Pin known-good `lsp-types` against current servers. |
| `lsp-types` is stalled and may drift from newer server protocol versions | Pin a known-good version; watch the `tower-lsp-community/ls-types` migration (`prior-art.md` caveat 6) as the future path. |
| A language server crashes or restart-storms | Supervise: on server exit, log and lazily restart on the next matching change with backoff; never panic the daemon (mirrors `serve_uds`'s accept-error discipline). |
| `Diagnostics` arrive for a path the client has not yet added (race vs. the worktree snapshot) | Push-as-source-of-truth ordering: the client tolerates diagnostics for an unknown path (buffer or drop until the entry exists, since the next authoritative update reconciles). Define the exact reconciliation in the first protocol issue (mirrors the git-status race risk). |
| Full-text `didChange` cost on large files on every save | Bounded by save frequency; full sync is mandated by the disk-backed model (no editor deltas exist). Acceptable for v1; revisit only if profiling shows a problem. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-06-10: Spec created from `/plan lsp`. Recorded as precedent / constraint-decided: daemon-side LSP, the disk-backed full-text document model driven by the worktree watcher, `async-lsp` (spike-gated, `helix-lsp` fallback), the lazy per-language `Registry` with multi-server-per-language, the protocol owning rift's own diagnostic types with full-set-replace + server-id aggregation, the new `crates/lsp` library, data-layer-only, and single-root. The one open decision — v1 feature scope (diagnostics-only vs. diagnostics + interactive navigation) — flagged for the review gate.
- 2026-06-11: Review gate resolved the open scope decision. This spec is **diagnostics (server-push)**; **navigation (pull: hover / go-to-definition / references) is a committed sibling sub-spec sequenced with the editor track** — its only consumer is rift's own editor (`vision.md` / `architecture.md` editor pivot, #153), which has no spec yet, so building the request/response plumbing now would be premature. The disk-backed document model gains a forward-note: source-of-truth shifts disk→rift-buffer once the editor lands; v1's disk path must not be baked in as a forever-invariant. Spec flipped `DRAFT → READY`.
