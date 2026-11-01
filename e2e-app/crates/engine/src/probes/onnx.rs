//! rightkit-onnx: bind the installed ONNX Runtime the way a shipped app does (bundled library resolved beside the
//! executable, checked against the native-library catalog row for this host, then bound), initialise the process
//! environment, probe the execution providers, and run a hand-encoded ONNX model (z = x + y) on CPU and on the
//! platform accelerator, asserting the numbers.
//!
//! Needs: the ONNX Runtime library staged as the approved/pinned catalog row for this host
//! (`onnxruntime-1.30.0-macos-universal2` on macOS, the DirectML 1.24.4 row on Windows) at, relative to the engine
//! executable, `<resources>/runtime/<libonnxruntime.dylib | onnxruntime.dll>` where `<resources>` is
//! `Contents/Resources` (executable in `Contents/MacOS`) on macOS and the executable's directory on Windows
//! (`rightkit_onnx::installed_runtime_candidate(exe, "runtime")`). Windows also needs `DirectML.dll` beside it to
//! use the accelerator. An inherited `ORT_DYLIB_PATH` that names another library is refused by the crate.
//! Catalog verification goes through `rightkit_ffmpeg::native::verified_library` (the `catalog` feature of
//! rightkit-onnx is not enabled by the engine, so `bind_installed_runtime` with `Catalog` is not used).
use std::path::PathBuf;
use std::sync::OnceLock;

use rightkit_ffmpeg::native::{verified_library, NativeLibrary};
use rightkit_onnx::ort::value::Tensor;
use rightkit_onnx::{
    configure_runtime, init_environment_with_options, installed_runtime_candidate, probe_providers, runtime_filename,
    EnvironmentInitOptions, ExecutionProvider, ProviderKind, SessionError, SessionOptions,
};
use serde_json::{json, Value};

/// What the shared binding established.
#[derive(Clone, Debug)]
pub struct Runtime {
    pub path: PathBuf,
    pub catalog_row: String,
    pub version: String,
    pub info: String,
}

/// Bind and initialise the runtime once per process (shared by the asr, vad, embed and ocr probes).
pub fn ensure_runtime() -> Result<Runtime, String> {
    static BOUND: OnceLock<Result<Runtime, String>> = OnceLock::new();
    BOUND.get_or_init(bind).clone()
}

fn bind() -> Result<Runtime, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let candidate = installed_runtime_candidate(&exe, "runtime")
        .ok_or_else(|| format!("no installed-runtime layout for {}", exe.display()))?;
    if !candidate.path.is_file() {
        return Err(format!(
            "needs the ONNX Runtime library at {} (the catalog row for this host staged by right-release stage-sidecar \
             as a native-library/onnxruntime entry into <resources>/runtime)",
            candidate.path.display()
        ));
    }
    let verified = verified_library(NativeLibrary::Onnxruntime, &candidate.path)
        .map_err(|e| format!("{} is not the catalog's {}: {e}", candidate.path.display(), runtime_filename()))?;
    let selection = configure_runtime([candidate]).map_err(|e| format!("bind runtime: {e}"))?;
    let report = init_environment_with_options(
        &selection,
        &EnvironmentInitOptions { tolerate_already_initialized: true, ..EnvironmentInitOptions::default() },
    )
    .map_err(|e| format!("init environment: {e}"))?;
    Ok(Runtime {
        path: selection.path,
        catalog_row: verified.id().to_string(),
        version: verified.version().to_string(),
        info: report.runtime_info,
    })
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

/// A real ONNX ModelProto, encoded by hand: z = Add(x, y) over float[4], opset 13.
fn add_model() -> Vec<u8> {
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

fn run_add(session: &mut rightkit_onnx::ort::session::Session, a: [f32; 4], b: [f32; 4]) -> Result<Vec<f32>, String> {
    let x = Tensor::from_array(([4usize], a.to_vec())).map_err(|e| format!("tensor x: {e}"))?;
    let y = Tensor::from_array(([4usize], b.to_vec())).map_err(|e| format!("tensor y: {e}"))?;
    let outputs = session.run(rightkit_onnx::ort::inputs!["x" => x, "y" => y]).map_err(|e| format!("run: {e}"))?;
    let (shape, data) = outputs["z"].try_extract_tensor::<f32>().map_err(|e| format!("extract z: {e}"))?;
    if &**shape != [4i64] {
        return Err(format!("output shape {shape:?}, expected [4]"));
    }
    Ok(data.to_vec())
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let runtime = ensure_runtime()?;
    if !runtime.info.starts_with("ORT Build Info:") && !runtime.info.is_empty() {
        return Err(format!("unexpected runtime info {:?}", runtime.info));
    }

    // Providers: CPU is always there; the platform accelerator is reported as found.
    let providers = probe_providers();
    let cpu = providers.iter().find(|p| p.provider == ProviderKind::Cpu).ok_or("probe_providers lists no CPU")?;
    if !cpu.available {
        return Err("the CPU provider is unavailable".into());
    }

    // CPU: exact arithmetic, from memory and from a file.
    let want = vec![11.0, 22.0, 33.0, 44.0];
    let mut cpu_session =
        SessionOptions::new(ExecutionProvider::Cpu).build_from_memory(&add_model()).map_err(|e| format!("cpu session: {e}"))?;
    let got = run_add(&mut cpu_session, [1.0, 2.0, 3.0, 4.0], [10.0, 20.0, 30.0, 40.0])?;
    if got != want {
        return Err(format!("cpu Add gave {got:?}, expected {want:?}"));
    }
    let dir = ctx.dir("onnx")?;
    let model_path = dir.join("add.onnx");
    std::fs::write(&model_path, add_model()).map_err(|e| format!("write model: {e}"))?;
    let mut from_file = SessionOptions::default().build_from_file(&model_path).map_err(|e| format!("file session: {e}"))?;
    let got = run_add(&mut from_file, [0.5; 4], [0.25; 4])?;
    if got != vec![0.75f32; 4] {
        return Err(format!("file-backed Add gave {got:?}"));
    }
    std::fs::remove_file(&model_path).map_err(|e| format!("remove model: {e}"))?;
    if !matches!(SessionOptions::default().build_from_file(&model_path), Err(SessionError::MissingArtifact(_))) {
        return Err("a missing model file must be MissingArtifact".into());
    }

    // The platform accelerator, with the fallback reported rather than hidden.
    let accelerator = ExecutionProvider::platform_accelerator();
    let mut built = SessionOptions::new(accelerator)
        .with_cpu_fallback(true)
        .build_from_memory_reported(&add_model())
        .map_err(|e| format!("accelerator session: {e}"))?;
    let got = run_add(&mut built.session, [1.0, 2.0, 3.0, 4.0], [10.0, 20.0, 30.0, 40.0])?;
    if got != want {
        return Err(format!("{:?} Add gave {got:?}, expected {want:?}", built.provider));
    }
    // An accelerator this OS cannot host is refused, not faked.
    let foreign = if cfg!(target_os = "windows") { ExecutionProvider::CoreMl } else { ExecutionProvider::DirectMl };
    if !matches!(SessionOptions::new(foreign).build_from_memory(&add_model()), Err(SessionError::ProviderUnsupported(_))) {
        return Err(format!("{foreign:?} must be refused on this OS"));
    }

    Ok(json!({
        "runtime": runtime.path.display().to_string(),
        "catalog_row": runtime.catalog_row,
        "version": runtime.version,
        "providers": providers.iter().map(|p| json!({ "provider": format!("{:?}", p.provider), "available": p.available })).collect::<Vec<_>>(),
        "accelerator_requested": format!("{accelerator:?}"),
        "accelerator_used": format!("{:?}", built.provider),
        "accelerator_fallback": built.fallback,
    }))
}
