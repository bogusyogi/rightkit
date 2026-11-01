mod common;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::*;
use rightkit_ffmpeg::loudnorm::{LoudnessMeasurement, LoudnessTarget};
use rightkit_ffmpeg::{FfmpegArg, FfmpegCommand};
use rightkit_timeline::*;

fn setup() -> (RenderPlan, BTreeMap<SourceId, PathBuf>) {
    let s = store_with(vec![
        video_source("scene", 10.0, true),
        audio_source("vo", 30.0),
        insert("a", "scene", 0, 0.0, 3.0),
        narration("n", "vo", 0.0, 0.0, 3.0, None),
    ]);
    let inputs = [("scene", "/m/scene.mov"), ("vo", "/m/vo.wav")]
        .into_iter()
        .map(|(n, p)| (sid(n), PathBuf::from(p)))
        .collect();
    (compile(s.timeline()).unwrap(), inputs)
}

fn graph(cmd: &FfmpegCommand) -> String {
    for pair in cmd.args().windows(2) {
        if let [FfmpegArg::Text(flag), FfmpegArg::Text(graph)] = pair {
            if flag == "-filter_complex" {
                return graph.clone();
            }
        }
    }
    panic!("no graph")
}

fn req<'a>(
    plan: &'a RenderPlan,
    inputs: &'a BTreeMap<SourceId, PathBuf>,
    preset: Preset,
) -> RenderRequest<'a> {
    RenderRequest {
        plan,
        preset,
        encoder: Encoder::host_system(),
        inputs,
        output: Path::new("/out/x.mp4"),
    }
}

#[test]
fn reels_defaults_to_center_crop_and_youtube_to_letterbox() {
    let (plan, inputs) = setup();
    let reels = graph(&build_ffmpeg_command(&req(&plan, &inputs, Preset::Reels)).unwrap());
    assert!(
        reels.contains("crop=w=min(iw\\,ih*1080/1920):h=min(ih\\,iw*1920/1080),scale=1080:1920")
    );
    assert!(!reels.contains(",pad="), "no black bars");
    let yt = graph(&build_ffmpeg_command(&req(&plan, &inputs, Preset::YouTube)).unwrap());
    assert!(yt.contains("pad=1920:1080"));
    assert!(!yt.contains("crop="));
}

#[test]
fn explicit_letterbox_keeps_the_old_reels_graph() {
    let (plan, inputs) = setup();
    let style = RenderStyle {
        fit: FitPolicy::Letterbox,
        audio: AudioFinish::None,
        video_pre: Vec::new(),
        captions: Vec::new(),
        video_post: Vec::new(),
        audio_post: Vec::new(),
        encoder_args: Vec::new(),
    };
    let b = build_ffmpeg_render_styled(&req(&plan, &inputs, Preset::Reels), None, &style).unwrap();
    assert!(graph(&b.command).contains("pad=1080:1920"));
}

#[test]
fn face_center_builds_sendcmd_crop_per_clip() {
    let (plan, inputs) = setup();
    let style = RenderStyle {
        fit: FitPolicy::FaceCenter(FaceTracks {
            clips: BTreeMap::from([(
                "a".to_string(),
                FaceTrack {
                    anchors: vec![
                        CropAnchor {
                            start_ms: 0,
                            center_x_milli: 300,
                            center_y_milli: 500,
                        },
                        CropAnchor {
                            start_ms: 3000,
                            center_x_milli: 700,
                            center_y_milli: 500,
                        },
                    ],
                },
            )]),
        }),
        video_pre: Vec::new(),
        audio: AudioFinish::None,
        captions: Vec::new(),
        video_post: Vec::new(),
        audio_post: Vec::new(),
        encoder_args: Vec::new(),
    };
    let b = build_ffmpeg_render_styled(&req(&plan, &inputs, Preset::Reels), None, &style).unwrap();
    let g = graph(&b.command);
    assert!(g.contains("sendcmd=f="), "moving path needs sendcmd: {g}");
    assert!(g.contains("crop@fc0=w="), "{g}");
    assert!(g.contains("scale=1080:1920"), "{g}");
    // The crop positions are emitted as a command file the runner writes first.
    assert_eq!(b.text_files.len(), 1);
    assert!(b.text_files[0]
        .path
        .to_string_lossy()
        .contains(".reframe-crop-0.cmd"));
    assert!(b.text_files[0].content.contains("crop@fc0"));
}

#[test]
fn face_center_holds_still_when_the_track_is_empty() {
    let (plan, inputs) = setup();
    let style = RenderStyle {
        fit: FitPolicy::FaceCenter(FaceTracks::default()),
        video_pre: Vec::new(),
        audio: AudioFinish::None,
        captions: Vec::new(),
        video_post: Vec::new(),
        audio_post: Vec::new(),
        encoder_args: Vec::new(),
    };
    let b = build_ffmpeg_render_styled(&req(&plan, &inputs, Preset::Reels), None, &style).unwrap();
    let g = graph(&b.command);
    assert!(g.contains("crop@fc0=w="), "{g}");
    assert!(!g.contains("sendcmd"), "no movement → constant crop: {g}");
    assert!(b.text_files.is_empty());
}

#[test]
fn fit_policy_json_is_typed() {
    assert_eq!(
        serde_json::to_string(&FitPolicy::CenterCrop).unwrap(),
        r#"{"fit":"center_crop"}"#
    );
    let back: FitPolicy = serde_json::from_str(r#"{"fit":"letterbox"}"#).unwrap();
    assert_eq!(back, FitPolicy::Letterbox);
}

#[test]
fn silent_measurement_is_detected() {
    let m = |i: f64| LoudnessMeasurement {
        input_i: i,
        input_tp: -70.0,
        input_lra: 0.0,
        input_thresh: -70.0,
        target_offset: 0.0,
    };
    assert!(m(f64::NEG_INFINITY).is_silent());
    assert!(!m(-20.0).is_silent());
}

#[test]
fn finishing_stages_run_after_captions_and_loudness() {
    let (plan, inputs) = setup();
    let measured = LoudnessMeasurement {
        input_i: -23.5,
        input_tp: -4.25,
        input_lra: 3.1,
        input_thresh: -33.9,
        target_offset: 0.4,
    };
    let style = RenderStyle {
        fit: FitPolicy::Letterbox,
        video_pre: Vec::new(),
        audio: AudioFinish::Normalize {
            target: LoudnessTarget::default(),
            measured,
        },
        captions: vec!["drawtext=text='hi'".into()],
        video_post: vec!["lut3d=file='x.cube',setparams=color_primaries=bt709".into()],
        audio_post: vec!["acompressor=threshold=0.5".into()],
        encoder_args: vec!["-color_primaries".into(), "bt709".into()],
    };
    let b = build_ffmpeg_render_styled(&req(&plan, &inputs, Preset::Reels), None, &style).unwrap();
    let g = graph(&b.command);
    assert!(g.contains("[vout]drawtext=text='hi'[vcap]"), "{g}");
    assert!(
        g.contains("[vcap]lut3d=file='x.cube',setparams=color_primaries=bt709[vpost]"),
        "{g}"
    );
    // Cleanup/compression runs BEFORE loudness mastering: aout -> acompressor ->
    // acln -> loudnorm -> aln.
    assert!(g.contains("[aout]acompressor=threshold=0.5[acln]"), "{g}");
    assert!(g.contains("[acln]loudnorm=I=-14.0"), "{g}");
    let text = b.command.display_lossy();
    assert!(text.contains("-map [vpost]"), "{text}");
    assert!(text.contains("-color_primaries bt709"), "{text}");
}

#[test]
fn face_tracks_are_per_clip_occurrence_and_sampled_at_source_in() {
    // One source placed twice with different source windows: each clip gets its
    // own command file sampling ITS track at its own source_in — no shared
    // global track, no cross-clip interpolation.
    let s = store_with(vec![
        video_source("scene", 10.0, true),
        insert("c1", "scene", 0, 0.0, 1.0),
        insert("c2", "scene", 1, 4.0, 5.0),
    ]);
    let inputs = [("scene", "/m/scene.mov")]
        .into_iter()
        .map(|(n, p)| (sid(n), PathBuf::from(p)))
        .collect();
    let plan = compile(s.timeline()).unwrap();
    let track = |x0: i32, x1: i32| FaceTrack {
        anchors: vec![
            CropAnchor {
                start_ms: 0,
                center_x_milli: x0,
                center_y_milli: 500,
            },
            CropAnchor {
                start_ms: 10_000,
                center_x_milli: x1,
                center_y_milli: 500,
            },
        ],
    };
    let style = RenderStyle {
        fit: FitPolicy::FaceCenter(FaceTracks {
            clips: BTreeMap::from([
                ("c1".to_string(), track(300, 500)),
                ("c2".to_string(), track(900, 700)),
            ]),
        }),
        audio: AudioFinish::None,
        video_pre: Vec::new(),
        captions: Vec::new(),
        video_post: Vec::new(),
        audio_post: Vec::new(),
        encoder_args: Vec::new(),
    };
    let b = build_ffmpeg_render_styled(&req(&plan, &inputs, Preset::Reels), None, &style).unwrap();
    let g = graph(&b.command);
    assert!(g.contains("crop@fc0") && g.contains("crop@fc1"), "{g}");
    assert_eq!(b.text_files.len(), 2, "{:?}", b.text_files);
    // c1 samples its track at source_in=0ms → x=300; c2 samples ITS track at
    // source_in=4000ms → 900+(700-900)*0.4 = 820 — isolation between occurrences.
    let left = b
        .text_files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("reframe-crop-0"))
        .unwrap();
    let right = b
        .text_files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("reframe-crop-1"))
        .unwrap();
    // cw=606 centred on x=0.300*1920=576-303=273→274; 0.820*1920=1574-303=1271→1272.
    assert!(left.content.contains("x 274"), "{}", left.content);
    assert!(right.content.contains("x 1272"), "{}", right.content);
}
