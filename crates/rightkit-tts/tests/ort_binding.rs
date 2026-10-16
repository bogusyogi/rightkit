//! The worker's runtime binding. Own binary: it changes `ORT_DYLIB_PATH` and `PATH`.
#![cfg(feature = "client")]

use rightkit_tts::models::bind_ort_runtime;
use std::path::PathBuf;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn entries() -> Vec<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect()
}

fn runtime(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("rk-tts-ort-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(rightkit_tts::models::runtime_lib_name());
    std::fs::write(&file, b"x").unwrap();
    (dir, file)
}

#[test]
fn an_explicit_runtime_binds_and_its_directory_leads_path_in_every_case() {
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (dir, file) = runtime("explicit");
    let (other_dir, other_file) = runtime("inherited");
    // Whatever the environment already named, the explicit runtime wins and is searched first.
    std::env::set_var("ORT_DYLIB_PATH", &other_file);
    bind_ort_runtime(Some(&file));
    assert_eq!(
        std::env::var_os("ORT_DYLIB_PATH"),
        Some(file.clone().into_os_string())
    );
    assert_eq!(entries().first(), Some(&dir));
    // The exported-runtime search that follows in the worker keeps that order.
    let got = rightkit_onnx::export_discovered_runtime([other_dir.clone()]);
    assert_eq!(got, Some(file));
    assert_eq!(entries().first(), Some(&dir));
    assert_eq!(entries().iter().filter(|p| **p == dir).count(), 1);
    std::env::remove_var("ORT_DYLIB_PATH");
    for d in [dir, other_dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn without_an_explicit_runtime_only_dev_overrides_keeps_the_inherited_variable() {
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (dir, file) = runtime("dev-override");
    std::env::set_var("ORT_DYLIB_PATH", &file);
    bind_ort_runtime(None);
    if cfg!(feature = "dev-overrides") {
        assert_eq!(
            std::env::var_os("ORT_DYLIB_PATH"),
            Some(file.into_os_string())
        );
    } else {
        assert_eq!(std::env::var_os("ORT_DYLIB_PATH"), None);
    }
    std::env::remove_var("ORT_DYLIB_PATH");
    let _ = std::fs::remove_dir_all(dir);
}
