//! Persistent window-state store: schema, platform paths, per-channel keying,
//! and atomic save/load (`docs/spec-window-state-persistence.md`).
//!
//! Deliberately headless and GPUI-free — this module never touches a live
//! window or display. The GPUI-side capture/restore wiring (observing
//! move/resize/maximize, seeding the window at startup, the debounced save
//! timer) is issue #225; this module only owns the store: what gets
//! persisted, where, and how the write survives a crash mid-save.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use gpui_component::ThemeMode;
use serde::{Deserialize, Serialize};

/// Schema version of the on-disk file. Bumped only if a future change breaks
/// the additive-fields-with-defaults contract ("Forward-compatible schema" in
/// the spec) — v1 needs no migration logic, only tolerant defaults.
pub const SCHEMA_VERSION: u32 = 1;

/// Default window size: the schema default, the clamp fallback for degenerate
/// restored sizes, and the size `main.rs` currently opens centered on first
/// launch.
pub const DEFAULT_WINDOW_WIDTH: f64 = 1200.0;
pub const DEFAULT_WINDOW_HEIGHT: f64 = 800.0;

/// Floor below which a restored dimension is treated as degenerate rather
/// than merely small, and reset to the default instead of clamped further
/// down.
const MIN_WINDOW_WIDTH: f64 = 200.0;
const MIN_WINDOW_HEIGHT: f64 = 150.0;

/// Font size rift starts at before any restore, mirroring
/// `rift_terminal::session_view`'s private `DEFAULT_FONT_SIZE`. Not read from
/// there directly: the terminal crate has no public font-size surface yet —
/// issue #225 adds the narrow `rift-terminal` seed/read API the spec names
/// and wires this value through it.
const DEFAULT_FONT_SIZE_PX: f32 = 14.0;

/// The diff view's Split|Unified display preference
/// (`docs/spec-source-control-write.md`, issue #547): which renderer the
/// header's segmented toggle selects for the open file's diff. `Unified` is
/// the default — the only renderer that exists until #548 lands the split
/// view. Persisted so a restart keeps the last choice, mirroring
/// `theme_mode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffViewMode {
    #[default]
    Unified,
    Split,
}

/// A plain axis-aligned rectangle in logical pixels. Used both for the
/// persisted window bounds and, at the caller's discretion, for the live
/// display bounds passed into [`clamp_bounds`] — this module has no GPUI
/// dependency, so converting `gpui::Bounds<Pixels>` to and from this shape is
/// the caller's job (issue #225).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Default for Rect {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: DEFAULT_WINDOW_WIDTH,
            height: DEFAULT_WINDOW_HEIGHT,
        }
    }
}

/// The persisted window state: bounds, maximized flag, the whole-client font
/// size (absolute px, not a ratio — spec decision log), and the active theme
/// choice (mode + named theme, `docs/spec-theme-settings.md`).
/// `#[serde(default)]` at the container level means a field missing from the
/// on-disk JSON (an older file, or a hand-edited one) takes its value from
/// [`WindowState::default`] rather than failing to parse; a field present but
/// unrecognized (a future schema version's addition) is silently ignored by
/// `serde_json`. Together these make load forward- and backward-tolerant
/// without migration code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub version: u32,
    pub bounds: Rect,
    pub maximized: bool,
    pub font_size_px: f32,
    /// Name of the last named theme activated via `crate::set_theme` (looked
    /// up in `gpui-component`'s `ThemeRegistry` at restore time). An
    /// arbitrary, unvalidated string as far as this module is concerned — an
    /// unknown-to-the-registry name is `set_theme`'s fallback to tolerate, not
    /// this store's.
    pub theme_name: String,
    /// Last active light/dark mode, which can diverge from `theme_name`'s own
    /// mode when toggled independently via `crate::set_theme_mode`.
    pub theme_mode: ThemeMode,
    /// The diff view's Split|Unified display preference, set independently
    /// of the theme/geometry fields above (`docs/spec-source-control-write.md`,
    /// issue #547).
    pub diff_view_mode: DiffViewMode,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            bounds: Rect::default(),
            maximized: false,
            font_size_px: DEFAULT_FONT_SIZE_PX,
            theme_name: crate::DEFAULT_THEME_NAME.to_string(),
            // Matches `DEFAULT_THEME_NAME`'s own mode (dark Catppuccin Mocha) —
            // the same "missing/corrupt/unknown falls back to dark" default the
            // rest of the store's tolerant load already promises.
            theme_mode: ThemeMode::Dark,
            diff_view_mode: DiffViewMode::default(),
        }
    }
}

/// Persist a theme change into the store at `path`: a read-modify-write that
/// updates only `theme_name`/`theme_mode`, leaving whatever bounds/maximized/
/// font size are already on disk untouched — that half of the schema is
/// #225's capture/restore concern, not this call's. The save-on-change
/// counterpart to `load`'s startup restore.
pub fn save_theme(path: &Path, name: &str, mode: ThemeMode) -> Result<(), StoreError> {
    let mut state = load(path);
    state.theme_name = name.to_string();
    state.theme_mode = mode;
    save(path, &state)
}

/// Persist a mode-only change into the store at `path`: a read-modify-write
/// that updates `theme_mode` alone, leaving `theme_name` — the last *named*
/// theme selection — untouched. The persistence counterpart to
/// `crate::set_theme_mode`'s "flip the mode, keep the named themes" semantics:
/// saving the active slot's name on a mode flip (via `save_theme`) would
/// overwrite the selection with the other mode's theme (issue #443).
pub fn save_theme_mode(path: &Path, mode: ThemeMode) -> Result<(), StoreError> {
    let mut state = load(path);
    state.theme_mode = mode;
    save(path, &state)
}

/// Persist the diff view's Split|Unified preference into the store at
/// `path`: a read-modify-write that updates only `diff_view_mode`, leaving
/// whatever bounds/theme/font is already on disk untouched — mirrors
/// [`save_theme_mode`]'s shape (issue #547).
pub fn save_diff_view_mode(path: &Path, mode: DiffViewMode) -> Result<(), StoreError> {
    let mut state = load(path);
    state.diff_view_mode = mode;
    save(path, &state)
}

/// Persist window geometry into the store at `path`: a read-modify-write that
/// updates only `bounds`/`maximized`/`font_size_px`, leaving whatever theme is
/// already on disk untouched — that half of the schema is `save_theme`'s
/// concern, not this call's. The save-on-change counterpart to `load`'s
/// startup restore, mirroring `save_theme`'s shape.
pub fn save_geometry(
    path: &Path,
    bounds: Rect,
    maximized: bool,
    font_size_px: f32,
) -> Result<(), StoreError> {
    let mut state = load(path);
    state.version = SCHEMA_VERSION;
    state.bounds = bounds;
    state.maximized = maximized;
    state.font_size_px = font_size_px;
    save(path, &state)
}

/// Failure modes for [`save`] and platform path resolution. [`load`] never
/// returns an error — a missing, corrupt, or unreadable file degrades to
/// [`WindowState::default`] instead (spec: "never a crash, never a refusal to
/// start").
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("no platform state directory: LOCALAPPDATA, XDG_STATE_HOME, and HOME are all unset")]
    NoStateDir,
    #[error("failed to serialize window state: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("failed to write window state file: {0}")]
    Io(#[from] std::io::Error),
}

/// Resolve the platform state directory from explicit env inputs (pure, for
/// tests) — the real entry point [`state_dir`] wraps this with the live
/// environment. Order: `LOCALAPPDATA` (Windows), then `XDG_STATE_HOME`
/// (Linux), then `~/.local/state` (Linux fallback). No `dirs` crate (spec
/// constraint) — three env reads cover both target platforms.
fn state_dir_from(
    localappdata: Option<&OsStr>,
    xdg_state_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, StoreError> {
    if let Some(base) = localappdata {
        return Ok(PathBuf::from(base).join("rift"));
    }
    if let Some(base) = xdg_state_home {
        return Ok(PathBuf::from(base).join("rift"));
    }
    if let Some(home) = home {
        return Ok(PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("rift"));
    }
    Err(StoreError::NoStateDir)
}

/// The platform state directory, read from the live environment. See
/// [`state_dir_from`] for the resolution order.
pub fn state_dir() -> Result<PathBuf, StoreError> {
    state_dir_from(
        env::var_os("LOCALAPPDATA").as_deref(),
        env::var_os("XDG_STATE_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

/// The instance-channel tag a state (and log) file is keyed by, pure over the
/// `windowed` feature flag's value — the same flag `main.rs`'s `log_channel`
/// and window title already key off, so the state file lands next to that
/// channel's log with no new knob (spec: "derived from the instance
/// identity"). `true` is the stable build (`just promote`'s `--features
/// windowed`), `false` is dev.
fn channel_tag(windowed: bool) -> &'static str {
    if windowed {
        "rift-stable"
    } else {
        "rift-dev"
    }
}

/// The state filename for `windowed`'s channel. Pure wrapper over
/// [`channel_tag`] so "stable and dev resolve different files" is testable
/// without a second compiled test binary.
fn state_file_name(windowed: bool) -> String {
    format!("{}-window-state.json", channel_tag(windowed))
}

/// The full path to this instance's state file, keyed by the live `windowed`
/// feature and the live environment.
pub fn state_path() -> Result<PathBuf, StoreError> {
    Ok(state_dir()?.join(state_file_name(cfg!(feature = "windowed"))))
}

/// Sibling temp path for an atomic write (`foo.json` -> `foo.json.tmp`), built
/// on the raw `OsString` so non-UTF-8 paths round-trip — mirrors
/// `rift_logging::sink`'s `rotated_path`.
fn tmp_path_for(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Persist `state` to `path` atomically: serialize, write to a sibling temp
/// file, `fsync`, then rename over the target. The rename is the only
/// operation that touches `path` itself, so a crash or kill at any point
/// before it leaves the previous file (or no file) exactly as it was — never
/// a half-written one (spec: "Atomic writes").
pub fn save(path: &Path, state: &WindowState) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_vec_pretty(state)?;
    let tmp_path = tmp_path_for(path);
    let mut file = File::create(&tmp_path)?;
    file.write_all(&json)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Load the state at `path`, tolerating everything: a missing file, a
/// permission error, truncated bytes, or invalid JSON all degrade to
/// [`WindowState::default`] rather than propagating an error or panicking.
/// Fields present and valid load normally; fields missing, or a future
/// schema version's unrecognized additions, fall back per-field to the
/// default (`#[serde(default)]` on [`WindowState`]).
pub fn load(path: &Path) -> WindowState {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

/// Whether point `(x, y)` falls inside `rect` (half-open: the right/bottom
/// edge is exclusive, the usual display-geometry convention).
fn point_in_rect(x: f64, y: f64, rect: Rect) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

/// Sanitize one dimension of a restored size against the display it will land
/// on: a non-finite or below-floor value resets to `default`, then the result
/// is capped to `available` (the display's extent in that axis) so the
/// window never claims to be bigger than the screen it is restoring onto.
fn sanitized_extent(requested: f64, default: f64, min: f64, available: f64) -> f64 {
    let value = if requested.is_finite() && requested >= min {
        requested
    } else {
        default
    };
    value.min(available.max(min))
}

/// Clamp a position so `[pos, pos + extent)` lies within
/// `[origin, origin + available)`, without panicking when `extent` exceeds
/// `available` (a display smaller than the window's sanitized size).
fn clamp_position(pos: f64, extent: f64, origin: f64, available: f64) -> f64 {
    let max_pos = origin + (available - extent).max(0.0);
    if !pos.is_finite() {
        return origin;
    }
    pos.clamp(origin, max_pos.max(origin))
}

/// Validate restored `bounds` against the current display topology (spec:
/// "Clamp, don't trust"). The display containing the bounds' top-left corner
/// is the clamp target; an off-screen position or a monitor that has since
/// disconnected has no match in `displays`, so both fall back to
/// `displays[0]` (the primary/first display) — the same fallback the
/// "no displays known" case uses, with a hardcoded default rect instead.
/// Degenerate sizes (non-finite, zero, negative, or below the floor) reset to
/// the default window size before being capped to the target display.
pub fn clamp_bounds(bounds: Rect, displays: &[Rect]) -> Rect {
    let Some(target) = displays
        .iter()
        .copied()
        .find(|d| point_in_rect(bounds.x, bounds.y, *d))
        .or(displays.first().copied())
    else {
        return Rect::default();
    };

    let width = sanitized_extent(
        bounds.width,
        DEFAULT_WINDOW_WIDTH,
        MIN_WINDOW_WIDTH,
        target.width,
    );
    let height = sanitized_extent(
        bounds.height,
        DEFAULT_WINDOW_HEIGHT,
        MIN_WINDOW_HEIGHT,
        target.height,
    );
    let x = clamp_position(bounds.x, width, target.x, target.width);
    let y = clamp_position(bounds.y, height, target.y, target.height);

    Rect {
        x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique scratch directory under the OS temp dir — no `tempfile`
    /// dependency, mirroring `rift_logging::sink`'s test helper.
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut dir = std::env::temp_dir();
            dir.push(format!(
                "rift-app-window-state-{}-{tag}-{n}",
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

    fn sample_state() -> WindowState {
        WindowState {
            version: SCHEMA_VERSION,
            bounds: Rect {
                x: 12.0,
                y: 34.0,
                width: 1024.0,
                height: 768.0,
            },
            maximized: true,
            font_size_px: 16.5,
            theme_name: "Default Light".to_string(),
            theme_mode: ThemeMode::Light,
            diff_view_mode: DiffViewMode::Split,
        }
    }

    fn contains(outer: Rect, inner: Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.x + inner.width <= outer.x + outer.width
            && inner.y + inner.height <= outer.y + outer.height
    }

    // --- schema round-trip -------------------------------------------------

    #[test]
    fn test_window_state_round_trips_through_json() {
        let state = sample_state();
        let json = serde_json::to_string(&state).expect("serialize");
        let parsed: WindowState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, state);
    }

    #[test]
    fn test_missing_fields_fall_back_to_default_per_field() {
        let parsed: WindowState = serde_json::from_str(r#"{"maximized": true}"#).expect("parse");
        assert_eq!(parsed.version, SCHEMA_VERSION);
        assert_eq!(parsed.bounds, Rect::default());
        assert!(parsed.maximized);
        assert_eq!(parsed.font_size_px, DEFAULT_FONT_SIZE_PX);
        assert_eq!(parsed.theme_name, crate::DEFAULT_THEME_NAME);
        assert_eq!(parsed.theme_mode, ThemeMode::Dark);
        assert_eq!(parsed.diff_view_mode, DiffViewMode::Unified);
    }

    #[test]
    fn test_unknown_fields_and_future_version_load_known_fields() {
        let json = r#"{
            "version": 99,
            "bounds": {"x": 5.0, "y": 6.0, "width": 640.0, "height": 480.0},
            "maximized": false,
            "font_size_px": 18.0,
            "theme_name": "Default Light",
            "theme_mode": "light",
            "panels": ["editor", "terminal"]
        }"#;
        let parsed: WindowState = serde_json::from_str(json).expect("parse despite unknown field");
        assert_eq!(
            parsed.bounds,
            Rect {
                x: 5.0,
                y: 6.0,
                width: 640.0,
                height: 480.0
            }
        );
        assert!(!parsed.maximized);
        assert_eq!(parsed.font_size_px, 18.0);
        assert_eq!(parsed.theme_name, "Default Light");
        assert_eq!(parsed.theme_mode, ThemeMode::Light);
        assert_eq!(
            parsed.diff_view_mode,
            DiffViewMode::Unified,
            "a field this JSON predates falls back to its default"
        );
    }

    /// A pre-Phase-17 on-disk file (#224's original fields only, no theme
    /// fields at all) still loads — the version-tolerance contract the schema
    /// was designed for, not just a hand-built partial JSON object.
    #[test]
    fn test_pre_phase17_file_without_theme_fields_loads_with_dark_default() {
        let json = r#"{
            "version": 1,
            "bounds": {"x": 12.0, "y": 34.0, "width": 1024.0, "height": 768.0},
            "maximized": true,
            "font_size_px": 16.5
        }"#;
        let parsed: WindowState = serde_json::from_str(json).expect("parse pre-Phase-17 file");
        assert!(parsed.maximized);
        assert_eq!(parsed.font_size_px, 16.5);
        assert_eq!(parsed.theme_name, crate::DEFAULT_THEME_NAME);
        assert_eq!(parsed.theme_mode, ThemeMode::Dark);
        assert_eq!(parsed.diff_view_mode, DiffViewMode::Unified);
    }

    #[test]
    fn test_load_missing_file_returns_default() {
        let scratch = Scratch::new("missing");
        let path = scratch.path("does-not-exist.json");

        assert_eq!(load(&path), WindowState::default());
    }

    #[test]
    fn test_load_corrupt_json_returns_default_without_panic() {
        let scratch = Scratch::new("corrupt");
        let path = scratch.path("state.json");
        fs::write(&path, b"{ this is not valid json").expect("write garbage");

        assert_eq!(load(&path), WindowState::default());
    }

    #[test]
    fn test_load_truncated_json_returns_default_without_panic() {
        let scratch = Scratch::new("truncated");
        let path = scratch.path("state.json");
        let full = serde_json::to_string(&sample_state()).expect("serialize");
        fs::write(&path, &full[..full.len() / 2]).expect("write truncated");

        assert_eq!(load(&path), WindowState::default());
    }

    // --- atomic persistence --------------------------------------------------

    #[test]
    fn test_save_then_load_round_trips() {
        let scratch = Scratch::new("roundtrip");
        let path = scratch.path("state.json");
        let state = sample_state();

        save(&path, &state).expect("save");

        assert_eq!(load(&path), state);
    }

    #[test]
    fn test_save_creates_parent_directories() {
        let scratch = Scratch::new("mkdirp");
        let path = scratch.path("nested").join("dir").join("state.json");

        save(&path, &sample_state()).expect("save creates parents");

        assert!(path.exists());
    }

    #[test]
    fn test_save_cleans_up_temp_file_after_rename() {
        let scratch = Scratch::new("tmpcleanup");
        let path = scratch.path("state.json");

        save(&path, &sample_state()).expect("save");

        assert!(!tmp_path_for(&path).exists());
    }

    #[test]
    fn test_simulated_interruption_never_corrupts_the_target_file() {
        let scratch = Scratch::new("interrupt");
        let path = scratch.path("state.json");
        let good_state = sample_state();
        save(&path, &good_state).expect("initial save");

        // Simulate a crash between the temp write and the rename: garbage
        // lands in the sibling temp path, but the target is never touched.
        fs::write(tmp_path_for(&path), b"{ garbage, mid-write").expect("write garbage temp");

        assert_eq!(load(&path), good_state);

        // A subsequent real save still completes normally — the leftover
        // garbage temp file is simply overwritten, not appended to.
        let mut next_state = good_state.clone();
        next_state.maximized = !good_state.maximized;
        save(&path, &next_state).expect("save after interruption");

        assert_eq!(load(&path), next_state);
    }

    // --- theme persistence -----------------------------------------------

    #[test]
    fn test_save_theme_updates_only_theme_fields_and_preserves_the_rest() {
        let scratch = Scratch::new("save_theme");
        let path = scratch.path("state.json");
        let initial = sample_state();
        save(&path, &initial).expect("initial save");

        save_theme(&path, "Catppuccin Mocha", ThemeMode::Dark).expect("save_theme");

        let loaded = load(&path);
        assert_eq!(loaded.theme_name, "Catppuccin Mocha");
        assert_eq!(loaded.theme_mode, ThemeMode::Dark);
        assert_eq!(loaded.bounds, initial.bounds);
        assert_eq!(loaded.maximized, initial.maximized);
        assert_eq!(loaded.font_size_px, initial.font_size_px);
    }

    #[test]
    fn test_save_theme_on_missing_file_starts_from_defaults() {
        let scratch = Scratch::new("save_theme_missing");
        let path = scratch.path("does-not-exist.json");

        save_theme(&path, "Default Light", ThemeMode::Light).expect("save_theme");

        let loaded = load(&path);
        assert_eq!(loaded.theme_name, "Default Light");
        assert_eq!(loaded.theme_mode, ThemeMode::Light);
        assert_eq!(loaded.bounds, Rect::default());
    }

    /// The regression `save_theme_mode` exists to prevent (issue #443): a
    /// persisted mode flip must not overwrite the persisted named-theme
    /// selection with the other slot's theme name.
    #[test]
    fn test_save_theme_mode_updates_only_mode_and_preserves_the_rest() {
        let scratch = Scratch::new("save_theme_mode");
        let path = scratch.path("state.json");
        let initial = sample_state();
        save(&path, &initial).expect("initial save");

        save_theme_mode(&path, ThemeMode::Dark).expect("save_theme_mode");

        let loaded = load(&path);
        assert_eq!(loaded.theme_mode, ThemeMode::Dark);
        assert_eq!(loaded.theme_name, initial.theme_name);
        assert_eq!(loaded.bounds, initial.bounds);
        assert_eq!(loaded.maximized, initial.maximized);
        assert_eq!(loaded.font_size_px, initial.font_size_px);
    }

    #[test]
    fn test_save_theme_mode_on_missing_file_starts_from_defaults() {
        let scratch = Scratch::new("save_theme_mode_missing");
        let path = scratch.path("does-not-exist.json");

        save_theme_mode(&path, ThemeMode::Light).expect("save_theme_mode");

        let loaded = load(&path);
        assert_eq!(loaded.theme_mode, ThemeMode::Light);
        assert_eq!(loaded.theme_name, crate::DEFAULT_THEME_NAME);
        assert_eq!(loaded.bounds, Rect::default());
    }

    /// The regression `save_geometry` exists to prevent: a debounced
    /// move/resize/font-change save must not clobber a theme the user picked
    /// via `save_theme` back to the schema default.
    #[test]
    fn test_save_geometry_updates_only_geometry_fields_and_preserves_theme() {
        let scratch = Scratch::new("save_geometry");
        let path = scratch.path("state.json");
        save_theme(&path, "Catppuccin Mocha", ThemeMode::Dark).expect("initial save_theme");

        let new_bounds = Rect {
            x: 12.0,
            y: 34.0,
            width: 1024.0,
            height: 768.0,
        };
        save_geometry(&path, new_bounds, true, 18.0).expect("save_geometry");

        let loaded = load(&path);
        assert_eq!(loaded.bounds, new_bounds);
        assert!(loaded.maximized);
        assert_eq!(loaded.font_size_px, 18.0);
        assert_eq!(loaded.theme_name, "Catppuccin Mocha");
        assert_eq!(loaded.theme_mode, ThemeMode::Dark);
    }

    // --- diff-view-mode persistence (#547) ----------------------------------

    #[test]
    fn test_save_diff_view_mode_updates_only_that_field_and_preserves_the_rest() {
        let scratch = Scratch::new("save_diff_view_mode");
        let path = scratch.path("state.json");
        let initial = WindowState {
            diff_view_mode: DiffViewMode::Unified,
            ..sample_state()
        };
        save(&path, &initial).expect("initial save");

        save_diff_view_mode(&path, DiffViewMode::Split).expect("save_diff_view_mode");

        let loaded = load(&path);
        assert_eq!(loaded.diff_view_mode, DiffViewMode::Split);
        assert_eq!(loaded.theme_name, initial.theme_name);
        assert_eq!(loaded.theme_mode, initial.theme_mode);
        assert_eq!(loaded.bounds, initial.bounds);
        assert_eq!(loaded.maximized, initial.maximized);
        assert_eq!(loaded.font_size_px, initial.font_size_px);
    }

    #[test]
    fn test_save_diff_view_mode_on_missing_file_starts_from_defaults() {
        let scratch = Scratch::new("save_diff_view_mode_missing");
        let path = scratch.path("does-not-exist.json");

        save_diff_view_mode(&path, DiffViewMode::Split).expect("save_diff_view_mode");

        let loaded = load(&path);
        assert_eq!(loaded.diff_view_mode, DiffViewMode::Split);
        assert_eq!(loaded.theme_name, crate::DEFAULT_THEME_NAME);
        assert_eq!(loaded.bounds, Rect::default());
    }

    // --- platform path resolution -------------------------------------------

    #[test]
    fn test_state_dir_prefers_localappdata() {
        let dir = state_dir_from(
            Some(OsStr::new("C:\\Users\\dev\\AppData\\Local")),
            Some(OsStr::new("/xdg/state")),
            Some(OsStr::new("/home/dev")),
        )
        .expect("resolves");

        assert_eq!(
            dir,
            PathBuf::from("C:\\Users\\dev\\AppData\\Local").join("rift")
        );
    }

    #[test]
    fn test_state_dir_falls_back_to_xdg_state_home() {
        let dir = state_dir_from(
            None,
            Some(OsStr::new("/xdg/state")),
            Some(OsStr::new("/home/dev")),
        )
        .expect("resolves");

        assert_eq!(dir, PathBuf::from("/xdg/state").join("rift"));
    }

    #[test]
    fn test_state_dir_falls_back_to_home_local_state() {
        let dir = state_dir_from(None, None, Some(OsStr::new("/home/dev"))).expect("resolves");

        assert_eq!(
            dir,
            PathBuf::from("/home/dev")
                .join(".local")
                .join("state")
                .join("rift")
        );
    }

    #[test]
    fn test_state_dir_errors_when_nothing_is_set() {
        assert!(matches!(
            state_dir_from(None, None, None),
            Err(StoreError::NoStateDir)
        ));
    }

    // --- per-channel keying --------------------------------------------------

    #[test]
    fn test_stable_and_dev_channels_resolve_different_file_names() {
        let stable = state_file_name(true);
        let dev = state_file_name(false);

        assert_ne!(stable, dev);
        assert_eq!(stable, "rift-stable-window-state.json");
        assert_eq!(dev, "rift-dev-window-state.json");
    }

    // --- clamp ---------------------------------------------------------------

    #[test]
    fn test_clamp_leaves_bounds_already_inside_the_display_unchanged() {
        let display = Rect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let bounds = Rect {
            x: 100.0,
            y: 100.0,
            width: 800.0,
            height: 600.0,
        };

        assert_eq!(clamp_bounds(bounds, &[display]), bounds);
    }

    #[test]
    fn test_clamp_relocates_off_screen_bounds_into_the_display() {
        let display = Rect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let bounds = Rect {
            x: -5000.0,
            y: -5000.0,
            width: 800.0,
            height: 600.0,
        };

        let clamped = clamp_bounds(bounds, &[display]);
        assert!(contains(display, clamped));
    }

    #[test]
    fn test_clamp_falls_back_to_primary_when_the_monitor_has_vanished() {
        // The window was on a secondary monitor to the right that is no
        // longer connected; only the primary display remains.
        let primary = Rect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let bounds = Rect {
            x: 2200.0,
            y: 100.0,
            width: 800.0,
            height: 600.0,
        };

        let clamped = clamp_bounds(bounds, &[primary]);
        assert!(contains(primary, clamped));
    }

    #[test]
    fn test_clamp_replaces_degenerate_sizes_with_the_default() {
        let display = Rect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        for degenerate in [0.0, -10.0, f64::NAN, f64::INFINITY] {
            let bounds = Rect {
                x: 50.0,
                y: 50.0,
                width: degenerate,
                height: degenerate,
            };
            let clamped = clamp_bounds(bounds, &[display]);
            assert!(contains(display, clamped), "degenerate size {degenerate}");
            assert!(clamped.width >= MIN_WINDOW_WIDTH);
            assert!(clamped.height >= MIN_WINDOW_HEIGHT);
        }
    }

    #[test]
    fn test_clamp_with_no_displays_returns_the_default_rect() {
        let bounds = Rect {
            x: 100.0,
            y: 100.0,
            width: 800.0,
            height: 600.0,
        };

        assert_eq!(clamp_bounds(bounds, &[]), Rect::default());
    }
}
