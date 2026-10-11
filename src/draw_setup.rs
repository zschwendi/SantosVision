//! Pre-start app choice for three independent, default-off draw setup experiments.
//! The app calls the typed setter before runtime startup. Diagnostic environment
//! values override each flag individually; no process environment is mutated.
use std::sync::atomic::{AtomicBool, Ordering};

static APP_REQUESTED: AtomicBool = AtomicBool::new(false);
const RENDER_PREPARE_ENV: &str = "MUGUET_RENDER_PREPARE_OPT";
const UNIFORM_STAGE_PACK_ENV: &str = "MUGUET_UNIFORM_STAGE_PACK_OPT";
const BIND_GROUP_PREPARE_ENV: &str = "MUGUET_BIND_GROUP_PREPARE_OPT";

pub(crate) fn set_app_requested(enabled: bool) {
    APP_REQUESTED.store(enabled, Ordering::Release);
}

pub(crate) fn app_requested() -> bool {
    APP_REQUESTED.load(Ordering::Acquire)
}

fn resolve(environment: Option<&str>, app_requested: bool) -> bool {
    match environment {
        Some("1") => true,
        Some("0") => false,
        _ => app_requested,
    }
}

fn flag_with(name: &str, app_requested: bool, lookup: impl FnOnce(&str) -> Option<String>) -> bool {
    let environment = lookup(name);
    resolve(environment.as_deref(), app_requested)
}

fn flag(name: &str) -> bool {
    flag_with(name, app_requested(), |name| std::env::var(name).ok())
}

pub(crate) fn render_prepare_enabled() -> bool {
    flag(RENDER_PREPARE_ENV)
}

pub(crate) fn uniform_stage_pack_enabled() -> bool {
    flag(UNIFORM_STAGE_PACK_ENV)
}

/// Resolve the same saved request for the cache-hit preparation experiment.
pub(crate) fn bind_group_prepare_enabled() -> bool {
    flag(BIND_GROUP_PREPARE_ENV)
}

pub(crate) fn status_json() -> serde_json::Value {
    serde_json::json!({
        "app_requested": app_requested(),
        "render_prepare": render_prepare_enabled(),
        "uniform_stage_pack": uniform_stage_pack_enabled(),
        "bind_group_prepare": bind_group_prepare_enabled(),
    })
}

#[cfg(test)]
mod tests {
    use super::{flag_with, resolve, BIND_GROUP_PREPARE_ENV, RENDER_PREPARE_ENV,
                UNIFORM_STAGE_PACK_ENV};

    #[test]
    fn exact_environment_values_override_prestart_app_choice() {
        for requested in [false, true] {
            assert_eq!(resolve(None, requested), requested);
            assert_eq!(resolve(Some(""), requested), requested);
            assert_eq!(resolve(Some("true"), requested), requested);
            assert_eq!(resolve(Some("2"), requested), requested);
            assert!(resolve(Some("1"), requested));
            assert!(!resolve(Some("0"), requested));
        }
    }

    #[test]
    fn each_diagnostic_flag_overrides_only_its_own_saved_choice() {
        assert_eq!(RENDER_PREPARE_ENV, "MUGUET_RENDER_PREPARE_OPT");
        assert_eq!(UNIFORM_STAGE_PACK_ENV, "MUGUET_UNIFORM_STAGE_PACK_OPT");
        assert_eq!(BIND_GROUP_PREPARE_ENV, "MUGUET_BIND_GROUP_PREPARE_OPT");
        for flag in [RENDER_PREPARE_ENV, UNIFORM_STAGE_PACK_ENV, BIND_GROUP_PREPARE_ENV] {
            for target in [RENDER_PREPARE_ENV, UNIFORM_STAGE_PACK_ENV, BIND_GROUP_PREPARE_ENV] {
                assert_eq!(flag_with(flag, false, |name| (name == target).then(|| "1".into())),
                           flag == target);
                assert_eq!(flag_with(flag, true, |name| (name == target).then(|| "0".into())),
                           flag != target);
            }
        }
    }
}
