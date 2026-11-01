//! Structural validation of an OTIO document against the core schema shapes
//! (field names and JSON types of `Timeline.1`, `Stack.1`, `Track.1`, `Clip.2`,
//! `Gap.1`, `Marker.2`, `ExternalReference.1`, `RationalTime.1`, `TimeRange.1`).
//! No Python / OTIO runtime is involved; this is the shape check the export and
//! its tests rely on.

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{path}: {message}")]
pub struct ShapeError {
    pub path: String,
    pub message: String,
}

fn err<T>(path: &str, message: impl Into<String>) -> Result<T, ShapeError> {
    Err(ShapeError {
        path: path.into(),
        message: message.into(),
    })
}

fn obj<'a>(
    v: &'a Value,
    path: &str,
    schema: &str,
) -> Result<&'a serde_json::Map<String, Value>, ShapeError> {
    let Some(o) = v.as_object() else {
        return err(path, format!("expected an object ({schema})"));
    };
    match o.get("OTIO_SCHEMA").and_then(Value::as_str) {
        Some(s) if s == schema => Ok(o),
        other => err(
            path,
            format!("expected OTIO_SCHEMA {schema}, found {other:?}"),
        ),
    }
}

type Obj = serde_json::Map<String, Value>;

fn field<'a>(o: &'a Obj, path: &str, key: &str) -> Result<&'a Value, ShapeError> {
    o.get(key)
        .map_or_else(|| err(path, format!("missing field {key:?}")), Ok)
}

fn string(o: &Obj, path: &str, key: &str) -> Result<(), ShapeError> {
    if field(o, path, key)?.is_string() {
        Ok(())
    } else {
        err(path, format!("{key:?} must be a string"))
    }
}

fn boolean(o: &Obj, path: &str, key: &str) -> Result<(), ShapeError> {
    if field(o, path, key)?.is_boolean() {
        Ok(())
    } else {
        err(path, format!("{key:?} must be a boolean"))
    }
}

fn dict(o: &Obj, path: &str, key: &str) -> Result<(), ShapeError> {
    if field(o, path, key)?.is_object() {
        Ok(())
    } else {
        err(path, format!("{key:?} must be an object"))
    }
}

fn list<'a>(o: &'a Obj, path: &str, key: &str) -> Result<&'a Vec<Value>, ShapeError> {
    field(o, path, key)?
        .as_array()
        .map_or_else(|| err(path, format!("{key:?} must be an array")), Ok)
}

fn rational_time(v: &Value, path: &str) -> Result<(f64, f64), ShapeError> {
    let o = obj(v, path, "RationalTime.1")?;
    let value = field(o, path, "value")?.as_f64();
    let rate = field(o, path, "rate")?.as_f64();
    match (value, rate) {
        (Some(v), Some(r)) if r > 0.0 => Ok((v, r)),
        _ => err(path, "RationalTime needs numeric value and positive rate"),
    }
}

fn time_range(v: &Value, path: &str) -> Result<(), ShapeError> {
    let o = obj(v, path, "TimeRange.1")?;
    rational_time(field(o, path, "start_time")?, &format!("{path}.start_time"))?;
    let (d, _) = rational_time(field(o, path, "duration")?, &format!("{path}.duration"))?;
    if d < 0.0 {
        return err(path, "negative duration");
    }
    Ok(())
}

fn composable_common(o: &Obj, path: &str) -> Result<(), ShapeError> {
    string(o, path, "name")?;
    boolean(o, path, "enabled")?;
    dict(o, path, "metadata")?;
    list(o, path, "effects")?;
    for (i, m) in list(o, path, "markers")?.iter().enumerate() {
        marker(m, &format!("{path}.markers[{i}]"))?;
    }
    match field(o, path, "source_range")? {
        Value::Null => Ok(()),
        v => time_range(v, &format!("{path}.source_range")),
    }
}

fn marker(v: &Value, path: &str) -> Result<(), ShapeError> {
    let o = obj(v, path, "Marker.2")?;
    string(o, path, "name")?;
    string(o, path, "color")?;
    string(o, path, "comment")?;
    dict(o, path, "metadata")?;
    time_range(
        field(o, path, "marked_range")?,
        &format!("{path}.marked_range"),
    )
}

fn external_reference(v: &Value, path: &str) -> Result<(), ShapeError> {
    let o = obj(v, path, "ExternalReference.1")?;
    string(o, path, "target_url")?;
    dict(o, path, "metadata")?;
    match field(o, path, "available_range")? {
        Value::Null => Ok(()),
        v => time_range(v, &format!("{path}.available_range")),
    }
}

fn clip(v: &Value, path: &str) -> Result<(), ShapeError> {
    let o = obj(v, path, "Clip.2")?;
    composable_common(o, path)?;
    let refs = field(o, path, "media_references")?
        .as_object()
        .ok_or_else(|| ShapeError {
            path: path.into(),
            message: "\"media_references\" must be an object".into(),
        })?;
    for (k, r) in refs {
        external_reference(r, &format!("{path}.media_references.{k}"))?;
    }
    let key = field(o, path, "active_media_reference_key")?
        .as_str()
        .ok_or_else(|| ShapeError {
            path: path.into(),
            message: "\"active_media_reference_key\" must be a string".into(),
        })?;
    if !refs.contains_key(key) {
        return err(
            path,
            format!("active media reference {key:?} is not in media_references"),
        );
    }
    if o.get("source_range").is_none_or(Value::is_null) {
        return err(path, "clip needs a source_range");
    }
    Ok(())
}

fn gap(v: &Value, path: &str) -> Result<(), ShapeError> {
    let o = obj(v, path, "Gap.1")?;
    composable_common(o, path)?;
    if o.get("source_range").is_none_or(Value::is_null) {
        return err(path, "gap needs a source_range");
    }
    Ok(())
}

fn track(v: &Value, path: &str) -> Result<(), ShapeError> {
    let o = obj(v, path, "Track.1")?;
    composable_common(o, path)?;
    let kind = field(o, path, "kind")?.as_str().unwrap_or("");
    if kind != "Video" && kind != "Audio" {
        return err(
            path,
            format!("track kind must be Video or Audio, found {kind:?}"),
        );
    }
    let mut fps_seen: Option<f64> = None;
    for (i, c) in list(o, path, "children")?.iter().enumerate() {
        let p = format!("{path}.children[{i}]");
        match c.get("OTIO_SCHEMA").and_then(Value::as_str) {
            Some("Clip.2") => {
                clip(c, &p)?;
                if kind == "Video" && c["metadata"]["cutright"]["linked_video_clip"].is_string() {
                    return err(&p, "linked audio clip on a video track");
                }
            }
            Some("Gap.1") => gap(c, &p)?,
            other => return err(&p, format!("unexpected track child {other:?}")),
        }
        // Items on one track share one rate.
        let rate = c["source_range"]["duration"]["rate"].as_f64();
        if let (Some(a), Some(b)) = (fps_seen, rate) {
            if (a - b).abs() > 1e-9 {
                return err(&p, "mixed rates on one track");
            }
        }
        fps_seen = fps_seen.or(rate);
    }
    Ok(())
}

/// Checks `doc` is a structurally valid OTIO `Timeline.1` as written by this crate.
pub fn validate_otio(doc: &Value) -> Result<(), ShapeError> {
    let o = obj(doc, "$", "Timeline.1")?;
    string(o, "$", "name")?;
    dict(o, "$", "metadata")?;
    match field(o, "$", "global_start_time")? {
        Value::Null => {}
        v => {
            rational_time(v, "$.global_start_time")?;
        }
    }
    let stack = obj(field(o, "$", "tracks")?, "$.tracks", "Stack.1")?;
    composable_common(stack, "$.tracks")?;
    for (i, t) in list(stack, "$.tracks", "children")?.iter().enumerate() {
        track(t, &format!("$.tracks.children[{i}]"))?;
    }
    Ok(())
}
