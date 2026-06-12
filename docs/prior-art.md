# rift — Parts Catalog of Open-Source Reference Projects

> Status: Living reference (last research: 2026-06)
> Purpose: Prior art for design decisions. Specs cite this in their "Prior decisions" section.

This is a **menu to draw from per phase**, not a mandate to build everything now. Adopt patterns and dependencies when a `READY` spec calls for them; keep scope incremental.

## TL;DR

- The closest precedent for rift is **Arbor (penso/arbor, Rust + GPUI, MIT)** — it already proves the pattern of a native GPUI desktop app + daemon + SSH outposts + agent state detection, and should be the first project to study end-to-end.
- For UI primitives use **longbridge/gpui-component** (dock layout, virtualized list/table, tabs, context menus, theming), and for engine-level subsystems borrow from the Zed monorepo crates (`crates/remote`, `crates/remote_server`, `crates/lsp`, `crates/project_panel`, `crates/git`, `crates/terminal`) plus **Helix's `helix-lsp`** or **`async-lsp`** for an extractable LSP client.
- tmux integration should be built directly against tmux **control mode (`-CC`)** — the only documented programmatic interface — using **iTerm2** and **Arbor** as architectural references and **AntonGepting/tmux-interface-rs** only as a typed command builder; do not attempt to parse tmux internals.

## Key Findings

1. **GPUI ecosystem is maturing fast.** longbridge/gpui-component now ships virtualized lists, a dock/tile layout system, code editor with LSP, and 60+ stateless components — rift should depend on it rather than rebuild primitives.
2. **Three Rust+GPUI projects are within one degree of rift's exact problem:** Arbor (worktrees + agent state + SSH outposts), Hunk (GPUI diff viewer + Codex orchestrator), Superconductor (native parallel-agent IDE). Arbor's source is MIT and directly readable; Superconductor publishes some open code on GitHub for the agent layer.
3. **Zed's remote architecture is the gold standard reference for rift's daemon design.** The `remote_server` binary, `RemoteConnection` trait, SSH ControlMaster reuse, automatic versioned binary upload, and `HeadlessProject` mirror almost exactly the architecture rift describes — read these crates before writing any daemon code.
4. **Helix's `helix-lsp` is the cleanest extractable LSP client implementation** for a non-editor project; for a greenfield design `async-lsp` (oxalica) is a Tower-based framework that works symmetrically as client and server and is actively maintained, while `tower-lsp`'s final release (v0.20.0) is now listed on crates.io as "by Eyal Kalderon over 2 years ago."
5. **tmux control mode is sparsely documented but stable.** The authoritative sources are the tmux wiki page on Control Mode, the iTerm2 documentation, and reading `control.c` in tmux/tmux. There is no separate spec — plan to implement a parser for `%output`, `%window-add`, `%session-changed`, `%layout-change` notifications and the `%begin`/`%end`/`%error` command-response guards.
6. **lsp-types is stalled.** The gluon-lang/lsp-types crate's latest release v0.97.0 is listed on crates.io as "about 1 year ago" (Markus Westerlind), and tower-lsp-community has forked it as `ls-types` with a planned codegen rewrite. Flag this risk in dependency planning.
7. **The "daemon with auto-deploy" pattern is now industry-standard.** Zed, Lapce, VS Code Remote, and Arbor all implement: SSH connect → detect platform via `uname -sm` → download/upload versioned daemon binary → spawn daemon → reattach on reconnect → multiplex multiple channels over one ControlMaster.

---

## Details

### Category 1: GPUI Applications & Components

#### 1. longbridge/gpui-component (TOP PRIORITY)
- **URL**: https://github.com/longbridge/gpui-component
- **License**: Apache-2.0
- **Tech stack**: Rust + GPUI
- **Stars / Activity**: 11.5k stars / 605 forks (May 2026); very active (releases through 2025/2026; recent PRs #1659, #1690, #1694 on scrollbar refactors, #1601 on bundled assets)
- **What it does**: 60+ cross-platform GPUI components — Button, Input, Select, Dropdown, virtualized List/Table, Dock layout (panels/splits/tabs/tiles), Markdown, HTML, charts, code editor with LSP and Tree-sitter syntax highlighting.
- **Relevant for rift**: This is the single most reusable dependency for rift's UI surface. **Dock layout** solves tab bars, split panels, and resize handles. **Virtualized List/Table** solves file explorer scaling. **Theme/ThemeColor** solves theming. **Scrollbar**, **TitleBar**, **Settings** components solve standard chrome. The `story` crate is a working gallery of every component.
- **Key files/modules**: `crates/story/` (gallery), `examples/dock/` (panels + splits + tabs), `crates/ui/src/dock/`, `crates/ui/src/list/virtual_list.rs`, `crates/ui/src/scrollbar.rs`, `examples/editor` (LSP-enabled code editor).
- **Usable as dependency?**: **Yes** — Apache-2.0, used via git dep (`gpui-component = { git = "https://github.com/longbridge/gpui-component" }`); the upstream warns to pin to a specific GPUI commit alongside it.

```rust
// Stateless RenderOnce pattern — gpui-component README
use gpui::*;
use gpui_component::{button::*, *};
pub struct HelloWorld;
impl Render for HelloWorld {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().v_flex().gap_2().size_full().items_center().justify_center()
            .child("Hello, World!")
            .child(Button::new("ok").primary().label("Let's Go!")
                .on_click(|_, _, _| println!("Clicked!")))
    }
}
```

#### 2. zed-industries/zed (architectural reference monorepo)
- **URL**: https://github.com/zed-industries/zed
- **License**: GPL-3.0 (most crates; mixed)
- **Tech stack**: Rust + GPUI
- **Activity**: 2-3 builds/week, ~50k+ stars
- **What it does**: The reference GPUI application — production code editor with workspace, dock, panels, panes, multi-window, remote development, LSP, terminal, git.
- **Relevant for rift**: The patterns rift needs are all here: `Workspace` orchestrator owning a `PaneGroup` tree, `Dock` entities for left/right/bottom panels, `Item` lifecycle (open/close/serialize), `SettingsStore` hierarchical merging, `WorkspaceDb` persistence with `SERIALIZATION_THROTTLE_TIME` debouncing.
- **Key files/modules**: `crates/workspace/src/workspace.rs`, `crates/workspace/src/dock.rs`, `crates/workspace/src/persistence.rs`, `crates/workspace/src/multi_workspace.rs`, `crates/gpui/`, `crates/zed/src/main.rs`.
- **Usable as dependency?**: GPUI itself yes (git dep); other crates mostly **no, study only** — most are tightly coupled to the Zed Project/Workspace model and GPL-3.0 means rift would need to remain GPL-3.0 (already is, so this is OK if extracting).

#### 3. smolcars/hunk
- **URL**: https://github.com/smolcars/hunk
- **License**: GPL-3.0
- **Tech stack**: Rust + GPUI + gpui-component + gix + git2
- **What it does**: Cross-platform GPUI git-diff viewer and embedded Codex orchestrator.
- **Relevant for rift**: A working GPUI app that combines a Codex CLI agent (`crates/hunk-codex`), a terminal (`crates/hunk-terminal`), git diffing (`crates/hunk-git`), and a rope-backed text model with undo (`crates/hunk-text`) — essentially every subsystem rift needs, in one repo at usable scale. Includes a perf harness for large-diff scrolling FPS testing.
- **Key files/modules**: `crates/hunk-git`, `crates/hunk-codex`, `crates/hunk-terminal`, `crates/hunk-text`, `crates/hunk-language` (Tree-sitter registry), `crates/hunk-desktop`, `PERFORMANCE_BENCHMARK.md`.
- **Usable as dependency?**: GPL-3.0 — license-compatible with rift; the per-domain crate split (`hunk-git`, `hunk-text`, etc.) is the borrowing target rather than direct dep.

#### 4. noh-rs/nohrs
- **URL**: https://github.com/noh-rs/nohrs
- **License**: (check repo; permissive intended) — early-stage GPUI app
- **Tech stack**: Rust + GPUI + virtualized list from gpui-component
- **What it does**: macOS Finder alternative built in GPUI.
- **Relevant for rift**: Smallest readable starting point for a GPUI file explorer. The author's writeup explicitly uses gpui-component's `VirtualList` for large directory rendering and demonstrates the `Context::listener` event pattern for safe self-capture.
- **Usable as dependency?**: No — study only.

#### 5. Augani/nexus-explorer
- **URL**: https://github.com/Augani/nexus-explorer
- **License**: (verify on repo)
- **Tech stack**: Rust + GPUI + jwalk + nucleo (from Helix) + notify
- **What it does**: GPU-accelerated multi-tab file explorer with per-tab integrated terminal.
- **Relevant for rift**: Explicit architecture diagram showing GPUI main thread + tokio worker pool + jwalk for directory traversal + `notify` for file watching — exactly rift's daemon-side workload. Use of nucleo (Helix's fuzzy matcher) is a good fzf-like search reference.
- **Usable as dependency?**: No — study its layering.

### Category 2: Terminal Emulators in Rust

#### 1. lassejlv/termy (TOP PRIORITY — closest to rift's stack)
- **URL**: https://github.com/lassejlv/termy
- **License**: MIT
- **Tech stack**: Rust + GPUI + alacritty_terminal (exact same triple as rift)
- **What it does**: Minimal cross-platform GPU-accelerated terminal emulator.
- **Relevant for rift**: Reference implementation for wiring `alacritty_terminal` to GPUI specifically. Read for: PTY event loop integration, render-on-change vs render-every-event throttling, IME (CJK input) handling on GPUI which the author calls out as undocumented, Windows ConPTY vs Unix PTY differences.
- **Key files/modules**: top-level `src/terminal.rs`, `src/pty/`, `docs/keybindings.md`.
- **Usable as dependency?**: No — extract patterns; small enough to read end-to-end.

#### 2. wezterm/wezterm
- **URL**: https://github.com/wezterm/wezterm
- **License**: MIT
- **Tech stack**: Rust workspace, 19+ crates; OpenGL renderer
- **What it does**: GPU-accelerated terminal emulator + multiplexer (similar conceptual scope to tmux + a GUI).
- **Relevant for rift**: WezTerm has solved the **client-server multiplexer architecture** in pure Rust with a codec-based RPC protocol. The crate split is itself a design template: `wezterm-term` (VTE), `mux` (sessions/windows/tabs/panes), `wezterm-client`, `wezterm-mux-server`, `wezterm-gui`. The `Pane` trait + `Domain` trait pattern for abstracting local-vs-remote panes maps directly onto rift's "remote is single source of truth" architecture.
- **Key files/modules**: `term/`, `mux/`, `wezterm-client/`, `wezterm-mux-server/`, `wezterm-gui/`, the `Pane` trait, `LocalPane` wrapping `Terminal` + `MasterPty`.
- **Usable as dependency?**: No (too large) — but the modular crate split is the architectural blueprint to copy.

#### 3. alacritty/alacritty (and the `alacritty_terminal` library)
- **URL**: https://github.com/alacritty/alacritty ; docs at https://docs.rs/alacritty_terminal
- **License**: Apache-2.0
- **What it does**: `alacritty_terminal` provides VTE parsing, the `Term<>` grid, Selection, scrollback — the "headless terminal" half of what rift needs.
- **Relevant for rift**: Already a dependency. Notable types to study: `event_loop` (PTY I/O loop), `grid::Grid`, `Term`, `Line`/`Column` strongly-typed indices.
- **Usable as dependency?**: **Yes** — already in use.

#### 4. gpui-terminal crate (third-party packaging)
- **URL**: https://docs.rs/gpui-terminal
- **License**: (check crate) — embeds alacritty_terminal in a `TerminalView` GPUI entity.
- **Relevant for rift**: Offers a near-drop-in `TerminalView` with PTY via `portable-pty`, color palette builder, font metrics, callback dispatch (resize/exit/bell/title/clipboard). Even if rift doesn't depend on it, the architecture diagram in its docs is a precise blueprint of the wiring rift needs.

### Category 3: tmux Integration

#### 1. tmux Control Mode spec (PRIMARY REFERENCE)
- **URL**: https://github.com/tmux/tmux/wiki/Control-Mode ; man page section "CONTROL MODE"; iTerm2 docs at https://iterm2.com/documentation-tmux-integration.html ; DeepWiki at https://deepwiki.com/tmux/tmux/7.1-control-mode ; `control.c` in tmux source.
- **What it provides**: `tmux -C` / `-CC` flags, line-oriented protocol with `%begin`/`%end`/`%error` command guards and `%output`, `%window-add`, `%window-close`, `%session-changed`, `%layout-change`, `%pane-mode-changed`, `%client-session-changed` notifications. `refresh-client -C` sets control-client size; `refresh-client -f` sets flags like `no-output`, `wait-exit`.
- **Relevant for rift**: This is the protocol rift's daemon must speak. Maintainer Nicholas Marriott (tmux upstream) confirmed in the tmux-users mailing list that **there is no separate spec beyond the man page, wiki, and source** — the iTerm2 code and tmux's `control.c` are the canonical references.

#### 2. AntonGepting/tmux-interface-rs
- **URL**: https://github.com/AntonGepting/tmux-interface-rs ; crates.io `tmux_interface = "0.3.2"`
- **License**: MIT
- **Activity**: Last release 2024 (~2 years old); pre-1.0
- **What it does**: Typed Rust builders for tmux CLI commands (`new-session`, `list-sessions`, etc.).
- **Relevant for rift**: Use as the **command-construction layer** when emitting tmux commands over control mode. Does NOT parse control-mode responses — rift will need its own parser for the `%`-prefixed notification stream.
- **Usable as dependency?**: Yes (MIT, small) — but expect to maintain a small fork or wrapper given the slow upstream cadence.

#### 3. smtg-ai/claude-squad
- **URL**: https://github.com/smtg-ai/claude-squad
- **License**: AGPL-3.0
- **Tech stack**: Go + tmux + git worktrees + Cobra
- **Stars / Activity**: 7.6k stars / 543 forks (May 2026); latest release v1.0.18 published May 23, 2026
- **What it does**: Multi-agent TUI session manager — spawns each agent in an isolated tmux session backed by a git worktree.
- **Relevant for rift**: The single most relevant working reference for "wrap CLI coding agents around tmux". Read `session/tmux/` for tmux capture-pane usage, `app/app.go` for the central `Instance` state machine, `daemon/` for the background mode that runs without the TUI attached. Issue #189 ("TMUX error capturing pane content") is itself a useful reading on edge cases.
- **Usable as dependency?**: No (Go; AGPL would conflict if forked into rift) — study only.

#### 4. workmux
- **URL**: https://crates.io/crates/workmux
- **License**: MIT
- **Tech stack**: Rust + tmux + git worktrees
- **What it does**: Couples git worktrees to tmux windows with declarative pane configs.
- **Relevant for rift**: Pane-spawning config schema (`panes:` array with `command`, `split`, `focus`) and `post_create` hooks are a clean template for rift's plugin/config layer.

#### 5. iTerm2 tmux integration (architectural reference only)
- **URL**: https://iterm2.com/documentation-tmux-integration.html (and the iTerm2 source on GitHub)
- **License**: GPL-2.0 (iTerm2)
- **Relevant for rift**: The original `-CC` consumer. Documents real operational concerns rift will hit: window-size constraints across attached clients, force-quit recovery (`X` key), the menu used to surface debugging.

### Category 4: Agent Orchestrators & Workspaces

#### 1. penso/arbor (HIGHEST OVERLAP WITH RIFT)
- **URL**: https://github.com/penso/arbor
- **License**: MIT
- **Tech stack**: Rust + GPUI + Ghostty terminal backend + daemon (`arbor-httpd`) + WebSocket + SSH/mosh transports + arbor-mcp + arbor-cli
- **Activity**: Active (multiple binaries shipped via Just; nightly Rust 2025-11-30 toolchain)
- **What it does**: Native desktop agentic workspace — worktree management, embedded terminals, managed processes, diff inspection, real-time Claude Code / Codex / OpenCode state detection, remote outposts over SSH+mosh.
- **Relevant for rift — read this first**:
  - **Daemon-shared backend**: `arbor-httpd` is the single source of truth for desktop, web UI, CLI, MCP — exactly rift's "remote is single source of truth" architecture.
  - **Remote outposts**: SSH + mosh transport with host status tracking is a working precedent for rift's russh + WebSocket plan.
  - **Agent state detection**: "Working/waiting state indicators with color-coded dots" via "real-time updates over WebSocket streaming" — the exact UX rift's plugins are meant to deliver.
  - **Plugin-style provider config**: `[[providers]]` in `~/.config/arbor/config.toml` with per-provider OpenAI/ACP detection.
- **Key crates to study**: `arbor-ssh` (SSH transport), `arbor-terminal-emulator` (Ghostty glue), `arbor-symphony` (orchestration runtime), `arbor-web-ui`, `arbor-mcp`, `arbor-cli`.
- **Usable as dependency?**: No (full app, MIT-licensed) — read it as the reference implementation.

#### 2. horang-labs/tessera
- **URL**: https://github.com/horang-labs/tessera
- **License**: Apache-2.0
- **Tech stack**: TypeScript + Electron, local server on `127.0.0.1:32123`
- **Activity**: Active in 2026; Linux beta builds added recently; `@horang-labs/tessera@0.1.2` published to npm
- **What it does**: Multi-CLI agent workspace running Claude Code, Codex, OpenCode side-by-side with kanban tasks, managed worktrees, unified diff/PR review.
- **Relevant for rift**: The exact **agent-protocol normalization** pattern rift needs. From the Tessera README: *"Provider adapter architecture: each CLI is isolated behind a CliProvider contract for process lifecycle, protocol parsing, runtime controls, approvals, interrupts, and skills. Protocol normalization layer: Claude Code stream-json, Codex app-server, and OpenCode ACP JSON-RPC events are translated into a shared realtime message model."* This `CliProvider` trait + shared message model is precisely the plugin contract rift should design.
- **Usable as dependency?**: No (TypeScript) — borrow the trait shape.

#### 3. Nimbalyst/nimbalyst
- **URL**: https://github.com/Nimbalyst/nimbalyst
- **License**: MIT (desktop+iOS) / AGPL (team layer) — split licensing
- **Tech stack**: Electron + Monaco; bundles `@openai/codex-sdk@0.128.0`
- **What it does**: Free local visual workspace and session manager for Claude Code, Codex, OpenCode (alpha), Copilot (alpha) with WYSIWYG editors and parallel kanban sessions in git worktrees.
- **Relevant for rift**:
  - **`EditorHost` contract**: Every editor (built-in or custom) plugs into the same host trait, with `supportsTranscriptEmbed`/`transcriptEmbedHeight` capability flags. Direct template for rift's "plugin per pane-type" design.
  - **Skills/commands compatibility layer**: Translates between Claude-Code-format and Codex-format slash commands so the user library works cross-agent — same mapping problem rift's daemon will face.
  - **Per-provider MCP configuration**: Session-scoped tool exposure.
- **Verbatim README quote**: *"Pluggable editors for any file type. Every editor (including built-ins) goes through the same EditorHost contract, so custom editors are first-class."*
- **Usable as dependency?**: No (Electron) — borrow the extension manifest + EditorHost trait shape.

#### 4. oscardobsonbrown/superconductor
- **URL**: https://github.com/oscardobsonbrown/superconductor
- **License**: (review on repo)
- **Tech stack**: 100% Rust, GPU-rendered on Metal (macOS native), no Electron
- **What it does**: Native macOS workspace for parallel coding agents with isolated git worktrees, native subprocess spawning, split terminal, code PIP, native diffs.
- **Relevant for rift**: The clearest existing example of "fully-native Rust agent IDE". Picture-in-picture pane mode and per-workspace theming are UX ideas rift can consider.
- **Usable as dependency?**: No — study, especially process lifecycle for agent CLIs.

#### 5. smtg-ai/claude-squad (also under Category 3)
- See Category 3. Read `app/app.go48-95` (the `Instance` struct as central orchestrator).

#### 6. ComposioHQ/agent-orchestrator
- **URL**: https://github.com/ComposioHQ/agent-orchestrator
- **License**: MIT
- **Tech stack**: TypeScript/Node, supports `tmux` runtime on macOS/Linux and ConPTY/`process` on Windows
- **What it does**: Plugin-based orchestrator (runtime/agent/workspace/tracker/SCM/notifier/terminal/lifecycle slots).
- **Relevant for rift**: The plugin-slot taxonomy is a useful inventory of extensibility hooks rift's plugin system might want.

#### 7. asheshgoplani/agent-deck
- **URL**: https://github.com/asheshgoplani/agent-deck
- **License**: (review)
- **What it does**: Remote SSH session manager surfacing remote agents alongside local in one TUI.
- **Relevant for rift**: Concrete example of "remote sessions appear alongside local sessions" — direct UX precedent for rift's outpost-via-SSH model. Docker sandbox pattern with shared `mount_ssh` directories.

### Category 5: File Explorers & Tree Views

#### 1. Zed `crates/project_panel`
- **URL**: https://github.com/zed-industries/zed/tree/main/crates/project_panel
- **License**: GPL-3.0
- **Relevant for rift**: The reference for large-repo (100k+ files) GPUI tree views with git status badges and reactive updates from a `Worktree` model. Read alongside `crates/worktree/` which provides the `Snapshot` model that mirrors filesystem state with proto-based incremental `UpdateWorktree` messages — exactly the daemon→client protocol rift needs.
- **Key files**: `crates/project_panel/src/project_panel.rs`, `crates/worktree/src/worktree.rs`, `crates/proto/proto/worktree.proto`.
- **Usable as dependency?**: Tightly coupled to Zed's `Project` — study only.

#### 2. sxyazi/yazi
- **URL**: https://github.com/sxyazi/yazi
- **License**: MIT
- **Tech stack**: Rust, fully async I/O on tokio
- **Stars**: 38.4k (May 2026)
- **What it does**: Async terminal file manager with preloading, plugin system, image previews.
- **Relevant for rift**: Architectural exemplar of "the UI never blocks on filesystem I/O". The blog post "Why is Yazi Fast?" is required reading; the project also implements a russh-based SFTP provider for remote file access — directly relevant to rift's remote-host file watching over SSH.
- **Key files**: `yazi-fs/`, `yazi-proxy/`, `yazi-plugin/`.
- **Usable as dependency?**: Some crates are independently usable; mostly study.

#### 3. Augani/nexus-explorer (see Category 1) — GPUI + jwalk + notify + nucleo.

#### 4. noh-rs/nohrs (see Category 1) — smallest GPUI file-explorer reading.

#### 5. broot (Canop/broot)
- **URL**: https://github.com/Canop/broot
- **License**: MIT
- **What it does**: TUI tree-view file navigator with fuzzy search.
- **Relevant for rift**: The fuzzy-narrowing UX of broot's incremental search is a useful pattern for the rift file panel's command-palette-style filter.

### Category 6: Git Integration in Rust

#### 1. Auto-Explore/GitComet (TOP PRIORITY — GPUI-native)
- **URL**: https://github.com/Auto-Explore/GitComet
- **License**: (review on repo — open-source per README)
- **Tech stack**: Rust + GPUI + `gix` (primary) + `git2` (narrow fallback for unsupported writes) + `smol`
- **What it does**: Cross-platform GPUI Git GUI optimized for huge repos (Chromium-scale).
- **Relevant for rift**: The team's writeup details two patterns rift will need verbatim: **history virtualization** (render only visible commits, reuse UI elements while scrolling) and **diff rendering throughput on 50MB / 500k-line files** (early naive implementations OOM'd). GitComet also doubles as a `git difftool` and `git mergetool` binary with both headless and GUI modes — a clean CLI surface design template.
- **Key files**: GPUI app code; the `cargo run -p gitcomet --features ui-gpui,gix` build target.
- **Usable as dependency?**: No — study patterns.

```bash
# GitComet's mergetool/difftool registration — a clean template for rift's git integration
git config --global merge.guitool gitcomet-gui
git config --global mergetool.gitcomet-gui.cmd \
  "'$GITCOMET_BIN' mergetool --gui --base \"\$BASE\" --local \"\$LOCAL\" \
   --remote \"\$REMOTE\" --merged \"\$MERGED\""
```

#### 2. smolcars/hunk (see Category 1) — GPUI diff viewer + perf harness.

#### 3. gitui-org/gitui
- **URL**: https://github.com/gitui-org/gitui
- **License**: MIT
- **Tech stack**: Rust + ratatui + `git2`
- **Stars**: 18k+; active
- **What it does**: TUI git client.
- **Relevant for rift**: Cleanest and smallest readable Rust `git2` usage in production — staging, hunk-level operations, branch ops, async fetch/push that doesn't block UI. The architecture of putting expensive git operations on background threads with channel-back UI updates is directly applicable.
- **Key files**: `asyncgit/` crate (async git operations), `src/components/` (UI components).
- **Usable as dependency?**: `asyncgit` could be referenced; otherwise study.

#### 4. Zed `crates/git`
- **URL**: https://github.com/zed-industries/zed/tree/main/crates/git
- **License**: GPL-3.0
- **Relevant for rift**: Production code for git diff rendering inside a GPUI editor and panel; shows the `GitStore` entity pattern that synchronizes git state between local and remote.

#### 5. jesseduffield/lazygit (Go; reference)
- **URL**: https://github.com/jesseduffield/lazygit
- **License**: MIT
- **Activity**: v0.61.0 released April 6, 2026; 78.3k stars / 2.8k forks
- **Relevant for rift**: UX reference for interactive rebase, hunk-level staging by line/range/hunk (`space`/`v`/`a` keys), and one-keystroke amend. The discoverability pattern (visible pane focus + hotkey hints) is also worth mimicking.

### Category 7: LSP Client Implementations

#### 1. helix-editor/helix — `helix-lsp` crate (TOP PRIORITY for extractable LSP client)
- **URL**: https://github.com/helix-editor/helix/tree/master/helix-lsp
- **License**: MPL-2.0 (license-compatible with rift's GPL-3.0)
- **Tech stack**: Rust + tokio + `lsp-types` + jsonrpc-core
- **What it does**: Production LSP client supporting multi-server-per-document, offset-encoding handling (UTF-8/UTF-16), capability negotiation, request-response correlation with timeout.
- **Relevant for rift**: The **`Registry` pattern** maintaining `HashMap<LanguageServerName, Vec<LanguageServerId>>` lets multiple servers per language coexist — directly addresses rift's "aggregate diagnostics from multiple language servers" requirement. The `Transport::start()` triple-task split (recv/send/err) is the canonical structure for an LSP stdio pipe.
- **Key files**: `helix-lsp/src/client.rs` (the `Client` struct with `OnceCell<ServerCapabilities>`), `helix-lsp/src/transport.rs` (recv/send/err tasks), `helix-lsp/src/util.rs` (offset-encoding conversion), `helix-lsp/src/lib.rs` (the `Registry`).
- **Usable as dependency?**: Yes — `helix-lsp` is published as part of the helix workspace; can be pulled as a git dep. License (MPL-2.0) compatible with GPL-3.0.

```rust
// Helix's stdio LSP transport: three tasks, JSON-RPC over Content-Length framing
// helix-lsp/src/transport.rs (pattern, paraphrased)
async fn start(server_stdout: BufReader<ChildStdout>,
               server_stdin: BufWriter<ChildStdin>,
               server_stderr: BufReader<ChildStderr>) {
    tokio::spawn(Self::recv(server_stdout, /* route to pending or notify channel */));
    tokio::spawn(Self::send(server_stdin,  /* serialize Payload to LSP wire format */));
    tokio::spawn(Self::err (server_stderr, /* log stderr */));
}
// Frame: "Content-Length: 123\r\n\r\n{\"jsonrpc\":\"2.0\",...}"
```

#### 2. oxalica/async-lsp
- **URL**: https://github.com/oxalica/async-lsp ; https://docs.rs/async-lsp
- **License**: MIT OR Apache-2.0
- **Activity**: v0.2.3 released Mar 4, 2026; 149+ stars; 158 commits — actively maintained
- **What it does**: Tower-based async LSP framework — symmetric (server **and** client) via duplex channels with composable `tower::Layer` middleware.
- **Relevant for rift**: This is the cleanest greenfield LSP-client choice. Provides:
  - `MainLoop::new_client` + `Router<ClientState>` for registering typed notification handlers via `.notification::<PublishDiagnostics>(|state, params| { /* merge */ ControlFlow::Continue(()) })`.
  - `ServiceBuilder::new().layer(TracingLayer).layer(CatchUnwindLayer).layer(ConcurrencyLayer).service(router)` for production-grade middleware.
  - `mainloop.run_buffered(stdout, stdin)` for piped child process plumbing.
- **README quote**: *"Despite the name of `LspService`, it can be used to build both Language Server and Language Client. They are logically symmetric and both using duplex channels."*
- **Usable as dependency?**: **Yes — recommended primary LSP client crate** for rift (active, MIT/Apache, client-first design). License-compatible with GPL-3.0.

#### 3. lapce/lapce — `lapce-proxy/src/plugin/lsp.rs`
- **URL**: https://github.com/lapce/lapce/blob/master/lapce-proxy/src/plugin/lsp.rs
- **License**: Apache-2.0
- **Relevant for rift**: The exact same architectural pattern rift describes — UI runs locally, **LSP servers run inside a remote proxy daemon**. The `PLUGIN_RPC.start_lsp(server_uri, server_args, document_selector, initialization_options)` API shows how to register one LSP per `DocumentSelector` so multiple servers coexist routed by file type. `PluginHostHandler` wraps `volt_id`, `document_selector`, and bidirectional RPC channels (`core_rpc`, `server_rpc`, `plugin_rpc`).
- **Key files**: `lapce-proxy/src/plugin/lsp.rs`, `lapce-proxy/src/plugin/wasi.rs`, `lapce-proxy/src/dispatch.rs`, `lapce-proxy/src/remote.rs`.
- **Usable as dependency?**: Not a crate — read and adapt.

#### 4. Zed `crates/lsp` + `crates/project` (`lsp_store.rs`)
- **URL**: https://github.com/zed-industries/zed/tree/main/crates/lsp
- **License**: GPL-3.0
- **Relevant for rift**: The `LspStore` entity exists in both `LocalLspStore` and `RemoteLspStore` variants behind a unified interface — the **dual-mode trait pattern** that rift needs throughout for "same code, local or remote". Also the `TrustedWorktrees` mechanism gating LSP execution by per-host trust state.
- **Key files**: `crates/lsp/`, `crates/project/src/lsp_store.rs`, `crates/project/src/trusted_worktrees.rs`.

#### 5. gluon-lang/lsp-types
- **URL**: https://github.com/gluon-lang/lsp-types ; https://crates.io/crates/lsp-types
- **License**: MIT
- **Status**: **Effectively stalled** — v0.97.0 (latest of 71 versions) listed on crates.io as "about 1 year ago" by Markus Westerlind; tower-lsp-community has forked it as `ls-types` with planned codegen rewrite.
- **Relevant for rift**: Currently still the only realistic dependency for typed LSP messages (everyone uses it transitively via helix-lsp/async-lsp/tower-lsp). Plan migration path; watch `tower-lsp-community/ls-types`.

#### 6. tower-lsp / tower-lsp-community
- **URL**: https://github.com/ebkalderon/tower-lsp (original, stalled — v0.20.0 listed on crates.io as "by Eyal Kalderon over 2 years ago") ; https://github.com/tower-lsp-community/tower-lsp-server (active fork, v0.23.0/v0.21.0)
- **License**: MIT OR Apache-2.0
- **Relevant for rift**: For writing LSP **servers**, not clients. Only relevant if rift later wants to expose its agent context as an LSP server to other editors.

### Category 8: Remote Development & SSH

#### 1. Zed `crates/remote` + `crates/remote_server` (TOP PRIORITY reference)
- **URL**: https://github.com/zed-industries/zed/tree/main/crates/remote ; https://github.com/zed-industries/zed/tree/main/crates/remote_server
- **License**: GPL-3.0
- **Relevant for rift — read these first**:
  - **`RemoteConnection` trait** abstracts SSH/WSL/Docker transports (`crates/remote/src/transport/ssh.rs`, `wsl.rs`, `docker.rs`).
  - **SSH ControlMaster reuse** (one persistent socket, multiplexed channels) on Unix (`ssh.rs:155-188`); Windows fallback uses a `ZED_SSH_CONNECTION_ESTABLISHED` magic string (`ssh.rs:202-230`).
  - **Auto-deploy versioned daemon**: `parse_platform` from `uname -sm`, `echo $SHELL` for shell discovery, downloads `zed-remote-server-<channel>-<version>` to `remote_server_dir_relative()` (`wsl.rs:169-210`, `ssh.rs:112-138`).
  - **Daemon-as-proxy mode**: reconnects to existing daemon if running, starts otherwise — survives connection drops.
  - **`HeadlessProject`** is the server-side `Project` mirror containing `WorktreeStore`, `BufferStore`, `LspStore`, `GitStore`, `DapStore`, etc. (`crates/remote_server/src/headless_project.rs`). This is the structure of rift's daemon.
- **Verbatim from Zed's blog**: *"Once we've established the connection and installed the remote server, we initialize it as a daemon, so that when connections do drop the remote server continues running and on reconnect your language servers are still fully initialized. We also back up any unsaved changes locally, so you never lose your work."*
- **Usable as dependency?**: No (Zed-coupled, GPL-3.0) — read as the implementation reference.

#### 2. lapce-proxy (see Category 7 too)
- **URL**: https://github.com/lapce/lapce/tree/master/lapce-proxy
- **License**: Apache-2.0
- **Relevant for rift**: An alternative, simpler reference for the same architecture as Zed's `remote_server` — `lapce-proxy` runs on the remote host and handles file I/O, terminal, LSP, plugins. Lapce founder's blog post is explicit about *why* this architecture (network latency in tightly bound editing engines is fatal, so UI stays local).
- **Key files**: `lapce-proxy/src/proxy.rs`, `lapce-proxy/src/dispatch.rs`, install scripts `extra/proxy.sh` and `extra/proxy.ps1`.

#### 3. Eugeny/russh
- **URL**: https://github.com/Eugeny/russh
- **License**: Apache-2.0
- **Tech stack**: Pure-Rust async SSH client/server (fork of Thrussh)
- **What it does**: Already a rift dependency.
- **Relevant for rift**: Read `russh/examples/client_exec_interactive.rs` and `client_pty.rs` for the canonical PTY-allocation flow used by Yazi's SFTP provider, kartoffels (ratatui-over-SSH game), ferrissh (network device automation), and HexPatch. The reverse-forwarding example (Sandhole project) is also relevant if rift ever wants the daemon to call back into the GUI client.
- **Usable as dependency?**: **Yes — already in use.**

#### 4. penso/arbor outposts (see Category 4)
- **Relevant for rift**: Working SSH + mosh implementation in Rust+GPUI for the exact "remote agent host with reattachable daemon" pattern rift wants.

#### 5. simple_ssh and russh-extra (third-party russh wrappers)
- **URL**: https://crates.io/crates/simple_ssh ; https://github.com/franckcl1989/russh-extra
- **License**: (verify per crate)
- **Relevant for rift**: Higher-level builder APIs over russh — useful study for the kind of API surface rift might want to expose internally to keep SSH plumbing out of business logic. `simple_ssh::Session::init().with_host().with_user().pty_builder()...` is a clean shape.

### Category 9: Developer Tools with Unique UX Patterns

#### 1. zellij-org/zellij (TOP PRIORITY — UX)
- **URL**: https://github.com/zellij-org/zellij
- **License**: MIT
- **Tech stack**: Rust workspace + WASM plugin runtime (wasmi) + vte parser
- **Relevant for rift**:
  - **Floating panes**: terminals that overlay the tiled layout, persist across tab switches, toggle via `Ctrl+p w`, can embed/escape via `Ctrl+p e`. Direct candidate for rift's "floating agent pane on top of editor" UX.
  - **Discoverability status bar**: hotkey hints render permanently — pattern rift's GPUI overlay could adopt.
  - **WASM plugin model**: tab-bar, status-bar, session-manager are all WASM plugins via Zellij's own runtime — a future-proofing pattern for rift's plugin system if it outgrows cargo-feature crates.
  - **Per-client state partitioning**: `active_panes` keyed by `ClientId` for multi-attach with independent focus — exactly the multi-client semantics rift's daemon needs.
- **Key files**: `zellij-server/src/screen.rs`, `zellij-server/src/tab/mod.rs` (TiledPanes/FloatingPanes/SuppressedPanes), `zellij-server/src/tab/layout_applier.rs`.

#### 2. wezterm (see Category 2)
- **Relevant for rift**: The `Pane` trait + `Domain` trait abstractions are a UX/architecture combo: panes can transparently be local PTYs or remote multiplexer-backed sessions — a model rift can adopt to unify "local terminal" and "tmux pane" rendering.

#### 3. penso/arbor (see Category 4) — picture-in-picture style worktree switching, terminal-bell-aware notifications.

#### 4. superconductor (see Category 4) — native Rust, picture-in-picture pane, configurable keybindings, per-workspace theming, 15 built-in notification sounds.

#### 5. yazi (see Category 5) — async-everywhere UX is a North Star: the UI never blocks while file ops happen; preloading by adapter; mouse + keyboard parity.

---

### Category 10: Logging & Diagnostics

How the reference projects solve debug logging for GUI apps (console vs file, rotation, panic capture, surfacing logs to the user). All findings verified against current default-branch source (2026-06). Notably, all four roll a custom logger over the `log` facade — none uses `tracing-subscriber` file sinks — but the *patterns* transfer directly to rift's existing `tracing` setup.

#### 1. zed-industries/zed — `zlog` (TOP PRIORITY)
- **Key files**: `crates/zlog/src/{zlog,sink,filter,env_config}.rs`, `crates/zlog_settings/`, `crates/zed/src/main.rs`, `crates/zed/src/zed.rs` (`open_log_file`), `crates/crashes/src/crashes.rs`
- **Sink decision by TTY detection, not build profile**: `if stdout_is_a_pty() { init_output_stdout() } else { init_output_file(log_file, Some(old_log_file)) }` — a windowed exe has no TTY and gets the file sink automatically; a terminal launch gets the console. `ZED_FORCE_CLI_MODE` env var covers the spawned-from-CLI case.
- **Size-based rotation, two files**: `Zed.log` + `Zed.log.old` in the app data dir (`%LOCALAPPDATA%\Zed\logs` on Windows). At 1 MB the current log is copied to `.old` and truncated; an `AtomicU64` + `SizedWriter` wrapper tracks bytes. File opens `append(true)` — history survives restarts.
- **Filtering**: `ZED_LOG` falling back to `RUST_LOG`, comma-separated `module=level` directives; **runtime reload** via a settings-store observer (`zlog_settings`) — live filter changes without restart.
- **Panic capture**: panic hook logs `thread '{name}' panicked at {location}` via `log::error!` (lands in the log file), then hands off to a minidump crash-handler subprocess; crash JSON + `.dmp` land next to the logs and are uploaded on next start.
- **Surfacing**: `zed: open log` action concatenates `.old` + current, keeps the last 1000 lines, opens them in an editor buffer with log syntax highlighting.

#### 2. wezterm — `env-bootstrap/src/ringlog.rs`
- **Three sinks simultaneously, always on (all profiles)**: (1) per-level **ring buffers** (16 entries each) feeding the in-app debug overlay (`Ctrl+Shift+L`, `wezterm-gui/src/overlay/debug.rs`) — log access with zero console and zero file I/O; (2) stderr with ANSI only when a TTY; (3) a **lazily created per-run file** `<exe>-log-<pid>.txt` in the runtime dir.
- **No size rotation — per-run PID files + age pruning**: files older than 7 days are deleted, but only on GUI startup ("cli commands should have as low startup overhead as possible").
- **Filtering**: `WEZTERM_LOG` parsed with `env_logger::filter` (the parser without the logger); default Info plus hardcoded per-module suppression of noisy deps (`wgpu_core`, `wgpu_hal`, `zbus` → Error) — directly relevant for rift's wgpu/GPUI noise.
- **Panic capture**: hook routes message + `backtrace::Backtrace` through `log::error!` → lands in ring buffer and file, then delegates to the default hook.

#### 3. alacritty — `alacritty/src/logging.rs`
- **Lazy file creation + user notification on error**: per-run `$TMPDIR/Alacritty-<pid>.log` created **only on first write**; path exported as `$ALACRITTY_LOG` env var so child shells can `cat` it; on Warn/Error the GUI **message bar** shows `"[ERROR] ...\nSee log at $ALACRITTY_LOG"` — the user is told where the log is exactly when something goes wrong, and a silent run creates no file at all.
- **Cleanup instead of rotation**: RAII guard deletes the log on clean exit unless `debug.persistent_logging = true`; a crash leaves the file behind.
- **Filtering**: no `RUST_LOG` — CLI `-v/-vv/-vvv` / `-q/-qq` plus a target allowlist (only own crates log below Trace), extra targets via `ALACRITTY_EXTRA_LOG_TARGETS`.
- **Windows console trick**: `AttachConsole(ATTACH_PARENT_PROCESS)` at start / `FreeConsole()` at exit — the windowed exe writes to the parent's console when launched from a terminal.

#### 4. helix — `helix-term/src/logging.rs`
- **Minimal zero-dep logger** (~120 lines over the `log` facade; replaced fern+chrono to cut dependencies): file-only sink, append forever, no rotation; `~/.cache/helix/helix.log`, overridable via `--log <path>`; CLI `-v` repeats map to Info/Debug/Trace.
- **Panic pattern (TUI-specific)**: no panic-to-logfile; instead an eager terminal-reset hook so the default stderr backtrace stays readable.

**Takeaways for rift**: keep the existing `tracing`/`EnvFilter` facade — the gaps are sink strategy, rotation, and the daemon. (1) Zed's TTY-detection beats the current `windowed`-feature gate: it handles dev console, windowed stable, and redirected output with one runtime check. (2) Zed's `.log`/`.log.old` 1 MB pair beats per-run truncation (current `rift-stable.log` loses the previous run's evidence) and beats unbounded append; note `tracing-appender` only rotates by *time* (minutely/daily/never), not size — size rotation needs a small custom writer like Zed's `SizedWriter` (~50 lines). (3) The daemon's stdout carries protocol frames; stderr is its log sink — wezterm's per-run PID-keyed files with age pruning is the model if the daemon ever needs a file sink on the remote host. (4) Panic-into-log via `log::error!`/`tracing::error!` in the hook (Zed, wezterm) — rift's stable build already does this; extend it to all sinks. (5) Surfacing: Alacritty's "tell the user where the log is, only on error" and wezterm's ring-buffer debug overlay are cheap, high-value follow-ups once a status/message surface exists.

---

## Priority reference projects (top 10)

1. **penso/arbor** — Closest existing implementation of rift's exact concept (Rust + GPUI + daemon + SSH outposts + agent state). Read end-to-end before writing any architecture docs.
2. **zed-industries/zed** (`crates/remote`, `crates/remote_server`, `crates/workspace`) — Gold-standard reference for daemon-with-auto-deploy, SSH ControlMaster reuse, HeadlessProject pattern.
3. **longbridge/gpui-component** — UI primitives rift should depend on directly (dock, virtualized list, table, theme, scrollbar, settings, code editor).
4. **helix-editor/helix** (`helix-lsp`) — Cleanest extractable LSP client with multi-server-per-doc registry; MPL-2.0 compatible.
5. **lapce/lapce** (`lapce-proxy`) — Simpler, more readable alternative to Zed's remote architecture; explicit "LSP runs on the remote" design.
6. **lassejlv/termy** — Reference wiring of GPUI + alacritty_terminal; same exact stack as rift's terminal layer.
7. **smtg-ai/claude-squad** — Most-mature production tmux-based agent session manager; read for tmux capture-pane usage and Instance state machine.
8. **Auto-Explore/GitComet** + **smolcars/hunk** — Two GPUI-native git diff implementations with virtualization patterns proven on Chromium-scale repos.
9. **horang-labs/tessera** + **Nimbalyst/nimbalyst** — The `CliProvider`/`EditorHost` trait shapes for cross-agent normalization; borrow the trait, ignore the Electron.
10. **zellij-org/zellij** — Floating panes, discoverability status bar, per-client state partitioning — UX patterns rift can adopt directly.

## Potential dependencies

Crates rift could pull in directly (license-compatible with GPL-3.0):

| Crate | Source | License | Use |
|---|---|---|---|
| `gpui`, `gpui_platform` | `zed-industries/zed` (git) | Apache-2.0 | UI framework (already in use) |
| `gpui-component` (+ `gpui-component-assets`) | `longbridge/gpui-component` (git) | Apache-2.0 | Dock, virtual list/table, theme, scrollbar |
| `alacritty_terminal` | `alacritty/alacritty` | Apache-2.0 | Terminal grid + VTE parsing (already in use) |
| `russh` (+ `russh-sftp`) | `Eugeny/russh` | Apache-2.0 | SSH transport (already in use) |
| `async-lsp` | `oxalica/async-lsp` | MIT OR Apache-2.0 | LSP client framework (recommended primary) |
| `lsp-types` | `gluon-lang/lsp-types` | MIT | LSP wire types (with migration plan to `ls-types`) |
| `helix-lsp` | `helix-editor/helix` (git dep on workspace) | MPL-2.0 | Alternative full LSP client |
| `notify` | `notify-rs/notify` | MIT/Apache-2.0 | File watching |
| `git2` | `rust-lang/git2-rs` | Apache-2.0/MIT | Git operations |
| `gix` | `Byron/gitoxide` | Apache-2.0/MIT | Pure-Rust git (used by GitComet and Hunk) |
| `tmux_interface` | `AntonGepting/tmux-interface-rs` | MIT | Typed tmux command builders |
| `portable-pty` | `wezterm/wezterm` (sub-crate) | MIT | Cross-platform PTY abstraction |
| `nucleo` | `helix-editor/nucleo` | MPL-2.0 | Fuzzy matching (used by Nexus Explorer) |
| `jwalk` | `Byron/jwalk` | MIT | Parallel directory traversal |

Crates inside monorepos to study (likely fork-and-adapt rather than direct dep):
- `zed/crates/remote`, `zed/crates/remote_server`, `zed/crates/lsp`, `zed/crates/project_panel`, `zed/crates/git`, `zed/crates/terminal`, `zed/crates/workspace`
- `lapce/lapce-proxy`, `lapce/lapce-rpc`
- `helix-editor/helix/helix-lsp`, `helix-editor/helix/helix-vcs`
- `wezterm/{term, mux, wezterm-client, wezterm-mux-server}`

## Architecture patterns to adopt

These patterns are validated by 2+ independent implementations:

1. **Daemon with auto-deploy + ControlMaster multiplexing** — single persistent SSH socket, daemon spawned on first connect, reattached on subsequent connects, versioned binary uploaded to remote when missing or outdated. *Validated by:* Zed (`crates/remote/src/transport/ssh.rs`), Lapce (`lapce-proxy/src/remote.rs` + `extra/proxy.sh`), Arbor (`arbor-ssh`), VS Code Remote.
2. **Local/Remote dual-mode trait pattern** — same high-level entity (`Project`/`Worktree`/`LspStore`/`GitStore`) implemented as `Local…` or `Remote…` behind a unified interface; UI code never knows the difference. *Validated by:* Zed (`crates/project/src/project.rs`, `lsp_store.rs`), Lapce (UI vs proxy split), Arbor (daemon-backed UI/web/CLI/MCP).
3. **Headless project as server-side mirror** — daemon owns a "project" entity holding all stores; clients are thin RPC proxies; multiple clients can attach with independent focus. *Validated by:* Zed `HeadlessProject`, Zellij's per-`ClientId` state, tmux multi-attach itself, Arbor's daemon-backed multi-client.
4. **Provider/CliProvider/Domain trait for agents and PTY backends** — one trait per agent CLI (or per local-vs-remote PTY) handling lifecycle, protocol parsing, approvals, interrupts; a normalization layer translates per-protocol events into a shared message model. *Validated by:* Tessera's `CliProvider` contract, Nimbalyst's `EditorHost`, WezTerm's `Pane`/`Domain` traits, ComposioHQ's plugin slots, Arbor's provider configs.
5. **Tmux as session substrate + control mode for awareness** — agents in vanilla tmux panes; the GUI is an iTerm2-style control-mode client that consumes `%output`/`%layout-change`/`%window-add` and emits commands. *Validated by:* iTerm2 (original consumer), tmux upstream control mode, Claude Squad's tmux session per agent, workmux's worktree/pane coupling.
6. **Virtualized rendering for huge lists** — render only visible items, reuse UI element pool while scrolling; mandatory for 100k+ files, 500k+ line diffs, deep git histories. *Validated by:* GitComet (history + diff), Hunk (perf harness on 25k-line diffs), gpui-component `VirtualList`, Nohrs file explorer, Zed `project_panel`.
7. **Three-task LSP stdio transport** — `recv` task reads framed JSON-RPC from server stdout into a notification channel and pending-request hashmap; `send` task serializes outgoing payloads with `Content-Length` headers; `err` task logs stderr. *Validated by:* Helix `helix-lsp/src/transport.rs`, async-lsp `MainLoop`, Lapce `lapce-proxy/src/plugin/lsp.rs`.
8. **Multi-server-per-document via Registry** — `HashMap<LanguageServerName, Vec<LanguageServerId>>` so linter + type-checker + formatter (e.g. ruff + pyright + black) can all attach to the same buffer; aggregate diagnostics keyed by server. *Validated by:* Helix Registry, Lapce `DocumentSelector` routing, Zed `LspStore`.
9. **Per-pane state machine with bell/activity awareness** — each agent pane tracks states (working/waiting/idle), surfaced via WebSocket-streamed events to all clients; terminal bell integrated as a state signal. *Validated by:* Arbor's working/waiting indicators, Claude Squad's `Instance` struct, Superconductor's notification system.
10. **Modular Cargo workspace with one crate per subsystem** — terminal/mux/client/server/GUI split so headless and GUI modes are first-class and the same binary can run with or without a window. *Validated by:* WezTerm (19+ crates), Hunk (hunk-git/hunk-codex/hunk-terminal/hunk-text/hunk-language), Arbor (arbor-ssh/arbor-symphony/arbor-mcp/arbor-cli), Lapce (lapce-app/lapce-proxy/lapce-rpc).

## Recommendations

**Phase 1 — Foundations (current sprint):** Adopt `longbridge/gpui-component` as the UI primitive layer now, before building any more custom tab/dock/list code; the project ships exactly what the rift README enumerates as TODOs (window tabs, dock, splits, theming). Read **lassejlv/termy** end-to-end before extending rift's terminal layer further — it is the only other project using the GPUI + alacritty_terminal combination and will save weeks of GPUI IME and Windows ConPTY debugging.

**Phase 2 — Daemon and remote (next):** Before writing the daemon, do a full read of `zed/crates/remote_server/src/headless_project.rs` and `zed/crates/remote/src/transport/ssh.rs`, plus `lapce/lapce-proxy/src/proxy.rs`. Decide explicitly whether rift mirrors Zed's `HeadlessProject` (heavy, entity-per-subsystem) or Lapce's flatter dispatch. Use `russh` as already planned; lift the ControlMaster + auto-deploy pattern verbatim from Zed.

**Phase 3 — File explorer + git:** Read **Augani/nexus-explorer** for the GPUI + jwalk + notify + nucleo wiring, **noh-rs/nohrs** for the smallest GPUI tree-view skeleton, and **Auto-Explore/GitComet** + **smolcars/hunk** for diff virtualization on huge files. Skip building your own fuzzy matcher — depend on `nucleo`.

**Phase 4 — LSP:** Choose between `async-lsp` (recommended — actively maintained, client-first, Tower middleware, MIT/Apache) and forking `helix-lsp` (MPL-2.0, multi-server registry already implemented). Validate by getting `rust-analyzer` diagnostics through `async-lsp`'s `client_builder.rs` example end-to-end in a one-day spike before committing. Plan migration of `lsp-types` → `tower-lsp-community/ls-types` over the next ~12 months as the latter stabilizes.

**Phase 5 — Plugin system for agent awareness:** Model rift's plugin trait after **Tessera's `CliProvider`** (process lifecycle + protocol parsing + approvals + interrupts) and **Nimbalyst's `EditorHost`** (capability flags like `supportsTranscriptEmbed`). Implement Claude Code first (stream-json), Codex second (app-server JSON-RPC), OpenCode third (ACP JSON-RPC) — that order matches the maturity of available references.

**Benchmarks/thresholds that would change recommendations:**
- If `async-lsp`'s diagnostic latency p99 > 50ms with rust-analyzer on a 100k-LOC repo, switch to forking `helix-lsp`.
- If `gpui-component`'s `VirtualList` cannot sustain 60fps on a 100k-entry file tree, fork it or fall back to a custom recycler.
- If tmux control-mode parsing becomes a maintenance burden in the daemon, evaluate WezTerm's mux RPC protocol as an alternative substrate (drop tmux, ship a WezTerm-mux client mode).
- If `lsp-types` is not refreshed by Q4 2026, migrate to `tower-lsp-community/ls-types` even if migration costs are non-trivial.

## Caveats

1. **GPUI is still pre-1.0 and ships from git, not crates.io.** Both rift and every dependency listed (gpui-component, hunk, arbor) pin to specific commits. Expect breaking-change churn.
2. **Tessera and Nimbalyst are TypeScript/Electron.** Their value to rift is purely architectural (the `CliProvider` trait shape and `EditorHost` contract). Do not attempt to consume them as runtime dependencies.
3. **Superconductor's open-source status is partial.** The user-facing app is downloadable for macOS and the GitHub org hosts a plugin marketplace and a fork of Codex, but the core app source layout is not fully clear from the repos visible at `github.com/Superconductor`. Treat as a UX reference, not a code reference, until the source is confirmed fully open.
4. **Arbor is described in some sources as ~1 week old and from a solo developer.** It is genuinely the closest precedent for rift, but its API surface may shift. Pin to a tag if depending on any crate; prefer reading and adapting.
5. **Claude Squad is AGPL-3.0**, which would be a viral license for any code copied into rift. Read for pattern only; do not copy code verbatim.
6. **`lsp-types` is effectively unmaintained** (v0.97.0 listed on crates.io as "about 1 year ago"). The tower-lsp-community fork `ls-types` is planning a codegen rewrite. Track the migration; do not let `lsp-types` choices lock rift into outdated LSP versions.
7. **`tmux_interface` is at v0.3.2 with last release in 2024** (~2 years ago, pre-1.0). It is fine as a typed CLI command builder but does not parse control-mode notifications — rift must write that parser itself.
8. **tmux control mode has no formal spec.** Authoritative sources are the tmux wiki, the man page, iTerm2 docs, and `control.c` in tmux/tmux. Plan to read source and write conformance tests against real tmux.
9. **Zed crates are GPL-3.0.** rift is also GPL-3.0 so this is fine for copying patterns and code, but it forecloses any future relicensing of rift to a permissive license without rewriting those subsystems.
10. **Auto-Explore/GitComet's exact license needs verification on the repo** before adopting any code — the marketing site advertises "open source" and a future Professional edition, which can sometimes signal source-available rather than OSI-approved licensing.
