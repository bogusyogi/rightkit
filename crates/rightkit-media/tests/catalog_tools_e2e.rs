//! Real catalog binaries (ffmpeg + ffprobe) through resolve -> verify -> externalBin
//! staging, and the development-only unverified path against a real host FFmpeg.
//! Artifacts live in the workspace `vendor/ffmpeg-lgpl/`; a missing artifact is
//! reported as SKIP on stderr rather than silently passing.

mod common;

use std::{path::PathBuf, time::Duration};

use rightkit_media::{
    resolve_ffmpeg, resolve_ffmpeg_or_unverified, resolve_unverified, sha256_file,
    stage_external_bin, tauri_target_triple, verify_ffmpeg, verify_ffprobe, AnyFfmpeg,
    ArtifactCatalog, BuildMode, ConfigLicense, MediaError, ResolutionRequest, Tool,
    UnverifiedRequest, UnverifiedSource, DECODE_VARIANT,
};

const T: Duration = Duration::from_secs(20);

fn vendor(name: &str) -> PathBuf {
    common::vendor_dir().join(name)
}

fn host() -> (&'static str, &'static str) {
    (
        match std::env::consts::OS {
            "macos" => "macos",
            "windows" => "windows",
            other => other,
        },
        std::env::consts::ARCH,
    )
}

fn vendor_file(tool: Tool, os: &str, arch: &str) -> PathBuf {
    match (tool, os, arch) {
        (Tool::Ffmpeg, "windows", _) => vendor("ffmpeg-win-x64.exe"),
        (Tool::Ffmpeg, "macos", "aarch64") => vendor("ffmpeg-macos-arm64"),
        (Tool::Ffmpeg, _, _) => vendor("ffmpeg-macos-x86_64"),
        (Tool::Ffprobe, "macos", "aarch64") => vendor("ffprobe-decode-macos-arm64"),
        (Tool::Ffprobe, _, _) => vendor("ffprobe-decode-macos-x86_64"),
    }
}

fn request(path: PathBuf) -> ResolutionRequest {
    ResolutionRequest {
        override_path: None,
        bundled_paths: vec![path],
        development_path: None,
        allow_development_path: false,
    }
}

#[test]
fn every_approved_macos_artifact_present_in_vendor_matches_its_catalog_digest() {
    let catalog = ArtifactCatalog::embedded().unwrap();
    let mut checked = 0;
    for tool in [Tool::Ffmpeg, Tool::Ffprobe] {
        for arch in ["aarch64", "x86_64"] {
            let artifact = catalog
                .approved_for(tool, DECODE_VARIANT, "macos", arch)
                .unwrap();
            let path = vendor_file(tool, "macos", arch);
            if !path.is_file() {
                common::skip_missing(
                    &format!("approved artifact {}", artifact.id),
                    &path.display().to_string(),
                );
                continue;
            }
            assert_eq!(
                Some(sha256_file(&path).unwrap()),
                artifact.sha256.clone(),
                "{}",
                artifact.id
            );
            checked += 1;
        }
    }
    eprintln!("verified {checked} vendor artifacts against the catalog");
}

#[test]
fn host_ffmpeg_and_ffprobe_verify_and_stage_as_tauri_external_bins() {
    let (os, arch) = host();
    let catalog = ArtifactCatalog::embedded().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let binaries = dir.path().join("src-tauri").join("binaries");
    let triple = tauri_target_triple(os, arch).unwrap();
    for tool in [Tool::Ffmpeg, Tool::Ffprobe] {
        let Ok(artifact) = catalog.approved_for(tool, DECODE_VARIANT, os, arch) else {
            eprintln!("SKIP {tool}: no approved {os}/{arch} artifact");
            continue;
        };
        let path = vendor_file(tool, os, arch);
        if !path.is_file() {
            common::skip_missing(&format!("{tool} artifact"), &path.display().to_string());
            continue;
        }
        let resolved = resolve_ffmpeg(artifact, &request(path.clone())).unwrap();
        let staged = match tool {
            Tool::Ffmpeg => {
                let verified = verify_ffmpeg(resolved, T).unwrap();
                assert_eq!(verified.artifact_id(), artifact.id);
                // A verified ffmpeg is not an ffprobe.
                let again = resolve_ffmpeg(artifact, &request(path.clone())).unwrap();
                assert!(matches!(
                    verify_ffprobe(again, T),
                    Err(MediaError::WrongTool { .. })
                ));
                let first = stage_external_bin(&verified, &binaries, None).unwrap();
                let second = stage_external_bin(&verified, &binaries, Some(triple)).unwrap();
                assert!(first.changed && !second.changed);
                first
            }
            Tool::Ffprobe => {
                let verified = verify_ffprobe(resolved, T).unwrap();
                let first = stage_external_bin(&verified, &binaries, None).unwrap();
                assert!(
                    !stage_external_bin(&verified, &binaries, None)
                        .unwrap()
                        .changed
                );
                first
            }
        };
        let exe = if os == "windows" { ".exe" } else { "" };
        assert_eq!(
            staged.path,
            binaries.join(format!("{}-{triple}{exe}", tool.stem()))
        );
        assert_eq!(Some(staged.sha256.clone()), artifact.sha256.clone());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&staged.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(mode, 0o755);
        }
        // The staged copy runs and reports its own banner.
        let out = std::process::Command::new(&staged.path)
            .args(["-hide_banner", "-version"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout)
            .starts_with(&format!("{} version 7.1.5", tool.stem())));
    }
}

fn host_path_ffmpeg() -> Option<PathBuf> {
    let name = Tool::Ffmpeg.file_name(host().0);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&name))
        .find(|p| p.is_file())
}

#[test]
fn unverified_path_ffmpeg_is_labelled_probed_and_refused_in_release() {
    let Some(expected) = host_path_ffmpeg() else {
        common::skip_missing("FFmpeg on PATH", "PATH");
        return;
    };
    let mut request = UnverifiedRequest::new(Tool::Ffmpeg);
    request.allow_path_lookup = true;
    assert!(matches!(
        resolve_unverified(&request, BuildMode::Release),
        Err(MediaError::UnverifiedRefusedInRelease)
    ));
    let tool = resolve_unverified(&request, BuildMode::Development).unwrap();
    assert_eq!(tool.path(), expected.as_path());
    assert_eq!(tool.source(), &UnverifiedSource::PathLookup);
    assert!(tool.label().starts_with("UNVERIFIED ffmpeg"));
    let caps = tool.probe_capabilities(T).unwrap();
    eprintln!(
        "{}: {} | licence {:?} | {} encoders, {} decoders, {} filters",
        tool.label(),
        caps.version_line,
        caps.license,
        caps.encoders.len(),
        caps.decoders.len(),
        caps.filters.len()
    );
    for (kind, ok) in [
        ("pcm_s16le encoder", caps.has_encoder("pcm_s16le")),
        ("pcm_s16le decoder", caps.has_decoder("pcm_s16le")),
        ("loudnorm filter", caps.has_filter("loudnorm")),
        ("alimiter filter", caps.has_filter("alimiter")),
    ] {
        assert!(ok, "missing {kind}");
    }
    if caps.configuration.contains("--enable-gpl") {
        assert_eq!(caps.license, ConfigLicense::GplOrNonfree);
    }
}

#[test]
fn catalog_first_resolution_falls_back_only_in_development() {
    let Some(host_ffmpeg) = host_path_ffmpeg() else {
        common::skip_missing("FFmpeg on PATH", "PATH");
        return;
    };
    let (os, arch) = host();
    let catalog = ArtifactCatalog::embedded().unwrap();
    let Ok(artifact) = catalog.approved(os, arch) else {
        eprintln!("SKIP: no approved ffmpeg for {os}/{arch}");
        return;
    };
    if sha256_file(&host_ffmpeg).ok() == artifact.sha256 {
        eprintln!("SKIP: PATH ffmpeg is the catalog artifact");
        return;
    }
    // The bundled candidate is the host FFmpeg, which does not match the catalog.
    let bundled = request(host_ffmpeg.clone());
    let mut unverified = UnverifiedRequest::new(Tool::Ffmpeg);
    unverified.allow_path_lookup = true;
    assert!(matches!(
        resolve_ffmpeg_or_unverified(artifact, &bundled, &unverified, BuildMode::Release, T),
        Err(MediaError::DigestMismatch { .. })
    ));
    match resolve_ffmpeg_or_unverified(artifact, &bundled, &unverified, BuildMode::Development, T)
        .unwrap()
    {
        AnyFfmpeg::Unverified {
            tool,
            catalog_error,
        } => {
            assert_eq!(tool.path(), host_ffmpeg.as_path());
            assert!(catalog_error.contains("digest mismatch"), "{catalog_error}");
        }
        AnyFfmpeg::Verified(_) => panic!("mismatched binary must not verify"),
    }
    let vendor_ffmpeg = vendor_file(Tool::Ffmpeg, os, arch);
    if vendor_ffmpeg.is_file() {
        let any = resolve_ffmpeg_or_unverified(
            artifact,
            &request(vendor_ffmpeg),
            &unverified,
            BuildMode::Release,
            T,
        )
        .unwrap();
        assert!(any.is_verified(), "{}", any.label());
    }
}
