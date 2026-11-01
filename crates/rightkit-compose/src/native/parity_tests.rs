//! Parity of `compose_frame` (base layers evaluated by `rightkit-compose`)
//! against `reference_compose_frame` (the previous local evaluation).
//!
//! Why this is a unit-level test and not an e2e: the property under test is the
//! frame-exact layer description (which layers, their order, source tick,
//! opacity bits) for every frame of many hand-built plans, including shapes the
//! compiler cannot be steered into (zero-clip hold, hold after black, negative
//! source-in, a non-whole-tick frame rate). Those are not observable through
//! pixels without a renderer, and the pixel-level behaviour is already covered
//! by the host application's playback end-to-end test.
//!
//! Comparison is bit-for-bit, with no epsilon. The descriptors do not derive
//! `PartialEq`, so equal `Debug` text is used; `f32`/`f64` `Debug` prints the
//! shortest string that round-trips, so equal text means equal bits (and keeps
//! `-0.0` distinct from `0.0`). Justification per field:
//! - active layers and order: integer and string data;
//! - source time: `Ticks`, integer arithmetic on both sides;
//! - opacity: `f32 -> f64 -> f32` is lossless, rk returns a keyframe value
//!   unchanged when sampled exactly at its time, and 1.0 between two 1.0 keys;
//! - transform / quad: computed by the host on both sides (not routed to rk);
//! - title fade: RightKit's `FrameFade` matches independent `overlay_opacity`
//!   reference values, including overlap & clipping.

use std::collections::BTreeMap;

use crate as rk;
use rightkit_compositor::FrameItemDescriptor;
use rightkit_timeline::model::TitleTemplate;
use rightkit_timeline::plan::{
    AudioMix, OverlayItem, PlanInput, RenderPlan, MIX_SAMPLE_RATE, PLAN_SCHEMA_VERSION,
};
use rightkit_timeline::{Fps, SourceId, Ticks, VideoSegment, TICKS_PER_SECOND};

use crate::native::base_eval::{
    build_composition, evaluate_base_layers, ActiveBase, BaseComposition,
};
use crate::native::mapper::{compose_frame, reference_compose_frame, ComposeError};
use crate::native::snapshot::{
    ClipVisual, ComposeSnapshot, LayerTransform, OutputSpec, SnapshotIdentity,
};

fn sid(n: u8) -> SourceId {
    SourceId::new(format!("{:064x}", n)).unwrap()
}

fn input(n: u8, width: u32, height: u32) -> PlanInput {
    PlanInput {
        source: sid(n),
        has_video: true,
        has_audio: false,
        duration: Ticks::ZERO,
        width,
        height,
    }
}

fn clip(id: &str, input: usize, source_in: i64, frames: i64) -> VideoSegment {
    VideoSegment::Clip {
        clip_id: id.into(),
        input,
        source_in: Ticks(source_in),
        frames,
        visuals: None,
    }
}

fn title(
    id: &str,
    start_frame: i64,
    frames: i64,
    fade_in_frames: i64,
    fade_out_frames: i64,
) -> OverlayItem {
    OverlayItem {
        id: id.into(),
        start_frame,
        frames,
        template: TitleTemplate::Centered { text: "t".into() },
        fade_in_frames,
        fade_out_frames,
        position: None,
    }
}

fn snapshot(
    (num, den): (u32, u32),
    inputs: Vec<PlanInput>,
    video: Vec<VideoSegment>,
    overlays: Vec<OverlayItem>,
    visuals: BTreeMap<String, ClipVisual>,
) -> ComposeSnapshot {
    let fps = Fps::new(num, den).unwrap();
    let total_frames: i64 = video.iter().map(VideoSegment::frames).sum();
    ComposeSnapshot {
        identity: SnapshotIdentity {
            project: "p".into(),
            revision: 1,
            timeline_blake3: "0".into(),
        },
        plan: RenderPlan {
            schema_version: PLAN_SCHEMA_VERSION,
            cards: Vec::new(),
            fps,
            total_frames,
            inputs,
            video,
            audio: AudioMix {
                sample_rate: MIX_SAMPLE_RATE,
                total: fps.frame_to_ticks(total_frames),
                layers: Vec::new(),
                programme_start: rightkit_timeline::Ticks::ZERO,
                programme_end: rightkit_timeline::Ticks::ZERO,
                room_tone: None,
            },
            overlays,
        },
        output: OutputSpec::full(1920, 1080),
        visuals,
    }
}

fn two_inputs() -> Vec<PlanInput> {
    // Landscape and portrait, so the letterbox fit differs per input.
    vec![input(1, 1920, 1080), input(2, 1080, 1920)]
}

fn no_visuals() -> BTreeMap<String, ClipVisual> {
    BTreeMap::new()
}

/// Every in-scope feature, as (name, snapshot).
// The scenario list reads best as one push per scenario.
#[allow(clippy::vec_init_then_push)]
fn scenarios() -> Vec<(&'static str, ComposeSnapshot)> {
    let t = TICKS_PER_SECOND;
    let mut out = Vec::new();

    out.push((
        "single clip",
        snapshot(
            (30, 1),
            two_inputs(),
            vec![clip("c1", 0, t, 10)],
            vec![],
            no_visuals(),
        ),
    ));

    out.push((
        "cuts across inputs, then hold tail",
        snapshot(
            (30, 1),
            two_inputs(),
            vec![
                clip("c1", 0, 0, 5),
                clip("c2", 1, 3 * t, 7),
                clip("c3", 0, 9 * t + 12_345, 4),
                VideoSegment::Hold { frames: 4 },
            ],
            vec![],
            no_visuals(),
        ),
    ));

    out.push((
        "black only, with a title (gap / black frame)",
        snapshot(
            (30, 1),
            two_inputs(),
            vec![VideoSegment::Black { frames: 8 }],
            vec![title("t1", 2, 4, 1, 1)],
            no_visuals(),
        ),
    ));

    out.push((
        "hold with no preceding clip is black",
        snapshot(
            (30, 1),
            two_inputs(),
            vec![VideoSegment::Hold { frames: 3 }],
            vec![],
            no_visuals(),
        ),
    ));

    out.push((
        "clip, black, hold (hold repeats the clip across a black gap)",
        snapshot(
            (30, 1),
            two_inputs(),
            vec![
                clip("c1", 1, 2 * t, 3),
                VideoSegment::Black { frames: 2 },
                VideoSegment::Hold { frames: 2 },
            ],
            vec![],
            no_visuals(),
        ),
    ));

    out.push((
        "titles: fades, overlap, clipped, negative start, duplicate ids",
        snapshot(
            (30, 1),
            two_inputs(),
            vec![clip("c1", 0, 0, 24)],
            vec![
                title("t1", 2, 12, 3, 3),
                title("t1", 5, 4, 3, 3),   // fades overlap each other
                title("t2", 20, 10, 2, 2), // runs past the end of the plan
                title("t3", -2, 6, 4, 0),  // starts before frame 0
                title("t4", 0, 24, 0, 5),
                title("t5", 10, 3, 0, 0),   // no fade
                title("", 8, 2, 5, 5),      // fades longer than the title
                title("zero", 12, 0, 2, 2), // never visible
            ],
            no_visuals(),
        ),
    ));

    let mut opacity_visuals = BTreeMap::new();
    opacity_visuals.insert(
        "c1".to_string(),
        ClipVisual {
            opacity: 0.5,
            ..Default::default()
        },
    );
    opacity_visuals.insert(
        "c2".to_string(),
        ClipVisual {
            opacity: 0.25,
            transform: Some(LayerTransform {
                center: [100.5, 200.25],
                size: [640.0, 360.0],
                rotation_degrees: 12.5,
                flip_x: true,
                flip_y: false,
            }),
            ..Default::default()
        },
    );
    out.push((
        "clip opacity and clip transform",
        snapshot(
            (30, 1),
            two_inputs(),
            vec![
                clip("c1", 0, 0, 6),
                clip("c2", 1, t, 6),
                clip("c3", 0, 0, 3),
            ],
            vec![title("t1", 4, 6, 2, 2)],
            opacity_visuals,
        ),
    ));

    out.push((
        "negative source-in, then signed hold",
        snapshot(
            (30, 1),
            two_inputs(),
            vec![clip("c1", 0, -t, 3), VideoSegment::Hold { frames: 2 }],
            vec![],
            no_visuals(),
        ),
    ));

    out.push((
        "NTSC 30000/1001 with big source-in and title",
        snapshot(
            (30000, 1001),
            two_inputs(),
            vec![
                clip("c1", 0, 123_456_789, 5),
                clip("c2", 1, 999_999_999_000, 3),
                VideoSegment::Hold { frames: 4 },
            ],
            vec![title("t1", 2, 8, 2, 3)],
            no_visuals(),
        ),
    ));

    out.push((
        "NTSC 24000/1001",
        snapshot(
            (24000, 1001),
            two_inputs(),
            vec![clip("c1", 0, 5 * t, 6), clip("c2", 1, 0, 6)],
            vec![title("t1", 3, 6, 2, 2)],
            no_visuals(),
        ),
    ));

    out.push((
        "25 fps",
        snapshot(
            (25, 1),
            two_inputs(),
            vec![
                clip("c1", 0, t / 5, 7),
                clip("c2", 1, 0, 5),
                VideoSegment::Hold { frames: 3 },
            ],
            vec![title("t1", 1, 10, 3, 4)],
            no_visuals(),
        ),
    ));

    // 11 fps has fractional ticks per frame. Start c1 at frame 1 so
    // ft(2) - ft(1) differs from ft(1), exposing absolute-offset retiming.
    out.push((
        "11 fps with relative source rounding",
        snapshot(
            (11, 1),
            two_inputs(),
            vec![
                VideoSegment::Black { frames: 1 },
                clip("c1", 0, t / 3, 4),
                clip("c2", 1, 7, 3),
                VideoSegment::Hold { frames: 2 },
            ],
            vec![title("t1", 1, 6, 2, 2)],
            no_visuals(),
        ),
    ));

    out
}

fn fmt<T: std::fmt::Debug>(value: &T) -> String {
    format!("{value:?}")
}

#[test]
fn every_frame_matches_the_reference() {
    let mut frames_checked = 0usize;
    let mut layers_seen = 0usize;
    for (name, snap) in scenarios() {
        let total = snap.plan.total_frames;
        // In range and one past either end (out-of-range errors are the host's).
        for frame in -1..=total {
            let new = compose_frame(&snap, frame);
            let reference = reference_compose_frame(&snap, frame);
            assert_eq!(
                fmt(&new),
                fmt(&reference),
                "{name}: frame {frame} differs from the reference"
            );
            if let Ok(ok) = &new {
                layers_seen += ok.descriptor.items.len();
                // The items really are layers, in the reference's order.
                assert!(ok
                    .descriptor
                    .items
                    .iter()
                    .all(|i| matches!(i, FrameItemDescriptor::Layer(_))));
            }
            frames_checked += 1;
        }
    }
    // Guard against a vacuous pass.
    assert!(frames_checked > 150, "only {frames_checked} frames checked");
    assert!(layers_seen > 100, "only {layers_seen} layers compared");
}

fn stacks_at(built: &BaseComposition, t: u64) -> Vec<usize> {
    rk::evaluate_v3(&built.composition, rk::Ticks(t))
        .unwrap()
        .layers
        .iter()
        .map(|l| l.stack_index)
        .collect()
}

/// Boundary rule at tick resolution: frame `f` owns `[ft(f), ft(f+1))` on both
/// sides. The host only ever asks for a frame start, so this pins rk's half-open
/// interval to the same ownership: the tick before a boundary still belongs to
/// the previous frame, the boundary tick and the tick after belong to the next.
#[test]
fn boundary_ticks_belong_to_the_same_frame_on_both_sides() {
    for (name, snap) in scenarios() {
        let built = build_composition(&snap).unwrap();
        let ft = |f: i64| snap.plan.fps.frame_to_ticks(f).0 as u64;
        let total = snap.plan.total_frames;
        for f in 1..total {
            assert_eq!(
                stacks_at(&built, ft(f) - 1),
                stacks_at(&built, ft(f - 1)),
                "{name}: tick before frame {f}"
            );
            if ft(f) + 1 < ft(f + 1) {
                assert_eq!(
                    stacks_at(&built, ft(f) + 1),
                    stacks_at(&built, ft(f)),
                    "{name}: tick after frame {f} start"
                );
            }
        }
        // First tick, last tick, and one past the end.
        assert_eq!(
            stacks_at(&built, ft(total) - 1),
            stacks_at(&built, ft(total - 1))
        );
        assert!(matches!(
            rk::evaluate_v3(&built.composition, rk::Ticks(ft(total))),
            Err(rk::CompositionError::OutOfRange { .. })
        ));
    }
}

#[test]
fn signed_sources_and_opacity_come_from_evaluated_payloads() {
    let mut negative_sources = 0;
    for (_, snap) in scenarios() {
        let built = build_composition(&snap).unwrap();
        for f in 0..snap.plan.total_frames {
            let tick = snap.plan.fps.frame_to_ticks(f).0 as u64;
            let description = rk::evaluate_v3(&built.composition, rk::Ticks(tick)).unwrap();
            let active = evaluate_base_layers(&snap, f).unwrap();
            assert_eq!(description.layers.len(), active.len());
            for (layer, active) in description.layers.iter().zip(active) {
                assert!(rk::Opacity::new(layer.opacity).is_ok());
                match (&layer.payload, active) {
                    (
                        rk::PayloadV3::Video { source, .. },
                        ActiveBase::Clip {
                            source_time,
                            opacity,
                            ..
                        },
                    )
                    | (
                        rk::PayloadV3::Video { source, .. },
                        ActiveBase::Hold {
                            source_time,
                            opacity,
                            ..
                        },
                    ) => {
                        assert_eq!(i128::from(source_time.0), source.0);
                        assert_eq!(opacity.to_bits(), (layer.opacity as f32).to_bits());
                        negative_sources += usize::from(source.0 < 0);
                    }
                    (rk::PayloadV3::Still { .. }, ActiveBase::Title { opacity, .. }) => {
                        assert_eq!(opacity.to_bits(), (layer.opacity as f32).to_bits());
                    }
                    other => panic!("payload & active kind differ: {other:?}"),
                }
            }
        }
    }
    assert!(
        negative_sources >= 5,
        "negative clip & hold were not evaluated"
    );
}

#[test]
fn invalid_static_and_crossfade_opacity_is_rejected() {
    for (opacity, expected) in [
        (1.5f32, rk::OpacityError::OutOfRange),
        (-0.25, rk::OpacityError::OutOfRange),
        (f32::NAN, rk::OpacityError::NonFinite),
        (f32::INFINITY, rk::OpacityError::NonFinite),
    ] {
        assert_eq!(rk::Opacity::new(f64::from(opacity)), Err(expected));
        for fade in [0, 3] {
            let snap = snapshot(
                (11, 1),
                two_inputs(),
                vec![clip("c1", 0, 0, 4)],
                vec![],
                BTreeMap::from([(
                    "c1".into(),
                    ClipVisual {
                        opacity,
                        cross_fade_in_frames: fade,
                        ..Default::default()
                    },
                )]),
            );
            assert!(matches!(
                build_composition(&snap),
                Err(ComposeError::InvalidVisual { clip_id, reason })
                    if clip_id == "c1" && reason == expected.to_string()
            ));
            assert!(matches!(
                compose_frame(&snap, 0),
                Err(ComposeError::InvalidVisual { clip_id, reason })
                    if clip_id == "c1" && reason == expected.to_string()
            ));
        }
    }
}

#[test]
fn opening_card_offset_and_clipped_clip_keep_relative_rounding() {
    let source_in = -TICKS_PER_SECOND;
    let mut snap = snapshot(
        (11, 1),
        two_inputs(),
        vec![clip("c1", 0, source_in, 4)],
        vec![],
        no_visuals(),
    );
    // One opening frame followed by only three visible frames of this clip.
    snap.plan.cards.push(rightkit_timeline::plan::CardSpan {
        id: "opening".into(),
        slot: rightkit_timeline::CardSlot::Opening,
        start_frame: 0,
        frames: 1,
        colour: "#000000".into(),
        fill: None,
    });
    let ft = |f| snap.plan.fps.frame_to_ticks(f).0;
    assert_ne!(ft(2) - ft(1), ft(1));
    let built = build_composition(&snap).unwrap();
    assert!(stacks_at(&built, 0).is_empty());
    for f in 1..snap.plan.total_frames {
        for tick in [ft(f), ft(f) + 1, ft(f + 1) - 1] {
            let frame = rk::evaluate_v3(&built.composition, rk::Ticks(tick as u64)).unwrap();
            let expected = i128::from(source_in + ft(f - 1));
            assert!(matches!(frame.layers[0].payload,
                rk::PayloadV3::Video { source, .. } if source.0 == expected));
        }
        assert!(matches!(evaluate_base_layers(&snap, f).unwrap()[0],
            ActiveBase::Clip { source_time, .. } if source_time.0 == source_in + ft(f - 1)));
    }
}

#[test]
fn crossfade_uses_signed_outgoing_payload_and_frame_exact_ramp() {
    for (has_handle, visible_frames) in [(false, 5), (true, 5), (false, 2), (true, 2)] {
        let fps = Fps::new(11, 1).unwrap();
        let ft = |f| fps.frame_to_ticks(f).0;
        let source_in = if has_handle { TICKS_PER_SECOND } else { -ft(4) };
        let mut inputs = two_inputs();
        inputs[0].duration = Ticks(if has_handle { 10 * TICKS_PER_SECOND } else { 0 });
        let visual_opacity = 0.37f32;
        let mut snap = snapshot(
            (11, 1),
            inputs,
            vec![
                VideoSegment::Black { frames: 1 },
                clip("c1", 0, source_in, 3),
                clip("c2", 1, 7, 5),
            ],
            vec![],
            BTreeMap::from([(
                "c2".into(),
                ClipVisual {
                    opacity: visual_opacity,
                    cross_fade_in_frames: 3,
                    ..Default::default()
                },
            )]),
        );
        snap.plan.total_frames = 4 + visible_frames;
        let built = build_composition(&snap).unwrap();
        for k in 0..visible_frames {
            let f = 4 + k;
            let evaluated = rk::evaluate_v3(&built.composition, rk::Ticks(ft(f) as u64)).unwrap();
            let active = evaluate_base_layers(&snap, f).unwrap();
            assert_eq!(evaluated.layers.len(), if k < 3 { 2 } else { 1 });
            if k < 3 {
                let expected = source_in + if has_handle { ft(3) + ft(k) } else { ft(2) };
                assert!(matches!(evaluated.layers[0].payload,
                    rk::PayloadV3::Video { source, .. } if source.0 == i128::from(expected)));
                assert!(
                    matches!(active[0], ActiveBase::Hold { source_time, .. } if source_time.0 == expected)
                );
            }
            let incoming = evaluated.layers.last().unwrap();
            let weight = ((k + 1) as f32 / 3.0).min(1.0);
            let expected = (f64::from(weight) * f64::from(visual_opacity)) as f32;
            assert_eq!((incoming.opacity as f32).to_bits(), expected.to_bits());
            assert!(matches!(incoming.payload,
                rk::PayloadV3::Video { source, .. } if source.0 == i128::from(7 + ft(k))));
            assert!(
                matches!(active.last().unwrap(), ActiveBase::Clip { source_time, opacity, .. }
                if source_time.0 == 7 + ft(k) && opacity.to_bits() == expected.to_bits())
            );
        }
    }
}

#[test]
fn source_payload_outside_clock_returns_error() {
    let snap = snapshot(
        (11, 1),
        two_inputs(),
        vec![clip("c1", 0, i64::MAX, 2), VideoSegment::Hold { frames: 1 }],
        vec![],
        no_visuals(),
    );
    assert!(matches!(
        evaluate_base_layers(&snap, 0).unwrap()[0],
        ActiveBase::Clip {
            source_time: Ticks(i64::MAX),
            ..
        }
    ));
    for f in [1, 2] {
        assert!(matches!(evaluate_base_layers(&snap, f),
            Err(ComposeError::Transport(reason))
                if reason == "video source time exceeds RightKit i64 clock"));
    }
}
