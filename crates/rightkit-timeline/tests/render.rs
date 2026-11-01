mod common;
use std::collections::BTreeMap;
use std::path::PathBuf;

use common::*;
use rightkit_timeline::plan::VideoSegment;
use rightkit_timeline::*;

// Kept unit tests: shapes an engine-level e2e cannot reasonably drive
// (audio-only timelines, unsupported track shapes, NTSC frame counts,
// missing inputs, plan serialisation). Everything assertable on delivered
// renders moved to `cutright-engine/tests/editing_e2e.rs`.

// A scene plus a voiceover longer than the video, with the last frame held.
fn held_scenario() -> TimelineStore {
    store_with(vec![
        video_source("scene", 10.0, false),
        audio_source("vo", 30.0),
        insert("c", "scene", 0, 0.0, 1.0),
        narration("n", "vo", 0.0, 0.0, 3.0, None),
    ])
}

#[test]
fn narration_longer_than_video_holds_last_frame() {
    let s = held_scenario();
    let plan = compile(s.timeline()).unwrap();
    assert_eq!(plan.total_frames, 90);
    assert_eq!(plan.video[1], VideoSegment::Hold { frames: 60 });
    let text = build_ffmpeg_command(&RenderRequest {
        plan: &plan,
        preset: Preset::Reels,
        encoder: Encoder::host_system(),
        inputs: &[
            (sid("scene"), PathBuf::from("/m/s.mov")),
            (sid("vo"), PathBuf::from("/m/v.wav")),
        ]
        .into_iter()
        .collect(),
        output: std::path::Path::new("/o.mp4"),
    })
    .unwrap()
    .display_lossy();
    assert!(text.contains("tpad=stop_mode=clone:stop=60[vout]"));
}

#[test]
fn audio_only_timeline_renders_black_video() {
    let s = store_with(vec![
        audio_source("vo", 30.0),
        narration("n", "vo", 0.0, 0.0, 1.0, None),
    ]);
    let plan = compile(s.timeline()).unwrap();
    assert_eq!(plan.video, vec![VideoSegment::Black { frames: 30 }]);
}

#[test]
fn empty_timeline_and_unsupported_shapes_are_errors() {
    let s = TimelineStore::new(FPS30);
    assert_eq!(compile(s.timeline()), Err(TimelineError::EmptyTimeline));

    let mut s = store_with(vec![
        video_source("a", 5.0, false),
        insert("c", "a", 0, 0.0, 1.0),
    ]);
    s.apply(Action::AddVideoTrack {
        track_id: "V2".into(),
    })
    .unwrap();
    s.apply(Action::InsertClip {
        track_id: "V2".into(),
        index: 0,
        clip: VideoClip {
            id: "d".into(),
            source: sid("a"),
            source_in: secs(0.0),
            source_out: secs(1.0),
        },
    })
    .unwrap();
    assert!(matches!(
        compile(s.timeline()),
        Err(TimelineError::Unsupported(_))
    ));
}

#[test]
fn missing_input_path_is_reported() {
    let s = store_with(vec![
        video_source("a", 5.0, false),
        insert("c", "a", 0, 0.0, 1.0),
    ]);
    let plan = compile(s.timeline()).unwrap();
    let r = build_ffmpeg_command(&RenderRequest {
        plan: &plan,
        preset: Preset::YouTube,
        encoder: Encoder::host_system(),
        inputs: &BTreeMap::new(),
        output: std::path::Path::new("/o.mp4"),
    });
    assert!(matches!(r, Err(TimelineError::MissingInputPath(_))));
}

#[test]
fn plan_is_deterministic_and_round_trips_json() {
    let s = held_scenario();
    let a = compile(s.timeline()).unwrap();
    let b = compile(s.timeline()).unwrap();
    assert_eq!(a, b);
    let json = serde_json::to_string(&a).unwrap();
    assert_eq!(serde_json::from_str::<RenderPlan>(&json).unwrap(), a);
}

#[test]
fn ntsc_project_frame_counts_use_one_rule() {
    let fps = Fps::new(30000, 1001).unwrap();
    let mut s = TimelineStore::new(fps);
    s.apply(video_source("a", 10.0, false)).unwrap();
    // 1.0 s of source = 29.97 frames -> 30 frames
    s.apply(insert("c", "a", 0, 0.0, 1.0)).unwrap();
    let plan = compile(s.timeline()).unwrap();
    assert_eq!(plan.total_frames, 30);
    assert_eq!(plan.audio.total, fps.frame_to_ticks(30));
}
