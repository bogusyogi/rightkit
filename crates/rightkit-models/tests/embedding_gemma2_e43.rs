//! EmbeddingGemma 2 E43 archive route and whole-archive pin (Orthic-Labs/rightkit#224).
//! The per-file pins and the archive pins are final (sealed 2026-10-22). The spec builder is
//! also exercised with synthetic, clearly fake archive values that
//! never leave this file.

use std::collections::BTreeSet;

use rightkit_models::suite::catalog::{
    add_ons, all_models, base_catalog, embedding_gemma2_e43_dml_spec,
    embedding_gemma2_e43_dml_spec_with, EmbeddingGemma2E43Pins, MissingE43Pins, SpecFile,
    SpecSource, EMBEDDINGGEMMA2_E43_DML_FILES, EMBEDDINGGEMMA2_E43_DML_ID,
    EMBEDDINGGEMMA2_E43_DML_INSTALLED_SIZE, EMBEDDINGGEMMA2_E43_DML_PINS,
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
fn the_route_is_the_gzip_archive_and_missing_pins_stay_a_typed_error() {
    assert!(EMBEDDINGGEMMA2_E43_DML_ROUTE.ends_with(".tar.gz"));
    // The builder still refuses a partial pin set with the typed error.
    assert_eq!(
        embedding_gemma2_e43_dml_spec_with(pins(FAKE_ARCHIVE, &[])).unwrap_err(),
        MissingE43Pins
    );
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

const FINAL_INSTALLED_SIZE: u64 = 494_393_592;

/// The 21 files `rightkit-embed` 0.6.1 reads for the E43 layout (the schema-2 bundle manifest as
/// `embed-manifest.json`, ten query graphs, six batch-4/8 ingest graphs, the shared weights, the
/// int8 table with its scales, and the tokenizer), written out independently of the catalog.
fn embed_layout() -> BTreeSet<String> {
    let mut files: BTreeSet<String> = [
        "embed-manifest.json",
        "model.onnx_data",
        "embed_tokens.i8.npy",
        "embed_scales.f16.npy",
        "tokenizer.json",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    for length in [16, 32, 48, 64, 96, 128, 192, 256, 384, 512] {
        files.insert(format!("model_{length}.onnx"));
    }
    for batch in [4, 8] {
        for length in [32, 128, 512] {
            files.insert(format!("ingest-b{batch}_{length}.onnx"));
        }
    }
    files
}

/// The per-file table is final: 21 rows, both pins each, the embed layout, the owner's sizes.
#[test]
fn the_per_file_pins_are_final_and_match_the_embed_layout() {
    let files = EMBEDDINGGEMMA2_E43_DML_FILES;
    assert_eq!(files.len(), 21);
    assert_eq!(EMBEDDINGGEMMA2_E43_DML_INSTALLED_SIZE, FINAL_INSTALLED_SIZE);
    assert_eq!(
        files.iter().filter_map(|f| f.size).sum::<u64>(),
        FINAL_INSTALLED_SIZE
    );
    for f in files {
        let sha = f.sha256.unwrap_or_else(|| panic!("{} unpinned", f.path));
        assert!(
            sha.len() == 64 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "{}",
            f.path
        );
        assert!(f.size.is_some_and(|n| n > 0), "{}", f.path);
    }
    let listed: BTreeSet<String> = files.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(listed.len(), files.len(), "no duplicate paths");
    assert_eq!(listed, embed_layout());
    // The store's own record is `manifest.json`; the bundle manifest must not share that name.
    assert!(!listed.contains("manifest.json"));
    let find = |name: &str| files.iter().find(|f| f.path == name).unwrap();
    // Same bytes as the earlier `manifest.json`, renamed.
    assert_eq!(find("embed-manifest.json").size, Some(1_041));
    assert_eq!(
        find("embed-manifest.json").sha256,
        Some("6b6c02a1d78c006d9632710432b34bf7a853a1a9b9d8874677c799c462c7c509")
    );
    assert_eq!(find("tokenizer.json").size, Some(32_170_510));
    assert_eq!(
        find("tokenizer.json").sha256,
        Some("4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4")
    );
    assert_eq!(find("model.onnx_data").size, Some(302_458_112));
    assert_eq!(find("embed_tokens.i8.npy").size, Some(134_217_856));
}

/// The real file table builds a spec once an archive digest and size are supplied; here with
/// fake archive values that never leave this file.
#[test]
fn the_real_file_table_builds_a_spec_with_explicit_archive_values() {
    let spec = embedding_gemma2_e43_dml_spec_with(EmbeddingGemma2E43Pins {
        archive_sha256: FAKE_ARCHIVE,
        archive_size: 1_000,
        files: EMBEDDINGGEMMA2_E43_DML_FILES,
        installed_size_bytes: EMBEDDINGGEMMA2_E43_DML_INSTALLED_SIZE,
    })
    .unwrap();
    assert_eq!(spec.files.len(), 21);
    assert_eq!(spec.size_hint_bytes, FINAL_INSTALLED_SIZE);
    assert!(spec.can_download() && !spec.requires_auth());
}

/// The shipped constant is either complete (every pin) or `None` (the spec fails closed);
/// closed, or (once pasted) it is the whole table with a well-formed digest and size.
#[test]
fn the_archive_digest_and_size_are_pending_or_complete_never_partial() {
    match EMBEDDINGGEMMA2_E43_DML_PINS {
        None => assert_eq!(embedding_gemma2_e43_dml_spec().unwrap_err(), MissingE43Pins),
        Some(pins) => {
            assert!(
                pins.archive_sha256.len() == 64
                    && pins
                        .archive_sha256
                        .bytes()
                        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            );
            assert!(pins.archive_size > 0);
            assert_eq!(pins.files.len(), 21);
            assert_eq!(pins.installed_size_bytes, FINAL_INSTALLED_SIZE);
            assert!(embedding_gemma2_e43_dml_spec().is_ok());
            // The superseded archive digests are not the shipped one.
            assert_ne!(
                pins.archive_sha256,
                "c8ad75d9abb69fa87fdee585655d8576d52c7ff52f3e7b8fbab90322e2b3af4d"
            );
            assert_ne!(
                pins.archive_sha256,
                "54f32f8e1ddc2fc535d9da95351aa4f11d56e750d04b230ba94c962e15ac63e0"
            );
        }
    }
}
