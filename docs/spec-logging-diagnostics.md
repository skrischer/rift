# Spec: Logging & diagnostics (tooling/DX track)

> Status: READY
> Created: 2026-06-12
> Completed: —

Professional debug logging for the dev and stable channels: one unified `tracing` setup across app and daemon — sink selection by runtime TTY detection instead of the compile-time `windowed` gate, size-based rotation (`.log`/`.log.old` pair) instead of per-run truncation, a panic hook that lands in the active sinks in every profile, and the daemon moved from raw `eprintln!` onto the same facade. Design is pre-decided by the Category 10 prior-art survey (`prior-art.md`, verified against Zed/wezterm/alacritty/helix source, 2026-06): keep the existing `tracing`/`EnvFilter` facade; the gaps are sink strategy, rotation, and the daemon.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] **Sink selection is a runtime TTY check, not a build-profile gate**: a terminal launch logs to the console; a windowed/redirected launch logs to the file sink — one mechanism covers dev console, windowed stable, and redirected output (the Zed pattern), with an env override for the forced cases. The `windowed` feature keeps gating only the Windows subsystem, no longer the logging.
- [ ] **The stable log rotates instead of truncating**: `rift-stable.log` + `rift-stable.log.old` in `%LOCALAPPDATA%\rift`, append mode, rotated at a size threshold (Zed's 1 MB pair as default) — the previous run's evidence survives a restart, and no file grows without bound.
- [ ] **Panics land in the log in every profile**: the panic hook routes thread, location, and message through `tracing::error!` into whatever sinks are active (today this exists only in the windowed stable build), then delegates to the default hook.
- [ ] **The daemon logs through `tracing`**: `eprintln!` is gone from the daemon binary; sink selection follows the same TTY rule — stderr-is-a-TTY (interactive run) → stderr fmt subscriber; redirected (the detached `--serve-uds` launch) → the daemon-managed size-rotated file sink **only**, so no log line is duplicated into the launch line's unrotatable redirect file. That redirect stays solely as the pre-init/panic-backtrace backstop and stays tiny. **stdout stays frame-only** in `--connect` relay mode (the #60 framing invariant is untouched and test-guarded).
- [ ] **Filtering is uniform**: `RIFT_LOG` falling back to `RUST_LOG` falling back to the built-in default, which includes per-module suppression of known-noisy dependencies (`wgpu_core`, `wgpu_hal` → error; the wezterm pattern) so a debug-level run stays readable.
- [ ] **One shared implementation**: app and daemon consume the same small logging library — no duplicated rotation/filter/panic code, no behavioral drift between channels.
- [ ] No telemetry, no analytics, no network sinks — logs are local files and consoles, full stop.

## Scope

### In scope

- **`crates/logging`** (new small library, `gpui`-free, musl-clean — it becomes a daemon dependency): the size-rotating append writer (the ~50-line `SizedWriter` shape from Zed — `tracing-appender` rotates only by time, so size rotation is custom by necessity, not preference), the `RIFT_LOG` → `RUST_LOG` → default filter chain with the noisy-dependency suppression, and the panic-hook-into-`tracing` helper. Registered in `architecture.md`'s repo structure when it lands (the `crates/lsp` precedent). Two consumers from day one — the constitution's 2+ threshold for extraction is met, not anticipated.
- **App wiring** (`crates/app`): replace the `#[cfg(feature = "windowed")]` logging branch with the runtime TTY check + env override; the stable file sink becomes the rotated append pair; the panic hook installs in every profile. The `windowed` feature itself stays (it gates `windows_subsystem`, which is a link-time property).
- **Daemon wiring** (`crates/daemon`): `eprintln!` → `tracing` with the shared filter chain and TTY-ruled sink selection — interactive (stderr TTY) → stderr; detached → the `--log-file`/env-configured rotated file sink, whose default path is **distinct from the launch line's redirect target** (naming pinned in the issue). The `>> log 2>&1` redirect stays as the pre-init/panic-backtrace backstop only — it catches what no subscriber can, and stays tiny because no subscriber writes to it. The daemon's throwaway `spike.rs` (and its `eprintln!` sites) is slated for deletion with the Phase 6 issues (#202–#205); the no-`eprintln!` criterion applies to what remains — don't polish code about to be deleted, sequence the grep after or around it.
- **Dev-loop fit**: `just dev-watch` (Linux) keeps console logs via the TTY check. **`just dev-windows[-watch]` runs the Windows exe through WSL binfmt interop, where stdio is a pipe relay — not a TTY** — so the Windows dev recipes pin the console-forcing env override explicitly; without it the logs would silently divert to the file sink. The stable shortcut launch is unchanged except rotation (no TTY → file).

### Out of scope

- **Log surfacing UI** — an "open log" action, an in-app ring-buffer overlay (wezterm), or Alacritty's "log path in the message bar on error": high-value follow-ups the survey itself defers until a status/message surface exists; this track is the sink/rotation foundation they will sit on.
- **Runtime filter reload** (Zed's `zlog_settings` observer) — rift has no settings store; revisit when one exists.
- **Remote log shipping / aggregation** — the daemon's logs stay on the remote host; reading them is `ssh` + the file, exactly like the stable log today.
- **Crash minidumps / crash-handler subprocess** (Zed's `crashes` crate) — panic-into-log is this track's bar; native crash capture is a separate, much heavier follow-up if ever needed.
- **Per-run PID-keyed log files with age pruning** (the wezterm model) — the survey names this as *the* model for a daemon-side file sink; this spec deliberately deviates toward a single rotated file to reuse the shared writer and keep one rotation story (own decision, not the survey's). The wezterm model is the recorded fallback if the single file proves wrong.

## Human prerequisites

None. Local files and existing channels only; no secrets, accounts, or provisioning.

## Constraints

- **The daemon's stdout is frame-only in `--connect` relay mode** (#60 framing invariant): no subscriber may ever write to stdout in that path — stderr and the file sink only. A test guards the invariant.
- **`crates/logging` must be `gpui`-free and musl-clean** — it becomes a daemon dependency (the established crate-boundary gate; verified by the `daemon-musl` CI job).
- **No new external dependencies**: `tracing`/`tracing-subscriber` are already workspace dependencies; the size rotation is a small custom writer precisely because `tracing-appender` cannot rotate by size (survey takeaway 2) — adding it would not even solve the problem.
- **Append, never truncate**: the rotated pair opens `append(true)`; per-run truncation (today's `File::create`) is the defect this track removes. Atomic rotation (copy-to-`.old` + truncate, the Zed mechanism) is acceptable — the file is a log, not state; the window-state spec's temp+rename bar does not apply here.
- **TTY detection via `std::io::IsTerminal`** (stable since Rust 1.70 — explicitly **no** `atty`/`is-terminal` crate), with one `RIFT_*` env override for the forced cases (windowed exe that should log to an attached console, piped/interop runs that should keep the console sink), mirroring Zed's force flag — not a second feature gate.
- **One writer per file pair**: log paths are keyed per executable/channel (`rift-stable.log` vs the dev exe's own pair vs the daemon's), so the side-by-side dogfooding instances never share a rotation pair — concurrent writers interleaving into one file are designed out, not handled.
- **Defaults stay developer-friendly**: the current `rift=debug,rift_ssh=debug` default filter is preserved as the fallback tier, extended by the noisy-dep suppression.
- Purely local I/O; no agent detection; no telemetry (constitution). `thiserror` in the library; no `.unwrap()` in library code.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Keep the `tracing`/`EnvFilter` facade; no custom logger, no facade switch** | Precedent-decided (Category 10 takeaways): all four reference projects roll custom loggers over `log`, but the patterns transfer directly onto rift's existing `tracing` setup — switching facades would be churn without gain. | 2026-06-12 |
| **Sink selection by runtime TTY detection, replacing the `windowed` logging gate; env override for forced cases** | Precedent-decided (Zed `zlog`): one runtime check covers dev console, windowed stable, and redirected output; the compile-time gate covers only one of the three. The `windowed` feature keeps its link-time job (`windows_subsystem`). | 2026-06-12 |
| **Size-based rotation as a `.log`/`.log.old` append pair (1 MB default), via a small custom writer** | Precedent-decided (Zed `SizedWriter`): beats per-run truncation (loses the previous run's evidence — today's defect) and beats unbounded append; `tracing-appender` rotates by time only, so the ~50-line custom writer is necessity, not preference. | 2026-06-12 |
| **Panic capture via `tracing::error!` in the hook, in every profile, into the active sinks** | Precedent-decided (Zed, wezterm): rift's stable build already does this; extending it to all sinks/profiles is the survey's explicit takeaway 4. | 2026-06-12 |
| **Daemon: TTY-ruled sinks (interactive → stderr; detached → rotated file only, distinct path); launch redirect stays as the tiny pre-init/panic backstop; stdout untouched** | Constraint-determined: the #60 framing invariant makes stdout sacred in relay mode; the current `>> log` shell append is unbounded for a long-running daemon, and an unconditional stderr subscriber would duplicate every line into that unrotatable redirect — so the stderr layer only runs when stderr is a TTY. The daemon-managed rotated sink reuses the shared writer (unified setup — the track's name); the single-rotated-file choice over wezterm's PID files is this spec's own deviation, recorded in Out of scope. | 2026-06-12 |
| **A shared `crates/logging` library** | Constraint-determined: two binaries need identical rotation/filter/panic behavior; the constitution's "extract at 2+ implementations" threshold is met exactly, and a library crate is the established pattern for headless-testable, musl-gated daemon code (`crates/explorer`, `crates/lsp` precedents). | 2026-06-12 |
| **Filter chain `RIFT_LOG` → `RUST_LOG` → built-in default with noisy-dep suppression** | Precedent-decided (Zed's `ZED_LOG`→`RUST_LOG`; wezterm's hardcoded `wgpu_core`/`wgpu_hal` suppression — directly relevant to rift's GPUI/wgpu noise). | 2026-06-12 |
| **This is a parallel tooling/DX track — no cross-milestone queue edge** | Constraint-determined: like the gallery and dogfooding-channels tracks, it is independent of the phase queue's data layers; it touches initialization code only and is immediately workable — which is why it was queued now (dogfooding pain). | 2026-06-12 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the track milestone. Created once this spec is `READY` and merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created at `READY` (Logging & diagnostics)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo build --release -p rift-daemon --target x86_64-unknown-linux-musl` still produces a static binary with `crates/logging` linked
- [ ] Rotation unit tests (pure, headless): the writer appends across restarts, rotates at the threshold into `.old` (previous `.old` replaced), never truncates the active file on open, and survives a simulated interruption without losing the pair
- [ ] Filter-chain tests: `RIFT_LOG` beats `RUST_LOG` beats the default; the default suppresses `wgpu_core`/`wgpu_hal` below error while keeping `rift=debug,rift_ssh=debug`
- [ ] TTY/no-TTY sink selection is exercised both ways (TTY → console, no TTY → file), and the env override forces each direction
- [ ] `just dev-windows-watch` shows console logs (the pinned override over the binfmt pipe relay); the daemon's detached mode writes only the rotated file — the redirect file stays empty save pre-init/panic output
- [ ] A deliberate panic in a dev-console run and in a windowed run both land thread, location, and message in the active sink (manual QA for the windowed path: read `rift-stable.log`)
- [ ] Daemon: a `grep` confirms no `eprintln!`/`println!` remains in `crates/daemon` (outside tests); a protocol round-trip test confirms stdout stays frame-only in `--connect` mode with logging active; the remote log file rotates at the threshold
- [ ] Two stable restarts in a row: the first run's tail is readable in `.old` / the pair (per-run truncation gone)
- [ ] A `grep` confirms no telemetry, no analytics, and no network log sinks
- [ ] Milestone QA (dev channel): a `dev-watch` (Linux) **and** a `dev-windows-watch` session show readable, filtered console logs; the stable channel's log pair is inspectable after a silent-death scenario

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| TTY detection misfires on the Windows windowed exe vs console-launched edge (a `windows_subsystem` exe never has a TTY, even from a terminal) | That is the correct default (file sink for the windowed exe); the env override covers the developer who wants console output from a windowed build (Alacritty's `AttachConsole` trick is noted as a future nicety, not v1). |
| Rotation races between the writer and an external reader (`tail -f`, the user reading the log) | Copy-to-`.old` + truncate keeps the active path stable (readers keep their handle; Zed ships this exact mechanism); a torn read at the rotation instant is cosmetic for a log. |
| The daemon's file sink and the launch-line `>>` redirect write the same path, or the redirect grows unbounded | Distinct paths (naming pinned in the issue) **and** no subscriber ever writes to stderr in detached mode (TTY rule) — the redirect only ever receives pre-init prints and default-hook panic backtraces, so it stays tiny without rotation. |
| Noisy-dep suppression hides a real GPU error | Suppression floor is `error` (not `off`), mirroring wezterm; `RIFT_LOG` overrides per module at runtime. |
| Scope creep toward surfacing UI / crash handlers | Hard out-of-scope list; the survey itself stages those as follow-ups on top of this foundation. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-06-12: Spec-acceptance gate. No genuinely-open decisions (the Category 10 survey pre-decided the design); the developer accepted the spec and confirmed human prerequisites as none. Spec flipped `DRAFT → READY`.
- 2026-06-12: Review gate (fresh-context Agent review, `NEEDS CHANGES` → addressed). Blocking findings folded in: (1) the daemon's stderr layer is gated on the TTY rule — an unconditional stderr subscriber would have duplicated every line into the unrotatable launch-redirect file, reintroducing the unbounded growth this track removes; the file-sink default path is now explicitly distinct from the redirect target (the in-scope/risk contradiction resolved); (2) `dev-windows[-watch]` runs through WSL binfmt interop where stdio is a pipe — the Windows dev recipes pin the console-forcing override, and milestone QA covers both dev loops. Non-blocking: the single-rotated-daemon-file choice is owned as this spec's deviation from the survey's wezterm-PID-files model; `std::io::IsTerminal` named (no `atty` crate); per-exe/channel log pairs design out concurrent writers; the `spike.rs` `eprintln!` sites are noted as Phase-6-doomed code the grep criterion must not force polishing.
- 2026-06-12: Spec created from `/loopkit:plan logging-diagnostics` (explicit-argument track, queued in the roadmap's Tracks section). The design is pre-decided by the developer's Category 10 prior-art survey (`prior-art.md`, source-verified 2026-06): TTY-based sink selection (Zed), size-rotated `.log`/`.log.old` append pair via a small custom writer (Zed; `tracing-appender` is time-only), panic-into-sinks in every profile (Zed/wezterm), `RIFT_LOG`→`RUST_LOG`→default filtering with noisy-dep suppression (Zed/wezterm), daemon on the same facade with stderr + rotated file and the stdout framing invariant untouched. Constraint-decided: the shared `crates/logging` library (2+ consumers), the parallel-track stance (no queue edge). No genuinely-open decisions — the gate is acceptance + prerequisites only.
