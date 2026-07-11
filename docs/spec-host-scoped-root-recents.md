# Spec: Host-scope the root-picker recents — a project root belongs to its host

> Status: READY
> Created: 2026-07-11

rift persists two client-side recents stores, keyed inconsistently.
`recents.rs::RecentConnection` is **host-keyed** (`same_target` =
host/user/port/key/wrapper) and already co-locates per-host state (the last
`session` name). `window_state.rs::recent_roots` is a **flat `Vec<String>`
keyed only by channel** (stable/dev), with no host association. But a project
root is host-specific — the path only exists on the host it was picked on — so
seeding the root picker from a channel-global list offers host B a root picked
on host A. On B that path does not exist, so the daemon's `QueryDirEntries`
answers `DirBrowseError::NotFound`; and because the *seed* browse failed,
`apply_dir_entries_reply` never sets `current_path` (`root_picker.rs:430-433`),
so no breadcrumb renders and the Browse surface is a dead-end (empty card, no
navigation, Create disabled). Fix: scope recent roots per host by co-locating
them on the host-keyed `RecentConnection`, seed the picker from the current
connection target (falling back to `""` → the host's `$HOME`), and make a
failed *seed* browse fall back to `$HOME` so the picker never dead-ends.

## Outcome

- [ ] The root picker's start level seeds from the roots picked **on the
      current connection target** (host/user/port/key/wrapper) — never a root
      picked on a different host. When the target has no recorded root, it seeds
      `""` (the daemon resolves `$HOME` on that host).
- [ ] Recent project roots are persisted **per host**, co-located on the
      host-keyed `RecentConnection` as a new `recent_roots: Vec<String>`
      (most-recent-first, capped), not in a channel-global flat list.
- [ ] The channel-global `window_state.recent_roots` (and its
      `record_recent_root` / `MAX_RECENT_ROOTS`) is removed. Old state files
      load unchanged — tolerant serde ignores the now-absent field — and the
      stale cross-host value simply drops.
- [ ] Recording a connection (the session refresh on every connect/reconnect)
      **preserves** the target's existing `recent_roots`; recording a picked
      root **merges** it move-to-front into the matching target's entry. Neither
      path clobbers the other.
- [ ] A **seed** browse that returns any `DirBrowseError` falls back to a `""`
      ($HOME) browse, so the picker always lands on a navigable level
      (breadcrumb present) instead of a dead-end empty card. A non-seed
      navigation error (a level already resolved, `current_path` non-empty)
      keeps today's inline-error-with-breadcrumb behavior.
- [ ] No protocol / daemon change; `PROTOCOL_VERSION` unchanged. Client-only
      (`crates/app`); the daemon's `""` → `$HOME` browse resolution is reused
      as-is.

## Scope

### In scope

- **`crates/app/src/recents.rs`**: add `recent_roots: Vec<String>` to
  `RecentConnection` (additive, `#[serde(default)]`, tolerant-load per #477); a
  small public helper to read the roots for a target (`same_target` stays
  private) and a helper to merge a picked root into the matching entry
  (move-to-front, cap — reuse a `MAX_RECENT_ROOTS`-style const, value `8`).
  `record` must **carry over** the existing matching entry's `recent_roots` so a
  session-only reconnect does not wipe them.
- **`crates/app/src/window_state.rs`**: remove `recent_roots`,
  `record_recent_root`, and `MAX_RECENT_ROOTS`; the store keeps its legitimately
  per-channel fields (geometry, theme, fonts, visibility).
- **`crates/app/src/main.rs`**: at the pick site (`RootPickerEvent::Picked`,
  ~`:1408`), record the picked root onto the **current target's** recents entry
  instead of the flat store; at the seed site (`show_root_picker`, ~`:1366`),
  compute the seed from the current target's `recent_roots.first()` (look up the
  matching entry in the recents file via the new helper) instead of
  `window_state::load(...).recent_roots`; in the Browse-reply routing, on a
  **seed** browse `DirBrowseError`, re-issue a `""` browse once.
- **`crates/app/src/root_picker.rs`**: `start_path` stays the pure
  "first recent root, else empty" over a slice (now fed the per-host slice); if
  a pure seam helps, a tiny pure helper for the "seed browse failed → browse
  \"\"" decision, unit-tested. The view keeps rendering an error reply inline
  without tearing down; the seed-fallback is the owner's (main.rs) routing, not
  the view's.

### Out of scope

- The session-name recents behavior (`RecentConnection.session`,
  move-to-front/timestamp, `Preferred` reattach) — unchanged.
- Any protocol / daemon change; the daemon's `""` → `$HOME` `QueryDirEntries`
  resolution — reused, not modified.
- **Migrating** the old flat `recent_roots` values into a host entry —
  impossible (a channel-global list has no host attribution); the values are
  dropped, which is the intended outcome (they are the bug).
- A recents-of-roots list **UI** — none exists; only the picker seed consumes
  roots.
- Host-scoping the other `window_state` fields (theme / geometry / fonts /
  visibility) — those are legitimately per-channel (one window per channel),
  not per-host.

## Constraints

- **`record` must not clobber roots.** It currently retain-removes the matching
  entry and inserts a fresh one built from `RecentTarget` (which has no roots),
  refreshing session + timestamp. Connection recording runs on **every** connect
  — before any root is picked — so the fresh entry must carry over the existing
  matching entry's `recent_roots`, or every reconnect wipes the roots picked
  earlier on that host.
- **Target identity for lookup = `same_target`** (host/user/port/key/wrapper),
  the exact existing dedupe key. Both the seed lookup and the root merge key on
  it; `RecentTarget` (main.rs) already carries all five fields, and
  `RootPickerLaunch.recents` already threads the `(path, RecentTarget)` into the
  picker launch, so the identity is available at both the seed and the pick.
- **Tolerant load (`docs/spec-connection-robustness.md` #477, constitution).**
  The new `recent_roots` field is additive with `#[serde(default)]`; removing
  `window_state.recent_roots` relies on serde ignoring an unknown field (no
  `deny_unknown_fields` is set on `WindowState`) — an old file carrying it still
  loads. A missing/corrupt file still degrades to default, never a crash.
- **Seed-fallback is bounded and seed-scoped.** Fall back **once**, only from a
  non-empty seed to `""`; a `""` seed that itself fails shows the inline error
  (no re-fallback → no loop). Scoped to the seed (the first browse, while
  `current_path` is still empty) so an explicit navigation into a since-deleted
  subfolder keeps the inline-error-with-breadcrumb behavior (that level's parent
  is resolved, so the breadcrumb is present and the user is not stuck).
- **No `.unwrap()` in library code** (constitution); both stores are already
  `thiserror` + tolerant-load. Client-only, no protocol change,
  `PROTOCOL_VERSION` unchanged. Agent-agnostic — a per-host root list is
  host-signal hygiene, no agent awareness.

## Prior art

- `docs/prior-art.md` — **Zed remote architecture**: `TrustedWorktrees` gates
  LSP execution by **per-host** trust state, and `LspStore` splits into
  `Local`/`Remote` per connection — the precedent that host-specific state in a
  remote IDE is keyed per host, not globally. rift's recent roots follow the
  same rule.
- In-repo precedent: `crates/app/src/recents.rs::RecentConnection` is **already**
  host-keyed (`same_target`) and **already** co-locates per-host state (the
  `session` field). Adding `recent_roots` is the symmetric extension — same key,
  same file, same move-to-front/cap shape (`record`) — not a new mechanism.

## Human prerequisites

- none — a `crates/app` client-side code change; no secret, provisioning, or
  account required to build or verify. (The behavioral check reuses the existing
  dogfooding hosts.)

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Recent roots are host-scoped by **co-locating on `RecentConnection`** (not a separate host-keyed file/map) | The recents store is already host-keyed (`same_target`) and already holds per-host state (`session`); adding `recent_roots` reuses that key and file. A separate map duplicates the host-key concept for no gain. | 2026-07-11 |
| **Seed** from the current target's `recent_roots.first()`, else `""` (→ `$HOME`) | The picker knows the connection target at launch (`RootPickerLaunch.recents` carries the `RecentTarget`); a per-host seed offers only roots valid on this host, and `$HOME` is always valid there. | 2026-07-11 |
| **Remove** the flat `window_state.recent_roots`; drop old values (no migration) | A channel-global root list has no host to attribute its entries to; the field is the bug. Tolerant serde load ignores the now-absent field, so old state files load unchanged. | 2026-07-11 |
| `record` **merges/preserves** roots (never clobbers) | Connection recording (session refresh) runs on every connect, before a root is picked; a fresh entry from `RecentTarget` carries no roots, so `record` must carry over the existing entry's roots or reconnecting wipes them. | 2026-07-11 |
| A failed **seed** browse falls back **once** to a `""` / `$HOME` browse | On a seed `NotFound`, `apply_dir_entries_reply` does not set `current_path` (`root_picker.rs:430`), so no breadcrumb renders → dead-end. Falling the seed back to `$HOME` guarantees a navigable landing; bounded (no re-fallback from `""`) and seed-scoped so a resolved-level navigation error keeps the inline-error-with-breadcrumb behavior. | 2026-07-11 |
| Split into **two issues, one spec** | Acceptance-gate, human-chosen. Issue A = the seed-`NotFound` → `$HOME` fallback (an immediate robustness net, independent). Issue B = host-scope the roots + remove the flat store. Both unblocked; A recommended first. | 2026-07-11 |

## Tracking

- Milestone: created at the acceptance gate (a **bug-fix** milestone; **not** a
  roadmap phase — no roadmap-overview row, mirroring the tmux-locale fix).
- Issues: created from this spec after merge — two, per the split above. Each
  references this spec path.

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace
      excluding `rift-app`); CI `app-check` compiles `rift-app`.
- [ ] Unit (`recents.rs`): `record` **preserves** an existing entry's
      `recent_roots` on a session-only reconnect (same target, different
      session); the root-merge helper move-to-fronts and caps; a field-absent
      old JSON loads `recent_roots` as `[]` (tolerant-load); the target-roots
      lookup keys on `same_target`.
- [ ] Unit (`window_state.rs`): `recent_roots` / `record_recent_root` no longer
      exist (compile-checked); an old state file carrying a `recent_roots` array
      still loads (unknown field ignored, no reset to default).
- [ ] Unit (seed fallback): the pure "seed browse failed → browse \"\"" decision
      is tested, including that a `""` seed failure does **not** re-fall-back
      (no loop) and that a resolved-level (`current_path` non-empty) error does
      not trigger the fallback.
- [ ] Behavioural (dev-channel QA): connecting the stable channel to host A,
      picking root `/a/proj`, then connecting to host B (no recorded root) lands
      the picker on host B's `$HOME` (navigable, breadcrumb present), **not**
      host A's `/a/proj`; reconnecting to A re-seeds `/a/proj`. A stale/deleted
      seed root falls back to `$HOME` rather than a dead-end empty card.
- [ ] Regression: a same-host reconnect still seeds the last root picked on that
      host; the connection recents (session refresh, timestamp, move-to-front,
      cap, wrapper distinctness) behave unchanged.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `record` clobbers roots on a session-only reconnect (the main foot-gun) | Explicit constraint + a unit test asserting an existing entry's `recent_roots` survive a same-target session refresh. |
| Old `window_state.json` files carry the flat `recent_roots` | Tolerant serde load ignores the unknown field; the stale cross-host value drops (intended). No `deny_unknown_fields` on `WindowState` — verified in scope. |
| The seed fallback masks a real navigation problem | Bounded (no re-fallback from `""`) and seed-scoped (only while `current_path` is empty); a resolved-level error still surfaces inline with the breadcrumb intact. |
| A host genuinely has no `$HOME` (exotic image) | The daemon's `""` resolution is pre-existing behavior; if it also fails, the inline error shows (no worse than today, and no loop). |

## Decision log

- 2026-07-11: Root-caused during devenv QA. The stable channel's root picker
  seeded a WSL path (`/home/developer/CascadeProjects/rift`) against the VPS /
  devenv container host, where it does not exist → `DirBrowseError::NotFound` →
  a dead-end Browse card (no breadcrumb, because a failed seed leaves
  `current_path` empty at `root_picker.rs:430`). Cause: `window_state.recent_roots`
  is channel-global, not host-scoped, while the sibling `recents` store is
  host-keyed. Cleaning the remote host could not fix it — the stale value is
  client-side state on the Windows host. Fix: co-locate recent roots on the
  host-keyed `RecentConnection`, seed per-target (else `$HOME`), and fall a
  failed seed browse back to `$HOME`. Client-only, no protocol change. Split
  into two issues under one spec at the acceptance gate (human-chosen): the seed
  fallback (immediate net) and the host-scoping (root-cause fix).
