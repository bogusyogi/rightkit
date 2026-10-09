//! End-to-end on Windows: stage the approved ONNX Runtime 1.24.4 `onnxruntime.dll` (and its
//! `DirectML.dll`) in an installed layout, initialise the environment, probe providers, and run
//! a tiny ONNX model on DirectML and CPU. Content is validated, not just "no error".
#![cfg(all(target_os = "windows", feature = "session"))]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rightkit_ort::ort::value::Tensor;
use rightkit_ort::{
    configure_runtime, init_environment, installed_runtime_candidate, probe_providers,
    resolve_runtime, runtime_filename, CandidateSource, EnvironmentOptions, ExecutionProvider,
    GlobalPool, ProviderKind, RuntimeCandidate, RuntimeError, SessionOptions,
};

/// SHA-256 of the approved Windows x64 1.24.4 runtime (characterization/ocr/contract.json).
const APPROVED_WIN32_SHA256: &str =
    "0a49bb13573807ad309aa9967a46471c07d6495bab5b279d1fad474aefd8ef4b";

/// The approved runtime directory, holding `onnxruntime.dll` and `DirectML.dll`.
fn real_runtime_dir() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    std::env::var_os("RIGHTKIT_ORT_TEST_DLL_DIR")
        .map(PathBuf::from)
        .into_iter()
        .chain([root.join("heardright/tauri-app-next/src-tauri/resources/runtime")])
        .find(|dir| dir.join("onnxruntime.dll").is_file() && dir.join("DirectML.dll").is_file())
        .expect("no approved onnxruntime.dll + DirectML.dll; set RIGHTKIT_ORT_TEST_DLL_DIR")
}

fn sha256_hex(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()))
}

/// `<app>\example.exe` with `<app>\runtime\{onnxruntime,DirectML}.dll`, as an installer lays it out.
fn staged_app() -> &'static (PathBuf, PathBuf) {
    static STAGED: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    STAGED.get_or_init(|| {
        let app = std::env::temp_dir().join(format!("rightkit-ort-e2e-win-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&app);
        let runtime_dir = app.join("runtime");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        let source = real_runtime_dir();
        let staged = runtime_dir.join(runtime_filename());
        std::fs::copy(source.join("onnxruntime.dll"), &staged).unwrap();
        std::fs::copy(
            source.join("DirectML.dll"),
            runtime_dir.join("DirectML.dll"),
        )
        .unwrap();
        (app.clone(), staged)
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
/// Hand-encoded ONNX ModelProto: z = Add(x, y), float[4].
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

fn run_add(options: &SessionOptions) -> Vec<f32> {
    let mut session = options.build_from_memory(&add_model()).expect("session");
    let x = Tensor::from_array(([4usize], vec![1.0f32, 2.0, 3.0, 4.0])).unwrap();
    let y = Tensor::from_array(([4usize], vec![10.0f32, 20.0, 30.0, 40.0])).unwrap();
    let outputs = session
        .run(rightkit_ort::ort::inputs!["x" => x, "y" => y])
        .unwrap();
    let (shape, data) = outputs["z"].try_extract_tensor::<f32>().unwrap();
    assert_eq!(&**shape, &[4i64]);
    data.to_vec()
}

fn setup() -> rightkit_ort::EnvironmentReport {
    static REPORT: OnceLock<rightkit_ort::EnvironmentReport> = OnceLock::new();
    REPORT
        .get_or_init(|| {
            let (app, _) = staged_app();
            let candidate =
                installed_runtime_candidate(&app.join("example.exe"), "runtime").unwrap();
            let selection = configure_runtime([candidate]).unwrap();
            assert_eq!(selection.source, CandidateSource::Bundled);
            init_environment(
                &selection,
                &EnvironmentOptions {
                    global_pool: Some(GlobalPool::from_budget()),
                },
            )
            .expect("environment")
        })
        .clone()
}

#[test]
fn approved_runtime_loads_from_the_installed_layout() {
    let (_, staged) = staged_app();
    assert_eq!(sha256_hex(staged), APPROVED_WIN32_SHA256);
    let report = setup();
    assert_eq!(report.runtime_path, staged.canonicalize().unwrap());
    assert!(
        report.runtime_info.starts_with("ORT Build Info:"),
        "{}",
        report.runtime_info
    );
}

#[test]
fn directml_is_probed_available_and_coreml_is_not() {
    setup();
    let probes = probe_providers();
    let get = |k| probes.iter().find(|p| p.provider == k).unwrap();
    assert!(get(ProviderKind::Cpu).available);
    assert!(
        get(ProviderKind::DirectMl).available,
        "{:?}",
        get(ProviderKind::DirectMl)
    );
    assert!(!get(ProviderKind::CoreMl).available);
}

#[test]
fn tiny_model_runs_on_directml_and_cpu() {
    setup();
    assert_eq!(
        ExecutionProvider::platform_accelerator(),
        ExecutionProvider::DirectMl
    );
    let reported = SessionOptions::new(ExecutionProvider::DirectMl)
        .with_cpu_fallback(true)
        .build_from_memory_reported(&add_model())
        .expect("directml session");
    assert_eq!(
        reported.provider,
        ExecutionProvider::DirectMl,
        "{:?}",
        reported.fallback
    );
    assert!(reported.fallback.is_none());
    assert_eq!(
        run_add(&SessionOptions::new(ExecutionProvider::DirectMl)),
        vec![11.0, 22.0, 33.0, 44.0]
    );
    assert_eq!(
        run_add(&SessionOptions::new(ExecutionProvider::Cpu)),
        vec![11.0, 22.0, 33.0, 44.0]
    );
}

#[test]
fn system32_runtime_copy_is_rejected_before_any_load() {
    // The real System32 copy, when Windows ships one, and a planted file under a System32 path.
    let system = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap()
        .join("System32");
    let real = system.join(runtime_filename());
    if real.is_file() {
        assert!(matches!(
            resolve_runtime([RuntimeCandidate::new(&real, CandidateSource::Explicit)]),
            Err(RuntimeError::SystemRuntimeRejected { .. })
        ));
    }
    let root = std::env::temp_dir().join(format!("rightkit-ort-sys-win-{}", std::process::id()));
    let planted = root.join("Windows").join("System32");
    std::fs::create_dir_all(&planted).unwrap();
    let dll = planted.join(runtime_filename());
    std::fs::write(&dll, b"x").unwrap();
    assert!(matches!(
        resolve_runtime([RuntimeCandidate::new(&dll, CandidateSource::Explicit)]),
        Err(RuntimeError::SystemRuntimeRejected { .. })
    ));
    std::fs::remove_dir_all(root).unwrap();
}
