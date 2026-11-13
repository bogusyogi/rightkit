//! Doctor and install guidance for a user-installed Wan2GP checkout (issue #79).
//! Wan2GP is user-managed external infrastructure: this crate neither downloads nor bundles it,
//! and this module runs only the discovery probes in `rightkit_media_gen::providers::local_wan2gp`.
//! Generation runs through the `local-wan2gp` provider (wan2gp-t2v, wan2gp-i2v), free and local.

use rightkit_media_gen::providers::local_wan2gp::{self, dir_env, python_env, Wan2GpInstall};
use rightkit_media_gen::providers::{tools_root, ProviderError};
use serde_json::{json, Value};

fn setup() -> Value {
    json!({
        "mode": "user_managed_external_runtime",
        "download_available": false,
        "wan2gp_dir_env": dir_env(),
        "python_env": python_env(),
        "entry": local_wan2gp::ENTRY,
        "models": ["wan2gp-t2v", "wan2gp-i2v"],
        "cost": "free_local",
        "documented_python": ["3.10", "3.11"],
        "install_recipe": "git clone https://github.com/deepbeepmeep/Wan2GP.git, create a Python 3.11 (RTX 20XX to 50XX) or 3.10 (GTX 10XX) environment, install torch and requirements.txt, then point RIGHTKIT_WAN2GP_DIR at the clone.",
    })
}

fn discovered() -> Option<Result<Wan2GpInstall, ProviderError>> {
    local_wan2gp::discover_from_env(tools_root())
}

fn gpu_note() -> Value {
    json!({"state": "not_run", "via": "wan2gp-t2v or wan2gp-i2v generation job"})
}

/// Doctor report: `external_ready` when the checkout and interpreter pass, `external_missing`
/// when no checkout is found, otherwise `external_unavailable` with the probe's own message.
pub fn doctor_readiness() -> Value {
    match discovered() {
        None => json!({
            "state": "external_missing",
            "external": true,
            "key_required": false,
            "setup": setup(),
            "gpu_qualification": gpu_note(),
        }),
        Some(Err(error)) => json!({
            "state": "external_unavailable",
            "external": true,
            "key_required": false,
            "code": error.code,
            "message": error.message,
            "setup": setup(),
            "gpu_qualification": gpu_note(),
        }),
        Some(Ok(install)) => json!({
            "state": "external_ready",
            "external": true,
            "key_required": false,
            "metadata_verified": true,
            "root": install.root,
            "python": install.python,
            "python_version": install.python_version,
            "revision": install.revision,
            "version_verification": "git_revision",
            "setup": setup(),
            "gpu_qualification": gpu_note(),
        }),
    }
}

/// Explicit install guidance without downloading or bundling user-owned tools.
pub fn install_guidance(id: &str) -> Value {
    json!({
        "id": id,
        "state": "external",
        "installable": false,
        "message": "Wan2GP is a user-managed external Python checkout; this crate does not download or bundle it.",
        "setup": setup(),
    })
}
