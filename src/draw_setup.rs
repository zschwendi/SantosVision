//! Pre-start app choice for seven independent, default-off rendering experiments.
//! The app calls the typed setter before runtime startup. Diagnostic environment
//! values override each flag individually; no process environment is mutated.
use std::sync::atomic::{AtomicBool, Ordering};

static APP_REQUESTED: AtomicBool = AtomicBool::new(false);
const RENDER_PREPARE_ENV: &str = "MUGUET_RENDER_PREPARE_OPT";
const UNIFORM_STAGE_PACK_ENV: &str = "MUGUET_UNIFORM_STAGE_PACK_OPT";
const BIND_GROUP_PREPARE_ENV: &str = "MUGUET_BIND_GROUP_PREPARE_OPT";
const UNIFORM_SNAPSHOT_ENV: &str = "MUGUET_UNIFORM_SNAPSHOT_CACHE";
pub(crate) const XR_EMISSION_ENV: &str = "MUGUET_XR_EMISSION_FASTPATH";

pub(crate) fn set_app_requested(enabled: bool) {
    wgpu::wgc::rendering_overhead::set_app_requested(enabled);
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

pub(crate) fn uniform_snapshot_enabled() -> bool {
    flag(UNIFORM_SNAPSHOT_ENV)
}

pub(crate) fn xr_emission_override(value: Option<&str>) -> Option<bool> {
    match value { Some("1") => Some(true), Some("0") => Some(false), _ => None }
}

pub(crate) fn xr_emission_requested(environment: Option<bool>) -> bool {
    environment.unwrap_or_else(app_requested)
}

pub(crate) fn status_json() -> serde_json::Value {
    serde_json::json!({
        "app_requested": app_requested(),
        "render_prepare": render_prepare_enabled(),
        "uniform_stage_pack": uniform_stage_pack_enabled(),
        "bind_group_prepare": bind_group_prepare_enabled(),
        "bind_group_key_first": crate::gpu::bind_group_key_first_status_json(),
        "uniform_snapshot": uniform_snapshot_enabled(),
        "metal_skip_empty_transit": wgpu::wgc::rendering_overhead::metal_transit_requested(),
        "metal_skip_empty_prepass": wgpu::wgc::rendering_overhead::metal_prepass_requested(),
    })
}

#[cfg(test)]
mod tests {
    use super::{flag_with, resolve, BIND_GROUP_PREPARE_ENV, RENDER_PREPARE_ENV,
                UNIFORM_STAGE_PACK_ENV, UNIFORM_SNAPSHOT_ENV, XR_EMISSION_ENV};
    use wgpu::wgc::rendering_overhead::{resolve_request, METAL_TRANSIT_ENVIRONMENT, METAL_PREPASS_ENVIRONMENT};

    #[test]
    fn metal_backend_resolution_matches_the_prestart_launcher_contract() {
        for requested in [false, true] {
            for value in [None, Some(""), Some("true"), Some("2"), Some("0"), Some("1"), Some(" 1"), Some("1 ")] {
                assert_eq!(resolve_request(value, requested), resolve(value, requested));
            }
        }
        assert!(!resolve_request(None, false));
        assert_eq!(METAL_TRANSIT_ENVIRONMENT, "MUGUET_METAL_SKIP_EMPTY_TRANSIT");
        assert_eq!(METAL_PREPASS_ENVIRONMENT, "MUGUET_METAL_SKIP_EMPTY_PREPASS");
    }

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
    fn cached_xr_override_preserves_a_later_prestart_choice() {
        use super::{xr_emission_override, resolve};
        for requested in [false, true] {
            for value in [None, Some(""), Some("true"), Some("0"), Some("1")] {
                let cached = xr_emission_override(value);
                assert_eq!(cached.unwrap_or(requested), resolve(value, requested));
            }
        }
        assert_eq!(xr_emission_override(None), None);
    }

    #[test]
    fn each_diagnostic_flag_overrides_only_its_own_saved_choice() {
        assert_eq!(RENDER_PREPARE_ENV, "MUGUET_RENDER_PREPARE_OPT");
        assert_eq!(UNIFORM_STAGE_PACK_ENV, "MUGUET_UNIFORM_STAGE_PACK_OPT");
        assert_eq!(BIND_GROUP_PREPARE_ENV, "MUGUET_BIND_GROUP_PREPARE_OPT");
        for flag in [RENDER_PREPARE_ENV, UNIFORM_STAGE_PACK_ENV, BIND_GROUP_PREPARE_ENV, UNIFORM_SNAPSHOT_ENV, XR_EMISSION_ENV,
                     METAL_TRANSIT_ENVIRONMENT, METAL_PREPASS_ENVIRONMENT] {
            for target in [RENDER_PREPARE_ENV, UNIFORM_STAGE_PACK_ENV, BIND_GROUP_PREPARE_ENV, UNIFORM_SNAPSHOT_ENV, XR_EMISSION_ENV,
                           METAL_TRANSIT_ENVIRONMENT, METAL_PREPASS_ENVIRONMENT, "MUGUET_OP47_UPLOAD_DEDUP", "MUGUET_BUFFER_UPLOAD_MODE"] {
                assert_eq!(flag_with(flag, false, |name| (name == target).then(|| "1".into())),
                           flag == target);
                assert_eq!(flag_with(flag, true, |name| (name == target).then(|| "0".into())),
                           flag != target);
            }
        }
    }
}
