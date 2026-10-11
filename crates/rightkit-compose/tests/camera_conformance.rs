//! Conformance of `Easing::Cosine` and `Easing::CameraSpring` against
//! ScreenRight's `cameraConformanceVectors()`, recorded in
//! `tests/fixtures/camera_conformance.json` (ScreenRight b4e2104,
//! src/postprocess.mjs). Values are rounded to six decimals by the source, so
//! each computed value is rounded the same way and compared within 1e-6.
//!
//! `panSpring` is the camera spring with bounce 0.15, `criticalSpring` with
//! bounce 0.

use rightkit_compose::*;

const TOLERANCE: f64 = 1e-6;

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

struct Vector {
    u: f64,
    cosine: f64,
    critical: f64,
    pan: f64,
}

fn load_vectors() -> Vec<Vector> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/camera_conformance.json"
    );
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("camera conformance fixture {path}: {e}"));
    let doc: serde_json::Value = serde_json::from_str(&text).expect("fixture is JSON");
    doc["vectors"]
        .as_array()
        .expect("vectors array")
        .iter()
        .map(|row| Vector {
            u: row["u"].as_f64().expect("u"),
            cosine: row["cosine"].as_f64().expect("cosine"),
            critical: row["criticalSpring"].as_f64().expect("criticalSpring"),
            pan: row["panSpring"].as_f64().expect("panSpring"),
        })
        .collect()
}

#[test]
fn fixture_has_the_full_101_sample_grid() {
    let vectors = load_vectors();
    assert_eq!(vectors.len(), 101);
    for (index, row) in vectors.iter().enumerate() {
        assert!(
            (row.u - index as f64 / 100.0).abs() < 1e-12,
            "u at row {index}"
        );
    }
}

#[test]
fn cosine_matches_screenright_vectors() {
    for row in load_vectors() {
        let got = round6(Easing::Cosine.apply(row.u));
        assert!(
            (got - row.cosine).abs() <= TOLERANCE,
            "cosine at u={}: got {got}, ScreenRight {}",
            row.u,
            row.cosine
        );
    }
}

#[test]
fn critical_camera_spring_matches_screenright_vectors() {
    for row in load_vectors() {
        let got = round6(Easing::CameraSpring { bounce: 0.0 }.apply(row.u));
        assert!(
            (got - row.critical).abs() <= TOLERANCE,
            "criticalSpring at u={}: got {got}, ScreenRight {}",
            row.u,
            row.critical
        );
    }
}

#[test]
fn pan_camera_spring_matches_screenright_vectors() {
    for row in load_vectors() {
        let got = round6(Easing::CameraSpring { bounce: 0.15 }.apply(row.u));
        assert!(
            (got - row.pan).abs() <= TOLERANCE,
            "panSpring (bounce 0.15) at u={}: got {got}, ScreenRight {}",
            row.u,
            row.pan
        );
    }
}
