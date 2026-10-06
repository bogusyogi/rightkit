//! Real-model end-to-end gate (platform bundled-dylib parity). Opt-in: set
//! `RIGHTKIT_OCR_MODEL_DIR` (absolute dir holding det.onnx/rec.onnx/en_dict.txt)
//! and `RIGHTKIT_OCR_ORT_RUNTIME` (absolute path to the platform ONNX Runtime
//! library). Without both the test reports SKIPPED and passes, so the default
//! suite stays model-free; the gate evidence run must set them.

use base64::Engine;
use rightkit_ocr::{OcrArtifactManifest, OcrEngine};
use rightkit_ort::{configure_runtime, CandidateSource, RuntimeCandidate};

#[test]
fn bundled_runtime_reproduces_the_characterized_fixture_output() {
    let (Some(models), Some(runtime)) = (
        locate("RIGHTKIT_OCR_MODEL_DIR", "../../../../scraperight/models/ocr/det.onnx", true),
        locate("RIGHTKIT_OCR_ORT_RUNTIME", "../../../bin/libonnxruntime.dylib", false),
    ) else {
        eprintln!("SKIPPED: models/runtime not found (set RIGHTKIT_OCR_MODEL_DIR / RIGHTKIT_OCR_ORT_RUNTIME)");
        return;
    };
    configure_runtime([RuntimeCandidate::new(runtime, CandidateSource::Explicit)]).unwrap();
    let manifest = OcrArtifactManifest::characterized_windows_contract()
        .with_resolved_provenance(
            "PP-OCRv6",
            "https://huggingface.co/PaddlePaddle/PP-OCRv6_medium_det_onnx + PP-OCRv6_small_rec_onnx",
            "Apache-2.0",
        )
        .unwrap();
    let mut engine = OcrEngine::load_with_manifest(&models, &manifest)
        .expect("verified models load with the platform runtime");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(include_str!("fixtures/rightkit-ocr-parity.png.base64").trim())
        .unwrap();
    let image = image::load_from_memory(&bytes).unwrap().to_rgb8();
    let text = engine.run(&image).unwrap();
    assert_eq!(text, manifest.expected_fixture_output);
    eprintln!("providers: {:?}", engine.provider_diagnostics());
}

/// Env override first (the managed broker strips env, so a monorepo-relative
/// default is also probed). `parent` returns the containing directory.
fn locate(var: &str, relative: &str, parent: bool) -> Option<std::path::PathBuf> {
    let found = std::env::var_os(var)
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
            p.exists().then(|| p.canonicalize().ok()).flatten()
        })?;
    if parent {
        if found.is_dir() { Some(found) } else { found.parent().map(|p| p.to_path_buf()) }
    } else {
        Some(found)
    }
}
