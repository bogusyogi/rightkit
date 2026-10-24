// rightkit-compose 0.2.0 consumed by exact registry pin, outside the workspace.
//
// Proves the evaluated composition model through its public API: the schema and clock constants;
// Ticks, Rational and SignedTicks arithmetic and wire bounds; keyframe sampling and every easing
// family; the retime segments; Opacity and FrameFade; a full composition (background, video with a
// keyframed transform, clip, shadow and motion blur, webcam, cursor, badge, caption, click effects)
// evaluated to pixel-space frame descriptions; the JSON edit-and-reopen round trip; the version 1
// document migration; the signed-source v3 frame against the v2 refusal; and each validation error.
// Public API only.
// Does not prove: any renderer or encoder (the crate has none; the frame description is the
// contract), real media assets (asset ids and sizes are fixtures), or the CutRight consumers.
// State: in-memory values only. No network, no files, no process spawn, no user data.
use rightkit_compose::{
    evaluate, evaluate_ms, evaluate_ms_v3, evaluate_v3, output_span, sample, source_time_at,
    source_time_at_signed, validate, AssetRef, Canvas, ClickShape, Clip, Composition,
    CompositionError, Content, CursorSample, CursorSprite, Easing, FrameDescription, FrameFade,
    FrameFadeError, Keyframe, Layer, Lerp, MotionBlur, Opacity, OpacityError, Pattern, Payload,
    PayloadV3, Point, Rational, Rect, RetimeSegment, Rgba, Shadow, SignedTicks, Stop, Style, Ticks,
    Timing, Transform, CAMERA_BOUNCE_MAX, FRAME_SCHEMA, FRAME_SCHEMA_V3, LEGACY_SCHEMA_V1, SCHEMA,
    TICKS_PER_MS, TICKS_PER_SECOND,
};

const EPS: f64 = 1e-6;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < EPS
}

fn ms(v: u64) -> Ticks {
    Ticks::from_ms(v)
}

fn canvas() -> Canvas {
    Canvas {
        width: 1920,
        height: 1080,
    }
}

fn asset(id: &str, width: u32, height: u32, duration_ms: Option<u64>) -> AssetRef {
    AssetRef {
        id: id.into(),
        blake3: format!("blake3-{id}"),
        width,
        height,
        duration: duration_ms.map(Ticks::from_ms),
    }
}

fn rgba(a: f64) -> Rgba {
    Rgba {
        r: 0.1,
        g: 0.2,
        b: 0.3,
        a,
    }
}

fn style() -> Style {
    Style {
        foreground: rgba(1.0),
        background: rgba(0.5),
    }
}

fn layer(id: &str, start_ms: u64, end_ms: u64, content: Content) -> Layer {
    Layer {
        id: id.into(),
        timing: Timing {
            start: ms(start_ms),
            end: ms(end_ms),
        },
        transform: Vec::new(),
        opacity: Vec::new(),
        clip: None,
        shadow: None,
        motion_blur: None,
        content,
    }
}

fn video(id: &str, source: AssetRef, start_ms: u64, end_ms: u64) -> Layer {
    layer(
        id,
        start_ms,
        end_ms,
        Content::Video {
            asset: source,
            placement: Rect::FULL,
            crop: Vec::new(),
            retime: Vec::new(),
        },
    )
}

fn frame_payload<'a>(frame: &'a FrameDescription, id: &str) -> &'a Payload {
    &frame
        .layers
        .iter()
        .find(|l| l.id == id)
        .unwrap_or_else(|| panic!("layer {id} active"))
        .payload
}

fn constants() {
    assert_eq!(SCHEMA, "rightkit.composition.v2");
    assert_eq!(LEGACY_SCHEMA_V1, "rightkit.composition.v1");
    assert_eq!(FRAME_SCHEMA, "rightkit.frame.v2");
    assert_eq!(FRAME_SCHEMA_V3, "rightkit.frame.v3");
    assert_eq!(TICKS_PER_SECOND, 705_600_000);
    assert_eq!(TICKS_PER_MS, 705_600);
    assert_eq!(TICKS_PER_SECOND, TICKS_PER_MS * 1000);
    assert_eq!(CAMERA_BOUNCE_MAX, 0.3);
}

fn ticks_and_rationals() {
    assert_eq!(ms(1), Ticks(705_600));
    assert_eq!(Ticks::from_ms(u64::MAX), Ticks::MAX, "huge values saturate");
    assert_eq!(Ticks::ZERO, Ticks::default());
    assert_eq!(Ticks::from_seconds_rational(1, 2), Some(Ticks(352_800_000)));
    assert_eq!(Ticks::from_seconds_rational(1, 0), None);
    assert_eq!(Ticks::from_seconds_rational(u64::MAX, 1), None);
    assert_eq!(ms(1500).to_ms_f64(), 1500.0);
    assert_eq!(Ticks(TICKS_PER_SECOND).to_seconds_f64(), 1.0);
    assert_eq!(Ticks(1).checked_sub(Ticks(2)), None);
    assert_eq!(Ticks(5).checked_sub(Ticks(2)), Some(Ticks(3)));
    assert_eq!(Ticks::MAX.checked_add(Ticks(1)), None);
    assert_eq!(Ticks::MAX.saturating_add(Ticks(1)), Ticks::MAX);
    assert_eq!(Ticks(5).to_string(), "5 ticks");

    // NTSC 29.97 has a whole-tick frame, which is the reason the clock rate was chosen.
    let ntsc = Rational::new(30000, 1001);
    assert_eq!(ntsc.frame_ticks(), Some(23_543_520));
    assert_eq!(Ticks::from_frames(1, ntsc), Some(Ticks(23_543_520)));
    assert_eq!(
        Ticks::from_frames(30000, ntsc),
        Some(Ticks(706_305_600_000))
    );
    assert_eq!(Rational::new(30, 1).as_f64(), 30.0);
    // 11 fps has no whole-tick frame: floor and half-down rounding differ by one tick.
    let eleven = Rational::new(11, 1);
    assert_eq!(eleven.frame_ticks(), None);
    assert_eq!(Ticks::from_frames(1, eleven), Some(Ticks(64_145_454)));
    assert_eq!(
        Ticks::from_frames_rounded(1, eleven),
        Some(Ticks(64_145_455))
    );
    for invalid in [Rational::new(0, 1), Rational::new(1, 0)] {
        assert_eq!(Ticks::from_frames(1, invalid), None);
        assert_eq!(Ticks::from_frames_rounded(1, invalid), None);
        assert_eq!(invalid.frame_ticks(), None);
    }

    // Signed frame grid at 30 fps: one frame is 23,520,000 ticks.
    let thirty = Rational::new(30, 1);
    assert_eq!(thirty.frame_to_ticks(1), Some(SignedTicks(23_520_000)));
    assert_eq!(thirty.frame_to_ticks(-1), Some(SignedTicks(-23_520_000)));
    assert_eq!(thirty.ticks_to_frame(SignedTicks(23_520_000)), Some(1));
    assert_eq!(thirty.frame_at_ticks(SignedTicks(23_519_999)), Some(0));
    assert_eq!(thirty.frame_at_ticks(SignedTicks(23_520_000)), Some(1));
    assert_eq!(Rational::new(0, 1).frame_to_ticks(1), None);
    assert_eq!(Rational::new(0, 1).frame_at_ticks(SignedTicks(0)), None);
    assert_eq!(thirty.frame_at_ticks(SignedTicks(i128::MAX)), None);

    // SignedTicks conversion and wire bounds.
    assert_eq!(SignedTicks::from(Ticks(7)), SignedTicks(7));
    assert_eq!(Ticks::try_from(SignedTicks(7)), Ok(Ticks(7)));
    assert!(Ticks::try_from(SignedTicks(-1)).is_err());
    assert_eq!(
        Ticks::try_from(SignedTicks::from(Ticks::MAX)),
        Ok(Ticks::MAX)
    );
    assert!(SignedTicks::MIN.is_wire_representable());
    assert!(SignedTicks::MAX.is_wire_representable());
    assert!(!SignedTicks(SignedTicks::MAX.0 + 1).is_wire_representable());
    for value in [
        SignedTicks::MIN,
        SignedTicks(-1),
        SignedTicks(0),
        SignedTicks::MAX,
    ] {
        let wire = serde_json::to_string(&value).unwrap();
        assert_eq!(serde_json::from_str::<SignedTicks>(&wire).unwrap(), value);
    }
    assert_eq!(
        serde_json::to_string(&SignedTicks::MAX).unwrap(),
        u64::MAX.to_string()
    );
    assert!(serde_json::to_string(&SignedTicks(SignedTicks::MAX.0 + 1)).is_err());
    assert!(serde_json::from_str::<SignedTicks>("18446744073709551616").is_err());
    assert!(serde_json::from_str::<SignedTicks>("-9223372036854775809").is_err());
}

fn tracks_and_easing() {
    let key = |at_ms: u64, value: f64, easing: Easing| Keyframe {
        at: ms(at_ms),
        value,
        easing,
    };
    let linear = vec![key(0, 1.0, Easing::Linear), key(1000, 0.0, Easing::Linear)];
    assert_eq!(sample(&linear, ms(500)), Some(0.5));
    assert_eq!(sample(&linear, ms(0)), Some(1.0));
    assert_eq!(sample(&linear, ms(5000)), Some(0.0), "the last value holds");
    assert_eq!(
        sample::<f64>(&[], ms(0)),
        None,
        "an empty track has no value"
    );
    let later = vec![
        key(500, 0.25, Easing::Linear),
        key(1000, 1.0, Easing::Linear),
    ];
    assert_eq!(
        sample(&later, ms(0)),
        Some(0.25),
        "the first value holds before the first key"
    );
    let held = vec![key(0, 1.0, Easing::Hold), key(1000, 0.0, Easing::Linear)];
    assert_eq!(
        sample(&held, ms(999)),
        Some(1.0),
        "a Hold key keeps its value until the next key"
    );

    // Each segment is shaped by the easing of the key it starts at.
    assert_eq!(Easing::default(), Easing::Linear);
    assert_eq!(Easing::Linear.apply(0.25), 0.25);
    assert_eq!(Easing::EaseIn.apply(0.5), 0.125);
    assert_eq!(Easing::EaseOut.apply(0.5), 0.875);
    assert_eq!(Easing::EaseInOut.apply(0.25), 0.0625);
    assert_eq!(Easing::EaseInOut.apply(0.5), 0.5);
    assert!(close(Easing::Cosine.apply(0.5), 0.5));
    assert_eq!(Easing::Hold.apply(0.5), 0.0);
    let straight = Easing::CubicBezier {
        x1: 0.0,
        y1: 0.0,
        x2: 1.0,
        y2: 1.0,
    };
    assert!(
        close(straight.apply(0.3), 0.3),
        "a diagonal bezier is the identity"
    );

    // ScreenRight's camera curve: pinned endpoints and published sample values.
    let critical = Easing::CameraSpring { bounce: 0.0 };
    let bouncy = Easing::CameraSpring { bounce: 0.15 };
    for curve in [critical, bouncy] {
        assert_eq!(curve.apply(0.0), 0.0);
        assert_eq!(curve.apply(1.0), 1.0);
    }
    assert!(close(critical.apply(0.5), 0.944399), "critical at 0.5");
    assert!(close(bouncy.apply(0.5), 0.973860), "bouncy at 0.5");
    assert!(bouncy.apply(0.7) > 1.0, "an underdamped curve overshoots");
    assert_eq!(
        Easing::CameraSpring { bounce: 5.0 }.apply(0.4),
        Easing::CameraSpring {
            bounce: CAMERA_BOUNCE_MAX
        }
        .apply(0.4),
        "bounce clamps to its maximum"
    );
    // A unit-mass spring with omega 9.23 reproduces the critical camera curve, and a spring
    // (unlike the camera curve) depends on the real segment length.
    let omega: f64 = 9.23;
    let spring = Easing::Spring {
        stiffness: omega * omega,
        damping: 2.0 * omega,
        mass: 1.0,
    };
    assert!(close(spring.apply_over(0.5, ms(1000)), critical.apply(0.5)));
    assert!((spring.apply_over(0.5, ms(1000)) - spring.apply_over(0.5, ms(2000))).abs() > 0.01);
    assert_eq!(
        critical.apply_over(0.5, ms(1000)),
        critical.apply_over(0.5, ms(2000))
    );

    // Interpolation of the keyframable value types.
    assert_eq!(f64::lerp(0.0, 10.0, 0.25), 2.5);
    let from = Rect {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };
    let to = Rect {
        x: 0.5,
        y: 0.25,
        width: 0.5,
        height: 0.5,
    };
    assert_eq!(
        Rect::lerp(from, to, 0.5),
        Rect {
            x: 0.25,
            y: 0.125,
            width: 0.75,
            height: 0.75
        }
    );
    let mid = Transform::lerp(
        Transform::IDENTITY,
        Transform {
            dx: 0.2,
            dy: -0.2,
            scale: 2.0,
        },
        0.5,
    );
    assert_eq!(
        mid,
        Transform {
            dx: 0.1,
            dy: -0.1,
            scale: 1.5
        }
    );
    assert_eq!(
        serde_json::to_string(&Easing::Cosine).unwrap(),
        r#"{"kind":"cosine"}"#
    );
}

fn retime_segments() {
    let play = |a: u64, b: u64, speed: f64| RetimeSegment::Play {
        source_in: ms(a),
        source_out: ms(b),
        speed,
    };
    let hold = |at: u64, duration: u64| RetimeSegment::Hold {
        source_at: ms(at),
        duration: ms(duration),
    };
    let reverse = |a: u64, b: u64, speed: f64| RetimeSegment::Reverse {
        source_in: ms(a),
        source_out: ms(b),
        speed,
    };

    let unit = [play(1000, 3000, 1.0)];
    assert_eq!(source_time_at(&unit, ms(500)), Some(ms(1500)));
    assert_eq!(source_time_at(&unit, ms(1999)), Some(ms(2999)));
    assert_eq!(
        source_time_at(&unit, ms(2000)),
        None,
        "the end is exclusive"
    );
    assert_eq!(play(0, 4000, 2.0).output_duration(), ms(2000));
    assert_eq!(
        source_time_at(&[play(0, 4000, 2.0)], ms(500)),
        Some(ms(1000))
    );
    assert_eq!(source_time_at(&[hold(750, 500)], ms(499)), Some(ms(750)));
    assert_eq!(source_time_at(&[hold(750, 500)], ms(500)), None);
    assert_eq!(
        source_time_at(&[reverse(1000, 3000, 1.0)], ms(500)),
        Some(ms(2500))
    );
    assert_eq!(
        source_time_at(&[], ms(0)),
        None,
        "an empty list maps nothing"
    );

    // Segments chain end to end and each boundary belongs to the next segment.
    let chain = [
        play(0, 1000, 1.0),
        hold(1000, 500),
        reverse(2000, 4000, 2.0),
    ];
    assert_eq!(output_span(&chain), ms(2500));
    assert_eq!(source_time_at(&chain, ms(999)), Some(ms(999)));
    assert_eq!(source_time_at(&chain, ms(1000)), Some(ms(1000)));
    assert_eq!(source_time_at(&chain, ms(1499)), Some(ms(1000)));
    assert_eq!(source_time_at(&chain, ms(1500)), Some(ms(4000)));
    assert_eq!(source_time_at(&chain, ms(2500)), None);
    assert_eq!(
        RetimeSegment::constant(2.0),
        RetimeSegment::Play {
            source_in: Ticks::ZERO,
            source_out: Ticks::MAX,
            speed: 2.0
        }
    );

    // Frame-quantised play holds each frame's source time between frame starts.
    let thirty = Rational::new(30, 1);
    let frame_play = RetimeSegment::FramePlay {
        source_in: SignedTicks(0),
        fps: thirty,
        start_frame: 0,
        frames: 30,
    };
    assert_eq!(frame_play.output_duration(), Ticks(TICKS_PER_SECOND));
    assert_eq!(
        source_time_at_signed(&[frame_play], Ticks(23_520_000)),
        Some(SignedTicks(23_520_000))
    );
    assert_eq!(
        source_time_at_signed(&[frame_play], Ticks(23_520_005)),
        Some(SignedTicks(23_520_000))
    );
    assert_eq!(
        source_time_at_signed(&[frame_play], Ticks(TICKS_PER_SECOND)),
        None
    );

    // Signed pre-roll survives the signed reader and is refused by the unsigned one.
    let preroll = RetimeSegment::SignedHold {
        source_at: SignedTicks(-17),
        duration: Ticks(4),
    };
    assert_eq!(
        source_time_at_signed(&[preroll], Ticks(3)),
        Some(SignedTicks(-17))
    );
    assert_eq!(source_time_at(&[preroll], Ticks(3)), None);
    assert_eq!(source_time_at_signed(&[preroll], Ticks(4)), None);
    let max_hold = RetimeSegment::Hold {
        source_at: Ticks::MAX,
        duration: Ticks(4),
    };
    assert_eq!(source_time_at(&[max_hold], Ticks(3)), Some(Ticks::MAX));
}

fn opacity_and_fades() {
    assert_eq!(Opacity::new(0.25).unwrap().as_f64(), 0.25);
    assert_eq!(Opacity::new(0.5).unwrap().as_f32(), 0.5f32);
    assert_eq!(Opacity::TRANSPARENT.as_f64(), 0.0);
    assert_eq!(Opacity::OPAQUE.as_f64(), 1.0);
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(Opacity::new(value), Err(OpacityError::NonFinite));
        assert_eq!(Opacity::clamped(value), Err(OpacityError::NonFinite));
    }
    assert_eq!(Opacity::new(1.5), Err(OpacityError::OutOfRange));
    assert_eq!(Opacity::new(-0.25), Err(OpacityError::OutOfRange));
    assert_eq!(Opacity::clamped(1.5), Ok(Opacity::OPAQUE));
    assert_eq!(Opacity::clamped(-0.25), Ok(Opacity::TRANSPARENT));
    assert_eq!(
        serde_json::to_string(&Opacity::new(0.25).unwrap()).unwrap(),
        "0.25"
    );
    assert!(
        serde_json::from_str::<Opacity>("1.5").is_err(),
        "deserialising validates"
    );
    let constant = Opacity::new(0.5).unwrap().constant_track();
    assert_eq!(constant.len(), 1);
    assert_eq!(
        (constant[0].at, constant[0].value, constant[0].easing),
        (Ticks::ZERO, 0.5, Easing::Hold)
    );

    // Fade ramps use f32 arithmetic; overlapping ramps combine with min.
    let fade = FrameFade {
        start_frame: 5,
        frames: 4,
        fade_in_frames: 3,
        fade_out_frames: 3,
    };
    let expected = [1.0f32 / 3.0, 2.0 / 3.0, 2.0 / 3.0, 1.0 / 3.0];
    for (index, value) in expected.into_iter().enumerate() {
        assert_eq!(
            fade.opacity_at(5 + index as i64).as_f32().to_bits(),
            value.to_bits()
        );
    }
    assert_eq!(fade.opacity_at(4), Opacity::TRANSPARENT);
    assert_eq!(fade.opacity_at(9), Opacity::TRANSPARENT);
    let no_ramp = FrameFade {
        fade_in_frames: -1,
        fade_out_frames: 0,
        ..fade
    };
    assert_eq!(
        no_ramp.opacity_at(6),
        Opacity::OPAQUE,
        "nonpositive lengths disable a ramp"
    );

    // The layer-local Hold track: one key per changed value, at rounded frame starts.
    let ramp = FrameFade {
        start_frame: 0,
        frames: 4,
        fade_in_frames: 2,
        fade_out_frames: 2,
    };
    let track = ramp.opacity_track(Rational::new(30, 1), 0, 4).unwrap();
    assert_eq!(
        track
            .iter()
            .map(|k| (k.at, k.value, k.easing))
            .collect::<Vec<_>>(),
        [
            (Ticks(0), 0.5, Easing::Hold),
            (Ticks(23_520_000), 1.0, Easing::Hold),
            (Ticks(70_560_000), 0.5, Easing::Hold),
        ]
    );
    for (start, end) in [(-1, 2), (2, 2), (0, 5)] {
        assert_eq!(
            ramp.opacity_track(Rational::new(30, 1), start, end),
            Err(FrameFadeError::VisibleRange)
        );
    }
    assert_eq!(
        ramp.opacity_track(Rational::new(0, 1), 0, 4),
        Err(FrameFadeError::FrameTiming)
    );
}

fn full_composition() -> Composition {
    let mut screen = video(
        "screen",
        asset("screen-src", 1920, 1080, Some(8000)),
        0,
        8000,
    );
    screen.transform = vec![
        Keyframe {
            at: ms(0),
            value: Transform::IDENTITY,
            easing: Easing::EaseInOut,
        },
        Keyframe {
            at: ms(2000),
            value: Transform {
                dx: -0.1,
                dy: 0.0,
                scale: 1.6,
            },
            easing: Easing::Hold,
        },
        Keyframe {
            at: ms(4000),
            value: Transform::IDENTITY,
            easing: Easing::Linear,
        },
    ];
    screen.clip = Some(Clip::RoundedRect { radius: 0.015 });
    screen.shadow = Some(Shadow {
        offset_x: 0.0,
        offset_y: 0.008,
        blur: 0.02,
        color: rgba(0.35),
    });
    screen.motion_blur = Some(MotionBlur {
        shutter_degrees: 180.0,
        samples: 5,
    });

    let background = layer(
        "bg",
        0,
        8000,
        Content::Background {
            pattern: Pattern::LinearGradient {
                angle_deg: 135.0,
                stops: vec![
                    Stop {
                        at: 0.0,
                        color: rgba(1.0),
                    },
                    Stop {
                        at: 1.0,
                        color: rgba(0.2),
                    },
                ],
            },
        },
    );
    let webcam = layer(
        "cam",
        1000,
        6000,
        Content::Webcam {
            asset: asset("cam-src", 1280, 720, Some(9000)),
            placement: Rect {
                x: 0.78,
                y: 0.68,
                width: 0.2,
                height: 0.28,
            },
            source_start: ms(500),
        },
    );
    let cursor = layer(
        "cursor",
        0,
        10_000,
        Content::Cursor {
            follows: "screen".into(),
            scale: 1.0,
            sprite: CursorSprite {
                asset: asset("arrow", 32, 32, None),
                hotspot: Point { x: 4.0, y: 4.0 },
            },
            samples: vec![
                CursorSample {
                    at: ms(0),
                    x: 0.2,
                    y: 0.2,
                    visible: true,
                },
                CursorSample {
                    at: ms(3000),
                    x: 0.6,
                    y: 0.4,
                    visible: true,
                },
                CursorSample {
                    at: ms(5000),
                    x: 0.6,
                    y: 0.4,
                    visible: false,
                },
            ],
        },
    );
    let badge = layer(
        "badge",
        2000,
        3500,
        Content::Badge {
            label: "Ctrl-S".into(),
            anchor: Point { x: 0.5, y: 0.1 },
            style: style(),
        },
    );
    let caption = layer(
        "caption",
        0,
        8000,
        Content::Caption {
            text: "Press Control S to save".into(),
            rect: Rect {
                x: 0.1,
                y: 0.85,
                width: 0.8,
                height: 0.08,
            },
            style: style(),
        },
    );
    let ripple = layer(
        "ripple",
        0,
        3000,
        Content::ClickEffect {
            anchor: Point { x: 0.5, y: 0.5 },
            effect: ClickShape::Ripple {
                max_radius: 0.05,
                stroke_width: 0.005,
            },
            style: style(),
            onset: ms(500),
            duration: ms(1000),
            easing: Easing::EaseOut,
        },
    );
    let highlight = layer(
        "highlight",
        0,
        3000,
        Content::ClickEffect {
            anchor: Point { x: 0.5, y: 0.5 },
            effect: ClickShape::Highlight {
                half_width: 0.1,
                half_height: 0.05,
                grow: 0.5,
                corner_radius: 0.02,
            },
            style: style(),
            onset: ms(500),
            duration: ms(1000),
            easing: Easing::EaseOut,
        },
    );
    let mut composition = Composition::new(canvas(), Rational::new(30, 1), ms(10_000));
    composition.layers = vec![
        background, screen, webcam, cursor, badge, caption, ripple, highlight,
    ];
    composition
}

fn evaluate_full_composition() {
    let composition = full_composition();
    assert_eq!(composition.schema, SCHEMA);
    validate(&composition).expect("the fixture is valid");

    // t = 2000 ms: every layer but the webcam-late cursor sample is active; stack order is back to front.
    let frame = evaluate_ms(&composition, 2000).unwrap();
    assert_eq!(frame.schema, FRAME_SCHEMA);
    assert_eq!((frame.t, frame.width, frame.height), (ms(2000), 1920, 1080));
    assert_eq!(
        evaluate(&composition, ms(2000)).unwrap(),
        frame,
        "evaluate_ms wraps evaluate exactly"
    );
    let order: Vec<(&str, usize)> = frame
        .layers
        .iter()
        .map(|l| (l.id.as_str(), l.stack_index))
        .collect();
    assert_eq!(
        order,
        [
            ("bg", 0),
            ("screen", 1),
            ("cam", 2),
            ("cursor", 3),
            ("badge", 4),
            ("caption", 5),
            ("ripple", 6),
            ("highlight", 7)
        ]
    );

    // Video: the Hold key at 2000 ms applies scale 1.6 and dx -0.1 about the placement centre.
    let screen = frame.layers.iter().find(|l| l.id == "screen").unwrap();
    let Payload::Video {
        asset_id,
        source,
        crop,
        dest_px,
    } = &screen.payload
    else {
        panic!("video payload")
    };
    assert_eq!(asset_id, "screen-src");
    assert_eq!(
        *source,
        ms(2000),
        "no retime means source time equals layer time"
    );
    assert_eq!(*crop, Rect::FULL);
    assert!(close(dest_px.width, 3072.0) && close(dest_px.height, 1728.0));
    assert!(close(dest_px.x, -768.0) && close(dest_px.y, -324.0));
    assert_eq!(
        screen.opacity, 1.0,
        "a missing opacity track is fully opaque"
    );
    // Clip, shadow and blur sizes scale with the shorter canvas side (1080 px).
    assert!(
        matches!(screen.clip, Some(rightkit_compose::ClipPx::RoundedRect { radius_px }) if close(radius_px, 16.2))
    );
    let shadow = screen.shadow.unwrap();
    assert!(
        close(shadow.offset_y_px, 8.64) && close(shadow.blur_px, 21.6) && shadow.offset_x_px == 0.0
    );
    assert_eq!(
        screen.motion_blur.as_ref().unwrap().offsets_ticks,
        [-4_704_000, -2_352_000, 0, 2_352_000, 4_704_000],
        "five samples over a 180 degree shutter at 30 fps, centred on the frame"
    );

    // Webcam keeps its own source offset; the cursor tip follows the zoomed source.
    let Payload::Webcam { source, .. } = frame_payload(&frame, "cam") else {
        panic!("webcam payload")
    };
    assert_eq!(*source, ms(1500));
    let later = evaluate_ms(&composition, 3000).unwrap();
    let Payload::Cursor {
        visible,
        tip_px,
        sprite_px,
        ..
    } = frame_payload(&later, "cursor")
    else {
        panic!("cursor payload")
    };
    assert!(*visible);
    assert!(close(tip_px.x, 1075.2) && close(tip_px.y, 367.2));
    assert!(close(sprite_px.x, tip_px.x - 4.0 * 1.6) && close(sprite_px.width, 32.0 * 1.6));

    // Canvas-space text and badges are placed in pixels; a layer's end is exclusive.
    let Payload::Badge {
        label, anchor_px, ..
    } = frame_payload(&frame, "badge")
    else {
        panic!("badge payload")
    };
    assert_eq!(label, "Ctrl-S");
    assert!(close(anchor_px.x, 960.0) && close(anchor_px.y, 108.0));
    let Payload::Caption { rect_px, text, .. } = frame_payload(&frame, "caption") else {
        panic!("caption payload")
    };
    assert_eq!(text, "Press Control S to save");
    assert!(
        close(rect_px.x, 192.0)
            && close(rect_px.y, 918.0)
            && close(rect_px.width, 1536.0)
            && close(rect_px.height, 86.4)
    );
    assert!(evaluate_ms(&composition, 3500)
        .unwrap()
        .layers
        .iter()
        .all(|l| l.id != "badge"));

    // The cursor hides at the idle sample, and hides while the followed layer is inactive.
    let idle = evaluate_ms(&composition, 5000).unwrap();
    let Payload::Cursor { visible, .. } = frame_payload(&idle, "cursor") else {
        panic!("cursor payload")
    };
    assert!(!visible);

    // Click effects: nothing before onset, half way at 1000 ms, finished at 1500 ms.
    let before = evaluate_ms(&composition, 100).unwrap();
    let Payload::Ripple {
        radius_px, alpha, ..
    } = frame_payload(&before, "ripple")
    else {
        panic!("ripple payload")
    };
    assert_eq!((*radius_px, *alpha), (0.0, 0.0));
    let half = evaluate_ms(&composition, 1000).unwrap();
    let Payload::Ripple {
        center_px,
        radius_px,
        stroke_px,
        alpha,
        ..
    } = frame_payload(&half, "ripple")
    else {
        panic!("ripple payload")
    };
    assert!(close(center_px.x, 960.0) && close(center_px.y, 540.0));
    assert!(
        close(*radius_px, 0.05 * 1080.0 * 0.875) && close(*stroke_px, 5.4) && close(*alpha, 0.5)
    );
    let Payload::Highlight {
        rect_px,
        corner_radius_px,
        alpha,
        ..
    } = frame_payload(&half, "highlight")
    else {
        panic!("highlight payload")
    };
    assert!(
        close(rect_px.width, 2.0 * 0.1 * 1.4375 * 1920.0)
            && close(rect_px.x, 960.0 - 0.1 * 1.4375 * 1920.0)
    );
    assert!(close(*corner_radius_px, 21.6) && close(*alpha, 0.5));
    let done = evaluate_ms(&composition, 1500).unwrap();
    let Payload::Ripple { alpha, .. } = frame_payload(&done, "ripple") else {
        panic!("ripple payload")
    };
    assert_eq!(*alpha, 0.0);
    let Payload::Highlight { alpha, .. } = frame_payload(&done, "highlight") else {
        panic!("highlight payload")
    };
    assert_eq!(*alpha, 0.0);

    // The cursor is hidden while the layer it follows is inactive.
    let mut late = composition.clone();
    late.layers[1].timing.start = ms(2500);
    let early = evaluate_ms(&late, 1000).unwrap();
    assert!(early.layers.iter().all(|l| l.id != "screen"));
    let Payload::Cursor { visible, .. } = frame_payload(&early, "cursor") else {
        panic!("cursor payload")
    };
    assert!(!visible);
}

fn json_round_trip_and_edit() {
    let original = full_composition();
    let json = serde_json::to_string(&original).unwrap();
    assert!(json.contains("\"schema\":\"rightkit.composition.v2\""));
    assert!(json.contains("\"duration_ticks\":7056000000"));
    let reopened = Composition::from_json_str(&json).unwrap();
    assert_eq!(reopened, original);
    assert_eq!(
        serde_json::from_str::<Composition>(&json).unwrap(),
        original
    );
    for t in [0, 1, 999, 2000, 3000, 3499, 3500, 4500, 6000, 7999] {
        assert_eq!(
            evaluate_ms(&original, t).unwrap(),
            evaluate_ms(&reopened, t).unwrap(),
            "frame {t}"
        );
    }

    // Editing one keyframe and reopening changes only the frames that keyframe governs.
    let mut edited = original.clone();
    edited.layers[1].transform[1].value = Transform {
        dx: 0.0,
        dy: 0.0,
        scale: 2.0,
    };
    let reopened = Composition::from_json_str(&serde_json::to_string(&edited).unwrap()).unwrap();
    assert_eq!(reopened, edited);
    let before = evaluate_ms(&original, 2000).unwrap();
    let after = evaluate_ms(&reopened, 2000).unwrap();
    assert_ne!(before, after);
    let Payload::Video { dest_px, .. } = frame_payload(&after, "screen") else {
        panic!("video payload")
    };
    assert!(close(dest_px.width, 3840.0));
    assert_eq!(
        evaluate_ms(&original, 5000).unwrap(),
        evaluate_ms(&reopened, 5000).unwrap()
    );
}

const V1_DOCUMENT: &str = r#"{
  "schema": "rightkit.composition.v1",
  "canvas": {"width": 1920, "height": 1080, "frame_rate_num": 30000, "frame_rate_den": 1001},
  "duration_ms": 3000,
  "layers": [
    {
      "id": "screen",
      "timing": {"start_ms": 1000, "end_ms": 3000},
      "transform": [{"at_ms": 0, "value": {"dx": 0, "dy": 0, "scale": 1}}, {"at_ms": 1000, "value": {"dx": 0, "dy": 0, "scale": 2}}],
      "content": {
        "kind": "video",
        "asset": {"id": "src", "blake3": "b3", "width": 1920, "height": 1080, "duration_ms": 8000},
        "placement": {"x": 0, "y": 0, "width": 1, "height": 1},
        "source_start_ms": 500,
        "speed": 2.0
      }
    }
  ]
}"#;

fn legacy_documents_and_errors() {
    // Version 1 millisecond fields convert exactly; speed becomes one retime segment.
    let migrated = Composition::from_json_str(V1_DOCUMENT).expect("version 1 reads");
    assert_eq!(migrated.schema, SCHEMA);
    assert_eq!(migrated.fps, Rational::new(30000, 1001));
    assert_eq!(migrated.duration, ms(3000));
    assert_eq!(
        migrated.layers[0].timing,
        Timing {
            start: ms(1000),
            end: ms(3000)
        }
    );
    assert_eq!(migrated.layers[0].transform[1].at, Ticks(705_600_000));
    let Content::Video { retime, .. } = &migrated.layers[0].content else {
        panic!("video content")
    };
    assert_eq!(
        retime,
        &vec![RetimeSegment::Play {
            source_in: ms(500),
            source_out: ms(4500),
            speed: 2.0
        }]
    );
    // The old model read 500 + 2 * 500 = 1500 ms of source at local 500 ms.
    let frame = evaluate_ms(&migrated, 1500).unwrap();
    assert!(
        matches!(frame_payload(&frame, "screen"), Payload::Video { source, .. } if *source == ms(1500))
    );
    // A reverse that would start before source zero is refused during conversion.
    let before_zero = V1_DOCUMENT
        .replace("\"source_start_ms\": 500", "\"source_start_ms\": 100")
        .replace("\"speed\": 2.0", "\"speed\": -1.0");
    assert!(matches!(
        Composition::from_json_str(&before_zero),
        Err(CompositionError::Legacy(_))
    ));
    assert!(matches!(
        Composition::from_json_str("{not json"),
        Err(CompositionError::Json(_))
    ));

    // Validation: each structural error by exact variant, reported before anything is drawn.
    let base = || {
        let mut c = Composition::new(canvas(), Rational::new(30, 1), ms(10_000));
        c.layers = vec![video("screen", asset("a", 10, 10, None), 0, 10_000)];
        c
    };
    assert_eq!(validate(&base()), Ok(()));
    assert!(
        matches!(evaluate_ms(&base(), 10_000), Err(CompositionError::OutOfRange { t, duration }) if t == ms(10_000) && duration == ms(10_000))
    );
    let mut wrong = base();
    wrong.schema = "rightkit.composition.v0".into();
    assert_eq!(
        evaluate_ms(&wrong, 0),
        Err(CompositionError::Schema("rightkit.composition.v0".into()))
    );
    let mut zero_canvas = base();
    zero_canvas.canvas.width = 0;
    assert_eq!(validate(&zero_canvas), Err(CompositionError::Canvas));
    let mut zero_rate = base();
    zero_rate.fps = Rational::new(30, 0);
    assert_eq!(validate(&zero_rate), Err(CompositionError::FrameRate));
    let mut zero_duration = base();
    zero_duration.duration = Ticks::ZERO;
    assert_eq!(validate(&zero_duration), Err(CompositionError::Duration));
    let mut duplicate = base();
    duplicate
        .layers
        .push(video("screen", asset("b", 10, 10, None), 0, 10_000));
    assert_eq!(
        validate(&duplicate),
        Err(CompositionError::LayerId("screen".into()))
    );
    let mut unnamed = base();
    unnamed.layers[0].id = String::new();
    assert_eq!(
        validate(&unnamed),
        Err(CompositionError::LayerId(String::new()))
    );
    let mut too_long = base();
    too_long.layers[0].timing.end = ms(20_000);
    assert!(
        matches!(validate(&too_long), Err(CompositionError::Invalid { layer, .. }) if layer == "screen")
    );
    let mut bad_opacity = base();
    bad_opacity.layers[0].opacity = vec![Keyframe {
        at: ms(0),
        value: 1.5,
        easing: Easing::Linear,
    }];
    assert!(matches!(
        validate(&bad_opacity),
        Err(CompositionError::Invalid { .. })
    ));
    let mut unsorted = base();
    unsorted.layers[0].opacity = vec![
        Keyframe {
            at: ms(500),
            value: 0.5,
            easing: Easing::Linear,
        },
        Keyframe {
            at: ms(100),
            value: 1.0,
            easing: Easing::Linear,
        },
    ];
    assert!(matches!(
        validate(&unsorted),
        Err(CompositionError::Invalid { .. })
    ));
    let mut bad_bezier = base();
    bad_bezier.layers[0].opacity = vec![
        Keyframe {
            at: ms(0),
            value: 1.0,
            easing: Easing::CubicBezier {
                x1: 1.5,
                y1: 0.0,
                x2: 0.5,
                y2: 1.0,
            },
        },
        Keyframe {
            at: ms(1000),
            value: 0.0,
            easing: Easing::Linear,
        },
    ];
    assert!(matches!(
        validate(&bad_bezier),
        Err(CompositionError::Invalid { .. })
    ));
    let mut transformed_background = base();
    transformed_background.layers = vec![layer(
        "bg",
        0,
        10_000,
        Content::Background {
            pattern: Pattern::Solid { color: rgba(1.0) },
        },
    )];
    transformed_background.layers[0].transform = vec![Keyframe {
        at: ms(0),
        value: Transform::IDENTITY,
        easing: Easing::Linear,
    }];
    assert!(
        matches!(validate(&transformed_background), Err(CompositionError::Invalid { layer, .. }) if layer == "bg")
    );
    assert_eq!(
        CompositionError::Canvas.to_string(),
        "canvas width and height must be positive"
    );
}

fn signed_source_frames() {
    // A video held at negative pre-roll: the v3 frame keeps it, the default v2 frame refuses it.
    let mut composition = Composition::new(canvas(), Rational::new(30, 1), ms(1000));
    composition.layers = vec![layer(
        "pre",
        0,
        100,
        Content::Video {
            asset: asset("clip", 1920, 1080, None),
            placement: Rect::FULL,
            crop: Vec::new(),
            retime: vec![RetimeSegment::SignedHold {
                source_at: SignedTicks(-17),
                duration: ms(100),
            }],
        },
    )];
    validate(&composition).unwrap();
    let v3 = evaluate_ms_v3(&composition, 50).unwrap();
    assert_eq!(v3.schema, FRAME_SCHEMA_V3);
    assert!(
        matches!(&v3.layers[0].payload, PayloadV3::Video { source, .. } if *source == SignedTicks(-17))
    );
    assert_eq!(evaluate_v3(&composition, ms(50)).unwrap(), v3);
    assert!(matches!(
        evaluate_ms(&composition, 50),
        Err(CompositionError::Invalid { layer, .. }) if layer == "pre"
    ));
    // The signed source survives the composition JSON round trip.
    let json = serde_json::to_string(&composition).unwrap();
    assert_eq!(Composition::from_json_str(&json).unwrap(), composition);
    // Past the layer, the v3 frame has no layers.
    assert!(evaluate_ms_v3(&composition, 500).unwrap().layers.is_empty());
}

fn main() {
    constants();
    ticks_and_rationals();
    tracks_and_easing();
    retime_segments();
    opacity_and_fades();
    evaluate_full_composition();
    json_round_trip_and_edit();
    legacy_documents_and_errors();
    signed_source_frames();
    println!("OK rightkit-compose 0.2.0 (registry consumer)");
}
