# Vision — rift

> An agent-centric IDE for terminal-based coding agents.

## The problem

Terminal-based coding agents like Claude Code have fundamentally changed how software gets written. You describe intent, the agent writes code, runs tests, iterates — and you guide the process. It's the most productive way to build software today.

But you're flying blind.

The agent edits files you can't see. It introduces errors you won't notice until the next prompt. It commits changes you have to `git log` to understand. You watch raw text scroll by in a terminal and reconstruct what happened after the fact. For vibe coding — shipping fast, trusting the agent — this works. For agentic coding — directing multiple agents, reviewing in real-time, maintaining quality at scale — it doesn't.

## The gap

The problem isn't that IDEs lack AI features. Every editor has bolted on copilots, chat panels, inline suggestions. But they all make the same architectural mistake: **they put the editor at the center and treat the agent as a peripheral feature.**

The result is a mismatch. You're running Claude Code in a terminal pane that the IDE barely acknowledges. The file explorer doesn't react to what the agent is doing. Diagnostics don't update in real-time as the agent edits. Git changes aren't surfaced until you manually refresh. The IDE and the agent exist in parallel universes.

On the other side, pure terminal setups (tmux + Neovim) give the agent a first-class home but lack the visual feedback that makes complex work manageable. You can build tmux layouts, configure status bars, write scripts — but a TUI will never match a native GUI for information density, mouse interaction, and spatial awareness.

Recent tools have started to address pieces of this. Nimbalyst wraps CLI agents in a visual workspace with session management and diff review — but it's Electron-based, local-only, and doesn't render terminals natively. Claude Squad manages multiple agents via tmux and git worktrees — but it's a TUI with no visual feedback beyond the terminal. Claude Code's own VS Code extension adds plan review and inline diffs — but it lives inside an editor-first IDE that treats the agent as a sidebar feature.

No tool combines native GUI performance, remote-first SSH architecture, tmux as the multiplexing engine, and unmodified CLI agents — while being open source and free.

## The solution

This project is an **agent-centric IDE** — a native GUI shell that treats terminal-based coding agents as the primary interface and provides reactive IDE features around them.

The core idea: **tmux is the engine, the GUI is the cockpit.**

tmux handles what it's already great at — session management, pane multiplexing, persistent processes. The GUI adds what tmux can't — real-time file explorer updates, live LSP diagnostics, visual git diffs, structured agent output, and native mouse interaction including context menus and ctrl+click navigation.

When a coding agent edits a file, the file explorer lights up. When it introduces a type error, the diagnostics panel shows it immediately. When it commits, the git diff view surfaces what changed. When multiple agents run in parallel across worktrees, you see all of them at a glance.

The IDE doesn't compete with the agent — it amplifies it.

## What this is not

- **Not another terminal emulator.** Alacritty, WezTerm, and Kitty are excellent at rendering terminal output. This project uses terminal emulation as a building block, not as the product.
- **Not another text editor.** Neovim runs inside the terminal panes and handles editing. This project doesn't reimplement text editing.
- **Not another tmux replacement.** Zellij and others rewrite multiplexing from scratch. This project uses tmux as its engine and adds a visual layer on top.
- **Not another AI IDE.** Cursor, Windsurf, and Zed build custom agent harnesses around raw LLMs. This project runs vanilla CLI agents unmodified — their harness is the product, not ours to reinvent.
- **Not another agent GUI wrapper.** Nimbalyst and Claude Code Desktop wrap agents in Electron. This project renders terminals natively in Rust with GPU acceleration, connects to remote hosts via SSH, and treats the terminal as the primary surface — not a chat panel.

## Why not existing tools?

| Tool | What it gets right | Where it falls short |
|---|---|---|
| **Nimbalyst** | Visual workspace around CLI agents, multi-session kanban, diff review | Electron, local-only (no SSH/remote), no terminal rendering, no tmux integration |
| **Claude Squad** | tmux-native, git worktree isolation, multi-agent TUI | Pure TUI — no visual feedback, no IDE features, no GUI |
| **Cursor / Windsurf** | Deep AI integration, parallel agents | Editor-first, builds own agent harness around raw LLMs — agents are not vanilla |
| **Zed** | Fast, native, Rust | Reinterprets the entire agent harness, no remote-first, terminal is secondary |
| **VS Code + Claude Code** | Official extension, plan review, inline diffs | Electron, editor-centric, agent is a sidebar panel |
| **Warp** | Modern terminal UX | No remote tmux support, no IDE features, no agent awareness |
| **tmux + Neovim** | Agent-native, lightweight, remote-ready | Blind — no visual feedback, TUI limits on information density |

The unique intersection: **vanilla CLI agents + native GUI + remote-first SSH + tmux engine + open source.**

## Core principles

1. **Agent-first.** The terminal agent is the primary actor. Every GUI feature exists to make agent-driven development more observable and controllable.
2. **Vanilla agents.** CLI coding agents (Claude Code, Codex, OpenCode, Gemini CLI) run completely unmodified. The companies behind these agents have invested deeply in their harnesses — prompt engineering, tool use, permission models, context management. This project does not reinterpret, wrap in custom protocols, or replace any of that. It builds around the agent as-is. Agents are interchangeable: swap Claude Code for Codex by changing one config line. If an agent improves upstream, this project benefits automatically.
3. **Open source and free.** Always. The project is open source from day one and free for everyone. It benefits from and contributes back to the open source ecosystem it builds on (alacritty_terminal, russh, tree-sitter, tmux). No freemium, no paid tiers, no telemetry. The tool is yours.
4. **Reactive, not manual.** The IDE reacts to what's happening in the terminal. File changes, diagnostics, git state — all update automatically without user action.
5. **tmux-native.** Don't reinvent multiplexing. tmux is battle-tested and universally available. Use it as the engine, not as a dependency to hide.
6. **Remote-first.** SSH is not an afterthought. The architecture assumes the code lives on a remote host. Local development is a special case of remote where the host is localhost.
7. **Native performance.** Rust + Tauri. No Electron, no web runtime overhead. Terminal rendering must be as fast as Alacritty. GUI must feel native on Windows.
8. **Personal tool.** This is built for a specific workflow — terminal-based agentic coding on remote hosts. Generality is a non-goal. Solving this one workflow exceptionally well is the goal.

## North star scenarios

**Scenario 1 — Single agent, full visibility.**
You connect to your VPS via the app. Claude Code runs in the main pane. As it works, the file explorer highlights every file it touches. The diagnostics panel shows errors appearing and resolving in real-time. When it's done, the git panel shows a clean diff of everything that changed. You review visually, approve, and move on.

**Scenario 2 — Parallel agents, multiple worktrees.**
You're working on a feature branch in one pane while a second Claude Code instance refactors tests in another worktree. Each pane has its own file explorer context. Diagnostics are scoped per worktree. You glance at both, notice the test agent introduced a type error, and intervene — all without switching windows or running manual commands.

**Scenario 3 — IDE comfort, terminal power.**
You right-click a function call in the terminal output and select "Go to Definition." The app sends the LSP request, Neovim jumps to the definition. You ctrl+click an import path — same thing. You scroll the file explorer, double-click a test file, it opens in Neovim in a new pane. The mouse works. The keyboard works. It feels like an IDE but the terminal is in charge.

**Scenario 4 — Swap agents, keep everything else.**
You've been using Claude Code for a feature. Mid-sprint, you want to try Codex on a different task. You open a new pane, it launches Codex instead of Claude Code — one config line different. The file explorer, diagnostics, git diff view all work identically because they don't care which agent is writing the code. The agent is a black box that edits files in a terminal. Everything around it stays the same.

## Current status

Early concept phase. Architecture defined, technology validated (Rust, Tauri, tmux control mode, alacritty_terminal, LSP). No code yet.
