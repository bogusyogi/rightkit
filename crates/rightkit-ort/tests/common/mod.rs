//! Shared by the macOS real-runtime integration tests (each binary has its own process-global
//! ONNX Runtime environment).
#![allow(dead_code)]

#[allow(unused_imports)]
use std::path::{Path, PathBuf};

/// The approved 1.24.4 file; `RIGHTKIT_ORT_TEST_DYLIB` overrides the search.
#[cfg(target_os = "macos")]
pub fn real_runtime_file() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    let mut candidates: Vec<PathBuf> = std::env::var_os("RIGHTKIT_ORT_TEST_DYLIB")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    candidates.push(root.join(
        "genright/.cache/devin-completion-20261006/batch5/voice/dl/onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib",
    ));
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .expect("no real ONNX Runtime 1.24.4 macOS dylib found; set RIGHTKIT_ORT_TEST_DYLIB")
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

/// The approved runtime directory holding `onnxruntime.dll` and `DirectML.dll`, found the same
/// way as `e2e_runtime_windows.rs`: `RIGHTKIT_ORT_TEST_DLL_DIR`, else the HeardRight staging
/// path relative to the workspace root (the broker does not forward the variable).
#[cfg(windows)]
pub fn real_runtime_dir() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    std::env::var_os("RIGHTKIT_ORT_TEST_DLL_DIR")
        .map(PathBuf::from)
        .into_iter()
        .chain([root.join("heardright/tauri-app-next/src-tauri/resources/runtime")])
        .find(|dir| dir.join("onnxruntime.dll").is_file() && dir.join("DirectML.dll").is_file())
        .expect("no approved onnxruntime.dll + DirectML.dll; set RIGHTKIT_ORT_TEST_DLL_DIR")
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
