//! `provision_runtime_with` round-trips through `verify_runtime_manifest` using fixture archives
//! built here. No network and no ONNX Runtime is loaded.
#![cfg(feature = "provision")]

use std::io::Write;
use std::path::{Path, PathBuf};

use rightkit_ffmpeg::native::NativeCatalog;
use rightkit_onnx::provision::{provision_runtime_with, ProvisionError, ProvisionTarget};
use rightkit_onnx::verify_runtime_manifest;
use serde_json::json;
use sha2::{Digest, Sha256};

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn zip_with(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut out);
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in members {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    out.into_inner()
}

fn tgz_with(members: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    for (name, bytes) in members {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, name, *bytes).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

struct Fixture {
    catalog: NativeCatalog,
    archives: Vec<(String, Vec<u8>)>,
}

#[allow(clippy::too_many_arguments)]
fn record(id: &str, library: &str, os: &str, arch: &str, status: &str, file: &str, url: &str, member: &[u8], archive: &[u8], license: &str) -> serde_json::Value {
    json!({
        "id": id, "library": library, "version": "1.24.4", "os": os, "arch": arch,
        "status": status, "linkage": "dynamic-library", "artifactFileName": file,
        "sha256": sha(member),
        "archive": { "url": url, "sha256": sha(archive) },
        "sourceUrl": "https://example.test/source", "licenseSpdx": license,
        "noticeFiles": ["third-party/NOTICE.txt"],
        "verification": { "method": "file-sha256", "evidence": "fixture" }
    })
}

fn fixture(onnx_status: &str) -> Fixture {
    let win_onnx: &[u8] = b"fixture onnxruntime.dll";
    let win_dml: &[u8] = b"fixture DirectML.dll";
    let mac_lib: &[u8] = b"fixture libonnxruntime dylib";
    let win_onnx_zip = zip_with(&[
        ("runtimes/win-x64/native/onnxruntime.dll", win_onnx),
        ("runtimes/win-x64/native/other.dll", b"not wanted"),
    ]);
    let win_dml_zip = zip_with(&[("bin/x64-win/DirectML.dll", win_dml)]);
    let mac_tgz = tgz_with(&[
        ("onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib", mac_lib),
        ("onnxruntime-osx-arm64-1.24.4/include/onnxruntime_c_api.h", b"header"),
    ]);
    let libraries = vec![
        record("onnxruntime-fixture-windows-x64-directml", "onnxruntime", "windows", "x86_64", onnx_status,
            "onnxruntime.dll", "https://example.test/ort-directml.nupkg", win_onnx, &win_onnx_zip, "MIT"),
        record("directml-fixture-windows-x64", "directml", "windows", "x86_64", "pinned",
            "DirectML.dll", "https://example.test/directml.nupkg", win_dml, &win_dml_zip,
            "LicenseRef-Microsoft-DirectML"),
        record("onnxruntime-fixture-macos-arm64", "onnxruntime", "macos", "aarch64", "approved",
            "libonnxruntime.1.24.4.dylib", "https://example.test/onnxruntime-osx-arm64-1.24.4.tgz",
            mac_lib, &mac_tgz, "MIT"),
    ];
    let text = json!({ "schemaVersion": 1, "libraries": libraries }).to_string();
    Fixture {
        catalog: NativeCatalog::parse_native_only(&text).unwrap(),
        archives: vec![
            ("ort-directml.nupkg".into(), win_onnx_zip),
            ("directml.nupkg".into(), win_dml_zip),
            ("onnxruntime-osx-arm64-1.24.4.tgz".into(), mac_tgz),
        ],
    }
}

fn run(
    fixture: &Fixture,
    target: ProvisionTarget,
    dest: &Path,
    cache: &Path,
    fetches: &mut Vec<String>,
) -> Result<rightkit_onnx::provision::ProvisionOutcome, ProvisionError> {
    provision_runtime_with(&fixture.catalog, target, dest, cache, &mut |url, _sha, out| {
        fetches.push(url.to_owned());
        let name = url.rsplit('/').next().unwrap();
        let bytes = &fixture.archives.iter().find(|(n, _)| n == name).ok_or("unknown url")?.1;
        std::fs::write(out, bytes).map_err(|e| e.to_string())
    })
}

fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("runtime");
    let cache = root.path().join("cache");
    (root, dest, cache)
}

#[test]
fn windows_round_trips_through_verify_runtime_manifest() {
    let f = fixture("pinned");
    let (_root, dest, cache) = dirs();
    let mut fetches = Vec::new();
    let outcome = run(&f, ProvisionTarget::WindowsX64, &dest, &cache, &mut fetches).unwrap();
    assert!(!outcome.reused);
    assert_eq!(fetches.len(), 2);
    let verified = verify_runtime_manifest(&dest).unwrap();
    assert_eq!(verified, outcome.runtime);
    assert_eq!(verified.platform, "windows-x64");
    assert_eq!(verified.onnxruntime.variant, "directml");
    assert_eq!(verified.onnxruntime.catalog_row, "onnxruntime-fixture-windows-x64-directml");
    let mut paths: Vec<_> = verified.files.iter().map(|f| f.path.as_str()).collect();
    paths.sort();
    assert_eq!(paths, ["DirectML.dll", "onnxruntime.dll"]);
    assert!(!dest.join("other.dll").exists());
}

#[test]
fn macos_extracts_the_versioned_dylib_as_the_unversioned_name() {
    let f = fixture("pinned");
    let (_root, dest, cache) = dirs();
    let outcome = run(&f, ProvisionTarget::MacosArm64, &dest, &cache, &mut Vec::new()).unwrap();
    assert_eq!(outcome.runtime.platform, "macos-arm64");
    assert_eq!(outcome.runtime.files.len(), 1);
    assert_eq!(outcome.runtime.files[0].path, "libonnxruntime.dylib");
    assert_eq!(std::fs::read(dest.join("libonnxruntime.dylib")).unwrap(), b"fixture libonnxruntime dylib");
}

#[test]
fn second_run_is_idempotent_and_downloads_nothing() {
    let f = fixture("pinned");
    let (_root, dest, cache) = dirs();
    run(&f, ProvisionTarget::WindowsX64, &dest, &cache, &mut Vec::new()).unwrap();
    let mut fetches = Vec::new();
    let again = run(&f, ProvisionTarget::WindowsX64, &dest, &cache, &mut fetches).unwrap();
    assert!(again.reused);
    assert!(fetches.is_empty());
}

#[test]
fn a_corrupted_bundle_is_replaced_from_the_cached_archives() {
    let f = fixture("pinned");
    let (_root, dest, cache) = dirs();
    run(&f, ProvisionTarget::WindowsX64, &dest, &cache, &mut Vec::new()).unwrap();
    std::fs::write(dest.join("onnxruntime.dll"), b"tampered").unwrap();
    let mut fetches = Vec::new();
    let again = run(&f, ProvisionTarget::WindowsX64, &dest, &cache, &mut fetches).unwrap();
    assert!(!again.reused);
    assert!(fetches.is_empty(), "cached archives passed their digest");
    verify_runtime_manifest(&dest).unwrap();
}

#[test]
fn a_wrong_archive_digest_fails_before_extraction_and_leaves_no_bundle() {
    let mut f = fixture("pinned");
    f.archives[0].1.push(0); // the served bytes no longer match the catalog digest
    let (_root, dest, cache) = dirs();
    let error = run(&f, ProvisionTarget::WindowsX64, &dest, &cache, &mut Vec::new()).unwrap_err();
    assert!(matches!(error, ProvisionError::Digest { .. }), "{error}");
    assert!(!dest.exists());
}

#[test]
fn planned_rows_are_refused() {
    let f = fixture("planned");
    let (_root, dest, cache) = dirs();
    let mut fetches = Vec::new();
    let error = run(&f, ProvisionTarget::WindowsX64, &dest, &cache, &mut fetches).unwrap_err();
    assert!(matches!(error, ProvisionError::Row { .. } | ProvisionError::Catalog(_)), "{error}");
    assert!(fetches.is_empty());
}
