//! The approved `export-ass` FFmpeg (libass + FreeType/HarfBuzz + VideoToolbox) burns an
//! `.ass` subtitle (`subtitles` filter, GenRight) and a `drawtext` caption (CutRight) into a
//! real h264_videotoolbox/AAC MP4, and the burned pixels are read back from the decoded file.
//! The binary lives in the workspace `vendor/ffmpeg-lgpl/`; when it is absent for this host
//! the test reports SKIP on stderr rather than silently passing.

mod common;

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use rightkit_ffmpeg::{
    resolve_ffmpeg, run_checked, verify_ffmpeg, ArtifactCatalog, CancellationToken, FfmpegCommand,
    ResolutionRequest, Tool, VerifiedFfmpeg, EXPORT_ASS_VARIANT,
};

const T: Duration = Duration::from_secs(120);
const W: usize = 320;
const H: usize = 240;
/// Rows the burned text is placed in (bottom band); the top band must stay untouched.
const TEXT_BAND: std::ops::Range<usize> = 150..240;
const CLEAN_BAND: std::ops::Range<usize> = 0..100;

fn export_ffmpeg() -> Option<VerifiedFfmpeg> {
    if std::env::consts::OS != "macos" {
        eprintln!("SKIP: export-ass is a macOS catalog variant");
        return None;
    }
    let arch = std::env::consts::ARCH;
    let file = if arch == "aarch64" { "arm64" } else { arch };
    let path = common::vendor_dir().join(format!("ffmpeg-export-ass-macos-{file}"));
    if !path.is_file() {
        common::skip_missing("export-ass FFmpeg", &path.display().to_string());
        return None;
    }
    let catalog = ArtifactCatalog::embedded().unwrap();
    let artifact = match catalog.approved_for(Tool::Ffmpeg, EXPORT_ASS_VARIANT, "macos", arch) {
        Ok(artifact) => artifact,
        Err(error) => {
            common::skip_missing(
                &format!("approved export-ass artifact for macos/{arch}"),
                &error.to_string(),
            );
            return None;
        }
    };
    let resolved = resolve_ffmpeg(
        artifact,
        &ResolutionRequest {
            override_path: None,
            bundled_paths: vec![path],
            development_path: None,
            allow_development_path: false,
        },
    )
    .unwrap();
    Some(verify_ffmpeg(resolved, T).expect("digest, LGPL and configure flags verify"))
}

fn run(bin: &VerifiedFfmpeg, args: &[&str]) {
    let mut c = FfmpegCommand::base("error");
    c.texts(args.iter().copied());
    run_checked(bin, &c, T, &CancellationToken::new()).expect("ffmpeg run");
}

fn listing(bin: &VerifiedFfmpeg, flag: &str) -> Vec<String> {
    let out = Command::new(bin.path())
        .args(["-hide_banner", flag])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1).map(str::to_owned))
        .collect()
}

/// Encode a 2 s black 320x240 clip with a sine track through `vf` (when given) to MP4.
fn render(bin: &VerifiedFfmpeg, dir: &Path, name: &str, vf: Option<&str>) -> PathBuf {
    let out = dir.join(name);
    let mut args = vec![
        "-y",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=320x240:r=10:d=2",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:duration=2",
    ];
    if let Some(vf) = vf {
        args.extend(["-vf", vf]);
    }
    args.extend([
        "-c:v",
        "h264_videotoolbox",
        "-b:v",
        "2M",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-shortest",
        out.to_str().unwrap(),
    ]);
    run(bin, &args);
    out
}

/// Decode the frame at 1 s of `video` as 320x240 gray8.
fn frame(bin: &VerifiedFfmpeg, video: &Path) -> Vec<u8> {
    let raw = video.with_extension("gray");
    run(
        bin,
        &[
            "-y",
            "-ss",
            "1",
            "-i",
            video.to_str().unwrap(),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            raw.to_str().unwrap(),
        ],
    );
    let bytes = std::fs::read(raw).unwrap();
    assert_eq!(bytes.len(), W * H, "one 320x240 gray frame");
    bytes
}

fn bright(frame: &[u8], rows: std::ops::Range<usize>) -> usize {
    rows.flat_map(|y| &frame[y * W..(y + 1) * W])
        .filter(|&&v| v > 128)
        .count()
}

#[test]
fn export_ass_burns_ass_subtitles_and_drawtext_captions_into_a_videotoolbox_mp4() {
    let Some(bin) = export_ffmpeg() else {
        return;
    };
    let filters = listing(&bin, "-filters");
    for name in ["subtitles", "ass", "drawtext"] {
        assert!(filters.iter().any(|f| f == name), "missing filter {name}");
    }
    let encoders = listing(&bin, "-encoders");
    for name in ["aac", "h264_videotoolbox"] {
        assert!(encoders.iter().any(|e| e == name), "missing encoder {name}");
    }

    let dir = tempfile::tempdir().unwrap();
    let ass = dir.path().join("caption.ass");
    std::fs::write(
        &ass,
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 240\n\n\
         [V4+ Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Default,Helvetica,36,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,-1,0,0,0,100,100,0,0,1,0,0,2,10,10,20,1\n\n\
         [Events]\n\
         Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
         Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,BURNED IN\n",
    )
    .unwrap();
    let font = [
        "/System/Library/Fonts/Helvetica.ttc",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
    ]
    .into_iter()
    .find(|p| Path::new(p).is_file())
    .expect("a macOS system font");

    let plain = frame(&bin, &render(&bin, dir.path(), "plain.mp4", None));
    // The filter graph is an FFmpeg option string; the temp path holds no ':' or '\''.
    let ass_vf = format!("subtitles=filename='{}'", ass.display());
    let subtitled = frame(&bin, &render(&bin, dir.path(), "ass.mp4", Some(&ass_vf)));
    let draw_vf = format!(
        "drawtext=fontfile='{font}':text='CAPTION':fontcolor=white:fontsize=40:x=(w-tw)/2:y=h-th-30"
    );
    let captioned = frame(
        &bin,
        &render(&bin, dir.path(), "drawtext.mp4", Some(&draw_vf)),
    );

    let baseline = bright(&plain, 0..H);
    assert_eq!(baseline, 0, "the plain black render has no bright pixels");
    for (label, burned) in [("subtitles", &subtitled), ("drawtext", &captioned)] {
        let text = bright(burned, TEXT_BAND);
        let clean = bright(burned, CLEAN_BAND);
        eprintln!("{label}: {text} bright pixels in the text band, {clean} above it");
        assert!(text > 300, "{label} burned too few pixels: {text}");
        assert_eq!(clean, 0, "{label} changed pixels outside the text band");
    }
}
