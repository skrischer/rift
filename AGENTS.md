# rift — AGENTS.md

## Project context

Open-source, agent-centric IDE built in Rust. Wraps tmux with a native GPU-accelerated GUI (GPUI) to provide visual feedback for terminal-based coding agents. Current state: single-window terminal connected via SSH using tmux control mode (`-CC`) with event-driven notification processing. Target architecture (Phase 3+): GPUI frontend with a remote daemon on Linux hosts, connected via SSH.

Coding agents (Claude Code, Codex, OpenCode, Gemini CLI) run completely unmodified. The IDE reacts to their side effects — file changes, terminal output, git state — never to agent internals. Agents are interchangeable black boxes. If you're writing code that detects or special-cases a specific agent, stop.

Read `VISION.md` for the why. Read `ARCHITECTURE.md` for the how.

## Tech stack

- **Language:** Rust (2021 edition)
- **Async runtime:** Tokio
- **GUI framework:** GPUI 0.2.2
- **Build target (daemon):** `x86_64-unknown-linux-musl`
- **Build target (app):** Linux/X11 (GPUI native), macOS and Windows deferred

## Repository layout

Cargo workspace. Each crate has a single responsibility:

- `crates/app/` — GPUI application binary.
- `crates/ssh/` — SSH connection and PTY stream.
- `crates/terminal/` — GPUI terminal widget. Wraps `alacritty_terminal` + `termy_terminal_ui`.
- `crates/daemon/` — Remote daemon binary (Phase 3+).
- `crates/tmux-core/` — tmux control mode state (Phase 3+, currently using termy's `TmuxClient` directly).
- `crates/explorer/` — File watching, git status, file sync (Phase 3+).
- `crates/protocol/` — Shared message types. Serializable with serde (Phase 3+).
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

## Commits

Conventional Commits. Scope matches crate name. Imperative mood, lowercase, no period.

```
feat(tmux-core): add window layout change event parsing
fix(terminal): handle malformed UTF-8 in cell output
refactor(explorer): extract git status into dedicated module
chore: update alacritty_terminal to 0.24
```

## Testing

Unit tests in `#[cfg(test)] mod tests {}` in the same file. Integration tests in `crates/*/tests/`. Name pattern: `test_<what>_<condition>_<expected>`. Prefer real fixtures over mocks. Every public API function needs at least one test. Parsers need tests for valid and malformed input.

## Open source

Always free, always open source. No telemetry, no analytics. License headers on every source file. Dependencies must pass `cargo deny check licenses`. Code must be understandable to outside contributors — clear module boundaries, documented public APIs.
