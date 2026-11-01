//! Real-ffmpeg checks of the QA gate and social packaging. Renders are made by
//! the editing ffmpeg named by `RIGHTKIT_FFMPEG` / `RIGHTKIT_FFPROBE`; each test prints
//! `SKIP: <reason>` and returns when it is not available.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rightkit_fs::zip::read_zip;
use rightkit_review::render_qa::receipt::{receipt_path, report_path};
use rightkit_review::render_qa::{
    package_render, parse_srt, qa_gate, verify_package, verify_receipt, PackageRequest, Platform,
    QaError, QaExpect, QaSpec, QaTools, SocialMetadata,
};

/// The host's own H.264 encoder: the bundled LGPL ffmpeg has no libx264.
const H264: &str = if cfg!(target_os = "macos") {
    "h264_videotoolbox"
} else {
    "h264_mf"
};

fn tools() -> Option<QaTools> {
    let bin = |env: &str| -> Option<PathBuf> { std::env::var_os(env).map(PathBuf::from) };
    let (Some(ffmpeg), Some(ffprobe)) = (bin("RIGHTKIT_FFMPEG"), bin("RIGHTKIT_FFPROBE")) else {
        eprintln!("SKIP: RIGHTKIT_FFMPEG / RIGHTKIT_FFPROBE not set");
        return None;
    };
    match QaTools::new(ffmpeg, ffprobe) {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("SKIP: editing ffmpeg unavailable ({e})");
            None
        }
    }
}

macro_rules! tools_or_skip {
    () => {
        match tools() {
            Some(t) => t,
            None => return,
        }
    };
}

#[derive(Clone, Copy)]
enum Defect {
    None,
    /// 2 s of black in the middle.
    BlackSegment,
    /// Audio driven far past full scale.
    ClippedAudio,
}

/// A real render: moving test pattern + a tone near -14 LUFS (sine source is already low-level), h264/aac in mp4.
fn render(
    t: &QaTools,
    dir: &Path,
    name: &str,
    w: u32,
    h: u32,
    secs: u32,
    defect: Defect,
) -> PathBuf {
    let out = dir.join(name);
    let mut vf = format!("testsrc2=s={w}x{h}:r=30:d={secs}");
    if let Defect::BlackSegment = defect {
        vf.push_str(",drawbox=x=0:y=0:w=iw:h=ih:color=black:t=fill:enable='between(t,1.5,3.5)'");
    }
    let af = match defect {
        Defect::ClippedAudio => "sine=frequency=330:sample_rate=48000:duration=SECS,volume=26dB",
        _ => "sine=frequency=330:sample_rate=48000:duration=SECS,volume=7.5dB",
    }
    .replace("SECS", &secs.to_string());
    let o = Command::new(&t.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(&vf)
        .args(["-f", "lavfi", "-i"])
        .arg(&af)
        .args(["-c:v", H264, "-b:v", "4M", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-b:a", "192k", "-shortest"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    out
}

fn expect_secs(secs: i64) -> QaExpect {
    QaExpect {
        duration_ms: Some(secs * 1000),
        fps: Some(rightkit_timeline::Fps { num: 30, den: 1 }),
        ..QaExpect::default()
    }
}

fn meta() -> SocialMetadata {
    SocialMetadata {
        title: "Test cut".into(),
        description: "A package built by the QA gate tests.".into(),
        tags: vec!["test".into(), "rightkit".into()],
    }
}

fn request(project: &Path, render: &Path, platform: Platform) -> PackageRequest {
    PackageRequest {
        project_dir: project.to_path_buf(),
        render: render.to_path_buf(),
        platform,
        metadata: meta(),
        captions_srt: None,
        out_dir: None,
        thumbnail_ms: None,
        zip: false,
    }
}

const SRT: &str = "1\n00:00:00,200 --> 00:00:01,800\nhello and welcome\n\n2\n00:00:02,000 --> 00:00:03,800\nto the test cut\n\n";

#[test]
fn good_render_passes_and_packages_for_youtube() {
    let t = tools_or_skip!();
    let d = tempfile::tempdir().unwrap();
    let project = d.path().join("project");
    let video = render(&t, d.path(), "final.mp4", 1920, 1080, 4, Defect::None);
    let spec = QaSpec::preset("youtube").unwrap();
    let mut expect = expect_secs(4);
    expect.captions_srt = Some(SRT.into());
    let q = qa_gate(&project, &t, &video, &spec, &expect).unwrap();
    for c in &q.report.checks {
        eprintln!("{:<24} {:?} {:?} {}", c.id, c.status, c.severity, c.message);
    }
    assert!(q.report.passed(), "blocking: {:?}", q.report.blocking_ids());
    assert!(q.report_path.is_file() && q.receipt_path.is_file());
    assert_eq!(q.report.media.width, Some(1920));
    // Loudness is measured, not assumed.
    let lufs = q.report.check("audio.loudness").unwrap().evidence["integrated_lufs"]
        .as_f64()
        .unwrap();
    assert!((lufs + 14.0).abs() < 2.0, "{lufs}");

    let verified = verify_receipt(&project, &q.report.render.blake3).unwrap();
    assert!(verified.report.passed());
    assert_eq!(
        verified.receipt_file_blake3,
        rightkit_fs::digest::blake3_file(&q.receipt_path).unwrap(),
    );
    assert_ne!(
        verified.receipt_file_blake3,
        verified.receipt.receipt_blake3
    );

    let mut req = request(&project, &video, Platform::YouTube);
    req.captions_srt = Some(SRT.into());
    req.zip = true;
    let out = package_render(&t, &req).unwrap();
    assert!(out.approved);
    for f in [
        "video.mp4",
        "captions.srt",
        "thumbnail.jpg",
        "metadata.json",
        "package-manifest.json",
    ] {
        assert!(out.dir.join(f).is_file(), "{f}");
    }
    let jpg = fs::read(out.dir.join("thumbnail.jpg")).unwrap();
    assert_eq!(&jpg[..2], &[0xFF, 0xD8], "thumbnail is a JPEG");
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(out.dir.join("metadata.json")).unwrap()).unwrap();
    assert_eq!(meta["platform"], "youtube");
    assert_eq!(meta["video"]["width"], 1920);
    assert_eq!(
        parse_srt(&fs::read_to_string(out.dir.join("captions.srt")).unwrap())
            .unwrap()
            .len(),
        2
    );
    assert_eq!(out.manifest.render_blake3, q.report.render.blake3);

    // Zip: every entry checks out and holds the same bytes.
    let zip = read_zip(out.zip.as_ref().unwrap()).unwrap();
    assert!(zip.iter().any(|(n, _)| n == "youtube/video.mp4"));
    assert!(zip
        .iter()
        .any(|(n, _)| n == "youtube/package-manifest.json"));
    let zv = zip.iter().find(|(n, _)| n == "youtube/video.mp4").unwrap();
    assert_eq!(zv.1, fs::read(&video).unwrap());

    let expected_names: Vec<_> = out
        .manifest
        .files
        .iter()
        .map(|f| format!("youtube/{}", f.path))
        .chain(["youtube/package-manifest.json".to_string()])
        .collect();
    assert_eq!(
        zip.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        expected_names.iter().collect::<Vec<_>>()
    );
    let zip_bytes = fs::read(out.zip.as_ref().unwrap()).unwrap();
    let mut offset = 0;
    for (name, bytes) in &zip {
        assert_eq!(&zip_bytes[offset..offset + 4], b"PK\x03\x04");
        assert_eq!(
            &zip_bytes[offset + 8..offset + 10],
            &[0, 0],
            "stored method"
        );
        assert_eq!(
            &zip_bytes[offset + 10..offset + 14],
            &[0, 0, 0x21, 0],
            "ZIP epoch"
        );
        offset += 30 + name.len() + bytes.len();
    }
    let entries: Vec<_> = expected_names
        .iter()
        .map(|name| {
            (
                name.clone(),
                out.dir.join(name.strip_prefix("youtube/").unwrap()),
            )
        })
        .collect();
    let repeat_zip = d.path().join("repeat.zip");
    rightkit_fs::zip::write_zip(&repeat_zip, &entries).unwrap();
    assert_eq!(
        zip_bytes,
        fs::read(&repeat_zip).unwrap(),
        "deterministic archive bytes"
    );
    let mut corrupt = zip_bytes;
    corrupt[30 + zip[0].0.len()] ^= 1;
    let corrupt_zip = d.path().join("corrupt.zip");
    fs::write(&corrupt_zip, corrupt).unwrap();
    assert_eq!(
        read_zip(&corrupt_zip).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );

    // Republish from the existing package: removing its destination first loses the input.
    fs::write(out.dir.join("old-only"), "stale").unwrap();
    req.render = out.dir.join("video.mp4");
    let again = package_render(&t, &req).unwrap();
    assert_eq!(again.manifest.files, out.manifest.files);
    assert!(!again.dir.join("old-only").exists());
    verify_package(&out.dir).unwrap();
    assert!(fs::read_dir(out.dir.parent().unwrap())
        .unwrap()
        .all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".rightkit-package-")
        }));

    // A source refused during staged copying leaves the approved package intact.
    #[cfg(unix)]
    {
        let linked = d.path().join("linked.mp4");
        std::os::unix::fs::symlink(&req.render, &linked).unwrap();
        req.render = linked;
        assert!(package_render(&t, &req).is_err());
        assert_eq!(
            fs::read(out.dir.join("video.mp4")).unwrap(),
            fs::read(&video).unwrap()
        );
        verify_package(&out.dir).unwrap();
    }

    // Tampering with the package is caught.
    fs::write(
        out.dir.join("captions.srt"),
        "1\n00:00:00,000 --> 00:00:01,000\nforged\n\n",
    )
    .unwrap();
    assert!(matches!(
        verify_package(&out.dir),
        Err(QaError::PackageInvalid(_))
    ));
}

#[test]
fn black_segment_fails_qa_and_blocks_packaging() {
    let t = tools_or_skip!();
    let d = tempfile::tempdir().unwrap();
    let project = d.path().join("project");
    let video = render(
        &t,
        d.path(),
        "black.mp4",
        1920,
        1080,
        5,
        Defect::BlackSegment,
    );
    let q = qa_gate(
        &project,
        &t,
        &video,
        &QaSpec::preset("youtube").unwrap(),
        &expect_secs(5),
    )
    .unwrap();
    assert!(!q.report.passed());
    let ids = q.report.blocking_ids();
    assert!(ids.contains(&"video.black_frames".to_string()), "{ids:?}");
    // The failure is on file, and the gate refuses on it.
    let r = package_render(&t, &request(&project, &video, Platform::YouTube));
    match r {
        Err(QaError::QaFailed {
            failing,
            render_blake3,
        }) => {
            assert!(failing.contains(&"video.black_frames".to_string()));
            assert_eq!(render_blake3, q.report.render.blake3);
        }
        other => panic!("expected QaFailed, got {other:?}"),
    }
    assert!(
        !project.join("exports").exists(),
        "nothing may be written for a failed render"
    );
}

#[test]
fn clipped_audio_fails_qa_and_blocks_packaging() {
    let t = tools_or_skip!();
    let d = tempfile::tempdir().unwrap();
    let project = d.path().join("project");
    let video = render(
        &t,
        d.path(),
        "clipped.mp4",
        1920,
        1080,
        4,
        Defect::ClippedAudio,
    );
    let q = qa_gate(
        &project,
        &t,
        &video,
        &QaSpec::preset("youtube").unwrap(),
        &expect_secs(4),
    )
    .unwrap();
    assert!(!q.report.passed());
    let ids = q.report.blocking_ids();
    assert!(ids.contains(&"audio.clipping".to_string()), "{ids:?}");
    assert!(matches!(
        package_render(&t, &request(&project, &video, Platform::YouTube)),
        Err(QaError::QaFailed { .. })
    ));
}

#[test]
fn missing_qa_or_a_changed_render_blocks_packaging() {
    let t = tools_or_skip!();
    let d = tempfile::tempdir().unwrap();
    let project = d.path().join("project");
    let video = render(&t, d.path(), "final.mp4", 1920, 1080, 3, Defect::None);
    // No QA run at all.
    assert!(matches!(
        package_render(&t, &request(&project, &video, Platform::YouTube)),
        Err(QaError::QaMissing { .. })
    ));
    // QA passes on these bytes...
    let q = qa_gate(
        &project,
        &t,
        &video,
        &QaSpec::preset("youtube").unwrap(),
        &expect_secs(3),
    )
    .unwrap();
    assert!(q.report.passed(), "{:?}", q.report.blocking_ids());
    // ...but a different render (re-encoded after the QA) has no receipt.
    let other = render(
        &t,
        d.path(),
        "other.mp4",
        1920,
        1080,
        3,
        Defect::BlackSegment,
    );
    assert!(matches!(
        package_render(&t, &request(&project, &other, Platform::YouTube)),
        Err(QaError::QaMissing { .. })
    ));
    // Appending a byte to the QA'd file also changes its identity.
    let mut bytes = fs::read(&video).unwrap();
    bytes.push(0);
    fs::write(&video, &bytes).unwrap();
    assert!(matches!(
        package_render(&t, &request(&project, &video, Platform::YouTube)),
        Err(QaError::QaMissing { .. })
    ));
}

#[test]
fn forged_report_or_receipt_does_not_verify() {
    let t = tools_or_skip!();
    let d = tempfile::tempdir().unwrap();
    let project = d.path().join("project");
    let video = render(
        &t,
        d.path(),
        "black.mp4",
        1920,
        1080,
        5,
        Defect::BlackSegment,
    );
    let q = qa_gate(
        &project,
        &t,
        &video,
        &QaSpec::preset("youtube").unwrap(),
        &expect_secs(5),
    )
    .unwrap();
    assert!(!q.report.passed());
    let hash = q.report.render.blake3.clone();

    // Flip the report to "pass" without touching the receipt.
    let rp = report_path(&project, &hash);
    let original = fs::read_to_string(&rp).unwrap();
    fs::write(
        &rp,
        original.replacen("\"status\": \"fail\"", "\"status\": \"pass\"", 1),
    )
    .unwrap();
    let r = package_render(&t, &request(&project, &video, Platform::YouTube));
    assert!(matches!(r, Err(QaError::ReceiptInvalid(_))), "{r:?}");

    // Flip the receipt too: its own hash no longer matches.
    let cp = receipt_path(&project, &hash);
    let rc = fs::read_to_string(&cp).unwrap();
    fs::write(
        &cp,
        rc.replacen("\"status\": \"fail\"", "\"status\": \"pass\"", 1),
    )
    .unwrap();
    let r = package_render(&t, &request(&project, &video, Platform::YouTube));
    assert!(matches!(r, Err(QaError::ReceiptInvalid(_))), "{r:?}");

    // A receipt copied from another render's name does not verify either.
    let good = render(&t, d.path(), "good.mp4", 1920, 1080, 3, Defect::None);
    let g = qa_gate(
        &project,
        &t,
        &good,
        &QaSpec::preset("youtube").unwrap(),
        &expect_secs(3),
    )
    .unwrap();
    assert!(g.report.passed());
    fs::copy(&g.report_path, report_path(&project, &hash)).unwrap();
    fs::copy(&g.receipt_path, receipt_path(&project, &hash)).unwrap();
    let r = package_render(&t, &request(&project, &video, Platform::YouTube));
    assert!(matches!(r, Err(QaError::ReceiptInvalid(_))), "{r:?}");
}

#[test]
fn platform_and_caption_binding() {
    let t = tools_or_skip!();
    let d = tempfile::tempdir().unwrap();
    let project = d.path().join("project");

    // A 16:9 render QA'd for YouTube cannot go to TikTok.
    let wide = render(&t, d.path(), "wide.mp4", 1920, 1080, 4, Defect::None);
    let mut e = expect_secs(4);
    e.captions_srt = Some(SRT.into());
    let q = qa_gate(&project, &t, &wide, &QaSpec::preset("youtube").unwrap(), &e).unwrap();
    assert!(q.report.passed(), "{:?}", q.report.blocking_ids());
    let mut req = request(&project, &wide, Platform::TikTok);
    req.captions_srt = Some(SRT.into());
    assert!(matches!(
        package_render(&t, &req),
        Err(QaError::PlatformMismatch { .. })
    ));
    // Different captions than QA checked.
    let mut req = request(&project, &wide, Platform::YouTube);
    req.captions_srt = Some("1\n00:00:00,000 --> 00:00:01,000\nother\n\n".into());
    assert!(matches!(
        package_render(&t, &req),
        Err(QaError::CaptionsMismatch { .. })
    ));
    // No captions although QA checked some.
    let req = request(&project, &wide, Platform::YouTube);
    assert!(matches!(
        package_render(&t, &req),
        Err(QaError::CaptionsMismatch { .. })
    ));
    // Over-long title.
    let mut req = request(&project, &wide, Platform::YouTube);
    req.captions_srt = Some(SRT.into());
    req.metadata.title = "x".repeat(101);
    assert!(matches!(
        package_render(&t, &req),
        Err(QaError::MetadataInvalid { .. })
    ));

    // Vertical renders package for all three vertical platforms.
    let tall = render(&t, d.path(), "tall.mp4", 1080, 1920, 4, Defect::None);
    let q = qa_gate(
        &project,
        &t,
        &tall,
        &QaSpec::preset("tiktok").unwrap(),
        &expect_secs(4),
    )
    .unwrap();
    assert!(q.report.passed(), "{:?}", q.report.blocking_ids());
    for p in [Platform::Shorts, Platform::Reels, Platform::TikTok] {
        let out = package_render(&t, &request(&project, &tall, p)).unwrap();
        assert!(out.approved && out.dir.ends_with(p.id()));
        assert!(!out.dir.join("captions.srt").exists());
    }
    // ...and not for YouTube (wrong aspect).
    assert!(matches!(
        package_render(&t, &request(&project, &tall, Platform::YouTube)),
        Err(QaError::PlatformMismatch { .. })
    ));
}

#[test]
fn wrong_size_wrong_duration_and_bad_captions_fail() {
    let t = tools_or_skip!();
    let d = tempfile::tempdir().unwrap();
    let project = d.path().join("project");
    // 720p against the youtube preset, timeline says 6 s but the render is 3 s.
    let small = render(&t, d.path(), "small.mp4", 1280, 720, 3, Defect::None);
    let mut e = expect_secs(6);
    e.captions_srt = Some("1\n00:00:00,000 --> 00:00:02,000\nok\n\n2\n00:00:01,000 --> 00:00:09,000\noverlaps and overruns\n\n".into());
    let q = qa_gate(
        &project,
        &t,
        &small,
        &QaSpec::preset("youtube").unwrap(),
        &e,
    )
    .unwrap();
    let ids = q.report.blocking_ids();
    for want in ["video.resolution", "media.duration", "captions.sanity"] {
        assert!(ids.contains(&want.to_string()), "{want} in {ids:?}");
    }
    assert!(!q.report.passed());
}

#[test]
fn truncated_render_fails_integrity() {
    let t = tools_or_skip!();
    let d = tempfile::tempdir().unwrap();
    let project = d.path().join("project");
    let video = render(&t, d.path(), "final.mp4", 1920, 1080, 4, Defect::None);
    let bytes = fs::read(&video).unwrap();
    let cut = d.path().join("cut.mp4");
    fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
    let q = qa_gate(
        &project,
        &t,
        &cut,
        &QaSpec::preset("youtube").unwrap(),
        &expect_secs(4),
    )
    .unwrap();
    assert!(!q.report.passed());
    assert!(q
        .report
        .blocking_ids()
        .contains(&"file.integrity".to_string()));
    assert!(matches!(
        package_render(&t, &request(&project, &cut, Platform::YouTube)),
        Err(QaError::QaFailed { .. })
    ));
}

#[test]
fn timeline_supplies_the_expectations() {
    use rightkit_timeline::*;
    let mut s = TimelineStore::new(Fps { num: 30, den: 1 });
    let id = SourceId::new(rightkit_fs::digest::blake3_bytes(b"x")).unwrap();
    s.apply(Action::RegisterSource {
        source: SourceInfo {
            id: id.clone(),
            duration: Ticks(10 * TICKS_PER_SECOND),
            video: Some(VideoStreamInfo {
                width: 1920,
                height: 1080,
                duration: None,
            }),
            has_audio: true,
        },
    })
    .unwrap();
    s.apply(Action::InsertClip {
        track_id: "V1".into(),
        index: 0,
        clip: VideoClip {
            id: "c".into(),
            source: id,
            source_in: Ticks(0),
            source_out: Ticks(4 * TICKS_PER_SECOND),
        },
    })
    .unwrap();
    let e = QaExpect::from_timeline(s.timeline());
    assert_eq!(e.duration_ms, Some(4000));
    assert_eq!(e.fps, Some(Fps { num: 30, den: 1 }));
    assert_eq!(
        QaSpec::for_timeline_preset(Preset::Reels).unwrap().height,
        1920
    );
}
