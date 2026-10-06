//! End-to-end: real image fixtures through the real OS-native backend (Apple
//! Vision on macOS, Windows.Media.Ocr on Windows), real child-per-request
//! workers, and the real PP-OCRv6 ONNX models, validated by recognized
//! content and geometry. ONNX tests report SKIPPED (and pass) when the model
//! set / runtime are absent; set RIGHTKIT_OCR_MODEL_DIR / RIGHTKIT_OCR_ORT_RUNTIME
//! to point at them (monorepo defaults are probed).
//!
//! Layout of tests/fixtures/layout.png (1000x600): "Quarterly Report 2026"
//! top-left, "Total due: 1234 USD" mid-left, "Page 7 of 12" bottom-centre.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_ocr::{
    is_backend_unavailable, worker, BackendPreference, Isolation, Ocr, OcrArtifactManifest,
    OcrBackend, OcrError, OcrLine, OcrOptions, OnnxConfig, WorkerCommand,
};

const EXPECT: [&str; 3] = ["quarterly report 2026", "total due: 1234 usd", "page 7 of 12"];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn norm(s: &str) -> String {
    s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn real_worker() -> WorkerCommand {
    WorkerCommand::new(env!("CARGO_BIN_EXE_rightkit-ocr"), [worker::DEFAULT_WORKER_ARG])
}

fn onnx_config() -> Option<OnnxConfig> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let models = std::env::var_os("RIGHTKIT_OCR_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("../../../../scraperight/models/ocr"));
    let runtime = std::env::var_os("RIGHTKIT_OCR_ORT_RUNTIME")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(if cfg!(windows) { "../../../bin/onnxruntime.dll" } else { "../../../bin/libonnxruntime.dylib" }));
    let (models, runtime) = (models.canonicalize().ok()?, runtime.canonicalize().ok()?);
    if !models.join("det.onnx").exists() {
        return None;
    }
    let manifest = OcrArtifactManifest::characterized_windows_contract()
        .with_resolved_provenance("PP-OCRv6", "PaddlePaddle PP-OCRv6", "Apache-2.0")
        .unwrap();
    let mut cfg = OnnxConfig::new(models, manifest);
    cfg.ort_runtime = Some(runtime);
    Some(cfg)
}

/// Lines in the fixture, matched by content (backends may merge/split differently).
fn assert_layout(lines: &[OcrLine], backend: &str) {
    let find = |needle: &str| {
        lines
            .iter()
            .find(|l| norm(&l.text).contains(needle))
            .unwrap_or_else(|| panic!("{backend}: no line containing {needle:?}; got {lines:#?}"))
    };
    let (title, total, page) = (find("quarterly report"), find("total due"), find("page 7"));
    assert!(norm(&title.text).contains("2026"), "{backend}: {title:?}");
    assert!(norm(&total.text).contains("1234"), "{backend}: {total:?}");
    // Normalized, top-left origin: title above total above page footer.
    assert!(title.rect.y < 0.2 && title.rect.y > 0.03, "{backend} title y {:?}", title.rect);
    assert!(total.rect.y > 0.35 && total.rect.y < 0.55, "{backend} total y {:?}", total.rect);
    assert!(page.rect.y > 0.75, "{backend} page y {:?}", page.rect);
    assert!(title.rect.y < total.rect.y && total.rect.y < page.rect.y);
    // Horizontal placement: footer starts right of the left-aligned lines.
    assert!(title.rect.x < 0.15 && page.rect.x > 0.3, "{backend} x {:?} {:?}", title.rect, page.rect);
    for l in lines {
        let r = l.rect;
        assert!(r.x >= 0.0 && r.y >= 0.0 && r.width > 0.0 && r.height > 0.0, "{backend} {l:?}");
        assert!(r.x + r.width <= 1.001 && r.y + r.height <= 1.001, "{backend} {l:?}");
        if let Some(c) = l.confidence {
            assert!((0.0..=1.0).contains(&c), "{backend} confidence {l:?}");
        }
    }
}

#[test]
fn native_backend_reads_layout_with_geometry_in_process() {
    if rightkit_ocr::native_backend().is_none() {
        eprintln!("SKIPPED: no native backend on this OS");
        return;
    }
    // Exactly one in-process request: Vision may reject repeated accurate
    // requests in one process (the reason the worker exists).
    let mut ocr = Ocr::new(OcrOptions::new(BackendPreference::Native));
    let result = ocr.recognize_file(&fixture("layout.png")).expect("native OCR");
    assert!(result.backend.is_native() && result.fallback_reason.is_none());
    assert_layout(&result.lines, "native");
    let text = norm(&result.text());
    for e in EXPECT {
        assert!(text.contains(e), "text {text:?} missing {e:?}");
    }
    if result.backend == OcrBackend::Vision {
        assert!(result.lines.iter().all(|l| l.confidence.is_some_and(|c| c > 0.3)));
    }
}

#[test]
fn isolated_workers_serve_repeated_requests_png_and_jpeg() {
    if rightkit_ocr::native_backend().is_none() {
        return;
    }
    let mut ocr = Ocr::new(
        OcrOptions::new(BackendPreference::Native).with_isolation(Isolation::Worker(real_worker())),
    );
    // Child-per-request: three requests, three fresh processes, all succeed.
    for name in ["layout.png", "layout.jpg", "layout.png"] {
        let r = ocr.recognize_file(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_layout(&r.lines, name);
    }
    assert!(ocr.probe_native().unwrap(), "canary must prove the backend healthy");
}

#[test]
fn isolated_blank_page_is_empty_not_unavailable_and_garbage_is_a_plain_error() {
    if rightkit_ocr::native_backend().is_none() {
        return;
    }
    let mut ocr = Ocr::new(
        OcrOptions::new(BackendPreference::Native).with_isolation(Isolation::Worker(real_worker())),
    );
    let blank = image::RgbImage::from_pixel(400, 300, image::Rgb([255, 255, 255]));
    let r = ocr.recognize(&blank).expect("blank page");
    assert!(r.lines.is_empty(), "{:?}", r.lines);

    let err = ocr.recognize_encoded(b"definitely not an image").unwrap_err();
    assert!(matches!(err, OcrError::Runtime(_)), "{err:?}");
    assert!(!is_backend_unavailable(&err.to_string()));
}

#[cfg(unix)]
#[test]
fn worker_deadline_kills_a_hung_child() {
    let cmd = WorkerCommand::new("/bin/sleep", ["30"]).with_deadline(Duration::from_millis(300));
    let started = std::time::Instant::now();
    let err = cmd.run(b"x").unwrap_err();
    assert!(err.contains("deadline"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[cfg(unix)]
#[test]
fn worker_rejects_oversized_and_malformed_child_output() {
    let junk = WorkerCommand::new("/bin/sh", ["-c", "cat >/dev/null; echo not-json"]);
    assert_eq!(junk.run(b"x").unwrap_err(), "OCR worker returned malformed JSON");
    let flood = WorkerCommand::new("/bin/sh", ["-c", "cat >/dev/null; head -c 9000000 /dev/zero"]);
    assert!(flood.run(b"x").unwrap_err().contains("limit"));
}

#[test]
fn onnx_backend_returns_lines_with_geometry_and_confidence() {
    let Some(cfg) = onnx_config() else {
        eprintln!("SKIPPED: ONNX models/runtime not found");
        return;
    };
    let mut ocr = Ocr::new(OcrOptions::new(BackendPreference::Onnx).with_onnx(cfg));
    let r = ocr.recognize_file(&fixture("layout.png")).expect("onnx OCR");
    assert_eq!(r.backend, OcrBackend::Onnx);
    assert_layout(&r.lines, "onnx");
    assert!(
        r.lines.iter().all(|l| l.confidence.is_some_and(|c| c > 0.5)),
        "PP-OCR recognizer emits softmax scores: {:?}",
        r.lines
    );
}

#[cfg(unix)]
#[test]
fn auto_falls_back_to_onnx_only_when_native_is_unavailable() {
    let Some(cfg) = onnx_config() else {
        eprintln!("SKIPPED: ONNX models/runtime not found");
        return;
    };
    // A real child process that reports the native backend unavailable.
    let unavailable = WorkerCommand::new(
        "/bin/sh",
        ["-c", r#"cat >/dev/null; echo '{"ok":false,"error":"macos-vision-unavailable: simulated"}'"#],
    );
    let mut ocr = Ocr::new(
        OcrOptions::new(BackendPreference::Auto)
            .with_onnx(cfg.clone())
            .with_isolation(Isolation::Worker(unavailable.clone())),
    );
    let r = ocr.recognize_file(&fixture("layout.png")).expect("fallback result");
    assert_eq!(r.backend, OcrBackend::Onnx);
    assert!(r.fallback_reason.as_deref().unwrap().starts_with("macos-vision-unavailable:"));
    assert_layout(&r.lines, "fallback");

    // No ONNX configured: the unavailable verdict surfaces, typed.
    let mut bare = Ocr::new(
        OcrOptions::new(BackendPreference::Auto).with_isolation(Isolation::Worker(unavailable)),
    );
    assert!(matches!(
        bare.recognize_file(&fixture("layout.png")),
        Err(OcrError::NoFallbackConfigured(_))
    ));

    // A per-image fault must NOT be masked by the fallback.
    let per_image = WorkerCommand::new(
        "/bin/sh",
        ["-c", r#"cat >/dev/null; echo '{"ok":false,"error":"empty image"}'"#],
    );
    let mut ocr = Ocr::new(
        OcrOptions::new(BackendPreference::Auto)
            .with_onnx(cfg)
            .with_isolation(Isolation::Worker(per_image)),
    );
    let err = ocr.recognize_file(&fixture("layout.png")).unwrap_err();
    assert!(matches!(err, OcrError::Runtime(ref m) if m == "empty image"), "{err:?}");
}

#[test]
fn auto_prefers_native_and_native_and_onnx_agree_on_content() {
    let Some(native) = rightkit_ocr::native_backend() else {
        return;
    };
    let mut auto = Ocr::new(
        OcrOptions::new(BackendPreference::Auto).with_isolation(Isolation::Worker(real_worker())),
    );
    let n = auto.recognize_file(&fixture("layout.png")).unwrap();
    assert_eq!(n.backend, native);
    assert!(n.fallback_reason.is_none());

    let Some(cfg) = onnx_config() else {
        eprintln!("SKIPPED agreement: ONNX models/runtime not found");
        return;
    };
    let mut onnx = Ocr::new(OcrOptions::new(BackendPreference::Onnx).with_onnx(cfg));
    let o = onnx.recognize_file(&fixture("layout.png")).unwrap();
    assert_eq!(norm(&n.text()), norm(&o.text()), "native vs onnx text");
    for needle in ["quarterly report", "total due", "page 7"] {
        let a = n.lines.iter().find(|l| norm(&l.text).contains(needle)).unwrap().rect;
        let b = o.lines.iter().find(|l| norm(&l.text).contains(needle)).unwrap().rect;
        assert!((a.y - b.y).abs() < 0.06 && (a.x - b.x).abs() < 0.06, "{needle}: {a:?} vs {b:?}");
    }
}
