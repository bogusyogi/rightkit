//! Promotion evidence for the Windows catalogue rows, run on the binary a build just produced.
//!
//! The `ffmpeg-windows` workflow sets `RIGHTKIT_MEDIA_PROMOTE_FFMPEG` or
//! `RIGHTKIT_MEDIA_PROMOTE_FFPROBE` to the built executable. Each test then prints the file's
//! SHA-256, size and the crate's own content identity, checks them against the catalogue row, and
//! exercises the binary: ffprobe reads a committed WAV, ffmpeg runs the four export smokes
//! (`h264_mf`, `subtitles`, `drawtext`, `zscale` + `tonemap`). With the variable unset a test
//! prints why and passes, so the ordinary CI run is unaffected.
//!
//! `RIGHTKIT_MEDIA_PROMOTE_EVIDENCE`, when set, is a directory the printed lines are also written
//! to (`promote-ffmpeg.txt`, `promote-ffprobe.txt`).
//!
//! On a hosted runner `h264_mf` is Microsoft's software encoder; no hardware encoder is exercised.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rightkit_ffmpeg::{content_identity_file, sha256_file};

const FFMPEG_ENV: &str = "RIGHTKIT_MEDIA_PROMOTE_FFMPEG";
const FFPROBE_ENV: &str = "RIGHTKIT_MEDIA_PROMOTE_FFPROBE";
const EVIDENCE_ENV: &str = "RIGHTKIT_MEDIA_PROMOTE_EVIDENCE";
const FFMPEG_ROW: &str = "ffmpeg-7.1.5-export-ass-zimg-windows-x64";
const FFPROBE_ROW: &str = "ffprobe-7.1.5-windows-x64";
const CATALOG: &str = include_str!("../catalog/artifacts.json");

const W: usize = 320;
const H: usize = 240;
const FRAMES: usize = 25;

/// The lines one test prints, also written to the evidence directory when there is one.
struct Evidence {
    name: &'static str,
    lines: Vec<String>,
}

impl Evidence {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            lines: Vec::new(),
        }
    }

    fn line(&mut self, text: String) {
        println!("{text}");
        self.lines.push(text);
    }

    fn save(&self) {
        let Some(dir) = std::env::var_os(EVIDENCE_ENV) else {
            return;
        };
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("create the evidence directory");
        let mut text = self.lines.join("\n");
        text.push('\n');
        std::fs::write(dir.join(self.name), text).expect("write the evidence file");
    }
}

/// The executable named by `variable`, or `None` (with the reason printed) when it is unset.
fn built(variable: &str) -> Option<PathBuf> {
    match std::env::var_os(variable) {
        Some(path) if !path.is_empty() => {
            let path = PathBuf::from(path);
            assert!(
                path.is_file(),
                "{variable} is not a file: {}",
                path.display()
            );
            Some(path)
        }
        _ => {
            println!(
                "skipped: {variable} is not set; this test runs only on a freshly built binary"
            );
            None
        }
    }
}

/// Record the file's identity and check it against the catalogue row `id`: an approved row's
/// recorded values, or the values a pending row's reason states.
fn check_identity(evidence: &mut Evidence, exe: &Path, id: &str) {
    let sha256 = sha256_file(exe).expect("sha256 of the built binary");
    let size = std::fs::metadata(exe).expect("metadata").len();
    let identity = content_identity_file(exe).expect("content identity of the built binary");
    assert_eq!(identity.slices.len(), 1, "a PE file has one slice");
    let content = identity.slices[0].sha256.clone();
    evidence.line(format!("row: {id}"));
    evidence.line(format!("sha256: {sha256}"));
    evidence.line(format!("size: {size}"));
    evidence.line(format!(
        "contentSha256 (rightkit_ffmpeg::content_identity_file, {:?}): {content}",
        identity.format
    ));

    let catalog: serde_json::Value = serde_json::from_str(CATALOG).expect("catalogue JSON");
    let row = catalog["artifacts"]
        .as_array()
        .expect("artifacts")
        .iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("the catalogue has no row {id}"));
    match row["status"].as_str() {
        Some("approved") => {
            assert_eq!(row["sha256"], sha256.as_str(), "approved sha256");
            assert_eq!(
                row["contentSha256"],
                content.as_str(),
                "approved contentSha256"
            );
            assert_eq!(row["size"], size, "approved size");
            evidence.line("catalogue: approved row matches sha256, contentSha256 and size".into());
        }
        Some("pending") => {
            let reason = row["pending"]["reason"].as_str().expect("pending reason");
            assert!(
                reason.contains(&sha256),
                "the pending row states another sha256"
            );
            assert!(
                reason.contains(&content),
                "the pending row states another contentSha256"
            );
            assert!(
                reason.contains(&format!("{size} bytes")),
                "the pending row states another size"
            );
            evidence
                .line("catalogue: pending row states this sha256, contentSha256 and size".into());
        }
        other => panic!("row {id} has status {other:?}"),
    }
}

/// Run `exe` in `dir` and return (stdout, stderr); a non-zero exit fails the test with both.
fn run(exe: &Path, dir: &Path, args: &[&str]) -> (String, String) {
    let output = Command::new(exe)
        .current_dir(dir)
        .args(args)
        .output()
        .expect("start the built binary");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "{} {args:?} exited with {:?}\n{stderr}",
        exe.display(),
        output.status.code()
    );
    (stdout, stderr)
}

#[test]
fn the_built_ffprobe_matches_its_row_and_reads_a_real_file() {
    let Some(ffprobe) = built(FFPROBE_ENV) else {
        return;
    };
    let mut evidence = Evidence::new("promote-ffprobe.txt");
    check_identity(&mut evidence, &ffprobe, FFPROBE_ROW);

    // 0.1 s of a 440 Hz tone, 8 kHz mono 16-bit PCM in a 44-byte RIFF header (1,644 bytes). Made
    // with Python's `wave` and `math` modules: 800 samples of round(12000 * sin(2*pi*440*n/8000)).
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone-8k-mono-s16.wav");
    let dir = tempfile::tempdir().unwrap();
    let (stdout, _) = run(
        &ffprobe,
        dir.path(),
        &[
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
            fixture.to_str().unwrap(),
        ],
    );
    let probed: serde_json::Value = serde_json::from_str(&stdout).expect("ffprobe JSON");
    let streams = probed["streams"].as_array().expect("streams");
    assert_eq!(streams.len(), 1, "one stream");
    let stream = &streams[0];
    assert_eq!(probed["format"]["format_name"], "wav");
    assert_eq!(stream["codec_type"], "audio");
    assert_eq!(stream["codec_name"], "pcm_s16le");
    assert_eq!(stream["sample_rate"], "8000");
    assert_eq!(stream["channels"], 1);
    assert_eq!(stream["bits_per_sample"], 16);
    assert_eq!(stream["duration_ts"], 800);
    evidence.line(
        "ffprobe: read tests/fixtures/tone-8k-mono-s16.wav: wav, pcm_s16le, 8000 Hz, 1 channel, 16 bit, 800 samples"
            .into(),
    );
    evidence.save();
}

/// Encode one second of `source` (a lavfi graph) through `vf` with `h264_mf` into `name`.
fn encode(ffmpeg: &Path, dir: &Path, source: &str, vf: Option<&str>, name: &str) {
    let mut args = vec![
        "-hide_banner",
        "-v",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        source,
    ];
    if let Some(vf) = vf {
        args.extend(["-vf", vf]);
    }
    args.extend([
        "-c:v", "h264_mf", "-b:v", "2M", "-pix_fmt", "nv12", "-an", name,
    ]);
    run(ffmpeg, dir, &args);
    let size = std::fs::metadata(dir.join(name))
        .expect("the output exists")
        .len();
    assert!(size > 0, "{name} is empty");
}

/// Decode `name` back with the same binary: its video codec must be H.264, and every frame is
/// returned as 320x240 gray8.
fn decode(ffmpeg: &Path, dir: &Path, name: &str) -> Vec<u8> {
    let raw = format!("{name}.gray");
    let (_, stderr) = run(
        ffmpeg,
        dir,
        &[
            "-hide_banner",
            "-y",
            "-i",
            name,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            &raw,
        ],
    );
    assert!(
        stderr.contains("Video: h264"),
        "{name} does not probe back as H.264:\n{stderr}"
    );
    let bytes = std::fs::read(dir.join(&raw)).expect("decoded frames");
    assert_eq!(
        bytes.len(),
        W * H * FRAMES,
        "{name} decodes to {} bytes, not {FRAMES} frames of {W}x{H}",
        bytes.len()
    );
    bytes
}

/// Pixels brighter than mid-grey in the last frame, within `rows`.
fn bright(frames: &[u8], rows: std::ops::Range<usize>) -> usize {
    let last = &frames[W * H * (FRAMES - 1)..];
    rows.flat_map(|y| &last[y * W..(y + 1) * W])
        .filter(|&&value| value > 128)
        .count()
}

#[test]
fn the_built_ffmpeg_matches_its_row_and_passes_the_four_export_smokes() {
    let Some(ffmpeg) = built(FFMPEG_ENV) else {
        return;
    };
    let mut evidence = Evidence::new("promote-ffmpeg.txt");
    check_identity(&mut evidence, &ffmpeg, FFMPEG_ROW);
    evidence.line(
        "encoder: h264_mf through Media Foundation; on a hosted runner this is the software encoder"
            .into(),
    );

    // Everything is named relative to this directory, so no filter argument carries a drive
    // letter (the colon would need filter-graph escaping).
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    let black = "color=c=black:s=320x240:r=25:d=1";

    // 1. h264_mf alone: a test pattern is bright all over.
    encode(
        &ffmpeg,
        dir,
        "testsrc=size=320x240:rate=25:duration=1",
        None,
        "plain.mp4",
    );
    let plain = decode(&ffmpeg, dir, "plain.mp4");
    assert!(
        bright(&plain, 0..H) > W * H / 10,
        "the test pattern is not there"
    );
    evidence.line(format!(
        "h264_mf: plain.mp4, H.264, {FRAMES} frames, pattern present"
    ));

    // The same black clip without any filter: nothing bright. The two text smokes compare to it.
    encode(&ffmpeg, dir, black, None, "black.mp4");
    let none = bright(&decode(&ffmpeg, dir, "black.mp4"), 0..H);
    assert_eq!(none, 0, "the black clip has bright pixels");

    // 2. subtitles (libass): one white line for the whole second, at the bottom.
    std::fs::write(
        dir.join("subs.ass"),
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 240\n\n\
         [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, OutlineColour, BackColour, Bold, Alignment, MarginV\n\
         Style: Default,Arial,36,&H00FFFFFF,&H00000000,&H00000000,1,2,20\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Text\n\
         Dialogue: 0,0:00:00.00,0:00:05.00,Default,SUBTITLE\n",
    )
    .unwrap();
    encode(
        &ffmpeg,
        dir,
        black,
        Some("subtitles=subs.ass"),
        "subtitles.mp4",
    );
    let burned = bright(&decode(&ffmpeg, dir, "subtitles.mp4"), H / 2..H);
    assert!(
        burned > 200,
        "no subtitle was burned in ({burned} bright pixels)"
    );
    evidence.line(format!(
        "subtitles: subtitles.mp4, H.264, {FRAMES} frames, {burned} bright pixels in the lower half"
    ));

    // 3. drawtext (FreeType + HarfBuzz): the build has no fontconfig, so the font is a file.
    let fonts = PathBuf::from(std::env::var_os("WINDIR").expect("WINDIR")).join("Fonts");
    std::fs::copy(fonts.join("arial.ttf"), dir.join("font.ttf")).expect("copy Arial");
    encode(
        &ffmpeg,
        dir,
        black,
        Some("drawtext=fontfile=font.ttf:text=CAPTION:fontcolor=white:fontsize=40:x=(w-tw)/2:y=20"),
        "drawtext.mp4",
    );
    let drawn = bright(&decode(&ffmpeg, dir, "drawtext.mp4"), 0..H / 2);
    assert!(drawn > 200, "no caption was drawn ({drawn} bright pixels)");
    evidence.line(format!(
        "drawtext: drawtext.mp4, H.264, {FRAMES} frames, {drawn} bright pixels in the upper half"
    ));

    // 4. zscale + tonemap: a PQ / bt2020 tagged 10-bit pattern down to bt709.
    encode(
        &ffmpeg,
        dir,
        "testsrc2=s=320x240:r=25:d=1,format=yuv420p10le,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc:range=tv",
        Some("zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"),
        "tonemapped.mp4",
    );
    let mapped = bright(&decode(&ffmpeg, dir, "tonemapped.mp4"), 0..H);
    assert!(mapped > 0, "the tone-mapped clip is black");
    evidence.line(format!(
        "zscale+tonemap: tonemapped.mp4, H.264, {FRAMES} frames, {mapped} bright pixels"
    ));
    evidence.save();
}
