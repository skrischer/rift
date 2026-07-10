# Constitution

> Normative and binding. Every principle is verifiable. CLAUDE.md loads this
> permanently; operational detail (commands, worktree topology) stays in CLAUDE.md.

## Tech stack

| Area | Choice | Rationale |
| ---- | ------ | --------- |
| Language | Rust 2021 | Native performance, no Electron; the GPUI ecosystem |
| GUI | GPUI 0.2.2 (git-pinned) | GPU-accelerated native rendering |
| UI components | gpui-component | Dock, virtualized lists, theming — don't rebuild primitives |
| Async runtime | Tokio | Async I/O for SSH, daemon, watchers |
| Terminal | alacritty_terminal + termy_terminal_ui | Proven VTE + GPUI grid rendering |
| Multiplexer | tmux control mode (`-CC`) | Structure as a protocol; engine not reinvented |
| SSH | russh | Pure-Rust async SSH; channel multiplexing replaces extra framing |
| LSP client | async-lsp | Active, client-first (helix-lsp fork as vetted fallback) |
| Git | gix | Pure Rust, musl-clean (git2/libgit2 ruled out by static musl) |
| Daemon target | x86_64-unknown-linux-musl | Static binary, auto-deployable to any Linux host |
| App targets | Windows (x86_64-pc-windows-gnu) + Linux/X11 | Primary dev loop on the Windows host; macOS deferred |
| License | GPL-3.0-or-later | Always free; deps must pass `cargo deny check licenses` |

## Architecture principles

- Agent-agnostic: no code path detects or special-cases a specific agent. All IDE
  features derive only from agent-agnostic host observables — PTY byte streams,
  filesystem events, and host resource state (`/proc`: CPU / memory / swap / load) —
  never from an agent's internals. Host resource state is host-global (the machine,
  not any pane); attributing it to a pane keys on the tmux pane's process, never on
  which agent runs there.
- Crate boundaries are contracts: public API through `lib.rs` only; `protocol` is
  the shared language — both sides depend on it, never on each other.
- Plugins extend, core stays agnostic: process-specific pane awareness lives in
  plugins; the daemon depends on `plugin-api` (the trait), never on implementations.
- No premature abstraction: extract a trait only at 2+ implementations.
- State flows through channels: single `State` struct + `watch`/`broadcast`;
  no `Arc<Mutex<State>>` where channels work.
- Async for I/O, `spawn_blocking` for CPU-bound work (VTE parsing, diffs).

## Conventions

- Conventional Commits, scope = crate name, imperative lowercase English.
- Branches `feat|fix|chore|docs/<scope>` off `develop`; PRs target `develop`;
  `main` only receives merges from `develop`.
- Tests: `#[cfg(test)]` in-file, integration tests in `crates/*/tests/`,
  named `test_<what>_<condition>_<expected>`. Every public API function has at
  least one test; parsers are tested with valid and malformed input. Prefer
  real fixtures over mocks.
- License declared once via the workspace `license` field (`license.workspace = true`); no per-file SPDX headers. Everything in English.

## Quality gates

- `cargo fmt --all --check`, `cargo clippy --workspace -- -D warnings` (zero
  warnings), `cargo test --workspace` — green before merge (CI-enforced).
- `planning-gate`: a `feat:`/`fix:` PR only merges if it closes an issue that
  references an existing spec.
- `cargo deny check licenses` passes.
- PRs split at ~400 lines (exception: initial scaffolding).
- Milestone QA gate: visual acceptance on the dev channel when a milestone's
  last issue closes (QA scenarios from the spec's Verification section).

## Don'ts

- No `.unwrap()` in library code — `thiserror` in libs, `anyhow` in binaries;
  `.expect("reason")` only for true invariants.
- No `clone()` to satisfy the borrow checker; no `String` where `&str` suffices.
- No `todo!()`/`unimplemented!()` in merged code; no feature flags for
  unimplemented features.
- No agent detection, no parsing of agent output formats.
- No telemetry, no analytics, no proprietary dependencies.
- No emojis in code or UI.

## Tech debt

| Deviation | Where | Plan |
| --------- | ----- | ---- |
| termy's `TmuxClient` used directly instead of own control-mode state | `crates/tmux-core` | Phase 3 transport swap (single-seam change, see architecture.md) |
| `lsp-types` upstream stalled | LSP track | Watch `tower-lsp-community/ls-types`; migrate when it stabilizes |
