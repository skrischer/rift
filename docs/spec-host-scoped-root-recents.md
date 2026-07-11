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
`RootPicker::apply_dir_entries_reply` never sets `current_path`
(`root_picker.rs:430-433`), so no breadcrumb renders and the Browse surface is a
dead-end (empty card, no navigation, Create disabled). Fix: scope recent roots
per host by co-locating them on the host-keyed `RecentConnection`, seed the
picker from the current connection target (falling back to `""` → the host's
`$HOME`), and make a failed *seed* browse fall back to `$HOME` so the picker
never dead-ends.

**There are two production root-picker owners, both exhibiting the bug** — the
pre-cockpit `Shell` picker (`main.rs::show_root_picker`) and the in-cockpit
"+ New session" picker (`workspace.rs::WorkspaceView::open_root_picker`). Both
seed from `window_state.recent_roots` and record via
`window_state::record_recent_root`; both share the same `RootPicker` view
(`root_picker.rs`). The fix must cover **both** owners; the shared view is the
natural home for the seed-fallback so both inherit it.

## Outcome

- [ ] **Both** root-picker owners (pre-cockpit `Shell`, in-cockpit
      `WorkspaceView`) seed their start level from the roots picked **on the
      current connection target** (host/user/port/key/wrapper) — never a root
      picked on a different host. When the target has no recorded root, they seed
      `""` (the daemon resolves `$HOME` on that host).
- [ ] Recent project roots are persisted **per host**, co-located on the
      host-keyed `RecentConnection` as a new `recent_roots: Vec<String>`
      (most-recent-first, capped), not in a channel-global flat list.
- [ ] The channel-global `window_state.recent_roots` (and its
      `record_recent_root` / `MAX_RECENT_ROOTS`) is removed, and **both** owners'
      seed + record sites use the host-scoped store. Old state files load
      unchanged — tolerant serde ignores the now-absent field — and the stale
      cross-host value simply drops.
- [ ] Recording a connection (the session refresh on every connect/reconnect)
      **preserves** the target's existing `recent_roots`; recording a picked
      root **merges** it move-to-front into the matching target's entry. Neither
      path clobbers the other.
- [ ] A **seed** browse that returns any `DirBrowseError` falls back **once** to
      a `""` ($HOME) browse — implemented **in the shared `RootPicker` view**, so
      both owners inherit it — and the picker lands on a navigable level
      (breadcrumb present) instead of a dead-end empty card. A non-seed
      navigation error (a level already resolved, `current_path` non-empty) keeps
      today's inline-error-with-breadcrumb behavior.
- [ ] No protocol / daemon change; `PROTOCOL_VERSION` unchanged. Client-only
      (`crates/app`); the daemon's `""` → `$HOME` browse resolution is reused
      as-is.

## Scope

### In scope

- **`crates/app/src/recents.rs`**: add `recent_roots: Vec<String>` to
  `RecentConnection` (additive, `#[serde(default)]`, tolerant-load per #477).
  Add two public helpers keyed on the connection identity (`same_target` stays
  private): read a target's roots, and merge a picked root into the matching
  entry (move-to-front, cap — a `MAX_RECENT_ROOTS`-style const, value `8`,
  living here). `record` (connection refresh) must **carry over** the existing
  matching entry's `recent_roots` so a session-only reconnect does not wipe them.
- **A lib-visible connection identity.** The seed lookup and the root merge key
  on the five `same_target` fields (host/user/port/key/remote_exec_wrapper).
  That identity currently lives only in the binary (`main.rs::RecentTarget`),
  invisible to the lib's `workspace.rs`. Make the identity **lib-visible** —
  either move `RecentTarget` into `recents.rs` or introduce a
  `recents::ConnectionTarget { host, user, port, key, remote_exec_wrapper }` the
  helpers take (and that `main.rs`'s `RecentTarget` becomes / wraps) — so both
  owners and the helpers share one type. The exact placement is an
  implementation choice within this constraint (the identity MUST be reachable
  from `workspace.rs`).
- **`crates/app/src/window_state.rs`**: remove `recent_roots`,
  `record_recent_root`, `MAX_RECENT_ROOTS`, and their tests; the store keeps its
  legitimately per-channel fields (geometry, theme, fonts, visibility).
- **`crates/app/src/root_picker.rs`** (Issue A — the shared view): in
  `RootPicker::apply_dir_entries_reply`, on an **error** reply where
  `current_path` is still empty (the seed browse) **and** the errored `path`
  (echoed by the daemon) is non-empty, clear the error, set `loading`, and emit
  `RootPickerEvent::Browse(String::new())` to re-seed at `$HOME` instead of
  rendering the dead-end. Bounded: an errored `""` retry (or an error while
  `current_path` is still empty on the second pass) renders inline — no loop.
  `start_path` stays the pure "first recent root, else empty" over a slice (now
  fed the per-host slice). Both owners already forward `RootPickerEvent::Browse`
  to the daemon, so this single view change covers both pickers.
- **`crates/app/src/main.rs`** (`Shell` owner, Issue B): at the pick site
  (`RootPickerEvent::Picked`, `:1409`) record the picked root onto the current
  target's recents entry via the new helper instead of the flat store; at the
  seed site (`show_root_picker`, `:1366`) compute the seed from the current
  target's roots (helper lookup keyed on the `RecentTarget` already threaded via
  `RootPickerLaunch.recents`) instead of `window_state::load(...).recent_roots`.
- **`crates/app/src/workspace.rs`** (`WorkspaceView` owner, Issue B): the
  in-cockpit picker's seed site (`open_root_picker`, `:2076`) and pick-record
  site (`:2115`) get the same host-scoped seed + record. `WorkspaceView::new`
  (`:637`, constructed at `main.rs:1094`) currently takes only
  `window_state_path`; thread in the `recents_path` + the connection identity so
  the in-cockpit picker can seed and record per host.

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
  entry and inserts a fresh one built from the identity (which has no roots),
  refreshing session + timestamp. Connection recording runs on **every** connect
  — before any root is picked — so the fresh entry must carry over the existing
  matching entry's `recent_roots`, or every reconnect wipes the roots picked
  earlier on that host.
- **The seed-fallback lives in the shared view, not the owners.** `current_path`,
  `loading`, and `error` are private to `RootPicker`, and an owner consuming
  `rift_app` as a library cannot read them; the view already owns loading/error
  state and already emits `RootPickerEvent::Browse` for the owner to forward.
  Putting the fallback in `apply_dir_entries_reply` (a) needs no owner-side seed
  tracking, (b) avoids `browse()`'s `if self.loading { return; }` guard and the
  "folder no longer exists" banner lingering through the `$HOME` round-trip
  (the view clears the error and re-enters loading itself), and (c) covers both
  owners at once. This **reverses** an earlier draft decision that placed the
  fallback in the owner.
- **The seed-error reply must reach the view.** The owner's correlation guard
  `browse_reply_matches` admits the seed's echoed concrete path (the daemon
  echoes the requested path on an error reply), so `apply_dir_entries_reply` is
  called with the error — the precondition for the view-side fallback. Documented
  assumption: if the daemon ever echoed `""` instead of the requested path on a
  browse error, the guard would drop the seed-error and the picker would hang;
  today it echoes the concrete path (`daemon .../browse.rs`), so the fallback is
  reachable.
- **Target identity for lookup = `same_target`** (host/user/port/key/wrapper),
  the exact existing dedupe key. `main.rs`'s `RootPickerLaunch.recents` already
  threads the `(path, RecentTarget)` into the `Shell` picker; the
  `WorkspaceView` picker gains the same identity via the new
  `WorkspaceView::new` parameter.
- **Tolerant load (`docs/spec-connection-robustness.md` #477, constitution).**
  The new `recent_roots` field is additive with `#[serde(default)]`; removing
  `window_state.recent_roots` relies on serde ignoring an unknown field (no
  `deny_unknown_fields` is set on `WindowState` — verified), so an old file
  carrying it still loads. A missing/corrupt file still degrades to default.
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
| **Both** root-picker owners are in scope — pre-cockpit `Shell` and in-cockpit `WorkspaceView` | The bug and the flat-store flow exist in both (`main.rs` and `workspace.rs`); fixing only one leaves the other dead-ending and would not compile after the flat store is removed. `WorkspaceView` gains the recents identity via a new `WorkspaceView::new` parameter. | 2026-07-11 |
| The connection identity is made **lib-visible** (move `RecentTarget` to `recents.rs`, or a new `recents::ConnectionTarget`) | `workspace.rs` (lib) needs the same host identity the binary's `RecentTarget` carries; the lookup/merge helpers key on it. A lib-visible type lets both owners and the helpers share one identity. | 2026-07-11 |
| The **seed-fallback lives in the shared `RootPicker` view** (`apply_dir_entries_reply`), not in the owners | `current_path`/`loading`/`error` are private to the view and unreadable by a library consumer; the view already owns that state and emits `Browse`. Placing the fallback there needs no owner-side tracking, avoids the loading-guard + lingering-banner pitfalls, and covers both owners at once. Reverses an earlier draft that put it in the owner. | 2026-07-11 |
| **Seed** from the current target's `recent_roots.first()`, else `""` (→ `$HOME`) | A per-host seed offers only roots valid on this host; `$HOME` is always valid there. Both owners have the identity available at launch. | 2026-07-11 |
| **Remove** the flat `window_state.recent_roots`; drop old values (no migration) | A channel-global root list has no host to attribute its entries to; the field is the bug. Tolerant serde load ignores the now-absent field, so old state files load unchanged. | 2026-07-11 |
| `record` **merges/preserves** roots (never clobbers) | Connection recording (session refresh) runs on every connect, before a root is picked; a fresh entry from the identity carries no roots, so `record` must carry over the existing entry's roots or reconnecting wipes them. | 2026-07-11 |
| The fallback is bounded (once) and seed-scoped | A `""`/`$HOME` retry that itself errors renders inline (no re-fallback → no loop); a resolved-level (`current_path` non-empty) navigation error keeps today's inline-error-with-breadcrumb behavior. | 2026-07-11 |
| Split into **two issues, one spec** | Acceptance-gate, human-chosen. Issue A = the seed-`NotFound` → `$HOME` fallback (shared-view change, `root_picker.rs` only, covers both owners, independent). Issue B = host-scope the roots + remove the flat store, across both owners. Disjoint code regions; both unblocked; A recommended first. | 2026-07-11 |

## Tracking

- Milestone: created at the acceptance gate (a **bug-fix** milestone; **not** a
  roadmap phase — no roadmap-overview row, mirroring the tmux-locale fix).
- Issues: created from this spec after merge — two, per the split above. Each
  references this spec path.

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace
      excluding `rift-app`); CI `app-check` compiles `rift-app` (**both**
      `main.rs` and `workspace.rs` must compile after the flat store is removed —
      the omission that would otherwise break the build).
- [ ] Unit (`recents.rs`): `record` **preserves** an existing entry's
      `recent_roots` on a session-only reconnect (same target, different
      session); the root-merge helper move-to-fronts and caps; the target-roots
      lookup keys on `same_target`; a field-absent old JSON loads `recent_roots`
      as `[]` (tolerant-load).
- [ ] Unit (`window_state.rs`): `recent_roots` / `record_recent_root` no longer
      exist (compile-checked); an old state file carrying a `recent_roots` array
      still loads (unknown field ignored, no reset to default).
- [ ] Unit (`root_picker.rs`, seed fallback): a seed **error** reply with a
      non-empty errored path (while `current_path` is empty) emits a
      `Browse("")` event and does not render the dead-end; a `""` retry error, or
      an error on a resolved level (`current_path` non-empty), renders inline and
      emits no further `Browse` (no loop).
- [ ] Behavioural (dev-channel QA): connecting the stable channel to host A,
      picking root `/a/proj`, then connecting to host B (no recorded root) lands
      the picker on host B's `$HOME` (navigable, breadcrumb present), **not**
      host A's `/a/proj`; reconnecting to A re-seeds `/a/proj`. A stale/deleted
      seed root falls back to `$HOME` rather than a dead-end empty card. The same
      holds for the in-cockpit "+ New session" picker.
- [ ] Regression: a same-host reconnect still seeds the last root picked on that
      host; the connection recents (session refresh, timestamp, move-to-front,
      cap, wrapper distinctness) behave unchanged.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `record` clobbers roots on a session-only reconnect (the main foot-gun) | Explicit constraint + a unit test asserting an existing entry's `recent_roots` survive a same-target session refresh. |
| The in-cockpit (`workspace.rs`) copy is missed — build breaks or it still dead-ends | Both owners are explicitly in scope; the Verification requires `app-check` (compiles `workspace.rs`) and a QA check of the in-cockpit picker. |
| Old `window_state.json` files carry the flat `recent_roots` | Tolerant serde load ignores the unknown field; the stale cross-host value drops (intended). No `deny_unknown_fields` on `WindowState`. |
| The seed fallback masks a real navigation problem, or loops | In the shared view, bounded (no re-fallback from `""`) and seed-scoped (only while `current_path` is empty); a resolved-level error surfaces inline with the breadcrumb intact. |
| Co-locating roots on `RecentConnection` ties root retention to the `MAX_RECENTS = 8` host cap — a host aged out of RECENT loses its recorded roots | Acceptable for a personal tool (8 recent hosts is generous); noted so it is a conscious trade, not a surprise. |

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
- 2026-07-11: Spec-acceptance review (fresh top-tier context) verified all code
  claims TRUE and raised two blocking scope gaps, both folded in: (1) the
  in-cockpit `WorkspaceView` picker (`workspace.rs` seed `:2076`, record `:2115`,
  reply-routing `:2153`) is a full second copy with no recents identity —
  now in scope, with the identity threaded into `WorkspaceView::new` and made
  lib-visible; (2) the seed-fallback cannot live in the owner (the view's
  `current_path`/`loading`/`error` are private and the `loading` guard + error
  banner block an owner-driven re-browse) — moved into the shared
  `RootPicker::apply_dir_entries_reply`, which also makes both owners inherit it.
  Non-blocking notes folded in: the `MAX_RECENTS` cap couples root retention to
  host retention; the fallback's reachability depends on the daemon echoing the
  concrete errored path; the lookup helper keys on the lib-visible identity, not
  the binary's `RecentTarget`.
