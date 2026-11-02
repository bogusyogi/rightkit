use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::LocalImagePaths;

use rightkit_models::{Event, FileSpec, InstallOptions, ModelSpec, RevisionSpec, Store};

use super::store::ComponentRecord;

const CATALOGUE_JSON: &str = include_str!("../assets/local-image/profiles.json");
const RUNTIME_MANIFEST_JSON: &str =
    include_str!("../assets/local-image/runtime-manifest.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalogue {
    pub schema_version: String,
    pub generated_at: String,
    #[serde(default)]
    pub profiles: Vec<ProfileSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSpec {
    pub id: String,
    pub revision: i64,
    pub label: String,
    pub role: String,
    #[serde(default)]
    pub description: String,
    pub license: LicenseSpec,
    #[serde(default)]
    pub tasks: Vec<String>,
    pub dimension_multiple: u32,
    pub defaults: ProfileDefaults,
    #[serde(default)]
    pub components: Vec<ComponentSpec>,
    #[serde(default)]
    pub qualification: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseSpec {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub commercial: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileDefaults {
    pub sample_steps: u32,
    pub sample_method: String,
    pub scheduler: Option<String>,
    pub txt_cfg: f64,
    #[serde(default)]
    pub negative_prompt: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentSpec {
    pub kind: String,
    pub file: String,
    #[serde(default = "default_true")]
    pub required: bool,
    pub bytes: Option<i64>,
    pub sha256: Option<String>,
    #[serde(default)]
    pub sha256_source: String,
    pub source: ComponentSource,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentSource {
    pub repo: String,
    pub revision: String,
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeManifest {
    pub schema_version: String,
    pub upstream: UpstreamPin,
    pub access_patch: Value,
    pub api: Value,
    pub binaries: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamPin {
    pub repo: String,
    pub tag: String,
    pub commit: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentView {
    pub kind: String,
    pub file: String,
    pub bytes: Option<i64>,
    pub sha256: Option<String>,
    pub required: bool,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileView {
    pub id: String,
    pub revision: i64,
    pub label: String,
    pub role: String,
    pub description: String,
    pub license: LicenseSpec,
    pub tasks: Vec<String>,
    pub dimension_multiple: u32,
    pub defaults: ProfileDefaults,
    pub components: Vec<ComponentView>,
    pub install_state: String,
    pub qualification: Value,
    pub total_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallProgress {
    pub profile_id: String,
    pub component: String,
    pub file: String,
    pub bytes_done: i64,
    pub bytes_total: i64,
    pub state: String,
    pub error: Option<String>,
}

pub fn catalogue() -> Result<&'static Catalogue, String> {
    static CATALOGUE: OnceLock<Result<Catalogue, String>> = OnceLock::new();
    CATALOGUE
        .get_or_init(|| {
            serde_json::from_str(CATALOGUE_JSON).map_err(|e| format!("parse profiles.json: {e}"))
        })
        .as_ref()
        .map_err(|e| e.clone())
}

pub fn runtime_manifest() -> Result<&'static RuntimeManifest, String> {
    static MANIFEST: OnceLock<Result<RuntimeManifest, String>> = OnceLock::new();
    MANIFEST
        .get_or_init(|| {
            serde_json::from_str(RUNTIME_MANIFEST_JSON)
                .map_err(|e| format!("parse runtime-manifest.json: {e}"))
        })
        .as_ref()
        .map_err(|e| e.clone())
}

pub fn profile(id: &str) -> Result<Option<&'static ProfileSpec>, String> {
    Ok(catalogue()?.profiles.iter().find(|spec| spec.id == id))
}

/// Where the runtime reads a profile revision: the `rightkit-models` store's
/// active revision when intact, else the pre-store flat `<id>/r<N>` folder.
pub fn profile_dir(paths: &LocalImagePaths, profile_id: &str, revision: i64) -> PathBuf {
    crate::model_store::profile_dir(&paths.model_root, profile_id, revision)
}

/// Pre-store flat folder of a profile revision (adopted by installs).
fn legacy_profile_dir(paths: &LocalImagePaths, profile_id: &str, revision: i64) -> PathBuf {
    paths
        .model_root
        .join(profile_id)
        .join(crate::model_store::sdcpp_revision(revision))
}

pub fn download_url(component: &ComponentSpec) -> String {
    format!(
        "https://huggingface.co/{}/resolve/{}/{}",
        component.source.repo, component.source.revision, component.source.file
    )
}

fn component_state(
    component: &ComponentSpec,
    records: &[ComponentRecord],
    model_dir: &Path,
) -> String {
    let file_present = model_dir.join(&component.file).is_file();
    match records
        .iter()
        .find(|record| record.kind == component.kind && record.file == component.file)
    {
        Some(record) if record.state == "installed" && file_present => "installed".to_string(),
        Some(record) if record.state == "staged" => "staged".to_string(),
        Some(record) if record.state == "failed" => "failed".to_string(),
        _ => "missing".to_string(),
    }
}

fn aggregate_install_state(states: &[(bool, String)]) -> String {
    let required = states.iter().filter(|(required, _)| *required);
    if required.clone().all(|(_, state)| state == "installed")
        && states.iter().any(|(required, _)| *required)
    {
        "installed".to_string()
    } else if states.iter().any(|(_, state)| state == "failed") {
        "failed".to_string()
    } else if states
        .iter()
        .any(|(_, state)| state == "installed" || state == "staged")
    {
        "partial".to_string()
    } else {
        "not_installed".to_string()
    }
}

pub fn profile_view(
    spec: &ProfileSpec,
    records: &[ComponentRecord],
    paths: &LocalImagePaths,
) -> ProfileView {
    let model_dir = profile_dir(paths, &spec.id, spec.revision);
    let mut total_bytes: i64 = 0;
    let mut states = Vec::with_capacity(spec.components.len());
    let components = spec
        .components
        .iter()
        .map(|component| {
            let state = component_state(component, records, &model_dir);
            if component.required {
                if let Some(bytes) = component.bytes {
                    total_bytes += bytes;
                }
            }
            states.push((component.required, state.clone()));
            ComponentView {
                kind: component.kind.clone(),
                file: component.file.clone(),
                bytes: component.bytes,
                sha256: component.sha256.clone(),
                required: component.required,
                state,
            }
        })
        .collect();
    ProfileView {
        id: spec.id.clone(),
        revision: spec.revision,
        label: spec.label.clone(),
        role: spec.role.clone(),
        description: spec.description.clone(),
        license: spec.license.clone(),
        tasks: spec.tasks.clone(),
        dimension_multiple: spec.dimension_multiple,
        defaults: spec.defaults.clone(),
        components,
        install_state: aggregate_install_state(&states),
        qualification: spec.qualification.clone(),
        total_bytes,
    }
}

pub fn installed_component_paths(
    spec: &ProfileSpec,
    paths: &LocalImagePaths,
) -> Result<Vec<(String, PathBuf)>, String> {
    let model_dir = profile_dir(paths, &spec.id, spec.revision);
    let mut missing = Vec::new();
    let mut resolved = Vec::new();
    for component in &spec.components {
        let path = model_dir.join(&component.file);
        if path.is_file() {
            resolved.push((component.kind.clone(), path));
        } else if component.required {
            missing.push(component.kind.clone());
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "model_not_installed: profile {} missing required components: {}",
            spec.id,
            missing.join(", ")
        ));
    }
    Ok(resolved)
}

pub fn component_hashes(spec: &ProfileSpec, paths: &LocalImagePaths) -> Vec<(String, String)> {
    component_hashes_cancellable(spec, paths, || false).unwrap_or_default()
}

pub fn component_hashes_cancellable(
    spec: &ProfileSpec,
    paths: &LocalImagePaths,
    mut cancelled: impl FnMut() -> bool,
) -> Result<Vec<(String, String)>, String> {
    let model_dir = profile_dir(paths, &spec.id, spec.revision);
    spec.components
        .iter()
        .filter(|component| component.required)
        .map(|component| {
            let path = model_dir.join(&component.file);
            let hash = if path.is_file() {
                match super::store::sha256_file_cancellable(&path, &mut cancelled) {
                    Ok(hash) => hash,
                    Err(error) if error.starts_with("cancelled:") => return Err(error),
                    Err(_) => "unreadable".to_string(),
                }
            } else {
                component
                    .sha256
                    .clone()
                    .unwrap_or_else(|| "missing".to_string())
            };
            Ok((component.kind.clone(), hash))
        })
        .collect()
}

pub fn launch_args(spec: &ProfileSpec, paths: &LocalImagePaths) -> Result<Vec<String>, String> {
    let resolved = installed_component_paths(spec, paths)?;
    let mut args = Vec::new();
    for (kind, path) in &resolved {
        let flag = match kind.as_str() {
            "transformer" => "--diffusion-model",
            "encoder" => "--llm",
            "mmproj" => "--llm_vision",
            "vae" => "--vae",
            _ => continue,
        };
        args.push(flag.to_string());
        args.push(path.to_string_lossy().to_string());
    }
    if paths.lora_root.is_dir() {
        args.push("--lora-model-dir".to_string());
        args.push(paths.lora_root.to_string_lossy().to_string());
    }
    args.push("--log-level".to_string());
    args.push("info".to_string());
    Ok(args)
}

/// Install a profile's required components through `rightkit-models`:
/// manifest-pinned resumable download, disk preflight, atomic activation.
/// Files already in the pre-store flat folder are verified and adopted instead
/// of re-downloaded. `pause` stops it keeping the partial download (installing
/// again resumes it); `cancel` stops it and discards the partial.
pub fn install_profile(
    paths: &LocalImagePaths,
    spec: &ProfileSpec,
    records: &[ComponentRecord],
    mut persist: impl FnMut(&ComponentRecord) -> Result<(), String>,
    mut progress: impl FnMut(&InstallProgress),
    pause: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    let model_dir = profile_dir(paths, &spec.id, spec.revision);
    let required: Vec<&ComponentSpec> = spec.components.iter().filter(|c| c.required).collect();
    // Optional components are opt-in files. In particular, GGUF vision
    // projectors are not used by the current Qwen image pipeline.
    if required
        .iter()
        .all(|c| component_state(c, records, &model_dir) == "installed")
    {
        return Ok(());
    }

    let revision = crate::model_store::sdcpp_revision(spec.revision);
    let quant = crate::model_store::sdcpp_quant(&spec.id).to_string();
    let model = ModelSpec::new(
        spec.id.clone(),
        vec![RevisionSpec {
            revision: revision.clone(),
            quant: Some(quant.clone()),
            files: required
                .iter()
                .map(|c| FileSpec {
                    path: c.file.clone(),
                    url: download_url(c),
                    bytes: c.bytes.map(|b| b.max(0) as u64),
                    sha256: c.sha256.clone(),
                    platforms: Vec::new(),
                    extract: None,
                    ..Default::default()
                })
                .collect(),
            requirements: Default::default(),
            measured: Vec::new(),
        }],
    )
    .with_label(spec.label.clone());
    let legacy = legacy_profile_dir(paths, &spec.id, spec.revision);
    let store = crate::model_store::with_legacy_alias(
        Store::new(&paths.model_root, crate::model_store::store_app()),
        &spec.id,
        &legacy,
    );
    let opts = InstallOptions {
        revision: Some(revision),
        quant: Some(quant),
        pause,
        cancel,
        ..InstallOptions::default()
    };
    let by_file = |file: &str| required.iter().find(|c| c.file == file).copied();
    let view = |c: &ComponentSpec, done: i64, total: i64, state: &str, error: Option<String>| {
        InstallProgress {
            profile_id: spec.id.clone(),
            component: c.kind.clone(),
            file: c.file.clone(),
            bytes_done: done,
            bytes_total: total,
            state: state.to_string(),
            error,
        }
    };
    let mut current: Option<&ComponentSpec> = None;
    let mut last_report = 0u64;
    let result = store.adopt_then_install(
        &model,
        &opts,
        |f| Some(legacy.join(&f.path)),
        |event| match event {
            Event::FileStart { path, .. } => {
                current = by_file(&path);
                last_report = 0;
            }
            Event::Progress {
                path,
                file_done,
                file_total,
                ..
            } => {
                if let Some(c) = by_file(&path) {
                    if file_done.saturating_sub(last_report) >= 64 * 1024 * 1024
                        || file_done == file_total
                    {
                        last_report = file_done;
                        progress(&view(
                            c,
                            file_done as i64,
                            file_total as i64,
                            "downloading",
                            None,
                        ));
                    }
                }
            }
            Event::FileVerified { path, .. } => {
                if let Some(c) = by_file(&path) {
                    let total = c.bytes.unwrap_or(0);
                    progress(&view(c, total, total, "verifying", None));
                }
            }
            _ => {}
        },
    );
    match result {
        Ok(_) => {
            for c in &required {
                persist(&ComponentRecord {
                    profile_id: spec.id.clone(),
                    profile_revision: spec.revision,
                    kind: c.kind.clone(),
                    file: c.file.clone(),
                    sha256: c.sha256.clone(),
                    bytes: c.bytes,
                    state: "installed".to_string(),
                    updated_at: super::store::now_iso(),
                })?;
                let total = c.bytes.unwrap_or(0);
                progress(&view(c, total, total, "installed", None));
            }
            Ok(())
        }
        Err(error) => {
            let code = crate::model_store::error_code(&error);
            let message = format!("{code}: {}", error.message);
            if code != "cancelled" && code != "paused" {
                if let Some(c) = current {
                    let _ = persist(&ComponentRecord {
                        profile_id: spec.id.clone(),
                        profile_revision: spec.revision,
                        kind: c.kind.clone(),
                        file: c.file.clone(),
                        sha256: c.sha256.clone(),
                        bytes: c.bytes,
                        state: "failed".to_string(),
                        updated_at: super::store::now_iso(),
                    });
                    progress(&view(
                        c,
                        0,
                        c.bytes.unwrap_or(0),
                        "failed",
                        Some(message.clone()),
                    ));
                }
            }
            Err(message)
        }
    }
}

/// Verify and adopt already staged model files into canonical revision layout.
/// This path never downloads; callers use it when an external installer has
/// already materialized every pinned component.
pub fn adopt_profile(
    paths: &LocalImagePaths,
    spec: &ProfileSpec,
    records: &[ComponentRecord],
    mut persist: impl FnMut(&ComponentRecord) -> Result<(), String>,
) -> Result<(), String> {
    let model_dir = profile_dir(paths, &spec.id, spec.revision);
    let required: Vec<&ComponentSpec> = spec.components.iter().filter(|c| c.required).collect();
    if required
        .iter()
        .all(|c| component_state(c, records, &model_dir) == "installed")
    {
        return Ok(());
    }
    let revision = crate::model_store::sdcpp_revision(spec.revision);
    let quant = crate::model_store::sdcpp_quant(&spec.id).to_string();
    let model = ModelSpec::new(
        spec.id.clone(),
        vec![RevisionSpec {
            revision: revision.clone(),
            quant: Some(quant.clone()),
            files: required
                .iter()
                .map(|c| FileSpec {
                    path: c.file.clone(),
                    url: download_url(c),
                    bytes: c.bytes.map(|b| b.max(0) as u64),
                    sha256: c.sha256.clone(),
                    platforms: Vec::new(),
                    extract: None,
                    ..Default::default()
                })
                .collect(),
            requirements: Default::default(),
            measured: Vec::new(),
        }],
    )
    .with_label(spec.label.clone());
    let legacy = legacy_profile_dir(paths, &spec.id, spec.revision);
    let store = crate::model_store::with_legacy_alias(
        Store::new(&paths.model_root, crate::model_store::store_app()),
        &spec.id,
        &legacy,
    );
    let opts = InstallOptions {
        revision: Some(revision),
        quant: Some(quant),
        ..InstallOptions::default()
    };
    store
        .adopt(&model, &opts, |file| Some(legacy.join(&file.path)))
        .map_err(|error| {
            let code = crate::model_store::error_code(&error);
            format!("{code}: {}", error.message)
        })?;
    for c in required {
        persist(&ComponentRecord {
            profile_id: spec.id.clone(),
            profile_revision: spec.revision,
            kind: c.kind.clone(),
            file: c.file.clone(),
            sha256: c.sha256.clone(),
            bytes: c.bytes,
            state: "installed".to_string(),
            updated_at: super::store::now_iso(),
        })?;
    }
    Ok(())
}

fn rightkit_error(error: rightkit_models::Error) -> String {
    format!(
        "{}: {}",
        crate::model_store::error_code(&error),
        error.message
    )
}

/// The rightkit store that holds a profile's revision, with its pre-store flat folder aliased.
fn store_for(paths: &LocalImagePaths, spec: &ProfileSpec) -> Store {
    let legacy = legacy_profile_dir(paths, &spec.id, spec.revision);
    crate::model_store::with_legacy_alias(
        Store::new(&paths.model_root, crate::model_store::store_app()),
        &spec.id,
        &legacy,
    )
}

/// Removes an installed profile: its store entry when the store holds one, and the component
/// files in the runtime's folder (store or flat). Drops the component records. Returns the
/// distinct file names removed. Deleting model files is the explicit action of an uninstall.
pub fn uninstall_profile(
    paths: &LocalImagePaths,
    spec: &ProfileSpec,
) -> Result<Vec<String>, String> {
    let store = store_for(paths, spec);
    // No store entry for this profile means there is nothing for the store to remove.
    if let Ok(state) = store.state(&spec.id) {
        if state.active.is_some() {
            store.uninstall(&spec.id).map_err(rightkit_error)?;
        }
    }
    let dirs = [
        legacy_profile_dir(paths, &spec.id, spec.revision),
        profile_dir(paths, &spec.id, spec.revision),
    ];
    let mut removed = Vec::new();
    for component in &spec.components {
        for dir in &dirs {
            let path = dir.join(&component.file);
            if path.is_file() {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("io: remove {}: {e}", path.display()))?;
                removed.push(component.file.clone());
            }
            let mut part = path.into_os_string();
            part.push(".part");
            let _ = std::fs::remove_file(part);
        }
    }
    removed.sort();
    removed.dedup();
    let _ = std::fs::remove_dir(&dirs[0]);
    super::store::Store::open(&paths.database_path)?.delete_components_for_profile(&spec.id)?;
    Ok(removed)
}

/// Discards the partial downloads a paused or cancelled install left in the store's staging.
/// Returns whether there were any.
pub fn discard_partial(paths: &LocalImagePaths, spec: &ProfileSpec) -> Result<bool, String> {
    let store = store_for(paths, spec);
    let mut discarded = false;
    for partial in store.paused(&spec.id).map_err(rightkit_error)? {
        store
            .discard_staging(&spec.id, &partial.key)
            .map_err(rightkit_error)?;
        discarded = true;
    }
    Ok(discarded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::EnvValues;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "rightkit-local-gen-profiles-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn local_paths(root: &Path, model_root: PathBuf) -> LocalImagePaths {
        let app = root.join("local-image");
        LocalImagePaths {
            app_data_dir: app.clone(),
            database_path: app.join("local-image.sqlite3"),
            log_dir: app.join("logs"),
            staging_dir: app.join("staging"),
            reference_dir: app.join("references"),
            lora_root: model_root.join("loras"),
            output_root: root.join("out"),
            backend_binary: app.join("bin").join("sd-server"),
            model_root,
        }
    }

    fn component(file: &str, bytes: i64) -> ComponentSpec {
        ComponentSpec {
            kind: file.trim_end_matches(".gguf").to_string(),
            file: file.to_string(),
            required: true,
            bytes: Some(bytes),
            sha256: None,
            sha256_source: String::new(),
            source: ComponentSource {
                repo: "test/repo".into(),
                revision: "main".into(),
                file: file.into(),
            },
        }
    }

    /// A catalogue profile reshaped to two small test components, so tests stay tiny.
    fn test_spec() -> ProfileSpec {
        let mut spec = catalogue().expect("catalogue").profiles[0].clone();
        spec.id = "test-profile".into();
        spec.revision = 1;
        spec.components = vec![component("a.gguf", 4), component("b.gguf", 4)];
        spec
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn uninstall_removes_files_and_component_records() {
        let root = scratch("uninstall");
        let paths = local_paths(&root, root.join("models"));
        let spec = test_spec();
        let dir = paths.model_root.join("test-profile").join("r1");
        write(&dir.join("a.gguf"), b"AAAA");
        write(&dir.join("b.gguf"), b"BBBB");
        {
            let db = super::super::store::Store::open(&paths.database_path).unwrap();
            db.upsert_component(&ComponentRecord {
                profile_id: spec.id.clone(),
                profile_revision: spec.revision,
                kind: "a".into(),
                file: "a.gguf".into(),
                sha256: None,
                bytes: Some(4),
                state: "installed".into(),
                updated_at: super::super::store::now_iso(),
            })
            .unwrap();
        }

        let removed = uninstall_profile(&paths, &spec).unwrap();
        assert_eq!(removed, vec!["a.gguf".to_string(), "b.gguf".to_string()]);
        assert!(!dir.join("a.gguf").exists());
        assert!(!dir.join("b.gguf").exists());
        let db = super::super::store::Store::open(&paths.database_path).unwrap();
        assert!(db
            .components_for_profile(&spec.id, spec.revision)
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn runtime_root_comes_from_the_shared_resolver() {
        let root = scratch("resolver");
        let env = EnvValues::default();
        let resolved = crate::paths::local_image(&root, "content", None, &env, &[]);
        assert_eq!(resolved.model_root, root.join("local-image").join("models"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
