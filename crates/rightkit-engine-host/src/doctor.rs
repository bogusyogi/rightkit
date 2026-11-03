//! The engine `doctor` report: identity, contract version, capabilities (every registered command
//! at version 1 plus extra named capabilities), platform, install location and data-folder
//! readiness. The host merges its own readiness (models, runtimes, providers) under `readiness`.
//! Donor: GenRight `genright-engine/src/main_doctor.rs`.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::registry::CommandRegistry;
use crate::EngineSpec;

/// Every registered command at version 1, then `extra` (`(name, version)`), as
/// `[{name, version}]`.
pub fn capabilities(registry: &CommandRegistry, extra: &[(&str, u32)]) -> Vec<Value> {
    registry
        .specs()
        .iter()
        .map(|s| (s.name.as_str(), 1))
        .chain(extra.iter().copied())
        .map(|(name, version)| json!({ "name": name, "version": version }))
        .collect()
}

/// Where resources resolve from for an executable: macOS `<App>.app/Contents/MacOS/<exe>` uses
/// `Contents/Resources`; elsewhere resources sit beside the executable.
pub fn resource_dir(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    if cfg!(target_os = "macos") && dir.ends_with("Contents/MacOS") {
        return Some(dir.parent()?.join("Resources"));
    }
    Some(dir.to_path_buf())
}

/// Whether `dir` can be created and written (a probe file is written and removed).
pub fn writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".doctor-probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// The report. `app` is the product's display name; `data_dir` the data folder in use;
/// `readiness` the host's own readiness object (merged with `app_data_dir`).
pub fn report(
    spec: &EngineSpec,
    app: &str,
    registry: &CommandRegistry,
    extra_capabilities: &[(&str, u32)],
    data_dir: &Path,
    readiness: Value,
) -> Value {
    let exe = std::env::current_exe().ok();
    let mut readiness = match readiness {
        Value::Object(map) => Value::Object(map),
        _ => json!({}),
    };
    readiness["app_data_dir"] = json!({
        "path": data_dir.to_string_lossy(),
        "writable": writable(data_dir),
    });
    json!({
        "app": app,
        "engine": spec.name,
        "app_version": spec.version,
        "contract_version": spec.contract_version,
        "capabilities": capabilities(registry, extra_capabilities),
        "platform": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "data_folder": {
            "path": data_dir.to_string_lossy(),
            "explicit": std::env::var_os(spec.env_name("APP_DATA")).is_some_and(|v| !v.is_empty()),
            "qa_marked": crate::data_folder::qa_marked(spec),
        },
        "binary": exe.as_ref().map(|p| p.to_string_lossy().to_string()),
        "resource_dir": exe.as_deref().and_then(resource_dir).map(|p| p.to_string_lossy().to_string()),
        "commands": registry.names(),
        "readiness": readiness,
    })
}
