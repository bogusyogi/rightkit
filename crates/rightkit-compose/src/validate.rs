//! Structural checks run before every evaluation. A composition that passes
//! `validate` evaluates without further error handling.

use std::collections::HashSet;

use crate::error::CompositionError;
use crate::model::{
    AssetRef, ClickShape, Clip, Composition, Content, Layer, MotionBlur, Pattern, Point, Rect,
    Rgba, Shadow, Stop, Style, Transform, SCHEMA,
};
use crate::track::{Easing, Keyframe};

const EDGE_EPSILON: f64 = 1e-9;
const MAX_MOTION_SAMPLES: u32 = 64;

pub fn validate(composition: &Composition) -> Result<(), CompositionError> {
    if composition.schema != SCHEMA {
        return Err(CompositionError::Schema(composition.schema.clone()));
    }
    let canvas = composition.canvas;
    if canvas.width == 0
        || canvas.height == 0
        || canvas.frame_rate_num == 0
        || canvas.frame_rate_den == 0
    {
        return Err(CompositionError::Canvas);
    }
    if composition.duration_ms == 0 {
        return Err(CompositionError::Duration);
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for (index, layer) in composition.layers.iter().enumerate() {
        if layer.id.is_empty() || !seen.insert(layer.id.as_str()) {
            return Err(CompositionError::LayerId(layer.id.clone()));
        }
        check_layer(composition, index, layer)?;
    }
    Ok(())
}

fn invalid(layer: &Layer, reason: impl Into<String>) -> CompositionError {
    CompositionError::Invalid {
        layer: layer.id.clone(),
        reason: reason.into(),
    }
}

fn check_layer(
    composition: &Composition,
    index: usize,
    layer: &Layer,
) -> Result<(), CompositionError> {
    let timing = layer.timing;
    if timing.start_ms >= timing.end_ms || timing.end_ms > composition.duration_ms {
        return Err(invalid(
            layer,
            "timing must satisfy start < end <= composition duration",
        ));
    }

    let (transform_ok, clip_ok, blur_ok) = match layer.content {
        Content::Video { .. } | Content::Still { .. } => (true, true, true),
        Content::Webcam { .. } => (true, true, true),
        Content::Cursor { .. } => (false, false, true),
        Content::Background { .. }
        | Content::Badge { .. }
        | Content::Caption { .. }
        | Content::ClickEffect { .. } => (false, false, false),
    };
    if !transform_ok && !layer.transform.is_empty() {
        return Err(invalid(
            layer,
            "a transform track is not supported by this layer kind",
        ));
    }
    if !clip_ok && (layer.clip.is_some() || layer.shadow.is_some()) {
        return Err(invalid(
            layer,
            "clip and shadow are not supported by this layer kind",
        ));
    }
    if !blur_ok && layer.motion_blur.is_some() {
        return Err(invalid(
            layer,
            "motion blur is not supported by this layer kind",
        ));
    }

    check_track(&layer.opacity, "opacity", |v| (0.0..=1.0).contains(v))
        .map_err(|reason| invalid(layer, reason))?;
    check_track(&layer.transform, "transform", transform_value_ok)
        .map_err(|reason| invalid(layer, reason))?;
    if let Some(clip) = layer.clip {
        check_clip(layer, clip)?;
    }
    if let Some(shadow) = &layer.shadow {
        check_shadow(layer, shadow)?;
    }
    if let Some(blur) = &layer.motion_blur {
        check_motion_blur(layer, blur)?;
    }

    match &layer.content {
        Content::Video {
            asset,
            placement,
            crop,
            speed,
            ..
        } => {
            check_asset(layer, asset)?;
            check_rect(layer, placement, "placement")?;
            check_crop_track(layer, crop)?;
            if !speed.is_finite() || *speed == 0.0 {
                return Err(invalid(layer, "speed must be finite and non-zero"));
            }
        }
        Content::Still {
            asset,
            placement,
            crop,
        } => {
            check_asset(layer, asset)?;
            check_rect(layer, placement, "placement")?;
            check_crop_track(layer, crop)?;
        }
        Content::Webcam {
            asset, placement, ..
        } => {
            check_asset(layer, asset)?;
            check_rect(layer, placement, "placement")?;
        }
        Content::Cursor {
            follows,
            scale,
            sprite,
            samples,
        } => {
            let target = composition.layers[..index]
                .iter()
                .find(|l| l.id == *follows)
                .ok_or_else(|| {
                    invalid(
                        layer,
                        format!("cursor follows {follows:?}, which is not an earlier layer"),
                    )
                })?;
            if !matches!(
                target.content,
                Content::Video { .. } | Content::Still { .. }
            ) {
                return Err(invalid(layer, "cursor must follow a video or still layer"));
            }
            if !scale.is_finite() || *scale <= 0.0 {
                return Err(invalid(layer, "cursor scale must be finite and positive"));
            }
            check_asset(layer, &sprite.asset)?;
            if !sprite.hotspot.x.is_finite() || !sprite.hotspot.y.is_finite() {
                return Err(invalid(layer, "cursor hotspot must be finite"));
            }
            for pair in samples.windows(2) {
                if pair[0].at_ms >= pair[1].at_ms {
                    return Err(invalid(
                        layer,
                        "cursor samples must be strictly increasing in time",
                    ));
                }
            }
            if samples.iter().any(|s| !s.x.is_finite() || !s.y.is_finite()) {
                return Err(invalid(layer, "cursor sample positions must be finite"));
            }
        }
        Content::Background { pattern } => check_pattern(layer, pattern)?,
        Content::Badge {
            label,
            anchor,
            style,
        } => {
            if label.is_empty() {
                return Err(invalid(layer, "badge label must not be empty"));
            }
            check_point_unit(layer, *anchor, "badge anchor")?;
            check_style(layer, style)?;
        }
        Content::Caption { text, rect, style } => {
            if text.is_empty() {
                return Err(invalid(layer, "caption text must not be empty"));
            }
            check_rect(layer, rect, "caption rect")?;
            check_style(layer, style)?;
        }
        Content::ClickEffect {
            anchor,
            effect,
            style,
            duration_ms,
            easing,
            ..
        } => {
            check_point_unit(layer, *anchor, "click anchor")?;
            check_style(layer, style)?;
            if *duration_ms == 0 {
                return Err(invalid(layer, "click effect duration must be positive"));
            }
            check_easing_params(layer, easing)?;
            check_click_shape(layer, effect)?;
        }
    }
    Ok(())
}

fn spring_params_ok(stiffness: f64, damping: f64, mass: f64) -> bool {
    stiffness.is_finite()
        && stiffness > 0.0
        && mass.is_finite()
        && mass > 0.0
        && damping.is_finite()
        && damping >= 0.0
}

fn check_click_shape(layer: &Layer, shape: &ClickShape) -> Result<(), CompositionError> {
    let positive = |v: f64| v.is_finite() && v > 0.0;
    let non_negative = |v: f64| v.is_finite() && v >= 0.0;
    let ok = match *shape {
        ClickShape::Ripple {
            max_radius,
            stroke_width,
        } => positive(max_radius) && positive(stroke_width),
        ClickShape::Highlight {
            half_width,
            half_height,
            grow,
            corner_radius,
        } => {
            positive(half_width)
                && positive(half_height)
                && non_negative(grow)
                && non_negative(corner_radius)
        }
    };
    if ok {
        Ok(())
    } else {
        Err(invalid(
            layer,
            "click shape sizes must be finite, positive where required, and non-negative otherwise",
        ))
    }
}

/// Spring parameters must describe a physical oscillator. Other easings
/// carry no parameters to check here.
fn check_easing_params(layer: &Layer, easing: &Easing) -> Result<(), CompositionError> {
    if let Easing::Spring {
        stiffness,
        damping,
        mass,
    } = *easing
    {
        if !spring_params_ok(stiffness, damping, mass) {
            return Err(invalid(
                layer,
                "spring needs finite stiffness > 0, mass > 0 and damping >= 0",
            ));
        }
    }
    Ok(())
}

fn transform_value_ok(t: &Transform) -> bool {
    t.dx.is_finite() && t.dy.is_finite() && t.scale.is_finite() && t.scale > 0.0
}

/// Checks keyframe ordering, values, and easing. Returns a reason on failure.
fn check_track<T>(
    track: &[Keyframe<T>],
    name: &str,
    value_ok: impl Fn(&T) -> bool,
) -> Result<(), String> {
    for pair in track.windows(2) {
        if pair[0].at_ms >= pair[1].at_ms {
            return Err(format!(
                "{name} keyframes must be strictly increasing in time"
            ));
        }
    }
    for keyframe in track {
        if !value_ok(&keyframe.value) {
            return Err(format!(
                "{name} keyframe at {} ms has an invalid value",
                keyframe.at_ms
            ));
        }
        if let Easing::CubicBezier { x1, y1, x2, y2 } = keyframe.easing {
            let x_ok = (0.0..=1.0).contains(&x1) && (0.0..=1.0).contains(&x2);
            if !x_ok || !y1.is_finite() || !y2.is_finite() {
                return Err(format!(
                    "{name} keyframe at {} ms has invalid bezier control points",
                    keyframe.at_ms
                ));
            }
        }
        if let Easing::Spring {
            stiffness,
            damping,
            mass,
        } = keyframe.easing
        {
            if !spring_params_ok(stiffness, damping, mass) {
                return Err(format!(
                    "{name} keyframe at {} ms has invalid spring parameters",
                    keyframe.at_ms
                ));
            }
        }
    }
    Ok(())
}

fn check_crop_track(layer: &Layer, track: &[Keyframe<Rect>]) -> Result<(), CompositionError> {
    check_track(track, "crop", |r| rect_is_valid(*r) && crop_in_source(*r))
        .map_err(|reason| invalid(layer, reason))
}

fn rect_is_valid(r: Rect) -> bool {
    [r.x, r.y, r.width, r.height].iter().all(|v| v.is_finite()) && r.width > 0.0 && r.height > 0.0
}

fn crop_in_source(r: Rect) -> bool {
    r.x >= -EDGE_EPSILON
        && r.y >= -EDGE_EPSILON
        && r.x + r.width <= 1.0 + EDGE_EPSILON
        && r.y + r.height <= 1.0 + EDGE_EPSILON
}

fn check_rect(layer: &Layer, r: &Rect, name: &str) -> Result<(), CompositionError> {
    if rect_is_valid(*r) {
        Ok(())
    } else {
        Err(invalid(
            layer,
            format!("{name} must have finite coordinates and positive size"),
        ))
    }
}

fn check_point_unit(layer: &Layer, p: Point, name: &str) -> Result<(), CompositionError> {
    let ok = (0.0..=1.0).contains(&p.x) && (0.0..=1.0).contains(&p.y);
    if ok {
        Ok(())
    } else {
        Err(invalid(
            layer,
            format!("{name} must lie within the canvas (0..=1)"),
        ))
    }
}

fn check_asset(layer: &Layer, asset: &AssetRef) -> Result<(), CompositionError> {
    if asset.id.is_empty() {
        return Err(invalid(layer, "asset id must not be empty"));
    }
    if asset.width == 0 || asset.height == 0 {
        return Err(invalid(
            layer,
            format!("asset {:?} must have positive pixel dimensions", asset.id),
        ));
    }
    Ok(())
}

fn color_ok(c: &Rgba) -> bool {
    [c.r, c.g, c.b, c.a]
        .iter()
        .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
}

fn check_style(layer: &Layer, style: &Style) -> Result<(), CompositionError> {
    if color_ok(&style.foreground) && color_ok(&style.background) {
        Ok(())
    } else {
        Err(invalid(
            layer,
            "style colours must be finite and within 0..=1",
        ))
    }
}

fn check_clip(layer: &Layer, clip: Clip) -> Result<(), CompositionError> {
    match clip {
        Clip::RoundedRect { radius } if radius.is_finite() && (0.0..=0.5).contains(&radius) => {
            Ok(())
        }
        Clip::RoundedRect { .. } => {
            Err(invalid(layer, "rounded clip radius must be within 0..=0.5"))
        }
        Clip::Circle => Ok(()),
    }
}

fn check_shadow(layer: &Layer, shadow: &Shadow) -> Result<(), CompositionError> {
    let finite = [shadow.offset_x, shadow.offset_y, shadow.blur]
        .iter()
        .all(|v| v.is_finite());
    if !finite || shadow.blur < 0.0 || !color_ok(&shadow.color) {
        return Err(invalid(
            layer,
            "shadow offsets and blur must be finite, blur non-negative, colour within 0..=1",
        ));
    }
    Ok(())
}

fn check_motion_blur(layer: &Layer, blur: &MotionBlur) -> Result<(), CompositionError> {
    let shutter_ok = blur.shutter_degrees.is_finite()
        && blur.shutter_degrees > 0.0
        && blur.shutter_degrees <= 360.0;
    if !shutter_ok || !(1..=MAX_MOTION_SAMPLES).contains(&blur.samples) {
        return Err(invalid(
            layer,
            "motion blur needs shutter in (0, 360] and 1..=64 samples",
        ));
    }
    Ok(())
}

fn check_pattern(layer: &Layer, pattern: &Pattern) -> Result<(), CompositionError> {
    match pattern {
        Pattern::Solid { color } => {
            if !color_ok(color) {
                return Err(invalid(
                    layer,
                    "background colour must be finite and within 0..=1",
                ));
            }
            Ok(())
        }
        Pattern::LinearGradient { angle_deg, stops } => {
            if !angle_deg.is_finite() {
                return Err(invalid(layer, "gradient angle must be finite"));
            }
            check_stops(layer, stops)
        }
        Pattern::RadialGradient { center, stops } => {
            if !center.x.is_finite() || !center.y.is_finite() {
                return Err(invalid(layer, "gradient centre must be finite"));
            }
            check_stops(layer, stops)
        }
    }
}

fn check_stops(layer: &Layer, stops: &[Stop]) -> Result<(), CompositionError> {
    if stops.is_empty() {
        return Err(invalid(layer, "a gradient needs at least one stop"));
    }
    let mut previous = 0.0_f64;
    for stop in stops {
        if !stop.at.is_finite() || !(0.0..=1.0).contains(&stop.at) || stop.at < previous {
            return Err(invalid(
                layer,
                "gradient stops must lie in 0..=1 and be non-decreasing",
            ));
        }
        if !color_ok(&stop.color) {
            return Err(invalid(
                layer,
                "gradient stop colour must be finite and within 0..=1",
            ));
        }
        previous = stop.at;
    }
    Ok(())
}
