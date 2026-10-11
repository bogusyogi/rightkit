//! Network-free tests for the shared Whisper route (rightkit-models 0.2.9).
//! No test downloads: the archive and bundle digests are checked as literal
//! values, and installs are simulated with a sparse file of the pinned size.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use rightkit_models::error::ErrorCode;
use rightkit_models::suite::catalog::{
    add_ons, all_models, catalog, whisper_turbo_ggml_spec, SpecSource, WHISPER_COREML_ARCHIVE,
    WHISPER_GGML_ARCHIVE_SHA256, WHISPER_GGML_FILE, WHISPER_GGML_FILE_SHA256,
    WHISPER_GGML_FILE_SIZE, WHISPER_IOS_ARCHIVE,
};
use rightkit_models::suite::manifest::{self, Manifest};
use rightkit_models::suite::paths;
use rightkit_models::suite::whisper::{
    ensure_whisper_model_at, resolve_whisper_model_at, WhisperKind,
};

fn is_lower_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn temp_models_root() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let models = paths::models_root_at(tmp.path());
    fs::create_dir_all(&models).unwrap();
    (tmp, models)
}

/// Simulate an installed ggml entry: a file of `bin_len` bytes plus the trusted
/// manifest the installer writes after verification.
fn install_fake_ggml(models: &Path, bin_len: u64) -> PathBuf {
    let spec = whisper_turbo_ggml_spec();
    let dir = paths::model_dir_at(models, spec.id);
    fs::create_dir_all(&dir).unwrap();
    File::create(dir.join(WHISPER_GGML_FILE))
        .unwrap()
        .set_len(bin_len)
        .unwrap();
    manifest::write_manifest(&dir, &spec.expected_manifest()).unwrap();
    dir
}

#[test]
fn ggml_catalog_entry_parses_and_is_in_the_catalog() {
    let spec = whisper_turbo_ggml_spec();
    assert_eq!(spec.id, "whisper-turbo");
    assert_eq!(spec.version, WHISPER_GGML_ARCHIVE_SHA256);
    assert!(spec.can_download());
    assert!(
        spec.requires_auth(),
        "Pro-gated archive must require a bearer"
    );

    let SpecSource::Archive {
        url,
        sha256,
        requires_auth,
        ..
    } = &spec.source
    else {
        panic!("whisper ggml must be an archive source");
    };
    assert_eq!(
        *url,
        Some("https://heardright-models.adrdsouza.workers.dev/whisper-win.tar.gz")
    );
    assert_eq!(*sha256, Some(WHISPER_GGML_ARCHIVE_SHA256));
    assert!(*requires_auth);

    // The manifest JSON parses back to the same trusted identity.
    let json = serde_json::to_string(&spec.expected_manifest()).unwrap();
    let parsed: Manifest = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.id, "whisper-turbo");
    assert_eq!(parsed.version, WHISPER_GGML_ARCHIVE_SHA256);
    assert_eq!(parsed.files.len(), 1);
    assert_eq!(parsed.files[0].path, WHISPER_GGML_FILE);
    assert_eq!(parsed.files[0].size, Some(WHISPER_GGML_FILE_SIZE));
    assert_eq!(
        parsed.files[0].sha256.as_deref(),
        Some(WHISPER_GGML_FILE_SHA256)
    );

    // An optional, entitlement-gated add-on: offered through add_ons(), never part of the
    // base catalog an install needs.
    assert!(!catalog().iter().any(|s| s.id == "whisper-turbo"));
    assert!(add_ons().iter().any(|s| s.id == "whisper-turbo"));
    assert!(all_models().iter().any(|s| s.id == "whisper-turbo"));
    assert!(spec.is_add_on() && spec.requires_entitlement() && !spec.requires_grant());
}

#[test]
fn whisper_digests_are_present_and_match_the_recorded_values() {
    // Literal values: a change here must be deliberate.
    assert_eq!(
        WHISPER_GGML_ARCHIVE_SHA256,
        "e4fa834a3b6000ca51b71ac42799277b33496e2f86c43e95b3da57b170f76f9f"
    );
    assert_eq!(
        WHISPER_GGML_FILE_SHA256,
        "fcacbe37c8ddd49a137b3ef7cccd2743bea1f9a9dd20d2b9755978972a02bb0b"
    );
    assert_eq!(WHISPER_GGML_FILE_SIZE, 574_041_195);
    assert_eq!(
        WHISPER_COREML_ARCHIVE.sha256,
        "046ce2aea772e2b102a31bfb52143b58b8ab97637dadc661d4723e4fd5a7f1d7"
    );
    assert_eq!(
        WHISPER_IOS_ARCHIVE.sha256,
        "a37fc293046bc9e77eafd592fc089bd75cb93079cda43bb1646461dee72de60c"
    );

    for digest in [
        WHISPER_GGML_ARCHIVE_SHA256,
        WHISPER_GGML_FILE_SHA256,
        WHISPER_COREML_ARCHIVE.sha256,
        WHISPER_IOS_ARCHIVE.sha256,
    ] {
        assert!(
            is_lower_hex64(digest),
            "{digest} is not a lowercase SHA-256"
        );
    }
    for pin in [WHISPER_COREML_ARCHIVE, WHISPER_IOS_ARCHIVE] {
        assert!(pin
            .url
            .starts_with("https://heardright-models.adrdsouza.workers.dev/"));
        assert_eq!(pin.directory_name, "whisper-multi");
        assert!(!pin.required_entries.is_empty());
    }

    // The ggml file itself is pinned by size and digest, so it is proven on install.
    let spec = whisper_turbo_ggml_spec();
    assert_eq!(spec.files.len(), 1);
    assert_eq!(spec.files[0].sha256, Some(WHISPER_GGML_FILE_SHA256));
    assert_eq!(spec.files[0].size, Some(WHISPER_GGML_FILE_SIZE));
}

#[test]
fn resolve_reports_missing_untrusted_and_ready_entries() {
    let (_tmp, models) = temp_models_root();
    let expected = paths::model_dir_at(&models, "whisper-turbo").join(WHISPER_GGML_FILE);

    // Missing: nothing installed yet.
    let err = resolve_whisper_model_at(&models, WhisperKind::Ggml).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(err.message.contains("not installed"), "{err}");

    // Wrong size: the manifest is trusted, the file is not.
    let dir = install_fake_ggml(&models, 1024);
    let err = resolve_whisper_model_at(&models, WhisperKind::Ggml).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(err.message.contains("files to repair"), "{err}");

    // Right size, no manifest: identity is not trusted, so not ready.
    fs::remove_file(paths::manifest_path(&dir)).unwrap();
    File::create(dir.join(WHISPER_GGML_FILE))
        .unwrap()
        .set_len(WHISPER_GGML_FILE_SIZE)
        .unwrap();
    let err = resolve_whisper_model_at(&models, WhisperKind::Ggml).unwrap_err();
    assert!(
        err.message.contains("does not match trusted revision"),
        "{err}"
    );

    // Trusted manifest and pinned size: ready, at the shared path.
    install_fake_ggml(&models, WHISPER_GGML_FILE_SIZE);
    let resolved = resolve_whisper_model_at(&models, WhisperKind::Ggml).unwrap();
    assert_eq!(resolved, expected);
    assert_eq!(
        resolved.file_name().and_then(|n| n.to_str()),
        Some(WHISPER_GGML_FILE)
    );
    assert_eq!(
        resolved
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str()),
        Some("whisper-turbo")
    );
    assert!(resolved.is_file());
}

#[test]
fn coreml_bundle_is_refused_until_per_file_pins_exist() {
    let (_tmp, models) = temp_models_root();

    let err = resolve_whisper_model_at(&models, WhisperKind::CoreMl).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(err.message.contains(WHISPER_COREML_ARCHIVE.sha256), "{err}");

    let err = ensure_whisper_model_at(
        &models,
        WhisperKind::CoreMl,
        "test-app",
        None,
        &|| false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);

    // Refusal creates nothing in the store.
    assert_eq!(fs::read_dir(&models).unwrap().count(), 0);
}

#[test]
fn ensure_without_an_entitlement_bearer_fails_before_any_network_call() {
    let (_tmp, models) = temp_models_root();
    let err = ensure_whisper_model_at(
        &models,
        WhisperKind::Ggml,
        "test-app",
        None,
        &|| false,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Download);
    assert!(err.message.contains("entitlement"), "{err}");
    assert_eq!(fs::read_dir(&models).unwrap().count(), 0);
}
