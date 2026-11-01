// rightkit-compositor 0.1.0 consumed by exact registry pin, outside the workspace.
//
// Proves the public data contract a host builds frames with: blend modes (distinct shader codes,
// CSS kebab-case names) and the frame descriptor's camelCase wire shape, parsed and serialised back.
// Does not prove: rendering (it needs a GPU adapter, which a hermetic runner may not have).
use rightkit_compositor::{BlendMode, FrameDescriptor};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        BlendMode::Normal.shader_code() == 0,
        "normal blend is shader code 0",
    );
    check(
        BlendMode::Multiply.shader_code() != BlendMode::Screen.shader_code(),
        "blend modes have distinct codes",
    );
    check(
        serde_json::to_string(&BlendMode::PlusLighter)
            .ok()
            .as_deref()
            == Some("\"plus-lighter\""),
        "blend modes use CSS names",
    );
    let json = serde_json::json!({
        "width": 1280, "height": 720,
        "clear": { "color": [0.0, 0.0, 0.0, 1.0] },
        "items": [{ "type": "layer", "textureId": "t",
            "transform": { "centerX": 640.0, "centerY": 360.0, "width": 1280.0, "height": 720.0, "rotationDegrees": 0.0, "flipX": false, "flipY": false },
            "opacity": 0.5, "blendMode": "screen", "mask": null }]
    });
    let frame: Result<FrameDescriptor, _> = serde_json::from_value(json);
    check(
        matches!(&frame, Ok(f) if f.width == 1280 && f.items.len() == 1),
        "a frame descriptor parses",
    );
    let back = serde_json::to_value(frame.expect("parsed")).expect("serialises");
    check(
        back["items"][0]["blendMode"] == "screen" && back["items"][0]["textureId"] == "t",
        "it serialises back in camelCase",
    );
    println!("rightkit-compositor consumer: all checks passed");
}
