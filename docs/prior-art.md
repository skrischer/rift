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

### Category 11: Host Resource Monitoring

Prior art for reading host resource state (CPU / memory / swap / load / disk, plus
per-process attribution) from the daemon on the remote host, and surfacing it
reactively in the cockpit — never by embedding a separate monitor. All verified
2026-07 (websearch). Backs roadmap Phases 43–46.

#### 1. GuillaumeGomez/sysinfo (TOP PRIORITY — the dependency)
- **URL**: https://crates.io/crates/sysinfo ; https://docs.rs/sysinfo
- **License**: MIT
- **What it does**: cross-platform `System` / `Process` / `Disks` API; on Linux it reads `/proc`. Exposes total / used / available memory, total / used swap, load average, per-CPU usage, and per-process RSS + CPU% + parent PID.
- **Relevant for rift**: the daemon's metrics source for Phase 43 (host CPU/RAM/swap/load), Phase 45 (per-PID RSS/CPU + parent PID for pane attribution), and Phase 46 (`Disks` for disk headroom) — no hand-rolled /proc parser. Caveat: instantiate `System` once and **reuse** it; CPU% needs two samples spaced by a refresh interval (`MINIMUM_CPU_UPDATE_INTERVAL`), so the daemon's sampler holds state across ticks.
- **Usable as dependency?**: **Yes** — MIT, `cargo deny`-clean; listed in Potential dependencies below.

#### 2. ClementTsang/bottom (btm)
- **URL**: https://github.com/ClementTsang/bottom
- **License**: MIT (Rust)
- **Relevant for rift**: reference for metric selection, sampling cadence, and time-series / sparkline history widgets (Phase 46). A polished Rust /proc consumer to read for "what to show and how often" without adopting its TUI.

#### 3. aristocratos/btop
- **URL**: https://github.com/aristocratos/btop
- **License**: Apache-2.0 (C++)
- **Relevant for rift**: reference for the process-tree parent/child view — the model for Phase 45's per-pane subtree roll-up and Phase 46's detail panels. UX only (C++, not a dependency).

#### 4. Linux PSI — Pressure Stall Information
- **URL**: https://docs.kernel.org/accounting/psi.html ; systemd-oomd ; Netdata PSI collector
- **What it provides**: `/proc/pressure/{cpu,memory,io}` — the share of walltime tasks are stalled on a resource, averaged over 10s / 1m / 5m; plus a trigger interface (write e.g. `some 150000 1000000`, then poll the fd) for early warning before an OOM. systemd-oomd and Netdata both act on PSI to catch memory death-spirals before the kernel OOM.
- **Relevant for rift**: Phase 44's optional precision enhancement. **Reality-check: absent on the stock `microsoft-standard-WSL2` kernel (`CONFIG_PSI` off — confirmed live), so `/proc/pressure` cannot be the primary signal.** Gate on file existence; the portable `MemAvailable` / swap / load baseline is the guaranteed path.

#### 5. YlanAllouche/tmux-task-monitor
- **URL**: https://github.com/YlanAllouche/tmux-task-monitor
- **License**: MIT
- **Relevant for rift**: the exact precedent for Phase 45 — CPU / MEM of the child processes of tmux panes, grouped by window / session, via `pane_pid` → /proc children (`/proc/$pid/task/$tid/children`, recursive). Confirms the attribution method; reference only (a TUI popup plugin).

#### What to AVOID
- **Embedding or shelling out to a TUI monitor** (btop / htop / glances) in a pane — that IS the "open the Task Manager" context-switch this milestone removes; rift renders the metrics natively in the cockpit.
- **Any agent-name-based labeling** of resource usage — Phase 45's pane label comes from `pane_current_command` / `pane_title`, never agent detection (constitution: "No agent detection").

---

## v1.0.0 cockpit phases — prior-art index (Phases 10–17)

Per-phase prior-art for the v1.0.0 "agent cockpit" roadmap block
([roadmap.md](roadmap.md)), so `/loopkit:plan` can resolve "prior art for phase N"
directly. Most entries point into the categories above; the three marked **(new)**
are added here.

| Phase | Concern | Reference (repo + path) | License | Verdict |
|---|---|---|---|---|
| 10 | Dock + resizable panel shell | `longbridge/gpui-component` `crates/ui/src/dock/`; `zed` `crates/workspace/src/dock.rs` | Apache-2.0 / GPL-3.0 | reuse (gpui-component Dock) / reference (Zed dock+pane tree) |
| 11 | Explorer panel (decoration, file ops, reveal, keyboard nav) | `zed` `crates/project_panel/src/project_panel.rs` | GPL-3.0 | reference — mirror the precomputed `EntryDetails` cache (one pass per data update, not per frame), the action set, and the git/diagnostic severity roll-up onto ancestor dirs |
| 12 | Source-control panel + visual diff | `Auto-Explore/GitComet`; `smolcars/hunk` `crates/hunk-git`; `zed` `crates/git` | mixed / GPL-3.0 / GPL-3.0 | reference — diff virtualization on large files; needs a new daemon diff capability |
| 13 | Problems panel (project-wide diagnostics) **(new)** | `zed` `crates/diagnostics/src/diagnostics.rs` | GPL-3.0 | reference — grouped-by-file diagnostics list, jump-to-location, severity sort; rift's data already streams via `Diagnostics` |
| 14 | Status bar (branch, ahead/behind, counts) **(new)** | `zed` `crates/status_bar`; `zellij` status-bar plugin (`default-plugins/status-bar`) | GPL-3.0 / MIT | reference — status-item registration slots (left/right), discoverability hints; reads the existing `RepoState` + diagnostics model |
| 15 | Editor tabs (multiple open files) | `zed` `crates/workspace` (`Item`/`Pane`); `longbridge/gpui-component` `Tab`/`TabBar` | GPL-3.0 / Apache-2.0 | reuse (gpui-component Tab — already used for terminal windows) / reference (Item open/close/dirty lifecycle) |
| 16 | Command palette **(new)** | `zed` `crates/command_palette/src/command_palette.rs` | GPL-3.0 | reference — fuzzy picker over registered GPUI actions. `nucleo` pairs with a *file* quick-open (post-v1.0.0); the v1 command palette uses a small subsequence match over a curated registry (no new dependency) — see `spec-command-palette.md` |
| 17 | Theme & settings | `longbridge/gpui-component` Theme/ThemeRegistry; `zed` `crates/settings` | Apache-2.0 / GPL-3.0 | reuse (gpui-component theming — already vendored) / reference (hierarchical settings store) |

All licenses are GPL-3.0-compatible (rift is GPL-3.0-or-later). The Zed crates are
study-only (tightly coupled to Zed's `Project`/`Workspace`); gpui-component Dock,
Tab, and theming are direct dependencies already vendored.

## Pane activity state on window tabs (Phase 18)

> Concern: per window tab, show how many panes run a long-lived process and their
> aggregated state (busy / idle / needs-attention), derived using ONLY rift's two
> sanctioned signals — PTY bytes and the terminal bell — never by parsing agent
> output. Backs roadmap Phase 18. Research: 2026-07 (websearch).

The idea arrives phrased as "how many panes run a Claude Code session", which would
require agent detection — forbidden by the constitution (`No agent detection`) and
vision (*"grepping the non-doc codebase for agent names returns nothing"*). The
agnostic reframe is "how many panes are actively producing output / bell", which
works identically for any agent (Claude Code, Codex, Gemini CLI) or a `cargo watch`.

| Concern | Reference (repo + path) | License | Verdict |
|---|---|---|---|
| Agnostic activity / idle / bell alerts | tmux `monitor-activity` / `monitor-silence` / `monitor-bell` options + control-mode notifications and `alert-activity`/`alert-silence`/`alert-bell` hooks ([Control Mode wiki](https://github.com/tmux/tmux/wiki/Control-Mode), man page "MONITORING") | ISC | **reuse** — tmux itself derives activity/silence/bell purely from PTY bytes (BEL for bell); consume them over the control-mode stream rift already parses. This is the agnostic substitute for content inspection. |
| Per-pane agent-state UX (color-coded dots + aggregate) | `penso/arbor` working/waiting indicators; `smtg-ai/claude-squad` `session/tmux/` `Instance` state machine; `zellij` status-bar plugin | MIT / AGPL-3.0 / MIT | **reference (UX only)** — adopt the color-coded dot + per-window aggregate roll-up. Explicitly AVOID Arbor's hardcoded agent detection and Claude-Squad's `capture-pane` content hashing. |
| What to AVOID (agent-specific detection) | `samleeney/tmux-agent-status` (spinner-glyph + "waiting"-prompt parsing of `capture-pane -p`); Claude-Code hooks stamping a `@claude_state` tmux option | — | **avoid** — both are agent-specific: parsing spinner/prompt glyphs out of pane content, or requiring the agent's own hook config. Violates `No agent detection` and the vanilla-agents principle. |

Open design decision, deferred to Phase 18's `/loopkit:plan` spec: cleanly separating
"waiting for input / needs attention" from "idle at a shell prompt" purely
agnostically. tmux exposes activity/silence/bell, not "blocked on stdin" — the bell
is the cleanest agnostic attention proxy; whether it suffices, or a silence-after-
activity heuristic is added, is a spec-time call, not a roadmap guess.

## v1.0 polish + robustness phases — prior-art index (Phases 19–26)

Per-phase prior-art for the v1.0 polish cut ([roadmap.md](roadmap.md)), same
shape as the v1.0.0 index above. Research mode: none (2026-07-05) — every
concern resolves against entries already catalogued above; no fresh research.

| Phase | Concern | Reference (repo + path) | License | Verdict |
|---|---|---|---|---|
| 19 | tmux session list/switch in control mode | tmux Control Mode (Category 3 #1): `list-sessions` under `%begin/%end` guards, `%sessions-changed`/`%session-changed`/`%session-renamed` notifications; iTerm2 tmux integration (Category 3 #5) session-picker UX | ISC | **reuse** (notifications + commands over the existing control stream) / **reference** (iTerm2's attach-per-session UX). AVOID `choose-tree` — control clients never see rendered chooser UIs; list via commands only |
| 19 | Parallel-attach client model | rift's own per-client control children (`crates/daemon/src/terminal.rs`) | — | greenfield — extend the existing per-client child to per-(client, session); no external precedent needed |
| 20 | Versioned daemon + skew recovery | `zed` `crates/remote` + `crates/remote_server` (Category 8 #1): versioned binary upload + reconnect; VS Code Remote server/client handshake | GPL-3.0 | **reference** — adopt "the client owns the daemon version": Hello/Welcome carries a message-set version token; mismatch → atomic replace + pidfile restart (the spec-daemon-redeploy mechanism) |
| 20 | Reconnect loop + connection screen | `zed` remote reconnect flow (banner + bounded retry); design artboards Connection — Startup / alert banners | GPL-3.0 | reference — visible reconnect state; never quit on drop |
| 21 | Title bar / activity rail / tab chrome | `longbridge/gpui-component` TitleBar, Dock, Tab, Badge (Category 1 #1); `zed` `crates/title_bar` | Apache-2.0 / GPL-3.0 | **reuse** (gpui-component widgets, already vendored) / reference |
| 22 | Composite status line | `zed` `crates/status_bar`; `zellij` status bar (Category 9 #1); rift's own statusline mirror (`spec-tmux-statusline-mirroring.md`) | GPL-3.0 / MIT | reference — ONE bar composing native segments + the tmux window list; supersedes the env-gated either/or between native and mirrored modes |
| 23 | Editor chrome (breadcrumb, hover card, references/outline, minimap) | `zed` `crates/editor` (hover popover, breadcrumbs), `crates/outline_panel` (Category 1 #2); gpui-component code-editor story | GPL-3.0 / Apache-2.0 | reference — anatomy only; rift renders its own chrome from the existing nav/diagnostics streams |
| 24 | Git write path (stage/unstage/commit, hunks) | `gix` (Byron/gitoxide) staging + commit APIs; `zed` `crates/git_ui`; `gitui` staging UX (Category 6 #3) | Apache-2.0 OR MIT / GPL-3.0 / MIT | **reuse** (gix — already the daemon's git dependency, musl-clean) / reference (hunk-staging interactions) |
| 24 | Split diff + word-level emphasis | `smolcars/hunk` (Category 1 #3); `Auto-Explore/GitComet` (Category 6 #1) | GPL-3.0 / verify | reference — diff virtualization + intra-line emphasis patterns |
| 25 | Explorer decoration + rollup | `zed` `crates/project_panel` (Category 5 #1) | GPL-3.0 | reference — `EntryDetails` precompute + ancestor git/severity rollup (already cited for Phase 11; the parity work completes it) |
| 26 | Settings shell + theme-driven terminal palette | `longbridge/gpui-component` Settings + Theme/ThemeRegistry (Category 1 #1); `alacritty` config theme→ANSI mapping (Category 2 #3) | Apache-2.0 / Apache-2.0 | **reuse** (widgets + theme registry) / reference (ANSI palette derivation from a theme) |

Open design decisions deferred to each phase's `/loopkit:plan` spec (never a
roadmap guess): phase 19 — whether parallel sessions render as one window with
a session switcher or as multiple OS windows; phase 20 — the exact version
token (protocol-hash vs bumped integer) and whether daemon restart stays
client-driven only; phase 24 — how index-vs-worktree staging semantics surface
in the UI.

## Explorer overhaul — prior-art index (Phases 27–31)

Per-phase prior-art for the explorer overhaul ([roadmap.md](roadmap.md)), same
shape as the indexes above. Research mode: websearch (2026-07-08) — the two
genuinely new concerns (icon-asset embedding, remote file operations) were topped
up with focused lookups; the rest resolves against Category 5 (File Explorers) and
Category 1 already catalogued. All licenses GPL-3.0-compatible.

| Phase | Concern | Reference (repo + path) | License | Verdict |
|---|---|---|---|---|
| 27 | Explorer visual-language redesign | Paper `rift` design file (Cockpit — IDE explorer + Styleguide); `zed` `crates/project_panel` row anatomy (Category 5 #1); `noh-rs/nohrs` + `sxyazi/yazi` GPUI/async tree density (Category 5 #4/#2) | GPL-3.0 / MIT | reference — anatomy + density only; the durable new artboard is authored in Phase 27's `/loopkit:plan` (reviewed at spec-acceptance), not seeded here |
| 28 | File-type icon theme + SVG asset embedding | Zed **icon themes** (JSON schema `default_file`/`default_folder`/`default_folder_open`/`file_types` → bundled `./icons/*.svg`, [docs](https://zed.dev/docs/extensions/icon-themes)); `longbridge/gpui-component` `Icon` element (SVGs **not** bundled by default — the exact gap `file_tree.rs` documented); icon sets: Seti (MIT), Material Icon Theme (MIT), `dmhendricks/file-icon-vectors` (MIT / CC-BY), Lucide (ISC) for chrome glyphs | Apache-2.0 / MIT / ISC | **reuse** (gpui-component `Icon` + a bundled MIT icon set embedded via `rust-embed` / gpui assets) / reference (Zed's icon-theme JSON mapping shape; per-extension icon `default_file` fallback + `file_types` extension map) |
| 29 | Tree context menu | `longbridge/gpui-component` ContextMenu / PopupMenu (Category 1 #1, already vendored); `zed` `crates/project_panel` right-click action taxonomy (Category 5 #1) | Apache-2.0 / GPL-3.0 | **reuse** (gpui-component popup menu) / reference (project_panel's action set — split client-capable actions from write actions, which land with Phase 30, so the menu ships no dead controls) |
| 30 | Remote file operations (create / rename / delete / move) | `zed` remote model — the daemon owns the fs, ops run **daemon-side** not client SFTP (Category 8 #1); rift's own daemon write precedent (Phase 24 git-write + buffer save); `remotefs-ssh` russh backend + `russh-sftp` (reference fallback only — rift's daemon uses `std::fs` on the remote host it already runs on) | GPL-3.0 / MIT-Apache | reference — file ops become new `protocol` messages executed by the daemon with `std::fs`, mirroring Zed's server-side fs; **not** client-side SFTP. Git-aware moves via the existing `gix` dependency |
| 31 | In-panel fuzzy filter + quick-open | `Canop/broot` incremental narrowing UX (Category 5 #5); `helix-editor/nucleo` fuzzy matcher (Potential dependencies); `Augani/nexus-explorer` GPUI + nucleo wiring (Category 1 #5); `zed` `crates/file_finder` | MIT / MPL-2.0 / GPL-3.0 | **reuse** (`nucleo` for matching — already a candidate dep, MPL-2.0-compatible) / reference (broot narrowing, Zed file finder; decide at plan time whether quick-open needs a daemon-side project file index or stays over the streamed tree) |

Open design decisions deferred to each phase's `/loopkit:plan` spec (never a
roadmap guess): phase 28 — which icon set ships and whether icon themes are
user-swappable (Zed-style) or a single bundled set for v1; phase 30 — the exact
file-op message shape, conflict / overwrite semantics, and how a rename racing a
daemon filesystem event is reconciled; phase 31 — whether quick-open indexes the
whole project daemon-side (jwalk) or narrows only the already-streamed tree.

## Visual UI harness — prior-art index (tooling track)

> Concern: give the coding agent eyes on the real GPUI UI (rift + rift-gallery)
> plus deterministic E2E and Paper-design-parity checks. GPUI has **no accessibility
> tree** (AccessKit unintegrated), so a11y/DOM-based external drivers do not apply —
> the harness is screenshot/vision-based, driven in-process. Research: 2026-07
> (websearch). Two phases: (1) a Linux/WSLg gpui headless renderer, (2) the harness.

| Concern (phase) | Reference (repo + path) | License | Verdict |
|---|---|---|---|
| Headless offscreen render → PNG, the macOS blueprint to port (Phase 1) | `zed` `crates/gpui_macos/src/metal_renderer.rs` (`MetalHeadlessRenderer`, `render_scene_to_image`); `crates/gpui/src/platform.rs` (`PlatformHeadlessRenderer` trait) | GPL-3.0 | **reference** — port the offscreen-texture + readback pattern to `gpui_wgpu` (wgpu `copy_texture_to_buffer`); Linux already renders via wgpu, `lavapipe` as the headless adapter |
| gpui fork / pin mechanics (Phase 1) | `rift` [archive/spec-gpui-rev-bump.md](archive/spec-gpui-rev-bump.md) | — | **reference** — the single-`gpui`-invariant + termy-fork precedent; here an **additive** `[patch]` fork on the frozen `4bee412` base (no API churn), single-`gpui` trial mandatory before landing |
| Deterministic in-process UI driving + capture (Phase 2) | `zed` `crates/zed/src/visual_test_runner.rs` (macOS-only today); gpui `TestAppContext` / `VisualTestContext` / `HeadlessAppContext` (`simulate_*`, `dispatch_action`, `run_until_parked`, `capture_screenshot`) | GPL-3.0 | **reference** — the exact drive-then-capture pattern rift needs, ported off-macOS via Phase 1's renderer; steering already works headless, only capture is gated |
| Agent "eyes" — screenshot-driven review (Phase 2) | `microsoft/playwright-mcp` (snapshot vs vision modes); `sethbang/mcp-screenshot-server`; `dddabtc/winremote-mcp` | MIT | **adopt** the vision (screenshot) mode; GPUI has no a11y tree so Playwright's cheaper snapshot mode is unavailable — rift lives in vision mode. A screenshot MCP/recipe (`claude mcp add`) gives Claude eyes directly |
| Screenshot-driven native E2E loop (Phase 2) | Anthropic Computer Use (Xvfb + scrot + `xdotool` reference setup) | — | **reference** — the screenshot→act→screenshot loop; blind coordinate driving is the fallback, in-process `TestAppContext` is preferred (deterministic, needs no a11y) |
| In-app cross-platform screenshot lib, if not shelling out (Phase 2) | `nashaofu/xcap` (X11 + Wayland + Windows); `grim` (WSLg is Wayland, not `scrot`) | MIT | **reference** — optional, only if the harness produces the shot itself instead of Phase 1's headless capture; else `grim` / ImageMagick `import` |
| Paper design-parity comparison (Phase 2) | rift `paper-reviewer` agent (Playwright/DOM path); Paper MCP `get_screenshot` / `get_computed_styles` | — | **adopt** the reporting pattern (design ↔ impl diff) but with a **native** GPUI screenshot; the Playwright/DOM path does not apply to a native app |
| Why not external a11y/DOM drivers (non-goal) | `tauri-pilot` (WebView DOM); FlaUI / WinAppDriver (Windows UIA); AT-SPI / dogtail (Linux) | mixed | **avoid** — all require an accessibility or DOM tree; GPUI exposes none (AccessKit unintegrated), so none can see rift's widgets |

Open design decisions deferred to each phase's `/loopkit:plan` spec (never a
roadmap guess): Phase 1 — whether the fork commit lands as a human-prerequisite
push to `skrischer/zed` or an in-loop step, and whether `lavapipe` renders in
headless WSL or the harness runs on the GPU station (the render probe settles it);
Phase 2 — whether "eyes" ship as a screenshot MCP or a `just` recipe, and whether
CI pixel-baseline diffing is in scope or the harness stays agent-assisted review.

## Session management & post-connect picker — prior-art index (Phases 32–33)

Per-phase prior-art for the session-management block ([roadmap.md](roadmap.md)),
same shape as the indexes above. Research mode: websearch (2026-07-08) — the two
genuinely new concerns (a full session-management surface beyond phase 19's
switch+new, and picking the session AFTER connecting) were topped up with focused
lookups; the control-mode plumbing resolves against Category 3 already catalogued.

| Phase | Concern | Reference (repo + path) | License | Verdict |
|---|---|---|---|---|
| 32 | Glanceable all-sessions surface (see every session at a glance, no click-to-open) | iTerm2 tmux **Dashboard** (Shell > tmux > Dashboard: view all sessions + windows at a glance, rename, switch — the original `-CC` consumer) ([tmux integration](https://iterm2.com/documentation-tmux-integration.html), [menu items](https://iterm2.com/documentation-menu-items.html)); `zellij` session-manager single-screen (create / attach / resurrect) ([tutorial](https://zellij.dev/tutorials/session-management/)) | GPL-2.0 / MIT | **reference (UX)** — iTerm2's Dashboard is the exact "all sessions at a glance + rename + switch" pattern, over the same control-mode stream rift already parses; render it natively from the phase-19 `SessionListReply` (already streamed). AVOID `choose-tree` (invisible to control clients, per the phase-19 spec) |
| 32 | Session operations — rename / kill / new from the UI | tmux `rename-session` / `kill-session` / `new-session` under `%begin/%end` guards (Category 3 #1); iTerm2 `-CC` (`kill-session`, `rename`; closing a tab kills the session); `zellij` session-manager + [`endoze/zellij-switcher`](https://github.com/endoze/zellij-switcher) (switch / create / rename / kill / resurrect, index quick-switch 1–9) | ISC / GPL-2.0 / MIT | **reuse** (tmux commands over the existing correlated-command mechanism; new `protocol` rename/kill messages, same shape as phase-19) / **reference** (zellij's unified create-or-attach screen + per-row kill/rename affordances) |
| 32 | Session reorder | tmux window ops (`swap-window` / `move-window` / `renumber-windows`) as the *window*-level analog — tmux has **no** native session order; `zellij-switcher` index quick-switch (1–9) | ISC / MIT | **greenfield — no external precedent for session reorder**: it is client-side ordering persisted locally (the window-state store pattern, phase 9), not a tmux concept. Closest UX analog is terminal tab drag-reorder; drag-to-order vs pin/favorite is a plan-time call |
| 33 | Post-connect session pick (connect to the host, THEN pick from a live list) | iTerm2 tmux integration — attach the `-CC` server first, the Dashboard / session list then drives which session shows (the pick is post-attach, never a pre-connect requirement) (Category 3 #5); `wezterm` launcher `ShowLauncherArgs { FUZZY\|WORKSPACES }` (fuzzy pick, create-on-select) ([docs](https://wezterm.org/config/lua/keyassignment/SwitchToWorkspace.html)) | GPL-2.0 / MIT | **reference** — rift already runs `QuerySessionList` post-connect (phase 19); this phase moves the *pick* ahead of the cockpit committing to a session, mirroring iTerm2's attach-server-then-Dashboard flow |
| 33 | De-hardcode the fixed default session + optional session on connect | rift's own Connection screen (`crates/app/src/connection_screen.rs` `DEFAULT_SESSION`) + connect pipeline (`crates/app/src/main.rs`) | — | greenfield — refactor: make the Session field optional / default-less so the post-connect picker (not a baked `"rift"`) resolves the session; no external precedent needed |

Open design decisions deferred to each phase's `/loopkit:plan` spec (never a
roadmap guess): phase 32 — whether the glanceable surface replaces or complements
the phase-21 title-bar popover, and whether reorder ships as drag-to-order or a
pinned/favorite model with local persistence; phase 33 — whether the picker is a
distinct step between connect and cockpit or an optional-session connect card
that lands on the picker when the field is left blank, and how a killed / renamed
session racing the picker is reconciled.

## Session ↔ project root coupling — prior-art index (Phases 34–36)

Per-phase prior-art for the session↔project-root block ([roadmap.md](roadmap.md)),
same shape as the indexes above. Research mode: websearch (2026-07-09) — the
session-scoped root-storage mechanism and the "one server, many per-context stores"
daemon shape were topped up with focused lookups; the tmux control-mode plumbing
resolves against Category 3 already catalogued. All licenses GPL-3.0-compatible.
Phase 36 (remote root picker) was added 2026-07-09 with no fresh research — its
browse capability resolves against the Zed remote-FS model (Category 8 #1) and
rift's own Phase-30 daemon file-op precedent already catalogued.

| Phase | Concern | Reference (repo + path) | License | Verdict |
|---|---|---|---|---|
| 34 | Start-directory for new panes / windows / sessions | tmux `new-session -c` / `new-window -c` / `split-window -c` and `attach-session -c` to (re)set a session's default working dir — new windows / panes inherit it ([tmux Advanced-Use wiki](https://github.com/tmux/tmux/wiki/Advanced-Use)); `#{pane_current_path}` inherit-cwd binding pattern ([DJ Adams](https://qmacro.org/blog/posts/2021/04/01/new-tmux-panes-and-windows-in-the-right-directory/)); `workmux` pane `-c` config (Category 3 #4) | ISC / MIT | **reuse** — tmux-native `-c`; AVOID `default-path` (removed in tmux 1.9 → `-c`). Thread the existing single root into `terminal.rs:275` / `session_view.rs:203/2097/2185`; the only site passing `-c` today is the explorer reveal path |
| 34/35 | "session = project" naming + create-with-dir convention | `joshmedeski/sesh` (folder basename → session name, git-worktree aware); `ThePrimeagen/tmux-sessionizer` + `jrmoulton/tmux-sessionizer` (git repo → session, created in its dir); `tmuxinator` / `smug` (declarative project `root:`) | MIT | **reference (pattern only)** — adopt the session-name = project-dir convention; NOT a dependency — rift is a control-mode client that attaches / creates via `new-session -A` itself, not an external session-spawner CLI |
| 35 | Session-scoped root storage (the coupling) | tmux **session user option** `@root`: `set -t <session> @root <path>`, query `display -p -t <session> '#{@root}'` — session-scoped, does not pollute the shell env ([tmux Advanced-Use wiki](https://github.com/tmux/tmux/wiki/Advanced-Use)); the session default working dir as a fallback signal; session **environment** (`set-environment -t`) as the rejected alternative | ISC | **reuse** — the `@root` user option is the clean, native, session-scoped coupling; no external project registry needed (tmux holds it). AVOID session-environment (leaks into child shells) and relying on a durable session-start-dir format var (tmux exposes per-pane `#{pane_current_path}`, not a stable session path) |
| 35 | One server holding N project-root contexts + per-context LSP / git (the per-session daemon shape) | `zed` `HeadlessProject` → `WorktreeStore` holds **multiple** `Worktree` entities at once; `LspStore` / `GitStore` share that store and operate per-worktree ([Project & Worktrees, DeepWiki](https://deepwiki.com/zed-industries/zed/5.1-project-and-worktrees); Category 8 #1); rift's own single-root chokepoint (`crates/daemon/src/lib.rs` — workers spawned once at serve start) + the Phase-3.5 bind-at-spawn / shared-socket decision ([archive/spec-daemon-project-root.md](archive/spec-daemon-project-root.md)) | GPL-3.0 | **reference** — the target shape: the one shared daemon holds a session-keyed map of watched contexts (not a single global root re-scanned on switch), so two app instances attaching different sessions to the one daemon each get their own tree / git / LSP. Rejected: per-project daemon / socket (breaks the reattachable-single-daemon contract, #62 / dogfooding-channels) |
| 35 | Which session → which root, across restarts + the connect flow | `zed` workspace persistence — root paths serialized per workspace in SQLite, `recent_project_workspaces` for the recents list ([Workspace Persistence, DeepWiki](https://deepwiki.com/zed-industries/zed/3.4-workspace-persistence)); rift's own recents / window-state store (phase 9) | GPL-3.0 | **reference / reuse own pattern** — the durable per-session root lives in tmux `@root`; the app keeps only a lightweight recents mapping (session → last-used root) for the connect / pick flow, reusing the phase-9 store — no bespoke external project-file format (Zed-style workspace files) |
| 36 | Remote directory browsing — pick a project root on the host | `zed` remote model — the daemon owns the fs and enumerates directories server-side, clients are thin proxies (`HeadlessProject` / `WorktreeStore`, Category 8 #1); rift's own Phase-30 daemon file-op precedent (`std::fs` on the remote host via new `protocol` messages, not client SFTP); `sxyazi/yazi` russh-SFTP remote-fs provider (Category 5 #2, reference-only — rift's daemon already runs on the host, so `std::fs::read_dir`, not SFTP) | GPL-3.0 / MIT | **reference / reuse own pattern** — a new `protocol` dir-listing request/reply, executed daemon-side like the Phase-30 file ops; the picker is the UI over it. AVOID client-side SFTP (the daemon is already on the host) |
| 36 | Folder picker → session (name = basename, git-aware, recents) | `joshmedeski/sesh` + `*/tmux-sessionizer` (folder / git repo → session in its dir, basename as the name — already cited for 34/35); `zed` "Open Folder" + `recent_project_workspaces` recents ([Workspace Persistence, DeepWiki](https://deepwiki.com/zed-industries/zed/3.4-workspace-persistence), Category 8 #1) | MIT / GPL-3.0 | **reference (pattern)** — folder-basename default + a phase-9 recents list of recent roots; the picker replaces the zero-sessions empty-state (connect with no sessions opens it directly) and reuses the phase-33 post-connect picker as its entry surface |

Open design decisions deferred to each phase's `/loopkit:plan` spec (never a
roadmap guess): phase 34 — session default dir (set once, inherited) vs per-call
`-c`, and whether a pre-existing `$HOME`-rooted session is re-rooted on attach;
phase 35 — the durable store (`@root` vs session dir vs app recents; recommendation
`@root`, written + read in one phase) and the daemon context depth (active-only
re-scan vs concurrent per-session contexts; recommendation concurrent, the only
shape correct under two instances sharing one daemon); phase 36 — where the browse
starts (`$HOME` vs a phase-9 recents list), whether non-git roots are allowed and
git repos flagged, and whether the first-run picker pre-selects a sensible default
vs always starting empty. No root-switch hook is
pre-baked into the in-flight phase-32/33 work — the `SessionSwitchRequest → Attach`
seam is already the extension point.

## Workspace visibility rail — prior-art index (Phase 39)

Per-concern prior art for the rail-driven visibility + solo model (Phase 39).

| Concern | Reference | Verdict |
|---|---|---|
| Rail icon toggles an area's visibility (inactive hidden, not just collapsed) | VS Code Activity Bar — view containers as activity items; toggle sidebar / activity-bar visibility ([Custom Layout](https://code.visualstudio.com/docs/configure/custom-layout), [Activity Bar UX](https://code.visualstudio.com/api/ux-guidelines/activity-bar)); Zed dock-toggle actions — toggling focus onto a panel opens its dock ([Zed new panel system](https://zed.dev/blog/new-panel-system)) | reference — adopt icon-per-area + click-toggles-visibility; a click on the active area hides it |
| Solo / maximize one area (zoom = deselect the rest) | Zed `workspace::ToggleZoom` (maximize a pane/panel, Shift+Esc; [Zed panel system](https://zed.dev/blog/new-panel-system)); VS Code "View: Toggle Maximized Panel"; JetBrains "Hide All Windows / Restore" (Ctrl+Shift+F12) + tool-window maximize ([Tool windows](https://www.jetbrains.com/help/idea/tool-windows.html)) | reference — rift's zoom = solo the area (hide the rest), restore on re-toggle; the visibility SET is rift-owned state driving the dock beneath |
| The dock / panel substrate | longbridge/gpui-component Dock (vendored, #325); `zed` `crates/workspace/src/dock.rs` — Dock entities, panel open / zoom lifecycle | reuse (gpui-component) / reference (Zed dock model) |

Notes — ADOPT: the canonical icon-rail + solo/maximize semantics; keep the visibility set
as rift-owned state driving gpui-component Dock's show/hide, so "inactive = not rendered"
and "zoom = solo" compose cleanly. AVOID: VS Code / Zed drag-rearrange, multiple docks,
floating panels, and per-user free layout — out of scope (rift's area set is fixed and
opinionated). Sources: VS Code Custom Layout / Activity Bar docs; JetBrains Tool Windows
help; Zed "new panel system" blog.

## Mid-session session lifecycle — prior-art index (Phase 40)

Per-concern prior art for the connected-but-sessionless mid-session state (Phase 40).

| Concern | Reference | Verdict |
|---|---|---|
| Kill the attached session → switch to another vs detach (never disconnect the transport) | **tmux `detach-on-destroy`** option: default `on` detaches the client to the shell when the attached session is killed; `off` switches the client to the most-recently-active remaining session instead ([tmux(1)](https://man7.org/linux/man-pages/man1/tmux.1.html)) | reference — rift's variant of `detach-on-destroy off`: on kill, show the session PICKER (user chooses, always — even for one) rather than auto-switch, and open the root picker when none remain; never drop the SSH/daemon connection |
| Connection persists independent of the active session / workspace | VS Code Remote-SSH — the SSH host stays connected across closing folders/terminals; only a real transport loss shows "Disconnected, reconnecting" ([Remote SSH](https://code.visualstudio.com/docs/remote/ssh)); Zed / rift's own phase-20 daemon-as-proxy — the reattachable daemon survives drops (Category 8 #1) | reference — model the connection (SSH + daemon) as persistent and the session (tmux) as ephemeral; "session ended" is an in-app transition, not a disconnect |
| Re-enter the picker states with a live connection | rift's own phase-33 post-connect picker + phase-20 recovery engine's re-Attach ([spec-post-connect-picker.md](spec-post-connect-picker.md), [spec-connection-robustness.md](spec-connection-robustness.md)); the existing `PickerOutcome::ShowPicker` empty-vs-non-empty routing | reuse own pattern — drive the pre-cockpit `ScreenState::Picker` / `RootPicker` machinery mid-session with the live daemon client, not only after a fresh connect |

Notes — ADOPT: tmux's `detach-on-destroy off` semantics (switch, don't disconnect), rendered as rift's picker; the connection-vs-session separation from VS Code Remote / Zed / rift's own daemon-as-proxy. AVOID: auto-attach on kill (the user chose always-picker), tmux's default detach-to-shell (rift stays in-app), and any teardown of the SSH/daemon on a session end. Sources: tmux(1) man page (`detach-on-destroy`); VS Code Remote-SSH docs.

## Clone-a-repository into a session — prior-art index (Phase 42)

Seeded 2026-07-10 from idea sparring (research mode: websearch). The
clone-from-URL-then-open pattern, adapted to rift's remote-native
session=project model: the daemon (already on the remote) clones with the
host's own git credentials and the new session is born rooted at the checkout.

| Concern | Reference | Verdict |
|---|---|---|
| Clone-from-URL → open as workspace/session | VS Code "Git: Clone" (URL → destination folder → prompt "Open" → workspace-trust) ([Working with repositories](https://code.visualstudio.com/docs/sourcecontrol/repos-remotes)); DevPod `devpod up <git-url>` (workspace from a git URL, optional `@ref`) ([DevPod create-a-workspace](https://devpod.sh/docs/developing-in-workspaces/create-a-workspace)); Gitpod / JetBrains Gateway (paste a source-control URL to start) | reference — adopt URL → parent → name(=basename) → clone → auto-create session; rift binds clone directly to session=project (`@root`), not clone→open-folder |
| git clone execution (pure-Rust, musl-clean) | `gix` (GitoxideLabs/gitoxide) clone/fetch vs. shelling out to system `git`; **Zed** (zed-industries/zed) shells out to `git clone` (`std::process::Command`, PR #35606) and **removed** `git2`/libgit2 (PR #53453, ~30k lines of vendored C); gitoxide's own mature CLI clones over curl+OpenSSL and labels its pure-Rust HTTPS transport "less mature" | **shell out to system `git`** (Phase 42 re-plan verdict, reversing the initial "clone via gix" choice). The plan-time check resolved against gix: enabling gix's HTTPS transport pulls `rustls` → `aws-lc-rs` (C crypto, needs a musl C cross-compiler), violating the constitution's pure-Rust/no-C daemon rule — and there is no production-grade pure-Rust TLS (`rustls-rustcrypto` is "DO NOT USE IN PRODUCTION"). Shelling out keeps the daemon pure-Rust/C-free and inherits host credential-helper auth for free; the accepted cost is a runtime dependency on `git` on the host (VS Code/JetBrains/Zed/orchestrators all require it). `gix` stays for local reads only (status/diff — pure-Rust, no network). |
| Remote-native credential model | rift's own daemon runs ON the remote / container with its ambient git creds (e.g. the homelab devenv `GIT_AUTH_TOKEN`) vs DevPod / VS Code credential *forwarding* to the remote | greenfield / differentiation — no credential forwarding: the daemon clones with the host's own credentials. AVOID a forwarding / auth-UI path at v1 |

Notes — ADOPT: the URL → parent → name → clone → open-as-session flow (VS Code / DevPod / Gitpod), rebound to session=project; **shell out to the host's `git` for the clone** (Phase 42 re-plan verdict — the pure-Rust gix HTTPS path pulls C crypto, see the verdict cell above; the daemon stays pure-Rust and inherits host credential-helper auth for free). AVOID: credential forwarding (the remote-native daemon uses the host's own creds), a git-remote-manager scope, and branch / PR-slug parsing at v1 (DevPod has it; defer). Foundation impact (recorded here, authored + ratified at Phase 42's /loopkit:plan spec-acceptance, never edited from here): a new `crates/protocol` clone channel is a deliberate API addition (`PROTOCOL_VERSION` bump), and the daemon gains git-clone execution (gix) with progress / error reporting. Sources: VS Code source-control docs; DevPod create-a-workspace docs; gitoxide (gix) project.

## Host resource telemetry — prior-art index (Phases 43–46)

Seeded 2026-07-11 from idea sparring (research mode: websearch). Per-phase prior
art for the host-telemetry block ([roadmap.md](roadmap.md)), same shape as the
indexes above; see Category 11 for the full entries.

| Phase | Concern | Reference (repo + path) | License | Verdict |
|---|---|---|---|---|
| 43 | Cross-platform host metrics from /proc (CPU / RAM / swap / load) | `GuillaumeGomez/sysinfo` (Category 11 #1) | MIT | **reuse (dependency)** — the daemon's `System` sampler; no hand-rolled /proc parser. Instantiate once, two-sample CPU%, hold state across ticks |
| 43 | Metric selection + sampling cadence + the status-line indicator | `ClementTsang/bottom` (Category 11 #2); rift's own composite status line (Phase 22, [spec-status-line.md](spec-status-line.md)) | MIT | reference (what to show, how often) / **reuse own** (a new segment on the phase-22 status line) |
| 44 | Memory-pressure signal (portable — the guaranteed path) | `/proc/meminfo` `MemAvailable` + swap + `/proc/loadavg`, via `sysinfo` (Category 11 #1) | MIT | **reuse** — `MemAvailable` ratio + swap-in-use + load trend; works on every host incl. WSL2 |
| 44 | Memory pressure as stall-time (optional enhancement) | Linux **PSI** `/proc/pressure/memory` (Category 11 #4); systemd-oomd + Netdata PSI triggers | kernel (GPL-2.0) / ref | reference — the trigger / poll pattern where /proc/pressure exists; **absent on the stock WSL2 kernel (`CONFIG_PSI` off, confirmed live)** → gate on file existence, never primary |
| 45 | Per-pane process-subtree attribution | `YlanAllouche/tmux-task-monitor` (Category 11 #5: `pane_pid` → /proc children, grouped per window / session); `aristocratos/btop` process tree (Category 11 #3); `sysinfo` per-PID RSS/CPU + parent PID | MIT / Apache-2.0 / MIT | reference (method + tree UX) — roll up the `pane_pid` subtree; **agnostic label from `pane_current_command`, never agent detection** |
| 46 | Sparkline history + detail breakdown + disk headroom | `ClementTsang/bottom` time-series widgets (Category 11 #2); `btop` detail panels (Category 11 #3); `sysinfo` `Disks` | MIT / Apache-2.0 / MIT | reference (history + breakdown UX) / **reuse** (`sysinfo` disks for project-FS headroom); history is a client-side ring buffer |

Open design decisions deferred to each phase's `/loopkit:plan` spec (never a
roadmap guess): phase 43 — the sampling interval and whether the daemon pushes on a
timer or the client polls (recommendation: daemon timer push on the existing stream,
coalesced), and the host-metrics message shape; phase 44 — the warning thresholds
(available-memory % and swap-in-use %) plus hysteresis to avoid flapping, and whether
the toast is dismissible / rate-limited; phase 45 — subtree cost at sampling cadence
(walk `/proc/<pane_pid>` descendants vs a parent-PID index built once per tick) and
how a short-lived child racing a sample is handled; phase 46 — which filesystem the
disk indicator tracks (the session `@root` mount vs the daemon's own) and the
sparkline retention window.

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
| `sysinfo` | `GuillaumeGomez/sysinfo` | MIT | Host + per-process resource metrics (/proc-backed on Linux; CPU / RAM / swap / load / disks) |

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
