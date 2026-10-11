//! Reading `rightkit.composition.v1` documents: the millisecond fields convert
//! exactly and the video speed becomes a retime segment.

use rightkit_compose::*;

const V1_SPEED_2: &str = r#"{
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
    },
    {
      "id": "click",
      "timing": {"start_ms": 0, "end_ms": 3000},
      "content": {
        "kind": "click_effect",
        "anchor": {"x": 0.5, "y": 0.5},
        "effect": {"shape": "ripple", "max_radius": 0.05, "stroke_width": 0.005},
        "style": {"foreground": {"r": 1, "g": 1, "b": 1, "a": 1}, "background": {"r": 0, "g": 0, "b": 0, "a": 0}},
        "onset_ms": 500,
        "duration_ms": 1000
      }
    }
  ]
}"#;

#[test]
fn legacy_document_converts_timing_fields_exactly() {
    let c = Composition::from_json_str(V1_SPEED_2).expect("v1 reads");
    assert_eq!(c.schema, SCHEMA);
    assert_eq!(c.fps, Rational::new(30000, 1001));
    assert_eq!(c.duration, Ticks::from_ms(3000));
    let screen = &c.layers[0];
    assert_eq!(screen.timing.start, Ticks::from_ms(1000));
    assert_eq!(screen.timing.end, Ticks::from_ms(3000));
    // The transform keyframe at 1000 ms becomes exactly 705,600,000 ticks.
    assert_eq!(screen.transform[1].at, Ticks(705_600_000));
    match &screen.content {
        Content::Video { asset, retime, .. } => {
            assert_eq!(asset.duration, Some(Ticks::from_ms(8000)));
            // source(L) = 500 + 2 * L over a 2000 ms layer: Play 500..4500 ms at 2x.
            assert_eq!(
                retime,
                &vec![RetimeSegment::Play {
                    source_in: Ticks::from_ms(500),
                    source_out: Ticks::from_ms(4500),
                    speed: 2.0,
                }]
            );
        }
        other => panic!("video expected, got {other:?}"),
    }
    match &c.layers[1].content {
        Content::ClickEffect { onset, duration, .. } => {
            assert_eq!(*onset, Ticks::from_ms(500));
            assert_eq!(*duration, Ticks::from_ms(1000));
        }
        other => panic!("click effect expected, got {other:?}"),
    }
}

#[test]
fn legacy_speed_keeps_its_frames() {
    let c = Composition::from_json_str(V1_SPEED_2).expect("v1 reads");
    // At local 500 ms the old model read 500 + 2 * 500 = 1500 ms of source.
    let frame = evaluate_ms(&c, 1500).expect("evaluates");
    let source = frame
        .layers
        .iter()
        .find_map(|l| match &l.payload {
            Payload::Video { source, .. } if l.id == "screen" => Some(*source),
            _ => None,
        })
        .expect("screen active");
    assert_eq!(source, Ticks::from_ms(1500));
}

#[test]
fn legacy_negative_speed_becomes_a_reverse_segment() {
    let doc = V1_SPEED_2
        .replace("\"source_start_ms\": 500", "\"source_start_ms\": 2000")
        .replace("\"speed\": 2.0", "\"speed\": -1.0")
        .replace(
            "\"timing\": {\"start_ms\": 1000, \"end_ms\": 3000}",
            "\"timing\": {\"start_ms\": 1000, \"end_ms\": 2000}",
        );
    let c = Composition::from_json_str(&doc).expect("v1 reads");
    match &c.layers[0].content {
        Content::Video { retime, .. } => assert_eq!(
            retime,
            &vec![RetimeSegment::Reverse {
                source_in: Ticks::from_ms(1000),
                source_out: Ticks::from_ms(2000),
                speed: 1.0,
            }]
        ),
        other => panic!("video expected, got {other:?}"),
    }
    // Local 500 ms reads 2000 - 500 = 1500 ms of source.
    let frame = evaluate_ms(&c, 1500).expect("evaluates");
    let source = frame
        .layers
        .iter()
        .find_map(|l| match &l.payload {
            Payload::Video { source, .. } if l.id == "screen" => Some(*source),
            _ => None,
        })
        .expect("screen active");
    assert_eq!(source, Ticks::from_ms(1500));
}

#[test]
fn legacy_reverse_before_source_zero_is_refused() {
    let doc = V1_SPEED_2
        .replace("\"source_start_ms\": 500", "\"source_start_ms\": 100")
        .replace("\"speed\": 2.0", "\"speed\": -1.0");
    assert!(matches!(
        Composition::from_json_str(&doc),
        Err(CompositionError::Legacy(_))
    ));
}

#[test]
fn version_two_documents_round_trip_through_from_json_str() {
    let mut c = Composition::new(
        Canvas {
            width: 640,
            height: 360,
        },
        Rational::new(30000, 1001),
        Ticks::from_ms(1000),
    );
    c.layers = vec![Layer {
        id: "hold".into(),
        timing: Timing {
            start: Ticks::ZERO,
            end: Ticks::from_ms(1000),
        },
        transform: Vec::new(),
        opacity: Vec::new(),
        clip: None,
        shadow: None,
        motion_blur: None,
        content: Content::Background {
            pattern: Pattern::Solid {
                color: Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
            },
        },
    }];
    let json = serde_json::to_string(&c).unwrap();
    assert!(json.contains("\"duration_ticks\":705600000"), "{json}");
    assert_eq!(Composition::from_json_str(&json).unwrap(), c);
}

#[test]
fn malformed_json_is_reported_not_panicked() {
    assert!(matches!(
        Composition::from_json_str("{not json"),
        Err(CompositionError::Json(_))
    ));
}
