# Spec: Dogfooding channels (rift-stable / rift-dev)

> Status: READY
> Created: 2026-06-10
> Completed: —

Two side-by-side rift instances on one machine — a pinned Release **stable** daily
driver and the existing Debug watch-loop **dev** channel for the acceptance gate —
sharing one tmux session (and daemon) so the dev loop's recompile-respawn churn can
never take down the tool the developer works in. The stable channel is launchable
without a terminal — a Windows desktop shortcut to the pinned binary — so it is a true
daily driver, not a recipe invocation.

## Why

rift is now dogfooded as the primary work tool. The current loop runs
`just dev-windows-watch` on `develop`: every file change rebuilds the app, kills
`rift.exe`, and relaunches it. Two consequences make it unusable as a daily driver:

1. The app closes and reopens on every change.
2. A broken build (compile error or panic) means **no app comes back** — and since
   the GUI is the only way to prompt the live tmux sessions, the developer is locked
   out of their own running agents until the build is fixed.

The fix is to decouple the tool-you-work-in from the iteration loop: a **stable
channel** that is pinned and never auto-rebuilt, plus the **dev channel** (today's
watch loop) reserved for the acceptance/visual gate.

The work the developer cares about already survives an app restart: rift attaches via
`tmux -CC new-session -A -s rift` (attach-or-create), so the tmux session `rift`
persists on the host and a restarted GUI reattaches to it. The stable channel exploits
this — it owns the live session; promotion restarts it without losing work.

## Outcome

What is true when this work is done:

- [ ] A `rift-stable` **Release** binary runs as the daily driver under its own
      process image name, attached to tmux session `rift`, and is never rebuilt or
      killed by the dev watch loop.
- [ ] `just promote` rebuilds stable from the accepted source and restarts it in one
      step; the tmux session (and any running agent work) survives the restart.
- [ ] The dev channel (`just dev-windows[-watch]`) attaches **by default to the same
      session `rift`** (mirror); a single env override points it at a throwaway
      `rift-dev` session for isolated/destructive tests.
- [ ] A broken dev build leaves stable running and promptable — the original pain no
      longer reproduces.
- [ ] The tmux session name is read from `RIFT_SESSION` (default `rift`); the daemon
      isolation knob is `RIFT_DAEMON_REMOTE_DIR` (already read by the app, not yet
      surfaced in the dev recipes).
- [ ] A Windows desktop shortcut launches the pinned `rift-stable.exe` **without a
      terminal** (no console window) and always reflects the latest promoted build —
      the shortcut targets the fixed in-place path that `promote` overwrites, so it is
      created once. The Release binary carries an embedded taskbar icon.
- [ ] `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` stay
      green; CI `app-check` (`cargo check -p rift-app`, native Linux) compiles the
      platform-agnostic `RIFT_SESSION` change, and the windows-target binary is
      verified by the local `just dev-windows` / `build-windows` cross-compile.

## Scope

### In scope

- **App:** read the tmux session name from `RIFT_SESSION` (default `"rift"`), used for
  both the `new-session -A -s <name>` command and the `TmuxClient` label
  (`crates/app/src/main.rs`). One env knob, matching the existing SSH-config pattern.
- **App (Windows launcher support):** gate the Windows subsystem to GUI for non-debug
  builds (`#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`) so the
  Release/stable binary launches with **no console window**, while debug/dev keeps the
  console for `RUST_LOG`. Embed a taskbar icon through the existing
  `crates/app/resources/windows/rift.rc` (`embed-resource`, already in the build) — one
  `ICON` directive plus a developer-supplied `.ico` asset; no new dependency.
- **just recipes (Windows host, the primary dev loop):**
  - `promote` — guard that HEAD is `develop` and fast-forwarded to `origin/develop`
    (refuse otherwise), then build Release, copy the binary to a distinct image name
    (`rift-stable.exe`), kill the old stable, and relaunch it **detached** on session
    `rift`.
  - `stable` — relaunch the pinned `rift-stable.exe` without rebuilding (e.g. after a
    reboot); hint to run `promote` if it is absent.
  - `dev-windows` updated to forward `RIFT_SESSION` (default `rift`) into the Windows
    process via `WSLENV`; its `taskkill.exe /F /IM rift.exe` continues to target only
    dev.
  - A shared private launch helper so the env block (SSH vars, later daemon vars) does
    not drift between the dev and stable recipes.
  - `install-shortcut` — a one-time PowerShell recipe (`WScript.Shell.CreateShortcut`)
    that drops a Desktop shortcut (manually pinnable to the taskbar) targeting the
    wslpath-translated `rift-stable.exe` with the embedded icon, and persists
    `RIFT_SSH_KEY` via `setx` (Windows user env) for the terminal-free direct launch —
    the only config the app's defaults do not already cover (host/user/port/session
    match; the daemon is skipped when `RIFT_DAEMON_BINARY` is unset).
- **tmux mirror policy (optional):** `window-size largest` for the shared session so a
  dev restart's transient small attach does not reflow stable's view — applied via a
  small recipe or a documented `~/.tmux.conf` line.
- **Docs:** a short "Dogfooding channels" note in `CLAUDE.md` under "Parallel
  development" so the two channels and recipes are discoverable.

### Out of scope

- **Auto-promotion on merge.** Promotion stays a manually-triggered recipe — the
  developer decides when a feature enters the tool they depend on. (Automate the
  steps, keep the trigger human.)
- **A Linux/X11 stable recipe.** The primary dev loop is the Windows host; the
  `RIFT_SESSION` env read benefits the Linux `dev`/`dev-watch` recipes too (platform-
  agnostic, in the app), but a dedicated Linux stable channel is a follow-up.
- **A persisted config file / settings UI.** The knobs are env vars, matching the
  existing SSH config; no new config layer.
- **Independent per-channel views** (different windows in stable vs. dev via grouped
  sequences). Explicitly not wanted — the mirror (one shared session, shown twice) is
  the goal.
- **Window-state persistence** across restarts — already its own deferred track
  (roadmap), not folded in here.
- **Programmatic taskbar pinning.** Windows has blocked pinning to the taskbar from
  scripts since Win10; the recipe creates a Desktop/Start shortcut the developer pins
  by hand.
- **A Linux/X11 `.desktop` launcher.** Follow-up, mirroring the Windows-host-primary
  stance already taken for the stable recipe itself.
- Any agent-specific behaviour.

## Constraints

- **Single heavy `target/` topology** (`CLAUDE.md`, "Parallel development"). The GPU
  app is the only expensive, non-parallelizable build; stable must reuse the one
  `target/` (a copied-aside exe), not a second checkout/worktree that would double the
  ~20 GB of skia/wgpu artifacts.
- **Process-image-name separation is mandatory.** The dev loop kills `rift.exe` by
  image name; stable must run under a different image (`rift-stable.exe`) or promotion
  in dev would kill the daily driver.
- `rift-app` is **excluded** from `just build` / `just lint`. CI `app-check` runs
  `cargo check -p rift-app` on **native Linux** (no windows-target cross-compile exists
  in CI); the `RIFT_SESSION` read is platform-agnostic, so that covers it, while the
  windows-target binary is checked only by the local `just dev-windows` /
  `build-windows` cross-compile.
- **No `.unwrap()` in library code.** The env read lives in the app binary (`anyhow`
  context is fine); it does not touch a library crate.
- **Mirror correctness rests on rift staying a stateless renderer over tmux.** Every
  UI action already routes through a tmux control-mode command (`select-window`,
  `select-pane`, `split-window`, `kill-window`, resize via `set_client_size`,
  keystrokes via `send-keys`) and the active window/pane is **derived from the tmux
  snapshot** (`apply_snapshot` reads `window.is_active`). Two clients on one session
  therefore mirror for free. Adding client-local UI state that bypasses tmux would
  silently break the mirror.
- **Daemon (Phase 3+) is congruent, with one rule.** The daemon socket is version-
  keyed (`$HOME/.rift/bin/rift-daemon-<version>.sock`) and the server is multi-client
  (task-per-connection, each its own `broadcast` subscription on a shared
  `watch::State`). Same-version stable + dev therefore **share one daemon**; a version
  bump in dev spawns a **second** daemon (redundant but read-only, no conflict). A
  protocol change in `crates/protocol` **must** carry a version bump, or the two
  builds share a socket with incompatible framing and both break.
- **The direct launcher reuses the app's env defaults, not a config file.** Only
  `RIFT_SSH_KEY` deviates from the dev setup (app default `id_ed25519` vs the dev
  `id_rsa`); it is pinned once via persistent Windows env (`setx`). Host/user/port/
  session match the defaults and the daemon is skipped when `RIFT_DAEMON_BINARY` is
  unset — congruent with the "env vars, no config file" decision.
- **Console gating is `debug_assertions`-based, not target-based**, so the dev
  watch-loop keeps its `RUST_LOG` console while only Release/stable goes console-free.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The dev channel mirrors by default (attaches to session `rift`) | The developer works in stable and fires up dev for the acceptance gate against the same live state. rift routes every UI action through tmux and derives active window/pane from snapshots, so two clients on one session mirror tab/pane/split/kill/resize/keystroke bidirectionally — an ideal same-input/two-renderers regression harness. Isolation stays one env override away. | 2026-06-10 |
| Stable is a Release build | It is the primary work tool run for hours; the one-time longer Release build buys a smoother daily driver, and promotion is infrequent (the developer chooses when). | 2026-06-10 |
| Channels are separated by process image name + tmux session, not by checkout | The GPU app is the only expensive build; a second checkout/worktree would double the ~20 GB `target/`. A copied-aside `rift-stable.exe` reuses the one `target/` and only needs a distinct image name so the dev loop's `taskkill /IM rift.exe` cannot kill the daily driver. | 2026-06-10 |
| Session name via `RIFT_SESSION` env (default `rift`); daemon isolation via existing `RIFT_DAEMON_REMOTE_DIR` | Matches the existing `env::var(...).unwrap_or_else(...)` config pattern (`SshConfig`); no new config file. Gives the same mirror-or-isolate switch on both the tmux and the daemon axis. | 2026-06-10 |
| Promotion is manual, never auto-on-merge | The developer explicitly wants to choose when a new feature lands in the tool they depend on. Automate the steps; keep the trigger human. | 2026-06-10 |
| tmux `window-size largest` for the shared session (optional, via tmux config) | On restart the dev client briefly attaches at 80×24; `largest` keeps the window at the larger client's size so stable's view does not reflow on every dev recompile. Two equally-maximized windows are unaffected. | 2026-06-10 |
| Adopt the release-channels pattern (side-by-side, isolated per-channel identity) | Precedent: VS Code Insiders installs beside Stable with isolated state for daily-driver dogfooding; Zed ships stable/preview/nightly/dev as separate apps with per-channel state dirs. rift's twist — a shared tmux session for a live mirror — is unique to it being a multiplexer frontend. | 2026-06-10 |
| `promote` builds the current checkout, guarded: it asserts HEAD is `develop` and fast-forwarded to `origin/develop`, and refuses otherwise | Guarantees stable == accepted develop with no ref-switching (which would disturb the station's running `dev-watch`). The guard is exactly what stops a promotion mid-gate, when the station sits detached on a feature branch (`CLAUDE.md`, "Parallel development") — it refuses rather than baking un-merged code into the daily driver. Rejected: always-build-`origin/develop` (extra recipe machinery to build a ref without disturbing the working tree) and explicit-ref (puts correctness on the operator and a checkout disturbs `dev-watch`). | 2026-06-10 |
| The launcher is a one-time `.lnk` to the fixed `rift-stable.exe` path | `promote` overwrites that path in place, so a single shortcut always points at the latest stable — no per-promote regeneration. | 2026-06-11 |
| Direct `.exe` launch + `setx RIFT_SSH_KEY`, over a `wsl.exe … just stable` wrapper | Gives a console-free double-click that pins the real exe in the taskbar and reuses the app's env defaults — matching the "env vars, no config file" decision. Rejected the wrapper: it flashes a console window and still needs the subsystem fix. | 2026-06-11 |
| Console suppressed via `cfg_attr(not(debug_assertions), windows_subsystem="windows")` | Release/stable launches with no terminal; debug/dev keeps the console for `RUST_LOG`. One attribute, no dependency. | 2026-06-11 |
| Icon embedded via the existing `rift.rc` / `embed-resource` | The Windows resource pipeline already exists (manifest); an `ICON` line + `.ico` adds the taskbar icon with no new dependency and shows even on a direct launch. | 2026-06-11 |

## Tracking

The decomposition lives as GitHub issues, grouped under a milestone — one issue per
implementable step. This spec owns the design; the issues own progress.

Commit types are `chore:` / `build:` (justfile + an env read) and so are **exempt from
the planning gate**. Issues still reference this spec path for traceability, exactly
like the other meta/DX tracks (workflow-automation, planning-automation, component
gallery).

- Milestone: created from this spec once it is `READY`.
- Issues: one per step, created once `READY`.

## Verification

How the whole spec is known complete:

- [ ] `cargo clippy --workspace -- -D warnings` passes; `cargo test --workspace`
      passes; CI `app-check` (`cargo check -p rift-app`, native Linux) compiles the
      `RIFT_SESSION` change; the windows-target binary builds via local
      `just dev-windows` / `build-windows`.
- [ ] `just promote` refuses unless HEAD is `develop` ff-synced to `origin/develop`;
      on a clean develop it produces `rift-stable.exe`, launches it **detached** (the
      recipe returns while the app keeps running), attached to session `rift`.
- [ ] `just dev-windows-watch` attaches to `rift` by default; a tab switch, split, and
      keystroke in one instance are reflected in the other, both directions.
- [ ] `RIFT_SESSION=rift-dev just dev-windows-watch` runs dev on an isolated session
      with stable untouched.
- [ ] Inducing a dev compile error leaves stable running and promptable — the original
      pain does not reproduce.
- [ ] Promoting (restart stable) does not lose the tmux session or running agent work.
- [ ] `just install-shortcut` creates a Desktop shortcut to `rift-stable.exe`;
      double-clicking it launches stable with **no console window**, attached to
      session `rift`, showing the embedded icon in the taskbar.
- [ ] After `just promote`, the same shortcut launches the newly-built stable (fixed
      path, overwritten in place — no shortcut regeneration).
- [ ] A debug `just dev-windows` still opens with its `RUST_LOG` console (subsystem
      gating is debug-only).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Shared session blast radius — a dev input bug reaches the real work session (e.g. a stray Ctrl-C to a running agent) | The mirror is used during the acceptance gate, not 24/7; risky/destructive tests use the `RIFT_SESSION=rift-dev` isolated session. |
| Daemon protocol skew if a protocol change ships without a version bump (shared socket, incompatible framing) | The version-keyed socket already routes a bumped dev to its own daemon; the discipline "protocol change ⇒ version bump" is recorded as a constraint and enforced at protocol-PR review. |
| Detached Windows-from-WSL launch may not survive the recipe exit, or env may not pass through `cmd.exe start` | Verified during implementation; fallback `nohup … &`. Captured as a verification step. |
| Reflow flicker in stable when dev restarts | `window-size largest` (prior decision); equally-sized maximized windows are unaffected regardless. |
| Two clients racing `set_client_size` | Benign size negotiation bounded by the `window-size` policy; no correctness impact. |
| Direct launch relies on the app's default SSH/daemon config; a future default divergence beyond the key could silently misconnect | Only the key deviates today and is pinned via `setx`; host/user/port/session match the defaults; the `install-shortcut` recipe documents the assumption. |
| `windows_subsystem = "windows"` hides early panics that previously printed to the console | Gating is debug-only, so dev keeps the console; for the GUI daily driver a failed launch shows as no window appearing, and Release logging still reaches stderr. |

## Decision log

- 2026-06-10: Spec created. Mirror-by-default and Release-stable were settled with the
  developer in the planning conversation; the channel-separation mechanism (image name
  + tmux session, single `target/`) is constraint-determined by the worktree topology;
  the release-channels framing is precedent (VS Code Insiders, Zed channels). One
  decision left open for the review gate: what `promote` builds stable from.
- 2026-06-10: Review gate (PR #148) resolved the open decision — `promote` builds the
  **current checkout, guarded** (HEAD must be `develop`, fast-forwarded to
  `origin/develop`; refuse otherwise). Picked over always-`origin/develop` and
  explicit-ref because it guarantees stable == accepted develop without ref-switching
  that would disturb the station's `dev-watch`, and the guard is what prevents a
  mid-gate promotion of un-merged feature code. Also corrected the spec's `app-check`
  claim — it is a native-Linux `cargo check -p rift-app`, not a windows-target build;
  the windows binary is checked only by the local `just dev-windows` / `build-windows`
  cross-compile.
- 2026-06-11: Extended with a no-terminal desktop launcher for the stable channel
  (developer request). Resolved with the developer: fold into this spec (same milestone
  #12), launch the **direct `rift-stable.exe`** via a one-time `.lnk` with `RIFT_SSH_KEY`
  pinned by `setx` (over a `wsl.exe … just stable` wrapper, which flashes a console and
  still needs the subsystem fix), developer-supplied logo. Survey findings: the Windows
  resource pipeline already exists (`rift.rc` + `embed-resource`), so the icon needs no
  new dependency; the app's env defaults already cover host/user/port/session and skip
  the daemon when unset, leaving only the SSH key to pin; the binary has no
  `windows_subsystem` attribute, so a Release launch currently spawns a console — gated
  to non-debug builds it goes console-free while dev keeps its `RUST_LOG` console.
