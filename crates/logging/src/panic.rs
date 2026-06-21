use std::any::Any;
use std::panic;

/// Install a panic hook that records the panicking thread, source location, and
/// message through `tracing::error!` — so panics land in whatever sinks are active
/// in every profile — and then delegates to the previously installed hook (so the
/// default backtrace behavior is preserved).
pub fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        tracing::error!(
            thread = thread_name,
            location = %location,
            message = %payload_message(info.payload()),
            "panic"
        );
        previous(info);
    }));
}

/// Extract a human-readable message from a panic payload. A panic carries either a
/// `&'static str` or a `String`; anything else is reported as opaque.
fn payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bind the payload to a typed variable so `&value` erases to the concrete
    // type a real panic carries (the `&str` / `String` cases below).

    #[test]
    fn test_payload_message_reads_static_str() {
        let payload: &str = "boom";
        assert_eq!(payload_message(&payload), "boom");
    }

    #[test]
    fn test_payload_message_reads_string() {
        let payload: String = "owned boom".to_string();
        assert_eq!(payload_message(&payload), "owned boom");
    }

    #[test]
    fn test_payload_message_reports_non_string_payload() {
        let payload: u32 = 7;
        assert_eq!(payload_message(&payload), "<non-string panic payload>");
    }
}
