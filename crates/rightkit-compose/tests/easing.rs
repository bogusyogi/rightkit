//! Cosine and camera spring easings, and the bounded camera bounce.

use rightkit_compose::*;

const NO_OVERSHOOT: f64 = 1e-6;

fn grid() -> Vec<f64> {
    (0..1000).map(|i| f64::from(i) / 999.0).collect()
}

/// Peak of the step response over the grid.
fn peak(easing: Easing) -> f64 {
    grid()
        .into_iter()
        .map(|u| easing.apply(u))
        .fold(f64::MIN, f64::max)
}

#[test]
fn cosine_is_a_half_cosine_ease_with_exact_endpoints() {
    let e = Easing::Cosine;
    assert_eq!(e.apply(0.0), 0.0);
    assert!((e.apply(0.5) - 0.5).abs() < 1e-12);
    assert!((e.apply(1.0) - 1.0).abs() < 1e-12);
    let quarter = 0.5 - 0.5 * std::f64::consts::FRAC_PI_4.cos();
    assert!((e.apply(0.25) - quarter).abs() < 1e-12);
    // Symmetric about the midpoint and monotone.
    for u in grid() {
        assert!(
            (e.apply(u) + e.apply(1.0 - u) - 1.0).abs() < 1e-12,
            "symmetry at {u}"
        );
    }
    let mut previous = 0.0;
    for u in grid() {
        let v = e.apply(u);
        assert!(v >= previous - 1e-12, "not monotone at {u}");
        previous = v;
    }
}

#[test]
fn cosine_is_span_independent() {
    let track = vec![
        Keyframe {
            at: Ticks::ZERO,
            value: 0.0_f64,
            easing: Easing::Cosine,
        },
        Keyframe {
            at: Ticks::from_ms(2000),
            value: 1.0,
            easing: Easing::Linear,
        },
    ];
    let mid = sample(&track, Ticks::from_ms(500)).unwrap();
    assert!((mid - Easing::Cosine.apply(0.25)).abs() < 1e-12);
}

#[test]
fn camera_spring_critical_has_exact_endpoints_and_never_overshoots() {
    let e = Easing::CameraSpring { bounce: 0.0 };
    assert_eq!(e.apply(0.0), 0.0);
    assert_eq!(e.apply(1.0), 1.0);
    let mut previous = 0.0_f64;
    for u in grid() {
        let v = e.apply(u);
        assert!(v <= 1.0 + NO_OVERSHOOT, "critical overshoot {v} at {u}");
        assert!(v >= previous - 1e-12, "critical not monotone at {u}");
        previous = v;
    }
}

#[test]
fn camera_spring_critical_matches_the_closed_form() {
    let e = Easing::CameraSpring { bounce: 0.0 };
    for u in [0.1, 0.37, 0.5, 0.9] {
        let expected = 1.0 - (1.0 + 9.23 * u) * (-9.23_f64 * u).exp();
        assert!((e.apply(u) - expected).abs() < 1e-12, "u={u}");
    }
}

#[test]
fn camera_spring_underdamped_endpoints_hold_and_overshoot_is_bounded() {
    for bounce in [0.1, 0.2, 0.3] {
        let e = Easing::CameraSpring { bounce };
        assert_eq!(e.apply(0.0), 0.0, "bounce {bounce} start");
        // The endpoint is pinned to exactly 1, so y(1) is within any tolerance of 1.
        assert_eq!(e.apply(1.0), 1.0, "bounce {bounce} end");

        // The step response of zeta < 1 peaks at 1 + exp(-pi zeta / sqrt(1 - zeta^2)).
        let zeta = 1.0 - bounce;
        let envelope = (-std::f64::consts::PI * zeta / (1.0 - zeta * zeta).sqrt()).exp();
        let observed = peak(e);
        assert!(
            observed > 1.0 + 1e-3,
            "bounce {bounce} should overshoot, peak {observed}"
        );
        assert!(
            observed <= 1.0 + envelope + NO_OVERSHOOT,
            "bounce {bounce} peak {observed} exceeds envelope {}",
            1.0 + envelope
        );
    }
}

#[test]
fn camera_spring_bounce_is_clamped_to_zero_through_point_three() {
    let below = Easing::CameraSpring { bounce: -0.5 };
    let zero = Easing::CameraSpring { bounce: 0.0 };
    let above = Easing::CameraSpring { bounce: 0.9 };
    let max = Easing::CameraSpring { bounce: 0.3 };
    for u in grid() {
        assert_eq!(below.apply(u), zero.apply(u), "below clamps to 0 at {u}");
        assert_eq!(above.apply(u), max.apply(u), "above clamps to 0.3 at {u}");
    }
}

#[test]
fn camera_spring_is_span_independent() {
    let e = Easing::CameraSpring { bounce: 0.15 };
    for span in [1000_u64, 2000, 500] {
        let track = vec![
            Keyframe {
                at: Ticks::ZERO,
                value: 0.0_f64,
                easing: e,
            },
            Keyframe {
                at: Ticks::from_ms(span),
                value: 1.0,
                easing: Easing::Linear,
            },
        ];
        let at_quarter = sample(&track, Ticks::from_ms(span / 4)).unwrap();
        assert!((at_quarter - e.apply(0.25)).abs() < 1e-12, "span {span}");
    }
}

#[test]
fn camera_spring_bounce_must_be_finite_to_validate() {
    let canvas = Canvas {
        width: 640,
        height: 360,
    };
    let mut c = Composition::new(canvas, Rational::new(30, 1), Ticks::from_ms(1000));
    c.layers = vec![Layer {
        id: "bg".into(),
        timing: Timing {
            start: Ticks::ZERO,
            end: Ticks::from_ms(1000),
        },
        transform: Vec::new(),
        opacity: vec![
            Keyframe {
                at: Ticks::ZERO,
                value: 0.0,
                easing: Easing::CameraSpring { bounce: f64::NAN },
            },
            Keyframe {
                at: Ticks::from_ms(1000),
                value: 1.0,
                easing: Easing::Linear,
            },
        ],
        clip: None,
        shadow: None,
        motion_blur: None,
        content: Content::Background {
            pattern: Pattern::Solid {
                color: Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
        },
    }];
    assert!(evaluate_ms(&c, 0).is_err());
}

#[test]
fn easing_serialises_with_kind_tags() {
    assert_eq!(
        serde_json::to_string(&Easing::Cosine).unwrap(),
        r#"{"kind":"cosine"}"#
    );
    assert_eq!(
        serde_json::to_string(&Easing::CameraSpring { bounce: 0.15 }).unwrap(),
        r#"{"kind":"camera_spring","bounce":0.15}"#
    );
    let back: Easing = serde_json::from_str(r#"{"kind":"camera_spring","bounce":0.15}"#).unwrap();
    assert_eq!(back, Easing::CameraSpring { bounce: 0.15 });
}
