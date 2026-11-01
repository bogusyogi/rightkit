//! End-to-end frame evaluation for durable clip visuals: keyframed transform,
//! keyframed opacity, keyframed and static crop, and a cross-fade in.
//!
//! Each assertion runs the real evaluation path (`rightkit-compose` evaluates
//! the adapted composition inside `compose_frame`) and checks the frame
//! descriptor and texture requirements a renderer would consume. No GPU and no
//! FFmpeg are involved, so these are the same numbers preview and export get
//! before they reach a decoder or compositor.

use std::collections::BTreeMap;

use rightkit_compose as rk;
use rightkit_compose::native::{
    compose_frame, ClipVisual, ComposeError, ComposeSnapshot, CropRect, OutputSpec,
    TextureRequirement,
};
use rightkit_compositor::FrameItemDescriptor;
use rightkit_timeline::model::{
    SourceId, SourceInfo, Timeline, VideoClip, VideoStreamInfo, VideoTrack,
};
use rightkit_timeline::Action;
use rightkit_timeline::{Fps, Ticks, TICKS_PER_SECOND};

const EPS: f64 = 1e-6;

fn sid(n: u8) -> SourceId {
    SourceId::new(format!("{:064x}", n)).unwrap()
}

fn src(n: u8) -> SourceInfo {
    SourceInfo {
        id: sid(n),
        duration: Ticks(60 * TICKS_PER_SECOND),
        video: Some(VideoStreamInfo {
            width: 1920,
            height: 1080,
            duration: None,
        }),
        has_audio: false,
    }
}

fn clip(id: &str, source: u8, secs: i64) -> VideoClip {
    VideoClip {
        id: id.into(),
        source: sid(source),
        source_in: Ticks(0),
        source_out: Ticks(secs * TICKS_PER_SECOND),
    }
}

/// A 30 fps timeline of contiguous clips on one video track, with sources
/// 1..=n registered in order.
fn timeline(clips: Vec<VideoClip>, sources: u8) -> Timeline {
    let mut tl = Timeline::new(Fps::new(30, 1).unwrap());
    tl.sources = (1..=sources).map(src).collect();
    tl.video_tracks = vec![VideoTrack {
        id: "V1".into(),
        clips,
    }];
    tl
}

fn snapshot(tl: &Timeline, visuals: BTreeMap<String, ClipVisual>) -> ComposeSnapshot {
    ComposeSnapshot::evaluate(
        "proj-visuals",
        1,
        "cafe",
        tl,
        OutputSpec::full(1920, 1080),
        visuals,
    )
    .unwrap()
}

/// Keyframe at `secs` seconds of clip-local time.
fn at<T>(secs: f64, value: T) -> rk::Keyframe<T> {
    rk::Keyframe {
        at: rk::Ticks((secs * rk::TICKS_PER_SECOND as f64) as u64),
        value,
        easing: rk::Easing::Linear,
    }
}

fn layer_of(item: &FrameItemDescriptor) -> &rightkit_compositor::LayerDescriptor {
    match item {
        FrameItemDescriptor::Layer(layer) => layer,
        other => panic!("expected a layer item, got {other:?}"),
    }
}

fn approx(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < EPS,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn keyframed_transform_is_evaluated_per_frame() {
    // Frame 0 at identity; at one second the clip is at half scale and shifted
    // right by a tenth of the canvas. Frame 15 is halfway through the segment.
    let tl = timeline(vec![clip("v", 1, 2)], 1);
    let mut visuals = BTreeMap::new();
    visuals.insert(
        "v".to_string(),
        ClipVisual {
            transform_keys: vec![
                at(
                    0.0,
                    rk::Transform {
                        dx: 0.0,
                        dy: 0.0,
                        scale: 1.0,
                    },
                ),
                at(
                    1.0,
                    rk::Transform {
                        dx: 0.1,
                        dy: 0.0,
                        scale: 0.5,
                    },
                ),
            ],
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);

    let start = compose_frame(&snap, 0).unwrap();
    let t0 = &layer_of(&start.descriptor.items[0]).transform;
    approx(f64::from(t0.center_x), 960.0);
    approx(f64::from(t0.width), 1920.0);

    let mid = compose_frame(&snap, 15).unwrap();
    let t1 = &layer_of(&mid.descriptor.items[0]).transform;
    // Scale 0.75 about the centre, then dx = 0.05 * 1920 = 96 px.
    approx(f64::from(t1.width), 1440.0);
    approx(f64::from(t1.height), 810.0);
    approx(f64::from(t1.center_x), 960.0 + 96.0);
    approx(f64::from(t1.center_y), 540.0);

    let end = compose_frame(&snap, 30).unwrap();
    let t2 = &layer_of(&end.descriptor.items[0]).transform;
    approx(f64::from(t2.width), 960.0);
    approx(f64::from(t2.center_x), 960.0 + 192.0);
}

#[test]
fn keyframed_opacity_is_evaluated_per_frame() {
    let tl = timeline(vec![clip("v", 1, 2)], 1);
    let mut visuals = BTreeMap::new();
    visuals.insert(
        "v".to_string(),
        ClipVisual {
            opacity_keys: vec![at(0.0, 1.0), at(1.0, 0.0)],
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);
    approx(
        f64::from(layer_of(&compose_frame(&snap, 0).unwrap().descriptor.items[0]).opacity),
        1.0,
    );
    approx(
        f64::from(layer_of(&compose_frame(&snap, 15).unwrap().descriptor.items[0]).opacity),
        0.5,
    );
}

#[test]
fn keyframed_crop_reaches_the_source_requirement() {
    // The source window slides from the full frame to its right half over one
    // second; frame 15 is halfway: x 0.25, width 0.75.
    let tl = timeline(vec![clip("v", 1, 2)], 1);
    let mut visuals = BTreeMap::new();
    visuals.insert(
        "v".to_string(),
        ClipVisual {
            crop_keys: vec![
                at(
                    0.0,
                    rk::Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 1.0,
                        height: 1.0,
                    },
                ),
                at(
                    1.0,
                    rk::Rect {
                        x: 0.5,
                        y: 0.0,
                        width: 0.5,
                        height: 1.0,
                    },
                ),
            ],
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);
    let frame = compose_frame(&snap, 15).unwrap();
    match &frame.requirements[0] {
        TextureRequirement::SourceFrame {
            crop, texture_id, ..
        } => {
            approx(crop.x, 0.25);
            approx(crop.width, 0.75);
            assert!(
                texture_id.contains(":c"),
                "crop must key the texture: {texture_id}"
            );
        }
        other => panic!("expected a source frame, got {other:?}"),
    }
    // Cropping samples the source; the drawn quad still fills the canvas.
    approx(
        f64::from(layer_of(&frame.descriptor.items[0]).transform.width),
        1920.0,
    );
}

#[test]
fn static_crop_is_one_key_and_keys_the_texture() {
    let tl = timeline(vec![clip("v", 1, 1)], 1);
    let mut visuals = BTreeMap::new();
    visuals.insert(
        "v".to_string(),
        ClipVisual {
            crop: Some(CropRect {
                x: 0.25,
                y: 0.0,
                width: 0.5,
                height: 1.0,
            }),
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);
    for frame_index in [0, 29] {
        let frame = compose_frame(&snap, frame_index).unwrap();
        match &frame.requirements[0] {
            TextureRequirement::SourceFrame { crop, .. } => {
                approx(crop.x, 0.25);
                approx(crop.width, 0.5);
            }
            other => panic!("expected a source frame, got {other:?}"),
        }
    }
}

#[test]
fn cross_fade_plays_an_outgoing_handle_or_holds_when_it_is_short() {
    // Clip A (source 1, 30 frames) then clip B (source 2, 60 frames) with a
    // 10-frame cross-fade in. Frame k of the fade draws B at (k + 1) / 10 over
    // A's continued source frames, and the last fade frame is B alone.
    let tl = timeline(vec![clip("a", 1, 1), clip("b", 2, 2)], 2);
    let mut visuals = BTreeMap::new();
    visuals.insert(
        "b".to_string(),
        ClipVisual {
            cross_fade_in_frames: 10,
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);

    // Before the cut: A alone, fully opaque.
    let before = compose_frame(&snap, 29).unwrap();
    assert_eq!(before.descriptor.items.len(), 1);
    approx(
        f64::from(layer_of(&before.descriptor.items[0]).opacity),
        1.0,
    );

    // First fade frame: the outgoing handle starts at A's source_out (1 s)
    // under B at weight 0.1.
    let first = compose_frame(&snap, 30).unwrap();
    assert_eq!(first.descriptor.items.len(), 2);
    approx(f64::from(layer_of(&first.descriptor.items[0]).opacity), 1.0);
    approx(f64::from(layer_of(&first.descriptor.items[1]).opacity), 0.1);
    match &first.requirements[0] {
        TextureRequirement::SourceFrame {
            input, source_time, ..
        } => {
            assert_eq!(*input, 0);
            assert_eq!(source_time.0, TICKS_PER_SECOND);
        }
        other => panic!("expected the outgoing source frame, got {other:?}"),
    }
    match &first.requirements[1] {
        TextureRequirement::SourceFrame {
            input, source_time, ..
        } => {
            assert_eq!(*input, 1);
            assert_eq!(source_time.0, 0);
        }
        other => panic!("expected the incoming source frame, got {other:?}"),
    }

    // Middle of the fade: weight 0.5 at fade frame 4.
    let mid = compose_frame(&snap, 34).unwrap();
    approx(f64::from(layer_of(&mid.descriptor.items[1]).opacity), 0.5);

    match &mid.requirements[0] {
        TextureRequirement::SourceFrame { source_time, .. } => {
            assert_eq!(source_time.0, 34 * TICKS_PER_SECOND / 30);
        }
        other => panic!("expected the moving outgoing frame, got {other:?}"),
    }

    // Last fade frame: B at full weight over the outgoing handle, which ends with
    // the fade. The frame after is B alone.
    let last = compose_frame(&snap, 39).unwrap();
    assert_eq!(last.descriptor.items.len(), 2);
    approx(f64::from(layer_of(&last.descriptor.items[1]).opacity), 1.0);
    let after = compose_frame(&snap, 40).unwrap();
    assert_eq!(after.descriptor.items.len(), 1);
    approx(f64::from(layer_of(&after.descriptor.items[0]).opacity), 1.0);
    // Less than a complete handle falls back to the last frame before the cut.
    let mut short = tl.clone();
    short.sources[0].duration = Ticks(TICKS_PER_SECOND + TICKS_PER_SECOND / 6);
    let held = compose_frame(&snapshot(&short, snap.visuals.clone()), 34).unwrap();
    match &held.requirements[0] {
        TextureRequirement::SourceFrame { source_time, .. } => {
            assert_eq!(source_time.0, 29 * TICKS_PER_SECOND / 30);
        }
        other => panic!("expected the held outgoing frame, got {other:?}"),
    }
    assert_eq!(snap.plan.total_frames, 90);
}

#[test]
fn unsupported_visual_combinations_are_rejected_explicitly() {
    // A cross-fade needs a plain outgoing clip: A with static opacity is not.
    let tl = timeline(vec![clip("a", 1, 1), clip("b", 2, 1)], 2);
    let mut visuals = BTreeMap::new();
    visuals.insert(
        "a".to_string(),
        ClipVisual {
            opacity: 0.5,
            ..Default::default()
        },
    );
    visuals.insert(
        "b".to_string(),
        ClipVisual {
            cross_fade_in_frames: 10,
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);
    assert!(matches!(
        compose_frame(&snap, 0),
        Err(ComposeError::UnsupportedVisual { .. })
    ));

    // A fade longer than its clip is invalid, not clamped.
    let tl = timeline(vec![clip("a", 1, 1)], 1);
    let mut visuals = BTreeMap::new();
    visuals.insert(
        "a".to_string(),
        ClipVisual {
            cross_fade_in_frames: 31,
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);
    assert!(matches!(
        compose_frame(&snap, 0),
        Err(ComposeError::InvalidVisual { .. })
    ));

    // Keyed opacity cannot be combined with a cross-fade.
    let tl = timeline(vec![clip("a", 1, 1), clip("b", 2, 1)], 2);
    let mut visuals = BTreeMap::new();
    visuals.insert(
        "b".to_string(),
        ClipVisual {
            cross_fade_in_frames: 10,
            opacity_keys: vec![at(0.0, 1.0)],
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);
    assert!(matches!(
        compose_frame(&snap, 30),
        Err(ComposeError::UnsupportedVisual { .. })
    ));
}

#[test]
fn timeline_keys_set_through_set_clip_visuals_reach_the_evaluated_frame() {
    // Keys go in through the timeline action (permille, timeline ticks), then
    // the snapshot adapter converts them. Frame 15 is halfway through a
    // one-second opacity fade 1000 -> 0 and crop slide full -> right half.
    use rightkit_timeline::{ClipKey, ClipVisualKeys, KeyedCrop};

    let mut tl = timeline(vec![clip("v", 1, 2)], 1);
    let keys = ClipVisualKeys {
        opacity: vec![
            ClipKey {
                at: Ticks(0),
                value: 1000,
            },
            ClipKey {
                at: Ticks(TICKS_PER_SECOND),
                value: 0,
            },
        ],
        crop: vec![
            ClipKey {
                at: Ticks(0),
                value: KeyedCrop {
                    x: 0,
                    y: 0,
                    width: 1000,
                    height: 1000,
                },
            },
            ClipKey {
                at: Ticks(TICKS_PER_SECOND),
                value: KeyedCrop {
                    x: 500,
                    y: 0,
                    width: 500,
                    height: 1000,
                },
            },
        ],
        ..Default::default()
    };
    let (next, _inverse) = rightkit_timeline::action::apply_action(
        &tl,
        &Action::SetClipVisuals {
            clip_id: "v".into(),
            visuals: keys,
        },
    )
    .unwrap();
    tl = next;

    let snap = ComposeSnapshot::from_timeline(
        "proj-visuals",
        1,
        "cafe",
        &tl,
        OutputSpec::full(1920, 1080),
    )
    .unwrap();
    let frame = compose_frame(&snap, 15).unwrap();
    approx(f64::from(layer_of(&frame.descriptor.items[0]).opacity), 0.5);
    match &frame.requirements[0] {
        TextureRequirement::SourceFrame { crop, .. } => {
            approx(crop.x, 0.25);
            approx(crop.width, 0.75);
        }
        other => panic!("expected a source frame, got {other:?}"),
    }
}
