# Spec: Remote exec wrapper — connect-card field + recents persistence

> Status: DRAFT
> Created: 2026-07-10
> Completed: —

Surface `RIFT_REMOTE_EXEC_WRAPPER` as an editable field on the Connection
screen's connect card and persist it per host/target in the recents store, so a
container connection can be set up and re-run from the UI without an env var.

## Outcome

- [ ] The connect card has a labeled, optional **Remote exec wrapper** input
      (alongside host / user / port / key). Its value is what the connection
      actually uses: at connect it is threaded onto `SshConnection` via
      `with_remote_exec_wrapper`, so a non-empty wrapper connects one hop deeper
      (e.g. `docker exec -i <container>`) and an empty field is a normal host
      connection (byte-for-byte passthrough).
- [ ] The field is **prefilled**, editable, from the same value
      `resolve_remote_exec_wrapper()` resolves today (runtime
      `RIFT_REMOTE_EXEC_WRAPPER` over the `RIFT_DEFAULT_REMOTE_EXEC_WRAPPER`
      compile-time bake) when the card opens fresh; selecting a RECENT entry
      prefills it from that entry's stored wrapper instead.
- [ ] The recents store persists the wrapper per entry: reconnecting from a
      RECENT container connection restores its wrapper, so the recent stays
      functional (a container recent without its wrapper would connect to the
      bare host, not the container).
- [ ] Loading a pre-existing recents file (entries written before this change)
      still works — a missing wrapper field degrades to empty (None), a normal
      host connection.
- [ ] No protocol change and no daemon change: this is a client-side connect /
      recents surface over the existing `SshConnection` wrapper mechanism.

## Scope

### In scope

- A **connect-card input field** for the wrapper in
  `crates/app/src/connection_screen.rs`, mirroring the existing host / user /
  port / key `InputState` fields (labeled 38px input, optional), whose value is
  threaded to `SshConnection::with_remote_exec_wrapper` at the single connect
  site in `crates/app/src/main.rs`.
- **Prefill precedence** for the field's initial value: a selected RECENT
  entry's stored wrapper, else `resolve_remote_exec_wrapper()` (runtime env over
  compile-time bake), else empty. The field is authoritative at connect (the
  user can override the prefill); an empty/whitespace field resolves to `None`.
- **Recents persistence**: extend `RecentConnection`
  (`crates/app/src/recents.rs`) with the wrapper, recorded on connect, restored
  as the prefill when a recent is selected, and shown on the RECENT row as a
  muted trailing indicator so a container connection is distinguishable from a
  plain one (exact visual form settled at the milestone visual-QA gate).

### Out of scope

- Any change to the wrapper **mechanism** — `exec::wrap_command`, the
  `SshConnection` builder/threading, and the `-i`-not-`-t` / absolute-path /
  legacy-path rules are settled by the archived
  `docs/archive/spec-remote-exec-wrapper.md`; this spec only adds the UI + recents
  surface over them.
- A picker / dropdown of known containers, wrapper validation, or auto-detection
  of `docker`/`podman` targets — the field is free text (the user's
  responsibility, exactly as the env var is today).
- Per-session (as opposed to per-connection) wrapper scoping; the wrapper is a
  connection-level concern (it wraps every non-PTY exec on that `SshConnection`).
- Legacy terminal path: the wrapper is only coherent on the daemon terminal
  path, never `RIFT_TERMINAL_LEGACY` (archived spec) — unchanged here.

## Constraints

- **Client-only, no protocol change.** The wrapper already lives on
  `SshConnection` (`crates/ssh`) and is resolved app-side
  (`resolve_remote_exec_wrapper`, `crates/app/src/main.rs`). This spec touches
  only `crates/app` (connect card + recents); `crates/protocol` and
  `crates/daemon` are untouched, `PROTOCOL_VERSION` unchanged.
- **Empty is `None`.** An empty/whitespace field (and a legacy recents entry
  with no wrapper) must resolve to `None` — byte-for-byte passthrough, a normal
  host connection — matching `resolve_remote_exec_wrapper`'s existing
  `filter(|s| !s.trim().is_empty())`.
- **Recents schema is additive.** `RecentConnection` is `#[serde(default)]` with
  a tolerant `load` (`crates/app/src/recents.rs`): adding a `String` field with a
  default keeps old files loadable (the connection-robustness contract, #477).
- **Env/bake still work with the field absent.** With no card interaction, the
  effective wrapper must remain what `resolve_remote_exec_wrapper()` yields today
  — the env-driven stable/dogfooding path (archived spec's decision) must not
  regress.
- **Config-surface convention.** SSH connection config in rift is env-prefilled
  into editable card fields (`docs/workflow.md`; host/user/port/key already work
  this way); the wrapper field extends that same pattern, it does not introduce a
  new config mechanism.
- **`-i` not `-t`.** A wrapper carrying `-t` corrupts the binary transport
  (archived spec). The field is free text and does not enforce this; a short
  placeholder / helper hint documents the `docker exec -i` shape (the daemon
  handshake already fails loudly on a `-t` wrapper).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The **card field is authoritative at connect**, prefilled (recent → else env → else bake), empty → `None` | Mirrors host/user/port/key exactly: the card field is the source of truth the connect uses, seeded from recents/env/defaults. Keeps `resolve_remote_exec_wrapper`'s runtime-over-bake precedence as the fresh-card prefill so the env/bake stable path is unchanged when the user does not touch the field. | 2026-07-10 |
| The connect site **stops calling `resolve_remote_exec_wrapper()` directly** (`main.rs:1804`); that resolution moves to the fresh-card **prefill**, and at connect the **threaded field value** reaches `with_remote_exec_wrapper` | Prevents the footgun of threading the field yet leaving the direct env call, which would silently override the field. The value rides the existing connect plumbing — `ConnectRequest` → `SshConfig` → `run_ssh_session` → `with_remote_exec_wrapper`, and into `RecentTarget` (the single capture point at `main.rs:778-800` feeding every `record` site) — exactly mirroring the existing `passphrase` field's path. | 2026-07-10 |
| **Recents persistence is additive** — a `remote_exec_wrapper` string on `RecentConnection`, `#[serde(default)]` "" | The store is already `#[serde(default)]` + tolerant-load (#477); an old file loads the field as "" → `None`. Persisting it is what makes a container recent re-runnable (the requesting need: a recent without its wrapper connects to the bare host). | 2026-07-10 |
| **Client-only, no protocol/daemon change** | The wrapper mechanism (`SshConnection` builder, `exec::wrap_command`) and its rules are already merged (archived `spec-remote-exec-wrapper.md`); this is purely the deferred UI + recents surface over them. | 2026-07-10 |
| Free-text field, no container picker / validation | Proportional (archived spec's "a UI field is a separate later issue if wanted"): the env var is free text today; the field matches. Validation/auto-detect is a later nicety. | 2026-07-10 |

## Prior art

- **Session ↔ connection config surface** — `docs/archive/spec-remote-exec-wrapper.md`
  (the wrapper mechanism + the explicit "a UI field is a separate later issue if
  wanted" deferral this spec picks up) and `docs/prior-art.md` row 33 (rift's own
  connect card / `connection_screen.rs` de-hardcoding precedent).
- **Recents store** — `docs/spec-connection-robustness.md` (#477): the
  `RecentConnection` schema, `same_target` dedupe, tolerant additive load, and
  move-to-front cap this spec extends.
- No external precedent needed: this is a field on rift's own connect card plus
  a field on rift's own recents entry.

## Human prerequisites

- none — the field's values are entered by the user at runtime; no secret,
  provisioning, or account is required to build or test this.

## Open decisions (resolved at the spec-acceptance gate)

| Question | Options | Recommendation |
|---|---|---|
| Does the wrapper participate in the recents **`same_target`** dedupe (`crates/app/src/recents.rs`)? | (A) **Include it** → a container connection `(host,user,port,key,wrapper)` and a plain connection to the same host are **distinct** recents; neither clobbers the other. (B) **Exclude it** → keep dedupe on host/user/port/key only; the newest wrapper wins on move-to-front (a plain reconnect overwrites a container recent's wrapper). | **(A) include it** — a container vs a bare-host connection to the same host are different functional targets; including the wrapper keeps both re-runnable, matching the "a recent must stay functional" intent. Trade-off: at most one extra entry per host that is used both ways (bounded by `MAX_RECENTS`). |

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace
      excluding `rift-app`); `app-check` compiles `rift-app`.
- [ ] Unit (`crates/app`): the connect-card field value is threaded to
      `with_remote_exec_wrapper` at connect (a non-empty field yields
      `Some(field)`, an empty/whitespace field yields `None`); with no card
      interaction the effective wrapper equals `resolve_remote_exec_wrapper()`
      (env-over-bake unchanged).
- [ ] Unit (`crates/app`): field prefill precedence — a selected RECENT prefills
      from that entry's stored wrapper; a fresh card prefills from
      `resolve_remote_exec_wrapper()`.
- [ ] Unit (`crates/app/src/recents.rs`): `RecentConnection` round-trips
      `remote_exec_wrapper`; a serialized entry with the field absent loads it as
      "" (→ `None`); `same_target` behaves per the accepted dedupe decision.
- [ ] Behavioral (dev-channel QA): with the field set to
      `docker exec -i <container>` (+ an absolute `/workspace` project root), the
      daemon deploys into the container and tmux/editor run in the container
      (mirrors the archived spec's QA); the field left empty is an unchanged
      normal host connection.
- [ ] Behavioral (dev-channel QA): a RECENT container connection, re-selected,
      restores its wrapper into the field and reconnects into the container; a
      plain RECENT (no wrapper) reconnects normally.

## Risks and mitigations

- **Prefill vs override confusion** — the user edits the field but a recent
  selection re-prefills it. Mitigation: re-prefill only on an explicit recent
  selection (or fresh card open), never mid-edit; the field value at the moment
  of Connect is authoritative.
- **Recents growth** if the wrapper joins `same_target` (open decision A):
  bounded by `MAX_RECENTS` (8, oldest dropped); at most one extra entry per host
  used both bare and wrapped.
- **A `-t` wrapper** typed by the user corrupts transport: unchanged pre-existing
  behavior — the daemon handshake fails loudly as a connect error (archived
  spec); a placeholder hint documents `-i`.

## Tracking

- Design doc: this spec.
- Milestone + issues: created at the spec-acceptance gate / after merge.

## Decision log

- 2026-07-10: Spec drafted. Picks up the connect-card UI field explicitly
  deferred by `docs/archive/spec-remote-exec-wrapper.md` ("a UI field is a
  separate later issue if wanted"), adding per-host recents persistence of the
  wrapper (the requesting need: a container recent must stay functional). Field
  authoritative at connect, prefilled recent→env→bake, empty→None; additive
  `RecentConnection.remote_exec_wrapper`; client-only, no protocol change. One
  open decision carried to the acceptance gate: whether the wrapper joins the
  recents `same_target` dedupe (recommended: yes).
