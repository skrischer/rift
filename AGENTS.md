# rift — AGENTS.md

## Project context

Open-source, agent-centric IDE built in Rust. Wraps tmux with a native GPU-accelerated GUI (GPUI) to provide visual feedback for terminal-based coding agents. Current state: single-window terminal connected via SSH using tmux control mode (`-CC`) with event-driven notification processing. Target architecture (Phase 3+): GPUI frontend with a remote daemon on Linux hosts, connected via SSH.

Coding agents (Claude Code, Codex, OpenCode, Gemini CLI) run completely unmodified. The IDE reacts to their side effects — file changes, terminal output, git state — never to agent internals. Agents are interchangeable black boxes. If you're writing code that detects or special-cases a specific agent, stop.

Read `docs/vision.md` for the why. Read `docs/architecture.md` for the how.

## Tech stack

- **Language:** Rust (2021 edition)
- **Async runtime:** Tokio
- **GUI framework:** GPUI 0.2.2
- **Build target (daemon):** `x86_64-unknown-linux-musl`
- **Build target (app):** Windows (`x86_64-pc-windows-gnu`, cross-compiled from WSL via MinGW) and Linux/X11 (GPUI native); macOS deferred. Primary dev loop runs the GPU app on the Windows host (`just dev-windows`).

## Repository layout

Cargo workspace. Each crate has a single responsibility:

- `crates/app/` — GPUI application binary.
- `crates/ssh/` — SSH connection and PTY stream.
- `crates/terminal/` — GPUI terminal widget. Wraps `alacritty_terminal` + `termy_terminal_ui`.
- `crates/daemon/` — Remote daemon binary. Runs on the remote host, manages file watching, git status, and language servers.
- `crates/tmux-core/` — tmux control mode state (currently using termy's `TmuxClient` directly).
- `crates/explorer/` — File watching, git status — library used by daemon (Phase 3+).
- `crates/protocol/` — Shared message types. Serializable with serde.
- `crates/plugin-api/` — Plugin trait for pane awareness (Phase 3+).
- `plugins/` — Optional pane awareness plugins (Phase 3+).

## Commands

```bash
cargo build --workspace                                          # compile all
cargo clippy --workspace -- -D warnings                          # lint (zero warnings policy)
cargo fmt --all                                                  # format
cargo test --workspace                                           # test all
cargo run -p rift-app                                            # run GPUI app in dev mode
cargo build --release -p daemon --target x86_64-unknown-linux-musl  # daemon release build (Phase 3+)
```

## Architectural rules

**Agent-agnostic by design.** The daemon sees PTY byte streams and filesystem events. It has no concept of which agent runs in a pane. All IDE features derive from these two universal signals. A new CLI agent shipping tomorrow must work with zero code changes.

**Crate boundaries are contracts.** Each crate exposes a public API through `lib.rs`. Internal modules are private. Import through the public API only. `crates/protocol/` is the shared language — both sides depend on it but never on each other.

**Plugins extend, core stays agnostic.** Process-specific pane awareness (detecting agent states, dev server status) lives in plugins, never in the daemon core. The daemon depends on `crates/plugin-api/` (the trait), never on plugin implementations. Plugins are optional cargo features.

**No premature abstraction.** Write concrete implementations first. Extract traits only when two or more implementations share a pattern. "We might need this later" is not a reason to add a trait.

**State flows through channels.** The daemon maintains a single `State` struct. Use `tokio::sync::watch` or `broadcast` channels to notify consumers. Avoid `Arc<Mutex<State>>` where channels work.

**Async for I/O, blocking for CPU.** All I/O is async via Tokio. CPU-bound work (VTE parsing, diff computation) runs on `tokio::task::spawn_blocking`.

## What to avoid

- **No agent-specific code.** No detection of which CLI agent runs. No parsing of agent output formats.
- **No proprietary dependencies.** MIT, Apache-2.0, ISC, BSD, GPL-3.0 compatible only. Run `cargo deny check licenses` in CI.
- **No `.unwrap()` in library code.** Use `thiserror` in libs, `anyhow` in binaries. `.expect("reason")` only for true invariants.
- **No `clone()` to satisfy the borrow checker.** Restructure ownership instead.
- **No `String` where `&str` suffices.** Be intentional about ownership at API boundaries.
- **No `todo!()` or `unimplemented!()` in merged code.**
- **No feature flags for unimplemented features.**
- **No large PRs.** Split at ~400 lines. Exception: initial scaffolding.

## Branching

- **`main`** — protected, production-ready. Only receives merges from `develop` via PR.
- **`develop`** — protected, integration branch. All feature work merges here first via PR.
- **Feature branches** — branch off `develop`, merge back into `develop`. Naming: `feat/<scope>`, `fix/<scope>`, `chore/<scope>`.

**Hard rules:**
- Always `git checkout develop && git pull` before creating a feature branch.
- Always target `develop` as base branch when creating a PR (`gh pr create --base develop`).
- Never target `main` for feature PRs. `main` only receives merges from `develop`.
- Never push directly to `main` or `develop`. Always use pull requests.
- Delete feature branches after merge.

## Parallel development (worktrees)

The GPU app (`rift-app`) is the only expensive, non-parallelizable build (pulls skia/wgpu — ~20 GB of debug artifacts). Topology that keeps a single heavy `target/`:

- **Main checkout = the one GPU station.** Stays on `develop`, runs `just dev-watch`. The only place `rift-app` is built and visually previewed.
- **Agents work headless in worktrees.** They verify with `just lint` / `just test` / `cargo build --workspace --exclude rift-app` — no GPU build, so their `target/` stays small.
- **Visual review is a gate *before* merge**, not after: the agent first commits its work in the worktree (worktrees share only committed objects, not the working tree, so uncommitted changes are invisible to the main checkout). Then, on the GPU station, `git checkout --detach <branch>` — a plain `git checkout <branch>` fails because the branch is already checked out in the worktree, so use `--detach` to ride the commit while reusing the station's heavy `target/`. Let `dev-watch` rebuild incrementally, then `git checkout develop` to return. Never blind-merge GPU changes into `develop`.

Worktrees live in a sibling container `../rift-worktrees/<branch-with-slashes-as-dashes>` (outside the repo tree, so `rg`/`cargo`/watchers don't traverse them; own `target/` per worktree). Use `just agent-worktree <branch>` to create and `just agent-worktree-rm <branch>` to remove.

### Dogfooding channels

Two side-by-side instances share tmux session `rift` (one daemon, mirrored views) — see `docs/spec-dogfooding-channels.md`:

- **Stable** — the daily driver. `just promote` (HEAD must be `develop`, ff-synced to `origin/develop`) builds the optimized `stable` profile, pins the exe at `%LOCALAPPDATA%\rift\rift-stable.exe` (outside `target/`, so `cargo clean` cannot touch it; own image name, so the dev loop's taskkill cannot either) and relaunches it detached. `just stable` relaunches without rebuilding (e.g. after a reboot).
- **Dev** — `just dev-windows[-watch]`, the acceptance/visual gate. Mirrors session `rift` by default; `RIFT_SESSION=rift-dev just dev-windows-watch` isolates destructive tests on a throwaway session.

One-time Windows launcher setup (manual, no recipe — it never recurs): create a Desktop shortcut to `%LOCALAPPDATA%\rift\rift-stable.exe` and pin it to the taskbar by hand. No env setup is needed: `promote` bakes the SSH key path (justfile `windows_ssh_key`) into the stable exe as a compile-time default (runtime `RIFT_SSH_KEY` still overrides); host/user/port/session match the app defaults, and the daemon is skipped while `RIFT_DAEMON_BINARY` is unset.

Stable diagnostics: the windowed build has no console — it logs to `%LOCALAPPDATA%\rift\rift-stable.log` (fresh file per start, panics included). If a launch dies silently, read that file.

Optional mirror polish: `set -g window-size largest` in the host's `~/.tmux.conf`, so a dev restart's transient 80x24 attach does not reflow stable's view.

## Commits

Conventional Commits. Scope matches crate name. Imperative mood, lowercase, no period.

```
feat(tmux-core): add window layout change event parsing
fix(terminal): handle malformed UTF-8 in cell output
refactor(explorer): extract git status into dedicated module
chore: update alacritty_terminal to 0.24
```

## Planning handover

Planning lives in `docs/` as SDD specs (`docs/spec-template.md`). The chain is **design-doc -> issue -> PR**, mechanically enforced — see `docs/handover-conventions.md` for the full rules.

- Spec (`docs/spec-*.md`) owns the **design**: outcome, scope, constraints, prior decisions, verification.
- A GitHub **milestone** groups a phase; **issues** own the step decomposition and progress. The step list lives only as issues, never as a task breakdown inside the spec.
- A `feat:`/`fix:` PR may only merge if it closes an issue that references an existing spec (`planning-gate` required check on `develop`). `chore:/docs:/refactor:/test:/ci:/build:/perf:` are exempt.

**Before starting work:**
1. Read `docs/handover-conventions.md` and the relevant `READY` `docs/spec-*.md` (never a `DRAFT`)
2. Pick the issue for the step; branch `feat/<scope>` or `fix/<scope>` off `develop`

**After completing work:**
1. Open a PR that closes the issue (`Closes #N`); the milestone closes when its issues do
2. When the spec's verification is fully met, set status to `COMPLETED` with date and move it to `archive/`
3. Add decisions made during implementation to the spec's decision log
4. Update `docs/roadmap.md` with the current phase status

**If blocked:**
1. Set spec status to `BLOCKED` with reason in the spec header
2. Comment the blocker on the affected issue

## Claude Code

When compacting, always preserve: the list of modified files, active crate context, and any failing test output.

For detailed patterns (error handling, state management, async discipline), read `docs/patterns.md` before implementing — don't guess from memory.

For protocol message types, read `docs/protocol.md` before adding or modifying messages.

1. Check architecture docs before adding dependencies or new crates.
2. Run `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` before considering a task done.
3. Don't refactor code you weren't asked to touch. Note opportunities in a comment if significant.
4. When in doubt about a design decision, state your reasoning — don't silently pick an approach.
5. Respect crate boundaries. Adding to `protocol` is a deliberate API change.

## Testing

Unit tests in `#[cfg(test)] mod tests {}` in the same file. Integration tests in `crates/*/tests/`. Name pattern: `test_<what>_<condition>_<expected>`. Prefer real fixtures over mocks. Every public API function needs at least one test. Parsers need tests for valid and malformed input.

## Open source

Always free, always open source. No telemetry, no analytics. License headers on every source file. Dependencies must pass `cargo deny check licenses`. Code must be understandable to outside contributors — clear module boundaries, documented public APIs.
