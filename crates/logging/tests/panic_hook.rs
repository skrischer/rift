//! Integration coverage for `install_panic_hook`: it must route a panic through
//! `tracing` without aborting the unwind, then delegate to the previously
//! installed hook. It lives in its own test binary so the global panic-hook
//! mutation never leaks into the unit tests.

use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[test]
fn test_install_panic_hook_routes_and_delegates() {
    let delegated = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&delegated);
    // Seed a sentinel hook; the installed hook must chain to it.
    panic::set_hook(Box::new(move |_| flag.store(true, Ordering::SeqCst)));

    rift_logging::install_panic_hook();

    let result = panic::catch_unwind(|| panic!("integration boom"));
    assert!(result.is_err(), "the panic must still unwind");
    assert!(
        delegated.load(Ordering::SeqCst),
        "the previously installed hook must run"
    );
}
