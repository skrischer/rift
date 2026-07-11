# Spec: Spawn the daemon's tmux in a UTF-8 locale — tab-separated format queries

> Status: READY
> Created: 2026-07-11

The daemon spawns its tmux control-mode server inheriting the target host's
default locale. On a target whose effective locale is non-UTF-8 (C/POSIX), tmux
replaces the TAB (`0x09`, non-printable in the C locale) in format-string output
with `_`, corrupting the daemon's tab-separated format queries (`ROOT_QUERY`,
`SESSION_LIST_QUERY`, `LAYOUT_QUERY`) — the `\t`-split parse then collapses the
fields into a garbage value (e.g. `@root` `/workspace/rift` + `session_path`
`/workspace/rift` resolve to `/workspace/rift_/workspace/rift`), so the daemon's
worktree scan fails ("path not found") and the reactive layer (file tree / git /
diagnostics) stays dead. Fix: the daemon spawns tmux with `LC_ALL=C.UTF-8` so
tmux preserves the tab in its format output — in a C/POSIX locale tmux sanitizes
the non-printable tab to `_`; a UTF-8 ctype stops that — regardless of the
target's default locale.

## Outcome

- [ ] Every tmux process the daemon spawns runs with `LC_ALL=C.UTF-8` set in its
      environment, so a daemon-created tmux server preserves the TAB in
      `display-message`/`list-sessions -F` format output on a target whose default
      locale is non-UTF-8 (C/POSIX).
- [ ] On a non-UTF-8-locale target (the homelab devenv container, `LANG=en_US.UTF-8`
      set but ungenerated → effective `LC_CTYPE=POSIX`), `resolve_session_root`
      resolves the real per-session `@root` (not a `<root>_<session_path>` mangle),
      the worktree scan succeeds, and the file explorer populates.
- [ ] The daily-driver (UTF-8-locale host) is unaffected — it already preserved
      the tab; the change only hardens the non-UTF-8 case.
- [ ] No protocol / daemon-message change; `PROTOCOL_VERSION` unchanged. No tmux
      command-string or format-query change — only the spawn environment.

## Scope

### In scope

- **`crates/daemon` (`terminal.rs`)**: set `.env("LC_ALL", "C.UTF-8")` on every
  `tokio::process::Command::new("tmux")` the daemon spawns — the control-mode
  attach child (the server-creating `tmux -C new-session -A`, `spawn_attach`
  ~`:273-284`) and the standalone `tmux list-sessions` command (~`:310`). This
  mirrors the established in-repo pattern of setting a locale on a spawned
  subprocess (`crates/daemon/src/clone.rs:177` sets `LC_ALL=C` on the git clone
  for stable output).
- A regression test for the env wiring where a spawn helper is unit-testable
  (assert the spawned tmux `Command` carries `LC_ALL=C.UTF-8`); the end-to-end
  tab-preservation is a behavioral check (needs a real tmux in a C-locale env).

### Out of scope

- **A tmux server the operator pre-started in a non-UTF-8 locale** (e.g. a
  `docker exec -it devenv tmux new -A -s main` run before rift attaches): the
  server's locale is fixed at its creation, so a later daemon attach inherits the
  pre-existing C-locale server and still mangles. This fix covers the
  daemon-owns-the-server case (the reported devenv failure and the normal flow);
  a pre-existing foreign C-locale server is a documented limitation — the operator
  ensures a UTF-8 locale for their own tmux, or lets rift create the server.
- The **`clone.rs` `LC_ALL=C`** for git — deliberately C (stable, English,
  machine-parseable git output); unrelated to tmux's UTF-8 need, left as-is.
- Changing the tab separator / the format queries themselves — no robust
  printable separator exists (tmux mangles all non-printables in the C locale,
  and printables appear in paths/names), so forcing UTF-8 is the fix, not a
  separator change.
- The `#[cfg(test)]` tmux test helpers (`TmuxServer` / `IsolatedTmux`,
  `terminal.rs` / `lib.rs`) spawn tmux via `std::process::Command` with no locale
  env; their tab-parse tests would regress only if CI ever ran under a C locale
  (it does not today). Pre-existing and orthogonal to this bug — noted, not fixed
  here.
- Any protocol / daemon-message change; any client (`crates/app`) change.

## Constraints

- **The tmux server locale is fixed at server creation.** `LC_ALL` set on the
  daemon's `tmux -C new-session -A` spawn takes effect only when that spawn
  *creates* the server (no server running yet) — which is the devenv case and the
  normal daemon-owns-tmux model (`docs/architecture.md`: the daemon is the sole
  terminal source). A server the daemon attaches to (already running) keeps its
  own locale (Out of scope).
- **Each tmux invocation sanitizes format output per its OWN locale**, not only
  the server's. A one-off `tmux list-sessions` client run in a C locale mangles
  the tab in its own `-F` output even against a UTF-8 server (tmux `list-sessions`
  cannot create a server, so its role is a short-lived client). So `LC_ALL=C.UTF-8`
  is required on EVERY tmux `Command` the daemon spawns that reads tab-separated
  format output — the control-mode attach (server-wide + its own decode) AND the
  one-off `list-sessions` — for two distinct reasons, not merely for consistency.
- **`LC_ALL` is the strongest locale override** (wins over every `LC_*` and
  `LANG`), so it forces UTF-8 ctype regardless of the target's `LANG`/`LC_*` —
  the robust choice, matching `clone.rs`'s use of `LC_ALL`.
- **`C.UTF-8` is the portable UTF-8 locale** — always present in musl and in
  glibc (2.35+), unlike `en_US.UTF-8` which must be generated. The daemon is a
  static musl binary but spawns the *host's* tmux, so the locale must be one the
  host's libc provides; `C.UTF-8` is the safe universal choice.
- Agent-agnostic, host-signal-only (`docs/constitution.md`) — a locale on a
  spawned tmux is transport hygiene, no agent awareness.

## Prior art

- `docs/tmux-reference.md` (control-mode encoding) — the tmux control-mode
  contract the daemon parses; this fix keeps the tab separator intact rather than
  changing the parse.
- In-repo precedent: `crates/daemon/src/clone.rs:177` sets `LC_ALL` on a spawned
  subprocess for deterministic output — the same mechanism, opposite locale
  (C for git's stable text vs C.UTF-8 for tmux's byte-preserving format output).
- `docs/prior-art.md` — none directly relevant (a locale/encoding robustness fix,
  not a product feature).

## Human prerequisites

- none — a `crates/daemon` code change; no secret, provisioning, or account
  required. (The behavioral verification uses the existing homelab devenv
  container, already reachable.)

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The daemon spawns tmux with `LC_ALL=C.UTF-8` (not a format/separator change) | tmux in a non-UTF-8 locale replaces the non-printable TAB separator with `_`; forcing a UTF-8 ctype makes tmux preserve it. No printable separator is safe (paths/names contain them), so the locale is the fix. | 2026-07-11 |
| `LC_ALL` (strongest override) + `C.UTF-8` (portable UTF-8) | `LC_ALL` beats the target's `LANG`/`LC_*`; `C.UTF-8` is always available (musl + glibc 2.35+), unlike `en_US.UTF-8`. Mirrors `clone.rs`'s `LC_ALL`. | 2026-07-11 |
| Set `LC_ALL=C.UTF-8` on EVERY daemon tmux spawn — the control-mode attach AND the one-off `list-sessions` | Two distinct reasons, not consistency: the attach spawn creates the server (server-wide locale) and is the control client that decodes format replies; the one-off `list-sessions` is a separate short-lived tmux CLIENT whose OWN locale governs how IT sanitizes format output — a C-locale one-off mangles the tab even against a UTF-8 server (`list-sessions` never creates a server). Neither env is redundant. | 2026-07-11 |
| A pre-existing operator-started C-locale tmux server is out of scope (documented limitation) | The server's locale is immutable after creation; the daemon cannot change an attached server's locale. The fix covers the daemon-owns-the-server case (the reported failure); a foreign pre-C server is the operator's to fix (UTF-8 shell) — no rift mechanism forces a foreign server's locale. | 2026-07-11 |

## Tracking

- Milestone: created at the acceptance gate (a bug-fix milestone; **not** a
  roadmap phase — no roadmap-overview row).
- Issues: created from this spec after merge.

Each issue references this spec path in its body.

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace
      excluding `rift-app`); the daemon-musl build stays green.
- [ ] Unit / grep: every `tokio::process::Command::new("tmux")` in
      `crates/daemon` carries `.env("LC_ALL", "C.UTF-8")` (compile/grep-checked);
      a spawn-helper test asserts the env is present.
- [ ] Behavioural (devenv QA): on a fresh devenv container (default POSIX
      locale, no rift daemon), connecting deploys the daemon and — after creating a
      session (root picker / clone-from-URL into `/workspace`) — the daemon log
      shows `resolved session root on attach ... root=Some("/workspace/<repo>")`
      (a clean single path, NOT `/workspace/<repo>_/workspace/<repo>`), the
      worktree scan succeeds, and the file explorer + git status populate.
- [ ] Regression check: the daily-driver (UTF-8 host) still resolves session roots
      and lists sessions correctly (unchanged).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The host's libc lacks `C.UTF-8` (very old glibc < 2.35 without it generated) | `C.UTF-8` is standard in musl and modern glibc; on a host missing it, tmux falls back to its prior behavior (no worse than today). If it proves a real gap, a follow-up can probe `locale -a` — out of scope here. |
| A pre-existing foreign tmux server in C locale still mangles | Documented Out-of-scope limitation; the daemon-created server (the reported case) is fixed. The operator ensures a UTF-8 locale for a self-started `main`, or lets rift own the server. |
| Setting `LC_ALL=C.UTF-8` changes tmux's collation/sorting for some format | tmux format output the daemon parses is byte/path data, not locale-collated text; `C.UTF-8` only affects ctype (printability), which is exactly the fix. No behavioral change beyond preserving the tab. |

## Decision log

- 2026-07-11: Root cause found during devenv QA. On the homelab devenv container
  (`LANG=en_US.UTF-8` set but ungenerated → effective `LC_CTYPE=POSIX`), tmux 3.4
  replaced the TAB in `#{@root}\t#{session_path}` (`ROOT_QUERY`) and
  `list-sessions -F` output with `_`; the daemon's `splitn(2, '\t')` then read the
  whole `<root>_<session_path>` as the root, so `resolve_session_root` returned a
  path that does not exist and the worktree scan failed → blank terminal + dead
  explorer. Confirmed by comparison: a container tmux server started with
  `LC_ALL=C.UTF-8` preserves the tab; one started in `LC_CTYPE=C` mangles it. The
  UTF-8 host (daily-driver) preserves the tab, which is why only the devenv
  failed. Independent of Phase 41 (root-less daemon) — a fresh build does not fix
  it. Fix: spawn the daemon's tmux with `LC_ALL=C.UTF-8`.
