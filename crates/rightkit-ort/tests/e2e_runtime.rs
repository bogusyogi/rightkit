//! End-to-end: load a real ONNX Runtime dylib on macOS, verify it against the
//! suite catalog, initialise the environment, probe providers, and run a tiny
//! ONNX model on CPU and CoreML. Content is validated, not just "no error".
#![cfg(all(target_os = "macos", feature = "session"))]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rightkit_ort::ort::value::Tensor;
use rightkit_media::native::{NativeCatalog, NativeLibrary};
use rightkit_ort::{
    configure_runtime, init_environment, installed_runtime_candidate, probe_providers,
    resolve_runtime, runtime_filename, CandidateSource, EnvironmentOptions, ExecutionProvider,
    GlobalPool, ProviderKind, RuntimeCandidate, RuntimeError, SessionError, SessionOptions,
};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}

/// The approved 1.24.4 file, searched in known real locations.
fn real_runtime_file() -> PathBuf {
    let root = workspace_root();
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

/// Stage the real file as an installed bundle would: <App>/Contents/Resources/runtime/libonnxruntime.dylib
fn staged_app() -> &'static (PathBuf, PathBuf) {
    static STAGED: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    STAGED.get_or_init(|| {
        let app = std::env::temp_dir().join(format!("rightkit-ort-e2e-{}.app", std::process::id()));
        let _ = std::fs::remove_dir_all(&app);
        let runtime_dir = app.join("Contents/Resources/runtime");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        let staged = runtime_dir.join(runtime_filename());
        std::fs::copy(real_runtime_file(), &staged).unwrap();
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
    let outputs = session.run(rightkit_ort::ort::inputs!["x" => x, "y" => y]).unwrap();
    let (shape, data) = outputs["z"].try_extract_tensor::<f32>().unwrap();
    assert_eq!(&**shape, &[4i64]);
    data.to_vec()
}

/// Builds a session straight through the `rightkit_ort::ort` re-export (no direct `ort`
/// dependency) with the same hand-encoded model.
fn run_add_via_reexport() -> Vec<f32> {
    use rightkit_ort::ort::session::Session;
    let mut session = Session::builder()
        .unwrap()
        .commit_from_memory(&add_model())
        .expect("session through re-export");
    let x = Tensor::from_array(([4usize], vec![1.0f32, 2.0, 3.0, 4.0])).unwrap();
    let y = Tensor::from_array(([4usize], vec![5.0f32, 6.0, 7.0, 8.0])).unwrap();
    let outputs = session
        .run(rightkit_ort::ort::inputs!["x" => x, "y" => y])
        .unwrap();
    let (_, data) = outputs["z"].try_extract_tensor::<f32>().unwrap();
    data.to_vec()
}

fn setup() -> rightkit_ort::EnvironmentReport {
    static REPORT: OnceLock<rightkit_ort::EnvironmentReport> = OnceLock::new();
    REPORT
        .get_or_init(|| {
            let (app, _) = staged_app();
            // Installed-layout discovery from a fake executable path inside the bundle.
            let exe = app.join("Contents/MacOS/example");
            let candidate = installed_runtime_candidate(&exe, "runtime").unwrap();
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
fn real_dylib_matches_approved_catalog_and_loads_with_expected_version() {
    let catalog = NativeCatalog::embedded().unwrap();
    let record = catalog
        .approved(NativeLibrary::Onnxruntime, "macos", "aarch64")
        .unwrap();
    assert_eq!(record.version, "1.24.4");
    let (_, staged) = staged_app();
    record
        .verify_file(staged)
        .expect("staged runtime must match the approved digest");

    let report = setup();
    assert_eq!(report.runtime_path, staged.canonicalize().unwrap());
    assert!(
        report.runtime_info.starts_with("ORT Build Info:"),
        "ORT info: {}",
        report.runtime_info
    );
    assert!(report.shared_pool_active);
}

#[test]
fn providers_are_probed_from_the_loaded_runtime() {
    setup();
    let probes = probe_providers();
    let get = |k| probes.iter().find(|p| p.provider == k).unwrap();
    assert!(get(ProviderKind::Cpu).available);
    assert!(
        get(ProviderKind::CoreMl).available,
        "{:?}",
        get(ProviderKind::CoreMl)
    );
    assert!(!get(ProviderKind::DirectMl).available);
    assert!(get(ProviderKind::DirectMl).detail.contains("not supported"));
}

#[test]
fn tiny_model_runs_on_cpu_with_shared_and_private_pools() {
    setup();
    assert_eq!(
        run_add(&SessionOptions::new(ExecutionProvider::Cpu)),
        vec![11.0, 22.0, 33.0, 44.0]
    );
    // Explicit threads opt out of the shared pool and still compute the same.
    let private = SessionOptions::new(ExecutionProvider::Cpu).with_threads(Some(2));
    assert_eq!(run_add(&private), vec![11.0, 22.0, 33.0, 44.0]);
}

#[test]
fn tiny_model_runs_on_coreml_and_directml_is_refused_on_macos() {
    setup();
    assert_eq!(
        run_add(&SessionOptions::new(ExecutionProvider::CoreMl)),
        vec![11.0, 22.0, 33.0, 44.0]
    );
    assert!(matches!(
        SessionOptions::new(ExecutionProvider::DirectMl).build_from_memory(&add_model()),
        Err(SessionError::ProviderUnsupported(
            ExecutionProvider::DirectMl
        ))
    ));
}

#[test]
fn platform_accelerator_is_coreml_and_cpu_fallback_is_reported() {
    setup();
    assert_eq!(
        ExecutionProvider::platform_accelerator(),
        ExecutionProvider::CoreMl
    );
    let accelerated = SessionOptions::new(ExecutionProvider::platform_accelerator())
        .with_cpu_fallback(true)
        .build_from_memory_reported(&add_model())
        .expect("coreml session");
    assert_eq!(accelerated.provider, ExecutionProvider::CoreMl);
    assert!(accelerated.fallback.is_none());

    // DirectML cannot exist on macOS: with fallback the session lands on CPU,
    // says why, and still computes the right answer.
    let mut fell_back = SessionOptions::new(ExecutionProvider::DirectMl)
        .with_cpu_fallback(true)
        .build_from_memory_reported(&add_model())
        .expect("cpu fallback session");
    assert_eq!(fell_back.provider, ExecutionProvider::Cpu);
    let reason = fell_back.fallback.as_deref().expect("fallback reason");
    assert!(reason.starts_with("DirectMl -> Cpu"), "{reason}");
    let a = Tensor::from_array(([4usize], vec![1.0f32, 2.0, 3.0, 4.0])).unwrap();
    let b = Tensor::from_array(([4usize], vec![10.0f32, 20.0, 30.0, 40.0])).unwrap();
    let out = fell_back
        .session
        .run(rightkit_ort::ort::inputs!["x" => a, "y" => b])
        .unwrap();
    assert_eq!(
        out["z"].try_extract_tensor::<f32>().unwrap().1,
        &[11.0f32, 22.0, 33.0, 44.0]
    );
}

#[test]
fn model_file_path_and_missing_file_are_distinguished() {
    setup();
    let path = std::env::temp_dir().join(format!("rightkit-ort-add-{}.onnx", std::process::id()));
    std::fs::write(&path, add_model()).unwrap();
    let mut session = SessionOptions::default().build_from_file(&path).unwrap();
    let a = Tensor::from_array(([4usize], vec![0.5f32; 4])).unwrap();
    let b = Tensor::from_array(([4usize], vec![0.25f32; 4])).unwrap();
    let out = session.run(rightkit_ort::ort::inputs!["x" => a, "y" => b]).unwrap();
    assert_eq!(
        out["z"].try_extract_tensor::<f32>().unwrap().1,
        &[0.75f32; 4]
    );
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        SessionOptions::default().build_from_file(&path),
        Err(SessionError::MissingArtifact(_))
    ));
}

#[test]
fn system_runtime_copy_is_rejected_before_any_load() {
    let root = std::env::temp_dir().join(format!("rightkit-ort-sys-{}", std::process::id()));
    let sys = root.join("Windows/System32");
    std::fs::create_dir_all(&sys).unwrap();
    let dll = sys.join(runtime_filename());
    std::fs::write(&dll, b"x").unwrap();
    assert!(matches!(
        resolve_runtime([RuntimeCandidate::new(&dll, CandidateSource::Explicit)]),
        Err(RuntimeError::SystemRuntimeRejected { .. })
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn reexported_ort_builds_and_runs_a_session_without_a_direct_dependency() {
    setup();
    assert_eq!(run_add_via_reexport(), vec![6.0, 8.0, 10.0, 12.0]);
}
