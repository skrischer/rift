# rift

> Reactive IDE awareness for terminal coding agents. Open source. Remote-native.

## The problem

Terminal-based coding agents like Claude Code have fundamentally changed how software gets written. You describe intent, the agent writes code, runs tests, iterates — and you guide the process. It's the most productive way to build software today.

But you're flying blind.

The agent edits files you can't see. It introduces errors you won't notice until the next prompt. It commits changes you have to `git log` to understand. You watch raw text scroll by in a terminal and reconstruct what happened after the fact. For vibe coding — shipping fast, trusting the agent — this works. For agentic coding — directing multiple agents, reviewing in real-time, maintaining quality at scale — it doesn't.

## The gap

The problem isn't that IDEs lack AI features. Every editor has bolted on copilots, chat panels, inline suggestions. But they all make the same architectural mistake: **they put the editor at the center and treat the agent as a peripheral feature.**

The result is a mismatch. You're running Claude Code in a terminal pane that the IDE barely acknowledges. The file explorer doesn't react to what the agent is doing. Diagnostics don't update in real-time as the agent edits. Git changes aren't surfaced until you manually refresh. The IDE and the agent exist in parallel universes.

On the other side, a new wave of agent orchestrators has emerged — tools like Arbor, Superconductor, Claude Squad, and Nimbalyst that put terminal agents at the center. They solve the management problem: running multiple agents in parallel, isolating worktrees, reviewing diffs after the fact. But they stop at the terminal boundary. **None of them give you live IDE awareness while agents are working** — no reactive file explorer, no real-time diagnostics, no LSP integration, no ctrl+click navigation. They show you what agents did. They don't show you what agents are doing to your codebase right now.

The missing tool is not another terminal manager or another agent orchestrator. It's an IDE that wraps around terminal agents and provides **reactive code intelligence while they work** — diagnostics updating as files change, errors surfacing before the agent finishes, file structure reflecting reality in real-time.

## The solution

rift is an **agent-centric IDE** — a native GUI shell that wraps terminal-based coding agents with **reactive code intelligence**.

The core differentiator is not session management or worktree isolation — other tools do that. It's that rift gives you **live IDE awareness while agents work**: diagnostics updating as files change, type errors surfacing before the agent finishes its turn, the file explorer reflecting every modification in real-time, and full code navigation (go-to-definition, find references, hover) without leaving the terminal workflow.

The architecture: **tmux is the engine, the GUI is the cockpit.**

tmux is the process runtime — session management, pane multiplexing, persistent processes. The coding agents, dev servers, and build/test scripts all run in tmux panes: persistent, remote, multi-pane. The GUI is the cockpit — it adds what the terminal can't: real-time file explorer updates, live LSP diagnostics, visual git diffs, full code navigation, and a first-class editor for reading and changing the code, with native mouse interaction, context menus, and ctrl+click navigation.

When a coding agent edits a file, the file explorer lights up and the file open in the editor reloads under you — you watch it change live. When it introduces a type error, the diagnostics panel shows it immediately — not after the agent is done, not after you run the compiler, but as it happens. You jump to the definition, fix it in place, and the change is saved to the remote. When it commits, the git diff view surfaces what changed. When multiple agents run in parallel across worktrees, you see all of them at a glance.

The IDE doesn't compete with the agent — it amplifies it.

## What this is not

- **Not another terminal emulator.** Alacritty, WezTerm, and Kitty are excellent at rendering terminal output. This project uses terminal emulation as a building block, not as the product.
- **Not just another remote editor.** rift edits code — but the differentiator is never the editor surface itself, which Zed and VS Code Remote already do well. It is that rift's *process layer* is real tmux running vanilla CLI agents, dev servers, and scripts, with reactive IDE awareness wrapped around them. An editor with no agents in its engine is not what this is. Terminal-driven editing still works in any pane (Neovim, Helix, whatever you run) — rift adds a GUI editor, it does not forbid the terminal one.
- **Not another tmux replacement.** Zellij and others rewrite multiplexing from scratch. This project uses tmux as its engine and adds a visual layer on top.
- **Not another AI IDE.** Cursor, Windsurf, and Zed build custom agent harnesses around raw LLMs. This project runs vanilla CLI agents unmodified — their harness is the product, not ours to reinvent.
- **Not another agent orchestrator.** Arbor, Superconductor, Claude Squad, and Nimbalyst manage agent sessions, worktrees, and diffs. This project does that too — but the core value is the reactive IDE layer on top: live diagnostics, file awareness, and code navigation while agents work. Without that layer, it's just another orchestrator.

## Why not existing tools?

**Agent orchestrators** — closest to rift's space, but missing the IDE layer:

| Tool | What it gets right | What's missing |
|---|---|---|
| **Arbor** | Native Rust + GPUI, remote SSH/mosh, agent-agnostic, open source (MIT), daemon architecture | No IDE features (no LSP, no diagnostics, no file explorer). No Windows support (GPUI = macOS/Linux). Replaces tmux with own session management. Hardcoded agent detection. |
| **Superconductor** | 100% Rust, GPU-rendered, unlimited parallel agents, agent-agnostic, polished UX | Closed source. macOS only. No remote-first architecture. No IDE awareness (no LSP, no diagnostics). |
| **Nimbalyst** | Visual workspace, multi-session kanban, diff review, open source | Electron-based. Local-only (no SSH/remote). No terminal rendering. No LSP integration. |
| **Claude Squad** | tmux-native, git worktree isolation, multi-agent TUI | Pure TUI — no visual feedback, no IDE features, no GUI. |

**Traditional IDEs** — have IDE features, but wrong architecture:

| Tool | What it gets right | What's missing |
|---|---|---|
| **Cursor / Windsurf** | Deep AI integration, parallel agents, LSP | Editor-first. Builds own agent harness — agents are not vanilla. Electron. |
| **Zed** | Fast, native, Rust, GPU-rendered | Reinterprets agent harness. No remote-first SSH. Terminal is secondary. |
| **VS Code + Claude Code** | Official extension, plan review, inline diffs | Electron. Editor-centric. Agent is a sidebar panel. |

**rift's unique position:** the only tool that combines reactive IDE awareness (LSP, diagnostics, file explorer) with vanilla terminal agents, remote-first SSH, tmux as engine, and open source — on Windows.

## Core principles

1. **Agent-first.** The terminal agent is the primary actor. Every GUI feature exists to make agent-driven development more observable and controllable.
2. **Vanilla agents.** CLI coding agents (Claude Code, Codex, OpenCode, Gemini CLI) run completely unmodified. The companies behind these agents have invested deeply in their harnesses — prompt engineering, tool use, permission models, context management. This project does not reinterpret, wrap in custom protocols, or replace any of that. It builds around the agent as-is. Agents are interchangeable: swap Claude Code for Codex by changing one config line. If an agent improves upstream, this project benefits automatically.
3. **Open source and free.** Always. The project is open source from day one and free for everyone. It benefits from and contributes back to the open source ecosystem it builds on (alacritty_terminal, russh, tree-sitter, tmux). No freemium, no paid tiers, no telemetry. The tool is yours.
4. **Reactive, not manual.** The IDE reacts to what's happening in the terminal. File changes, diagnostics, git state — all update automatically without user action.
5. **tmux-native.** Don't reinvent multiplexing. tmux is battle-tested and universally available. Use it as the engine, not as a dependency to hide.
6. **Remote-first.** SSH is not an afterthought. The architecture assumes the code lives on a remote host. Local development is a special case of remote where the host is localhost.
7. **Native performance.** Rust + GPUI. No Electron, no web runtime overhead. Terminal rendering must be as fast as Alacritty. GPU-accelerated native rendering.
8. **Personal tool.** This is built for a specific workflow — terminal-based agentic coding on remote hosts. Generality is a non-goal. Solving this one workflow exceptionally well is the goal.

## North star scenarios

**Scenario 1 — Single agent, full visibility.**
You connect to your VPS via the app. Claude Code runs in the main pane. As it works, the file explorer highlights every file it touches. The diagnostics panel shows errors appearing and resolving in real-time. When it's done, the git panel shows a clean diff of everything that changed. You review visually, adjust anything you want directly in the editor, approve, and move on.

**Scenario 2 — Parallel agents, multiple worktrees.**
You're working on a feature branch in one pane while a second Claude Code instance refactors tests in another worktree. Each pane has its own file explorer context. Diagnostics are scoped per worktree. You glance at both, notice the test agent introduced a type error, and intervene — all without switching windows or running manual commands.

**Scenario 3 — IDE comfort, terminal power.**
You right-click a function call and select "Go to Definition." The app sends the LSP request and rift's editor jumps to the definition — in the GUI, not by remote-controlling a terminal editor. You ctrl+click an import path — same thing. You scroll the file explorer, double-click a test file, it opens in the editor; you fix a typo and save, and the change lands on the remote. The mouse works. The keyboard works. It feels like an IDE because it is one — while every process still runs in tmux.

**Scenario 4 — Swap agents, keep everything else.**
You've been using Claude Code for a feature. Mid-sprint, you want to try Codex on a different task. You open a new pane, it launches Codex instead of Claude Code — one config line different. The file explorer, diagnostics, git diff view all work identically because they don't care which agent is writing the code. The agent is a black box that edits files in a terminal. Everything around it stays the same.

## Current status

Phase 2 (tmux control mode integration) complete. SSH connection to remote tmux via control mode (`-CC`), event-driven notification processing, flow control, active pane tracking, working directory from tmux snapshots, terminal rendering through GPUI with `alacritty_terminal` and `termy_terminal_ui`. Multi-pane awareness (Phase 2c) complete: per-pane VTE parsers, snapshot-driven pane lifecycle, split-tree layout from tmux pane coordinates. Next: tab bar for tmux window switching, then daemon with file tree, git status, and LSP diagnostics on the remote host.
