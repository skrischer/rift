# Spec: Phase 9 — Window-state persistence

> Status: READY
> Created: 2026-06-12
> Completed: —

rift's GUI window remembers itself across restarts: bounds (position + size), maximized state, and the whole-client font scale are restored at startup instead of resetting to defaults. tmux already persists the entire process layer; what dies today on every `promote` relaunch, reboot, or dev-loop rebuild is the *client-side* window state — a daily dogfooding papercut deferred to this track by `spec-dogfooding-channels.md` ("Window-state persistence across restarts — already its own deferred track").

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] Closing and relaunching rift (including `just promote`'s detached relaunch and a reboot-then-shortcut launch) reopens the window at the previous position and size, with the previous maximized state and font scale.
- [ ] The **two dogfooding channels keep independent state**: stable and dev instances restore their own bounds/zoom and never overwrite each other's.
- [ ] State survives an unclean exit (kill, crash, reboot): persistence is change-driven (debounced) with atomic writes — **plus a best-effort flush on clean close**, so the last change inside the debounce window is never lost in the close-and-reopen flow.
- [ ] A missing, corrupt, or stale state file degrades to today's defaults — never a crash, never a refusal to start; unknown fields are tolerated (forward-compatible load).
- [ ] Restoring onto a changed monitor topology clamps the window into the visible work area (a disconnected monitor never yields an off-screen window).
- [ ] Nothing else is persisted: no tmux/session state (tmux owns that), no editor/panel layout (no premature persistence of surfaces that barely exist), no telemetry of any kind — the file holds window geometry and font scale, locally.

## Scope

### In scope

- **A small window-state store** (`crates/app`): serde-JSON schema (bounds, maximized, font size in px, schema version), tolerant load (defaults on missing/corrupt/unknown), debounced change-driven save with atomic temp+rename writes plus a best-effort flush on clean close.
- **A small `rift-terminal` API for the font size** (deliberate crate-boundary addition): the zoom state lives inside `SessionView` today; capture/restore needs read/seed access and a change notification — one narrow public surface, named here so the boundary change is deliberate (`CLAUDE.md` rule 5). The persisted value is the absolute px size, not a ratio.
- **Platform state path**: Windows `%LOCALAPPDATA%\rift\` (the directory the stable channel already owns for exe + log); Linux `$XDG_STATE_HOME/rift/` (fallback `~/.local/state/rift/`).
- **Per-channel keying**: the state file is keyed by instance channel (e.g. derived from the executable name — `rift-stable` vs `rift`), so the side-by-side dogfooding instances never share a file; exact derivation pinned in the first issue.
- **Capture + restore wiring**: observe window move/resize/maximize and font-scale changes (the existing `Ctrl+=`/`Ctrl+-` path), restore at window creation, clamp to the visible work area.

### Out of scope

- **Workspace/layout persistence** — open editor files, panel/dock layout, scroll positions: future surfaces (editor track) persist via their own spec once they exist; the schema is versioned so this can extend without migration pain.
- **tmux/session state** — tmux is the persistence for the process layer (sessions, windows, panes survive rift restarts by design); rift never duplicates it.
- **A settings/config layer** — this is *state*, not configuration; the dogfooding-channels decision ("knobs are env vars, no new config layer") stands untouched.
- **Sync across machines / cloud state** — local file only.
- **A database** — Zed's `WorkspaceDb` (SQLite) is the precedent for *what* to persist and *when* (debounced), not for the storage engine; bounds + zoom in JSON need no database (no new dependency, minimal solution).

## Human prerequisites

None. Local file I/O only; no secrets, accounts, or provisioning.

## Constraints

- **No new dependencies**: `serde`/`serde_json` are workspace dependencies; platform dirs resolve via environment variables (`LOCALAPPDATA`, `XDG_STATE_HOME`) with documented fallbacks — no `dirs` crate for two paths.
- **Atomic writes** (temp + rename), the same pattern `spec-editor.md` pins for the daemon buffer service (a design precedent — that service is not yet implemented, so there is no code to mirror, only the pattern) — a crash mid-save never corrupts the state file; a corrupt file degrades to defaults anyway (belt and suspenders).
- **Debounced saves on change plus flush on clean close** (the Zed precedent does both: `SERIALIZATION_THROTTLE_TIME` throttling *and* close-path saves) — no save per pixel of a drag, no lost last change on close. Change-driven remains the primary mechanism: the dev channel only ever dies by `taskkill /F`, where no close path runs. Debounce cadence in the Zed order (~100–250 ms), pinned in the issue.
- **Per-channel isolation is mandatory**, not cosmetic: the dogfooding mirror runs stable and dev side by side (`spec-dogfooding-channels.md`); a shared file would make the last writer win and desync both.
- **Forward-compatible schema**: versioned, unknown fields ignored on load (serde defaults) — the editor-track extension must not require a migration.
- **Clamp, don't trust**: restored bounds are validated against the current display topology; invalid/off-screen bounds fall back to centered defaults.
- **Store logic is headless-testable** (pure serde round-trip + clamp functions); only the GPUI wiring is QA-gated — consistent with the worktree/CI split (`app-check` compiles, milestone QA validates visually).
- Purely client-side: no protocol, daemon, or tmux changes; no agent detection. `thiserror` in library code; no `.unwrap()`.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Own deferred track, split from dogfooding channels** | `spec-dogfooding-channels.md` Out of scope (2026-06-10): window-state persistence is its own roadmap track, not folded into the channel setup. | 2026-06-10 |
| **v1 state set = window bounds + maximized + font scale** — nothing more | Constraint-determined: these are exactly the client-side losses felt today (promote relaunch, reboot, dev rebuild); everything else either belongs to tmux (process layer) or to surfaces that barely exist (editor/panels — premature). Minimal-solution rule. | 2026-06-12 |
| **serde-JSON file in the platform state dir; no database** | Constraint-determined: Zed's `WorkspaceDb` validates the *pattern* (persist on change, debounced), but SQLite for bounds+zoom violates the no-new-dependency and minimal-solution rules; `%LOCALAPPDATA%\rift` is already rift's Windows home (stable exe, log). | 2026-06-12 |
| **Per-channel state keying derived from the instance identity** (executable name; exact derivation pinned in the first issue) | Constraint-determined: the dogfooding mirror runs two instances side by side with distinct image names by design (`rift-stable.exe` vs `rift.exe` — the same property the dev loop's `taskkill` relies on); reusing it adds no new config knob. | 2026-06-12 |
| **Change-driven debounced saves as the primary mechanism, plus a best-effort flush on clean close** | Constraint-determined: the stable channel dies by kill/reboot more often than by clean exit (detached process, `promote` relaunch, `taskkill /F` in the dev loop), so save-on-exit alone would lose exactly the sessions this spec exists for — but close-flush alone-debounce would lose the last change in the close-and-reopen flow. Zed does both (throttled serialization + close-path saves). | 2026-06-12 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 9 milestone. Created once this spec is `READY` and merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created at `READY` (Phase 9 — Window-state persistence)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Store round-trip tests: schema serializes/deserializes; unknown fields and a future schema version load with defaults for the rest; corrupt/truncated files yield full defaults (no panic)
- [ ] Clamp tests (pure function): off-screen bounds, a monitor that no longer exists, and degenerate sizes all land inside the visible work area
- [ ] Atomic-write test: a simulated interruption never leaves a corrupt file in place
- [ ] Restart restores position, size, maximized state, and font size (manual QA: close/reopen — including a geometry change made *immediately* before closing, exercising the close-flush — `just promote` relaunch, reboot-then-shortcut)
- [ ] Stable and dev instances running side by side persist independently; restarting one never moves the other
- [ ] Deleting the state file yields a clean first-launch default; the file is recreated on the next change
- [ ] A `grep` confirms no telemetry, no network I/O, and no agent detection in the persistence path
- [ ] Milestone QA (dev channel): the daily-driver restart loop (promote, reboot) feels seamless — the window is where it was

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| GPUI's window-bounds observation/restore API is awkward or platform-inconsistent (Windows vs X11) | Zed restores window bounds through GPUI in production — read its wiring first (`crates/workspace` persistence); validate on the Windows host early (it is the primary loop); X11 quirks bound to the clamp path. |
| Saving on every move/resize event floods I/O | Debounce (Zed precedent); writes are tiny and atomic. |
| Restored state fights the dev loop's frequent restarts (watch rebuilds) | That is the feature, not a hazard — the dev window reopening in place is exactly the papercut being fixed; per-channel keying keeps stable unaffected. |
| Monitor topology changes between sessions (dock/undock, RDP) | Clamp-don't-trust constraint; centered-default fallback; clamp logic is a tested pure function. |
| Two instances of the *same* channel run simultaneously (e.g. a double shortcut launch) and write one file | Accepted: atomic writes rule out corruption; last writer wins — the same property the per-run stable log already has. Single-instance enforcement is out of scope. |
| Scope creep toward workspace persistence (open files, panels) | Hard out-of-scope; the versioned schema is the extension point, reserved for the editor track's own spec. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-06-12: Spec created from `/loopkit:plan` (loop mode — roadmap Phase 9, the last unplanned phase). Recorded constraint-determined: the v1 state set (bounds + maximized + font size), serde-JSON in the platform state dir (no database, no new dependency), per-channel keying from instance identity, change-driven debounced atomic saves, clamp-don't-trust restore, and the hard workspace-persistence exclusion. No genuinely-open decisions — the gate is acceptance + prerequisites only.
- 2026-06-12: Spec-acceptance gate. No genuinely-open decisions; the developer accepted the spec and confirmed human prerequisites as none. Spec flipped `DRAFT → READY`.
- 2026-06-12: Review gate (fresh-context Agent review, `NEEDS CHANGES` → addressed). Blocking finding folded in: debounce-only saving would lose the last change in the close-and-reopen flow — the mechanism is now debounced change-driven saves **plus** a best-effort flush on clean close (the Zed precedent does both; change-driven stays primary since the dev channel only dies by `taskkill /F`). Non-blocking: the temp+rename citation now names `spec-editor.md` as a design precedent (no implemented code to mirror yet); the `rift-terminal` font-size API is a named, deliberate crate-boundary addition (persisted value is absolute px, not a ratio); same-channel duplicate instances acknowledged as last-writer-wins. The reviewer confirmed: exe-name keying sound per platform, GPUI APIs present in the pinned checkout (`observe_window_bounds`, `window_bounds()`, `WindowBounds::Maximized` carrying inner bounds), theme correctly omitted (no runtime switcher), XDG fallback necessary and correct, no-`dirs`-crate the right call.
