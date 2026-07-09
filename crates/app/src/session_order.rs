//! Client-side session-order store (#686, `docs/spec-session-management.md`):
//! "a local per-channel order store, the recents/window-state pattern" — the
//! same per-channel JSON file, tolerant-load / atomic-write pattern as
//! [`crate::recents`] (itself modeled on [`crate::window_state`]),
//! deliberately headless (GPUI-free) so it is unit-testable without a live
//! window — `rift-terminal` never depends on this module (crate boundary).
//!
//! Reorder is a total user-set order (spec Prior decisions: "not a pin/
//! favorite subset flag"), keyed by session NAME — durable across a daemon/
//! tmux-server restart, which re-mints session ids
//! (`rift_terminal::SessionListItem::id`). [`sort_sessions`] applies the
//! stored order as a render-time sort over the server's session list; the
//! store itself is never written back into `SessionView.sessions`, which
//! stays the daemon's replace-semantics model (spec: "never mutating the
//! server list").

use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use rift_terminal::SessionListItem;

use crate::window_state::{self, StoreError};

/// Load the stored session-name order at `path`, tolerating everything
/// exactly like [`window_state::load`]/[`crate::recents::load`]: a missing
/// file, a permission error, truncated bytes, or invalid JSON all degrade to
/// an empty order (spec: "never a crash, never a refusal to start") rather
/// than propagating an error or panicking.
pub fn load(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

/// Sibling temp path for an atomic write, mirroring `window_state`'s/
/// `recents`'s helper of the same name.
fn tmp_path_for(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Persist `order` to `path` atomically: serialize, write to a sibling temp
/// file, `fsync`, then rename over the target — the same crash-safe sequence
/// as [`window_state::save`]/[`crate::recents::save`].
pub fn save(path: &Path, order: &[String]) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_vec_pretty(order)?;
    let tmp_path = tmp_path_for(path);
    let mut file = File::create(&tmp_path)?;
    file.write_all(&json)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Apply the stored `order` as a render-time sort over the server's live
/// `sessions` list (spec: "applied as a sort over the server `sessions` list
/// at render time, never written into `SessionView.sessions`"): every name in
/// `order` that still has a matching session is placed first, in the stored
/// sequence; every session NOT in `order` (never dragged, or new since the
/// order was last saved) is appended after, sorted by name — the tmux default
/// order — so an unknown session always sorts last rather than jumping
/// unpredictably among the known ones.
pub fn sort_sessions(sessions: Vec<SessionListItem>, order: &[String]) -> Vec<SessionListItem> {
    let mut remaining = sessions;
    let mut sorted = Vec::with_capacity(remaining.len());
    for name in order {
        if let Some(pos) = remaining.iter().position(|s| &s.name == name) {
            sorted.push(remaining.remove(pos));
        }
    }
    remaining.sort_by(|a, b| a.name.cmp(&b.name));
    sorted.extend(remaining);
    sorted
}

/// Replace the stored order with `new_sequence` — a drag-to-reorder commit's
/// full resequencing of every visible session name (spec Prior decisions:
/// "drag-to-order, a total user-set order", not a partial patch). The
/// previous `order` is deliberately discarded wholesale rather than merged:
/// the render layer (`session_view.rs`'s drag handler) builds `new_sequence`
/// from the FULL current session list, so it already carries every session
/// `order` could have named. Kept as an explicit parameter — rather than
/// dropping it — so the call site reads as the same read-modify-write shape
/// as every other store mutation here (mirrors `recents::record`).
pub fn apply_reorder(_order: &[String], new_sequence: Vec<String>) -> Vec<String> {
    new_sequence
}

/// Rename the stored order's key for a session (`old` -> `new`) in place, so
/// a client-initiated in-UI rename preserves that session's reordered slot
/// (spec Prior decisions: "a client-initiated rename updates the order-store
/// key ... in the SAME action, preserving that session's slot; only an
/// external CLI rename re-slots it"). A no-op (order returned unchanged) when
/// `old` is not a stored key — nothing to preserve a slot for.
pub fn apply_rename(order: &[String], old: &str, new: &str) -> Vec<String> {
    order
        .iter()
        .map(|name| {
            if name == old {
                new.to_string()
            } else {
                name.clone()
            }
        })
        .collect()
}

/// The per-channel tag this instance's session-order file is keyed by — its
/// own tiny copy of the `windowed`-feature check, matching
/// `recents::channel_tag`/`window_state::channel_tag`.
fn channel_tag(windowed: bool) -> &'static str {
    if windowed {
        "rift-stable"
    } else {
        "rift-dev"
    }
}

/// The session-order filename for `windowed`'s channel.
fn file_name(windowed: bool) -> String {
    format!("{}-session-order.json", channel_tag(windowed))
}

/// The full path to this instance's session-order file: beside the
/// window-state/recents files (same [`window_state::state_dir`]), keyed by
/// the live `windowed` feature.
pub fn session_order_path() -> Result<PathBuf, StoreError> {
    Ok(window_state::state_dir()?.join(file_name(cfg!(feature = "windowed"))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique scratch directory under the OS temp dir, mirroring
    /// `recents`'/`window_state`'s test helper of the same shape.
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut dir = std::env::temp_dir();
            dir.push(format!(
                "rift-app-session-order-{}-{tag}-{n}",
                std::process::id()
            ));
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

    fn session(name: &str, id: u32) -> SessionListItem {
        SessionListItem {
            id,
            name: name.to_string(),
            windows: 1,
            attached: false,
            root: None,
        }
    }

    // --- load ----------------------------------------------------------------

    #[test]
    fn test_load_missing_file_returns_empty() {
        let scratch = Scratch::new("missing");
        let path = scratch.path("does-not-exist.json");

        assert_eq!(load(&path), Vec::<String>::new());
    }

    #[test]
    fn test_load_corrupt_json_returns_empty_without_panic() {
        let scratch = Scratch::new("corrupt");
        let path = scratch.path("session-order.json");
        fs::write(&path, b"{ not valid json").expect("write garbage");

        assert_eq!(load(&path), Vec::<String>::new());
    }

    #[test]
    fn test_load_truncated_json_returns_empty_without_panic() {
        let scratch = Scratch::new("truncated");
        let path = scratch.path("session-order.json");
        let full = serde_json::to_string(&vec!["rift".to_string(), "agent".to_string()])
            .expect("serialize");
        fs::write(&path, &full[..full.len() / 2]).expect("write truncated");

        assert_eq!(load(&path), Vec::<String>::new());
    }

    // --- save / round-trip -----------------------------------------------------

    #[test]
    fn test_save_then_load_round_trips() {
        let scratch = Scratch::new("roundtrip");
        let path = scratch.path("session-order.json");
        let order = vec!["tests".to_string(), "rift".to_string(), "agent".to_string()];

        save(&path, &order).expect("save");

        assert_eq!(load(&path), order);
    }

    #[test]
    fn test_save_creates_parent_directories() {
        let scratch = Scratch::new("mkdirp");
        let path = scratch
            .path("nested")
            .join("dir")
            .join("session-order.json");

        save(&path, &["rift".to_string()]).expect("save creates parents");

        assert!(path.exists());
    }

    #[test]
    fn test_save_cleans_up_temp_file_after_rename() {
        let scratch = Scratch::new("tmpcleanup");
        let path = scratch.path("session-order.json");

        save(&path, &["rift".to_string()]).expect("save");

        assert!(!tmp_path_for(&path).exists());
    }

    // --- sort_sessions -----------------------------------------------------

    #[test]
    fn test_sort_sessions_places_stored_names_first_in_stored_order() {
        let sessions = vec![session("agent", 1), session("rift", 0), session("tests", 2)];
        let order = vec!["tests".to_string(), "rift".to_string()];

        let sorted = sort_sessions(sessions, &order);

        assert_eq!(
            sorted.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["tests", "rift", "agent"],
            "stored names lead in stored order, the unknown one falls after"
        );
    }

    #[test]
    fn test_sort_sessions_empty_order_falls_back_to_name_order() {
        let sessions = vec![session("tests", 2), session("agent", 1), session("rift", 0)];

        let sorted = sort_sessions(sessions, &[]);

        assert_eq!(
            sorted.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["agent", "rift", "tests"],
            "no stored order at all falls back to the tmux default (name) order"
        );
    }

    #[test]
    fn test_sort_sessions_stale_stored_name_is_dropped_not_inserted() {
        let sessions = vec![session("rift", 0)];
        let order = vec!["gone".to_string(), "rift".to_string()];

        let sorted = sort_sessions(sessions, &order);

        assert_eq!(
            sorted.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["rift"],
            "a stored name with no matching live session is simply skipped"
        );
    }

    #[test]
    fn test_sort_sessions_multiple_unknowns_sort_by_name_after_known() {
        let sessions = vec![session("zeta", 3), session("rift", 0), session("alpha", 4)];
        let order = vec!["rift".to_string()];

        let sorted = sort_sessions(sessions, &order);

        assert_eq!(
            sorted.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["rift", "alpha", "zeta"],
            "unknowns sort by name among themselves after the known ones"
        );
    }

    // --- apply_reorder -------------------------------------------------------

    #[test]
    fn test_apply_reorder_replaces_the_stored_order_wholesale() {
        let old_order = vec!["rift".to_string(), "agent".to_string()];
        let new_sequence = vec!["agent".to_string(), "tests".to_string(), "rift".to_string()];

        let updated = apply_reorder(&old_order, new_sequence.clone());

        assert_eq!(updated, new_sequence);
    }

    #[test]
    fn test_apply_reorder_on_empty_previous_order_just_adopts_the_sequence() {
        let new_sequence = vec!["rift".to_string(), "agent".to_string()];

        let updated = apply_reorder(&[], new_sequence.clone());

        assert_eq!(updated, new_sequence);
    }

    // --- apply_rename --------------------------------------------------------

    #[test]
    fn test_apply_rename_preserves_the_slot() {
        let order = vec!["tests".to_string(), "rift".to_string(), "agent".to_string()];

        let updated = apply_rename(&order, "rift", "my agent");

        assert_eq!(
            updated,
            vec![
                "tests".to_string(),
                "my agent".to_string(),
                "agent".to_string(),
            ],
            "the renamed session keeps its middle slot"
        );
    }

    #[test]
    fn test_apply_rename_unknown_old_name_leaves_order_unchanged() {
        let order = vec!["tests".to_string(), "rift".to_string()];

        let updated = apply_rename(&order, "gone", "whatever");

        assert_eq!(
            updated, order,
            "nothing to preserve a slot for when the old name was never stored"
        );
    }

    // --- channel keying --------------------------------------------------------

    #[test]
    fn test_stable_and_dev_channels_resolve_different_file_names() {
        let stable = file_name(true);
        let dev = file_name(false);

        assert_ne!(stable, dev);
        assert_eq!(stable, "rift-stable-session-order.json");
        assert_eq!(dev, "rift-dev-session-order.json");
    }
}
