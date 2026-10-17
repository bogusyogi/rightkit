//! CutRight's signed-source & f32 fade formulas, copied as independent oracles.
use rightkit_compose::*;

fn ft(fps: Rational, frame: i64) -> i128 {
    let n = i128::from(frame) * i128::from(fps.den) * 705_600_000;
    let d = i128::from(fps.num);
    let q = n.div_euclid(d);
    let r = n.rem_euclid(d);
    q + i128::from(2 * r > d)
}

fn tick(fps: Rational, frame: i64) -> Ticks {
    Ticks(u64::try_from(ft(fps, frame)).unwrap())
}

fn reference_opacity(start: i64, frames: i64, fade_in: i64, fade_out: i64, f: i64) -> f32 {
    let into = f - start;
    let remaining = frames - into - 1;
    let mut opacity = 1.0f32;
    if fade_in > 0 {
        opacity = opacity.min((into + 1) as f32 / fade_in as f32);
    }
    if fade_out > 0 {
        opacity = opacity.min((remaining + 1) as f32 / fade_out as f32);
    }
    opacity.clamp(0.0, 1.0)
}

fn layer(start: Ticks, end: Ticks, retime: Vec<RetimeSegment>) -> Layer {
    Layer {
        id: "clip".into(),
        timing: Timing { start, end },
        transform: vec![],
        opacity: vec![],
        clip: None,
        shadow: None,
        motion_blur: None,
        content: Content::Video {
            asset: AssetRef {
                id: "input".into(),
                blake3: String::new(),
                width: 1920,
                height: 1080,
                duration: None,
            },
            placement: Rect::FULL,
            crop: vec![],
            retime,
        },
    }
}

fn composition(fps: Rational, layers: Vec<Layer>, frames: i64) -> Composition {
    let mut c = Composition::new(
        Canvas {
            width: 1920,
            height: 1080,
        },
        fps,
        tick(fps, frames),
    );
    c.layers = layers;
    c
}

fn source(c: &Composition, t: Ticks) -> SignedTicks {
    match evaluate_v3(c, t).unwrap().layers[0].payload {
        PayloadV3::Video { source, .. } => source,
        ref other => panic!("expected video, got {other:?}"),
    }
}

#[test]
fn source_parity_all_frames_and_boundary_ticks() {
    for fps in [
        Rational::new(11, 1),
        Rational::new(13, 1),
        Rational::new(30, 1),
        Rational::new(30000, 1001),
    ] {
        for start in [0, 1, 4, 13] {
            for source_in in [
                -705_600_000i128,
                7,
                999_999_999_000,
                i128::from(u64::MAX) - 10_000_000_000,
            ] {
                let frames = 12;
                let retime = vec![RetimeSegment::FramePlay {
                    source_in: SignedTicks(source_in),
                    fps,
                    start_frame: start,
                    frames,
                }];
                let c = composition(
                    fps,
                    vec![layer(tick(fps, start), tick(fps, start + frames), retime)],
                    start + frames + 1,
                );
                for f in start..start + frames {
                    let expected = SignedTicks(source_in + ft(fps, f - start));
                    assert_eq!(
                        source(&c, tick(fps, f)),
                        expected,
                        "{fps:?}, start {start}, frame {f}"
                    );
                    assert_eq!(source(&c, Ticks(tick(fps, f + 1).0 - 1)), expected);
                    assert_eq!(source(&c, Ticks(tick(fps, f).0 + 1)), expected);
                }
                assert!(evaluate_v3(&c, tick(fps, start + frames))
                    .unwrap()
                    .layers
                    .is_empty());
                // Ensure signed source fields survive composition & evaluated JSON.
                let json = serde_json::to_string(&c).unwrap();
                let back = Composition::from_json_str(&json).unwrap();
                assert_eq!(back, c);
                let frame = evaluate_v3(&back, tick(fps, start)).unwrap();
                assert_eq!(frame.schema, FRAME_SCHEMA_V3);
                assert_eq!(frame.schema, "rightkit.frame.v3");
                let json = serde_json::to_string(&frame).unwrap();
                assert_eq!(
                    serde_json::from_str::<FrameDescriptionV3>(&json).unwrap(),
                    frame
                );
                // v2 stays the default: same composition, unsigned source when
                // non-negative, evaluation error when negative.
                let v2 = evaluate(&back, tick(fps, start));
                if source_in < 0 {
                    assert!(matches!(v2, Err(CompositionError::Invalid { .. })));
                } else {
                    let v2 = v2.unwrap();
                    assert_eq!(v2.schema, "rightkit.frame.v2");
                    let Payload::Video { source, .. } = &v2.layers[0].payload else {
                        panic!("video")
                    };
                    assert_eq!(SignedTicks::from(*source), SignedTicks(source_in));
                }
            }
        }
    }
}

#[test]
fn frame_conversion_round_trips_and_matches_signed_half_down_oracle() {
    for fps in [
        Rational::new(11, 1),
        Rational::new(13, 1),
        Rational::new(30, 1),
        Rational::new(30000, 1001),
    ] {
        for f in -1000..=1000 {
            let t = fps.frame_to_ticks(f).unwrap();
            assert_eq!(t.0, ft(fps, f));
            assert_eq!(fps.ticks_to_frame(t), Some(f));
            assert_eq!(fps.frame_at_ticks(t), Some(f));
            assert_eq!(fps.frame_at_ticks(SignedTicks(t.0 - 1)), Some(f - 1));
            if f >= 0 {
                assert_eq!(
                    Ticks::from_frames_rounded(f as u64, fps),
                    Some(Ticks(t.0 as u64))
                );
            }
        }
    }
}

#[test]
fn fade_tracks_match_cutright_bits_including_overlap_and_clipped_titles() {
    for fps in [
        Rational::new(11, 1),
        Rational::new(30, 1),
        Rational::new(30000, 1001),
    ] {
        for (start, frames, fade_in, fade_out) in [
            (1, 10, 3, 4),
            (2, 6, 5, 5),
            (-3, 10, 6, 5),
            (3, 1, 1, 1),
            (1, 5, 0, -1),
            (0, 4, 9, 7),
            (0, 10, 1, 0),
        ] {
            let visible_start = start.max(0);
            let visible_end = (start + frames).min(9);
            let fade = FrameFade {
                start_frame: start,
                frames,
                fade_in_frames: fade_in,
                fade_out_frames: fade_out,
            };
            let mut title = layer(tick(fps, visible_start), tick(fps, visible_end), vec![]);
            title.opacity = fade.opacity_track(fps, visible_start, visible_end).unwrap();
            title.id = "title".into();
            let c = composition(fps, vec![title], visible_end + 1);
            for f in visible_start..visible_end {
                let expected = reference_opacity(start, frames, fade_in, fade_out, f);
                let frame = evaluate(&c, tick(fps, f)).unwrap();
                assert_eq!(
                    (frame.layers[0].opacity as f32).to_bits(),
                    expected.to_bits()
                );
                let before_next = evaluate(&c, Ticks(tick(fps, f + 1).0 - 1)).unwrap();
                assert_eq!(
                    (before_next.layers[0].opacity as f32).to_bits(),
                    expected.to_bits()
                );
            }
            assert!(evaluate(&c, tick(fps, visible_end))
                .unwrap()
                .layers
                .is_empty());
            assert_eq!(
                Composition::from_json_str(&serde_json::to_string(&c).unwrap()).unwrap(),
                c
            );
        }
    }
}

#[test]
fn crossfade_ramp_and_static_opacity_follow_cutright_precision() {
    let fps = Rational::new(11, 1);
    for n in [2, 3, 7, 11] {
        let visual_opacity = 0.37f32;
        let opacity = Opacity::new(f64::from(visual_opacity)).unwrap();
        let fade = FrameFade {
            start_frame: 4,
            frames: n + 3,
            fade_in_frames: n,
            fade_out_frames: 0,
        };
        let mut incoming = layer(tick(fps, 4), tick(fps, 4 + n + 3), vec![]);
        incoming.opacity = fade.opacity_track(fps, 4, 4 + n + 3).unwrap();
        for key in &mut incoming.opacity {
            key.value *= opacity.as_f64();
        }
        let c = composition(fps, vec![incoming], 4 + n + 3);
        for k in 0..n + 3 {
            let weight = if k < n {
                (k + 1) as f32 / n as f32
            } else {
                1.0
            };
            let expected = (f64::from(weight) * f64::from(visual_opacity)) as f32;
            assert_eq!(
                (evaluate(&c, tick(fps, 4 + k)).unwrap().layers[0].opacity as f32).to_bits(),
                expected.to_bits()
            );
        }
    }
}

#[test]
fn negative_hold_and_outgoing_handle_need_no_local_source_path() {
    let fps = Rational::new(11, 1);
    let end_source = -705_600_000 + ft(fps, 4);
    let held = -705_600_000 + ft(fps, 3);
    let segments = vec![
        RetimeSegment::FramePlay {
            source_in: SignedTicks(-705_600_000),
            fps,
            start_frame: 1,
            frames: 4,
        },
        RetimeSegment::SignedHold {
            source_at: SignedTicks(held),
            duration: Ticks(tick(fps, 7).0 - tick(fps, 5).0),
        },
        RetimeSegment::FramePlay {
            source_in: SignedTicks(end_source),
            fps,
            start_frame: 7,
            frames: 3,
        },
    ];
    let c = composition(fps, vec![layer(tick(fps, 1), tick(fps, 10), segments)], 11);
    assert_eq!(source(&c, tick(fps, 5)), SignedTicks(held));
    assert_eq!(source(&c, tick(fps, 6)), SignedTicks(held));
    assert_eq!(
        source(&c, tick(fps, 8)),
        SignedTicks(end_source + ft(fps, 1))
    );
    let hold_json = serde_json::to_string(&c).unwrap();
    assert_eq!(Composition::from_json_str(&hold_json).unwrap(), c);
}

#[test]
fn invalid_opacity_is_typed_by_constructor_and_invalid_in_composition() {
    let fps = Rational::new(30, 1);
    for (value, expected) in [
        (1.5, OpacityError::OutOfRange),
        (-0.25, OpacityError::OutOfRange),
        (f64::NAN, OpacityError::NonFinite),
        (f64::INFINITY, OpacityError::NonFinite),
    ] {
        assert_eq!(Opacity::new(value), Err(expected));
        let mut clip = layer(tick(fps, 0), tick(fps, 4), vec![]);
        clip.opacity = vec![Keyframe {
            at: Ticks::ZERO,
            value,
            easing: Easing::Linear,
        }];
        let c = composition(fps, vec![clip], 4);
        assert!(evaluate_v3(&c, Ticks::ZERO).is_err());
        assert!(
            matches!(evaluate(&c, Ticks::ZERO), Err(CompositionError::Invalid { layer, .. }) if layer == "clip")
        );
    }
    let mut clip = layer(tick(fps, 0), tick(fps, 4), vec![]);
    clip.opacity = Opacity::new(f64::from(0.37f32)).unwrap().constant_track();
    let c = composition(fps, vec![clip], 4);
    assert_eq!(
        (evaluate(&c, Ticks::ZERO).unwrap().layers[0].opacity as f32).to_bits(),
        0.37f32.to_bits()
    );
}

#[test]
fn invalid_frame_retime_is_rejected_before_evaluation() {
    let fps = Rational::new(11, 1);
    for (rate, start, frames, source_in) in [
        (fps, 0, 0, 0),
        (fps, 0, -1, 0),
        (fps, 0, 1, 0), // insufficient coverage
        (Rational::new(0, 1), 0, 4, 0),
        (Rational::new(11, 0), 0, 4, 0),
        (Rational::new(30, 1), 0, 4, 0), // output FPS mismatch
        (fps, 1, 4, 0),                  // absolute start mismatch
        (fps, i64::MAX, 4, 0),
        (fps, 0, 4, i128::MAX),
    ] {
        let retime = vec![RetimeSegment::FramePlay {
            source_in: SignedTicks(source_in),
            fps: rate,
            start_frame: start,
            frames,
        }];
        let c = composition(fps, vec![layer(tick(fps, 0), tick(fps, 4), retime)], 4);
        assert!(matches!(
            evaluate(&c, Ticks::ZERO),
            Err(CompositionError::Invalid { .. })
        ));
    }
}

#[test]
fn v2_default_rejects_negative_source_and_v3_preserves_it() {
    let fps = Rational::new(30, 1);
    let retime = vec![RetimeSegment::SignedHold {
        source_at: SignedTicks(-17),
        duration: tick(fps, 4),
    }];
    let c = composition(fps, vec![layer(tick(fps, 0), tick(fps, 4), retime)], 4);
    assert!(matches!(
        evaluate(&c, Ticks::ZERO),
        Err(CompositionError::Invalid { .. })
    ));
    assert_eq!(source(&c, Ticks::ZERO), SignedTicks(-17));
    assert_eq!(FRAME_SCHEMA, "rightkit.frame.v2");
}

#[test]
fn v2_and_v3_agree_on_non_video_and_unsigned_video_layers() {
    let fps = Rational::new(30, 1);
    let c = composition(fps, vec![layer(tick(fps, 0), tick(fps, 4), vec![])], 4);
    let v2 = evaluate_ms(&c, 10).unwrap();
    let v3 = evaluate_ms_v3(&c, 10).unwrap();
    assert_eq!(v2.layers.len(), v3.layers.len());
    assert_eq!(
        PayloadV3::from(v2.layers[0].payload.clone()),
        v3.layers[0].payload
    );
    assert_eq!(v2.layers[0].opacity, v3.layers[0].opacity);
}
