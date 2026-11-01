//! rightkit-compose: build a composition (a scaling video, a fading still and a retimed video), evaluate it at known
//! times against hand-computed frame descriptions, round-trip it through JSON, and refuse invalid compositions.
use rightkit_compose::{
    evaluate_ms, sample, validate, AssetRef, Canvas, CompositionError, Composition, Content, Easing, Keyframe, Layer,
    Payload, Rational, Rect, RetimeSegment, Ticks, Timing, Transform, TICKS_PER_SECOND,
};
use serde_json::{json, Value};

use super::Ctx;

const EPS: f64 = 1e-6;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < EPS
}

fn asset(id: &str, duration_ms: Option<u64>) -> AssetRef {
    AssetRef { id: id.into(), blake3: format!("blake3-{id}"), width: 1920, height: 1080, duration: duration_ms.map(Ticks::from_ms) }
}

fn layer(id: &str, start_ms: u64, end_ms: u64, content: Content) -> Layer {
    Layer {
        id: id.into(),
        timing: Timing { start: Ticks::from_ms(start_ms), end: Ticks::from_ms(end_ms) },
        transform: Vec::new(),
        opacity: Vec::new(),
        clip: None,
        shadow: None,
        motion_blur: None,
        content,
    }
}

fn kf<T>(at_ms: u64, value: T) -> Keyframe<T> {
    Keyframe { at: Ticks::from_ms(at_ms), value, easing: Easing::Linear }
}

fn composition() -> Composition {
    let mut screen = layer(
        "screen",
        0,
        10_000,
        Content::Video { asset: asset("screen-src", None), placement: Rect::FULL, crop: Vec::new(), retime: Vec::new() },
    );
    screen.transform = vec![
        kf(0, Transform { dx: 0.0, dy: 0.0, scale: 1.0 }),
        kf(1000, Transform { dx: 0.0, dy: 0.0, scale: 2.0 }),
    ];
    let mut fade = layer(
        "fade",
        1000,
        5000,
        Content::Still { asset: asset("title-card", None), placement: Rect::FULL, crop: Vec::new() },
    );
    fade.opacity = vec![kf(0, 0.0), kf(2000, 1.0)];
    // Two seconds of output reading four seconds of source at double speed.
    let slow = layer(
        "double-speed",
        3000,
        5000,
        Content::Video {
            asset: asset("clip-src", Some(10_000)),
            placement: Rect::FULL,
            crop: Vec::new(),
            retime: vec![RetimeSegment::Play { source_in: Ticks::from_ms(0), source_out: Ticks::from_ms(4000), speed: 2.0 }],
        },
    );
    let mut c = Composition::new(Canvas { width: 1920, height: 1080 }, Rational::new(30, 1), Ticks::from_ms(10_000));
    c.layers = vec![screen, fade, slow];
    c
}

fn payload<'a>(frame: &'a rightkit_compose::FrameDescription, id: &str) -> Result<&'a Payload, String> {
    frame.layers.iter().find(|l| l.id == id).map(|l| &l.payload).ok_or_else(|| format!("layer {id} is not active"))
}

pub fn probe(_ctx: &Ctx) -> Result<Value, String> {
    if TICKS_PER_SECOND != 705_600_000 || Ticks::from_ms(1000) != Ticks(TICKS_PER_SECOND) {
        return Err("the CutRight tick clock is not 705,600,000 per second".into());
    }
    let c = composition();
    validate(&c).map_err(|e| format!("a valid composition was refused: {e}"))?;

    // t = 0: only the always-on layer is active, at identity scale.
    let f0 = evaluate_ms(&c, 0).map_err(|e| format!("evaluate 0: {e}"))?;
    if (f0.width, f0.height) != (1920, 1080) || f0.layers.len() != 1 {
        return Err(format!("frame 0 is {}x{} with {} layers", f0.width, f0.height, f0.layers.len()));
    }
    let Payload::Video { dest_px, .. } = payload(&f0, "screen")? else { return Err("screen is not a video payload".into()) };
    if !(close(dest_px.x, 0.0) && close(dest_px.width, 1920.0) && close(dest_px.height, 1080.0)) {
        return Err(format!("frame 0 placement is wrong: {dest_px:?}"));
    }

    // t = 500 ms: halfway through a 1x to 2x scale about the centre.
    let f500 = evaluate_ms(&c, 500).map_err(|e| format!("evaluate 500: {e}"))?;
    let Payload::Video { dest_px, .. } = payload(&f500, "screen")? else { return Err("screen is not a video payload".into()) };
    if !(close(dest_px.width, 2880.0) && close(dest_px.x, -480.0)) {
        return Err(format!("frame 500 placement is wrong: {dest_px:?}"));
    }
    if f500.layers.len() != 1 {
        return Err("the fade layer is active before its start".into());
    }

    // t = 1000 ms: scale 2 and the fade layer's first instant (opacity 0). t = 2000 ms: opacity halfway.
    let f1000 = evaluate_ms(&c, 1000).map_err(|e| e.to_string())?;
    let Payload::Video { dest_px, .. } = payload(&f1000, "screen")? else { return Err("screen is not a video payload".into()) };
    if !(close(dest_px.width, 3840.0) && close(dest_px.x, -960.0)) {
        return Err(format!("frame 1000 placement is wrong: {dest_px:?}"));
    }
    let fade_at_start = f1000.layers.iter().find(|l| l.id == "fade").ok_or("fade is not active at 1000 ms")?;
    if !close(fade_at_start.opacity, 0.0) || fade_at_start.stack_index != 1 {
        return Err(format!("fade starts at opacity {} in stack slot {}", fade_at_start.opacity, fade_at_start.stack_index));
    }
    let f2000 = evaluate_ms(&c, 2000).map_err(|e| e.to_string())?;
    let fade = f2000.layers.iter().find(|l| l.id == "fade").ok_or("fade is not active at 2000 ms")?;
    if !close(fade.opacity, 0.5) {
        return Err(format!("fade opacity at 2000 ms is {}", fade.opacity));
    }

    // t = 4000 ms: the double-speed layer is 1 s in, so it reads 2 s of source.
    let f4000 = evaluate_ms(&c, 4000).map_err(|e| e.to_string())?;
    let Payload::Video { source, .. } = payload(&f4000, "double-speed")? else { return Err("double-speed is not a video payload".into()) };
    if *source != Ticks::from_ms(2000) {
        return Err(format!("double speed reads source {source:?} at output 1 s, expected 2 s"));
    }
    // After the layers end only the screen remains; the composition's own end is outside the timeline.
    let f6000 = evaluate_ms(&c, 6000).map_err(|e| e.to_string())?;
    if f6000.layers.len() != 1 {
        return Err(format!("{} layers at 6 s, expected only the screen", f6000.layers.len()));
    }
    match evaluate_ms(&c, 10_000) {
        Err(CompositionError::OutOfRange { .. }) => {}
        other => return Err(format!("a time at the duration: expected OutOfRange, got {other:?}")),
    }

    // Easing on its own: ease-in is quadratic, so halfway in time is a quarter... of a half: 0.125 at 500 of 1000 ms.
    let ease = vec![Keyframe { at: Ticks::from_ms(0), value: 0.0, easing: Easing::EaseIn }, kf(1000, 1.0)];
    let mid = sample(&ease, Ticks::from_ms(500)).ok_or("an eased track sampled to nothing")?;
    if !close(mid, 0.125) {
        return Err(format!("ease-in at the midpoint is {mid}, expected 0.125"));
    }

    // Durable document: JSON round trip is lossless and evaluates identically.
    let text = serde_json::to_string(&c).map_err(|e| e.to_string())?;
    let back = Composition::from_json_str(&text).map_err(|e| format!("reload: {e}"))?;
    if back != c || evaluate_ms(&back, 500).map_err(|e| e.to_string())? != f500 {
        return Err("the composition changed across a JSON round trip".into());
    }

    // Invalid compositions are typed refusals, never drawn.
    let mut duplicate = composition();
    duplicate.layers[1].id = "screen".into();
    match validate(&duplicate) {
        Err(CompositionError::LayerId(id)) if id == "screen" => {}
        other => return Err(format!("duplicate layer id: got {other:?}")),
    }
    let mut overrun = composition();
    overrun.layers[2].timing.end = Ticks::from_ms(11_000);
    match evaluate_ms(&overrun, 0) {
        Err(CompositionError::Invalid { layer, .. }) if layer == "double-speed" => {}
        other => return Err(format!("a layer past the composition end: got {other:?}")),
    }
    let mut empty = composition();
    empty.canvas = Canvas { width: 0, height: 1080 };
    if !matches!(validate(&empty), Err(CompositionError::Canvas)) {
        return Err("a zero-width canvas was accepted".into());
    }
    match Composition::from_json_str("{ nope") {
        Err(CompositionError::Json(_)) => {}
        other => return Err(format!("non-JSON: got {other:?}")),
    }

    Ok(json!({
        "layers": c.layers.len(),
        "frames": { "0": f0.layers.len(), "500": f500.layers.len(), "1000": f1000.layers.len(), "2000": f2000.layers.len(), "4000": f4000.layers.len() },
        "fadeOpacityAt2000": fade.opacity,
        "documentBytes": text.len(),
    }))
}
