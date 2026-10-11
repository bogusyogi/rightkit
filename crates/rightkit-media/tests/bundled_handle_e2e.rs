//! `verify_bundled_ffmpeg` / `verify_bundled_ffprobe`: a bundled path plus the expected tool and
//! variant becomes a verified handle that every run and probe call accepts.
//!
//! The unit-style tests use stand-in scripts and a catalog built for them (unix only). The
//! last test runs the real `export-ass-zimg` binary from the workspace `vendor/ffmpeg-lgpl/`
//! against the embedded catalog: digest, build probe, then an HDR to SDR `zscale` + `tonemap`
//! render and an ffprobe of the result. It reports SKIP on stderr when the binary is absent.

#![cfg(unix)]

use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use rightkit_media::{
    digest_computations, run_checked, sha256_file, verify_bundled_ffmpeg, verify_bundled_ffmpeg_in,
    verify_bundled_ffprobe, verify_bundled_ffprobe_in, ArtifactCatalog, CancellationToken,
    FfmpegBinary, FfmpegCommand, MediaError, DECODE_VARIANT, EXPORT_ASS_ZIMG_VARIANT,
};

const T: Duration = Duration::from_secs(120);
const VENDOR: &str = "/Volumes/D/claude/vendor/ffmpeg-lgpl";

/// The verify cache and its counters are process-wide: these tests run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn script(path: &Path, marker: &Path, banner: &str, configuration: &str) {
    let body = format!(
        "#!/bin/sh\necho probe >> '{}'\nprintf '{banner}\\nconfiguration: {configuration}\\n'\n",
        marker.display()
    );
    let mut file = fs::File::create(path).unwrap();
    file.write_all(body.as_bytes()).unwrap();
    file.sync_all().unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

const GOOD: &str = "--disable-gpl --disable-nonfree --disable-autodetect --enable-libzimg";

fn row(
    id: &str,
    tool: &str,
    variant: &str,
    arch: &str,
    status: &str,
    digest: Option<&str>,
) -> String {
    let digests = digest.map_or(String::new(), |d| {
        format!("\"sha256\": \"{d}\", \"contentSha256\": \"{d}\",")
    });
    let pending = if status == "pending" {
        r#","pending": {"reason": "not built", "recipe": "r.sh", "commands": ["r.sh"], "promote": "p"}"#
    } else {
        ""
    };
    format!(
        r#"{{"id": "{id}", "tool": "{tool}", "variant": "{variant}", "os": "macos", "arch": "{arch}",
        "version": "7.1.5", "status": "{status}", {digests}
        "sourceSha256": "de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f",
        "attribution": {{"licenseSpdx": "LGPL-2.1-or-later",
          "sourceUrl": "https://ffmpeg.org/releases/ffmpeg-7.1.5.tar.xz",
          "buildProvenance": "BUILD-NOTES.txt", "noticeFiles": ["LGPL-2.1.txt", "BUILD-NOTES.txt"],
          "components": []}},
        "requiredConfigureFlags": ["--disable-gpl", "--disable-nonfree", "--disable-autodetect"]
        {pending}}}"#
    )
}

struct Fixture {
    _dir: tempfile::TempDir,
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    marker: PathBuf,
    catalog: ArtifactCatalog,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("probe-runs");
    let ffmpeg = dir.path().join("ffmpeg");
    let ffprobe = dir.path().join("ffprobe");
    script(&ffmpeg, &marker, "ffmpeg version 7.1.5 stand-in", GOOD);
    script(&ffprobe, &marker, "ffprobe version 7.1.5 stand-in", GOOD);
    let (m, p) = (
        sha256_file(&ffmpeg).unwrap(),
        sha256_file(&ffprobe).unwrap(),
    );
    let rows = [
        row(
            "ffmpeg-fix",
            "ffmpeg",
            EXPORT_ASS_ZIMG_VARIANT,
            "aarch64",
            "approved",
            Some(&m),
        ),
        row(
            "ffprobe-fix",
            "ffprobe",
            EXPORT_ASS_ZIMG_VARIANT,
            "aarch64",
            "approved",
            Some(&p),
        ),
        row(
            "ffmpeg-fix-x86",
            "ffmpeg",
            EXPORT_ASS_ZIMG_VARIANT,
            "x86_64",
            "pending",
            None,
        ),
    ]
    .join(",");
    let catalog =
        ArtifactCatalog::parse(&format!(r#"{{"schemaVersion": 2, "artifacts": [{rows}]}}"#))
            .unwrap();
    Fixture {
        _dir: dir,
        ffmpeg,
        ffprobe,
        marker,
        catalog,
    }
}

fn probe_runs(marker: &Path) -> usize {
    fs::read_to_string(marker).map_or(0, |text| text.lines().count())
}

#[test]
fn one_call_returns_handles_that_the_run_calls_accept_and_verification_happens_once() {
    let _serial = serial();
    let f = fixture();
    rightkit_media::verify_cache_clear();

    let before = digest_computations();
    let ffmpeg = verify_bundled_ffmpeg_in(
        &f.catalog,
        "macos",
        "aarch64",
        &f.ffmpeg,
        EXPORT_ASS_ZIMG_VARIANT,
        T,
    )
    .unwrap();
    let ffprobe = verify_bundled_ffprobe_in(
        &f.catalog,
        "macos",
        "aarch64",
        &f.ffprobe,
        EXPORT_ASS_ZIMG_VARIANT,
        T,
    )
    .unwrap();
    assert_eq!(digest_computations() - before, 2, "each binary hashed once");
    assert_eq!(probe_runs(&f.marker), 2, "each binary probed once");
    assert_eq!(ffmpeg.artifact_id(), "ffmpeg-fix");
    assert_eq!(ffprobe.artifact_id(), "ffprobe-fix");

    // The handles are the argument of the run calls, by reference, any number of times.
    let cancel = CancellationToken::new();
    for _ in 0..3 {
        run_checked(&ffmpeg, &FfmpegCommand::base("error"), T, &cancel).unwrap();
        run_checked(&ffprobe, &FfmpegCommand::base("error"), T, &cancel).unwrap();
    }
    let as_binary: &dyn FfmpegBinary = &ffmpeg;
    assert_eq!(as_binary.ffmpeg_path(), f.ffmpeg.as_path());
    let _copy = ffmpeg.clone();

    // Asking again for the same unchanged file goes through the verify cache.
    let before = digest_computations();
    let again = verify_bundled_ffmpeg_in(
        &f.catalog,
        "macos",
        "aarch64",
        &f.ffmpeg,
        EXPORT_ASS_ZIMG_VARIANT,
        T,
    )
    .unwrap();
    assert_eq!(digest_computations() - before, 0, "cache hit must not hash");
    assert_eq!(
        probe_runs(&f.marker),
        2 + 6,
        "only the six explicit runs executed the scripts; the repeat verification did not"
    );
    assert_eq!(again.path(), ffmpeg.path());
}

#[test]
fn a_path_that_is_not_the_approved_build_for_that_tool_and_variant_is_refused() {
    let _serial = serial();
    let f = fixture();
    let ok = |variant: &str| {
        verify_bundled_ffmpeg_in(&f.catalog, "macos", "aarch64", &f.ffmpeg, variant, T)
    };

    // No row for this variant: not in the catalog, so a development fallback is permitted.
    let error = ok(DECODE_VARIANT).unwrap_err();
    assert!(
        matches!(error, MediaError::NoApprovedArtifact { .. }),
        "{error}"
    );
    assert!(error.is_not_in_catalog());

    // A row exists but is not approved yet: a defect to show, not a fallback.
    let pending = verify_bundled_ffmpeg_in(
        &f.catalog,
        "macos",
        "x86_64",
        &f.ffmpeg,
        EXPORT_ASS_ZIMG_VARIANT,
        T,
    )
    .unwrap_err();
    assert!(
        matches!(pending, MediaError::ArtifactPending { .. }),
        "{pending}"
    );
    assert!(!pending.is_not_in_catalog());

    // The ffprobe file under the ffmpeg row, and the reverse: digest mismatch, never a run.
    let before = probe_runs(&f.marker);
    assert!(matches!(
        verify_bundled_ffmpeg_in(
            &f.catalog,
            "macos",
            "aarch64",
            &f.ffprobe,
            EXPORT_ASS_ZIMG_VARIANT,
            T
        ),
        Err(MediaError::DigestMismatch { .. })
    ));
    assert!(matches!(
        verify_bundled_ffprobe_in(
            &f.catalog,
            "macos",
            "aarch64",
            &f.ffmpeg,
            EXPORT_ASS_ZIMG_VARIANT,
            T
        ),
        Err(MediaError::DigestMismatch { .. })
    ));
    assert_eq!(
        probe_runs(&f.marker),
        before,
        "a mismatched file is never executed"
    );

    // A missing file is not "unapproved".
    let missing = verify_bundled_ffmpeg_in(
        &f.catalog,
        "macos",
        "aarch64",
        &f.ffmpeg.with_file_name("absent"),
        EXPORT_ASS_ZIMG_VARIANT,
        T,
    )
    .unwrap_err();
    assert!(matches!(missing, MediaError::ArtifactNotFound), "{missing}");
    assert!(!missing.is_not_in_catalog());

    // Same path, different bytes: refused.
    let _ = fs::remove_file(&f.ffmpeg);
    script(&f.ffmpeg, &f.marker, "ffmpeg version 7.1.5 tampered", GOOD);
    assert!(matches!(
        ok(EXPORT_ASS_ZIMG_VARIANT),
        Err(MediaError::DigestMismatch { .. })
    ));
}

#[test]
fn the_build_probe_still_applies_to_a_binary_with_the_right_digest() {
    let _serial = serial();
    let f = fixture();
    // Re-pin the row to a script whose banner reports a GPL build.
    let bad = f.ffmpeg.with_file_name("ffmpeg-gpl");
    script(
        &bad,
        &f.marker,
        "ffmpeg version 7.1.5 gpl",
        "--enable-gpl --disable-nonfree --disable-autodetect",
    );
    let digest = sha256_file(&bad).unwrap();
    let catalog = ArtifactCatalog::parse(&format!(
        r#"{{"schemaVersion": 2, "artifacts": [{}]}}"#,
        row(
            "ffmpeg-gpl",
            "ffmpeg",
            EXPORT_ASS_ZIMG_VARIANT,
            "aarch64",
            "approved",
            Some(&digest)
        )
    ))
    .unwrap();
    assert!(matches!(
        verify_bundled_ffmpeg_in(
            &catalog,
            "macos",
            "aarch64",
            &bad,
            EXPORT_ASS_ZIMG_VARIANT,
            T
        ),
        Err(MediaError::ForbiddenConfigureFlag(_))
    ));
}

/// Real binary, embedded catalog, this host. macOS aarch64 only (the one approved row).
#[test]
fn the_export_ass_zimg_binary_verifies_in_one_call_and_converts_hdr_to_sdr() {
    let _serial = serial();
    if std::env::consts::OS != "macos" || std::env::consts::ARCH != "aarch64" {
        eprintln!("SKIP: export-ass-zimg is approved for macOS aarch64 only");
        return;
    }
    let ffmpeg_path = Path::new(VENDOR).join("ffmpeg-export-ass-zimg-macos-arm64");
    let ffprobe_path = Path::new(VENDOR).join("ffprobe-decode-macos-arm64");
    if !ffmpeg_path.is_file() || !ffprobe_path.is_file() {
        eprintln!(
            "SKIP: {} or {} not present",
            ffmpeg_path.display(),
            ffprobe_path.display()
        );
        return;
    }
    let ffmpeg = match verify_bundled_ffmpeg(&ffmpeg_path, EXPORT_ASS_ZIMG_VARIANT, T) {
        Ok(handle) => handle,
        Err(MediaError::ArtifactPending { id }) => {
            eprintln!("SKIP: catalog row {id} is still pending");
            return;
        }
        Err(error) => panic!("catalog digest, LGPL banner and configure flags: {error}"),
    };
    let ffprobe = verify_bundled_ffprobe(&ffprobe_path, DECODE_VARIANT, T).unwrap();
    assert_eq!(
        ffmpeg.artifact_id(),
        "ffmpeg-7.1.5-export-ass-zimg-macos-arm64"
    );

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("hdr.mkv");
    let sdr = dir.path().join("sdr.mp4");
    let cancel = CancellationToken::new();
    let mut make = FfmpegCommand::base("error");
    make.texts([
        "-y", "-f", "lavfi", "-i",
        "testsrc2=s=640x360:r=25:d=1,format=yuv420p10le,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc:range=tv",
        "-c:v", "ffv1", "-color_primaries", "bt2020", "-color_trc", "smpte2084", "-colorspace", "bt2020nc",
    ]);
    make.text(source.to_str().unwrap());
    run_checked(&ffmpeg, &make, T, &cancel).expect("generate the HDR-tagged source");

    let mut convert = FfmpegCommand::base("error");
    convert.texts([
        "-y",
        "-i",
        source.to_str().unwrap(),
        "-vf",
        "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p",
        "-c:v",
        "h264_videotoolbox",
        "-b:v",
        "2M",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
        "-colorspace",
        "bt709",
        "-an",
        sdr.to_str().unwrap(),
    ]);
    run_checked(&ffmpeg, &convert, T, &cancel).expect("zscale + tonemap + h264_videotoolbox");

    let probed = ffprobe.ffprobe_json(&sdr, Some(T), &cancel).unwrap();
    let video = probed
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video"))
        .expect("a video stream");
    assert_eq!(video.codec_name.as_deref(), Some("h264"));
    assert_eq!(video.pix_fmt.as_deref(), Some("yuv420p"));
    assert_eq!(video.color_primaries.as_deref(), Some("bt709"));
    assert_eq!(video.color_transfer.as_deref(), Some("bt709"));
    assert_eq!(video.color_space.as_deref(), Some("bt709"));
}
