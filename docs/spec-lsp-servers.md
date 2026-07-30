# Spec: Multi-language LSP servers

> Created: 2026-07-27

Populate the daemon's built-in language-server registry beyond the single
rust-analyzer row so rift streams diagnostics for the common project languages
(Python, TypeScript/JavaScript, Go, C/C++), consuming whatever servers are on the
remote host's `$PATH` and degrading gracefully when one is absent. Roadmap Phase 48.

## Outcome

- [ ] Opening a file of each shipped language on a remote host where that language's server is installed streams its diagnostics into rift's editor/problems surfaces, exactly as rust does today.
- [ ] Each shipped `ServerSpec` row's `extensions` are all covered by `language_id_for` (`crates/lsp/src/document.rs`), so a matched file both spawns the server and opens the document.
- [ ] Opening a file whose server is NOT installed on the remote `$PATH` never crashes the daemon and is surfaced as a distinct `NotInstalled` status (informational, not an alarming crash), leaving other languages' servers unaffected.
- [ ] The selector's unit tests assert every shipped row matches its extensions (and the multi-server-per-language case still holds).

## Scope

### In scope

- Adding `ServerSpec` rows to `BUILTIN_SERVERS` (`crates/lsp/src/selector.rs`) for the shipped language set (final set resolved at the gate; recommended: Python, TypeScript/JavaScript, Go, C/C++), each with the correct `binary`, `args`, and `extensions`.
- Confirming (and, only if a chosen extension is missing, extending) `language_id_for` coverage for every shipped row's extensions.
- Unit tests in `selector.rs` for the new rows.
- Adding a distinct `NotInstalled` server state: mapping the missing-binary spawn error to `NotInstalled` (daemon), a new `protocol` `LspServerState` variant (`PROTOCOL_VERSION` bump — a deliberate, reviewed API change), and distinct informational app rendering (not the alarming crash colour).

### Out of scope

- Installing language servers — rift consumes servers already on the remote `$PATH`, never installs them (`docs/archive/spec-daemon-lsp.md`).
- A user-configurable / per-project server table, glob or language/scheme `DocumentSelector` patterns, or `initializationOptions` per server — deferred behind a real need (constitution: no premature abstraction). The current extension-based selector and default `initialize` are the seam these would slot into later.
- LSP features beyond diagnostics (formatting, code actions, rename, completion) — those are separate phases; navigation (hover/def/refs) is already generic and needs no work here.
- Bundling or version-pinning servers.

## Constraints

- Servers are resolved on the daemon's `$PATH` at spawn time and never installed (`crates/lsp/src/selector.rs` doc; `docs/archive/spec-daemon-lsp.md`).
- The lifecycle is already generic and needs NO new code: lazy spawn is driven by `DocumentSelector::matching(path)` over `BUILTIN_SERVERS` (`registry.rs:214-217`) — adding a row auto-enables the server; the `Registry` runs N servers concurrently keyed per language (`by_language`, `registry.rs:93-95`) with diagnostics aggregated per `(path, server-id)`; a missing binary is already non-fatal (warn-once + per-binary backoff + a `Crashed` lifecycle event, `registry.rs:251-254`, tested `test_missing_binary_is_skipped_and_never_fatal`); `initialize` sends generic capabilities with no rust-analyzer-specific options (`server.rs:330-369`); offset encoding is negotiated (`nav.rs:156-160`); nav is capability-gated (`lsp.rs:632`).
- The wire `languageId` comes from `language_id_for(path)` (`crates/lsp/src/document.rs:366`), NOT from `ServerSpec.language` — two separate tables that must not drift: a row whose extension is absent from `language_id_for` spawns a server but the document never opens (`document.rs:270` returns `None`), so no diagnostics flow. `language_id_for` today covers rs/ts/tsx/js/jsx/mjs/cjs/py/pyi/go/c/h/cc/cpp/cxx/hpp/hh — the recommended four servers need no change.
- One row per server binary with an `extensions` list suffices (e.g. one `typescript-language-server` row for ts/tsx/js/jsx/mjs/cjs); per-file `languageId` is still correctly differentiated by `language_id_for`. No separate row per languageId is needed.
- Agent-agnostic and constitution-clean: no new dependency (data rows only); LSP diagnostics are already an agent-agnostic filesystem-derived signal.

## Prior art

- [QA-seeded phases — prior-art index (Phases 48–56)](prior-art.md#qa-seeded-phases--prior-art-index-phases-4856) — the Phase 48 row: reuse the existing registry, servers from the remote `$PATH`, candidates pyright / typescript-language-server / gopls / clangd; the multi-server-per-doc (linter + type-checker) shape is the Helix `Registry` pattern.
- [Category 7: LSP Client Implementations](prior-art.md#category-7-lsp-client-implementations) — helix `Registry` (`HashMap<LanguageServerName, Vec<LanguageServerId>>`) and lapce `DocumentSelector` per-language routing: the multi-server-per-document design rift already implements.
- [Architecture patterns to adopt](prior-art.md#architecture-patterns-to-adopt) #8 "Multi-server-per-document via Registry" — validates that linter + type-checker on one buffer is the intended shape, already realised in `crates/lsp/src/registry.rs`.

## Human prerequisites

For the milestone-QA gate, the developer must have the relevant language servers installed on the **remote** host so diagnostics can actually be observed, and at least one small source file per language to open. (rift never installs these — QA verifies against the remote's real `$PATH`.)

- [ ] `pyright-langserver` on the remote `$PATH` (Python) — or the alternative chosen at the gate
- [ ] `typescript-language-server` on the remote `$PATH` (TypeScript/JavaScript)
- [ ] `gopls` on the remote `$PATH` (Go)
- [ ] `clangd` on the remote `$PATH` (C/C++)
- [ ] One or more remote source files per shipped language to open during QA; plus one language whose server is deliberately NOT installed, to verify graceful degradation

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The spawn/lifecycle machinery needs no new code (spawn, multi-server registry, missing-binary non-fatality, generic `initialize`, encoding negotiation, capability-gated nav are already generic, verified against develop); the phase adds data rows PLUS the `NotInstalled` status below | Keeps the bulk of the phase pure-data; the one code change is the accepted `NotInstalled` state | 2026-07-27 |
| One `ServerSpec` row per server binary with an `extensions` list; NOT one row per `languageId` | The selector matches by extension; the wire `languageId` is differentiated separately by `language_id_for` | 2026-07-27 |
| Every shipped row's `extensions` must be present in `language_id_for`; extend it only if a chosen extension is missing | The two tables must not drift, or the server spawns but the document never opens (`document.rs:270`) | 2026-07-27 |
| Keep the built-in table hardcoded; no user/per-project server config this phase | Constitution: no premature abstraction; the `DocumentSelector` type is already the seam a richer config would slot into | 2026-07-27 |
| Recommended binaries/args: `pyright-langserver --stdio`, `typescript-language-server --stdio`, `gopls` (no args), `clangd` (no args) | Each server's documented stdio launch; confirmed at the gate | 2026-07-27 |
| Ship all four servers: `pyright-langserver --stdio` (python: py, pyi), `typescript-language-server --stdio` (ts, tsx, js, jsx, mjs, cjs), `gopls` (go), `clangd` (c, h, cc, cpp, cxx, hpp, hh) | Accepted at the gate; covers the common project languages; all extensions already in `language_id_for` | 2026-07-28 |
| Add a distinct `NotInstalled` `LspServerState`: the daemon maps the missing-binary `LspError::Spawn` to `NotInstalled` (not `Crashed`); `protocol` gains the variant (`PROTOCOL_VERSION` bump); the app renders it distinctly (informational, not alarming red) | Accepted at the gate; with several languages 'not installed' is the common case, and collapsing it into `Crashed` misleads | 2026-07-28 |

## Tracking

- Milestone: created at the spec-acceptance gate
- Issues: created from this spec once merged (one per implementable step)

## Verification

- [ ] `just ci` equivalent green (fmt + clippy `-D warnings` + tests, workspace excl. `rift-app`); locally the `lsp`/`daemon` crates compile warm-target clean.
- [ ] Unit: `selector.rs` tests assert each shipped row matches its declared extensions and unknown extensions still match nothing.
- [ ] QA (remote, servers installed): opening a `.py` / `.ts` / `.tsx` / `.go` / `.c` / `.cpp` file with real errors streams diagnostics into rift within the same latency as rust today; fixing the error clears them.
- [ ] QA (graceful degradation): opening a file whose server is NOT installed on the remote produces no daemon crash, no stuck state, and the distinct `NotInstalled` status (not the crash colour) — other languages' servers keep working.
- [ ] QA: two languages open simultaneously each get their own diagnostics independently (multi-server concurrency holds).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| A shipped row's extension is missing from `language_id_for` → server spawns but no diagnostics | Outcome + Prior-decision require confirming coverage; the recommended four are already covered — verify in the implementing PR |
| A server needs `initializationOptions` to behave (e.g. some Python setups) | Out of scope for v1 (default `initialize` is generic and works for the recommended servers); revisit per-server options behind a real need |
| Every uninstalled server shows as "Crashed" red, reading as broken | Resolved: the accepted `NotInstalled` state renders missing servers distinctly (informational), separate from a real crash |
| clangd without `compile_commands.json` gives limited diagnostics | Acceptable — still non-fatal; full C/C++ index config is the user's project concern, not rift's |

## Decision log

- 2026-07-27: Scoped from a verified develop-code read — the LSP lifecycle is fully generic, so this phase is data rows plus the `language_id_for` coupling discipline, with two genuinely-open decisions (language set, `NotInstalled` state) carried to the gate.
- 2026-07-28: Spec-acceptance gate — accepted. Ship all four servers (pyright / typescript-language-server / gopls / clangd). Introduce a distinct `NotInstalled` `LspServerState` (protocol variant + `PROTOCOL_VERSION` bump + informational app rendering), rather than collapsing a missing binary into `Crashed`. Spec review (PR #909) returned APPROVE with only non-blocking nits, addressed.
