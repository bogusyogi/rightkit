//! Mapper contract tests that no playback e2e can reach.
//!
//! Timing, texture identity, title fades and multi-source mapping are covered
//! end to end by the host application's playback end-to-end test (real frames
//! compared against the FFmpeg export). What remains here is the behavior the
//! session boundary cannot produce: sessions always pass default per-clip
//! visuals, so crop/effect/mask/blend rejection and the explicit transform
//! pass-through are only reachable by other `ComposeSnapshot` consumers, and
//! `due_frame` clamps its index, so out-of-range is a compose-call error.

use std::collections::BTreeMap;

use rightkit_compose::native::{
    audio_descriptor, compose_frame, ClipVisual, ComposeError, ComposeSnapshot, CropRect,
    EffectPassSpec, LayerTransform, OutputSpec, TextureRequirement,
};
use rightkit_compositor::{BlendMode, EffectUniformValueDescriptor, FrameItemDescriptor};
use rightkit_timeline::model::{
    AudioClip, AudioRole, AudioTrack, CardAudio, CardClip, CardSlot, SourceId, SourceInfo,
    Timeline, UnderAudio, VideoClip, VideoStreamInfo, VideoTrack,
};
use rightkit_timeline::{Fps, MilliDb, Ticks, TICKS_PER_SECOND};

fn sid(n: u8) -> SourceId {
    SourceId::new(format!("{:064x}", n)).unwrap()
}

fn src(n: u8, secs: i64, has_video: bool, has_audio: bool) -> SourceInfo {
    SourceInfo {
        id: sid(n),
        duration: Ticks(secs * TICKS_PER_SECOND),
        video: has_video.then_some(VideoStreamInfo {
            width: 1920,
            height: 1080,
            duration: None,
        }),
        has_audio,
    }
}

fn clip(id: &str, src: u8, in_s: i64, out_s: i64) -> VideoClip {
    VideoClip {
        id: id.into(),
        source: sid(src),
        source_in: Ticks(in_s * TICKS_PER_SECOND),
        source_out: Ticks(out_s * TICKS_PER_SECOND),
    }
}

fn base_timeline(sources: Vec<SourceInfo>, clips: Vec<VideoClip>) -> Timeline {
    let mut tl = Timeline::new(Fps::new(30, 1).unwrap());
    tl.sources = sources;
    tl.video_tracks = vec![VideoTrack {
        id: "V1".into(),
        clips,
    }];
    tl
}

fn snapshot(tl: &Timeline, visuals: BTreeMap<String, ClipVisual>) -> ComposeSnapshot {
    ComposeSnapshot::evaluate(
        "proj-test",
        7,
        "deadbeef",
        tl,
        OutputSpec::full(1920, 1080),
        visuals,
    )
    .unwrap()
}

/// The audio-mix contract (gain, duck windows, hold tails) feeds the playback
/// feeder but is never asserted on delivered samples by the session e2e; the
/// window math is pinned here instead.
#[test]
fn audio_tail_extends_with_hold_of_last_frame() {
    let mut tl = base_timeline(vec![src(1, 60, true, true)], vec![clip("c1", 1, 0, 1)]);
    tl.audio_tracks = vec![AudioTrack {
        id: "A1".into(),
        role: AudioRole::Narration,
        clips: vec![AudioClip {
            id: "n1".into(),
            source: sid(1),
            trim_in: Ticks(0),
            trim_out: Ticks(3 * TICKS_PER_SECOND),
            start: Ticks(0),
            gain: MilliDb::ZERO,
            link: Some("c1".into()),
            under: UnderAudio::Keep,
        }],
    }];
    let snap = snapshot(&tl, BTreeMap::new());
    assert_eq!(snap.plan.total_frames, 90);
    // Frame 60 is inside the Hold tail: it must reuse the clip's last source
    // frame texture (t = source_in + frame 29), not decode a new time.
    let f = compose_frame(&snap, 60).unwrap();
    match &f.requirements[0] {
        TextureRequirement::SourceFrame { source_time, .. } => {
            assert_eq!(*source_time, Ticks(29 * TICKS_PER_SECOND / 30));
        }
        _ => panic!("hold must reference the last source frame"),
    }
    let mix = audio_descriptor(&snap.plan);
    assert_eq!(mix.sample_rate, 48_000);
    assert_eq!(mix.total_samples, 3 * 48_000);
    let n = mix.layers.iter().find(|l| l.id == "n1").unwrap();
    assert_eq!(n.duration_samples, 3 * 48_000);
}

#[test]
fn duck_windows_and_gain_land_in_audio_descriptor() {
    let mut tl = base_timeline(
        vec![src(1, 60, true, true), src(2, 60, true, true)],
        vec![clip("v", 1, 0, 4)],
    );
    tl.audio_tracks = vec![AudioTrack {
        id: "A1".into(),
        role: AudioRole::Narration,
        clips: vec![AudioClip {
            id: "n".into(),
            source: sid(2),
            trim_in: Ticks(0),
            trim_out: Ticks(2 * TICKS_PER_SECOND),
            start: Ticks(TICKS_PER_SECOND), // starts at 1s output
            gain: MilliDb::from_db(-6.0),
            link: Some("v".into()),
            under: UnderAudio::Duck {
                amount: MilliDb(12_000),
            },
        }],
    }];
    let snap = snapshot(&tl, BTreeMap::new());
    let mix = audio_descriptor(&snap.plan);
    let narration = mix.layers.iter().find(|l| l.id == "n").unwrap();
    assert_eq!(narration.start_sample, 48_000);
    assert!((narration.gain_linear - 0.501).abs() < 0.001);
    // The video clip's own audio carries the duck window 1s..3s.
    let v = mix.layers.iter().find(|l| l.id == "v").unwrap();
    assert_eq!(v.windows.len(), 1);
    assert_eq!(v.windows[0].start_sample, 48_000);
    assert_eq!(v.windows[0].end_sample, 3 * 48_000);
    assert_eq!(v.windows[0].duck, Some(MilliDb(12_000)));
}

#[test]
fn unsupported_features_are_rejected_not_approximated() {
    let tl = base_timeline(vec![src(1, 60, true, true)], vec![clip("v", 1, 0, 2)]);
    let mut visuals = BTreeMap::new();
    visuals.insert(
        "v".to_string(),
        ClipVisual {
            crop: Some(CropRect {
                x: 0.1,
                y: 0.0,
                width: 0.8,
                height: 1.0,
            }),
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);
    // Crop is evaluated into the texture requirement, not rejected.
    let frame = compose_frame(&snap, 0).expect("crop is evaluated, not rejected");
    match &frame.requirements[0] {
        TextureRequirement::SourceFrame { crop, .. } => {
            assert!((crop.x - 0.1).abs() < 1e-6 && (crop.width - 0.8).abs() < 1e-6);
        }
        other => panic!("expected a source frame, got {other:?}"),
    }

    let mut visuals = BTreeMap::new();
    visuals.insert(
        "v".to_string(),
        ClipVisual {
            effect_groups: vec![vec![EffectPassSpec {
                shader: "chromatic-aberration".into(),
                uniforms: Default::default(),
            }]],
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);
    assert!(matches!(
        compose_frame(&snap, 0),
        Err(ComposeError::UnsupportedEffect { .. })
    ));

    let mut visuals = BTreeMap::new();
    visuals.insert(
        "v".to_string(),
        ClipVisual {
            mask: Some("any".into()),
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);
    assert!(matches!(
        compose_frame(&snap, 0),
        Err(ComposeError::UnsupportedMask { .. })
    ));

    let mut visuals = BTreeMap::new();
    visuals.insert(
        "v".to_string(),
        ClipVisual {
            blend_mode: Some("vivid-light".into()),
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);
    assert!(matches!(
        compose_frame(&snap, 0),
        Err(ComposeError::UnsupportedBlendMode { .. })
    ));
}

#[test]
fn supported_effect_and_transform_pass_through() {
    let tl = base_timeline(vec![src(1, 60, true, true)], vec![clip("v", 1, 0, 2)]);
    let mut uniforms = std::collections::HashMap::new();
    uniforms.insert("u_sigma".into(), EffectUniformValueDescriptor::Number(3.0));
    uniforms.insert("u_step".into(), EffectUniformValueDescriptor::Number(1.0));
    uniforms.insert(
        "u_direction".into(),
        EffectUniformValueDescriptor::Vector(vec![1.0, 0.0]),
    );
    let mut visuals = BTreeMap::new();
    visuals.insert(
        "v".to_string(),
        ClipVisual {
            opacity: 0.5,
            transform: Some(LayerTransform {
                center: [480.0, 270.0],
                size: [960.0, 540.0],
                rotation_degrees: 90.0,
                flip_x: true,
                flip_y: false,
            }),
            blend_mode: Some("screen".into()),
            effect_groups: vec![vec![EffectPassSpec {
                shader: "gaussian-blur".into(),
                uniforms,
            }]],
            ..Default::default()
        },
    );
    let snap = snapshot(&tl, visuals);
    let f = compose_frame(&snap, 0).unwrap();
    let FrameItemDescriptor::Layer(layer) = &f.descriptor.items[0] else {
        panic!()
    };
    assert_eq!(layer.opacity, 0.5);
    assert_eq!(layer.transform.center_x, 480.0);
    assert_eq!(layer.transform.rotation_degrees, 90.0);
    assert!(layer.transform.flip_x);
    assert_eq!(layer.blend_mode, BlendMode::Screen);
    assert_eq!(layer.effect_pass_groups.len(), 1);
    assert_eq!(layer.effect_pass_groups[0][0].shader, "gaussian-blur");
}

#[test]
fn out_of_range_frame_is_rejected() {
    let tl = base_timeline(vec![src(1, 60, true, true)], vec![clip("v", 1, 0, 1)]);
    let snap = snapshot(&tl, BTreeMap::new());
    assert!(matches!(
        compose_frame(&snap, 30),
        Err(ComposeError::OutOfRange(30, 30))
    ));
    assert!(matches!(
        compose_frame(&snap, -1),
        Err(ComposeError::OutOfRange(-1, 30))
    ));
}

/// An opening card is output time before the programme: frame 0 is the card
/// colour (with its title drawn over it), and programme content starts right
/// after the card, at programme frame 0.
#[test]
fn opening_card_maps_colour_and_title_then_offsets_programme() {
    let mut tl = base_timeline(vec![src(1, 60, true, true)], vec![clip("v", 1, 0, 1)]);
    tl.cards = vec![CardClip {
        id: "open".into(),
        slot: CardSlot::Opening,
        colour: "#102030".into(),
        title: Some("Intro".into()),
        frames: 10,
        audio: CardAudio::Silence,
    }];
    let snap = snapshot(&tl, BTreeMap::new());
    // 10 card frames, then the 30-frame programme.
    assert_eq!(snap.plan.total_frames, 40);

    let card = compose_frame(&snap, 0).unwrap();
    let c = card.descriptor.clear.color;
    assert!((c[0] - 16.0 / 255.0).abs() < 1e-6, "red {c:?}");
    assert!((c[1] - 32.0 / 255.0).abs() < 1e-6, "green {c:?}");
    assert!((c[2] - 48.0 / 255.0).abs() < 1e-6, "blue {c:?}");
    // No programme video under the card; only the card title is drawn.
    assert!(card
        .requirements
        .iter()
        .all(|r| !matches!(r, TextureRequirement::SourceFrame { .. })));
    assert!(card.requirements.iter().any(|r| matches!(
        r,
        TextureRequirement::RasterizedTitle { overlay_id, .. } if overlay_id == "card:open"
    )));
    assert_eq!(card.descriptor.items.len(), 1);

    let first = compose_frame(&snap, 10).unwrap();
    assert_eq!(first.descriptor.clear.color, [0.0, 0.0, 0.0, 1.0]);
    let source_time = first.requirements.iter().find_map(|r| match r {
        TextureRequirement::SourceFrame { source_time, .. } => Some(*source_time),
        _ => None,
    });
    assert_eq!(
        source_time,
        Some(Ticks(0)),
        "programme frame 0 after the card"
    );
    assert!(first.descriptor.items.iter().any(|i| matches!(
        i,
        FrameItemDescriptor::Layer(l) if l.texture_id.starts_with("src:")
    )));
}
