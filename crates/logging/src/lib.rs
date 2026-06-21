//! Shared logging sink for rift's app and daemon.
//!
//! `gpui`-free and musl-clean — the daemon depends on it — so the crate is pure
//! `std` plus the `tracing` / `tracing-subscriber` facade already in the
//! workspace. It provides the pieces both binaries need identically, so there is
//! no duplicated rotation/filter/panic code and no behavioral drift between
//! channels:
//!
//! - [`SizedWriter`] — a size-rotating append writer (`.log` / `.log.old` pair)
//!   that replaces per-run truncation, wrapped by [`RotatingMakeWriter`] for the
//!   `tracing` fmt layer.
//! - [`build_filter`] — the `RIFT_LOG` -> `RUST_LOG` -> default filter chain,
//!   with the noisy-dependency suppression baked into the default tier.
//! - [`install_panic_hook`] — routes panics through `tracing::error!` before
//!   delegating to the previously installed hook.
//! - [`log_target`] — TTY-based console/file selection with one console-forcing
//!   env override.

mod filter;
mod panic;
mod sink;
mod tty;

pub use filter::{build_filter, resolve_directives, DEFAULT_FILTER};
pub use panic::install_panic_hook;
pub use sink::{LockedWriter, RotatingMakeWriter, SizedWriter, DEFAULT_MAX_BYTES};
pub use tty::{log_target, log_target_from, LogTarget, FORCE_CONSOLE_ENV};
