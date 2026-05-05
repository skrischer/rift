# rift — AGENTS.md

## Project context

Open-source, agent-centric IDE built in Rust. Wraps tmux with a native GUI (Tauri) to provide visual feedback for terminal-based coding agents. Split architecture: Tauri frontend on Windows, daemon on remote Linux hosts, connected via SSH.

Coding agents (Claude Code, Codex, OpenCode, Gemini CLI) run completely unmodified. The IDE reacts to their side effects — file changes, terminal output, git state — never to agent internals. Agents are interchangeable black boxes. If you're writing code that detects or special-cases a specific agent, stop.

Read `VISION.md` for the why. Read `ARCHITECTURE.md` for the how.

## Tech stack

- **Language:** Rust (2021 edition), TypeScript (Tauri webview)
- **Async runtime:** Tokio
- **GUI framework:** Tauri v2
- **Build target (daemon):** `x86_64-unknown-linux-musl`
- **Build target (app):** `x86_64-pc-windows-msvc`

## Repository layout

Cargo workspace. Each crate has a single responsibility:

- `crates/daemon/` — Remote daemon binary. Depends on all other crates.
- `crates/tmux-core/` — tmux control mode parser and session state tree. Pure logic, no I/O.
- `crates/terminal/` — VTE parsing and cell grid. Wraps `alacritty_terminal`.
- `crates/explorer/` — File watching, git status, file sync.
- `crates/protocol/` — Shared message types. Serializable with serde. Both sides depend on this, never on each other.
- `crates/plugin-api/` — Plugin trait for pane awareness. Daemon depends on this, never on plugin implementations.
- `plugins/` — Optional pane awareness plugins (e.g. claude-code, codex, devserver). Compiled as cargo features.
- `app/` — Tauri frontend (src-tauri/ for Rust backend, src/ for TypeScript).

## Commands

```bash
cargo build --workspace                                          # compile all
cargo clippy --workspace -- -D warnings                          # lint (zero warnings policy)
cargo fmt --all                                                  # format
cargo test --workspace                                           # test all
cargo build --release -p daemon --target x86_64-unknown-linux-musl  # daemon release build
cargo run -p daemon -- --port 9500                               # run daemon locally
cd app && cargo tauri dev                                        # run Tauri app in dev mode
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
- **No proprietary dependencies.** MIT, Apache-2.0, ISC, BSD compatible only. Run `cargo deny check licenses` in CI.
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

Never push directly to `main` or `develop`. Always use pull requests. Delete feature branches after merge.

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
