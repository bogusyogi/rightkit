//! A runtime named by `ORT_DYLIB_PATH` must still get its directory on `PATH`: on Windows the
//! dependent DLLs beside `onnxruntime.dll` load only from a searched directory. Own binary: the
//! process environment is global.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

/// The `PATH` entries. An unset or empty `PATH` has none: `split_paths("")` on Unix yields one
/// empty entry, which would read as a real one.
fn path_entries() -> Vec<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|entry| !entry.as_os_str().is_empty())
        .collect()
}

fn fixture(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("rightkit-ort-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let runtime = dir.join(rightkit_onnx::runtime_filename());
    std::fs::write(&runtime, b"x").unwrap();
    (dir, runtime)
}

#[test]
fn explicit_env_runtime_still_puts_its_directory_on_path() {
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (dir, runtime) = fixture("explicit-env");
    let before: Option<OsString> = std::env::var_os("PATH");
    std::env::set_var("ORT_DYLIB_PATH", &runtime);
    let got = rightkit_onnx::export_discovered_runtime([]);
    assert_eq!(got.as_deref(), Some(runtime.as_path()));
    assert_eq!(path_entries().first(), Some(&dir), "runtime dir leads PATH");
    // Idempotent: a second export does not stack a duplicate entry.
    rightkit_onnx::export_discovered_runtime([]);
    assert_eq!(path_entries().iter().filter(|p| **p == dir).count(), 1);
    // Existing entries survive.
    if let Some(before) = before {
        for entry in std::env::split_paths(&before) {
            assert!(path_entries().contains(&entry), "{entry:?} kept");
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn prepend_helper_creates_path_when_unset_and_ignores_a_bare_file_name() {
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (dir, runtime) = fixture("unset-path");
    std::env::remove_var("PATH");
    rightkit_onnx::prepend_runtime_dir_to_path(&runtime);
    assert_eq!(path_entries(), vec![dir.clone()]);
    // A path with no parent directory names nothing to search.
    std::env::remove_var("PATH");
    rightkit_onnx::prepend_runtime_dir_to_path(std::path::Path::new(
        rightkit_onnx::runtime_filename(),
    ));
    assert!(path_entries().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}
