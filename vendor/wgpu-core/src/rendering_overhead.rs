//! Pre-start app request for the two conservative Metal command-buffer skips.
//! Devices and queues capture their own setting at creation; later requests
//! affect only new objects. Each exact diagnostic environment 0/1 overrides
//! this default-off request independently. No process environment is changed.
use core::sync::atomic::{AtomicBool, Ordering};

pub const METAL_TRANSIT_ENVIRONMENT: &str = "MUGUET_METAL_SKIP_EMPTY_TRANSIT";
pub const METAL_PREPASS_ENVIRONMENT: &str = "MUGUET_METAL_SKIP_EMPTY_PREPASS";

static APP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Apply the application's experimental rendering choice before GPU startup.
/// This does not change any existing device or queue's captured setting.
pub fn set_app_requested(enabled: bool) {
    APP_REQUESTED.store(enabled, Ordering::Release);
}

/// Resolve a diagnostic value against an explicit app request without reading
/// or changing process state. Only the exact values `0` and `1` override it.
pub fn resolve_request(environment: Option<&str>, app_requested: bool) -> bool {
    match environment { Some("1") => true, Some("0") => false, _ => app_requested }
}

fn flag_with(name: &str, app_requested: bool, lookup: impl FnOnce(&str) -> Option<alloc::string::String>) -> bool {
    let environment = lookup(name);
    resolve_request(environment.as_deref(), app_requested)
}

fn requested(name: &str) -> bool {
    let app_requested = APP_REQUESTED.load(Ordering::Acquire);
    #[cfg(feature = "std")]
    { flag_with(name, app_requested, |name| std::env::var(name).ok()) }
    #[cfg(not(feature = "std"))]
    { let _ = name; resolve_request(None, app_requested) }
}

/// Resolved request; only a Metal queue can enable the Transit skip.
pub fn metal_transit_requested() -> bool { requested(METAL_TRANSIT_ENVIRONMENT) }

/// Resolved request; only a Metal device can enable the per-pass skip.
pub fn metal_prepass_requested() -> bool { requested(METAL_PREPASS_ENVIRONMENT) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_overrides_preserve_default_off_and_prestart_choice() {
        for requested in [false, true] {
            for value in [None, Some(""), Some("true"), Some("2"), Some(" 1"), Some("1 ")] {
                assert_eq!(resolve_request(value, requested), requested);
            }
            assert!(resolve_request(Some("1"), requested));
            assert!(!resolve_request(Some("0"), requested));
        }
        assert!(!resolve_request(None, false));
    }

    #[test]
    fn diagnostic_overrides_are_independent_of_each_other_and_other_experiments() {
        let flags = [METAL_TRANSIT_ENVIRONMENT, METAL_PREPASS_ENVIRONMENT];
        for flag in flags {
            for target in [METAL_TRANSIT_ENVIRONMENT, METAL_PREPASS_ENVIRONMENT,
                           "MUGUET_OP47_UPLOAD_DEDUP", "MUGUET_BUFFER_UPLOAD_MODE"] {
                assert_eq!(flag_with(flag, false, |name| (name == target).then(|| "1".into())), flag == target);
                assert_eq!(flag_with(flag, true, |name| (name == target).then(|| "0".into())), flag != target);
            }
        }
    }
}
