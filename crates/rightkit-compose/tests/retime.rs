//! Retime: the piecewise output-to-source map, its evaluator and validation.

use rightkit_compose::*;

fn ms(v: u64) -> Ticks {
    Ticks::from_ms(v)
}

fn play(source_in: u64, source_out: u64, speed: f64) -> RetimeSegment {
    RetimeSegment::Play {
        source_in: ms(source_in),
        source_out: ms(source_out),
        speed,
    }
}

fn hold(source_at: u64, duration: u64) -> RetimeSegment {
    RetimeSegment::Hold {
        source_at: ms(source_at),
        duration: ms(duration),
    }
}

fn reverse(source_in: u64, source_out: u64, speed: f64) -> RetimeSegment {
    RetimeSegment::Reverse {
        source_in: ms(source_in),
        source_out: ms(source_out),
        speed,
    }
}

#[test]
fn play_at_unit_speed_is_identity_offset_by_the_source_in() {
    let segments = [play(1000, 3000, 1.0)];
    assert_eq!(source_time_at(&segments, ms(0)), Some(ms(1000)));
    assert_eq!(source_time_at(&segments, ms(500)), Some(ms(1500)));
    // Output 2000 ms is the first tick past the segment.
    assert_eq!(source_time_at(&segments, ms(1999)), Some(ms(2999)));
    assert_eq!(source_time_at(&segments, ms(2000)), None);
}

#[test]
fn play_at_double_speed_covers_its_source_in_half_the_output() {
    let segments = [play(0, 4000, 2.0)];
    assert_eq!(segments[0].output_duration(), ms(2000));
    assert_eq!(source_time_at(&segments, ms(500)), Some(ms(1000)));
    assert_eq!(source_time_at(&segments, ms(1999)), Some(ms(3998)));
    assert_eq!(source_time_at(&segments, ms(2000)), None);
}

#[test]
fn play_rounds_output_offsets_down_to_source_ticks() {
    // Speed 3 over 1 ms of source: output duration ceil(705,600 / 3) = 235,200.
    let segment = RetimeSegment::Play {
        source_in: Ticks::ZERO,
        source_out: Ticks(705_600),
        speed: 3.0,
    };
    assert_eq!(segment.output_duration(), Ticks(235_200));
    // Output tick 1 reads floor(1 * 3) = 3 source ticks.
    assert_eq!(source_time_at(&[segment], Ticks(1)), Some(Ticks(3)));
    // Output tick 235,199 reads floor(235,199 * 3) = 705,597, still below source_out.
    assert_eq!(
        source_time_at(&[segment], Ticks(235_199)),
        Some(Ticks(705_597))
    );
}

#[test]
fn hold_freezes_one_source_time_for_its_duration() {
    let segments = [hold(750, 500)];
    assert_eq!(source_time_at(&segments, ms(0)), Some(ms(750)));
    assert_eq!(source_time_at(&segments, ms(499)), Some(ms(750)));
    assert_eq!(source_time_at(&segments, ms(500)), None);
}

#[test]
fn reverse_walks_the_source_backwards_from_source_out() {
    let segments = [reverse(1000, 3000, 1.0)];
    assert_eq!(source_time_at(&segments, ms(0)), Some(ms(3000)));
    assert_eq!(source_time_at(&segments, ms(500)), Some(ms(2500)));
    // The last output tick is still at or above source_in.
    let last = source_time_at(&segments, ms(1999)).unwrap();
    assert!(last >= ms(1000) && last < ms(1010));
    assert_eq!(source_time_at(&segments, ms(2000)), None);
}

#[test]
fn segments_chain_end_to_end_and_each_boundary_belongs_to_the_next() {
    // 1000 ms of play (1 s output), then 500 ms hold, then reverse at 2x for 1 s output.
    let segments = [
        play(0, 1000, 1.0),
        hold(1000, 500),
        reverse(2000, 4000, 2.0),
    ];
    assert_eq!(output_span(&segments), ms(2500));
    // Last tick of play, first tick of hold.
    assert_eq!(source_time_at(&segments, ms(999)), Some(ms(999)));
    assert_eq!(source_time_at(&segments, ms(1000)), Some(ms(1000)));
    // Last tick of hold, first tick of reverse.
    assert_eq!(source_time_at(&segments, ms(1499)), Some(ms(1000)));
    assert_eq!(source_time_at(&segments, ms(1500)), Some(ms(4000)));
    // Reverse ends at output 2500 ms, exclusive.
    assert_eq!(
        source_time_at(&segments, ms(2499)).map(|t| t.0 >= ms(2000).0),
        Some(true)
    );
    assert_eq!(source_time_at(&segments, ms(2500)), None);
}

#[test]
fn empty_segment_list_maps_nothing() {
    assert_eq!(source_time_at(&[], ms(0)), None);
}

#[test]
fn constant_helper_is_an_open_ended_forward_play_from_source_zero() {
    let segment = RetimeSegment::constant(2.0);
    assert_eq!(
        segment,
        RetimeSegment::Play {
            source_in: Ticks::ZERO,
            source_out: Ticks::MAX,
            speed: 2.0,
        }
    );
    assert_eq!(source_time_at(&[segment], ms(500)), Some(ms(1000)));
    // Open-ended: the span saturates at ceil(2^64 / 2) = 2^63 ticks, far beyond any layer.
    assert_eq!(output_span(&[segment]), Ticks(1 << 63));
}

fn video_with(retime: Vec<RetimeSegment>, span_ms: u64) -> Composition {
    let asset = AssetRef {
        id: "src".into(),
        blake3: "b3".into(),
        width: 1920,
        height: 1080,
        duration: None,
    };
    let layer = Layer {
        id: "screen".into(),
        timing: Timing {
            start: Ticks::ZERO,
            end: ms(span_ms),
        },
        transform: Vec::new(),
        opacity: Vec::new(),
        clip: None,
        shadow: None,
        motion_blur: None,
        content: Content::Video {
            asset,
            placement: Rect::FULL,
            crop: Vec::new(),
            retime,
        },
    };
    let mut c = Composition::new(
        Canvas {
            width: 1920,
            height: 1080,
        },
        Rational::new(30, 1),
        ms(10_000),
    );
    c.layers = vec![layer];
    c
}

fn source_of(c: &Composition, t_ms: u64) -> Ticks {
    match &evaluate_ms(c, t_ms).unwrap().layers[0].payload {
        Payload::Video { source, .. } => *source,
        other => panic!("video payload, got {other:?}"),
    }
}

#[test]
fn evaluator_drives_video_source_through_the_retime() {
    let c = video_with(vec![hold(500, 1000), play(0, 3000, 1.0)], 3000);
    assert_eq!(source_of(&c, 0), ms(500));
    assert_eq!(source_of(&c, 999), ms(500));
    assert_eq!(source_of(&c, 1000), ms(0));
    assert_eq!(source_of(&c, 2500), ms(1500));
}

#[test]
fn empty_retime_is_identity() {
    let c = video_with(Vec::new(), 2000);
    assert_eq!(source_of(&c, 750), ms(750));
}

#[test]
fn retime_source_is_clamped_to_the_asset_duration() {
    let mut c = video_with(vec![play(0, 10_000, 1.0)], 4000);
    if let Content::Video { asset, .. } = &mut c.layers[0].content {
        asset.duration = Some(ms(1500));
    }
    assert_eq!(source_of(&c, 3000), ms(1500));
}

#[test]
fn validation_rejects_zero_and_non_finite_speeds() {
    for speed in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let c = video_with(vec![play(0, 1000, speed)], 1000);
        assert!(
            matches!(evaluate_ms(&c, 0), Err(CompositionError::Invalid { .. })),
            "speed {speed} should be rejected"
        );
    }
    let c = video_with(vec![reverse(0, 1000, 0.0)], 1000);
    assert!(evaluate_ms(&c, 0).is_err());
}

#[test]
fn validation_rejects_empty_segments() {
    assert!(evaluate_ms(&video_with(vec![play(500, 500, 1.0)], 500), 0).is_err());
    assert!(evaluate_ms(&video_with(vec![play(600, 500, 1.0)], 500), 0).is_err());
    assert!(evaluate_ms(&video_with(vec![hold(0, 0)], 0), 0).is_err());
    assert!(evaluate_ms(&video_with(vec![reverse(700, 700, 1.0)], 700), 0).is_err());
}

#[test]
fn validation_rejects_retime_that_does_not_cover_the_layer() {
    // 1000 ms of output cannot cover a 2000 ms layer.
    let c = video_with(vec![play(0, 1000, 1.0)], 2000);
    assert!(matches!(
        evaluate_ms(&c, 0),
        Err(CompositionError::Invalid { reason, .. }) if reason.contains("less output time")
    ));
    // At double speed, 2000 ms of source covers exactly 1000 ms of output.
    assert!(evaluate_ms(&video_with(vec![play(0, 2000, 2.0)], 1000), 0).is_ok());
    // The same segment is short of a 1200 ms layer.
    assert!(evaluate_ms(&video_with(vec![play(0, 2000, 2.0)], 1200), 0).is_err());
    // 1000 ms of source at double speed is only 500 ms of output: short of 1000 ms.
    assert!(evaluate_ms(&video_with(vec![play(0, 1000, 2.0)], 1000), 0).is_err());
}

#[test]
fn retime_round_trips_through_serde_with_tick_field_names() {
    let segments = vec![play(0, 1000, 1.5), hold(1000, 250), reverse(0, 500, 0.5)];
    let json = serde_json::to_string(&segments).unwrap();
    assert!(json.contains("\"source_in_ticks\""), "{json}");
    assert!(json.contains("\"duration_ticks\""), "{json}");
    let back: Vec<RetimeSegment> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, segments);
}
