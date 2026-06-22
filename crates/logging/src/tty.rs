use std::io::IsTerminal;

/// Env var that overrides TTY-based sink selection. A truthy value forces the
/// console sink (e.g. a windowed exe attached to a console, or a piped interop run
/// that should keep console output); a falsy value forces the file sink. Unset or
/// unrecognized falls back to live TTY detection.
pub const FORCE_CONSOLE_ENV: &str = "RIFT_LOG_CONSOLE";

/// Where log output should go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogTarget {
    /// A terminal is attached (or forced): log to the console.
    Console,
    /// No terminal (windowed/redirected, or forced): log to the rotating file.
    File,
}

/// Select the log target from the live environment, checking whether stderr is a
/// terminal. Consumers that key off a different stream (e.g. the app's stdout
/// console) can call [`log_target_from`] with their own TTY check.
pub fn log_target() -> LogTarget {
    log_target_from(
        std::env::var(FORCE_CONSOLE_ENV).ok().as_deref(),
        std::io::stderr().is_terminal(),
    )
}

/// Pure target-selection policy: the env override wins in either direction, else
/// the TTY check decides. An unrecognized override value is ignored (falls back to
/// the TTY check).
pub fn log_target_from(force_console: Option<&str>, stream_is_tty: bool) -> LogTarget {
    match force_console.and_then(parse_force) {
        Some(true) => LogTarget::Console,
        Some(false) => LogTarget::File,
        None if stream_is_tty => LogTarget::Console,
        None => LogTarget::File,
    }
}

fn parse_force(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tty_selects_console_when_no_override() {
        assert_eq!(log_target_from(None, true), LogTarget::Console);
    }

    #[test]
    fn test_no_tty_selects_file_when_no_override() {
        assert_eq!(log_target_from(None, false), LogTarget::File);
    }

    #[test]
    fn test_override_forces_console_over_no_tty() {
        assert_eq!(log_target_from(Some("1"), false), LogTarget::Console);
        assert_eq!(log_target_from(Some("true"), false), LogTarget::Console);
        assert_eq!(log_target_from(Some("ON"), false), LogTarget::Console);
    }

    #[test]
    fn test_override_forces_file_over_tty() {
        assert_eq!(log_target_from(Some("0"), true), LogTarget::File);
        assert_eq!(log_target_from(Some("false"), true), LogTarget::File);
        assert_eq!(log_target_from(Some("off"), true), LogTarget::File);
    }

    #[test]
    fn test_unrecognized_override_falls_back_to_tty() {
        assert_eq!(log_target_from(Some("maybe"), true), LogTarget::Console);
        assert_eq!(log_target_from(Some(""), false), LogTarget::File);
    }
}
