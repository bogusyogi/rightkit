//! Glue over `rightkit-models` (the shared local-model store):
//! where runtimes find an installed model (store revision first, then the
//! pre-store flat layout so already-installed models are recognised without a
//! re-download) and the legacy-layout alias the store keeps linked.

use std::path::{Path, PathBuf};

use rightkit_models::Store;

/// Store app name recorded in model-directory locks (the host's [`crate::host::HostConfig::store_app`]).
pub fn store_app() -> &'static str {
    crate::host::config().store_app.as_str()
}

/// Quantisation label of a bundled sd.cpp profile id (catalog wire value).
pub fn sdcpp_quant(profile_id: &str) -> &'static str {
    if profile_id.contains("int8") {
        "int8"
    } else if profile_id.contains("q6") {
        "Q6_K"
    } else {
        "gguf"
    }
}

/// Revision label of a local-image profile revision (`r<N>`, the pre-store
/// directory name).
pub fn sdcpp_revision(revision: i64) -> String {
    format!("r{revision}")
}

/// Wire code of a store error (`disk_space`, `hash_mismatch`, `cancelled`, ...).
pub fn error_code(e: &rightkit_models::Error) -> String {
    serde_json::to_value(e.code)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "io".into())
}

/// Runtime directory for a local-image profile revision: the store's active
/// revision when it is that revision and intact, else the pre-store flat
/// directory `<model_root>/<id>/r<N>`.
pub fn profile_dir(model_root: &Path, profile_id: &str, revision: i64) -> PathBuf {
    let store = Store::new(model_root, store_app());
    let rev = sdcpp_revision(revision);
    let key = rightkit_models::encode_revision_key(&rev, Some(sdcpp_quant(profile_id)))
        .map(|k| k.as_str().to_string())
        .ok();
    store
        .resolve(profile_id)
        .filter(|r| Some(&r.key) == key.as_ref())
        .map(|r| r.dir)
        .unwrap_or_else(|| model_root.join(profile_id).join(rev))
}

/// Keep a model's pre-store flat layout linked to the store's entry: `legacy` is
/// where runtimes already read it (the QTS flat folder, whose parent is allowed
/// as an alias root). For SD.cpp `<root>/<id>/rN`, inside the store root, no link
/// is written since rightkit-models 0.7.0: the entry `<root>/<id>` is already flat
/// and [`profile_dir`] resolves it. A real directory there (an existing flat
/// install) is left alone and keeps loading.
pub fn with_legacy_alias(store: Store, id: &str, legacy: &Path) -> Store {
    let mut paths = std::collections::BTreeMap::new();
    paths.insert(id.to_string(), legacy.to_path_buf());
    let store = store.with_legacy_alias_paths(paths);
    match legacy.parent() {
        Some(parent) => store.with_legacy_alias_allowed_root(parent),
        None => store,
    }
}
