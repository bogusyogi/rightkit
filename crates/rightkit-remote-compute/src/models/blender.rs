//! Catalog and doctor status for the user-managed RenderRight/Blender runtime.
//! This module observes provider metadata only; GPU qualification is a normal
//! `blender-probe` generation job, never a catalog or discovery side effect.

use rightkit_media_gen::providers::{registry, Provider};
use serde_json::{json, Value};

const PROVIDER_ID: &str = "local-blender";

fn provider() -> Option<Box<dyn Provider>> {
    registry().into_iter().find(|p| p.id() == PROVIDER_ID)
}

fn setup() -> Value {
    json!({
        "mode": "user_managed_external_runtime",
        "download_available": false,
        "blender_path_env": "RIGHTKIT_BLENDER_PATH",
        "worker_path_env": "RIGHTKIT_BLENDER_WORKER_DIR",
        "worker_default": rightkit_media_gen::providers::orthic_root()
            .map(|p| p.join("shared").join("tools").join("renderright").to_string_lossy().into_owned()),
        "worker_install_recipe": "Install Blender and the RenderRight worker separately, then point these paths at them.",
    })
}

/// Produce external-worker catalog rows from the provider's public model list.
pub fn manifest_entries() -> Vec<Value> {
    let Some(provider) = provider() else {
        return Vec::new();
    };
    provider.models().iter().map(|model| {
        json!({
            "id": model.model,
            "label": model.label,
            "modality": model.kind,
            "engine": PROVIDER_ID,
            "engine_kind": "external-worker",
            "revision": "user-managed",
            "requirements": {},
            "capabilities": [],
            "license": {"name": "Blender and RenderRight worker licenses; inspect installed distributions"},
            "est_usd": model.est_usd,
            "provider": PROVIDER_ID,
            "external_readiness": {
                "key_required": false,
                "runtime_owner": "user",
                "setup": setup(),
                "gpu_qualification": {"state": "not_run", "model": "blender-probe", "via": "host generation job"}
            }
        })
    }).collect()
}

/// Cheap path and manifest checks for catalog/describe/probe. It does not run
/// Blender, render a frame, or qualify a GPU.
pub fn readiness() -> Value {
    let Some(provider) = provider() else {
        return json!({
            "state": "external_unavailable",
            "engine": PROVIDER_ID,
            "external": true,
            "key_required": false,
            "provider_registered": false,
            "setup": setup(),
            "gpu_qualification": {"state": "not_run", "model": "blender-probe", "via": "host generation job"}
        });
    };
    let checked = provider.runtime_ready();
    match checked {
        Some(Ok(())) => json!({
            "state": "external_available",
            "engine": PROVIDER_ID,
            "external": true,
            "key_required": false,
            "metadata_verified": true,
            "version_verification": "doctor",
            "setup": setup(),
            "gpu_qualification": {"state": "not_run", "model": "blender-probe", "via": "host generation job"}
        }),
        Some(Err(error)) => json!({
            "state": "external_unavailable",
            "engine": PROVIDER_ID,
            "external": true,
            "key_required": false,
            "metadata_verified": false,
            "code": error.code,
            "message": error.message,
            "setup": setup(),
            "gpu_qualification": {"state": "not_run", "model": "blender-probe", "via": "host generation job"}
        }),
        None => json!({
            "state": "external_unverified",
            "engine": PROVIDER_ID,
            "external": true,
            "key_required": false,
            "metadata_verified": false,
            "message": "Provider does not expose an external runtime readiness check",
            "setup": setup(),
            "gpu_qualification": {"state": "not_run", "model": "blender-probe", "via": "host generation job"}
        }),
    }
}

/// Doctor performs the provider's cached Blender/worker/recipe version check.
/// The provider persists its probe identity; no GPU render is started here.
pub fn doctor_readiness() -> Value {
    let Some(provider) = provider() else {
        return json!({
            "state": "external_unavailable",
            "external": true,
            "key_required": false,
            "provider_registered": false,
            "gpu_qualification": {"state": "not_run", "model": "blender-probe", "via": "host generation job"}
        });
    };
    if let Some(Err(error)) = provider.runtime_ready() {
        return json!({
            "state": "external_unavailable",
            "external": true,
            "key_required": false,
            "metadata_verified": false,
            "code": error.code,
            "message": error.message,
            "gpu_qualification": {"state": "not_run", "model": "blender-probe", "via": "host generation job"}
        });
    }
    match provider.test_key("") {
        Ok(()) => json!({
            "state": "external_ready",
            "external": true,
            "key_required": false,
            "metadata_verified": true,
            "version_worker_recipe_verified": true,
            "gpu_qualification": {"state": "not_run", "model": "blender-probe", "via": "host generation job"}
        }),
        Err(error) => json!({
            "state": "external_unverified",
            "external": true,
            "key_required": false,
            "metadata_verified": true,
            "version_worker_recipe_verified": false,
            "code": error.code,
            "message": error.message,
            "gpu_qualification": {"state": "not_run", "model": "blender-probe", "via": "host generation job"}
        }),
    }
}

/// Explicit install guidance without downloading or bundling user-owned tools.
pub fn install_guidance(id: &str) -> Value {
    json!({
        "id": id,
        "state": "external",
        "installable": false,
        "message": "Blender and RenderRight are user-managed external runtime components; this crate does not download or bundle them.",
        "setup": setup()
    })
}
