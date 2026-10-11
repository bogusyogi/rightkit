//! Verifies the approved native-library catalog against real local artifacts
//! (binaries and lock files) wherever they exist on this machine, and checks
//! suite invariants of the embedded catalog.

use std::path::{Path, PathBuf};

use rightkit_media::native::*;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn embedded_catalog_pins_one_version_per_library_and_covers_all_eight_libraries() {
    let catalog = NativeCatalog::embedded().unwrap();
    for library in [
        NativeLibrary::Ffmpeg,
        NativeLibrary::Onnxruntime,
        NativeLibrary::Pdfium,
        NativeLibrary::SqliteVec,
        NativeLibrary::Libgit2,
        NativeLibrary::Ggml,
        NativeLibrary::WhisperCpp,
        NativeLibrary::SherpaOnnx,
    ] {
        assert!(
            catalog.suite_version(library).is_some(),
            "{library:?} missing"
        );
    }
    assert_eq!(catalog.suite_version(NativeLibrary::Ffmpeg), Some("7.1.5"));
    assert_eq!(
        catalog.suite_version(NativeLibrary::Onnxruntime),
        Some("1.24.4")
    );
    // FFmpeg is derived from the legacy catalog: same ids and digests.
    let legacy =
        rightkit_media::ArtifactCatalog::parse(rightkit_media::APPROVED_CATALOG_JSON).unwrap();
    // Only decode-variant ffmpeg records are derived; ffprobe and feature variants are
    // selected through the FFmpeg catalog by tool and variant.
    for a in legacy.artifacts.iter().filter(|a| {
        a.tool == rightkit_media::Tool::Ffmpeg && a.variant == rightkit_media::DECODE_VARIANT
    }) {
        let r = catalog
            .approved(NativeLibrary::Ffmpeg, &a.os, &a.arch)
            .unwrap();
        assert_eq!(
            (r.id.as_str(), r.sha256.as_deref()),
            (a.id.as_str(), a.sha256.as_deref())
        );
    }
    assert!(legacy
        .artifacts
        .iter()
        .any(|a| a.tool == rightkit_media::Tool::Ffprobe));
    assert!(catalog
        .records(NativeLibrary::Ffmpeg)
        .all(|r| !r.id.starts_with("ffprobe") && !r.id.contains("export")));
}

#[test]
fn mixed_versions_gpl_and_unverifiable_approvals_fail_closed() {
    let mut value: serde_json::Value = serde_json::from_str(NATIVE_CATALOG_JSON).unwrap();
    let libs = value["libraries"].as_array_mut().unwrap();
    let mut second = libs[0].clone();
    second["id"] = "onnxruntime-9-any".into();
    second["version"] = "9.9.9".into();
    libs.push(second);
    let mixed = NativeCatalog::parse_native_only(&value.to_string()).unwrap();
    assert!(matches!(
        mixed.validate(),
        Err(NativeCatalogError::MixedVersions { .. })
    ));

    let mut value: serde_json::Value = serde_json::from_str(NATIVE_CATALOG_JSON).unwrap();
    value["libraries"][0]["licenseSpdx"] = "GPL-3.0-only".into();
    let gpl = NativeCatalog::parse_native_only(&value.to_string()).unwrap();
    assert!(matches!(
        gpl.validate(),
        Err(NativeCatalogError::Invalid { .. })
    ));

    let mut value: serde_json::Value = serde_json::from_str(NATIVE_CATALOG_JSON).unwrap();
    value["libraries"][0]
        .as_object_mut()
        .unwrap()
        .remove("sha256");
    let weak = NativeCatalog::parse_native_only(&value.to_string()).unwrap();
    assert!(weak.validate().is_err());

    let mut value: serde_json::Value = serde_json::from_str(NATIVE_CATALOG_JSON).unwrap();
    value["libraries"][0]["unreviewed"] = true.into();
    assert!(NativeCatalog::parse_native_only(&value.to_string()).is_err());
}

#[test]
fn planned_and_pinned_records_never_resolve_as_approved() {
    let catalog = NativeCatalog::embedded().unwrap();
    assert!(matches!(
        catalog.approved(NativeLibrary::Onnxruntime, "windows", "x86_64"),
        Err(NativeCatalogError::NoApproved { .. })
    ));
    assert!(matches!(
        catalog.approved(NativeLibrary::Pdfium, "macos", "aarch64"),
        Err(NativeCatalogError::NoApproved { .. })
    ));
}

#[test]
fn every_notice_file_is_packaged_with_the_crate() {
    let catalog = NativeCatalog::embedded().unwrap();
    for r in &catalog.libraries {
        for n in &r.notice_files {
            assert!(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(n).is_file(),
                "{} missing {n}",
                r.id
            );
        }
    }
}

/// Real artifacts present on this machine must hash to their approved digests.
/// Reads ignored binaries in sibling app checkouts, so it audits workstation
/// state rather than this crate; run it explicitly with `--ignored`.
#[test]
#[ignore = "audits app checkout binaries outside this crate"]
fn real_local_artifacts_and_locks_match_their_catalog_records() {
    let catalog = NativeCatalog::embedded().unwrap();
    let root = root();
    let mut verified = Vec::new();

    let files: [(NativeLibrary, &str, &str, &str); 3] = [
        (NativeLibrary::Ffmpeg, "macos", "aarch64", "heardright/tauri-app-next/src-tauri/resources/runtime/ffmpeg"),
        (NativeLibrary::Onnxruntime, "macos", "aarch64", "genright/.cache/devin-completion-20261006/batch5/voice/dl/onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib"),
        (NativeLibrary::SherpaOnnx, "macos", "aarch64", "cutright/runtime/packs/speech/bin/libsherpa-onnx-c-api.dylib"),
    ];
    for (lib, os, arch, rel) in files {
        let path = root.join(rel);
        if !path.is_file() {
            eprintln!("SKIP {lib:?}: {rel} not present");
            continue;
        }
        catalog
            .approved(lib, os, arch)
            .unwrap()
            .verify_file(&path)
            .unwrap();
        verified.push(format!("{lib:?}"));
    }

    // Upstream archive digest for ONNX Runtime.
    let tgz = root.join("genright/.cache/devin-completion-20261006/batch5/voice/dl/onnxruntime-osx-arm64-1.24.4.tgz");
    if tgz.is_file() {
        catalog
            .approved(NativeLibrary::Onnxruntime, "macos", "aarch64")
            .unwrap()
            .verify_archive(&tgz)
            .unwrap();
        verified.push("Onnxruntime-archive".into());
    }

    for (lib, lock) in [
        (NativeLibrary::SqliteVec, "mailright/src-tauri/Cargo.lock"),
        (NativeLibrary::Libgit2, "coderight/engine/Cargo.lock"),
    ] {
        let path = root.join(lock);
        if !path.is_file() {
            eprintln!("SKIP {lib:?}: {lock} not present");
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();
        catalog
            .approved(lib, "macos", "aarch64")
            .unwrap()
            .verify_cargo_lock(&text)
            .unwrap();
        verified.push(format!("{lib:?}"));
    }

    eprintln!("verified against real artifacts: {verified:?}");
    // The approved artifacts are macOS builds; elsewhere only the platform-neutral Cargo.lock
    // pins (sqlite-vec, libgit2) are guaranteed to be checkable.
    let required = if cfg!(target_os = "macos") { 4 } else { 2 };
    assert!(
        verified.len() >= required,
        "too few real artifacts present: {verified:?}"
    );

    // A wrong file must be rejected by content, not by name.
    let wrong = std::env::temp_dir().join(format!("rk-native-wrong-{}", std::process::id()));
    std::fs::write(&wrong, b"not onnxruntime").unwrap();
    assert!(matches!(
        catalog
            .approved(NativeLibrary::Onnxruntime, "macos", "aarch64")
            .unwrap()
            .verify_file(&wrong),
        Err(NativeCatalogError::DigestMismatch { .. })
    ));
    std::fs::remove_file(wrong).unwrap();
}

#[test]
fn whisper_and_ggml_dylibs_are_catalogued_by_file_and_verify_by_content() {
    let catalog = NativeCatalog::embedded().unwrap();
    assert!(matches!(
        catalog.approved(NativeLibrary::Ggml, "macos", "aarch64"),
        Err(NativeCatalogError::Ambiguous { .. })
    ));
    let dir =
        std::path::Path::new("/Volumes/D/rightkit-publish/artifacts/whisper-cpp-1.9.3/macos-arm64");
    for (lib, file) in [
        (NativeLibrary::WhisperCpp, "libwhisper.1.9.3.dylib"),
        (NativeLibrary::Ggml, "libggml.0.20.2.dylib"),
        (NativeLibrary::Ggml, "libggml-base.0.20.2.dylib"),
        (NativeLibrary::Ggml, "libggml-cpu.0.20.2.dylib"),
        (NativeLibrary::Ggml, "libggml-metal.0.20.2.dylib"),
        (NativeLibrary::Ggml, "libggml-blas.0.20.2.dylib"),
    ] {
        let record = catalog
            .approved_file(lib, "macos", "aarch64", file)
            .unwrap();
        assert!(
            record.sha256.is_some() && record.content_sha256.is_some(),
            "{file}"
        );
        let path = dir.join(file);
        if !path.is_file() {
            eprintln!("SKIPPED: {} not retained", path.display());
            continue;
        }
        record.verify_file(&path).unwrap();
        assert_eq!(
            rightkit_media::sha256_file(&path).unwrap(),
            record.sha256.clone().unwrap()
        );
        // A different file is refused by content.
        let other = dir.join("LICENSE-whisper.cpp");
        if other.is_file() {
            assert!(record.verify_file(&other).is_err());
        }
    }
}
