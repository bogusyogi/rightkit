//! Verifies the approved native-library catalog against real local artifacts
//! (binaries and lock files) wherever they exist on this machine, and checks
//! suite invariants of the embedded catalog.

mod common;

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use rightkit_ffmpeg::native::*;

/// The claude workspace root: three levels above `crates/<crate>` in this checkout layout.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
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
    // ONNX Runtime pins one version per OS (bogusyogi/rightkit#889): macOS universal2 1.30.0,
    // Windows DirectML 1.24.4.
    assert_eq!(
        catalog.suite_version(NativeLibrary::Onnxruntime),
        Some("1.30.0")
    );
    assert_eq!(
        catalog.suite_version_for(NativeLibrary::Onnxruntime, "macos"),
        Some("1.30.0")
    );
    assert_eq!(
        catalog.suite_version_for(NativeLibrary::Onnxruntime, "windows"),
        Some("1.24.4")
    );
    // FFmpeg is derived from the legacy catalog: same ids and digests.
    let legacy =
        rightkit_ffmpeg::ArtifactCatalog::parse(rightkit_ffmpeg::APPROVED_CATALOG_JSON).unwrap();
    // Only decode-variant ffmpeg records are derived; ffprobe and feature variants are
    // selected through the FFmpeg catalog by tool and variant.
    for a in legacy.artifacts.iter().filter(|a| {
        a.tool == rightkit_ffmpeg::Tool::Ffmpeg && a.variant == rightkit_ffmpeg::DECODE_VARIANT
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
        .any(|a| a.tool == rightkit_ffmpeg::Tool::Ffprobe));
    assert!(catalog
        .records(NativeLibrary::Ffmpeg)
        .all(|r| !r.id.starts_with("ffprobe") && !r.id.contains("export")));
}

#[test]
fn ffmpeg_licence_admits_only_toolchain_runtime_terms_after_lgpl() {
    let catalog = NativeCatalog::embedded().unwrap();
    // The Windows decode binary is a static MinGW-w64 build: its record names the runtime too.
    let windows = catalog
        .approved(NativeLibrary::Ffmpeg, "windows", "x86_64")
        .unwrap();
    assert_eq!(
        windows.license_spdx,
        "LGPL-2.1-or-later AND LicenseRef-MinGW-w64-runtime AND MIT AND BSD-3-Clause \
         AND GPL-3.0-or-later WITH GCC-exception-3.1"
    );
    windows.validate().unwrap();
    for arch in ["aarch64", "x86_64"] {
        let mac = catalog
            .approved(NativeLibrary::Ffmpeg, "macos", arch)
            .unwrap();
        assert_eq!(mac.license_spdx, "LGPL-2.1-or-later");
    }

    // Nothing but the toolchain runtime may follow FFmpeg's own licence.
    for refused in [
        "LGPL-2.1-or-later AND ISC",
        "LGPL-2.1-or-later AND GPL-3.0-or-later",
        "LGPL-2.1-or-later AND GPL-2.0-only WITH GCC-exception-2.0",
        "MIT AND LGPL-2.1-or-later",
        "GPL-3.0-or-later WITH GCC-exception-3.1",
        "MIT",
    ] {
        let mut record = windows.clone();
        record.license_spdx = refused.into();
        assert!(record.validate().is_err(), "{refused}");
    }
}

/// The one-version rule is per (library, OS): ONNX Runtime may differ between macOS and Windows,
/// never within one OS, and a stale record never counts.
#[test]
fn onnxruntime_versions_may_diverge_by_os_but_not_within_one() {
    let catalog = NativeCatalog::embedded().unwrap();
    catalog.validate().unwrap();

    let mut value: serde_json::Value = serde_json::from_str(NATIVE_CATALOG_JSON).unwrap();
    let libs = value["libraries"].as_array_mut().unwrap();
    // A second live Windows version is refused.
    let mut windows = libs
        .iter()
        .find(|r| r["id"] == "onnxruntime-1.24.4-windows-x64-directml")
        .unwrap()
        .clone();
    windows["id"] = "onnxruntime-9-windows".into();
    windows["version"] = "9.9.9".into();
    libs.push(windows.clone());
    let refused = NativeCatalog::parse_native_only(&value.to_string()).unwrap();
    assert!(matches!(
        refused.validate(),
        Err(NativeCatalogError::MixedVersions { ref versions, .. })
            if versions.len() == 2
    ));
    // The same record marked stale is history and is ignored.
    let libs = value["libraries"].as_array_mut().unwrap();
    libs.pop();
    windows["status"] = "stale-unverified".into();
    libs.push(windows);
    NativeCatalog::parse_native_only(&value.to_string())
        .unwrap()
        .validate()
        .unwrap();
}

/// The macOS ONNX Runtime pin is one universal2 record that resolves for both Apple architectures,
/// while the 1.24.4 thin records stay in the catalog as history and never resolve.
#[test]
fn the_macos_onnxruntime_pin_is_one_universal2_record_and_the_old_rows_are_stale() {
    let catalog = NativeCatalog::embedded().unwrap();
    for arch in ["aarch64", "x86_64"] {
        let record = catalog
            .approved(NativeLibrary::Onnxruntime, "macos", arch)
            .unwrap();
        assert_eq!(record.id, "onnxruntime-1.30.0-macos-universal2");
        assert_eq!(record.version, "1.30.0");
        assert_eq!(record.arch, "universal");
        assert_eq!(
            record.artifact_file_name.as_deref(),
            Some("libonnxruntime.dylib")
        );
        assert!(record
            .archive
            .as_ref()
            .unwrap()
            .url
            .ends_with("/v1.30.0/onnxruntime-osx-universal2-1.30.0.zip"));
        // Both the MIT text and the 1.30.0 third-party notices ship with the crate.
        assert!(record
            .notice_files
            .iter()
            .any(|n| n == "third-party/ONNXRuntime-1.30.0-ThirdPartyNotices.txt"));
    }
    for id in [
        "onnxruntime-1.24.4-macos-arm64",
        "onnxruntime-1.24.4-macos-x86_64",
    ] {
        let old = catalog
            .records(NativeLibrary::Onnxruntime)
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{id} is kept as history"));
        assert_eq!(old.status, NativeStatus::StaleUnverified, "{id}");
    }
}

/// An exact-architecture approved record wins over a universal one; two exact ones stay ambiguous.
#[test]
fn an_exact_arch_record_is_preferred_over_a_universal_one() {
    let mut value: serde_json::Value = serde_json::from_str(NATIVE_CATALOG_JSON).unwrap();
    let libs = value["libraries"].as_array_mut().unwrap();
    let mut thin = libs[0].clone();
    thin["id"] = "onnxruntime-1.30.0-macos-arm64".into();
    thin["arch"] = "aarch64".into();
    // The per-slice digests belong to the universal row only.
    thin.as_object_mut().unwrap().remove("contentSha256ByArch");
    libs.push(thin.clone());
    let catalog = NativeCatalog::parse_native_only(&value.to_string()).unwrap();
    assert_eq!(
        catalog
            .approved(NativeLibrary::Onnxruntime, "macos", "aarch64")
            .unwrap()
            .id,
        "onnxruntime-1.30.0-macos-arm64"
    );
    // x86_64 still resolves to the universal record.
    assert_eq!(
        catalog
            .approved(NativeLibrary::Onnxruntime, "macos", "x86_64")
            .unwrap()
            .id,
        "onnxruntime-1.30.0-macos-universal2"
    );
    // A second thin record for the same arch is ambiguous.
    thin["id"] = "onnxruntime-1.30.0-macos-arm64-b".into();
    value["libraries"].as_array_mut().unwrap().push(thin);
    let twice = NativeCatalog::parse_native_only(&value.to_string()).unwrap();
    assert!(matches!(
        twice.approved(NativeLibrary::Onnxruntime, "macos", "aarch64"),
        Err(NativeCatalogError::Ambiguous { .. })
    ));
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

/// DirectML's Microsoft licence is admitted for the DirectML library on Windows and nowhere else.
#[test]
fn the_directml_licence_is_scoped_to_directml_on_windows() {
    let catalog = NativeCatalog::embedded().unwrap();
    assert_eq!(
        catalog.suite_version(NativeLibrary::Directml),
        Some("1.15.4")
    );
    // Pinned, so it does not resolve as approved yet.
    assert!(matches!(
        catalog.approved(NativeLibrary::Directml, "windows", "x86_64"),
        Err(NativeCatalogError::NoApproved { .. })
    ));

    let refused = |id: &str, field: &str, value: &str| {
        let mut catalog: serde_json::Value = serde_json::from_str(NATIVE_CATALOG_JSON).unwrap();
        let record = catalog["libraries"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("{id} is in the catalog"));
        record[field] = value.into();
        let parsed = NativeCatalog::parse_native_only(&catalog.to_string()).unwrap();
        assert!(
            matches!(parsed.validate(), Err(NativeCatalogError::Invalid { .. })),
            "{id} with {field} = {value} must be refused"
        );
    };
    // Another library may not carry the licence.
    refused(
        "onnxruntime-1.24.4-windows-x64-directml",
        "licenseSpdx",
        DIRECTML_LICENSE,
    );
    // DirectML is not admitted on another OS, nor under another licence.
    refused("directml-1.15.4-windows-x64", "os", "macos");
    refused("directml-1.15.4-windows-x64", "licenseSpdx", "MIT");
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

/// Every row that can serve a dynamic library (approved or pinned) names its notices, and each
/// notice is a file this crate packages under `third-party/`.
#[test]
fn every_serving_dynamic_library_row_names_an_existing_notice_under_third_party() {
    let catalog = NativeCatalog::embedded().unwrap();
    catalog.validate().unwrap();
    let serving: Vec<&NativeRecord> = catalog
        .libraries
        .iter()
        .filter(|r| r.status.serves() && r.linkage == Linkage::DynamicLibrary)
        .collect();
    assert!(serving.iter().any(|r| r.status == NativeStatus::Pinned));
    assert!(serving.iter().any(|r| r.status == NativeStatus::Approved));
    for r in serving {
        assert!(!r.notice_files.is_empty(), "{} names no notice", r.id);
        for notice in &r.notice_files {
            assert!(
                notice.starts_with("third-party/"),
                "{}: {notice} is outside third-party/",
                r.id
            );
            assert!(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(notice).is_file(),
                "{}: {notice} is not in the crate",
                r.id
            );
        }
    }
}

/// The Windows rows nobody has loaded on a Windows host. Each is `pinned` (digests measured by a
/// session without a Windows host) or `planned` (no digest), carries `hostProof` `never-run`, and
/// none is `approved`. A new non-FFmpeg Windows row must be listed here or the test fails, so
/// its host proof is decided on purpose; promoting a row to `approved` means removing it.
const NEVER_RUN_WINDOWS_ROWS: [&str; 9] = [
    "onnxruntime-1.24.4-windows-x64-directml",
    "directml-1.15.4-windows-x64",
    "pdfium-chromium-8021-windows-x64",
    "whisper-cpp-1.9.3-windows-x64",
    "ggml-0.20.2-windows-x64",
    "ggml-base-0.20.2-windows-x64",
    "ggml-cpu-0.20.2-windows-x64",
    "ggml-vulkan-0.20.2-windows-x64",
    "sherpa-onnx-1.13.4-windows-x64",
];

#[test]
fn the_windows_rows_that_were_never_run_say_so_and_none_is_approved() {
    let catalog = NativeCatalog::embedded().unwrap();
    for id in NEVER_RUN_WINDOWS_ROWS {
        let row = catalog
            .libraries
            .iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{id} is not in the catalog"));
        assert_eq!(row.os, "windows", "{id}");
        assert_eq!(row.host_proof, Some(HostProof::NeverRun), "{id}");
        assert!(
            matches!(row.status, NativeStatus::Pinned | NativeStatus::Planned),
            "{id} is {:?}",
            row.status
        );
    }
    let windows_rows: BTreeSet<&str> = catalog
        .libraries
        .iter()
        .filter(|r| r.os == "windows" && r.library != NativeLibrary::Ffmpeg)
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(
        windows_rows,
        BTreeSet::from(NEVER_RUN_WINDOWS_ROWS),
        "a Windows row was added or removed without deciding its host proof"
    );
    // Only those rows are marked, and nothing marked is approved (the catalog validates that too).
    for row in catalog.libraries.iter().filter(|r| r.host_proof.is_some()) {
        assert!(
            NEVER_RUN_WINDOWS_ROWS.contains(&row.id.as_str()),
            "{}",
            row.id
        );
        assert_ne!(row.status, NativeStatus::Approved, "{}", row.id);
    }
}

/// A pinned row serves (loading it is how host proof gets produced); a planned or approved-less
/// one does not, and the plain `approved_file` lookup still refuses the pinned one.
#[test]
fn pinned_windows_rows_serve_but_planned_ones_never_resolve() {
    let catalog = NativeCatalog::embedded().unwrap();
    let pdfium = catalog
        .serving_file(NativeLibrary::Pdfium, "windows", "x86_64", "pdfium.dll")
        .unwrap();
    assert_eq!(pdfium.id, "pdfium-chromium-8021-windows-x64");
    assert_eq!(pdfium.status, NativeStatus::Pinned);
    assert!(pdfium.content_sha256.is_some());
    assert_eq!(
        pdfium.archive.as_ref().unwrap().member.as_deref(),
        Some("bin/pdfium.dll")
    );
    assert!(matches!(
        catalog.approved_file(NativeLibrary::Pdfium, "windows", "x86_64", "pdfium.dll"),
        Err(NativeCatalogError::NoApproved { .. })
    ));
    for (library, file) in [
        (NativeLibrary::WhisperCpp, "whisper.dll"),
        (NativeLibrary::Ggml, "ggml.dll"),
        (NativeLibrary::Ggml, "ggml-base.dll"),
        (NativeLibrary::Ggml, "ggml-cpu.dll"),
        (NativeLibrary::Ggml, "ggml-vulkan.dll"),
        (NativeLibrary::SherpaOnnx, "sherpa-onnx-c-api.dll"),
    ] {
        assert!(
            matches!(
                catalog.serving_file(library, "windows", "x86_64", file),
                Err(NativeCatalogError::NoApproved { .. })
            ),
            "{file}"
        );
    }
}

/// The macOS ONNX Runtime row pins each slice; the Windows one names its member and its
/// DirectML companion, which is in the catalog for the same OS.
#[test]
fn the_onnxruntime_rows_carry_their_slice_digests_member_and_companion() {
    let catalog = NativeCatalog::embedded().unwrap();
    let universal = catalog
        .approved(NativeLibrary::Onnxruntime, "macos", "aarch64")
        .unwrap();
    let by_arch = universal.content_sha256_by_arch.as_ref().unwrap();
    assert_eq!(
        by_arch.keys().map(String::as_str).collect::<Vec<_>>(),
        ["aarch64", "x86_64"]
    );
    assert!(universal.content_sha256.is_none());

    let windows = catalog
        .serving_file(
            NativeLibrary::Onnxruntime,
            "windows",
            "x86_64",
            "onnxruntime.dll",
        )
        .unwrap();
    assert_eq!(windows.companions, ["directml-1.15.4-windows-x64"]);
    assert_eq!(
        windows.archive.as_ref().unwrap().member.as_deref(),
        Some("runtimes/win-x64/native/onnxruntime.dll")
    );
    let directml = catalog
        .serving_file(NativeLibrary::Directml, "windows", "x86_64", "DirectML.dll")
        .unwrap();
    assert_eq!(directml.id, windows.companions[0]);
}

/// The verified-library API is reachable from the crate root, beside the `native` module path.
#[test]
fn the_verified_library_api_is_exported_at_the_crate_root() {
    let catalog = NativeCatalog::embedded().unwrap();
    let relative = Path::new("relative-library");
    let entries = [(NativeLibrary::WhisperCpp, "libwhisper.dylib")];
    let unsafe_path = |result: Result<(), NativeCatalogError>| {
        assert!(matches!(result, Err(NativeCatalogError::UnsafePath { .. })))
    };
    unsafe_path(rightkit_ffmpeg::verified_library(NativeLibrary::Pdfium, relative).map(drop));
    unsafe_path(
        rightkit_ffmpeg::verified_library_in(
            &catalog,
            "macos",
            "aarch64",
            NativeLibrary::Pdfium,
            relative,
        )
        .map(drop),
    );
    unsafe_path(rightkit_ffmpeg::verified_libraries(relative, &entries).map(drop));
    unsafe_path(
        rightkit_ffmpeg::verified_libraries_in(&catalog, "macos", "aarch64", relative, &entries)
            .map(drop),
    );
    let _: Option<(rightkit_ffmpeg::VerifiedLibrary, rightkit_ffmpeg::HostProof)> = None;
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

    let files: [(NativeLibrary, &str, &str, &str); 2] = [
        (
            NativeLibrary::Ffmpeg,
            "macos",
            "aarch64",
            "heardright/tauri-app-next/src-tauri/resources/runtime/ffmpeg",
        ),
        (
            NativeLibrary::SherpaOnnx,
            "macos",
            "aarch64",
            "cutright/runtime/packs/speech/bin/libsherpa-onnx-c-api.dylib",
        ),
    ];
    for (lib, os, arch, rel) in files {
        let path = root.join(rel);
        if !path.is_file() {
            common::skip_missing(&format!("{lib:?} artifact"), rel);
            continue;
        }
        catalog
            .approved(lib, os, arch)
            .unwrap()
            .verify_file(&path)
            .unwrap();
        verified.push(format!("{lib:?}"));
    }

    // ONNX Runtime 1.24.4 (macOS arm64) is kept as a stale history record; its cached files still
    // hash to it. The approved macOS record is 1.30.0 universal2 (no workstation copy is known).
    let old_ort = catalog
        .records(NativeLibrary::Onnxruntime)
        .find(|r| r.id == "onnxruntime-1.24.4-macos-arm64")
        .unwrap();
    let dylib = "genright/.cache/devin-completion-20261006/batch5/voice/dl/onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib";
    if root.join(dylib).is_file() {
        old_ort.verify_file(&root.join(dylib)).unwrap();
        verified.push("Onnxruntime".into());
    } else {
        common::skip_missing("Onnxruntime artifact", dylib);
    }
    // Upstream archive digest for ONNX Runtime 1.24.4.
    let tgz = root.join("genright/.cache/devin-completion-20261006/batch5/voice/dl/onnxruntime-osx-arm64-1.24.4.tgz");
    if tgz.is_file() {
        old_ort.verify_archive(&tgz).unwrap();
        verified.push("Onnxruntime-archive".into());
    }

    for (lib, lock) in [
        (NativeLibrary::SqliteVec, "mailright/src-tauri/Cargo.lock"),
        (NativeLibrary::Libgit2, "coderight/engine/Cargo.lock"),
    ] {
        let path = root.join(lock);
        if !path.is_file() {
            common::skip_missing(&format!("{lib:?} lock file"), lock);
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

    if verified.is_empty() {
        // Every artifact was reported as skipped above; nothing on this host to check.
        return;
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
            common::skip_missing(&format!("retained {file}"), &path.display().to_string());
            continue;
        }
        record.verify_file(&path).unwrap();
        assert_eq!(
            rightkit_ffmpeg::sha256_file(&path).unwrap(),
            record.sha256.clone().unwrap()
        );
        // A different file is refused by content.
        let other = dir.join("LICENSE-whisper.cpp");
        if other.is_file() {
            assert!(record.verify_file(&other).is_err());
        }
    }
}
