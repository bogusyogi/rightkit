//! End-to-end tests with the real YuNet model on ONNX Runtime and the editing ffmpeg.
//!
//! Needs (a) the editing ffmpeg/ffprobe — `RIGHTKIT_FFMPEG`/`RIGHTKIT_FFPROBE`, (b) an ONNX
//! Runtime library — `RIGHTKIT_ORT_DYLIB` (see `ort_rt::dylib_candidates`), and (c) network
//! once for the pinned model (cached in the shared Orthic Labs store). A missing prerequisite
//! prints `SKIP <test>: ...`, or fails under `RIGHTKIT_REQUIRE_ASSETS=1` (ci/asset-gated.json).

use rightkit_reframe::analyze::{analyze, AnalyzeOptions, Evidence};
use rightkit_reframe::detect::YuNet;
use rightkit_reframe::media::{probe, Tools};
use rightkit_reframe::models::ensure_yunet;
use rightkit_reframe::plan::{plan_from_evidence, PlanMode, PlanParams, ReframePlan};
use rightkit_reframe::render::{render, RenderOptions};
use rightkit_reframe::ReframeError;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The host's own H.264 encoder: the bundled LGPL ffmpeg has no libx264.
const H264: &str = if cfg!(target_os = "macos") {
    "h264_videotoolbox"
} else {
    "h264_mf"
};

/// The editing ffmpeg and ffprobe, or a SKIP line (a failure under RIGHTKIT_REQUIRE_ASSETS=1).
fn tools() -> Option<Tools> {
    if let (Some(f), Some(p)) = (
        std::env::var_os("RIGHTKIT_FFMPEG"),
        std::env::var_os("RIGHTKIT_FFPROBE"),
    ) {
        return Some(Tools::new(f, p));
    }
    let test = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned();
    let message =
        format!("SKIP {test}: no editing ffmpeg (looked at RIGHTKIT_FFMPEG and RIGHTKIT_FFPROBE)");
    if std::env::var("RIGHTKIT_REQUIRE_ASSETS").is_ok_and(|value| value == "1") {
        panic!("{message}");
    }
    eprintln!("{message}");
    None
}

#[test]
fn reframe_job_sweeps_crash_temps_before_publishing() {
    let Some(tools) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = fixture("talk.mp4");
    let before = rightkit_reframe::media::blake3_file(&src).unwrap();
    let ev = analyse(&tools, &src, &AnalyzeOptions::default());
    let mut plan = plan_from_evidence(&ev, &PlanParams::default()).unwrap();
    plan.approve_all("sweep-test", chrono::Utc::now()).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--list")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    assert!(child.wait().unwrap().success());
    let orphan = dir.path().join(format!(".vertical.mp4.rktmp-{pid}-0-0"));
    std::fs::write(&orphan, b"interrupted encode").unwrap();
    let out = dir.path().join("vertical.mp4");
    let receipt = render(&tools, &src, &plan, &out, &RenderOptions::default()).unwrap();
    assert!(!orphan.exists(), "reframe job left its crash orphan");
    let info = probe(&tools, &out).unwrap();
    assert_eq!(
        (info.width, info.height),
        (plan.output_width, plan.output_height),
    );
    assert_eq!(
        receipt.output_blake3,
        rightkit_reframe::media::blake3_file(&out).unwrap(),
    );
    assert_eq!(before, rightkit_reframe::media::blake3_file(&src).unwrap());
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run_ffmpeg(tools: &Tools, args: &[&str]) {
    let out = Command::new(&tools.ffmpeg)
        .args(["-v", "error", "-y"])
        .args(args)
        .output()
        .expect("spawn ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 1280x720 25fps 8s: a face image drifting left to right across a flat background.
fn moving_face_video(tools: &Tools, out: &Path) {
    let face = fixture("face.jpg");
    run_ffmpeg(
        tools,
        &[
            "-f", "lavfi", "-i", "color=c=0x30404f:s=1280x720:r=25:d=8",
            "-loop", "1", "-framerate", "25", "-t", "8", "-i", face.to_str().unwrap(),
            "-filter_complex",
            "[1:v]scale=220:220[f];[0:v][f]overlay=x='100+(t/8)*860':y=250:shortest=1,format=yuv420p",
            "-c:v", H264, "-b:v", "4M", out.to_str().unwrap(),
        ],
    );
}

/// An isolated models root (never the user's real shared store: `ensure_yunet`
/// registers a ref). Only the YuNet model is cloned from the real store
/// (copy-on-write on macOS), so an installed YuNet is reused without a download
/// and the root stays a few hundred KB. The root lives under the test cache, not
/// the system temp, and roots left by dead test processes are removed first.
fn isolated_models_root() -> &'static std::path::Path {
    static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let parent = suite_parent();
        sweep_dead(&parent);
        let suite = tempfile::Builder::new()
            .prefix(&format!("reframe-{}-", std::process::id()))
            .tempdir_in(&parent)
            .expect("isolated suite root")
            .keep();
        // rightkit-models test-mode refuses the real per-user suite root for every store call,
        // including ensure_ready on an explicit root. Redirect the resolver to this temp suite
        // before any detector use; only this OnceLock init sets it, and every other path into
        // the resolver is reached through detector(), which waits on this init.
        std::env::set_var("RIGHTKIT_SUITE_ROOT", &suite);
        let dir = suite.join("models");
        std::fs::create_dir_all(&dir).expect("isolated models dir");
        // Seed only from an explicit source directory (the folder that holds
        // `yunet-face-detection-2023mar`). The real per-user store is never resolved here:
        // rightkit-models test-mode refuses it. Unset, ensure_yunet downloads and verifies
        // the model into this isolated root.
        if let Some(seed) = std::env::var_os("CUTRIGHT_YUNET_SEED").map(std::path::PathBuf::from) {
            let source = seed.join(YUNET_MODEL_DIR);
            if cfg!(target_os = "macos") && source.is_dir() {
                let _ = std::process::Command::new("cp")
                    .args(["-c", "-R"])
                    .arg(&source)
                    .arg(dir.join(YUNET_MODEL_DIR))
                    .status();
            }
        }
        dir
    })
}

/// The YuNet model's directory name in the shared store.
const YUNET_MODEL_DIR: &str = "yunet-face-detection-2023mar";

fn suite_parent() -> std::path::PathBuf {
    let parent = match std::env::var_os("HOME") {
        Some(home) if cfg!(target_os = "macos") => {
            std::path::PathBuf::from(home).join("Library/Caches/rightkit-reframe-e2e")
        }
        _ => std::env::temp_dir().join("rightkit-reframe-e2e"),
    };
    std::fs::create_dir_all(&parent).expect("suite parent");
    parent
}

/// Removes roots left by reframe test processes that no longer exist.
fn sweep_dead(parent: &std::path::Path) {
    for e in std::fs::read_dir(parent).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_prefix("reframe-")
            .and_then(|r| r.split('-').next())
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        let alive = cfg!(unix)
            && std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
        if !alive && cfg!(unix) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn detector() -> YuNet {
    let model = ensure_yunet(isolated_models_root(), "rightkit-test").expect("YuNet model");
    YuNet::load(&model)
        .expect("load YuNet (is an ONNX Runtime library available? set CUTRIGHT_ORT_DYLIB)")
}

fn analyse(tools: &Tools, media: &Path, opts: &AnalyzeOptions) -> Evidence {
    let mut det = detector();
    let info = det.info();
    eprintln!(
        "execution provider: {} (requested {})",
        info.execution_provider, info.requested_provider
    );
    analyze(tools, media, &mut det, info, opts).expect("analyze")
}

/// Gate error paths an engine-level e2e cannot reach: a tampered plan fails its
/// approval hash, a changed source fails its bound hash, and a retry is idempotent.
#[test]
fn tampered_plan_or_changed_source_refuses_the_render() {
    let Some(tools) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("moving.mp4");
    moving_face_video(&tools, &src);

    let ev = analyse(&tools, &src, &AnalyzeOptions::default());
    let plan = plan_from_evidence(&ev, &PlanParams::default()).unwrap();
    assert_eq!(plan.mode, PlanMode::Tracked, "{:?}", plan.fallback);

    let out = dir.path().join("vertical.mp4");
    let err = render(&tools, &src, &plan, &out, &RenderOptions::default()).unwrap_err();
    assert!(matches!(err, ReframeError::NotApproved(_)), "{err}");
    assert!(!out.exists());

    let mut approved: ReframePlan = plan.clone();
    approved.approve_all("adrian", chrono::Utc::now()).unwrap();
    let mut tampered = approved.clone();
    tampered.keyframes[3].x += 20;
    assert!(matches!(
        render(&tools, &src, &tampered, &out, &RenderOptions::default()).unwrap_err(),
        ReframeError::NotApproved(_)
    ));
    assert!(!out.exists());

    let receipt = render(&tools, &src, &approved, &out, &RenderOptions::default()).expect("render");
    assert!(!receipt.reused);

    // Idempotent retry reuses the identical output.
    let again = render(&tools, &src, &approved, &out, &RenderOptions::default()).unwrap();
    assert!(again.reused);
    assert_eq!(again.output_blake3, receipt.output_blake3);

    // The source is bound by hash: a changed source is refused.
    let other = dir.path().join("other.mp4");
    run_ffmpeg(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=1280x720:r=25:d=2",
            "-pix_fmt",
            "yuv420p",
            other.to_str().unwrap(),
        ],
    );
    assert!(matches!(
        render(
            &tools,
            &other,
            &approved,
            &dir.path().join("x.mp4"),
            &RenderOptions::default()
        )
        .unwrap_err(),
        ReframeError::SourceChanged { .. }
    ));
}

#[test]
fn a_scene_without_faces_falls_back_to_a_reported_centre_crop() {
    let Some(tools) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("empty.mp4");
    run_ffmpeg(
        &tools,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=0x305040:s=1280x720:r=25:d=4",
            "-pix_fmt",
            "yuv420p",
            src.to_str().unwrap(),
        ],
    );
    let ev = analyse(&tools, &src, &AnalyzeOptions::default());
    let plan = plan_from_evidence(&ev, &PlanParams::default()).unwrap();
    assert_eq!(plan.mode, PlanMode::CentreFallback);
    let fb = plan.fallback.as_ref().expect("fallback must be reported");
    assert!(fb.reason.contains("tracked in only"), "{}", fb.reason);
    let centre = (1280 - plan.crop_width) / 2;
    assert!((plan.keyframes[0].x as i64 - centre as i64).abs() <= 1);
    assert_eq!(plan.keyframes[0].x % 2, 0);

    // A fallback plan still needs approval before it renders.
    let out = dir.path().join("v.mp4");
    assert!(render(&tools, &src, &plan, &out, &RenderOptions::default()).is_err());
    let mut approved = plan.clone();
    approved.approve_all("adrian", chrono::Utc::now()).unwrap();
    render(&tools, &src, &approved, &out, &RenderOptions::default()).unwrap();
    assert_eq!(probe(&tools, &out).unwrap().height, 1920);
}

/// Two faces: the left one is a real talking-head clip (mouth moving), the right one a
/// frozen frame of it. With speech present the crop must settle on the talker.
#[test]
fn the_active_speaker_is_the_face_whose_mouth_moves() {
    let Some(tools) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("two.mp4");
    let talk = fixture("talk.mp4");
    run_ffmpeg(
        &tools,
        &[
            "-f", "lavfi", "-i", "color=c=0x30404f:s=1280x720:r=25:d=6",
            "-i", talk.to_str().unwrap(),
            "-i", fixture("face.jpg").to_str().unwrap(),
            "-filter_complex",
            "[1:v]scale=300:300,fps=25[a];[2:v]scale=300:300[b];[0:v][a]overlay=x=150:y=210:shortest=1[t];[t][b]overlay=x=830:y=210,format=yuv420p",
            "-t", "6", "-c:v", H264, "-b:v", "4M", src.to_str().unwrap(),
        ],
    );
    let left_cx = (150.0 + 150.0) / 1280.0;
    let right_cx = (830.0 + 150.0) / 1280.0;

    let speaking = analyse(
        &tools,
        &src,
        &AnalyzeOptions {
            speech: Some(vec![(0, 6000)]),
            ..Default::default()
        },
    );
    let params = PlanParams {
        min_tracked_fraction: 0.1,
        ..PlanParams::default()
    };
    let plan = plan_from_evidence(&speaking, &params).unwrap();
    assert_eq!(plan.mode, PlanMode::Tracked);
    let late: Vec<_> = plan.keyframes.iter().filter(|k| k.t_ms > 2500).collect();
    assert!(!late.is_empty());
    let mean_cx: f64 = late.iter().map(|k| k.center_x).sum::<f64>() / late.len() as f64;
    assert!(
        (mean_cx - left_cx).abs() < (mean_cx - right_cx).abs() && mean_cx < 0.45,
        "crop settled at {mean_cx:.2}; talker is at {left_cx:.2}, listener at {right_cx:.2}"
    );
    assert!(plan.stats.active_speaker_samples > plan.stats.samples / 2);

    // No speech at all -> no active-speaker evidence; both faces are treated alike.
    let silent = analyse(
        &tools,
        &src,
        &AnalyzeOptions {
            speech: Some(vec![]),
            ..Default::default()
        },
    );
    let plan = plan_from_evidence(&silent, &params).unwrap();
    assert_eq!(plan.stats.active_speaker_samples, 0);
}
