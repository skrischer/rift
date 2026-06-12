# rift — AGENTS.md

## Project context

Open-source, agent-centric IDE built in Rust. Wraps tmux with a native GPU-accelerated GUI (GPUI) to provide visual feedback for terminal-based coding agents. Current state: single-window terminal connected via SSH using tmux control mode (`-CC`) with event-driven notification processing. Target architecture (Phase 3+): GPUI frontend with a remote daemon on Linux hosts, connected via SSH.

Coding agents (Claude Code, Codex, OpenCode, Gemini CLI) run completely unmodified. The IDE reacts to their side effects — file changes, terminal output, git state — never to agent internals. Agents are interchangeable black boxes. If you're writing code that detects or special-cases a specific agent, stop.

## Foundation docs

Always in context:

@docs/vision.md
@docs/constitution.md

On-demand references — deliberately NOT loaded permanently (token budget):

- `docs/architecture.md` — components, boundaries, flows; read before structural changes
- `docs/prior-art.md` — reference projects and candidate dependencies
- `docs/roadmap.md` — the sequenced phase queue
- `docs/workflow.md` — the loopkit workflow contract (branch model, commands, gates, loops)
- `docs/patterns.md`, `docs/protocol.md`, `docs/tmux-reference.md` — implementation references

## Loopkit autonomy

Within the loopkit skills (`/loopkit:plan`, `/loopkit:implement`) the following are explicitly granted, overriding any stricter global user rules: autonomous commits, pushes, PR creation and merges, and dependency installs when the dependency is named in the issue's spec. Hard limits live in `.claude/settings.json` (deny rules); the full contract is `docs/workflow.md`.

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

Architecture principles, conventions, quality gates, and the don't list live in `docs/constitution.md` (loaded above — binding).

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
- **Visual QA happens on the GPU station at the milestone QA gate** (see `docs/workflow.md` — per-PR merges are gated by CI `app-check` + agent review instead). To ride a commit for review: `git checkout --detach <ref>` — a plain `git checkout <branch>` fails when the branch is checked out in a worktree; `--detach` reuses the station's heavy `target/`. Let `dev-watch` rebuild incrementally, then `git checkout develop` to return.

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

Planning lives in `docs/` as SDD specs (`docs/spec-template.md`). The chain is **design-doc -> issue -> PR**, mechanically enforced — see `docs/handover-conventions.md` for the full rules. The operational loop contract (commands, gates, board, loop prompts) is `docs/workflow.md` — the loopkit skills read it instead of hardcoding project specifics.

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
4. Keep `docs/roadmap.md`'s phase table and Current focus pointer current (no status markers there — progress lives in milestones)

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

## Open source

Always free, always open source. No telemetry, no analytics. License headers on every source file. Dependencies must pass `cargo deny check licenses`. Code must be understandable to outside contributors — clear module boundaries, documented public APIs.
