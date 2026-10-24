// rightkit-ocr 0.2.3 consumed by exact registry pin (default features: the OS-native backend and
// the verified ONNX fallback are both compiled), outside the workspace.
//
// Proves everything that does not need an OS text recognizer, a model or ONNX Runtime: the error
// and kind contract, the image decode guard, the artifact manifest and its fail-closed provenance,
// digest-verified model directories built from tiny synthetic files, the model lease hook, the
// router's refusals, and the isolated worker protocol in both directions. The worker child is this
// same executable: the crate's own `worker::run_if_requested` serves real requests (which fail on
// purpose at the image decoder, before any recognition), and a fake child speaking the same
// protocol drives every success, failure kind and lifecycle event, including a router round trip
// where the host has a native backend. Public API only.
// Does not prove: recognition by Apple Vision, Windows.Media.Ocr or the PP-OCR models, ONNX Runtime
// loading or session building, DirectML or CoreML, and accuracy of any kind.
// State: a per-process directory under the OS temp directory, removed at the end, and short-lived
// child processes of this executable. No network, no user data.
use rightkit_ocr::worker::{
    self, WorkerResponse, DEFAULT_WORKER_ARG, MAX_INPUT_BYTES, MAX_OUTPUT_BYTES,
    SELF_DEADLINE_EXIT, WORKER_DEADLINE,
};
use rightkit_ocr::{
    canary_png, decode_encoded, is_backend_unavailable, native_backend, ocr_image_with_manifest,
    BackendPreference, Isolation, ModelLeaseHook, NativeHealthHook, Ocr, OcrArtifactManifest,
    OcrBackend, OcrEngine, OcrError, OcrErrorKind, OcrLine, OcrOptions, OcrOutcome, OcrProvenance,
    OcrRect, OcrResult, OnnxConfig, VerifiedOcrArtifacts, WorkerCommand, WorkerEvent, WorkerEvents,
    WorkerFailure, MACOS_VISION_UNAVAILABLE, MAX_PIXELS, OCR_UNSUPPORTED_PLATFORM,
    WINDOWS_OCR_UNAVAILABLE,
};
use std::any::Any;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FAKE_ARG: &str = "--rightkit-ocr-consumer-fake-worker";

// SHA-256 of the three synthetic model files below.
const DET_SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"; // "abc"
const REC_SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"; // empty
const DICT_SHA: &str = "911169ddaaf146aff539f58c26c489af3b892dff0fe283c1c264c65ae5aa59a2"; // "a\nb\n"
const WRONG_SHA: &str = "2b64eaadff58068f5695b65b6fd86742a36a6e2f7ce1bfc662dc1df4da580f97"; // "wrong-artifact"

fn line(text: &str) -> OcrLine {
    OcrLine {
        text: text.into(),
        rect: OcrRect {
            x: 0.125,
            y: 0.25,
            width: 0.5,
            height: 0.0625,
        },
        confidence: Some(0.75),
    }
}

// ---- the fake worker child ------------------------------------------------------------------

fn respond(response: &WorkerResponse) {
    let mut out = std::io::stdout();
    writeln!(out, "{}", serde_json::to_string(response).unwrap()).unwrap();
    out.flush().unwrap();
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0u64, |sum, b| (sum * 31 + u64::from(*b)) % 1_000_003)
}

fn run_fake_worker(mode: &str) -> ! {
    let mut input = Vec::new();
    if mode != "sleep" && mode != "flood" {
        std::io::stdin().read_to_end(&mut input).unwrap();
    }
    let ok = |lines: Vec<OcrLine>| WorkerResponse {
        ok: true,
        lines: Some(lines),
        error: None,
    };
    let failed = |message: &str| WorkerResponse {
        ok: false,
        lines: None,
        error: Some(message.into()),
    };
    match mode {
        "ok" => respond(&ok(vec![line("RIGHTKIT OCR 2026")])),
        "echo" => respond(&ok(vec![line(&format!(
            "{}:{}",
            input.len(),
            checksum(&input)
        ))])),
        "empty" => respond(&ok(vec![])),
        "windows-unavailable" => respond(&failed(&format!(
            "{WINDOWS_OCR_UNAVAILABLE} no language pack"
        ))),
        "vision-unavailable" => respond(&failed(&format!(
            "{MACOS_VISION_UNAVAILABLE} request failed"
        ))),
        "unsupported" => respond(&failed(&format!("{OCR_UNSUPPORTED_PLATFORM} none here"))),
        "runtime-error" => respond(&failed("decode exploded")),
        "garbage" => println!("this is not json"),
        "no-lines" => println!(r#"{{"ok":true}}"#),
        "bad-exit" => {
            respond(&ok(vec![]));
            std::process::exit(3);
        }
        "self-deadline" => std::process::exit(SELF_DEADLINE_EXIT),
        "sleep" => std::thread::sleep(Duration::from_secs(60)),
        "flood" => {
            let chunk = vec![b'x'; 64 * 1024];
            let mut out = std::io::stdout();
            for _ in 0..(MAX_OUTPUT_BYTES / chunk.len() + 8) {
                if out.write_all(&chunk).is_err() {
                    break;
                }
            }
        }
        other => panic!("unknown fake worker mode {other}"),
    }
    std::process::exit(0)
}

fn fake(mode: &str) -> WorkerCommand {
    WorkerCommand::new(std::env::current_exe().unwrap(), [FAKE_ARG, mode])
}

#[derive(Default)]
struct Recorder(Mutex<Vec<WorkerEvent>>);

impl Recorder {
    fn tags(&self) -> Vec<&'static str> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|event| match event {
                WorkerEvent::Spawned { .. } => "spawned",
                WorkerEvent::Waiting => "waiting",
                WorkerEvent::Responded { .. } => "responded",
                WorkerEvent::Exited { .. } => "exited",
                WorkerEvent::TimedOut => "timed-out",
                _ => "other",
            })
            .collect()
    }
}

impl WorkerEvents for Recorder {
    fn on_event(&self, event: WorkerEvent) {
        self.0.lock().unwrap().push(event);
    }
}

fn expect_failure(command: &WorkerCommand, bytes: &[u8]) -> WorkerFailure {
    match command.run_observed(bytes, &|_: WorkerEvent| {}) {
        Ok(run) => panic!("expected a failure, got {run:?}"),
        Err(failure) => failure,
    }
}

// ---- PNG helpers ---------------------------------------------------------------------------

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
    out
}

/// A PNG whose header claims `width` by `height` RGB pixels; it carries no pixel data.
fn png_header_only(width: u32, height: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    out.extend(chunk(b"IHDR", &ihdr));
    out.extend(chunk(b"IDAT", &[]));
    out
}

// ---- checks --------------------------------------------------------------------------------

fn kinds_errors_and_serde() {
    // Prefixes are compatibility text; kinds are the contract.
    assert_eq!(WINDOWS_OCR_UNAVAILABLE, "windows-ocr-unavailable:");
    assert_eq!(MACOS_VISION_UNAVAILABLE, "macos-vision-unavailable:");
    assert_eq!(OCR_UNSUPPORTED_PLATFORM, "ocr-unsupported-platform:");
    for (prefix, kind) in [
        (WINDOWS_OCR_UNAVAILABLE, OcrErrorKind::WindowsOcrUnavailable),
        (
            MACOS_VISION_UNAVAILABLE,
            OcrErrorKind::MacosVisionUnavailable,
        ),
        (OCR_UNSUPPORTED_PLATFORM, OcrErrorKind::UnsupportedPlatform),
    ] {
        let message = format!("{prefix} detail");
        assert!(is_backend_unavailable(&message));
        assert_eq!(OcrErrorKind::from_message(&message), Some(kind));
        assert!(kind.is_backend_unavailable());
    }
    assert!(!is_backend_unavailable("Cannot decode image"));
    assert_eq!(OcrErrorKind::from_message("Cannot decode image"), None);
    for (kind, name) in [
        (
            OcrErrorKind::MacosVisionUnavailable,
            "macos_vision_unavailable",
        ),
        (
            OcrErrorKind::WindowsOcrUnavailable,
            "windows_ocr_unavailable",
        ),
        (OcrErrorKind::UnsupportedPlatform, "unsupported_platform"),
        (OcrErrorKind::BackendUnavailable, "backend_unavailable"),
        (OcrErrorKind::NoFallbackConfigured, "no_fallback_configured"),
        (OcrErrorKind::BlankPage, "blank_page"),
        (OcrErrorKind::RequestTooLarge, "request_too_large"),
        (OcrErrorKind::WorkerTimeout, "worker_timeout"),
        (OcrErrorKind::WorkerOutputExceeded, "worker_output_exceeded"),
        (OcrErrorKind::WorkerProtocol, "worker_protocol"),
        (OcrErrorKind::WorkerFailed, "worker_failed"),
        (OcrErrorKind::Provenance, "provenance"),
        (OcrErrorKind::Artifact, "artifact"),
        (OcrErrorKind::Runtime, "runtime"),
    ] {
        assert_eq!(kind.as_str(), name);
    }
    assert!(!OcrErrorKind::BackendUnavailable.is_backend_unavailable());
    assert_eq!(
        serde_json::to_string(&OcrErrorKind::WorkerTimeout).unwrap(),
        "\"workerTimeout\""
    );

    // OcrError: Display text and the kind each variant maps to.
    let path = PathBuf::from("models");
    for (error, kind, text) in [
        (
            OcrError::UnresolvedProvenance {
                labels: vec!["A".into(), "B".into()],
            },
            OcrErrorKind::Provenance,
            "OCR provenance conflict is unresolved: A vs B",
        ),
        (
            OcrError::InvalidProvenance,
            OcrErrorKind::Provenance,
            "OCR provenance label, source, and license must be non-empty",
        ),
        (
            OcrError::UnsafeArtifactPath { path: path.clone() },
            OcrErrorKind::Artifact,
            "OCR artifact path must be absolute: models",
        ),
        (
            OcrError::MissingArtifact { path: path.clone() },
            OcrErrorKind::Artifact,
            "OCR artifact is missing: models",
        ),
        (
            OcrError::DigestMismatch {
                path: path.clone(),
                expected: "e".into(),
                actual: "a".into(),
            },
            OcrErrorKind::Artifact,
            "OCR artifact digest mismatch for models: expected e, got a",
        ),
        (
            OcrError::Runtime("boom".into()),
            OcrErrorKind::Runtime,
            "OCR runtime error: boom",
        ),
        (
            OcrError::BackendUnavailable("nope".into()),
            OcrErrorKind::BackendUnavailable,
            "OCR backend unavailable: nope",
        ),
        (
            OcrError::NoFallbackConfigured("why".into()),
            OcrErrorKind::NoFallbackConfigured,
            "OCR native backend unavailable and no ONNX fallback configured: why",
        ),
    ] {
        assert_eq!(error.kind(), kind, "{error}");
        assert_eq!(error.to_string(), text);
    }
    assert_eq!(
        OcrError::BackendUnavailable(format!("{MACOS_VISION_UNAVAILABLE} x")).kind(),
        OcrErrorKind::MacosVisionUnavailable,
        "a backend verdict is refined by its prefix"
    );
    assert_eq!(
        OcrError::from("text".to_string()),
        OcrError::Runtime("text".into())
    );

    // Results and lines serialise in camelCase.
    let result = OcrResult {
        lines: vec![line("one"), line("two")],
        backend: OcrBackend::Onnx,
        fallback_reason: Some("why".into()),
    };
    assert_eq!(result.text(), "one\ntwo");
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["backend"], "onnx");
    assert_eq!(json["fallbackReason"], "why");
    assert_eq!(json["lines"][0]["rect"]["width"], 0.5);
    assert_eq!(json["lines"][0]["confidence"], 0.75);
    let wire: OcrLine = serde_json::from_value(json["lines"][1].clone()).unwrap();
    assert_eq!(wire, line("two"));
    assert_eq!(
        serde_json::to_string(&OcrBackend::WindowsMedia).unwrap(),
        "\"windowsMedia\""
    );
    assert_eq!(
        serde_json::to_string(&OcrBackend::Vision).unwrap(),
        "\"vision\""
    );
    assert!(OcrBackend::Vision.is_native() && OcrBackend::WindowsMedia.is_native());
    assert!(!OcrBackend::Onnx.is_native());
    assert_eq!(
        OcrOutcome {
            text: "t".into(),
            backend: OcrBackend::Onnx
        },
        OcrOutcome {
            text: "t".into(),
            backend: OcrBackend::Onnx
        }
    );

    // Which native backend this build has depends only on the target.
    let expected = if cfg!(target_os = "macos") {
        Some(OcrBackend::Vision)
    } else if cfg!(windows) {
        Some(OcrBackend::WindowsMedia)
    } else {
        None
    };
    assert_eq!(native_backend(), expected);
}

fn decode_guard() {
    assert_eq!(MAX_PIXELS, 40_000_000);
    let canary = decode_encoded(canary_png()).expect("the embedded canary decodes");
    let pixels = u64::from(canary.width()) * u64::from(canary.height());
    assert!(pixels > 0 && pixels < MAX_PIXELS);
    let garbage = decode_encoded(b"definitely not an image").unwrap_err();
    assert!(garbage.starts_with("Cannot"), "{garbage}");
    // The pixel limit is enforced on the header, before any buffer is allocated.
    let bomb = decode_encoded(&png_header_only(10_000, 10_000)).unwrap_err();
    assert_eq!(
        bomb,
        "Image is 10000x10000 (100000000 pixels), over the 40000000-pixel limit"
    );
}

fn write_model_dir(root: &Path, name: &str, files: [(&str, &[u8]); 3]) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, bytes) in files {
        std::fs::write(dir.join(file), bytes).unwrap();
    }
    dir.canonicalize().unwrap()
}

fn synthetic_manifest() -> OcrArtifactManifest {
    let mut manifest = OcrArtifactManifest::pp_ocr_v6();
    manifest.detector_sha256 = DET_SHA.into();
    manifest.recognizer_sha256 = REC_SHA.into();
    manifest.dictionary_sha256 = DICT_SHA.into();
    manifest
}

#[derive(Debug, Default)]
struct RecordingHook {
    calls: AtomicUsize,
    paths: Mutex<Vec<PathBuf>>,
    drops: Arc<AtomicUsize>,
    refuse: bool,
}

struct Guard(Arc<AtomicUsize>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl ModelLeaseHook for RecordingHook {
    fn acquire_model_lease(&self, paths: &[PathBuf]) -> Result<Box<dyn Any + Send>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.paths.lock().unwrap() = paths.to_vec();
        if self.refuse {
            return Err("model is in use by another app".into());
        }
        Ok(Box::new(Guard(self.drops.clone())))
    }
}

fn manifest_and_artifacts(root: &Path) {
    let characterized = OcrArtifactManifest::characterized_windows_contract();
    assert_eq!(
        characterized.provenance,
        OcrProvenance::Conflicting {
            labels: vec!["PP-OCRv5".into(), "PP-OCRv6".into()]
        }
    );
    assert_eq!(
        characterized.validate_metadata(),
        Err(OcrError::UnresolvedProvenance {
            labels: vec!["PP-OCRv5".into(), "PP-OCRv6".into()]
        })
    );
    assert_eq!(
        characterized.detector_sha256,
        "eb13b44b25bb36f89528b68720af8a61d9cf381176107f465db1757b65d086e1"
    );
    assert_eq!(
        characterized.expected_fixture_output,
        "RIGHTKIT OCR 2026\nShared parity fixture"
    );
    let v6 = OcrArtifactManifest::pp_ocr_v6();
    v6.validate_metadata()
        .expect("the PP-OCRv6 manifest is resolved");
    let OcrProvenance::Resolved {
        label,
        source,
        license,
    } = &v6.provenance
    else {
        panic!("pp_ocr_v6 must be resolved")
    };
    assert_eq!(
        (label.as_str(), license.as_str()),
        ("PP-OCRv6", "Apache-2.0")
    );
    assert!(source.contains("PaddlePaddle/PP-OCRv6_medium_det_onnx"));
    assert!(source.contains("PaddlePaddle/PP-OCRv6_small_rec_onnx"));
    assert_eq!(
        (
            &v6.detector_sha256,
            &v6.recognizer_sha256,
            &v6.dictionary_sha256,
            &v6.fixture_sha256
        ),
        (
            &characterized.detector_sha256,
            &characterized.recognizer_sha256,
            &characterized.dictionary_sha256,
            &characterized.fixture_sha256
        ),
        "resolving provenance changes no digest"
    );
    // Resolution needs a label, a source and a licence, each non-blank.
    for (label, source, license) in [
        ("", "s", "l"),
        ("a", "", "l"),
        ("a", "s", ""),
        ("a", " ", "l"),
    ] {
        assert_eq!(
            OcrArtifactManifest::characterized_windows_contract()
                .with_resolved_provenance(label, source, license),
            Err(OcrError::InvalidProvenance)
        );
    }
    let resolved = OcrArtifactManifest::characterized_windows_contract()
        .with_resolved_provenance("PP-OCR-test", "test-catalog", "test-license")
        .unwrap();
    assert!(
        matches!(&resolved.provenance, OcrProvenance::Resolved { label, .. } if label == "PP-OCR-test")
    );

    // Directory verification: absolute path first, then existence, then each digest.
    assert!(matches!(
        synthetic_manifest().validate_model_dir(Path::new("models/ocr")),
        Err(OcrError::UnsafeArtifactPath { .. })
    ));
    let absent = root.join("no-such-model-dir");
    assert!(matches!(
        synthetic_manifest().validate_model_dir(&absent),
        Err(OcrError::MissingArtifact { .. })
    ));
    assert!(
        matches!(
            characterized.validate_model_dir(&absent),
            Err(OcrError::UnresolvedProvenance { .. })
        ),
        "provenance is checked before the directory"
    );

    let good = write_model_dir(
        root,
        "good",
        [
            ("det.onnx", b"abc"),
            ("rec.onnx", b""),
            ("en_dict.txt", b"a\nb\n"),
        ],
    );
    let verified: VerifiedOcrArtifacts = synthetic_manifest()
        .validate_model_dir(&good)
        .expect("digests match");
    for (path, name) in [
        (&verified.detector, "det.onnx"),
        (&verified.recognizer, "rec.onnx"),
        (&verified.dictionary, "en_dict.txt"),
    ] {
        assert_eq!(path.file_name().unwrap(), name);
        assert!(path.is_file());
    }
    // The shipped digests do not match synthetic bytes: the detector is checked first.
    match OcrArtifactManifest::pp_ocr_v6().validate_model_dir(&good) {
        Err(OcrError::DigestMismatch {
            path,
            expected,
            actual,
        }) => {
            assert_eq!(path.file_name().unwrap(), "det.onnx");
            assert_eq!(expected, OcrArtifactManifest::pp_ocr_v6().detector_sha256);
            assert_eq!(actual, DET_SHA);
        }
        other => panic!("expected a digest mismatch, got {other:?}"),
    }
    let tampered = write_model_dir(
        root,
        "tampered",
        [
            ("det.onnx", b"abc"),
            ("rec.onnx", b"wrong-artifact"),
            ("en_dict.txt", b"a\nb\n"),
        ],
    );
    match synthetic_manifest().validate_model_dir(&tampered) {
        Err(OcrError::DigestMismatch {
            path,
            expected,
            actual,
        }) => {
            assert_eq!(path.file_name().unwrap(), "rec.onnx");
            assert_eq!((expected.as_str(), actual.as_str()), (REC_SHA, WRONG_SHA));
        }
        other => panic!("expected a digest mismatch, got {other:?}"),
    }
    std::fs::remove_file(good.join("en_dict.txt")).unwrap();
    assert!(matches!(
        synthetic_manifest().validate_model_dir(&good),
        Err(OcrError::MissingArtifact { .. })
    ));
    std::fs::write(good.join("en_dict.txt"), b"a\nb\n").unwrap();

    // Loading verifies before any ONNX session: a wrong set fails with a digest error, the one-call
    // helper does the same, and the lease hook is taken first and released when the load fails.
    assert!(matches!(
        OcrEngine::load_with_manifest(&good, &OcrArtifactManifest::pp_ocr_v6()),
        Err(OcrError::DigestMismatch { .. })
    ));
    assert!(matches!(
        ocr_image_with_manifest(
            &root.join("image.png"),
            &good,
            &OcrArtifactManifest::pp_ocr_v6()
        ),
        Err(OcrError::DigestMismatch { .. })
    ));
    assert!(matches!(
        OcrEngine::load(&good),
        Err(OcrError::DigestMismatch { .. })
    ));
    let hook = RecordingHook::default();
    let result = OcrEngine::load_with_lease(&good, &OcrArtifactManifest::pp_ocr_v6(), Some(&hook));
    assert!(matches!(result, Err(OcrError::DigestMismatch { .. })));
    assert_eq!(hook.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        hook.drops.load(Ordering::SeqCst),
        1,
        "the guard is released on failure"
    );
    let names: Vec<String> = hook
        .paths
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["det.onnx", "rec.onnx", "en_dict.txt"]);
    let refusing = RecordingHook {
        refuse: true,
        ..Default::default()
    };
    assert_eq!(
        OcrEngine::load_with_lease(&good, &OcrArtifactManifest::pp_ocr_v6(), Some(&refusing)).err(),
        Some(OcrError::Runtime("model is in use by another app".into())),
        "a refused lease stops the load before any file is verified"
    );
    assert!(matches!(
        OcrEngine::validate_sessions(&absent, &OcrArtifactManifest::pp_ocr_v6()),
        Err(OcrError::MissingArtifact { .. })
    ));
}

fn router_without_a_recognizer(root: &Path) {
    let bad = write_model_dir(
        root,
        "router",
        [
            ("det.onnx", b"abc"),
            ("rec.onnx", b""),
            ("en_dict.txt", b"a\nb\n"),
        ],
    );
    let image = decode_encoded(canary_png()).unwrap();

    // Defaults.
    let defaults = OcrOptions::default();
    assert_eq!(defaults.preference, BackendPreference::Auto);
    assert!(defaults.onnx.is_none() && matches!(defaults.isolation, Isolation::InProcess));
    assert!(matches!(Isolation::default(), Isolation::InProcess));
    let hook = Arc::new(RecordingHook::default());
    let config = OnnxConfig::new(&bad, OcrArtifactManifest::pp_ocr_v6());
    assert_eq!(
        (
            config.model_dir.as_path(),
            config.ort_runtime.is_none(),
            config.lease_hook.is_none()
        ),
        (bad.as_path(), true, true)
    );
    let config = config.with_lease_hook(hook.clone() as Arc<dyn ModelLeaseHook>);
    assert!(config.lease_hook.is_some());

    // The ONNX preference with no model config, and with a config whose files fail verification.
    let mut onnx_only = Ocr::new(OcrOptions::new(BackendPreference::Onnx));
    assert_eq!(
        onnx_only.recognize(&image).unwrap_err(),
        OcrError::NoFallbackConfigured("ONNX backend requested without model config".into())
    );
    let mut configured =
        Ocr::new(OcrOptions::new(BackendPreference::Onnx).with_onnx(config.clone()));
    assert!(matches!(
        configured.recognize(&image),
        Err(OcrError::DigestMismatch { .. })
    ));
    assert_eq!(hook.calls.load(Ordering::SeqCst), 1);
    assert_eq!(hook.drops.load(Ordering::SeqCst), 1);
    assert!(matches!(
        configured.recognize_with_provenance(&image),
        Err(OcrError::DigestMismatch { .. })
    ));

    // Inputs fail before any backend runs.
    let garbage = onnx_only
        .recognize_encoded(b"definitely not an image")
        .unwrap_err();
    assert!(
        matches!(&garbage, OcrError::Runtime(m) if m.starts_with("Cannot")),
        "{garbage}"
    );
    let missing = onnx_only
        .recognize_file(&root.join("missing.png"))
        .unwrap_err();
    assert!(
        matches!(&missing, OcrError::Runtime(m) if m.starts_with("Cannot read image")),
        "{missing}"
    );

    // Fallback validation verifies the files first.
    let unconfigured = Ocr::new(OcrOptions::new(BackendPreference::Auto));
    assert!(matches!(
        unconfigured.validate_fallback(),
        Err(OcrError::NoFallbackConfigured(_))
    ));
    let with_files = Ocr::new(OcrOptions::new(BackendPreference::Auto).with_onnx(config));
    assert!(matches!(
        with_files.validate_fallback(),
        Err(OcrError::DigestMismatch { .. })
    ));
}

fn worker_protocol() {
    assert_eq!(WORKER_DEADLINE, Duration::from_secs(90));
    assert_eq!(
        (MAX_INPUT_BYTES, MAX_OUTPUT_BYTES),
        (32 * 1024 * 1024, 8 * 1024 * 1024)
    );
    assert_eq!(
        (DEFAULT_WORKER_ARG, SELF_DEADLINE_EXIT),
        ("--rightkit-ocr-worker", 124)
    );
    let exe = std::env::current_exe().unwrap();
    let command = WorkerCommand::current_exe(DEFAULT_WORKER_ARG).unwrap();
    assert_eq!(command.executable, exe);
    assert_eq!(command.args, [std::ffi::OsString::from(DEFAULT_WORKER_ARG)]);
    assert_eq!(
        (command.deadline, command.canary_deadline),
        (WORKER_DEADLINE, WORKER_DEADLINE)
    );
    let tuned = command
        .clone()
        .with_deadline(Duration::from_secs(5))
        .with_canary_deadline(Duration::from_secs(2));
    assert_eq!(
        (tuned.deadline, tuned.canary_deadline),
        (Duration::from_secs(5), Duration::from_secs(2))
    );
    let canary = tuned.for_canary();
    assert_eq!(canary.deadline, Duration::from_secs(2));
    assert_eq!(canary.canary_deadline, Duration::from_secs(2));

    // A success: lifecycle events in order, and a record that matches them.
    let events = Recorder::default();
    let run = fake("ok")
        .run_observed(b"payload", &events)
        .expect("the fake worker answers");
    assert_eq!(run.lines, vec![line("RIGHTKIT OCR 2026")]);
    assert_eq!(events.tags(), ["spawned", "waiting", "exited", "responded"]);
    let expected_bytes = serde_json::to_string(&WorkerResponse {
        ok: true,
        lines: Some(vec![line("RIGHTKIT OCR 2026")]),
        error: None,
    })
    .unwrap()
    .len()
        + 1;
    assert_eq!(run.record.response_bytes, expected_bytes);
    assert!(run.record.pid.is_some() && run.record.exit_success && !run.record.timed_out);
    assert_eq!(run.record.exit_code, Some(0));
    // `run` is the same call without events and returns only the lines.
    assert_eq!(fake("ok").run(b"payload").unwrap(), run.lines);

    // The request reaches the child unchanged, including a payload larger than a pipe buffer.
    let payload: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8).collect();
    let echoed = fake("echo").run(&payload).unwrap();
    assert_eq!(
        echoed[0].text,
        format!("{}:{}", payload.len(), checksum(&payload))
    );
    assert!(fake("empty").run(b"x").unwrap().is_empty());

    // Each failure keeps its typed kind and its legacy text.
    for (mode, kind, text) in [
        (
            "windows-unavailable",
            OcrErrorKind::WindowsOcrUnavailable,
            "windows-ocr-unavailable: no language pack",
        ),
        (
            "vision-unavailable",
            OcrErrorKind::MacosVisionUnavailable,
            "macos-vision-unavailable: request failed",
        ),
        (
            "unsupported",
            OcrErrorKind::UnsupportedPlatform,
            "ocr-unsupported-platform: none here",
        ),
        ("runtime-error", OcrErrorKind::Runtime, "decode exploded"),
        (
            "garbage",
            OcrErrorKind::WorkerProtocol,
            "OCR worker returned malformed JSON",
        ),
        (
            "no-lines",
            OcrErrorKind::WorkerProtocol,
            "OCR worker success response omitted lines",
        ),
        (
            "bad-exit",
            OcrErrorKind::WorkerFailed,
            "OCR worker exited with a failure status",
        ),
    ] {
        let failure = expect_failure(&fake(mode), b"x");
        assert_eq!(failure.kind, kind, "{mode}");
        assert_eq!(failure.message, text, "{mode}");
        assert_eq!(failure.to_string(), text);
        assert!(
            failure.record.pid.is_some() && !failure.record.timed_out,
            "{mode}"
        );
    }
    let bad_exit = expect_failure(&fake("bad-exit"), b"x");
    assert_eq!(
        (bad_exit.record.exit_code, bad_exit.record.exit_success),
        (Some(3), false)
    );

    // The child's own watchdog exit code is a timeout, reported after the exit.
    let events = Recorder::default();
    let watchdog = fake("self-deadline")
        .run_observed(b"x", &events)
        .unwrap_err();
    assert_eq!(watchdog.kind, OcrErrorKind::WorkerTimeout);
    assert!(watchdog.record.timed_out);
    assert_eq!(watchdog.record.exit_code, Some(SELF_DEADLINE_EXIT));
    assert_eq!(
        events.tags(),
        ["spawned", "waiting", "exited", "timed-out", "responded"]
    );

    // The parent's deadline kills a child that never answers.
    let events = Recorder::default();
    let slow = fake("sleep")
        .with_deadline(Duration::from_millis(500))
        .run_observed(b"x", &events)
        .unwrap_err();
    assert_eq!(slow.kind, OcrErrorKind::WorkerTimeout);
    assert_eq!(slow.message, "OCR worker exceeded its deadline");
    assert!(slow.record.timed_out && !slow.record.exit_success);
    assert!(slow.record.elapsed >= Duration::from_millis(400));
    assert_eq!(
        events.tags(),
        ["spawned", "waiting", "timed-out", "exited", "responded"]
    );

    // Output beyond the protocol limit is cut off and reported.
    let flood = expect_failure(&fake("flood").with_deadline(Duration::from_secs(30)), b"x");
    assert_eq!(flood.kind, OcrErrorKind::WorkerOutputExceeded);

    // A request over the input limit never starts a child; a missing executable reports that.
    let events = Recorder::default();
    let huge = vec![0u8; MAX_INPUT_BYTES + 1];
    let refused = fake("ok").run_observed(&huge, &events).unwrap_err();
    assert_eq!(refused.kind, OcrErrorKind::RequestTooLarge);
    assert_eq!(refused.record.pid, None);
    assert!(events.tags().is_empty());
    let events = Recorder::default();
    let missing = WorkerCommand::new(
        std::env::temp_dir().join("rightkit-ocr-no-such-worker"),
        ["--probe"],
    )
    .run_observed(b"x", &events)
    .unwrap_err();
    assert_eq!(
        (missing.kind, missing.message.as_str()),
        (
            OcrErrorKind::WorkerFailed,
            "OCR worker process could not start"
        )
    );
    assert!(events.tags().is_empty() && missing.record.pid.is_none());

    // The crate's own worker, served by this executable through `run_if_requested`: it reads the
    // request, decodes it, and reports a decoder error before any recognizer is involved.
    let real = WorkerCommand::current_exe(DEFAULT_WORKER_ARG).unwrap();
    let not_an_image = expect_failure(&real, b"definitely not an image");
    assert_eq!(not_an_image.kind, OcrErrorKind::Runtime);
    assert!(
        not_an_image.message.starts_with("Cannot"),
        "{}",
        not_an_image.message
    );
    assert!(
        not_an_image.record.exit_success,
        "the worker answers with exit status zero"
    );
    let bomb = expect_failure(&real, &png_header_only(10_000, 10_000));
    assert!(
        bomb.message.contains("over the 40000000-pixel limit"),
        "{}",
        bomb.message
    );
}

struct Verdict(Option<bool>, Mutex<Vec<bool>>);

impl NativeHealthHook for Verdict {
    fn cached_verdict(&self) -> Option<bool> {
        self.0
    }

    fn record_verdict(&self, healthy: bool) {
        self.1.lock().unwrap().push(healthy);
    }
}

fn router_with_a_fake_worker() {
    let png = canary_png();
    let native = native_backend();

    // Native preference through an isolated child. Where the host has no native backend the router
    // refuses before spawning; where it has one, the child's lines come back under that backend.
    let mut ocr = Ocr::new(
        OcrOptions::new(BackendPreference::Native).with_isolation(Isolation::Worker(fake("ok"))),
    );
    match native {
        Some(backend) => {
            let result = ocr
                .recognize_encoded(png)
                .expect("the isolated native path answers");
            assert_eq!(result.backend, backend);
            assert_eq!(result.lines, vec![line("RIGHTKIT OCR 2026")]);
            assert_eq!(result.fallback_reason, None);
            assert_eq!(result.text(), "RIGHTKIT OCR 2026");
        }
        None => {
            let error = ocr.recognize_encoded(png).unwrap_err();
            assert!(matches!(error, OcrError::BackendUnavailable(_)));
            assert_eq!(error.kind(), OcrErrorKind::UnsupportedPlatform);
        }
    }

    // Auto with a native verdict of "unavailable" and no ONNX model set: no silent downgrade.
    let mut auto = Ocr::new(
        OcrOptions::new(BackendPreference::Auto)
            .with_isolation(Isolation::Worker(fake("windows-unavailable"))),
    );
    assert!(matches!(
        auto.recognize_encoded(png),
        Err(OcrError::NoFallbackConfigured(_))
    ));
    // A per-image fault stays visible rather than arming the fallback.
    if native.is_some() {
        let mut faulty = Ocr::new(
            OcrOptions::new(BackendPreference::Auto)
                .with_isolation(Isolation::Worker(fake("runtime-error"))),
        );
        assert_eq!(
            faulty.recognize_encoded(png).unwrap_err(),
            OcrError::Runtime("decode exploded".into())
        );
    }

    // The availability probe: the host's cached verdict short-circuits it; with none, a worker
    // that returns lines proves the native backend on a host that has one.
    let denied = Arc::new(Verdict(Some(false), Mutex::default()));
    let probe =
        Ocr::new(OcrOptions::new(BackendPreference::Native)).with_health_hook(denied.clone());
    assert_eq!(probe.probe_native(), Ok(false));
    assert!(!probe.is_available());
    let granted = Arc::new(Verdict(Some(true), Mutex::default()));
    let probe = Ocr::new(OcrOptions::new(BackendPreference::Native)).with_health_hook(granted);
    assert_eq!(probe.probe_native(), Ok(native.is_some()));
    assert_eq!(probe.is_available(), native.is_some());
    let unknown = Arc::new(Verdict(None, Mutex::default()));
    let probe = Ocr::new(
        OcrOptions::new(BackendPreference::Native).with_isolation(Isolation::Worker(fake("ok"))),
    )
    .with_health_hook(unknown.clone());
    assert_eq!(probe.probe_native(), Ok(native.is_some()));
    assert_eq!(
        *unknown.1.lock().unwrap(),
        if native.is_some() { vec![true] } else { vec![] }
    );
}

fn main() {
    // The crate's real worker mode first, as an app must (before any UI or other start-up work).
    if worker::run_if_requested(DEFAULT_WORKER_ARG) {
        return;
    }
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(FAKE_ARG) {
        run_fake_worker(args.get(2).map(String::as_str).unwrap_or("ok"));
    }
    let root = std::env::temp_dir().join(format!("rk-ocr-consumer-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create consumer temp dir");
    kinds_errors_and_serde();
    decode_guard();
    manifest_and_artifacts(&root);
    router_without_a_recognizer(&root);
    worker_protocol();
    router_with_a_fake_worker();
    std::fs::remove_dir_all(&root).expect("remove consumer temp dir");
    println!("OK rightkit-ocr 0.2.3 (registry consumer)");
}
