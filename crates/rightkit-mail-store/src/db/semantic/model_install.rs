//! Installation of the EmbeddingGemma bundle through `rightkit-models`.
//!
//! RightKit owns model installation: the store layout, resumable download,
//! sha256/size verification and atomic activation all live in
//! `rightkit_models`. This module only DESCRIBES the bundle (id, file names,
//! sizes, digests) and reports availability to the UI.
//!
//! Location: `rightkit_models::suite::paths::models_root()` (the per-user
//! Right Suite models directory), never the read-only Tauri resource dir.
//!
//! Pins: the six files (name, size, sha256) are RightKit's own catalog entry,
//! `rightkit_models::suite::catalog::embedding_gemma_q4_spec()`; this module repeats
//! none of them. The build-time script `scripts/fetch-embed-model.mjs` still carries
//! the same digests (`FILE_HASHES`) until RightKit owns model-bundle provisioning
//! (bogusyogi/rightkit#993).

use super::resolve;
use rightkit_models::suite::catalog::embedding_gemma_q4_spec;
use rightkit_models::suite::paths::models_root;
use rightkit_models::{Event, FileSpec, InstallOptions, ModelSpec, RevisionSpec, Store};
use std::path::Path;

/// Model id in the RightKit store (equals `rightkit_embed::EMBEDDINGGEMMA2_MODEL_ID`).
pub const MODEL_ID: &str = "embeddinggemma2-q4";
/// Revision label of the pinned bundle (`...-b64acc4` in the R2 prefix).
pub const MODEL_REVISION: &str = "b64acc4";
/// Lock-holder name passed to the store.
/// The model store app name: the host app slug (see `MailStoreHost::app_slug`).
pub fn store_app() -> String {
    crate::host::host().app_slug().to_string()
}

// RIGHTKIT GAP (download source): the six files are pinned (name, size,
// sha256) but no public or grant-issued HTTPS URL exists for them. Today they
// live in a private R2 bucket reachable only with S3-signed requests
// (`scripts/fetch-embed-model.mjs`), and `rightkit_models` downloads plain
// HTTPS with an optional `Authorization` header. The base URL is therefore
// supplied at build time (`RIGHTKIT_EMBED_MODEL_URL`, serving `<base>/<file>`)
// and install refuses to run without it, unless the bundled resource dir
// already holds all six files (adopted and verified, no download).
pub const DOWNLOAD_BASE_URL: Option<&str> = option_env!("RIGHTKIT_EMBED_MODEL_URL");
/// Reserved-TLD stand-in so a spec can be built for adoption with no source.
const NO_SOURCE_BASE: &str = "https://model-source-not-configured.invalid";

/// One pinned file of a model revision.
pub struct PinnedFile {
    pub path: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// The six pinned files of the bundle, read from the RightKit catalog entry (store id
/// `embeddinggemma2-q4`, revision `b64acc4`). The catalog pins every file by size and sha256.
pub fn pinned_files() -> Vec<PinnedFile> {
    embedding_gemma_q4_spec()
        .files
        .iter()
        .map(|f| PinnedFile {
            path: f.path,
            bytes: f.size.expect("catalog pins the size"),
            sha256: f.sha256.expect("catalog pins the sha256"),
        })
        .collect()
}

pub fn total_bytes() -> u64 {
    pinned_files().iter().map(|f| f.bytes).sum()
}

/// Build a `rightkit_models` manifest for `files` served under `base`.
pub fn model_spec(id: &str, revision: &str, files: &[PinnedFile], base: &str) -> ModelSpec {
    let base = base.trim_end_matches('/');
    let revision = RevisionSpec {
        revision: revision.to_string(),
        quant: None,
        files: files
            .iter()
            .map(|f| FileSpec {
                path: f.path.to_string(),
                url: format!("{base}/{}", f.path),
                bytes: Some(f.bytes),
                sha256: Some(f.sha256.to_string()),
                ..FileSpec::default()
            })
            .collect(),
        ..RevisionSpec::default()
    };
    // `ModelSpec` is `#[non_exhaustive]` in rightkit-models 0.3.0: no struct literal.
    ModelSpec::new(id, vec![revision])
}

/// The shipped bundle's manifest.
pub fn bundle_spec() -> ModelSpec {
    model_spec(
        MODEL_ID,
        MODEL_REVISION,
        &pinned_files(),
        DOWNLOAD_BASE_URL.unwrap_or(NO_SOURCE_BASE),
    )
}

/// The per-user model store.
pub fn default_store() -> Store {
    Store::new(store_root(), &store_app())
}

/// The per-user models directory; a temp folder inside test binaries.
pub(crate) fn store_root() -> std::path::PathBuf {
    #[cfg(test)]
    {
        if let Some(dir) = crate::host::host().test_selector("models-sandbox") {
            return std::path::PathBuf::from(dir);
        }
    }
    models_root()
}

/// What the Settings panel needs to render the model state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Availability {
    pub installed: bool,
    pub bytes_present: u64,
    pub bytes_total: u64,
    pub runtime_available: bool,
    pub reason: String,
}

/// Progress payload published as `runtime::event_map::EVT_MODEL_INSTALL_PROGRESS`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InstallProgress {
    pub phase: &'static str,
    pub done: u64,
    pub total: u64,
    pub file: Option<String>,
}

/// Compact UI view of a RightKit install event (events with no UI meaning
/// map to `None`).
pub fn progress_of(event: &Event) -> Option<InstallProgress> {
    let p = |phase, done, total, file: Option<&String>| {
        Some(InstallProgress {
            phase,
            done,
            total,
            file: file.cloned(),
        })
    };
    match event {
        Event::Preflight { needed_bytes, .. } => p("preflight", 0, *needed_bytes, None),
        Event::FileStart { path, .. } => p("file", 0, 0, Some(path)),
        Event::Progress {
            path,
            overall_done,
            overall_total,
            ..
        } => p("download", *overall_done, *overall_total, Some(path)),
        Event::FileVerified { path, .. } => p("verified", 0, 0, Some(path)),
        Event::Activated { .. } | Event::AlreadyInstalled { .. } => {
            let t = total_bytes();
            p("done", t, t, None)
        }
        _ => None,
    }
}

/// Model + runtime availability. The reason joins the runtime and model
/// causes (runtime first: it blocks search even with a model installed).
pub fn availability(store: &Store, model_id: &str, resource_base: Option<&Path>) -> Availability {
    let model = resolve::locate_model(store, model_id, resource_base);
    let runtime = resolve::onnxruntime_dylib_path(resource_base);
    let installed = model.is_ok();
    let bytes_present = if installed {
        total_bytes()
    } else {
        // Staged bytes of a paused or interrupted install, if any.
        store
            .paused(MODEL_ID)
            .ok()
            .and_then(|p| p.iter().map(|x| x.staged_bytes).max())
            .unwrap_or(0)
            .min(total_bytes())
    };
    let reason = [runtime.as_ref().err(), model.as_ref().err()]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    Availability {
        installed,
        bytes_present,
        bytes_total: total_bytes(),
        runtime_available: runtime.is_ok(),
        reason,
    }
}

/// Install (or resume, or adopt from the bundled resource dir) the model
/// into `store`. RightKit verifies every file against the pins and activates
/// atomically; a digest or size mismatch aborts with nothing recorded.
pub fn install(
    store: &Store,
    spec: &ModelSpec,
    bundled: Option<&Path>,
    has_source: bool,
    sink: impl FnMut(Event),
) -> Result<(), String> {
    let have_all_bundled =
        bundled.is_some_and(|d| pinned_files().iter().all(|f| d.join(f.path).is_file()));
    if !has_source && !have_all_bundled {
        return Err(
            "RIGHTKIT GAP: no download source is configured for the semantic model \
             (set RIGHTKIT_EMBED_MODEL_URL at build time) and no bundled copy exists"
                .to_string(),
        );
    }
    let opts = InstallOptions {
        require_pinned: true,
        ..InstallOptions::default()
    };
    store
        .adopt_then_install(spec, &opts, |f| bundled.map(|d| d.join(&f.path)), sink)
        .map(|_| ())
        .map_err(|e| format!("semantic model install failed: {e}"))
}

#[cfg(test)]
pub(crate) fn scratch_dir(label: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rightkit-model-{label}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    // sha256("hello")
    const HELLO_SHA: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn fixture(sha256: &'static str) -> [PinnedFile; 1] {
        [PinnedFile {
            path: "a.bin",
            bytes: 5,
            sha256,
        }]
    }

    #[test]
    fn pins_are_complete_and_well_formed() {
        let files = pinned_files();
        assert_eq!(files.len(), 6);
        for f in &files {
            assert_eq!(f.sha256.len(), 64, "{}", f.path);
            assert!(
                f.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "{}",
                f.path
            );
            assert!(f.bytes > 0);
        }
        assert_eq!(total_bytes(), 187_632_172);
        let spec = bundle_spec();
        assert_eq!(spec.id, MODEL_ID);
        assert_eq!(spec.revisions[0].files.len(), 6);
    }

    #[test]
    fn digest_mismatch_is_rejected_and_nothing_is_recorded() {
        let root = scratch_dir("mismatch-store");
        let src = scratch_dir("mismatch-src");
        std::fs::write(src.join("a.bin"), b"hello").unwrap();
        let store = Store::new(&root, &store_app());
        let bad = "0".repeat(64);
        let bad: &'static str = Box::leak(bad.into_boxed_str());
        let spec = model_spec(MODEL_ID, "t1", &fixture(bad), "http://127.0.0.1:9");
        let opts = InstallOptions {
            require_pinned: true,
            ..InstallOptions::default()
        };
        let res = store.adopt(&spec, &opts, |f| Some(src.join(&f.path)));
        assert!(res.is_err(), "wrong digest must be refused");
        assert!(store.resolve(MODEL_ID).is_none());
    }

    #[test]
    fn matching_digest_is_adopted() {
        let root = scratch_dir("ok-store");
        let src = scratch_dir("ok-src");
        std::fs::write(src.join("a.bin"), b"hello").unwrap();
        let store = Store::new(&root, &store_app());
        let spec = model_spec(MODEL_ID, "t1", &fixture(HELLO_SHA), "http://127.0.0.1:9");
        let opts = InstallOptions {
            require_pinned: true,
            ..InstallOptions::default()
        };
        store
            .adopt(&spec, &opts, |f| Some(src.join(&f.path)))
            .expect("pinned file adopts");
        assert!(store.resolve(MODEL_ID).is_some());
    }

    #[test]
    fn status_on_empty_directories_is_not_installed() {
        let root = scratch_dir("empty-store");
        let res = scratch_dir("empty-res");
        let store = Store::new(&root, &store_app());
        let a = availability(&store, MODEL_ID, Some(&res));
        assert!(!a.installed);
        assert_eq!(a.bytes_present, 0);
        assert_eq!(a.bytes_total, total_bytes());
        assert!(!a.reason.is_empty());
    }

    #[test]
    fn install_without_source_or_bundle_reports_the_gap() {
        let root = scratch_dir("gap-store");
        let store = Store::new(&root, &store_app());
        let err = install(&store, &bundle_spec(), None, false, |_| {}).unwrap_err();
        assert!(err.contains("RIGHTKIT GAP"), "{err}");
    }
}
