//! CPU-side checks: blend-mode codes and names, the frame descriptor wire shape, and that every WGSL
//! shader the compositor ships parses and validates (naga, the shader front end wgpu itself uses).
//! No GPU is needed.
use rightkit_compositor::*;
use std::collections::HashSet;

const ALL: [BlendMode; 17] = [
    BlendMode::Normal,
    BlendMode::Darken,
    BlendMode::Multiply,
    BlendMode::ColorBurn,
    BlendMode::Lighten,
    BlendMode::Screen,
    BlendMode::PlusLighter,
    BlendMode::ColorDodge,
    BlendMode::Overlay,
    BlendMode::SoftLight,
    BlendMode::HardLight,
    BlendMode::Difference,
    BlendMode::Exclusion,
    BlendMode::Hue,
    BlendMode::Saturation,
    BlendMode::Color,
    BlendMode::Luminosity,
];

#[test]
fn blend_modes_have_distinct_shader_codes_and_kebab_names() {
    let codes: HashSet<u32> = ALL.iter().map(|m| m.shader_code()).collect();
    assert_eq!(codes.len(), ALL.len());
    assert_eq!(BlendMode::Normal.shader_code(), 0);
    assert_eq!(
        serde_json::to_string(&BlendMode::PlusLighter).unwrap(),
        "\"plus-lighter\""
    );
    assert_eq!(
        serde_json::from_str::<BlendMode>("\"color-burn\"").unwrap(),
        BlendMode::ColorBurn
    );
}

#[test]
fn a_frame_descriptor_round_trips_in_camel_case() {
    let json = serde_json::json!({
        "width": 1920, "height": 1080,
        "clear": { "color": [0.0, 0.0, 0.0, 1.0] },
        "items": [{ "type": "layer", "textureId": "clip-1",
            "transform": { "centerX": 960.0, "centerY": 540.0, "width": 1920.0, "height": 1080.0, "rotationDegrees": 0.0, "flipX": false, "flipY": false },
            "opacity": 1.0, "blendMode": "normal", "mask": null }]
    });
    let frame: FrameDescriptor = serde_json::from_value(json.clone()).expect("descriptor parses");
    assert_eq!(
        (frame.width, frame.height, frame.items.len()),
        (1920, 1080, 1)
    );
    let back = serde_json::to_value(&frame).unwrap();
    assert_eq!(back["items"][0]["textureId"], "clip-1");
    assert_eq!(back["items"][0]["blendMode"], "normal");
}

fn validate(name: &str, source: &str) {
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|e| panic!("{name} does not parse: {}", e.emit_to_string(source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{name} does not validate: {e:?}"));
}

#[test]
fn every_shader_parses_and_validates() {
    let fullscreen = include_str!("../src/gpu/shaders/fullscreen.wgsl");
    for (name, source) in [
        ("fullscreen", fullscreen),
        ("blit", include_str!("../src/gpu/shaders/blit.wgsl")),
        (
            "gaussian_blur",
            include_str!("../src/effects/shaders/gaussian_blur.wgsl"),
        ),
        (
            "jfa_init",
            include_str!("../src/masks/shaders/jfa_init.wgsl"),
        ),
        (
            "jfa_step",
            include_str!("../src/masks/shaders/jfa_step.wgsl"),
        ),
        (
            "jfa_distance",
            include_str!("../src/masks/shaders/jfa_distance.wgsl"),
        ),
        ("layer", include_str!("../src/shaders/layer.wgsl")),
        ("blend", include_str!("../src/shaders/blend.wgsl")),
        ("mask", include_str!("../src/shaders/mask.wgsl")),
    ] {
        // Some fragment shaders are compiled after the shared fullscreen vertex stage.
        if naga::front::wgsl::parse_str(source).is_ok() {
            validate(name, source);
        } else {
            validate(name, &format!("{fullscreen}\n{source}"));
        }
    }
}
