use tracing_subscriber::EnvFilter;

/// Built-in fallback filter: the developer-friendly `rift` defaults plus
/// suppression of the known-noisy GPU dependencies (the wezterm pattern) so a
/// debug-level run stays readable. The floor is `error`, not `off`, so a real GPU
/// error still surfaces.
pub const DEFAULT_FILTER: &str = "rift=debug,rift_ssh=debug,wgpu_core=error,wgpu_hal=error";

/// Resolve the directive string by precedence: `RIFT_LOG` beats `RUST_LOG` beats
/// the built-in [`DEFAULT_FILTER`]. An empty (or whitespace-only) value is treated
/// as unset, so an exported-but-blank variable does not silence everything.
pub fn resolve_directives(rift_log: Option<String>, rust_log: Option<String>) -> String {
    first_non_blank(rift_log)
        .or_else(|| first_non_blank(rust_log))
        .unwrap_or_else(|| DEFAULT_FILTER.to_string())
}

fn first_non_blank(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// Build the process [`EnvFilter`] from the live environment using the precedence
/// in [`resolve_directives`]. Parsing is lossy: an invalid directive is skipped
/// (with a warning) rather than aborting startup.
pub fn build_filter() -> EnvFilter {
    let directives = resolve_directives(
        std::env::var("RIFT_LOG").ok(),
        std::env::var("RUST_LOG").ok(),
    );
    EnvFilter::new(directives)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rift_log_beats_rust_log_and_default() {
        let resolved = resolve_directives(Some("a=info".into()), Some("b=warn".into()));
        assert_eq!(resolved, "a=info");
    }

    #[test]
    fn test_rust_log_beats_default_when_rift_log_unset() {
        let resolved = resolve_directives(None, Some("b=warn".into()));
        assert_eq!(resolved, "b=warn");
    }

    #[test]
    fn test_default_used_when_both_unset() {
        let resolved = resolve_directives(None, None);
        assert_eq!(resolved, DEFAULT_FILTER);
    }

    #[test]
    fn test_blank_values_are_treated_as_unset() {
        assert_eq!(
            resolve_directives(Some("   ".into()), Some("b=warn".into())),
            "b=warn"
        );
        assert_eq!(
            resolve_directives(Some(String::new()), None),
            DEFAULT_FILTER
        );
    }

    #[test]
    fn test_default_keeps_rift_debug_and_suppresses_wgpu() {
        assert!(DEFAULT_FILTER.contains("rift=debug"));
        assert!(DEFAULT_FILTER.contains("rift_ssh=debug"));
        assert!(DEFAULT_FILTER.contains("wgpu_core=error"));
        assert!(DEFAULT_FILTER.contains("wgpu_hal=error"));
    }

    #[test]
    fn test_default_filter_parses_without_dropping_directives() {
        // Lossy parsing must accept every directive in the default (Display may
        // reorder them, so check membership rather than exact equality).
        let rendered = EnvFilter::new(DEFAULT_FILTER).to_string();
        for directive in DEFAULT_FILTER.split(',') {
            assert!(
                rendered.contains(directive),
                "default directive `{directive}` was dropped: {rendered}"
            );
        }
    }
}
