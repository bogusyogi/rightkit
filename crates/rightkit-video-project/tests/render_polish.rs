//! Real-ffmpeg checks kept from the polish lane: render-record binding and
//! per-occurrence caption timing — both assertable only through the
//! project-level render API with hand-built cue lists, which the engine's
//! serve boundary does not expose. Loudness targets, true-peak ceilings and
//! crop/letterbox geometry are covered end to end by
//! `cutright-engine/tests/editing_e2e.rs` and `talking_head_e2e.rs`.
//! Fails with `SKIP: <reason>` when the editing ffmpeg is not available:
//! `CUTRIGHT_FFMPEG`/`CUTRIGHT_FFPROBE`, else the staged runtime build
//! (`apps/cutright/src-tauri/resources/runtime`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use rightkit_video_project::loudness::LoudnessSetting;
use std::collections::BTreeMap;

use rightkit_timeline::{
    Action, Encoder, FitPolicy, Fps, Preset, Ticks, VideoClip, TICKS_PER_SECOND,
};
use rightkit_video_project::render::{
    execute, prepare, EncoderChoice, RenderOptions, RenderOrigin,
};
use rightkit_video_project::source::add_source;
use rightkit_video_project::{Access, MediaTools, Project};

/// The host's own H.264 encoder: the bundled LGPL ffmpeg has no libx264.
const H264: &str = if cfg!(target_os = "macos") {
    "h264_videotoolbox"
} else {
    "h264_mf"
};

fn skip_missing(what: &str) {
    let test = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned();
    let message = format!("SKIP {test}: {what}");
    if std::env::var("RIGHTKIT_REQUIRE_ASSETS").is_ok_and(|value| value == "1") {
        panic!("{message}");
    }
    eprintln!("{message}");
}

/// The editing ffmpeg pair (RIGHTKIT_FFMPEG, RIGHTKIT_FFPROBE). Missing: a SKIP line, or a failure
/// under RIGHTKIT_REQUIRE_ASSETS=1 (ci/asset-gated.json).
fn tools() -> Option<MediaTools> {
    let bin = |env: &str| {
        std::env::var_os(env)
            .map(PathBuf::from)
            .filter(|p| p.is_file())
    };
    match (bin("RIGHTKIT_FFMPEG"), bin("RIGHTKIT_FFPROBE")) {
        (Some(ffmpeg), Some(ffprobe)) => Some(MediaTools { ffmpeg, ffprobe }),
        _ => {
            skip_missing("no editing ffmpeg (looked at RIGHTKIT_FFMPEG and RIGHTKIT_FFPROBE)");
            None
        }
    }
}

fn run(tool: &Path, args: &[&str]) -> std::process::Output {
    let o = Command::new(tool).args(args).output().unwrap();
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

/// 4 s, 1920x1080@30: red with a green centre band (wider than a 9:16 crop), and a quiet sine.
fn make_source(t: &MediaTools, dir: &Path, audio_filter: &str) -> PathBuf {
    let out = dir.join("talk.mp4");
    run(
        &t.ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=1920x1080:r=30:d=4,drawbox=x=576:y=0:w=768:h=1080:color=green:t=fill",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:sample_rate=48000:duration=4",
            "-af",
            audio_filter,
            "-c:v",
            H264,
            "-b:v",
            "4M",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            out.to_str().unwrap(),
        ],
    );
    out
}

struct Rendered {
    out: PathBuf,
    report: rightkit_video_project::render::RenderReport,
}

fn render(
    t: &MediaTools,
    fit: Option<FitPolicy>,
    loudness: LoudnessSetting,
    name: &str,
    audio_filter: &str,
) -> (tempfile::TempDir, Rendered) {
    render_before_execute(t, fit, loudness, name, audio_filter, |_| {})
}

fn render_before_execute(
    t: &MediaTools,
    fit: Option<FitPolicy>,
    loudness: LoudnessSetting,
    name: &str,
    audio_filter: &str,
    before: impl FnOnce(&rightkit_video_project::render::Prepared),
) -> (tempfile::TempDir, Rendered) {
    let tmp = tempfile::tempdir().unwrap();
    let src = make_source(t, tmp.path(), audio_filter);
    let proj = tmp.path().join("proj");
    Project::init(&proj, Some("t"), Fps { num: 30, den: 1 }).unwrap();
    let mut p = Project::open(&proj, Access::Write).unwrap();
    let r = add_source(&mut p, t, &src, false, false).unwrap();
    p.edit(Action::InsertClip {
        track_id: "V1".into(),
        index: 0,
        clip: VideoClip {
            id: "c1".into(),
            source: r.id.clone(),
            source_in: Ticks::ZERO,
            source_out: Ticks(4 * TICKS_PER_SECOND),
        },
    })
    .unwrap();
    let opts = RenderOptions {
        preset: Preset::Reels,
        encoder: EncoderChoice::Explicit(Encoder::host_system()),
        out_name: Some(name.into()),
        job: "j".into(),
        timeout: Duration::from_secs(120),
        cancel: Arc::new(AtomicBool::new(false)),
        font: None,
        fit,
        loudness,
        captions: Vec::new(),
        caption_style: None,
        caption_words: Vec::new(),
        video_pre: BTreeMap::new(),
        video_post: Vec::new(),
        audio_post: Vec::new(),
        music_duck_windows: Vec::new(),
        encoder_args: Vec::new(),
        origin: RenderOrigin::Main,
    };
    let prepared = prepare(&p, t, &opts, "macos").unwrap();
    before(&prepared);
    let report = execute(&prepared, t, &opts, &mut |_| {}).unwrap();
    let out = report.output.clone();
    (tmp, Rendered { out, report })
}

#[test]
fn render_job_uses_a_pid_scoped_partial_and_publishes() {
    let Some(t) = tools() else { return };
    let (_tmp, rendered) = render_before_execute(
        &t,
        Some(FitPolicy::Letterbox),
        LoudnessSetting::Off,
        "sweep",
        "volume=0.04",
        |prepared| {
            let leaf = prepared.partial_path.file_name().unwrap().to_str().unwrap();
            assert!(leaf.starts_with(&format!(".sweep.mp4.rktmp-{}-", std::process::id())));
            assert!(leaf.ends_with("-0"));
        },
    );
    assert!(rendered.report.bytes > 0);
    let info = rightkit_video_project::probe::probe_file(&t, &rendered.out).unwrap();
    assert!(info.duration.0 > 0);
}

fn font() -> PathBuf {
    std::env::var_os("RIGHTKIT_FONT")
        .map(PathBuf::from)
        .into_iter()
        .chain([
            PathBuf::from("/System/Library/Fonts/Monaco.ttf"),
            PathBuf::from("C:\\Windows\\Fonts\\consola.ttf"),
        ])
        .find(|p| p.is_file())
        .unwrap_or_else(|| {
            eprintln!("SKIP: no caption font");
            panic!("render_polish not run: no caption font");
        })
}

/// Bright-pixel count in the bottom third of frame `t` seconds into `file`.
fn caption_band_bright(tools: &MediaTools, file: &Path, t: f64) -> usize {
    let o = run(
        &tools.ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &format!("{t}"),
            "-i",
            file.to_str().unwrap(),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ],
    );
    let rgb = o.stdout;
    let (w, h) = (1920usize, 1080usize);
    assert_eq!(rgb.len(), w * h * 3, "frame size");
    (h * 2 / 3..h)
        .flat_map(|y| (0..w).map(move |x| (y, x)))
        .filter(|&(y, x)| {
            let p = &rgb[(y * w + x) * 3..];
            p[0] > 200 && p[1] > 200 && p[2] > 200
        })
        .count()
}

#[test]
fn render_records_bind_export_hash_revision_and_time_map() {
    let Some(t) = tools() else { return };
    let (tmp, r) = render(
        &t,
        Some(FitPolicy::Letterbox),
        LoudnessSetting::Off,
        "recorded",
        "volume=0.04",
    );
    let _keep = &tmp;
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&r.report.record_path).unwrap()).unwrap();
    assert_eq!(record["schema"], "cutright.render.v1", "{record}");
    assert_eq!(record["origin"]["kind"], "main", "{record}");
    assert_eq!(
        record["output_blake3"].as_str().unwrap(),
        rightkit_fs::digest::blake3_file(&r.out).unwrap(),
    );
    assert!(
        record["origin"]["timeline_head_hash"]
            .as_str()
            .unwrap()
            .len()
            == 64
    );
    let map = record["time_map"].as_array().unwrap();
    let video: Vec<_> = map.iter().filter(|r| r["kind"] == "video").collect();
    assert_eq!(video.len(), 1, "{map:?}");
    assert_eq!(video[0]["out_in_ms"], 0);
    assert_eq!(video[0]["out_end_ms"], 4000);
    assert_eq!(video[0]["source_in_ms"], 0);
    assert_eq!(video[0]["source_out_ms"], 4000);
}

/// A repeated source on the timeline: captions must fire at BOTH placements'
/// output times and stay dark in the gap — proven on the rendered pixels.
#[test]
fn burned_captions_fire_at_each_occurrences_output_time() {
    let Some(t) = tools() else { return };
    let font = font();
    let tmp = tempfile::tempdir().unwrap();
    let src = make_source(&t, tmp.path(), "volume=0.04");
    let proj = tmp.path().join("proj");
    Project::init(&proj, Some("t"), Fps { num: 30, den: 1 }).unwrap();
    let mut p = Project::open(&proj, Access::Write).unwrap();
    let r = add_source(&mut p, &t, &src, false, false).unwrap();
    // Same source used twice with a gap of black between (out 0-1s, 1-2s, 2-3s).
    for (i, (id, si, so)) in [("c1", 0.0, 1.0), ("c2", 1.0, 2.0), ("c3", 0.0, 1.0)]
        .iter()
        .enumerate()
    {
        p.edit(Action::InsertClip {
            track_id: "V1".into(),
            index: i,
            clip: VideoClip {
                id: id.to_string(),
                source: r.id.clone(),
                source_in: Ticks((si * TICKS_PER_SECOND as f64) as i64),
                source_out: Ticks((so * TICKS_PER_SECOND as f64) as i64),
            },
        })
        .unwrap();
    }
    // Cues as burn_caption_cues would map them: occurrence windows on output.
    let cue = |start: u64, end: u64, text: &str| rightkit_captions::Cue {
        start_ms: start,
        end_ms: end,
        lines: vec![text.to_string()],
        first_word: 0,
        last_word: 0,
    };
    let opts = RenderOptions {
        preset: Preset::YouTube,
        encoder: EncoderChoice::Explicit(Encoder::host_system()),
        out_name: Some("capped".into()),
        job: "j".into(),
        timeout: Duration::from_secs(120),
        cancel: Arc::new(AtomicBool::new(false)),
        font: Some(font),
        fit: None,
        loudness: LoudnessSetting::Off,
        captions: vec![cue(100, 900, "FIRST"), cue(2100, 2900, "SECOND")],
        caption_style: None,
        caption_words: Vec::new(),
        video_pre: BTreeMap::new(),
        video_post: Vec::new(),
        audio_post: Vec::new(),
        music_duck_windows: Vec::new(),
        encoder_args: Vec::new(),
        origin: RenderOrigin::Main,
    };
    let prepared = prepare(&p, &t, &opts, "macos").unwrap();
    let report = execute(&prepared, &t, &opts, &mut |_| {}).unwrap();
    let at = |t_sec: f64| caption_band_bright(&t, &report.output, t_sec);
    assert!(at(0.5) > 200, "first cue window is dark");
    assert_eq!(at(1.5), 0, "between placements must stay dark");
    assert!(at(2.5) > 200, "second occurrence window is dark");
}
