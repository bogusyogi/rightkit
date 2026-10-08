use rightkit_compose::*;

const EPS: f64 = 1e-6;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < EPS
}

fn canvas() -> Canvas {
    Canvas {
        width: 1920,
        height: 1080,
        frame_rate_num: 30,
        frame_rate_den: 1,
    }
}

fn asset(id: &str, width: u32, height: u32, duration_ms: Option<u64>) -> AssetRef {
    AssetRef {
        id: id.into(),
        blake3: format!("blake3-{id}"),
        width,
        height,
        duration_ms,
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
        timing: Timing { start_ms, end_ms },
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
            source_start_ms: 0,
            speed: 1.0,
        },
    )
}

fn composition(layers: Vec<Layer>) -> Composition {
    let mut c = Composition::new(canvas(), 10_000);
    c.layers = layers;
    c
}

fn kf<T>(at_ms: u64, value: T) -> Keyframe<T> {
    Keyframe {
        at_ms,
        value,
        easing: Easing::Linear,
    }
}

fn cursor_layer(follows: &str, samples: Vec<CursorSample>) -> Layer {
    layer(
        "cursor",
        0,
        10_000,
        Content::Cursor {
            follows: follows.into(),
            scale: 1.0,
            sprite: CursorSprite {
                asset: asset("arrow", 32, 32, None),
                hotspot: Point { x: 4.0, y: 4.0 },
            },
            samples,
        },
    )
}

fn sample_at(at_ms: u64, x: f64, y: f64, visible: bool) -> CursorSample {
    CursorSample {
        at_ms,
        x,
        y,
        visible,
    }
}

fn payload(frame: &FrameDescription, id: &str) -> Payload {
    frame
        .layers
        .iter()
        .find(|l| l.id == id)
        .unwrap_or_else(|| panic!("layer {id} not active"))
        .payload
        .clone()
}

// --- keyframe sampling and easing -----------------------------------------

#[test]
fn linear_track_interpolates_between_keyframes_and_holds_at_the_ends() {
    let track = vec![kf(0, 0.0), kf(1000, 1.0)];
    assert!(close(sample(&track, 250).unwrap(), 0.25));
    assert!(close(sample(&track, 0).unwrap(), 0.0));
    assert!(close(sample(&track, 1000).unwrap(), 1.0));
    assert!(close(sample(&track, 5000).unwrap(), 1.0));
}

#[test]
fn track_holds_first_value_before_its_first_keyframe() {
    let track = vec![kf(100, 0.5), kf(200, 0.8)];
    assert!(close(sample(&track, 0).unwrap(), 0.5));
    assert!(close(sample(&track, 300).unwrap(), 0.8));
    let empty: Track<f64> = Vec::new();
    assert!(sample(&empty, 10).is_none());
}

#[test]
fn easing_shapes_each_segment_by_its_starting_keyframe() {
    let ease_in = vec![
        Keyframe {
            at_ms: 0,
            value: 0.0,
            easing: Easing::EaseIn,
        },
        kf(1000, 1.0),
    ];
    assert!(close(sample(&ease_in, 500).unwrap(), 0.125));

    let ease_in_out = vec![
        Keyframe {
            at_ms: 0,
            value: 0.0,
            easing: Easing::EaseInOut,
        },
        kf(1000, 1.0),
    ];
    assert!(close(sample(&ease_in_out, 500).unwrap(), 0.5));
    assert!(sample(&ease_in_out, 250).unwrap() < 0.25);

    let bezier_linear = vec![
        Keyframe {
            at_ms: 0,
            value: 0.0,
            easing: Easing::CubicBezier {
                x1: 0.0,
                y1: 0.0,
                x2: 1.0,
                y2: 1.0,
            },
        },
        kf(1000, 1.0),
    ];
    assert!((sample(&bezier_linear, 250).unwrap() - 0.25).abs() < 1e-6);
}

#[test]
fn hold_easing_keeps_its_value_until_the_next_keyframe() {
    let track = vec![
        Keyframe {
            at_ms: 0,
            value: 0.0,
            easing: Easing::Hold,
        },
        kf(1000, 1.0),
    ];
    assert!(close(sample(&track, 999).unwrap(), 0.0));
    assert!(close(sample(&track, 1000).unwrap(), 1.0));
}

// --- evaluation at keyframes and between them -----------------------------

#[test]
fn transform_keyframes_scale_about_the_placement_centre() {
    let mut v = video("screen", asset("screen-src", 1920, 1080, None), 0, 10_000);
    v.transform = vec![
        Keyframe {
            at_ms: 0,
            value: Transform {
                dx: 0.0,
                dy: 0.0,
                scale: 1.0,
            },
            easing: Easing::Linear,
        },
        Keyframe {
            at_ms: 1000,
            value: Transform {
                dx: 0.0,
                dy: 0.0,
                scale: 2.0,
            },
            easing: Easing::Linear,
        },
    ];
    let c = composition(vec![v]);

    let at_start = evaluate(&c, 0).unwrap();
    let Payload::Video { dest_px, .. } = payload(&at_start, "screen") else {
        panic!("video payload")
    };
    assert!(close(dest_px.x, 0.0) && close(dest_px.width, 1920.0) && close(dest_px.height, 1080.0));

    let midway = evaluate(&c, 500).unwrap();
    let Payload::Video { dest_px, .. } = payload(&midway, "screen") else {
        panic!("video payload")
    };
    assert!(close(dest_px.width, 2880.0), "width {}", dest_px.width);
    assert!(close(dest_px.x, -480.0), "x {}", dest_px.x);

    let at_end = evaluate(&c, 1000).unwrap();
    let Payload::Video { dest_px, .. } = payload(&at_end, "screen") else {
        panic!("video payload")
    };
    assert!(close(dest_px.width, 3840.0));
    assert!(close(dest_px.x, -960.0));
}

#[test]
fn opacity_track_is_sampled_at_layer_local_time() {
    let mut v = video("screen", asset("screen-src", 1920, 1080, None), 1000, 5000);
    v.opacity = vec![kf(0, 0.0), kf(2000, 1.0)];
    let c = composition(vec![v]);
    let frame = evaluate(&c, 2000).unwrap();
    assert!(close(frame.layers[0].opacity, 0.5));
}

#[test]
fn video_source_time_follows_speed_and_clamps_to_media_duration() {
    let mut v = video(
        "screen",
        asset("screen-src", 1920, 1080, Some(2000)),
        1000,
        5000,
    );
    if let Content::Video {
        source_start_ms,
        speed,
        ..
    } = &mut v.content
    {
        *source_start_ms = 500;
        *speed = 2.0;
    }
    let c = composition(vec![v]);

    let early = evaluate(&c, 1500).unwrap();
    let Payload::Video { source_ms, .. } = payload(&early, "screen") else {
        panic!()
    };
    assert!(close(source_ms, 1500.0));

    let late = evaluate(&c, 4000).unwrap();
    let Payload::Video { source_ms, .. } = payload(&late, "screen") else {
        panic!()
    };
    assert!(
        close(source_ms, 2000.0),
        "clamped to duration, got {source_ms}"
    );
}

#[test]
fn crop_keyframes_pan_the_visible_region_of_a_still() {
    let still = layer(
        "photo",
        0,
        10_000,
        Content::Still {
            asset: asset("photo-src", 4000, 3000, None),
            placement: Rect::FULL,
            crop: vec![
                kf(
                    0,
                    Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 1.0,
                        height: 1.0,
                    },
                ),
                kf(
                    1000,
                    Rect {
                        x: 0.2,
                        y: 0.2,
                        width: 0.5,
                        height: 0.5,
                    },
                ),
            ],
        },
    );
    let c = composition(vec![still]);
    let frame = evaluate(&c, 1000).unwrap();
    let Payload::Still { crop, .. } = payload(&frame, "photo") else {
        panic!()
    };
    assert!(close(crop.x, 0.2) && close(crop.width, 0.5));
}

// --- clipping, shadows, blur ----------------------------------------------

#[test]
fn rounded_clip_radius_scales_with_the_shorter_canvas_side() {
    let mut v = video("screen", asset("screen-src", 1920, 1080, None), 0, 10_000);
    v.clip = Some(Clip::RoundedRect { radius: 0.02 });
    v.shadow = Some(Shadow {
        offset_x: 0.0,
        offset_y: 0.01,
        blur: 0.02,
        color: rgba(0.4),
    });
    let c = composition(vec![v]);
    let frame = evaluate(&c, 0).unwrap();
    let f = &frame.layers[0];
    let Some(ClipPx::RoundedRect { radius_px }) = f.clip else {
        panic!("rounded clip")
    };
    assert!(close(radius_px, 21.6), "radius {radius_px}");
    let shadow = f.shadow.expect("shadow");
    assert!(close(shadow.offset_y_px, 10.8) && close(shadow.blur_px, 21.6));
}

#[test]
fn webcam_is_independently_timed_with_its_own_source_offset() {
    let screen = video("screen", asset("screen-src", 1920, 1080, None), 0, 10_000);
    let webcam = layer(
        "webcam",
        3000,
        6000,
        Content::Webcam {
            asset: asset("cam-src", 1280, 720, None),
            placement: Rect {
                x: 0.8,
                y: 0.7,
                width: 0.18,
                height: 0.25,
            },
            source_start_ms: 4000,
        },
    );
    let mut webcam = webcam;
    webcam.clip = Some(Clip::Circle);
    let c = composition(vec![screen, webcam]);

    let before = evaluate(&c, 2000).unwrap();
    assert!(before.layers.iter().all(|l| l.id != "webcam"));

    let during = evaluate(&c, 3500).unwrap();
    let Payload::Webcam { source_ms, .. } = payload(&during, "webcam") else {
        panic!()
    };
    assert!(close(source_ms, 4500.0));
    let cam = during.layers.iter().find(|l| l.id == "webcam").unwrap();
    assert_eq!(cam.clip, Some(ClipPx::Circle));
}

#[test]
fn motion_blur_offsets_are_centred_on_the_frame_time() {
    let mut v = video("screen", asset("screen-src", 1920, 1080, None), 0, 10_000);
    v.motion_blur = Some(MotionBlur {
        shutter_degrees: 180.0,
        samples: 4,
    });
    let c = composition(vec![v]);
    let frame = evaluate(&c, 0).unwrap();
    let offsets = &frame.layers[0]
        .motion_blur
        .as_ref()
        .expect("blur")
        .offsets_ms;
    assert_eq!(offsets.len(), 4);
    assert!(close(offsets[0], -6.25), "first offset {}", offsets[0]);
    assert!(close(offsets[3], 6.25));
    assert!(offsets.iter().sum::<f64>().abs() < EPS);
}

// --- layer order, overlays, cursor ----------------------------------------

#[test]
fn layers_are_emitted_in_stack_order_back_to_front() {
    let background = layer(
        "bg",
        0,
        10_000,
        Content::Background {
            pattern: Pattern::Solid { color: rgba(1.0) },
        },
    );
    let screen = video("screen", asset("screen-src", 1920, 1080, None), 0, 10_000);
    let cursor = cursor_layer("screen", vec![sample_at(0, 0.5, 0.5, true)]);
    let badge = layer(
        "badge",
        0,
        10_000,
        Content::Badge {
            label: "Ctrl-S".into(),
            anchor: Point { x: 0.5, y: 0.5 },
            style: style(),
        },
    );
    let c = composition(vec![background, screen, cursor, badge]);

    let frame = evaluate(&c, 100).unwrap();
    let ids: Vec<&str> = frame.layers.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["bg", "screen", "cursor", "badge"]);
    let indices: Vec<usize> = frame.layers.iter().map(|l| l.stack_index).collect();
    assert_eq!(indices, [0, 1, 2, 3]);
}

#[test]
fn badge_anchor_is_placed_in_canvas_pixels() {
    let badge = layer(
        "badge",
        0,
        10_000,
        Content::Badge {
            label: "Save".into(),
            anchor: Point { x: 0.25, y: 0.5 },
            style: style(),
        },
    );
    let c = composition(vec![badge]);
    let frame = evaluate(&c, 0).unwrap();
    let Payload::Badge { anchor_px, .. } = payload(&frame, "badge") else {
        panic!()
    };
    assert!(close(anchor_px.x, 480.0) && close(anchor_px.y, 540.0));
}

#[test]
fn cursor_tip_and_sprite_follow_a_zoomed_source_layer() {
    let mut screen = video("screen", asset("screen-src", 1920, 1080, None), 0, 10_000);
    // Show the top-left quarter of the source (half width and height) across the canvas.
    if let Content::Video { crop, .. } = &mut screen.content {
        *crop = vec![kf(
            0,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 0.5,
            },
        )];
    }
    let c = composition(vec![
        screen,
        cursor_layer("screen", vec![sample_at(0, 0.25, 0.25, true)]),
    ]);
    let frame = evaluate(&c, 0).unwrap();
    let Payload::Cursor {
        visible,
        tip_px,
        sprite_px,
        ..
    } = payload(&frame, "cursor")
    else {
        panic!()
    };
    assert!(visible);
    // The source point (0.25, 0.25) is the centre of the visible crop, so it lands at canvas centre.
    assert!(
        close(tip_px.x, 960.0) && close(tip_px.y, 540.0),
        "tip {tip_px:?}"
    );
    // Source density: 32 sprite pixels become 64 canvas pixels in a 2x zoom; hotspot is 8 px in.
    assert!(
        close(sprite_px.width, 64.0) && close(sprite_px.height, 64.0),
        "sprite {sprite_px:?}"
    );
    assert!(close(sprite_px.x, 952.0) && close(sprite_px.y, 532.0));
}

#[test]
fn cursor_outside_the_visible_crop_is_hidden() {
    let mut screen = video("screen", asset("screen-src", 1920, 1080, None), 0, 10_000);
    if let Content::Video { crop, .. } = &mut screen.content {
        *crop = vec![kf(
            0,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 0.5,
            },
        )];
    }
    let c = composition(vec![
        screen,
        cursor_layer("screen", vec![sample_at(0, 0.9, 0.9, true)]),
    ]);
    let frame = evaluate(&c, 0).unwrap();
    let Payload::Cursor { visible, .. } = payload(&frame, "cursor") else {
        panic!()
    };
    assert!(!visible);
}

#[test]
fn cursor_interpolates_position_and_holds_visibility_from_the_earlier_sample() {
    let screen = video("screen", asset("screen-src", 1920, 1080, None), 0, 10_000);
    let samples = vec![
        sample_at(0, 0.0, 0.5, true),
        sample_at(1000, 1.0, 0.5, false),
    ];
    let c = composition(vec![screen, cursor_layer("screen", samples)]);

    let mid = evaluate(&c, 500).unwrap();
    let Payload::Cursor {
        visible, tip_px, ..
    } = payload(&mid, "cursor")
    else {
        panic!()
    };
    assert!(visible, "earlier sample visibility holds");
    assert!(close(tip_px.x, 960.0) && close(tip_px.y, 540.0));

    let later = evaluate(&c, 1000).unwrap();
    let Payload::Cursor { visible, .. } = payload(&later, "cursor") else {
        panic!()
    };
    assert!(!visible, "idle hide takes effect at the sample");
}

#[test]
fn cursor_is_hidden_while_its_followed_layer_is_inactive() {
    let screen = video(
        "screen",
        asset("screen-src", 1920, 1080, None),
        2000,
        10_000,
    );
    let c = composition(vec![
        screen,
        cursor_layer("screen", vec![sample_at(0, 0.5, 0.5, true)]),
    ]);
    let before = evaluate(&c, 1000).unwrap();
    assert!(before.layers.iter().all(|l| l.id != "screen"));
    let Payload::Cursor { visible, .. } = payload(&before, "cursor") else {
        panic!()
    };
    assert!(!visible);
}

#[test]
fn caption_rect_is_scaled_to_canvas_pixels() {
    let caption = layer(
        "caption",
        0,
        10_000,
        Content::Caption {
            text: "Open the export menu".into(),
            rect: Rect {
                x: 0.1,
                y: 0.8,
                width: 0.8,
                height: 0.1,
            },
            style: style(),
        },
    );
    let c = composition(vec![caption]);
    let frame = evaluate(&c, 0).unwrap();
    let Payload::Caption { rect_px, .. } = payload(&frame, "caption") else {
        panic!()
    };
    assert!(
        close(rect_px.x, 192.0) && close(rect_px.width, 1536.0) && close(rect_px.height, 108.0)
    );
}

// --- validation ------------------------------------------------------------

#[test]
fn rejects_wrong_schema_duplicate_ids_and_out_of_range_times() {
    let mut wrong = composition(vec![]);
    wrong.schema = "rightkit.composition.v0".into();
    assert!(matches!(
        evaluate(&wrong, 0),
        Err(CompositionError::Schema(_))
    ));

    let dup = composition(vec![
        video("screen", asset("a", 10, 10, None), 0, 10_000),
        video("screen", asset("b", 10, 10, None), 0, 10_000),
    ]);
    assert!(matches!(evaluate(&dup, 0), Err(CompositionError::LayerId(id)) if id == "screen"));

    let ok = composition(vec![video("screen", asset("a", 10, 10, None), 0, 10_000)]);
    assert!(matches!(
        evaluate(&ok, 10_000),
        Err(CompositionError::OutOfRange { .. })
    ));
}

#[test]
fn rejects_transform_on_background_and_cursor_following_a_later_layer() {
    let mut bg = layer(
        "bg",
        0,
        10_000,
        Content::Background {
            pattern: Pattern::Solid { color: rgba(1.0) },
        },
    );
    bg.transform = vec![kf(0, Transform::IDENTITY)];
    assert!(matches!(
        evaluate(&composition(vec![bg]), 0),
        Err(CompositionError::Invalid { .. })
    ));

    let early_cursor = cursor_layer("screen", vec![sample_at(0, 0.5, 0.5, true)]);
    let c = composition(vec![
        early_cursor,
        video("screen", asset("a", 10, 10, None), 0, 10_000),
    ]);
    assert!(
        matches!(evaluate(&c, 0), Err(CompositionError::Invalid { layer, .. }) if layer == "cursor")
    );
}

#[test]
fn rejects_unsorted_keyframes_bad_opacity_and_bezier_points() {
    let mut unsorted = video("screen", asset("a", 10, 10, None), 0, 10_000);
    unsorted.opacity = vec![kf(500, 0.5), kf(100, 1.0)];
    assert!(matches!(
        evaluate(&composition(vec![unsorted]), 0),
        Err(CompositionError::Invalid { .. })
    ));

    let mut bad_opacity = video("screen", asset("a", 10, 10, None), 0, 10_000);
    bad_opacity.opacity = vec![kf(0, 1.5)];
    assert!(matches!(
        evaluate(&composition(vec![bad_opacity]), 0),
        Err(CompositionError::Invalid { .. })
    ));

    let mut bad_bezier = video("screen", asset("a", 10, 10, None), 0, 10_000);
    bad_bezier.opacity = vec![
        Keyframe {
            at_ms: 0,
            value: 1.0,
            easing: Easing::CubicBezier {
                x1: 1.5,
                y1: 0.0,
                x2: 0.5,
                y2: 1.0,
            },
        },
        kf(1000, 0.0),
    ];
    assert!(matches!(
        evaluate(&composition(vec![bad_bezier]), 0),
        Err(CompositionError::Invalid { .. })
    ));
}

#[test]
fn rejects_layers_whose_timing_exceeds_the_composition() {
    let late = video("screen", asset("a", 10, 10, None), 5000, 20_000);
    assert!(matches!(
        evaluate(&composition(vec![late]), 0),
        Err(CompositionError::Invalid { .. })
    ));
}

// --- serialisation and the edit-reopen round trip ---------------------------

fn full_composition() -> Composition {
    let mut screen = video(
        "screen",
        asset("screen-src", 1920, 1080, Some(8000)),
        0,
        8000,
    );
    screen.transform = vec![
        Keyframe {
            at_ms: 0,
            value: Transform::IDENTITY,
            easing: Easing::EaseInOut,
        },
        Keyframe {
            at_ms: 2000,
            value: Transform {
                dx: -0.1,
                dy: 0.0,
                scale: 1.6,
            },
            easing: Easing::Hold,
        },
        Keyframe {
            at_ms: 4000,
            value: Transform {
                dx: 0.0,
                dy: 0.0,
                scale: 1.0,
            },
            easing: Easing::CubicBezier {
                x1: 0.2,
                y1: 0.0,
                x2: 0.0,
                y2: 1.0,
            },
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
            source_start_ms: 500,
        },
    );
    let cursor = cursor_layer(
        "screen",
        vec![
            sample_at(0, 0.2, 0.2, true),
            sample_at(3000, 0.6, 0.4, true),
            sample_at(5000, 0.6, 0.4, false),
        ],
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
    let mut c = Composition::new(canvas(), 10_000);
    c.layers = vec![background, screen, webcam, cursor, badge, caption];
    c
}

#[test]
fn composition_survives_serialisation_with_identical_frames() {
    let original = full_composition();
    validate(&original).expect("fixture is valid");
    let json = serde_json::to_string_pretty(&original).unwrap();
    assert!(json.contains("\"schema\": \"rightkit.composition.v1\""));
    let reopened: Composition = serde_json::from_str(&json).unwrap();
    assert_eq!(reopened, original);

    for t in [0, 1, 999, 2000, 3000, 3499, 3500, 4500, 6000, 7999] {
        assert_eq!(
            evaluate(&original, t).unwrap(),
            evaluate(&reopened, t).unwrap(),
            "frame {t}"
        );
    }
}

#[test]
fn edit_then_reopen_changes_only_the_edited_frames() {
    let original = full_composition();
    let before = evaluate(&original, 2000).unwrap();

    let mut edited = original.clone();
    if let Some(screen) = edited.layers.iter_mut().find(|l| l.id == "screen") {
        screen.transform[1].value = Transform {
            dx: 0.0,
            dy: 0.0,
            scale: 2.0,
        };
    }
    let json = serde_json::to_string(&edited).unwrap();
    let reopened: Composition = serde_json::from_str(&json).unwrap();
    assert_eq!(reopened, edited);

    let after = evaluate(&reopened, 2000).unwrap();
    assert_ne!(before, after, "the edited keyframe changes its frame");
    let Payload::Video { dest_px, .. } = payload(&after, "screen") else {
        panic!()
    };
    assert!(
        close(dest_px.width, 3840.0),
        "edited scale 2.0 applied, got {}",
        dest_px.width
    );

    // Frames after the last keyframe of the edited track are unchanged.
    assert_eq!(
        evaluate(&original, 5000).unwrap(),
        evaluate(&reopened, 5000).unwrap()
    );
}
