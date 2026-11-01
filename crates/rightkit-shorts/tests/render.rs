//! Real render of one proposal from the synthetic talking-head clip with the editing
//! ffmpeg. Skipped (with a note) when the clip or the ffmpeg build is missing.
//!
//! The transcript is hand-written to the clip's script (qualify-out/m2-input/README.txt),
//! with word timings spread across the speech blocks that `silencedetect` finds in the
//! clip's audio (the 7 sentences, with the "Um,"/"Uh,"/"Hi," pauses). It is an
//! approximation of a recogniser's output, not a recording of one.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use rightkit_shorts::*;
use rightkit_timeline::{Encoder, Fps};
use rightkit_transcript::{EndSource, ModelRef, Span, Transcript, Word, LEGACY_SCHEMA as SCHEMA};
use rightkit_video_project::loudness::LoudnessSetting;
use rightkit_video_project::render::{execute, prepare, EncoderChoice, RenderOptions};
use rightkit_video_project::source::add_source;
use rightkit_video_project::{Access, MediaTools, Project};

fn tools() -> Option<MediaTools> {
    let t = MediaTools {
        ffmpeg: std::env::var_os("RIGHTKIT_FFMPEG").map(PathBuf::from)?,
        ffprobe: std::env::var_os("RIGHTKIT_FFPROBE").map(PathBuf::from)?,
    };
    (t.ffmpeg.is_file() && t.ffprobe.is_file()).then_some(t)
}

/// (words, start_ms, end_ms): each group's words are spread over its span in
/// proportion to their length, with a 30 ms gap between words.
const GROUPS: &[(&str, u64, u64)] = &[
    ("Hi,", 60, 560),
    ("I'm testing CutRight today.", 740, 2410),
    ("Um,", 3020, 3380),
    ("so the first thing to know is that", 3670, 5400),
    ("editing takes too long.", 5440, 7000),
    ("Uh,", 7440, 7800),
    ("every recording has pauses and mistakes.", 7840, 10200),
    ("The best part of this tool is the speed.", 10800, 13060),
    (
        "The best part of this tool is how fast it is.",
        13600,
        16110,
    ),
    ("Um,", 16630, 17090),
    ("let's see how well it cleans this up.", 17370, 19110),
    ("Thanks for watching.", 19690, 20420),
];

fn hand_transcript(blake3: &str, duration_ms: u64) -> Transcript {
    let mut words = Vec::new();
    for &(text, s, e) in GROUPS {
        let toks: Vec<&str> = text.split_whitespace().collect();
        let gaps = 30 * (toks.len() as u64 - 1);
        let total_len: u64 = toks.iter().map(|t| t.len() as u64).sum();
        let avail = e - s - gaps;
        let mut t = s;
        for (i, tok) in toks.iter().enumerate() {
            let len = if i + 1 == toks.len() {
                e - t
            } else {
                avail * tok.len() as u64 / total_len
            };
            words.push(Word {
                text: (*tok).into(),
                start_ms: t,
                end_ms: t + len,
                confidence: Some(0.95),
                end_source: EndSource::Model,
            });
            t += len + 30;
        }
    }
    let vad = GROUPS
        .iter()
        .map(|&(_, s, e)| Span {
            start_ms: s,
            end_ms: e,
        })
        .collect();
    Transcript {
        schema: SCHEMA.into(),
        source_blake3: blake3.into(),
        language: "en".into(),
        duration_ms,
        words,
        vad,
        model: ModelRef {
            id: "hand-written".into(),
            revision: "1".into(),
            backend: "synthetic".into(),
        },
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

#[test]
fn render_the_top_proposal_from_the_synthetic_talking_head() {
    let clip = std::env::var_os("RIGHTKIT_TALKING_HEAD_CLIP")
        .map(PathBuf::from)
        .unwrap_or_default();
    let font = std::env::var_os("RIGHTKIT_FONT")
        .map(PathBuf::from)
        .unwrap_or_default();
    let (Some(t), true, true) = (tools(), clip.is_file(), font.is_file()) else {
        let message = "SKIP render_the_top_proposal_from_the_synthetic_talking_head: editing ffmpeg, the talking-head clip or the font is missing (looked at RIGHTKIT_FFMPEG, RIGHTKIT_FFPROBE, RIGHTKIT_TALKING_HEAD_CLIP, RIGHTKIT_FONT)";
        if std::env::var("RIGHTKIT_REQUIRE_ASSETS").is_ok_and(|value| value == "1") {
            panic!("{message}");
        }
        eprintln!("{message}");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("proj");
    Project::init(&dir, Some("shorts"), Fps::new(30, 1).unwrap()).unwrap();
    let mut project = Project::open(&dir, Access::Write).unwrap();
    let reg = add_source(&mut project, &t, &clip, false, false).unwrap();
    let info = reg.info.clone();

    // The clip's audio is 20.64 s (the file is 22.63 s: video runs past the audio).
    let tr = hand_transcript(info.id.as_str(), 20_640);
    tr.validate().unwrap();

    let proposals = propose_shorts(&ProposeInput::new(&tr), &ShortsProfile::default(), 3).unwrap();
    assert!(!proposals.proposals.is_empty());
    for p in &proposals.proposals {
        eprintln!(
            "{} rank {} {}..{} composite {:.3} title {:?}\n   {}",
            p.id, p.rank, p.start_ms, p.end_ms, p.scores.composite, p.title, p.rationale
        );
    }
    let top = &proposals.proposals[0];

    let fps = Fps::new(30, 1).unwrap();
    let plan = build_short(top, &tr, &info, fps, &ShortOptions::default(), None).unwrap();
    eprintln!(
        "variant {} frames {} kept {:?}",
        plan.variant_id,
        plan.output_frames,
        plan.cut_plan
            .kept
            .iter()
            .map(|k| (k.start_ms(), k.end_ms()))
            .collect::<Vec<_>>()
    );
    for d in &plan.cut_plan.decisions {
        eprintln!(
            "  cut {:?} {}..{} {:?}",
            d.kind, d.start_ms, d.end_ms, d.text
        );
    }
    eprintln!("{}", plan.srt);

    // Persist the variant as its own timeline document and reload it.
    let variant = plan.variant_store().unwrap();
    let doc = dir.join(format!("{}.timeline.json", plan.variant_id));
    std::fs::write(&doc, variant.to_canonical_json()).unwrap();
    let reloaded =
        rightkit_timeline::TimelineStore::from_canonical_json(&std::fs::read(&doc).unwrap())
            .unwrap();
    assert_eq!(reloaded.head_hash(), variant.head_hash());

    // Apply the same actions to the project (the render path reads the project's store).
    for a in &plan.actions {
        if matches!(a, rightkit_timeline::Action::RegisterSource { .. }) {
            continue; // already registered by add_source
        }
        project.edit(a.clone()).unwrap();
    }
    let req = &plan.request;
    let opts = RenderOptions {
        preset: req.preset,
        encoder: EncoderChoice::Explicit(Encoder::host_system()),
        out_name: Some(req.out_name.clone()),
        job: "shorts".into(),
        timeout: Duration::from_secs(180),
        cancel: Arc::new(AtomicBool::new(false)),
        font: Some(font),
        fit: Some(req.fit.clone()),
        loudness: if req.loudness_normalize {
            LoudnessSetting::default()
        } else {
            LoudnessSetting::Off
        },
        captions: Vec::new(),
        caption_style: None,
        caption_words: Vec::new(),
        video_pre: Default::default(),
        video_post: Vec::new(),
        audio_post: Vec::new(),
        music_duck_windows: Vec::new(),
        encoder_args: Vec::new(),
        origin: rightkit_video_project::render::RenderOrigin::Main,
    };
    let prepared = prepare(&project, &t, &opts, "macos").unwrap();
    let report = execute(&prepared, &t, &opts, &mut |_| {}).unwrap();
    eprintln!("rendered {}", report.output.display());

    // Probe: 1080x1920, duration = the plan's, audio present.
    let probe = run(
        &t.ffprobe,
        &[
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,width,height,duration",
            "-of",
            "csv=p=0",
            report.output.to_str().unwrap(),
        ],
    );
    let probe = String::from_utf8_lossy(&probe.stdout).into_owned();
    eprintln!("probe: {probe}");
    assert!(probe.contains("1080,1920"), "{probe}");
    assert!(probe.contains("audio"), "{probe}");
    let plan_s = plan.output_frames as f64 / 30.0;
    let video_s: f64 = probe
        .lines()
        .find(|l| l.starts_with("video"))
        .and_then(|l| l.split(',').nth(3))
        .and_then(|s| s.parse().ok())
        .unwrap();
    assert!(
        (video_s - plan_s).abs() < 0.1,
        "{video_s} vs planned {plan_s}"
    );
    assert!(
        !plan.request.deliverable,
        "centre-crop placeholder must not be deliverable"
    );

    // Frames at the middle of the first and last caption, for visual inspection.
    let keep = std::env::var_os("RIGHTKIT_SHORTS_KEEP").map(PathBuf::from);
    let frames = keep.clone().unwrap_or_else(|| tmp.path().to_path_buf());
    std::fs::create_dir_all(&frames).unwrap();
    let mut stamps = vec![plan.cues[0].start_ms, plan.cues.last().unwrap().start_ms];
    stamps.push(plan.cues[plan.cues.len() / 2].start_ms);
    for (n, ms) in stamps.iter().enumerate() {
        let png = frames.join(format!("{}-frame{n}.png", plan.variant_id));
        let at = format!("{:.3}", (*ms as f64 + 250.0) / 1000.0);
        run(
            &t.ffmpeg,
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-ss",
                &at,
                "-i",
                report.output.to_str().unwrap(),
                "-frames:v",
                "1",
                png.to_str().unwrap(),
            ],
        );
        assert!(std::fs::metadata(&png).unwrap().len() > 10_000);
        eprintln!("frame {}", png.display());
    }
    if let Some(k) = keep {
        std::fs::copy(&report.output, k.join(format!("{}.mp4", plan.variant_id))).unwrap();
        std::fs::write(k.join(format!("{}.srt", plan.variant_id)), &plan.srt).unwrap();
        std::fs::write(
            k.join("proposals.json"),
            serde_json::to_vec_pretty(&proposals).unwrap(),
        )
        .unwrap();
    }
}
