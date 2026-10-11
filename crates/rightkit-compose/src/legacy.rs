//! Read-only conversion of `rightkit.composition.v1` documents to v2.
//!
//! The conversion rewrites a JSON value in place, then the normal v2
//! deserialiser runs. Rules:
//!
//! - Every millisecond field `X_ms` becomes `X_ticks` with the value times
//!   705,600, which is exact. This covers timing, keyframes, durations, cursor
//!   samples, click effects, webcam source offsets and asset durations.
//! - `canvas.frame_rate_num` and `frame_rate_den` move to `fps`.
//! - A video's `source_start_ms` (default 0) and `speed` (default 1) become one
//!   retime segment spanning the layer. Forward speed becomes `Play` with
//!   `source_out = source_start + ceil(speed * span)`. Negative speed becomes
//!   `Reverse` ending at `source_start`. Both have output duration at least the
//!   layer span, so validation accepts them.
//!
//! The conversion is one-way. Writing a composition always emits v2.

use serde_json::{json, Map, Value};

use crate::ticks::TICKS_PER_MS;

pub(crate) fn migrate_v1(doc: &mut Value) -> Result<(), String> {
    let root = doc.as_object_mut().ok_or("composition must be an object")?;
    root.insert(
        "schema".into(),
        Value::String(crate::model::SCHEMA.to_string()),
    );
    convert_key(root, "duration_ms", "duration_ticks")?;

    let canvas = root
        .get_mut("canvas")
        .and_then(Value::as_object_mut)
        .ok_or("canvas must be an object")?;
    let num = canvas
        .remove("frame_rate_num")
        .ok_or("missing frame_rate_num")?;
    let den = canvas
        .remove("frame_rate_den")
        .ok_or("missing frame_rate_den")?;
    root.insert("fps".into(), json!({ "num": num, "den": den }));

    let layers = root
        .get_mut("layers")
        .and_then(Value::as_array_mut)
        .ok_or("layers must be an array")?;
    for layer in layers {
        migrate_layer(layer)?;
    }
    Ok(())
}

fn migrate_layer(layer: &mut Value) -> Result<(), String> {
    let obj = layer.as_object_mut().ok_or("layer must be an object")?;
    let timing = obj
        .get_mut("timing")
        .and_then(Value::as_object_mut)
        .ok_or("layer timing must be an object")?;
    convert_key(timing, "start_ms", "start_ticks")?;
    convert_key(timing, "end_ms", "end_ticks")?;
    let span = {
        let start = timing["start_ticks"].as_u64().ok_or("start_ticks")?;
        let end = timing["end_ticks"].as_u64().ok_or("end_ticks")?;
        end.checked_sub(start).ok_or("layer end precedes start")?
    };

    for key in ["transform", "opacity"] {
        if let Some(track) = obj.get_mut(key) {
            migrate_track(track)?;
        }
    }

    let content = obj
        .get_mut("content")
        .and_then(Value::as_object_mut)
        .ok_or("layer content must be an object")?;
    let kind = content
        .get("kind")
        .and_then(Value::as_str)
        .ok_or("content kind missing")?
        .to_string();
    match kind.as_str() {
        "video" => {
            migrate_asset(content.get_mut("asset"))?;
            if let Some(crop) = content.get_mut("crop") {
                migrate_track(crop)?;
            }
            let source_start = take_ms(content, "source_start_ms", 0)?;
            let speed = content
                .remove("speed")
                .map(|v| v.as_f64().ok_or("speed must be a number"))
                .transpose()?
                .unwrap_or(1.0);
            content.insert(
                "retime".into(),
                Value::Array(vec![retime_for(source_start, speed, span)?]),
            );
        }
        "still" => {
            migrate_asset(content.get_mut("asset"))?;
            if let Some(crop) = content.get_mut("crop") {
                migrate_track(crop)?;
            }
        }
        "webcam" => {
            migrate_asset(content.get_mut("asset"))?;
            convert_key(content, "source_start_ms", "source_start_ticks")?;
        }
        "cursor" => {
            if let Some(sprite) = content.get_mut("sprite").and_then(Value::as_object_mut) {
                migrate_asset(sprite.get_mut("asset"))?;
            }
            if let Some(samples) = content.get_mut("samples").and_then(Value::as_array_mut) {
                for sample in samples {
                    if let Some(s) = sample.as_object_mut() {
                        convert_key(s, "at_ms", "at_ticks")?;
                    }
                }
            }
        }
        "click_effect" => {
            convert_key(content, "onset_ms", "onset_ticks")?;
            convert_key(content, "duration_ms", "duration_ticks")?;
        }
        _ => {}
    }
    Ok(())
}

/// The retime segment equivalent to `source(L) = source_start + speed * L` for
/// local time `L` in `0..span`.
fn retime_for(source_start: u64, speed: f64, span: u64) -> Result<Value, String> {
    if !speed.is_finite() || speed == 0.0 {
        return Err("legacy speed must be finite and non-zero".into());
    }
    let travel = (speed.abs() * span as f64).ceil() as u64;
    if speed > 0.0 {
        let source_out = source_start
            .checked_add(travel)
            .ok_or("legacy source range overflows")?;
        Ok(json!({
            "kind": "play",
            "source_in_ticks": source_start,
            "source_out_ticks": source_out,
            "speed": speed,
        }))
    } else {
        let source_in = source_start
            .checked_sub(travel)
            .ok_or("legacy reverse speed reads before source time zero")?;
        Ok(json!({
            "kind": "reverse",
            "source_in_ticks": source_in,
            "source_out_ticks": source_start,
            "speed": -speed,
        }))
    }
}

fn migrate_asset(asset: Option<&mut Value>) -> Result<(), String> {
    let asset = asset
        .and_then(Value::as_object_mut)
        .ok_or("asset must be an object")?;
    if asset.contains_key("duration_ms") {
        convert_key(asset, "duration_ms", "duration_ticks")?;
    }
    Ok(())
}

fn migrate_track(track: &mut Value) -> Result<(), String> {
    let keyframes = track.as_array_mut().ok_or("track must be an array")?;
    for keyframe in keyframes {
        let obj = keyframe
            .as_object_mut()
            .ok_or("keyframe must be an object")?;
        convert_key(obj, "at_ms", "at_ticks")?;
    }
    Ok(())
}

/// Moves `from` (milliseconds) to `to` (ticks), failing on a non-integer or
/// overflowing value.
fn convert_key(obj: &mut Map<String, Value>, from: &str, to: &str) -> Result<(), String> {
    if let Some(ms_value) = obj.remove(from) {
        let ms = ms_value
            .as_u64()
            .ok_or_else(|| format!("{from} must be a non-negative integer"))?;
        let ticks = ms
            .checked_mul(TICKS_PER_MS)
            .ok_or_else(|| format!("{from} overflows the tick range"))?;
        obj.insert(to.into(), Value::from(ticks));
    }
    Ok(())
}

fn take_ms(obj: &mut Map<String, Value>, key: &str, default: u64) -> Result<u64, String> {
    match obj.remove(key) {
        None => Ok(default),
        Some(v) => {
            let ms = v
                .as_u64()
                .ok_or_else(|| format!("{key} must be a non-negative integer"))?;
            ms.checked_mul(TICKS_PER_MS)
                .ok_or_else(|| format!("{key} overflows the tick range"))
        }
    }
}
