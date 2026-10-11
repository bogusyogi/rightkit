//! Catalog identity on real binaries: signing must not change it. The retained build
//! outputs live outside the repository; each test skips (loudly) when its input is absent.
//! Only ad-hoc signing is used (`codesign --sign -`): no signing identity, no notarisation.

mod common;

use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use rightkit_media::{
    content_identity_file, resolve_ffmpeg, verify_cache_clear, ContentFormat, ResolutionRequest,
};
use rightkit_media::{sha256_file, verify_content, ArtifactCatalog, MediaError, Tool};

const PUBLISH: &str = "/Volumes/D/rightkit-publish/artifacts";

fn retained(relative: &str) -> Option<PathBuf> {
    let path = Path::new(PUBLISH).join(relative);
    if path.is_file() {
        Some(path)
    } else {
        common::skip_missing(
            &format!("retained build output {relative}"),
            &path.display().to_string(),
        );
        None
    }
}

#[cfg(target_os = "macos")]
fn request(path: &Path) -> ResolutionRequest {
    ResolutionRequest {
        override_path: Some(path.to_owned()),
        bundled_paths: vec![],
        development_path: None,
        allow_development_path: false,
    }
}

#[cfg(target_os = "macos")]
fn codesign(args: &[&str], path: &Path) {
    let status = std::process::Command::new("/usr/bin/codesign")
        .args(args)
        .arg(path)
        .output()
        .unwrap();
    assert!(status.status.success(), "codesign {args:?}: {status:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn ad_hoc_signing_changes_the_file_hash_but_not_the_content_digest() {
    let Some(source) = retained("ffmpeg-7.1.5-lgpl-dav1d/macos-arm64/ffmpeg") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("ffmpeg");
    std::fs::copy(&source, &copy).unwrap();
    let before = content_identity_file(&copy).unwrap();
    let whole_before = sha256_file(&copy).unwrap();
    assert_eq!(before.format, ContentFormat::MachO);

    codesign(&["--force", "--sign", "-"], &copy);
    let signed = content_identity_file(&copy).unwrap();
    assert_ne!(
        sha256_file(&copy).unwrap(),
        whole_before,
        "signing rewrote the file"
    );
    assert_eq!(signed, before, "content digest is signature-independent");

    // Strip it again and sign a second time with a different identifier: still the same.
    codesign(&["--remove-signature"], &copy);
    assert_eq!(content_identity_file(&copy).unwrap(), before);
    codesign(
        &[
            "--force",
            "--sign",
            "-",
            "--identifier",
            "org.example.other",
        ],
        &copy,
    );
    assert_eq!(content_identity_file(&copy).unwrap(), before);
}

#[cfg(target_os = "macos")]
#[test]
fn a_universal_file_verifies_by_slice_against_the_per_arch_rows() {
    let Some(universal) = retained("ffmpeg-7.1.5-lgpl-dav1d/macos-universal/ffmpeg") else {
        return;
    };
    let catalog = ArtifactCatalog::embedded().unwrap();
    let arm = catalog
        .approved_for(Tool::Ffmpeg, "decode-av1", "macos", "aarch64")
        .unwrap();
    let intel = catalog
        .approved_for(Tool::Ffmpeg, "decode-av1", "macos", "x86_64")
        .unwrap();
    let identity = content_identity_file(&universal).unwrap();
    assert_eq!(identity.format, ContentFormat::MachOUniversal);
    assert_eq!(
        identity.digest_for("aarch64"),
        arm.content_sha256.as_deref()
    );
    assert_eq!(
        identity.digest_for("x86_64"),
        intel.content_sha256.as_deref()
    );
    // The whole universal file matches neither row's retained-file hash...
    let whole = sha256_file(&universal).unwrap();
    assert_ne!(Some(whole.as_str()), arm.sha256.as_deref());
    assert_ne!(Some(whole.as_str()), intel.sha256.as_deref());
    // ...but verifies for either architecture, signed or not.
    verify_content(arm, &universal).unwrap();
    verify_content(intel, &universal).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let signed = dir.path().join("ffmpeg");
    std::fs::copy(&universal, &signed).unwrap();
    codesign(&["--force", "--sign", "-"], &signed);
    verify_cache_clear();
    resolve_ffmpeg(arm, &request(&signed)).unwrap();
    resolve_ffmpeg(intel, &request(&signed)).unwrap();
    // Another variant's rows (different build) do not accept it.
    let decode = catalog.approved("macos", "aarch64").unwrap();
    assert!(matches!(
        verify_content(decode, &signed),
        Err(MediaError::DigestMismatch { .. })
    ));
}

#[test]
fn a_universal_file_with_a_slice_that_has_no_approved_row_is_refused() {
    // The retained ffprobe file holds arm64 and x86_64 slices. Both have rows, so it passes.
    let Some(universal) = retained("ffprobe-7.1.5-lgpl/macos-arm64/ffprobe") else {
        return;
    };
    let catalog = ArtifactCatalog::embedded().unwrap();
    let arm = catalog
        .approved_for(Tool::Ffprobe, "decode-apple", "macos", "aarch64")
        .unwrap();
    verify_content(arm, &universal).unwrap();
    // A row of another version has the same arm64 digest but no x86_64 sibling, so the
    // x86_64 slice is unapproved and the whole file is refused.
    let mut lone = arm.clone();
    lone.version = "9.9.9".into();
    assert!(matches!(
        verify_content(&lone, &universal),
        Err(MediaError::DigestMismatch { .. })
    ));
    // The plain `decode` ffprobe rows describe other builds: no cross-variant acceptance.
    let decode = catalog
        .approved_for(Tool::Ffprobe, "decode", "macos", "aarch64")
        .unwrap();
    assert!(verify_content(decode, &universal).is_err());
}

#[test]
fn embedded_rows_match_the_retained_bytes_they_were_taken_from() {
    let catalog = ArtifactCatalog::embedded().unwrap();
    for (id, relative) in [
        (
            "ffmpeg-7.1.5-decode-av1-macos-arm64",
            "ffmpeg-7.1.5-lgpl-dav1d/macos-arm64/ffmpeg",
        ),
        (
            "ffmpeg-7.1.5-decode-av1-macos-x86_64",
            "ffmpeg-7.1.5-lgpl-dav1d/macos-x86_64/ffmpeg",
        ),
        (
            "ffmpeg-7.1.5-export-ass-macos-arm64",
            "ffmpeg-7.1.5-export-ass/macos-arm64/ffmpeg",
        ),
        (
            "ffprobe-7.1.5-decode-apple-macos-arm64",
            "ffprobe-7.1.5-lgpl/macos-arm64/ffprobe",
        ),
        (
            "ffprobe-7.1.5-decode-apple-macos-x86_64",
            "ffprobe-7.1.5-lgpl/macos-arm64/ffprobe",
        ),
    ] {
        let Some(path) = retained(relative) else {
            continue;
        };
        let row = catalog.by_id(id).unwrap();
        verify_content(row, &path).unwrap_or_else(|e| panic!("{id}: {e}"));
    }
    for (id, relative) in [
        ("ffmpeg-7.1.5-macos-arm64", "ffmpeg-macos-arm64"),
        ("ffmpeg-7.1.5-macos-x86_64", "ffmpeg-macos-x86_64"),
        ("ffmpeg-7.1.5-windows-x64", "ffmpeg-win-x64.exe"),
        ("ffprobe-7.1.5-macos-arm64", "ffprobe-decode-macos-arm64"),
        ("ffprobe-7.1.5-macos-x86_64", "ffprobe-decode-macos-x86_64"),
    ] {
        let path = common::vendor_dir().join(relative);
        if !path.is_file() {
            common::skip_missing(
                &format!("vendor artifact {relative}"),
                &path.display().to_string(),
            );
            continue;
        }
        let row = catalog.by_id(id).unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            row.sha256.clone().unwrap(),
            "{id}: retained bytes drifted from the provenance digest"
        );
        verify_content(row, &path).unwrap_or_else(|e| panic!("{id}: {e}"));
    }
}
