//! Recent-connections store for the Connection screen (issue #477,
//! `docs/spec-connection-robustness.md`): "a small local store beside the
//! window-state store" — same per-channel JSON file, same tolerant-load /
//! atomic-write pattern as [`crate::window_state`], deliberately headless
//! (GPUI-free) so it is unit-testable without a live window, mirroring that
//! module's own split.
//!
//! Recents are convenience prefills, not a session manager or a connection
//! history (`docs/spec-connection-robustness.md`'s "Out of scope"): the list
//! is capped at [`MAX_RECENTS`] and a reconnect to an already-known host/user/
//! port/key moves that entry to the front instead of growing the list.

use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::window_state::{self, StoreError};

/// Recents beyond this count are dropped (oldest first) on every
/// [`record`] — a handful of convenience prefills, not a growing history.
pub const MAX_RECENTS: usize = 8;

/// Recent project roots beyond this count are dropped (oldest first) on
/// every [`merge_recent_root`] — the same handful-of-prefills shape as
/// [`MAX_RECENTS`], scoped to one connection's `recent_roots` instead of the
/// whole list (issue #873, `docs/spec-host-scoped-root-recents.md`).
pub const MAX_RECENT_ROOTS: usize = 8;

/// One entry in the RECENT list: everything the Connection screen's connect
/// card needs to prefill and reconnect with one click.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecentConnection {
    pub host: String,
    pub user: String,
    pub port: u16,
    /// SSH private key path, as displayed/edited in the connect card (not a
    /// `PathBuf`: the store is plain JSON text, and the card round-trips it
    /// through a text input either way).
    pub key: String,
    pub session: String,
    /// The connect card's Remote exec wrapper field value at connect time
    /// (issue #790, `docs/spec-remote-exec-wrapper-ui.md`), e.g.
    /// `docker exec -i devenv`; empty for a normal host connection. Additive
    /// over the pre-#790 schema — `#[serde(default)]` on the struct plus this
    /// hand-written `Default` keep a field-absent entry (written before this
    /// change) loading as `""` (the tolerant-load contract, #477).
    pub remote_exec_wrapper: String,
    /// Unix seconds this entry was last connected (or reconnected) with —
    /// the RECENT row's relative-time caption is computed from this.
    pub last_connected_unix_secs: u64,
    /// Recently-picked project roots on this host, most-recent-first, capped
    /// at [`MAX_RECENT_ROOTS`] (issue #873, `docs/spec-host-scoped-root-
    /// recents.md`). Co-located here rather than in a channel-global list
    /// (the pre-#873 `window_state::recent_roots`) because a project root
    /// only exists on the host it was picked on — the same host identity
    /// [`same_target`] already keys this entry on. Additive over the
    /// pre-#873 schema — `#[serde(default)]` on the struct plus this
    /// hand-written `Default` keep a field-absent entry loading as `[]`
    /// (the tolerant-load contract, #477).
    pub recent_roots: Vec<String>,
}

impl Default for RecentConnection {
    fn default() -> Self {
        Self {
            host: String::new(),
            user: String::new(),
            port: 22,
            key: String::new(),
            session: String::new(),
            remote_exec_wrapper: String::new(),
            last_connected_unix_secs: 0,
            recent_roots: Vec::new(),
        }
    }
}

/// The host/user/port/key/wrapper identity a recents entry is keyed on, as a
/// comparable tuple — the shared shape [`same_target`] (two entries) and
/// [`matches_target`] (an entry vs. a live [`RecentTarget`]) both compare, so
/// the five fields are named in exactly one place.
fn identity(entry: &RecentConnection) -> (&str, &str, u16, &str, &str) {
    (
        entry.host.as_str(),
        entry.user.as_str(),
        entry.port,
        entry.key.as_str(),
        entry.remote_exec_wrapper.as_str(),
    )
}

/// Whether two entries are the same connection target for the recents list's
/// dedupe/move-to-front purposes. The session name is deliberately excluded:
/// reconnecting to the same host/user/port/key under a different session name
/// still updates the one existing recent (to the newest session), rather than
/// growing the list per session tried against the same host. The wrapper
/// (issue #790) joins the key: a container recent (host + wrapper) and a
/// bare-host recent to the same host are distinct functional targets, so both
/// stay re-runnable rather than one clobbering the other's wrapper.
fn same_target(a: &RecentConnection, b: &RecentConnection) -> bool {
    identity(a) == identity(b)
}

/// Whether `entry` is the connection `target` identifies — the same five
/// fields [`same_target`] compares, against a live [`RecentTarget`] instead
/// of a second stored entry (issue #873, `docs/spec-host-scoped-root-
/// recents.md`).
fn matches_target(entry: &RecentConnection, target: &RecentTarget) -> bool {
    identity(entry)
        == (
            target.host.as_str(),
            target.user.as_str(),
            target.port,
            target.key.as_str(),
            target.remote_exec_wrapper.as_str(),
        )
}

/// The host/user/port/key/wrapper identity for a recents entry (issue #707,
/// wrapper added by #790). Made lib-visible (moved here from `main.rs`) by
/// issue #873 (`docs/spec-host-scoped-root-recents.md`) so `workspace.rs`'s
/// `WorkspaceView` — a library type, not the binary — can key its own
/// in-cockpit root picker's seed/record on the same identity `main.rs`'s
/// `Shell` uses for its pre-cockpit one. `Shell::connect` captures one per
/// connect attempt (before `ConnectRequest`'s fields move into `SshConfig`)
/// and threads it through the post-connect pickers and into
/// `workspace::WorkspaceView::new`.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentTarget {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub key: String,
    /// The connect-time Remote exec wrapper field value (issue #790), empty
    /// for a normal host connection — persisted onto the recorded
    /// [`RecentConnection`] so a container recent stays re-runnable.
    pub remote_exec_wrapper: String,
}

impl RecentTarget {
    /// Record a connection attempt under this identity: builds a fresh
    /// [`RecentConnection`] (only the session name is newly known) and calls
    /// [`record`], which carries over whatever `recent_roots` the matching
    /// entry already had rather than wiping them on a session-only reconnect.
    pub fn record(&self, path: &Path, session: &str) {
        let now = now_unix_secs();
        let entry = RecentConnection {
            host: self.host.clone(),
            user: self.user.clone(),
            port: self.port,
            key: self.key.clone(),
            session: session.to_string(),
            remote_exec_wrapper: self.remote_exec_wrapper.clone(),
            last_connected_unix_secs: now,
            recent_roots: Vec::new(),
        };
        if let Err(e) = record(path, entry, now) {
            warn!(%e, "failed to record recent connection");
        }
    }
}

/// The recent project roots recorded for `target` (issue #873,
/// `docs/spec-host-scoped-root-recents.md`), most-recent-first — empty if no
/// recent connection matches `target` yet. Keys on [`matches_target`], the
/// same identity [`RecentTarget::record`] stores the connection under, so a
/// root picked on a different host is never offered as a seed here.
pub fn target_recent_roots(path: &Path, target: &RecentTarget) -> Vec<String> {
    load(path)
        .into_iter()
        .find(|entry| matches_target(entry, target))
        .map(|entry| entry.recent_roots)
        .unwrap_or_default()
}

/// Merge a freshly-picked root into `target`'s matching entry: move-to-front
/// if already present, insert as newest otherwise, capped at
/// [`MAX_RECENT_ROOTS`] — the same move-to-front/cap shape as [`record`]'s
/// own list, scoped to one entry's `recent_roots` rather than the whole file.
/// If no entry matches `target` yet — a root picked before any connection was
/// recorded, which should not happen since [`RecentTarget::record`] runs on
/// every connect before a root is ever picked — a fresh entry is inserted for
/// it rather than silently dropping the root.
pub fn merge_recent_root(path: &Path, target: &RecentTarget, root: &str) -> Result<(), StoreError> {
    let mut recents = load(path);
    match recents
        .iter_mut()
        .find(|entry| matches_target(entry, target))
    {
        Some(entry) => {
            entry.recent_roots.retain(|existing| existing != root);
            entry.recent_roots.insert(0, root.to_string());
            entry.recent_roots.truncate(MAX_RECENT_ROOTS);
        }
        None => {
            let entry = RecentConnection {
                host: target.host.clone(),
                user: target.user.clone(),
                port: target.port,
                key: target.key.clone(),
                remote_exec_wrapper: target.remote_exec_wrapper.clone(),
                recent_roots: vec![root.to_string()],
                ..RecentConnection::default()
            };
            recents.insert(0, entry);
            recents.truncate(MAX_RECENTS);
        }
    }
    save(path, &recents)
}

/// Load the recents list at `path`, tolerating everything exactly like
/// [`window_state::load`]: a missing file, a permission error, truncated
/// bytes, or invalid JSON all degrade to an empty list rather than
/// propagating an error or panicking.
pub fn load(path: &Path) -> Vec<RecentConnection> {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

/// Sibling temp path for an atomic write, mirroring `window_state`'s helper
/// of the same name.
fn tmp_path_for(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Persist `recents` to `path` atomically: serialize, write to a sibling temp
/// file, `fsync`, then rename over the target — the same crash-safe sequence
/// as [`window_state::save`].
pub fn save(path: &Path, recents: &[RecentConnection]) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_vec_pretty(recents)?;
    let tmp_path = tmp_path_for(path);
    let mut file = File::create(&tmp_path)?;
    file.write_all(&json)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Record a connection attempt: move an existing matching target
/// ([`same_target`]) to the front (refreshing its session and timestamp), or
/// insert `entry` as the newest one — then cap at [`MAX_RECENTS`], dropping
/// the oldest. A read-modify-write over the store at `path`, mirroring
/// `window_state::save_theme`'s shape; returns the updated list so the caller
/// can render it immediately without a second [`load`].
///
/// Carries over the matching existing entry's `recent_roots` onto `entry`
/// first (issue #873, `docs/spec-host-scoped-root-recents.md`): this runs on
/// every connect, before any root is ever picked on that connection, and
/// `entry` itself carries no roots — without the carry-over, every reconnect
/// to an already-known host would silently wipe the roots picked on it
/// earlier.
pub fn record(
    path: &Path,
    mut entry: RecentConnection,
    now_unix_secs: u64,
) -> Result<Vec<RecentConnection>, StoreError> {
    entry.last_connected_unix_secs = now_unix_secs;
    let mut recents = load(path);
    if let Some(existing) = recents
        .iter()
        .find(|existing| same_target(existing, &entry))
    {
        entry.recent_roots = existing.recent_roots.clone();
    }
    recents.retain(|existing| !same_target(existing, &entry));
    recents.insert(0, entry);
    recents.truncate(MAX_RECENTS);
    save(path, &recents)?;
    Ok(recents)
}

/// The per-channel tag this instance's recents file is keyed by — its own
/// tiny copy of the `windowed`-feature check, matching the existing per-site
/// convention (`window_state::channel_tag`, `main.rs::log_channel`) rather
/// than exposing `window_state`'s private helper.
fn channel_tag(windowed: bool) -> &'static str {
    if windowed {
        "rift-stable"
    } else {
        "rift-dev"
    }
}

/// The recents filename for `windowed`'s channel.
fn file_name(windowed: bool) -> String {
    format!("{}-recents.json", channel_tag(windowed))
}

/// The full path to this instance's recents file: beside the window-state
/// file (same [`window_state::state_dir`]), keyed by the live `windowed`
/// feature.
pub fn recents_path() -> Result<PathBuf, StoreError> {
    Ok(window_state::state_dir()?.join(file_name(cfg!(feature = "windowed"))))
}

/// Seconds since the Unix epoch, floor-clamped to 0 on a pre-1970 clock — the
/// timestamp live callers stamp a fresh [`record`] with.
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A short relative-time label for a RECENT row's trailing caption ("just
/// now", "5m ago", "3h ago", ...), computed from an explicit `now` so it is
/// deterministic and testable without a live clock.
pub fn relative_time(now_unix_secs: u64, then_unix_secs: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;

    let elapsed = now_unix_secs.saturating_sub(then_unix_secs);
    if elapsed < MINUTE {
        "just now".to_string()
    } else if elapsed < HOUR {
        format!("{}m ago", elapsed / MINUTE)
    } else if elapsed < DAY {
        format!("{}h ago", elapsed / HOUR)
    } else if elapsed < WEEK {
        format!("{}d ago", elapsed / DAY)
    } else {
        format!("{}w ago", elapsed / WEEK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique scratch directory under the OS temp dir, mirroring
    /// `window_state`'s test helper of the same shape.
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut dir = std::env::temp_dir();
            dir.push(format!("rift-app-recents-{}-{tag}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn sample(host: &str) -> RecentConnection {
        RecentConnection {
            host: host.to_string(),
            user: "developer".to_string(),
            port: 22,
            key: "/home/developer/.ssh/id_ed25519".to_string(),
            session: "rift".to_string(),
            remote_exec_wrapper: String::new(),
            last_connected_unix_secs: 0,
            recent_roots: Vec::new(),
        }
    }

    fn sample_target(host: &str) -> RecentTarget {
        RecentTarget {
            host: host.to_string(),
            user: "developer".to_string(),
            port: 22,
            key: "/home/developer/.ssh/id_ed25519".to_string(),
            remote_exec_wrapper: String::new(),
        }
    }

    // --- load ----------------------------------------------------------------

    #[test]
    fn test_load_missing_file_returns_empty() {
        let scratch = Scratch::new("missing");
        let path = scratch.path("does-not-exist.json");

        assert_eq!(load(&path), Vec::new());
    }

    #[test]
    fn test_load_corrupt_json_returns_empty_without_panic() {
        let scratch = Scratch::new("corrupt");
        let path = scratch.path("recents.json");
        fs::write(&path, b"{ not valid json").expect("write garbage");

        assert_eq!(load(&path), Vec::new());
    }

    #[test]
    fn test_load_truncated_json_returns_empty_without_panic() {
        let scratch = Scratch::new("truncated");
        let path = scratch.path("recents.json");
        let full = serde_json::to_string(&vec![sample("100.64.0.1")]).expect("serialize");
        fs::write(&path, &full[..full.len() / 2]).expect("write truncated");

        assert_eq!(load(&path), Vec::new());
    }

    // --- save / round-trip -----------------------------------------------------

    #[test]
    fn test_save_then_load_round_trips() {
        let scratch = Scratch::new("roundtrip");
        let path = scratch.path("recents.json");
        let recents = vec![sample("100.64.0.1"), sample("100.64.0.2")];

        save(&path, &recents).expect("save");

        assert_eq!(load(&path), recents);
    }

    #[test]
    fn test_save_then_load_round_trips_remote_exec_wrapper() {
        let scratch = Scratch::new("roundtrip_wrapper");
        let path = scratch.path("recents.json");
        let mut entry = sample("100.64.0.1");
        entry.remote_exec_wrapper = "docker exec -i devenv".to_string();
        let recents = vec![entry];

        save(&path, &recents).expect("save");

        let loaded = load(&path);
        assert_eq!(loaded, recents);
        assert_eq!(loaded[0].remote_exec_wrapper, "docker exec -i devenv");
    }

    #[test]
    fn test_load_field_absent_remote_exec_wrapper_defaults_to_empty() {
        let scratch = Scratch::new("wrapper_absent");
        let path = scratch.path("recents.json");
        // Hand-written JSON without `remote_exec_wrapper`, simulating an
        // entry written before this field existed (#790's tolerant-load
        // contract, #477).
        let json = r#"[{
            "host": "100.64.0.1",
            "user": "developer",
            "port": 22,
            "key": "/home/developer/.ssh/id_ed25519",
            "session": "rift",
            "last_connected_unix_secs": 1000
        }]"#;
        fs::write(&path, json).expect("write field-absent json");

        let recents = load(&path);

        assert_eq!(recents.len(), 1);
        assert_eq!(recents[0].remote_exec_wrapper, "");
    }

    /// Issue #873 (`docs/spec-host-scoped-root-recents.md`): an entry written
    /// before `recent_roots` existed still loads, defaulting to `[]` rather
    /// than failing the parse — the same tolerant-load contract #790's
    /// `remote_exec_wrapper` field above already relies on.
    #[test]
    fn test_load_field_absent_recent_roots_defaults_to_empty() {
        let scratch = Scratch::new("roots_absent");
        let path = scratch.path("recents.json");
        let json = r#"[{
            "host": "100.64.0.1",
            "user": "developer",
            "port": 22,
            "key": "/home/developer/.ssh/id_ed25519",
            "session": "rift",
            "remote_exec_wrapper": "",
            "last_connected_unix_secs": 1000
        }]"#;
        fs::write(&path, json).expect("write field-absent json");

        let recents = load(&path);

        assert_eq!(recents.len(), 1);
        assert_eq!(recents[0].recent_roots, Vec::<String>::new());
    }

    #[test]
    fn test_save_creates_parent_directories() {
        let scratch = Scratch::new("mkdirp");
        let path = scratch.path("nested").join("dir").join("recents.json");

        save(&path, &[sample("100.64.0.1")]).expect("save creates parents");

        assert!(path.exists());
    }

    #[test]
    fn test_save_cleans_up_temp_file_after_rename() {
        let scratch = Scratch::new("tmpcleanup");
        let path = scratch.path("recents.json");

        save(&path, &[sample("100.64.0.1")]).expect("save");

        assert!(!tmp_path_for(&path).exists());
    }

    // --- record ------------------------------------------------------------

    #[test]
    fn test_record_on_missing_file_starts_from_empty() {
        let scratch = Scratch::new("record_missing");
        let path = scratch.path("recents.json");

        let recents = record(&path, sample("100.64.0.1"), 1_000).expect("record");

        assert_eq!(recents.len(), 1);
        assert_eq!(recents[0].host, "100.64.0.1");
        assert_eq!(recents[0].last_connected_unix_secs, 1_000);
    }

    #[test]
    fn test_record_inserts_newest_entry_first() {
        let scratch = Scratch::new("record_order");
        let path = scratch.path("recents.json");

        record(&path, sample("100.64.0.1"), 1_000).expect("first record");
        let recents = record(&path, sample("100.64.0.2"), 2_000).expect("second record");

        assert_eq!(recents.len(), 2);
        assert_eq!(recents[0].host, "100.64.0.2");
        assert_eq!(recents[1].host, "100.64.0.1");
    }

    #[test]
    fn test_record_same_target_moves_to_front_instead_of_duplicating() {
        let scratch = Scratch::new("record_dedupe");
        let path = scratch.path("recents.json");

        record(&path, sample("100.64.0.1"), 1_000).expect("first record");
        record(&path, sample("100.64.0.2"), 2_000).expect("second record");
        let mut reconnect = sample("100.64.0.1");
        reconnect.session = "other-session".to_string();
        let recents = record(&path, reconnect, 3_000).expect("reconnect");

        assert_eq!(recents.len(), 2, "no duplicate entry for the same target");
        assert_eq!(recents[0].host, "100.64.0.1");
        assert_eq!(recents[0].session, "other-session", "session refreshed");
        assert_eq!(recents[0].last_connected_unix_secs, 3_000);
        assert_eq!(recents[1].host, "100.64.0.2");
    }

    /// The spec's main foot-gun (issue #873, `docs/spec-host-scoped-root-
    /// recents.md`): `record` runs on every connect, before any root is ever
    /// picked, so a session-only reconnect to an already-known host must not
    /// wipe the roots recorded on it earlier.
    #[test]
    fn test_record_preserves_existing_recent_roots_on_session_only_reconnect() {
        let scratch = Scratch::new("record_preserves_roots");
        let path = scratch.path("recents.json");
        let target = sample_target("100.64.0.1");
        record(&path, sample("100.64.0.1"), 1_000).expect("first record");
        merge_recent_root(&path, &target, "/home/dev/proj").expect("merge root");

        let mut reconnect = sample("100.64.0.1");
        reconnect.session = "other-session".to_string();
        let recents = record(&path, reconnect, 2_000).expect("session-only reconnect");

        assert_eq!(recents.len(), 1);
        assert_eq!(
            recents[0].recent_roots,
            vec!["/home/dev/proj".to_string()],
            "reconnecting must not wipe the roots already recorded for this host"
        );
        assert_eq!(recents[0].session, "other-session");
    }

    #[test]
    fn test_record_different_user_or_port_is_a_distinct_target() {
        let scratch = Scratch::new("record_distinct");
        let path = scratch.path("recents.json");

        record(&path, sample("100.64.0.1"), 1_000).expect("first record");
        let mut other_port = sample("100.64.0.1");
        other_port.port = 2222;
        let recents = record(&path, other_port, 2_000).expect("distinct port");

        assert_eq!(recents.len(), 2, "differing port is a distinct target");
    }

    #[test]
    fn test_same_target_same_host_different_wrapper_is_not_same_target() {
        let mut container = sample("100.64.0.1");
        container.remote_exec_wrapper = "docker exec -i devenv".to_string();
        let bare_host = sample("100.64.0.1");

        assert!(
            !same_target(&container, &bare_host),
            "a container recent and a bare-host recent to the same host are distinct targets"
        );
    }

    #[test]
    fn test_same_target_identical_including_wrapper_is_same_target() {
        let mut a = sample("100.64.0.1");
        a.remote_exec_wrapper = "docker exec -i devenv".to_string();
        let mut b = sample("100.64.0.1");
        b.remote_exec_wrapper = "docker exec -i devenv".to_string();

        assert!(same_target(&a, &b));
    }

    #[test]
    fn test_record_same_host_different_wrapper_stays_distinct() {
        let scratch = Scratch::new("record_wrapper_distinct");
        let path = scratch.path("recents.json");

        record(&path, sample("100.64.0.1"), 1_000).expect("bare host record");
        let mut container = sample("100.64.0.1");
        container.remote_exec_wrapper = "docker exec -i devenv".to_string();
        let recents = record(&path, container, 2_000).expect("container record");

        assert_eq!(
            recents.len(),
            2,
            "same host with a different wrapper is a distinct, re-runnable target"
        );
    }

    #[test]
    fn test_record_caps_at_max_recents_dropping_the_oldest() {
        let scratch = Scratch::new("record_cap");
        let path = scratch.path("recents.json");

        for i in 0..(MAX_RECENTS + 3) {
            record(&path, sample(&format!("host-{i}")), i as u64).expect("record");
        }

        let recents = load(&path);
        assert_eq!(recents.len(), MAX_RECENTS);
        assert_eq!(
            recents[0].host,
            format!("host-{}", MAX_RECENTS + 2),
            "newest first"
        );
        assert_eq!(
            recents[MAX_RECENTS - 1].host,
            format!("host-{}", 3),
            "oldest entries beyond the cap are dropped"
        );
    }

    // --- host-scoped recent roots (#873) ------------------------------------

    #[test]
    fn test_target_recent_roots_no_matching_entry_returns_empty() {
        let scratch = Scratch::new("roots_lookup_missing");
        let path = scratch.path("recents.json");
        record(&path, sample("100.64.0.1"), 1_000).expect("record");

        let roots = target_recent_roots(&path, &sample_target("100.64.0.2"));

        assert_eq!(
            roots,
            Vec::<String>::new(),
            "a different host has no recorded roots"
        );
    }

    #[test]
    fn test_target_recent_roots_keys_on_same_target_not_a_different_host() {
        let scratch = Scratch::new("roots_lookup_scoped");
        let path = scratch.path("recents.json");
        record(&path, sample("100.64.0.1"), 1_000).expect("record host A");
        record(&path, sample("100.64.0.2"), 2_000).expect("record host B");
        merge_recent_root(&path, &sample_target("100.64.0.1"), "/a/proj").expect("merge for A");

        let roots_a = target_recent_roots(&path, &sample_target("100.64.0.1"));
        let roots_b = target_recent_roots(&path, &sample_target("100.64.0.2"));

        assert_eq!(roots_a, vec!["/a/proj".to_string()]);
        assert_eq!(
            roots_b,
            Vec::<String>::new(),
            "a root picked on host A must never seed host B"
        );
    }

    #[test]
    fn test_merge_recent_root_move_to_fronts_existing_entry() {
        let scratch = Scratch::new("merge_root_order");
        let path = scratch.path("recents.json");
        let target = sample_target("100.64.0.1");
        record(&path, sample("100.64.0.1"), 1_000).expect("record");
        merge_recent_root(&path, &target, "/a/one").expect("merge one");
        merge_recent_root(&path, &target, "/a/two").expect("merge two");

        merge_recent_root(&path, &target, "/a/one").expect("re-merge one");

        let roots = target_recent_roots(&path, &target);
        assert_eq!(
            roots,
            vec!["/a/one".to_string(), "/a/two".to_string()],
            "re-picking an existing root moves it to front without duplicating"
        );
    }

    #[test]
    fn test_merge_recent_root_caps_at_max_recent_roots_dropping_the_oldest() {
        let scratch = Scratch::new("merge_root_cap");
        let path = scratch.path("recents.json");
        let target = sample_target("100.64.0.1");
        record(&path, sample("100.64.0.1"), 1_000).expect("record");

        for i in 0..(MAX_RECENT_ROOTS + 3) {
            merge_recent_root(&path, &target, &format!("/root-{i}")).expect("merge");
        }

        let roots = target_recent_roots(&path, &target);
        assert_eq!(roots.len(), MAX_RECENT_ROOTS);
        assert_eq!(
            roots[0],
            format!("/root-{}", MAX_RECENT_ROOTS + 2),
            "newest first"
        );
        assert_eq!(
            roots[MAX_RECENT_ROOTS - 1],
            "/root-3",
            "oldest roots beyond the cap are dropped"
        );
    }

    #[test]
    fn test_merge_recent_root_with_no_matching_entry_creates_one() {
        let scratch = Scratch::new("merge_root_no_entry");
        let path = scratch.path("recents.json");
        let target = sample_target("100.64.0.1");

        merge_recent_root(&path, &target, "/a/proj").expect("merge without a prior connect");

        let roots = target_recent_roots(&path, &target);
        assert_eq!(roots, vec!["/a/proj".to_string()]);
    }

    // --- channel keying --------------------------------------------------------

    #[test]
    fn test_stable_and_dev_channels_resolve_different_file_names() {
        let stable = file_name(true);
        let dev = file_name(false);

        assert_ne!(stable, dev);
        assert_eq!(stable, "rift-stable-recents.json");
        assert_eq!(dev, "rift-dev-recents.json");
    }

    // --- relative_time -----------------------------------------------------

    #[test]
    fn test_relative_time_just_now_under_a_minute() {
        assert_eq!(relative_time(1_000, 1_000), "just now");
        assert_eq!(relative_time(1_059, 1_000), "just now");
    }

    #[test]
    fn test_relative_time_minutes() {
        assert_eq!(relative_time(1_000 + 60 * 5, 1_000), "5m ago");
    }

    #[test]
    fn test_relative_time_hours() {
        assert_eq!(relative_time(1_000 + 3600 * 3, 1_000), "3h ago");
    }

    #[test]
    fn test_relative_time_days() {
        assert_eq!(relative_time(1_000 + 86_400 * 2, 1_000), "2d ago");
    }

    #[test]
    fn test_relative_time_weeks() {
        assert_eq!(relative_time(1_000 + 86_400 * 7 * 2, 1_000), "2w ago");
    }

    #[test]
    fn test_relative_time_clock_earlier_than_entry_saturates_to_just_now() {
        assert_eq!(relative_time(1_000, 5_000), "just now");
    }
}
