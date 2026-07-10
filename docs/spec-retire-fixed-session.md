# Spec: Retire the fixed RIFT_SESSION default — connect-and-list session model

> Status: READY
> Created: 2026-07-10
> Completed: —

Move rift's default session behaviour from a hardcoded fixed session
(`RIFT_SESSION=rift`, pinned by every launch recipe) to the already-shipped
"connect to the host, then see and pick from the live session list" model. At v1
the product becomes host-agnostic: no baked session name, the session list /
root picker (Phases 33 + 36) is the default path on every launch. The launch
recipes stop setting `RIFT_SESSION`; the shared-session mirror of the two
dogfooding channels is re-expressed as "both instances attach the same session
on the same host" via the recents reattach path, not a fixed env name.

## Outcome

- [ ] No launch path sets `RIFT_SESSION` by default: `just dev-windows[-watch]`,
      `just promote`, and `just stable` launch with `RIFT_SESSION` unset, so on
      connect the app resolves `SessionIntent::Pick` — the live session list when
      the host has sessions (Phase 33), the remote root picker when it has none
      (Phase 36) — never a silent `Fixed("rift")` auto-attach at the baked root.
- [ ] `RIFT_SESSION` is **removed entirely**: `SessionIntent::Fixed`,
      `session_intent_from_env`, and its three `main.rs` consumers
      (~L916/942/1622-1626) are deleted; the env var is no longer read; every
      connect resolves `Pick`. Dev-isolation (a throwaway `rift-dev` session) is
      done by picking/creating that session in the picker, not via env.
- [ ] The dogfooding-channels mirror is re-specified: stable + dev no longer
      auto-share session `rift` via a baked env; they mirror by attaching the same
      session on the same host — each channel picks it once, then later launches of
      that channel reattach it via the recents `Preferred` path (a remembered
      still-live session attaches directly, no picker). Recents are per-channel
      (`rift-stable-recents.json` vs `rift-dev-recents.json`), so each channel
      records its own first pick. `docs/spec-dogfooding-channels.md` and the
      `CLAUDE.md` dogfooding-channels section reflect this.
- [ ] Empty **and** unset `RIFT_SESSION` both resolve to `Pick`
      (`session_intent_from_env`), verified — the recipe change depends on it.
- [ ] No protocol / daemon change; the connect-and-list UI (Phases 33/36) is
      reused unchanged; `PROTOCOL_VERSION` unchanged.

## Scope

### In scope

- **Launch recipes (`justfile`)**: stop setting `RIFT_SESSION` entirely — drop the
  `rift_session := env("RIFT_SESSION", "rift")` variable, the `RIFT_SESSION` export
  and its `WSLENV` entry in `_launch-windows`, and the literal `rift` passed by
  `promote` / `stable`. With the knob removed there is no override to preserve; the
  recipes never set the var.
- **Remove the `RIFT_SESSION` env knob** (acceptance-gate decision): delete
  `connection_screen.rs::session_intent_from_env` + `SessionIntent::Fixed` and its
  three functional consumers in `crates/app/src/main.rs` — the eager-recents-record
  branch (~L916), `is_fixed_intent` gating direct-attach vs picker (~L942), and the
  `initial_session`/`preferred_session` seeding match (~L1622-1626) — plus the
  `RIFT_SESSION` doc-comments; the env var is no longer read. Compiler-enforced, so
  no consumer is silently missed.
- **Docs**: `docs/spec-dogfooding-channels.md` (a live operational contract whose
  Outcome — "`RIFT_SESSION` (default `rift`)" — becomes factually wrong, so it is
  **edited**) + `CLAUDE.md` (a symlink to `AGENTS.md`; the dogfooding-channels
  section and every `RIFT_SESSION` mention) + `docs/roadmap.md` (the "attaches
  directly (dogfooding fast-path)" / "stays the picker-skipping fast-path" notes
  at ~L189/L252 — the latter goes stale even under keep-as-override, since the
  fast-path is demoted from default to override) updated to the connect-and-list
  default and the recents-based mirror. The `crates/protocol` doc-comments and
  `docs/protocol.md` `RIFT_SESSION` mentions are left as historical phase notes.
  The Paper "Session flows" artboard's `RIFT_SESSION` fast-path route is
  re-annotated (deprecated/optional) at the visual-QA gate.

### Out of scope

- The session list / picker / root-picker UI and the per-session `@root`
  mechanics — already shipped (Phases 33, 35, 36); this only stops bypassing them.
- `RIFT_PROJECT_ROOT` / `RIFT_DEFAULT_PROJECT_ROOT` — the daemon's fallback
  watched root when a session carries no `@root`; orthogonal to the session name,
  unchanged here.
- Any protocol / daemon change; any new UI surface.
- Removing the other launch env knobs (SSH host/user/port/key, daemon binary,
  exec wrapper) — they stay env-configured with working defaults.

## Constraints

- **The connect-and-list behaviour already exists.** The app resolves `Pick` on
  empty/unset `RIFT_SESSION` (`crates/app/src/connection_screen.rs`
  `session_intent_from_env`, ~L243: `Some(name) if !name.is_empty() => Fixed, _ =>
  Pick`), and the session list (Phase 33) + zero-session root picker (Phase 36)
  are shipped. This phase retires the default bypass, it does not build a flow.
- **Agnostic direction (`docs/vision.md`).** A baked `rift` session name is a
  personal-tool artifact; v1 moves toward host-agnostic "connect and list". The
  change must not reintroduce any agent- or host-specific default.
- **The dogfooding mirror must survive** — the two channels must still be able to
  show the same session side by side, re-expressed via same-session attach
  (recents `Preferred`), not a fixed env name.
- **No protocol / daemon change**; `PROTOCOL_VERSION` unchanged; client + tooling
  + docs only.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Default flips to **connect-and-list** (`Pick`); recipes stop pinning `RIFT_SESSION` | The user's v1 direction: host-agnostic, "connect and see which sessions exist". The picker/list is already shipped (Phases 33/36); the fixed default is the only thing bypassing it. | 2026-07-10 |
| The recipe change is **default `rift_session` → empty** + omit the export when empty; a user-set `RIFT_SESSION` is still honoured | One change that both flips the default and preserves the documented `RIFT_SESSION=rift-dev` dev-isolation override. | 2026-07-10 |
| The **dogfooding mirror is re-expressed via recents `Preferred`** (same-session attach), not a baked env | The user accepts manually attaching the same session across channels; the recents reattach already makes a remembered still-live session attach directly, giving the mirror back without a fixed name. | 2026-07-10 |
| **No protocol / daemon change**; client + justfile + docs only | The UI is shipped; this only removes the bypass and updates tooling/docs. | 2026-07-10 |
| `RIFT_SESSION` knob is **removed entirely** — `SessionIntent::Fixed`, `session_intent_from_env`, and its three `main.rs` consumers (~L916/942/1622-1626) are deleted; every connect resolves `Pick` | Resolved at the spec-acceptance gate. The knob is an obsolete early-project artifact; retiring it (not just demoting it) is the clean host-agnostic v1 end state and strands no capability — dev-isolation (`rift-dev`) is done by picking/creating that session in the picker, not via env. Compiler-enforced removal. | 2026-07-10 |

## Prior art

- Session-management prior-art index (Phases 32–33) and Session ↔ project-root
  index (Phases 34–36) in `docs/prior-art.md` — the connect → list → pick/create
  model this makes the default.
- `docs/prior-art.md` Category 3 (tmux control mode: `list-sessions`; iTerm2) and
  Claude Squad (Category 4 — a tmux session manager that lists/attaches sessions
  with no baked session name) — the canonical "connect and list" reference; a
  fixed session default is the anomaly being retired.
- Supersedes the "`RIFT_SESSION` is the picker-skipping fast-path" stance recorded
  in `spec-post-connect-picker.md` (Phase 33) and `spec-session-root-picker.md`
  (Phase 36). Those two are historical phase records, so they are
  decision-log-superseded here, not edited; `spec-dogfooding-channels.md` is a
  live operational contract with a now-false Outcome, so it is edited directly.

## Human prerequisites

- none — config + docs only; no secret, provisioning, or account is required to
  build or test this.

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace
      excluding `rift-app`); `app-check` compiles `rift-app`.
- [ ] Recipe inspection: `just dev-windows`, `just promote`, `just stable` no
      longer reference or export `RIFT_SESSION` at all.
- [ ] Unit / build (`crates/app`): `session_intent_from_env` and
      `SessionIntent::Fixed` no longer exist (compile-checked); the connect path
      resolves `Pick` with no env read; the old `Fixed`-path tests are removed.
- [ ] Behavioural (dev-channel QA): `just dev-windows-watch` with no
      `RIFT_SESSION` lands on the session list (host has sessions) or the root
      picker (none) — never a silent `rift` auto-attach at the baked root. A
      second channel reattaches the same session via its recent (mirror holds).
- [ ] Docs: `docs/spec-dogfooding-channels.md` + `CLAUDE.md` describe the
      connect-and-list default and the recents-based mirror; no stale
      "`RIFT_SESSION=rift` shared session" instruction remains.

## Risks and mitigations

- **Mirror regression** — the two channels no longer auto-share a session.
  Mitigation: the recents `Preferred` reattach makes a remembered still-live
  session attach directly, so after the first pick the channels re-mirror
  automatically; documented in the dogfooding-channels spec.
- **A muscle-memory `RIFT_SESSION=rift` in a shell profile** — with the knob
  removed the env var is simply ignored, so it cannot re-enable the old fast-path.
  No mitigation needed; noted so the disappearance of the doc-referenced knob is
  expected.
- **The stable channel launches detached** — with no session it lands on the
  connect screen. Today `promote` / `stable` pass a literal `rift` and auto-attach;
  after this change they land on the connect screen instead (the Phase-20 startup
  state — the same state an env-free desktop-shortcut launch already opens to), and
  the user connects. Not a regression, but a deliberate behaviour change for the
  detached launches, called out here.

## Tracking

- Design doc: this spec.
- Milestone + issues: created at the spec-acceptance gate / after merge.

## Decision log

- 2026-07-10: Spec drafted. Retires the fixed `RIFT_SESSION` default in favour of
  the shipped connect-and-list model (Phases 33/36); the launch recipes stop
  pinning `rift`; the dogfooding mirror moves to recents-based same-session
  attach. Supersedes the "`RIFT_SESSION` stays the picker-skipping fast-path"
  stance of Phases 33/36. One open decision (knob keep vs remove) carried to the
  acceptance gate.
- 2026-07-10: Spec-acceptance gate. Open decision resolved — **`RIFT_SESSION` is
  removed entirely** (not kept as an override): `SessionIntent::Fixed`,
  `session_intent_from_env`, and its three `main.rs` consumers are deleted and the
  env var is no longer read; every connect resolves `Pick`. Dev-isolation moves to
  the picker. Spec-review refinements folded in pre-acceptance (doc-scope now
  includes `docs/roadmap.md`; per-channel recents; risk wording). Human
  prerequisites: none. Status `DRAFT` → `READY`.
