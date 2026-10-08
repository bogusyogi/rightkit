//! Spring easing and click-effect conformance and behaviour (RK9).
//!
//! `SCREENRIGHT_VECTORS` are the 101 samples of ScreenRight's
//! `cameraConformanceVectors()` (archive/screenright-2026-10-08, src/postprocess.mjs),
//! at u = 0.00, 0.01, ..., 1.00. Columns are `criticalSpring` (bounce 0) and
//! `panSpring` (bounce 0.15), each rounded to six places by the source.
//! ScreenRight's `springProgress` is the step response of a unit mass spring:
//! critical uses omega = 9.23; bounce b uses zeta = 1 - b, omega = 6.9 / zeta.
//! The matching `Easing::Spring` parameters are therefore
//! `stiffness = omega^2`, `damping = 2 * zeta * omega`, `mass = 1`, sampled over a
//! 1000 ms segment so that `t` in seconds equals `u`.

use rightkit_compose::track::{sample, Easing, Keyframe};
use rightkit_compose::*;

const EPS: f64 = 1e-6;

const SCREENRIGHT_VECTORS: [(f64, f64, f64); 101] = [
    (0.00, 0.000000, 0.000000),
    (0.01, 0.004006, 0.003147),
    (0.02, 0.015080, 0.012020),
    (0.03, 0.031943, 0.025828),
    (0.04, 0.053490, 0.043848),
    (0.05, 0.078762, 0.065428),
    (0.06, 0.106935, 0.089975),
    (0.07, 0.137302, 0.116957),
    (0.08, 0.169257, 0.145892),
    (0.09, 0.202283, 0.176352),
    (0.10, 0.235943, 0.207953),
    (0.11, 0.269869, 0.240353),
    (0.12, 0.303753, 0.273250),
    (0.13, 0.337337, 0.306377),
    (0.14, 0.370411, 0.339500),
    (0.15, 0.402804, 0.372418),
    (0.16, 0.434380, 0.404953),
    (0.17, 0.465030, 0.436955),
    (0.18, 0.494672, 0.468295),
    (0.19, 0.523246, 0.498867),
    (0.20, 0.550709, 0.528581),
    (0.21, 0.577036, 0.557364),
    (0.22, 0.602213, 0.585161),
    (0.23, 0.626239, 0.611926),
    (0.24, 0.649120, 0.637629),
    (0.25, 0.670872, 0.662247),
    (0.26, 0.691515, 0.685771),
    (0.27, 0.711077, 0.708195),
    (0.28, 0.729588, 0.729523),
    (0.29, 0.747081, 0.749767),
    (0.30, 0.763591, 0.768940),
    (0.31, 0.779155, 0.787063),
    (0.32, 0.793813, 0.804160),
    (0.33, 0.807603, 0.820258),
    (0.34, 0.820564, 0.835387),
    (0.35, 0.832736, 0.849578),
    (0.36, 0.844156, 0.862866),
    (0.37, 0.854862, 0.875284),
    (0.38, 0.864892, 0.886868),
    (0.39, 0.874281, 0.897656),
    (0.40, 0.883065, 0.907682),
    (0.41, 0.891278, 0.916984),
    (0.42, 0.898951, 0.925598),
    (0.43, 0.906117, 0.933559),
    (0.44, 0.912804, 0.940902),
    (0.45, 0.919042, 0.947663),
    (0.46, 0.924858, 0.953874),
    (0.47, 0.930277, 0.959568),
    (0.48, 0.935325, 0.964777),
    (0.49, 0.940025, 0.969531),
    (0.50, 0.944399, 0.973860),
    (0.51, 0.948468, 0.977792),
    (0.52, 0.952252, 0.981353),
    (0.53, 0.955769, 0.984570),
    (0.54, 0.959037, 0.987468),
    (0.55, 0.962072, 0.990068),
    (0.56, 0.964891, 0.992395),
    (0.57, 0.967507, 0.994469),
    (0.58, 0.969935, 0.996309),
    (0.59, 0.972188, 0.997935),
    (0.60, 0.974277, 0.999364),
    (0.61, 0.976214, 1.000613),
    (0.62, 0.978009, 1.001697),
    (0.63, 0.979673, 1.002631),
    (0.64, 0.981214, 1.003428),
    (0.65, 0.982641, 1.004101),
    (0.66, 0.983963, 1.004663),
    (0.67, 0.985187, 1.005123),
    (0.68, 0.986319, 1.005492),
    (0.69, 0.987367, 1.005780),
    (0.70, 0.988337, 1.005996),
    (0.71, 0.989233, 1.006146),
    (0.72, 0.990063, 1.006240),
    (0.73, 0.990830, 1.006283),
    (0.74, 0.991538, 1.006282),
    (0.75, 0.992193, 1.006242),
    (0.76, 0.992799, 1.006169),
    (0.77, 0.993358, 1.006068),
    (0.78, 0.993875, 1.005942),
    (0.79, 0.994352, 1.005796),
    (0.80, 0.994793, 1.005633),
    (0.81, 0.995199, 1.005456),
    (0.82, 0.995575, 1.005269),
    (0.83, 0.995922, 1.005073),
    (0.84, 0.996242, 1.004872),
    (0.85, 0.996537, 1.004666),
    (0.86, 0.996809, 1.004458),
    (0.87, 0.997061, 1.004250),
    (0.88, 0.997292, 1.004042),
    (0.89, 0.997506, 1.003836),
    (0.90, 0.997703, 1.003634),
    (0.91, 0.997885, 1.003434),
    (0.92, 0.998052, 1.003240),
    (0.93, 0.998207, 1.003051),
    (0.94, 0.998349, 1.002867),
    (0.95, 0.998480, 1.002689),
    (0.96, 0.998601, 1.002518),
    (0.97, 0.998713, 1.002353),
    (0.98, 0.998815, 1.002195),
    (0.99, 0.998910, 1.002044),
    (1.00, 1.000000, 1.000000),
];

/// ScreenRight `criticalSpring`: zeta = 1, omega = 9.23.
fn critical_spring() -> Easing {
    Easing::Spring {
        stiffness: 9.23 * 9.23,
        damping: 2.0 * 9.23,
        mass: 1.0,
    }
}

/// ScreenRight `spring` with bounce 0.15: zeta = 0.85, omega = 6.9 / 0.85.
fn pan_spring() -> Easing {
    let zeta = 0.85_f64;
    let omega = 6.9 / zeta;
    Easing::Spring {
        stiffness: omega * omega,
        damping: 2.0 * zeta * omega,
        mass: 1.0,
    }
}

/// Samples a 0 -> 1 segment of `span_ms` with the given easing at `local` ms.
fn segment(easing: Easing, span_ms: u64, local: u64) -> f64 {
    let track = vec![
        Keyframe {
            at_ms: 0,
            value: 0.0,
            easing,
        },
        Keyframe {
            at_ms: span_ms,
            value: 1.0,
            easing: Easing::Linear,
        },
    ];
    sample(&track, local).expect("non-empty track")
}

#[test]
fn spring_matches_screenright_critical_spring_vectors() {
    for (index, (u, critical, _)) in SCREENRIGHT_VECTORS.iter().enumerate() {
        let local = index as u64 * 10;
        let got = segment(critical_spring(), 1000, local);
        assert!(
            (got - critical).abs() < EPS,
            "criticalSpring at u={u}: got {got}, ScreenRight {critical}"
        );
    }
}

#[test]
fn spring_matches_screenright_pan_spring_vectors() {
    for (index, (u, _, pan)) in SCREENRIGHT_VECTORS.iter().enumerate() {
        let local = index as u64 * 10;
        let got = segment(pan_spring(), 1000, local);
        assert!(
            (got - pan).abs() < EPS,
            "panSpring (bounce 0.15) at u={u}: got {got}, ScreenRight {pan}"
        );
    }
}

#[test]
fn spring_sampling_is_deterministic() {
    for local in (0..=1000).step_by(7) {
        let a = segment(pan_spring(), 1000, local);
        let b = segment(pan_spring(), 1000, local);
        assert_eq!(a.to_bits(), b.to_bits());
    }
}

#[test]
fn critically_damped_spring_never_overshoots_and_is_monotonic() {
    // zeta = 1 exactly: damping = 2 * sqrt(stiffness * mass).
    let easing = Easing::Spring {
        stiffness: 100.0,
        damping: 20.0,
        mass: 1.0,
    };
    let mut previous = 0.0_f64;
    for local in 0..=1000 {
        let v = segment(easing, 1000, local);
        assert!(v <= 1.0, "overshoot {v} at {local} ms");
        assert!(v >= previous - 1e-12, "not monotonic at {local} ms");
        previous = v;
    }
}

#[test]
fn overdamped_spring_is_monotonic_and_slower_than_critical() {
    let over = Easing::Spring {
        stiffness: 100.0,
        damping: 40.0,
        mass: 1.0,
    };
    let critical = critical_spring();
    let mut previous = 0.0_f64;
    for local in 0..=1000 {
        let v = segment(over, 1000, local);
        assert!(
            v <= 1.0 && v >= previous - 1e-12,
            "overdamped not monotonic at {local}"
        );
        previous = v;
    }
    assert!(segment(over, 1000, 300) < segment(critical, 1000, 300));
}

#[test]
fn underdamped_spring_overshoots_as_configured() {
    let easing = Easing::Spring {
        stiffness: 100.0,
        damping: 4.0,
        mass: 1.0,
    };
    let peak = (0..=1000)
        .map(|local| segment(easing, 1000, local))
        .fold(0.0_f64, f64::max);
    assert!(peak > 1.1, "expected overshoot, peak was {peak}");
}

#[test]
fn spring_runs_in_real_time_from_the_segment_start() {
    // t = u * span / 1000 equals local_ms / 1000, so a spring reads the same
    // value at a given local time whatever the segment's span.
    let easing = pan_spring();
    for local in [100_u64, 250, 500, 800] {
        let short = segment(easing, 1000, local);
        let long = segment(easing, 2000, local);
        assert!(
            (short - long).abs() < EPS,
            "real-time mismatch at {local} ms"
        );
    }
}

fn canvas() -> Canvas {
    Canvas {
        width: 1920,
        height: 1080,
        frame_rate_num: 30,
        frame_rate_den: 1,
    }
}

fn colour(a: f64) -> Rgba {
    Rgba {
        r: 0.9,
        g: 0.4,
        b: 0.1,
        a,
    }
}

fn click_style() -> Style {
    Style {
        foreground: colour(1.0),
        background: colour(0.25),
    }
}

fn click_layer(effect: ClickShape, onset_ms: u64, duration_ms: u64, easing: Easing) -> Layer {
    Layer {
        id: "click".into(),
        timing: Timing {
            start_ms: 0,
            end_ms: 2000,
        },
        transform: Vec::new(),
        opacity: Vec::new(),
        clip: None,
        shadow: None,
        motion_blur: None,
        content: Content::ClickEffect {
            anchor: Point { x: 0.5, y: 0.5 },
            effect,
            style: click_style(),
            onset_ms,
            duration_ms,
            easing,
        },
    }
}

fn composition_with(layer: Layer) -> Composition {
    Composition {
        schema: SCHEMA.to_string(),
        canvas: canvas(),
        duration_ms: 3000,
        layers: vec![layer],
    }
}

fn ripple_at(composition: &Composition, t_ms: u64) -> (f64, f64) {
    let frame = evaluate(composition, t_ms).expect("evaluates");
    match frame.layers.first().map(|l| &l.payload) {
        Some(Payload::Ripple {
            radius_px, alpha, ..
        }) => (*radius_px, *alpha),
        other => panic!("expected a ripple at {t_ms} ms, got {other:?}"),
    }
}

#[test]
fn ripple_is_timed_by_onset_and_duration() {
    let ripple = ClickShape::Ripple {
        max_radius: 0.1,
        stroke_width: 0.01,
    };
    let composition = composition_with(click_layer(ripple, 500, 1000, Easing::Linear));

    // Before onset the ring is absent: zero radius and alpha.
    assert_eq!(ripple_at(&composition, 400), (0.0, 0.0));
    // At onset it starts at zero radius at full alpha.
    assert_eq!(ripple_at(&composition, 500), (0.0, 1.0));
    // Halfway: linear easing, radius = 0.1 * 1080 * 0.5, alpha 0.5.
    let (radius, alpha) = ripple_at(&composition, 1000);
    assert!((radius - 54.0).abs() < EPS, "radius {radius}");
    assert!((alpha - 0.5).abs() < EPS, "alpha {alpha}");
    // At and after completion the ring is gone again.
    assert_eq!(ripple_at(&composition, 1500), (0.0, 0.0));
    assert_eq!(ripple_at(&composition, 1900), (0.0, 0.0));
}

#[test]
fn ripple_radius_follows_spring_without_overshoot_when_critical() {
    let max_radius = 0.1;
    let composition = composition_with(click_layer(
        ClickShape::Ripple {
            max_radius,
            stroke_width: 0.01,
        },
        0,
        1000,
        Easing::Spring {
            stiffness: 100.0,
            damping: 20.0,
            mass: 1.0,
        },
    ));
    let limit = max_radius * 1080.0 + EPS;
    for t in (0..1000).step_by(20) {
        let (radius, _) = ripple_at(&composition, t);
        assert!(radius <= limit, "radius {radius} exceeds {limit} at {t} ms");
    }
}

#[test]
fn highlight_grows_and_fades_within_its_animation() {
    let highlight = ClickShape::Highlight {
        half_width: 0.1,
        half_height: 0.05,
        grow: 0.5,
        corner_radius: 0.01,
    };
    let composition = composition_with(click_layer(highlight, 200, 800, Easing::Linear));

    let rect_at = |t_ms: u64| {
        let frame = evaluate(&composition, t_ms).expect("evaluates");
        match &frame.layers[0].payload {
            Payload::Highlight { rect_px, alpha, .. } => (*rect_px, *alpha),
            other => panic!("expected highlight, got {other:?}"),
        }
    };

    let (start, start_alpha) = rect_at(200);
    let (mid, mid_alpha) = rect_at(600);
    // Width at onset is 2 * 0.1 * 1920; the box grows by (1 + 0.5 * eased).
    assert!((start.width - 2.0 * 0.1 * 1920.0).abs() < EPS);
    assert!(mid.width > start.width);
    assert!((start_alpha - 1.0).abs() < EPS);
    assert!((mid_alpha - 0.5).abs() < EPS);
    // Centred on the anchor throughout.
    assert!((mid.x + mid.width / 2.0 - 960.0).abs() < EPS);
    assert!((mid.y + mid.height / 2.0 - 540.0).abs() < EPS);
    // Gone once the animation has completed.
    assert_eq!(rect_at(1000).1, 0.0);
}

#[test]
fn click_effects_are_pure_across_evaluation_order() {
    let composition = composition_with(click_layer(
        ClickShape::Ripple {
            max_radius: 0.08,
            stroke_width: 0.004,
        },
        300,
        900,
        Easing::EaseInOut,
    ));
    let times: Vec<u64> = (0..1500).step_by(50).collect();
    let forward: Vec<_> = times
        .iter()
        .map(|&t| evaluate(&composition, t).unwrap())
        .collect();
    let mut reverse: Vec<_> = times
        .iter()
        .rev()
        .map(|&t| evaluate(&composition, t).unwrap())
        .collect();
    reverse.reverse();
    assert_eq!(forward, reverse);
}

#[test]
fn click_effect_json_round_trips_and_defaults_to_ease_out() {
    let composition = composition_with(click_layer(
        ClickShape::Highlight {
            half_width: 0.1,
            half_height: 0.1,
            grow: 0.2,
            corner_radius: 0.0,
        },
        0,
        500,
        Easing::Spring {
            stiffness: 50.0,
            damping: 6.0,
            mass: 2.0,
        },
    ));
    let json = serde_json::to_string(&composition).expect("serialises");
    let back: Composition = serde_json::from_str(&json).expect("deserialises");
    assert_eq!(back, composition);

    let without_easing = r#"{"schema":"rightkit.composition.v1","canvas":{"width":1920,"height":1080,"frame_rate_num":30,"frame_rate_den":1},"duration_ms":3000,"layers":[{"id":"c","timing":{"start_ms":0,"end_ms":1000},"content":{"kind":"click_effect","anchor":{"x":0.5,"y":0.5},"effect":{"shape":"ripple","max_radius":0.05,"stroke_width":0.005},"style":{"foreground":{"r":1,"g":1,"b":1,"a":1},"background":{"r":0,"g":0,"b":0,"a":0}},"onset_ms":0,"duration_ms":500}}]}"#;
    let parsed: Composition = serde_json::from_str(without_easing).expect("deserialises");
    match &parsed.layers[0].content {
        Content::ClickEffect { easing, .. } => assert_eq!(*easing, Easing::EaseOut),
        other => panic!("unexpected content {other:?}"),
    }
}

#[test]
fn rejects_invalid_spring_and_click_parameters() {
    let good = ClickShape::Ripple {
        max_radius: 0.1,
        stroke_width: 0.01,
    };
    let bad_spring = Easing::Spring {
        stiffness: 0.0,
        damping: 1.0,
        mass: 1.0,
    };
    let rejected = [
        click_layer(good, 0, 0, Easing::Linear),
        click_layer(good, 0, 100, bad_spring),
        click_layer(
            ClickShape::Ripple {
                max_radius: -0.1,
                stroke_width: 0.01,
            },
            0,
            100,
            Easing::Linear,
        ),
        click_layer(
            ClickShape::Highlight {
                half_width: 0.1,
                half_height: 0.1,
                grow: -1.0,
                corner_radius: 0.0,
            },
            0,
            100,
            Easing::Linear,
        ),
    ];
    for layer in rejected {
        assert!(evaluate(&composition_with(layer), 0).is_err());
    }

    // A spring on an opacity track is validated too.
    let mut layer = click_layer(good, 0, 100, Easing::Linear);
    layer.content = Content::Background {
        pattern: Pattern::Solid { color: colour(1.0) },
    };
    layer.opacity = vec![
        Keyframe {
            at_ms: 0,
            value: 0.0,
            easing: Easing::Spring {
                stiffness: 10.0,
                damping: -1.0,
                mass: 1.0,
            },
        },
        Keyframe {
            at_ms: 500,
            value: 1.0,
            easing: Easing::Linear,
        },
    ];
    assert!(evaluate(&composition_with(layer), 0).is_err());
}
