//! Shared by the macOS real-runtime integration tests (each binary has its own process-global
//! ONNX Runtime environment).
#![allow(dead_code)]

#[allow(unused_imports)]
use std::path::{Path, PathBuf};

/// Reports an asset that this host does not have, then returns so the caller returns from the
/// test. Prints one `SKIP <test>: <what> (looked at <where>)` line to stderr. With
/// `RIGHTKIT_REQUIRE_ASSETS=1` it panics with that same text instead, so a host that is meant to
/// have the asset cannot pass by skipping. The test name is the name of the test thread.
pub fn skip_missing(what: &str, looked_at: &str) {
    let test = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned();
    let message = format!("SKIP {test}: {what} (looked at {looked_at})");
    if std::env::var("RIGHTKIT_REQUIRE_ASSETS").is_ok_and(|value| value == "1") {
        panic!("{message}");
    }
    eprintln!("{message}");
}

/// `rel` found by walking up from this crate: `<ancestor>/<rel>`, then `<ancestor>/claude/<rel>`.
/// A fixed `../..` depth breaks when the checkout moves, so workspace assets are searched for.
pub fn workspace_asset(rel: &str) -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .flat_map(|dir| [dir.join(rel), dir.join("claude").join(rel)])
        .find(|path| path.exists())
}

/// The approved 1.24.4 dylib in the genright download cache, found by walking up from this crate.
#[cfg(target_os = "macos")]
const MACOS_RUNTIME: &str = "genright/.cache/devin-completion-20261006/batch5/voice/dl/onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib";

/// The approved 1.24.4 file: `RIGHTKIT_ORT_TEST_DYLIB` when it names a file, else the cache.
#[cfg(target_os = "macos")]
fn find_real_runtime() -> Option<PathBuf> {
    std::env::var_os("RIGHTKIT_ORT_TEST_DYLIB")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| workspace_asset(MACOS_RUNTIME).filter(|p| p.is_file()))
}

/// The approved 1.24.4 file. Tests call [`runtime_or_skip`] first.
#[cfg(target_os = "macos")]
pub fn real_runtime_file() -> PathBuf {
    find_real_runtime()
        .expect("no real ONNX Runtime 1.24.4 macOS dylib found; set RIGHTKIT_ORT_TEST_DYLIB")
}

/// Asset gate for the macOS real-runtime tests: `true` when the approved dylib is present.
/// Otherwise it reports a SKIP (or panics under `RIGHTKIT_REQUIRE_ASSETS=1`) and returns `false`.
#[cfg(target_os = "macos")]
pub fn runtime_or_skip() -> bool {
    if find_real_runtime().is_some() {
        return true;
    }
    skip_missing(
        "approved ONNX Runtime 1.24.4 macOS dylib",
        &format!("RIGHTKIT_ORT_TEST_DYLIB or {MACOS_RUNTIME} found by walking up from crates/rightkit-ort"),
    );
    false
}

/// A fresh temp directory holding a copy of the real runtime under `name`.
#[cfg(target_os = "macos")]
pub fn staged_runtime(tag: &str, name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("rightkit-ort-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(name);
    std::fs::copy(real_runtime_file(), &file).unwrap();
    (dir, file)
}

fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}
fn field_len(num: u64, payload: &[u8], out: &mut Vec<u8>) {
    varint(num << 3 | 2, out);
    varint(payload.len() as u64, out);
    out.extend_from_slice(payload);
}
fn field_varint(num: u64, v: u64, out: &mut Vec<u8>) {
    varint(num << 3, out);
    varint(v, out);
}
fn value_info(name: &str, dim: u64) -> Vec<u8> {
    let mut dim_msg = Vec::new();
    field_varint(1, dim, &mut dim_msg);
    let mut shape = Vec::new();
    field_len(1, &dim_msg, &mut shape);
    let mut tensor = Vec::new();
    field_varint(1, 1, &mut tensor); // FLOAT
    field_len(2, &shape, &mut tensor);
    let mut ty = Vec::new();
    field_len(1, &tensor, &mut ty);
    let mut vi = Vec::new();
    field_len(1, name.as_bytes(), &mut vi);
    field_len(2, &ty, &mut vi);
    vi
}
/// Hand-encoded ONNX ModelProto: z = Add(x, y), float[4].
pub fn add_model() -> Vec<u8> {
    let mut node = Vec::new();
    field_len(1, b"x", &mut node);
    field_len(1, b"y", &mut node);
    field_len(2, b"z", &mut node);
    field_len(4, b"Add", &mut node);
    let mut graph = Vec::new();
    field_len(1, &node, &mut graph);
    field_len(2, b"add", &mut graph);
    field_len(11, &value_info("x", 4), &mut graph);
    field_len(11, &value_info("y", 4), &mut graph);
    field_len(12, &value_info("z", 4), &mut graph);
    let mut opset = Vec::new();
    field_varint(2, 13, &mut opset);
    let mut model = Vec::new();
    field_varint(1, 8, &mut model);
    field_len(7, &graph, &mut model);
    field_len(8, &opset, &mut model);
    model
}

/// Versioned file name used by the Windows explicit-file tests.
#[cfg(windows)]
pub const WINDOWS_VERSIONED_NAME: &str = "onnxruntime-1.24.4.dll";

/// The HeardRight staging folder, relative to the workspace root (found by walking up).
#[cfg(windows)]
const WINDOWS_RUNTIME_DIR: &str = "heardright/tauri-app-next/src-tauri/resources/runtime";

/// The runtime directory holding `onnxruntime.dll` and `DirectML.dll`: `RIGHTKIT_ORT_TEST_DLL_DIR`
/// first, else the HeardRight staging folder (the broker does not forward the variable).
#[cfg(windows)]
fn find_real_runtime_dir() -> Option<PathBuf> {
    std::env::var_os("RIGHTKIT_ORT_TEST_DLL_DIR")
        .map(PathBuf::from)
        .into_iter()
        .chain(workspace_asset(WINDOWS_RUNTIME_DIR))
        .find(|dir| dir.join("onnxruntime.dll").is_file() && dir.join("DirectML.dll").is_file())
}

/// The approved runtime directory. Tests call [`runtime_dir_or_skip`] first.
#[cfg(windows)]
pub fn real_runtime_dir() -> PathBuf {
    find_real_runtime_dir()
        .expect("no approved onnxruntime.dll + DirectML.dll; set RIGHTKIT_ORT_TEST_DLL_DIR")
}

/// Asset gate for the Windows real-runtime tests; see [`runtime_or_skip`].
#[cfg(windows)]
pub fn runtime_dir_or_skip() -> bool {
    if find_real_runtime_dir().is_some() {
        return true;
    }
    skip_missing(
        "approved onnxruntime.dll and DirectML.dll (ONNX Runtime 1.24.4)",
        &format!("RIGHTKIT_ORT_TEST_DLL_DIR or {WINDOWS_RUNTIME_DIR} found by walking up from crates/rightkit-ort"),
    );
    false
}

/// A fresh temp directory with the real `onnxruntime.dll` copied under `name`, next to its
/// `DirectML.dll`, as `e2e_runtime_windows.rs` stages them. Returns (directory, runtime file).
#[cfg(windows)]
pub fn staged_runtime(tag: &str, name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("rightkit-ort-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = real_runtime_dir();
    let file = dir.join(name);
    std::fs::copy(source.join("onnxruntime.dll"), &file).unwrap();
    std::fs::copy(source.join("DirectML.dll"), dir.join("DirectML.dll")).unwrap();
    (dir, file)
}
