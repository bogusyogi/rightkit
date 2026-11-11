//! Host hardware detection and model fit, delegated to `rightkit-models`
//! (macOS `sysctl` + `system_profiler`, Windows kernel32/`reg.exe`, Linux
//! `/proc/meminfo`). `RIGHTKIT_HW_JSON` injects a full profile for tests or
//! hardware reported by another machine (it wins over the shared
//! `RIGHTKIT_HW_JSON`).

use rightkit_models::{
    Hardware, HardwareOverride, Measurement, ProfileAlias, RemoteRoute, Requirements,
};
use serde_json::{json, Value};

pub fn probe() -> Hardware {
    let raw = std::env::var("RIGHTKIT_HW_JSON").ok();
    let mut hw = Hardware::probe_with(raw.as_deref().map(HardwareOverride::Json))
        .unwrap_or_else(|_| Hardware::probe());
    // The recorded RTX 4070 laptop measurements are filed under this profile.
    hw.apply_profile_aliases(&[ProfileAlias {
        alias: "rtx4070-8gb".into(),
        gpu_name_contains: Some("4070".into()),
        primary: true,
        ..Default::default()
    }]);
    hw
}

/// `{os, arch, memory_gb, gpus[], profile, source}`.
pub fn detect_hardware() -> Value {
    serde_json::to_value(probe()).unwrap_or_else(|_| json!({}))
}

/// Manifest `requirements` (the shared shape's own keys). Rented/experimental
/// engines have no local backend (`remote_only`), so their fit is
/// `rental_only` unless a measurement names this hardware.
fn requirements(entry: &Value) -> Requirements {
    let r = entry["requirements"].clone();
    let mut req: Requirements = serde_json::from_value(r).unwrap_or_default();
    if matches!(
        entry["engine_kind"].as_str(),
        Some("rented-remote" | "experimental")
    ) {
        req.remote_only = Some(RemoteRoute::Rental);
    }
    req
}

fn measurements(entry: &Value) -> Vec<Measurement> {
    entry["measured"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| serde_json::from_value(m.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Fit: `measured` only when a recorded measurement names this
/// hardware profile; otherwise `estimated`/`unsupported`/`unqualified`.
pub fn local_fit(entry: &Value, hw: &Value) -> Value {
    let hw: Hardware = serde_json::from_value(hw.clone()).unwrap_or_else(|_| probe());
    let fit = hw.estimate_fit(&requirements(entry), &measurements(entry));
    let mut out = json!({
        "fit": fit.kind,
        "profile": fit.profile,
        "required_gb": fit.required_gb,
        "available_gb": fit.available_gb,
        "evidence": fit.evidence,
        "reasons": fit.reasons,
    });
    if let Some(o) = out.as_object_mut() {
        o.retain(|_, v| !v.is_null());
    }
    out
}

/// Fit of a billed provider model: the shared `Api` class (no hardware fit).
pub fn api_fit(est_usd: &Value) -> Value {
    let req = Requirements {
        remote_only: Some(RemoteRoute::Api),
        ..Default::default()
    };
    let fit = probe().estimate_fit(&req, &[]);
    json!({"fit": fit.kind, "est_usd": est_usd, "reasons": fit.reasons})
}
