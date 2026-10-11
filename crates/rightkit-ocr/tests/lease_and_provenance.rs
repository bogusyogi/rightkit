#![cfg(feature = "onnx")]

use std::any::Any;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use image::RgbImage;
use rightkit_ocr::{
    BackendPreference, ModelLeaseHook, Ocr, OcrArtifactManifest, OcrEngine, OcrError, OcrOptions,
    OcrProvenance, OnnxConfig,
};

#[test]
fn pp_ocr_v6_manifest_is_resolved_and_matches_the_recorded_digests() {
    let manifest = OcrArtifactManifest::pp_ocr_v6();
    manifest.validate_metadata().unwrap();
    let OcrProvenance::Resolved {
        label,
        source,
        license,
    } = &manifest.provenance
    else {
        panic!("pp_ocr_v6 must be resolved");
    };
    assert_eq!(label, "PP-OCRv6");
    assert!(source.contains("PaddlePaddle/PP-OCRv6_medium_det_onnx"));
    assert!(source.contains("PaddlePaddle/PP-OCRv6_small_rec_onnx"));
    assert_eq!(license, "Apache-2.0");

    let provenance = include_str!("../PROVENANCE.toml");
    let contract = include_str!("contract.json");
    for digest in [
        &manifest.detector_sha256,
        &manifest.recognizer_sha256,
        &manifest.dictionary_sha256,
    ] {
        assert!(provenance.contains(digest.as_str()), "{digest}");
        assert!(contract.contains(digest.as_str()), "{digest}");
    }
    assert!(provenance.contains("label = \"PP-OCRv6\""));
    assert!(provenance.contains("license = \"Apache-2.0\""));
}

#[test]
fn characterized_contract_still_fails_closed() {
    let err = OcrArtifactManifest::characterized_windows_contract()
        .validate_metadata()
        .unwrap_err();
    assert!(matches!(err, OcrError::UnresolvedProvenance { .. }));
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

fn empty_model_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rightkit-ocr-lease-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["det.onnx", "rec.onnx", "en_dict.txt"] {
        std::fs::write(dir.join(name), b"not a model").unwrap();
    }
    dir.canonicalize().unwrap()
}

#[test]
fn lease_is_taken_before_verification_and_released_when_load_fails() {
    let dir = empty_model_dir("acquire");
    let hook = RecordingHook::default();
    let result = OcrEngine::load_with_lease(&dir, &OcrArtifactManifest::pp_ocr_v6(), Some(&hook));
    // Wrong bytes: verification fails, after the lease was already taken.
    assert!(matches!(result, Err(OcrError::DigestMismatch { .. })));
    assert_eq!(hook.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        hook.drops.load(Ordering::SeqCst),
        1,
        "guard released on failure"
    );
    let paths = hook.paths.lock().unwrap().clone();
    let names: Vec<_> = paths
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["det.onnx", "rec.onnx", "en_dict.txt"]);
    assert!(paths.iter().all(|p| p.starts_with(&dir)));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn refused_lease_stops_the_load_before_any_file_is_verified() {
    let dir = empty_model_dir("refuse");
    let hook = RecordingHook {
        refuse: true,
        ..Default::default()
    };
    let err = OcrEngine::load_with_lease(&dir, &OcrArtifactManifest::pp_ocr_v6(), Some(&hook))
        .err()
        .unwrap();
    assert_eq!(
        err,
        OcrError::Runtime("model is in use by another app".into())
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn onnx_config_hook_reaches_the_router_load() {
    let dir = empty_model_dir("router");
    let hook = Arc::new(RecordingHook::default());
    let cfg = OnnxConfig::new(&dir, OcrArtifactManifest::pp_ocr_v6())
        .with_lease_hook(hook.clone() as Arc<dyn ModelLeaseHook>);
    let mut ocr = Ocr::new(OcrOptions::new(BackendPreference::Onnx).with_onnx(cfg));
    let err = ocr
        .recognize(&RgbImage::from_pixel(8, 8, image::Rgb([255, 255, 255])))
        .unwrap_err();
    assert!(matches!(err, OcrError::DigestMismatch { .. }));
    assert_eq!(hook.calls.load(Ordering::SeqCst), 1);
    assert_eq!(hook.drops.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn without_a_hook_loading_is_unchanged() {
    let dir = empty_model_dir("nohook");
    let result = OcrEngine::load_with_manifest(&dir, &OcrArtifactManifest::pp_ocr_v6());
    assert!(matches!(result, Err(OcrError::DigestMismatch { .. })));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn validate_fallback_verifies_before_building_sessions() {
    let ocr = Ocr::new(OcrOptions::new(BackendPreference::Auto));
    assert!(matches!(
        ocr.validate_fallback(),
        Err(OcrError::NoFallbackConfigured(_))
    ));

    let dir = empty_model_dir("validate");
    let cfg = OnnxConfig::new(&dir, OcrArtifactManifest::pp_ocr_v6());
    let ocr = Ocr::new(OcrOptions::new(BackendPreference::Auto).with_onnx(cfg));
    assert!(matches!(
        ocr.validate_fallback(),
        Err(OcrError::DigestMismatch { .. })
    ));
    // An absolute path on every OS: a rooted path without a drive is not absolute on Windows
    // and is refused as unsafe before the directory is looked up.
    let missing =
        std::env::temp_dir().join(format!("nonexistent-rightkit-ocr-{}", std::process::id()));
    assert!(matches!(
        OcrEngine::validate_sessions(&missing, &OcrArtifactManifest::pp_ocr_v6()),
        Err(OcrError::MissingArtifact { .. })
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn is_available_and_provenance_report_consistently_with_the_backend() {
    let mut ocr = Ocr::new(OcrOptions::new(BackendPreference::Native));
    let available = ocr.is_available();
    let img = RgbImage::from_pixel(64, 64, image::Rgb([255, 255, 255]));
    match ocr.recognize_with_provenance(&img) {
        Ok(outcome) => {
            assert!(outcome.backend.is_native());
            assert!(available || outcome.text.is_empty());
        }
        Err(_) => assert!(!available || cfg!(not(feature = "native"))),
    }
}
