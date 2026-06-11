# Spec: Dogfooding channels (rift-stable / rift-dev)

> Status: READY
> Created: 2026-06-10
> Completed: —

Two side-by-side rift instances on one machine — a pinned, optimized **stable** daily
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

- [ ] A `rift-stable` binary, built under an optimized `stable` cargo profile, runs as
      the daily driver under its own process image name, attached to tmux session
      `rift`, and is never rebuilt or killed by the dev watch loop.
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
      the shortcut targets the fixed Windows-native path
      (`%LOCALAPPDATA%\rift\rift-stable.exe`) that `promote` overwrites, so it is
      created once. The binary carries an embedded taskbar icon.
- [ ] `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` stay
      green; CI `app-check` (`cargo check -p rift-app`, native Linux) compiles the
      platform-agnostic `RIFT_SESSION` change, and the windows-target binary is
      verified by the local `just dev-windows` / `build-windows` cross-compile.

## Scope

### In scope

- **App:** read the tmux session name from `RIFT_SESSION` (default `"rift"`), used for
  both the `new-session -A -s <name>` command and the `TmuxClient` label
  (`crates/app/src/main.rs`). One env knob, matching the existing SSH-config pattern.
- **App (Windows launcher support):** suppress the console window for the stable build
  via a cargo feature (`#![cfg_attr(feature = "windowed", windows_subsystem = "windows")]`),
  enabled only by `promote`'s build — **not** `not(debug_assertions)`, because the
  `stable` profile keeps `debug-assertions` on (the GPUI Windows renderer needs its
  runtime-shader path to cross-compile from WSL, see Constraints), so a debug-assertions
  gate would never fire. The default (feature off) keeps the `RUST_LOG` console for dev.
  The feature is declared in `crates/app/Cargo.toml` `[features]`, excluded from
  `default` — like the existing `gallery` opt-in. Embed a taskbar icon through the
  existing
  `crates/app/resources/windows/rift.rc` (`embed-resource`, already in the build) —
  one `ICON` directive per channel, selected via an rc `#ifdef WINDOWED` macro that
  `build.rs` defines from the same `windowed` feature: stable embeds the primary
  brand mark, dev the monochrome outline, so the two channels are visually distinct
  in the taskbar. Assets are developer-supplied `.ico` files from the brand kit; no
  new dependency.
- **just recipes (Windows host, the primary dev loop):**
  - `promote` — guard that HEAD is `develop` and fast-forwarded to `origin/develop`
    (refuse otherwise), then build `--profile stable --features windowed`, copy the
    binary to the pinned Windows-native launcher path
    (`%LOCALAPPDATA%\rift\rift-stable.exe` — distinct image name, outside `target/`),
    kill the old stable, and relaunch it **detached** (direct binfmt exec via
    `setsid`) on session `rift`.
  - `stable` — relaunch the pinned `rift-stable.exe` without rebuilding (e.g. after a
    reboot); hint to run `promote` if it is absent.
  - `dev-windows` updated to forward `RIFT_SESSION` (default `rift`) into the Windows
    process via `WSLENV`; its `taskkill.exe /F /IM rift.exe` continues to target only
    dev.
  - A shared private launch helper so the env block (SSH vars, later daemon vars) does
    not drift between the dev and stable recipes.
  - One-time launcher setup is **documented, not automated** (`CLAUDE.md`): create a
    Desktop shortcut to `%LOCALAPPDATA%\rift\rift-stable.exe` by hand (pin to the
    taskbar manually). No env setup: the SSH key — the only config the app's defaults
    do not already cover (host/user/port/session match; the daemon is skipped when
    `RIFT_DAEMON_BINARY` is unset) — is baked into the stable exe at promote-build
    time (see Constraints), and so is the WSL-root working directory the stable
    profile needs for GPUI's runtime path resolution (`RIFT_DEFAULT_WORKDIR`, set as
    cwd at startup — an earlier revision wrongly claimed the launcher's working
    directory was irrelevant). The exe is a plain Windows path, so no UNC shortcut
    target is involved.
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
- **The pinned stable exe lives outside `target/`** (`%LOCALAPPDATA%\rift`). `target/`
  belongs to cargo — a `cargo clean` must not delete the daily driver or break the
  shortcut — and a Windows-native path avoids a `\\wsl.localhost` UNC shortcut target
  and the 9P redirector overhead on every launch.
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
- **Stable builds under an optimized `stable` cargo profile, not `--release`.** The GPUI
  Windows DirectX renderer precompiles HLSL shaders at build time only in release
  (`cfg(not(debug_assertions))`), via a build script that runs solely on a Windows host
  and shells out to `fxc.exe`. So `cargo build --release` for the windows-gnu target
  **cannot cross-compile from WSL**: the build script is a silent no-op on Linux and the
  `include!(shaders_bytes.rs)` then fails to compile. The `stable` profile
  (`inherits = "release"`, `debug-assertions = true`, same for `build-override`) keeps
  release-grade optimization but takes the renderer's runtime-shader path — the path the
  debug dev loop already uses — so it cross-compiles. Verified on the station: a
  `stable`-profile build completes and the `gpui_windows` build script runs as a no-op
  (no `fxc`, no `shaders_bytes.rs`). Knock-on: the runtime path resolves GPUI's
  compile-time `CARGO_MANIFEST_DIR` paths at startup (shader sources, DirectWrite
  setup) — WSL paths, root-relative on Windows, resolvable only while the current
  drive is the WSL distro root. `promote` therefore bakes `RIFT_DEFAULT_WORKDIR`
  (= `wslpath -w /`) and the app sets it as cwd at startup, so an Explorer launch
  (cwd `C:\`) does not panic in platform init before any window appears.
- **The direct launcher reuses the app's env defaults, not a config file.** Only the
  SSH key deviates: the working dev key is `id_rsa` (justfile `windows_ssh_key`, the
  value every current launch already uses), while the app's *unused* code default is
  `id_ed25519`. `promote` bakes the dev key path into the stable exe as a
  **compile-time default** (`RIFT_DEFAULT_SSH_KEY`, read via `option_env!`, set from
  `windows_ssh_key`) — originally this was a `setx` Windows-user-env step, but
  Explorer's environment snapshot does not refresh dependably after the setx
  broadcast, so the pinned shortcut kept launching without the key and stable exited
  silently. Host/user/port/session match the defaults and the daemon is skipped
  when `RIFT_DAEMON_BINARY` is unset. The baked default cannot leak into the dev
  recipes: it only exists in promote's build, and `_launch-windows` exports
  `RIFT_SSH_KEY` unconditionally at runtime (which always wins over the baked value).
- **Console gating is a cargo feature (`windowed`), not `debug_assertions`.** The
  `stable` profile keeps `debug-assertions` on for the shader path (above), so the
  console-suppress attribute must key on a feature `promote` enables, not on
  `not(debug_assertions)`; the feature is off by default, so dev keeps its `RUST_LOG`
  console.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The dev channel mirrors by default (attaches to session `rift`) | The developer works in stable and fires up dev for the acceptance gate against the same live state. rift routes every UI action through tmux and derives active window/pane from snapshots, so two clients on one session mirror tab/pane/split/kill/resize/keystroke bidirectionally — an ideal same-input/two-renderers regression harness. Isolation stays one env override away. | 2026-06-10 |
| Stable builds under an optimized `stable` cargo profile (`inherits = "release"` + `debug-assertions = true`), not plain `--release` | Originally specced as a Release build for a smoother hours-long daily driver. A plain `--release` windows-gnu cross-compile from WSL turned out impossible: GPUI precompiles shaders via a Windows-host-only, `fxc`-dependent build script in release. The `stable` profile keeps release-grade optimization but rides the renderer's runtime-shader path (as debug does), so it cross-compiles from WSL while staying fast. Rejected plain-debug (loses optimization) and a native-Windows release build (needs a Windows toolchain + breaks the single-`target/` topology). | 2026-06-10, revised 2026-06-11 |
| Channels are separated by process image name + tmux session, not by checkout | The GPU app is the only expensive build; a second checkout/worktree would double the ~20 GB `target/`. A copied-aside `rift-stable.exe` reuses the one `target/` and only needs a distinct image name so the dev loop's `taskkill /IM rift.exe` cannot kill the daily driver. | 2026-06-10 |
| Session name via `RIFT_SESSION` env (default `rift`); daemon isolation via existing `RIFT_DAEMON_REMOTE_DIR` | Matches the existing `env::var(...).unwrap_or_else(...)` config pattern (`SshConfig`); no new config file. Gives the same mirror-or-isolate switch on both the tmux and the daemon axis. | 2026-06-10 |
| Promotion is manual, never auto-on-merge | The developer explicitly wants to choose when a new feature lands in the tool they depend on. Automate the steps; keep the trigger human. | 2026-06-10 |
| tmux `window-size largest` for the shared session (optional, via tmux config) | On restart the dev client briefly attaches at 80×24; `largest` keeps the window at the larger client's size so stable's view does not reflow on every dev recompile. Two equally-maximized windows are unaffected. | 2026-06-10 |
| Adopt the release-channels pattern (side-by-side, isolated per-channel identity) | Precedent: VS Code Insiders installs beside Stable with isolated state for daily-driver dogfooding; Zed ships stable/preview/nightly/dev as separate apps with per-channel state dirs. rift's twist — a shared tmux session for a live mirror — is unique to it being a multiplexer frontend. | 2026-06-10 |
| `promote` builds the current checkout, guarded: it asserts HEAD is `develop` and fast-forwarded to `origin/develop`, and refuses otherwise | Guarantees stable == accepted develop with no ref-switching (which would disturb the station's running `dev-watch`). The guard is exactly what stops a promotion mid-gate, when the station sits detached on a feature branch (`CLAUDE.md`, "Parallel development") — it refuses rather than baking un-merged code into the daily driver. Rejected: always-build-`origin/develop` (extra recipe machinery to build a ref without disturbing the working tree) and explicit-ref (puts correctness on the operator and a checkout disturbs `dev-watch`). | 2026-06-10 |
| The launcher is a one-time, hand-created `.lnk` to the fixed `%LOCALAPPDATA%\rift\rift-stable.exe` path | `promote` overwrites that path in place, so a single shortcut always points at the latest stable — no per-promote regeneration. Shortcut creation is a once-ever action and taskbar pinning is manual regardless (see Out of scope), so a dedicated `install-shortcut` recipe would automate nothing recurring — dropped for a documented manual step. | 2026-06-11 |
| Stable is pinned under `%LOCALAPPDATA%\rift` and launched detached via `setsid` direct exec (binfmt), not from `target/` via `cmd.exe start` | `target/` belongs to cargo (`cargo clean` would delete the daily driver and break the shortcut); a Windows-native path needs no `\\wsl.localhost` UNC shortcut target and skips the 9P redirector at launch. `cmd.exe` is not resolvable from the recipes on this host (Windows PATH not appended in WSL), while binfmt direct exec is proven by the foreground dev path; `setsid` with detached stdio survives the recipe exit (verified, incl. `taskkill.exe` via absolute `System32` path — the bare name was silently failing for the same PATH reason). | 2026-06-11 |
| Direct `.exe` launch + `setx RIFT_SSH_KEY`, over a `wsl.exe … just stable` wrapper | Gives a console-free double-click that pins the real exe in the taskbar and reuses the app's env defaults — matching the "env vars, no config file" decision. Rejected the wrapper: it flashes a console window and still needs the subsystem fix. | 2026-06-11 |
| The SSH-key default is baked into the stable exe at promote-build time (`RIFT_DEFAULT_SSH_KEY` via `option_env!`), replacing the `setx` step | `setx` proved unreliable in practice: Explorer's env snapshot does not refresh dependably after the broadcast, so the pinned shortcut kept launching without the key and stable exited silently (no console, no window). Baking removes the launcher's only external config; runtime `RIFT_SSH_KEY` still overrides, dev builds are unaffected (env unset at their build). | 2026-06-11 |
| Console suppressed via a cargo feature (`cfg_attr(feature = "windowed", windows_subsystem="windows")`), enabled by `promote` | The `stable` profile keeps `debug-assertions` on (shader path), so `not(debug_assertions)` would never fire; a feature decouples console-suppression from the profile. Off by default, so dev keeps its `RUST_LOG` console. No dependency. | 2026-06-11 |
| Icon embedded via the existing `rift.rc` / `embed-resource` | The Windows resource pipeline already exists (manifest); an `ICON` line + `.ico` adds the taskbar icon with no new dependency and shows even on a direct launch. | 2026-06-11 |
| Per-channel icons: stable embeds the primary brand mark, dev the monochrome outline | Developer request — the two mirrored instances must be visually distinguishable in the taskbar. Selection rides the existing `windowed` feature (`build.rs` defines a `WINDOWED` rc macro), so no new knob; dev needs no extra build flag. Assets from the brand kit: stable = bolder favicon cut (16/32/48) + 256 brand PNG, dev = rasterized `rift-icon-mono.svg`. | 2026-06-11 |

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
      on a clean develop it builds the `stable` profile, produces `rift-stable.exe`, and
      launches it **detached** (the recipe returns while the app keeps running), attached
      to session `rift`.
- [ ] `just dev-windows-watch` attaches to `rift` by default; a tab switch, split, and
      keystroke in one instance are reflected in the other, both directions.
- [ ] `RIFT_SESSION=rift-dev just dev-windows-watch` runs dev on an isolated session
      with stable untouched.
- [ ] Inducing a dev compile error leaves stable running and promptable — the original
      pain does not reproduce.
- [ ] Promoting (restart stable) does not lose the tmux session or running agent work.
- [ ] A hand-created Desktop shortcut to `%LOCALAPPDATA%\rift\rift-stable.exe`
      (one-time setup per `CLAUDE.md`) launches stable with **no console window**,
      attached to session `rift`, showing the embedded icon in the taskbar.
- [ ] After `just promote`, the same shortcut launches the newly-built stable (fixed
      path, overwritten in place — no shortcut regeneration).
- [ ] A debug `just dev-windows` still opens with its `RUST_LOG` console (the `windowed`
      feature is off by default).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Shared session blast radius — a dev input bug reaches the real work session (e.g. a stray Ctrl-C to a running agent) | The mirror is used during the acceptance gate, not 24/7; risky/destructive tests use the `RIFT_SESSION=rift-dev` isolated session. |
| Daemon protocol skew if a protocol change ships without a version bump (shared socket, incompatible framing) | The version-keyed socket already routes a bumped dev to its own daemon; the discipline "protocol change ⇒ version bump" is recorded as a constraint and enforced at protocol-PR review. |
| Detached Windows-from-WSL launch may not survive the recipe exit | Resolved: `setsid` direct binfmt exec with detached stdio — verified on the station: the recipe returns, the process survives the launching shell, env passes via `WSLENV` exactly as in the foreground path. |
| Reflow flicker in stable when dev restarts | `window-size largest` (prior decision); equally-sized maximized windows are unaffected regardless. |
| Two clients racing `set_client_size` | Benign size negotiation bounded by the `window-size` policy; no correctness impact. |
| Direct launch relies on the app's default SSH/daemon config; a future default divergence beyond the key could silently misconnect | Only the key deviates today and is baked at promote-build time; host/user/port/session match the defaults; `CLAUDE.md` documents the assumption. |
| `windows_subsystem = "windows"` hides early panics that previously printed to the console | The `windowed` feature is off by default, so dev keeps the console; a failed stable launch shows as no window appearing, and the pinned exe can be run foreground from a WSL terminal (binfmt direct exec, env via `_launch-windows` without `detach`) to surface stderr for diagnosis. |

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
  the daemon when unset, leaving only the SSH key to pin (to the dev `id_rsa`, not the
  unused `id_ed25519` code default); the binary has no
  `windows_subsystem` attribute, so a Release launch currently spawns a console —
  suppressed by a `windowed` cargo feature (see the next entry for why not
  `not(debug_assertions)`).
- 2026-06-11: `just promote`'s Release cross-compile failed on the GPU station —
  `gpui_windows` `include!`s build-time `fxc`-compiled shaders in release, and that build
  script only runs on a Windows host, so the windows-gnu cross-compile from WSL is a
  silent no-op that then fails to compile. Resolved with the developer: build stable under
  a new `stable` cargo profile (`inherits = "release"` + `debug-assertions = true`, incl.
  `build-override`) so it takes the renderer's runtime-shader path (as debug does) while
  keeping release-grade optimization. Verified on the station (3m20s build,
  `rift-stable.exe` produced, `gpui_windows` build script a no-op). Knock-on: the
  launcher's console-suppress can no longer key on `not(debug_assertions)` (the profile
  keeps it on) — it moves to a `windowed` cargo feature that `promote` enables. Rejected
  plain-debug (unoptimized) and a native-Windows release build (needs a Windows toolchain;
  breaks the single-`target/`).
- 2026-06-11: Simplified the launcher mechanics after the first detached launch broke
  (`cmd.exe: command not found` — this WSL config does not append the Windows PATH).
  Replaced `cmd.exe start` with `setsid` direct binfmt exec (no PATH dependency; WSLENV
  proven by the foreground path; verified detached and surviving the recipe exit),
  relocated the pinned exe from `target/.../stable/` to `%LOCALAPPDATA%\rift\`
  (`cargo clean` can no longer delete the daily driver; the shortcut targets a plain
  Windows path instead of a `\\wsl.localhost` UNC; no 9P redirector at launch), fixed
  all `taskkill.exe` calls to the absolute `System32` path (the bare name was silently
  failing for the same PATH reason — promote would have hit the Windows file lock on
  the second run), and dropped the planned `install-shortcut` recipe — shortcut
  creation is a once-ever manual action and taskbar pinning is manual regardless, so
  the recipe automated nothing recurring; the steps are documented in `CLAUDE.md`.
- 2026-06-11: Replaced the `setx RIFT_SSH_KEY` user-env step with a compile-time
  default baked by `promote` (`RIFT_DEFAULT_SSH_KEY` → `option_env!`, value from the
  justfile's `windows_ssh_key`). The setx route failed in practice: Explorer's
  environment snapshot did not pick up the new variable (even though it landed in
  `HKCU\Environment`), so the pinned taskbar shortcut kept launching the app without
  the key — SSH auth failed against the unauthorized `id_ed25519` default and stable
  exited silently (~1 s, no console, no window; reproduced and confirmed via captured
  stderr). A no-env launch of the baked exe connects and stays running. Runtime
  `RIFT_SSH_KEY` (exported by `_launch-windows`) still overrides; dev builds carry no
  baked default.
- 2026-06-11: Second silent-death cause behind the shortcut launch, after the baked
  key: with cwd on `C:\` (Explorer launch) the app panicked in GPUI platform init
  ("Error creating DirectWriteTextSystem … os error 3") before any window. The
  `stable` profile's runtime path resolves compile-time WSL `CARGO_MANIFEST_DIR`
  paths (shaders via `D3DCompileFromFile`, DirectWrite), which are root-relative on
  Windows and resolve only against the WSL distro root as current drive — true for
  every recipe launch (cwd inside WSL, hence never seen before), false for Explorer.
  Fix: `promote` bakes `RIFT_DEFAULT_WORKDIR` (= `wslpath -w /`) and `main()` sets it
  as cwd at startup (best-effort). Corrected the spec's earlier claim that the
  launcher's working directory is irrelevant. Verified: the same exe dies with cwd
  `C:\` and runs with the baked workdir.
