//! EmbeddingGemma 2 E43 archive route and whole-archive pin (Orthic-Labs/rightkit#224).
//! The real archive digest is not in the repository, so the shipped pins are `None`; the
//! spec builder is exercised with synthetic, clearly fake pins that never leave this file.

use rightkit_models::suite::catalog::{
    add_ons, all_models, base_catalog, embedding_gemma2_e43_dml_spec,
    embedding_gemma2_e43_dml_spec_with, EmbeddingGemma2E43Pins, MissingE43Pins, SpecFile,
    SpecSource, EMBEDDINGGEMMA2_E43_DML_ID, EMBEDDINGGEMMA2_E43_DML_PINS,
    EMBEDDINGGEMMA2_E43_DML_REVISION, EMBEDDINGGEMMA2_E43_DML_ROUTE, EMBEDDINGGEMMA2_E43_DML_URL,
    MODEL_DOWNLOAD_BASE,
};

const FAKE_ARCHIVE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FAKE_FILE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

const FAKE_FILES: &[SpecFile] = &[SpecFile {
    path: "model_32.onnx",
    sha256: Some(FAKE_FILE),
    size: Some(10),
}];

const UNPINNED_FILES: &[SpecFile] = &[SpecFile {
    path: "model_32.onnx",
    sha256: None,
    size: Some(10),
}];

fn pins(archive_sha256: &'static str, files: &'static [SpecFile]) -> EmbeddingGemma2E43Pins {
    EmbeddingGemma2E43Pins {
        archive_sha256,
        archive_size: 1_000,
        files,
        installed_size_bytes: 10,
    }
}

#[test]
fn the_route_is_the_worker_archive_path() {
    assert_eq!(
        EMBEDDINGGEMMA2_E43_DML_ROUTE,
        format!("/{EMBEDDINGGEMMA2_E43_DML_ID}/{EMBEDDINGGEMMA2_E43_DML_REVISION}/{EMBEDDINGGEMMA2_E43_DML_ID}.tar.gz")
    );
    assert_eq!(
        EMBEDDINGGEMMA2_E43_DML_URL,
        format!("{MODEL_DOWNLOAD_BASE}{EMBEDDINGGEMMA2_E43_DML_ROUTE}")
    );
}

#[test]
fn no_digest_is_invented_while_the_real_pins_are_absent() {
    assert!(EMBEDDINGGEMMA2_E43_DML_PINS.is_none());
    assert_eq!(embedding_gemma2_e43_dml_spec().unwrap_err(), MissingE43Pins);
}

#[test]
fn explicit_pins_make_a_whole_archive_spec() {
    let spec = embedding_gemma2_e43_dml_spec_with(pins(FAKE_ARCHIVE, FAKE_FILES)).unwrap();
    assert_eq!(spec.id, EMBEDDINGGEMMA2_E43_DML_ID);
    assert!(spec.can_download());
    assert!(!spec.requires_auth() && !spec.requires_grant() && !spec.requires_entitlement());
    let SpecSource::Archive {
        url,
        sha256,
        size,
        requires_auth,
    } = &spec.source
    else {
        panic!("E43 must be one archive");
    };
    assert_eq!(*url, Some(EMBEDDINGGEMMA2_E43_DML_URL));
    assert_eq!(*sha256, Some(FAKE_ARCHIVE));
    assert_eq!(*size, Some(1_000));
    assert!(!requires_auth);
    assert_eq!(spec.expected_manifest().files.len(), FAKE_FILES.len());
}

#[test]
fn malformed_pins_are_refused() {
    assert!(embedding_gemma2_e43_dml_spec_with(pins("abc", FAKE_FILES)).is_err());
    assert!(embedding_gemma2_e43_dml_spec_with(pins(
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        FAKE_FILES
    ))
    .is_err());
    assert!(embedding_gemma2_e43_dml_spec_with(pins(FAKE_ARCHIVE, UNPINNED_FILES)).is_err());
    assert!(embedding_gemma2_e43_dml_spec_with(pins(FAKE_ARCHIVE, &[])).is_err());
    let mut zero = pins(FAKE_ARCHIVE, FAKE_FILES);
    zero.archive_size = 0;
    assert!(embedding_gemma2_e43_dml_spec_with(zero).is_err());
}

#[test]
fn e43_is_not_in_the_base_or_add_on_lists() {
    assert!(!base_catalog()
        .iter()
        .any(|s| s.id == EMBEDDINGGEMMA2_E43_DML_ID));
    assert!(!add_ons().iter().any(|s| s.id == EMBEDDINGGEMMA2_E43_DML_ID));
    assert!(!all_models()
        .iter()
        .any(|s| s.id == EMBEDDINGGEMMA2_E43_DML_ID));
}
