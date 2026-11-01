//! Fixtures for tests that touch the shared store, the app data folder or the process-wide
//! activation state. Compiled only for tests and `test-mode` builds (the shell and app-runtime
//! tests reuse it); a release build has none of it.
//!
//! Isolation is asserted, not assumed: `isolate_env` refuses to continue unless the suite root and
//! The app data folder really resolve inside the temp directory, so a build that ignores the
//! override variables fails the test instead of writing into a real user folder.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::shared::{self, store};

/// A temp directory removed on drop, for crates that have no `tempfile` dependency.
pub struct ScratchDir(PathBuf);

pub fn scratch_dir(label: &str) -> ScratchDir {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rightkit-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    ScratchDir(dir)
}

impl ScratchDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One test at a time owns the process environment and the activation state.
static ENV: Mutex<()> = Mutex::new(());

/// Redirects the suite root to `<tmp>/shared` (so `models_root()` is `<tmp>/shared/models`) and the
/// app data folder to `<tmp>/app`, and starts from an unset activation. Restores both on drop.
pub struct IsolatedEnv {
    _lock: MutexGuard<'static, ()>,
    /// The isolated app data folder.
    pub app: PathBuf,
}

pub fn isolate_env(tmp: &Path) -> IsolatedEnv {
    let lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let app = tmp.join("app");
    std::env::set_var("RIGHTKIT_SUITE_ROOT", tmp.join("shared"));
    std::env::set_var("RIGHTKIT_APP_DATA_DIR", &app);
    std::env::remove_var("RIGHTKIT_MODEL_VERIFY");
    shared::reset_activation_for_tests();
    assert_eq!(
        store::paths::models_root(),
        tmp.join("shared").join("models"),
        "suite root override not honoured: refusing to touch a real store"
    );
    assert_eq!(
        crate::app_paths::app_data_root(),
        app,
        "app data override not honoured: refusing to touch real app data"
    );
    IsolatedEnv { _lock: lock, app }
}

impl IsolatedEnv {
    /// `RIGHTKIT_MODEL_VERIFY=receipt`: a recorded stat receipt stands in for the ~350 MB hash, so a
    /// test can run a real `prepare` over a sparse fixture.
    pub fn verify_by_receipt(&self) {
        std::env::set_var("RIGHTKIT_MODEL_VERIFY", "receipt");
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        std::env::remove_var("RIGHTKIT_SUITE_ROOT");
        std::env::remove_var("RIGHTKIT_APP_DATA_DIR");
        std::env::remove_var("RIGHTKIT_MODEL_VERIFY");
        shared::reset_activation_for_tests();
    }
}

/// Where `prepare` writes the activation record.
pub fn record_file() -> PathBuf {
    crate::app_paths::app_data_file("shared-model-activation.json")
}

/// A Ready (by size, trusted manifest) Ultra entry in the isolated shared store. The files are
/// sparse zeros of the pinned sizes: enough for every size-level check, never for a hash.
pub fn write_ready_ultra() -> PathBuf {
    let spec = crate::ultra_model::spec();
    let root = store::paths::models_root();
    let dir = store::paths::model_dir_at(&root, spec.id);
    for file in spec.files {
        let path = dir.join(file.path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::File::create(&path)
            .expect("create")
            .set_len(file.size.expect("pinned size"))
            .expect("size");
    }
    store::manifest::write_manifest(&dir, &spec.expected_manifest()).expect("manifest");
    assert_eq!(
        store::status_at(&root, &spec, store::VerifyMode::Size).state,
        store::EntryState::Ready
    );
    dir
}

/// The record `prepare` writes after a native load of `dir`.
pub fn write_activation_record(dir: &Path) {
    let spec = crate::ultra_model::spec();
    write_record_text(
        &serde_json::json!({"id": spec.id, "revision": spec.version, "path": dir}).to_string(),
    );
}

pub fn write_record_text(text: &str) {
    let record = record_file();
    std::fs::create_dir_all(record.parent().expect("parent")).expect("mkdir");
    std::fs::write(record, text).expect("record");
}

/// A complete retired TDT model fileset in `dir` (the layout of `RETIRED_TDT_MODELS[0]`).
pub fn write_retired_fileset(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    #[cfg(target_os = "macos")]
    {
        std::fs::write(dir.join("pipeline.json"), b"{}").unwrap();
        std::fs::write(dir.join("vocab.txt"), b"a\n").unwrap();
        for stage in ["MelSpectrogram", "AudioEncoder", "TextDecoder", "Joint"] {
            std::fs::create_dir_all(dir.join(format!("{stage}.mlmodelc"))).unwrap();
        }
    }
    #[cfg(not(target_os = "macos"))]
    for name in [
        "config.json",
        "encoder-model.onnx",
        "decoder_joint-model.onnx",
        "mel_nemo.onnx",
        "mel_nemo.onnx.data",
        "vocab.txt",
    ] {
        std::fs::write(dir.join(name), b"test").unwrap();
    }
}
