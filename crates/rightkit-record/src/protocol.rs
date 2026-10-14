//! The helper wire protocol: the [`Backend`] seam carried as length-prefixed JSON
//! frames (`rightkit-sidecar`) between the core and an out-of-process helper.
//!
//! Every frame is one JSON object with a `kind`:
//!
//! - `request` (core to helper): `{"kind":"request","id":N,"op":OP,...}`. The
//!   ops are `hello`, `sources`, `start` (with `plan`), `stop`, and `abort`.
//! - `response` (helper to core): `{"kind":"response","id":N,"ok":true,"result":...}`
//!   or `{"kind":"response","id":N,"ok":false,"error":{"code":...,"message":...}}`.
//! - `event` (helper to core): `{"kind":"event","event":...}`, unsolicited.
//! - `bye` (helper to core): sent after the final response to `stop` or `abort`.
//!   A helper that ends without `bye` has been lost.
//!
//! The core sends one request at a time and waits for its response. A helper
//! writes every event it has produced before it writes the response that follows
//! them, so the core sees the events before the answer. Frames carry JSON only.
//! Pixels never cross this protocol: frames are reported as hashes.

use std::path::PathBuf;

use rightkit_contracts::types::record_event::DropReason;
use rightkit_contracts::types::record_session::{RecorderOs, TrackKind, VisibilityState};
use rightkit_contracts::types::Rect;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::backend::{
    BackendPlan, Codec, Failure, Finished, FrameSample, HelperEvent, HelperIdentity, Permission,
    Source, StartInfo, TrackOutput,
};
use crate::error::{Code, RecordError};
use crate::plan::PixelMasking;

/// The largest frame either side accepts. Frames hold JSON only, so this bounds
/// a corrupt or hostile length prefix without limiting any real message.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Core-side identity read from `hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub os: RecorderOs,
    pub os_build: String,
    pub helper: HelperIdentity,
    pub permission: Permission,
}

pub fn identity_result(identity: &Identity) -> Value {
    json!({
        "os": identity.os,
        "osBuild": identity.os_build,
        "helper": { "identity": identity.helper.identity, "signed": identity.helper.signed },
        "permission": encode_permission(identity.permission),
    })
}

pub fn decode_identity(value: &Value) -> Result<Identity, String> {
    Ok(Identity {
        os: serde_field(value, "os")?,
        os_build: str_field(value, "osBuild")?.to_string(),
        helper: HelperIdentity {
            identity: str_field(field(value, "helper")?, "identity")?.to_string(),
            signed: bool_field(field(value, "helper")?, "signed")?,
        },
        permission: decode_permission(str_field(value, "permission")?)?,
    })
}

pub fn encode_permission(permission: Permission) -> &'static str {
    match permission {
        Permission::Granted => "granted",
        Permission::Denied => "denied",
        Permission::Undetermined => "undetermined",
    }
}

fn decode_permission(text: &str) -> Result<Permission, String> {
    match text {
        "granted" => Ok(Permission::Granted),
        "denied" => Ok(Permission::Denied),
        "undetermined" => Ok(Permission::Undetermined),
        other => Err(format!("unknown permission '{other}'")),
    }
}

pub fn encode_sources(sources: &[Source]) -> Value {
    Value::Array(
        sources
            .iter()
            .map(|source| {
                json!({
                    "windowId": source.window_id,
                    "pid": source.pid,
                    "appId": source.app_id,
                    "title": source.title,
                    "boundsPt": source.bounds_pt,
                    "visibility": source.visibility,
                })
            })
            .collect(),
    )
}

pub fn decode_sources(value: &Value) -> Result<Vec<Source>, String> {
    value
        .as_array()
        .ok_or("sources must be an array")?
        .iter()
        .map(|item| {
            Ok(Source {
                window_id: u64_field(item, "windowId")?,
                pid: u32_field(item, "pid")?,
                app_id: str_field(item, "appId")?.to_string(),
                title: str_field(item, "title")?.to_string(),
                bounds_pt: serde_field::<Rect>(item, "boundsPt")?,
                visibility: serde_field(item, "visibility")?,
            })
        })
        .collect()
}

pub fn encode_plan(plan: &BackendPlan) -> Result<Value, String> {
    let out_dir = plan
        .out_dir
        .to_str()
        .ok_or("the output directory is not valid UTF-8")?;
    Ok(json!({
        "outDir": out_dir,
        "windowId": plan.window_id,
        "pid": plan.pid,
        "fps": plan.fps,
        "scale": plan.scale,
        "codec": plan.codec.as_str(),
        "audio": plan.audio,
        "visibility": plan.visibility,
        "masking": encode_masking(&plan.masking),
    }))
}

pub fn decode_plan(value: &Value) -> Result<BackendPlan, String> {
    let codec_text = str_field(value, "codec")?;
    Ok(BackendPlan {
        out_dir: PathBuf::from(str_field(value, "outDir")?),
        window_id: u64_field(value, "windowId")?,
        pid: u32_field(value, "pid")?,
        fps: u32_field(value, "fps")?,
        scale: f64_field(value, "scale")?,
        codec: Codec::parse(codec_text).ok_or_else(|| format!("unknown codec '{codec_text}'"))?,
        audio: serde_field::<Vec<TrackKind>>(value, "audio")?,
        visibility: serde_field::<VisibilityState>(value, "visibility")?,
        masking: decode_masking(field(value, "masking")?)?,
    })
}

fn encode_masking(masking: &PixelMasking) -> Value {
    match masking {
        PixelMasking::Declared(regions) => json!({ "mode": "declared", "regions": regions }),
        PixelMasking::CaptureSafeState => json!({ "mode": "capture_safe_state" }),
        PixelMasking::Unknown => json!({ "mode": "unknown" }),
    }
}

fn decode_masking(value: &Value) -> Result<PixelMasking, String> {
    match str_field(value, "mode")? {
        "declared" => Ok(PixelMasking::Declared(serde_field::<Vec<Rect>>(
            value, "regions",
        )?)),
        "capture_safe_state" => Ok(PixelMasking::CaptureSafeState),
        "unknown" => Ok(PixelMasking::Unknown),
        other => Err(format!("unknown masking mode '{other}'")),
    }
}

pub fn encode_start_info(info: &StartInfo) -> Value {
    json!({
        "hostNsAtStart": info.host_ns_at_start,
        "width": info.width,
        "height": info.height,
        "scale": info.scale,
        "boundsPt": info.bounds_pt,
        "dpi": info.dpi,
        "codec": info.codec,
        "osBuild": info.os_build,
        "visibility": info.visibility,
    })
}

pub fn decode_start_info(value: &Value) -> Result<StartInfo, String> {
    Ok(StartInfo {
        host_ns_at_start: u64_field(value, "hostNsAtStart")?,
        width: u32_field(value, "width")?,
        height: u32_field(value, "height")?,
        scale: f64_field(value, "scale")?,
        bounds_pt: serde_field(value, "boundsPt")?,
        dpi: u32_field(value, "dpi")?,
        codec: str_field(value, "codec")?.to_string(),
        os_build: str_field(value, "osBuild")?.to_string(),
        visibility: serde_field(value, "visibility")?,
    })
}

pub fn encode_finished(finished: &Finished) -> Value {
    json!({
        "clean": finished.clean,
        "tracks": finished
            .tracks
            .iter()
            .map(|track| json!({ "kind": track.kind, "offsetMs": track.offset_ms }))
            .collect::<Vec<Value>>(),
    })
}

pub fn decode_finished(value: &Value) -> Result<Finished, String> {
    let tracks = field(value, "tracks")?
        .as_array()
        .ok_or("tracks must be an array")?
        .iter()
        .map(|track| {
            Ok(TrackOutput {
                kind: serde_field(track, "kind")?,
                offset_ms: i64_field(track, "offsetMs")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Finished {
        clean: bool_field(value, "clean")?,
        tracks,
    })
}

pub fn encode_error(error: &RecordError) -> Value {
    json!({ "code": error.code.as_str(), "message": error.message })
}

/// An error from the helper. An unknown code is a helper failure, not a refusal.
pub fn decode_error(value: &Value) -> RecordError {
    let code = value["code"]
        .as_str()
        .and_then(Code::parse)
        .unwrap_or(Code::Helper);
    let message = value["message"]
        .as_str()
        .unwrap_or("the helper refused without a message");
    RecordError::new(code, message)
}

/// The event as a complete frame, ready to write.
pub fn event_frame(event: &HelperEvent) -> Value {
    json!({ "kind": "event", "event": encode_event(event) })
}

pub fn encode_event(event: &HelperEvent) -> Value {
    match event {
        HelperEvent::Ready(sample) => json!({ "type": "ready", "sample": encode_sample(sample) }),
        HelperEvent::Frame(sample) => json!({ "type": "frame", "sample": encode_sample(sample) }),
        HelperEvent::Dropped {
            host_ns,
            count,
            interval_ms,
            reason,
        } => json!({
            "type": "dropped",
            "hostNs": host_ns,
            "count": count,
            "intervalMs": interval_ms,
            "reason": reason,
        }),
        HelperEvent::Geometry {
            host_ns,
            bounds_pt,
            scale,
            dpi,
        } => json!({
            "type": "geometry",
            "hostNs": host_ns,
            "boundsPt": bounds_pt,
            "scale": scale,
            "dpi": dpi,
        }),
        HelperEvent::Failed(failure) => {
            json!({ "type": "failed", "failure": encode_failure(failure) })
        }
        HelperEvent::Exited => json!({ "type": "exited" }),
    }
}

pub fn decode_event(value: &Value) -> Result<HelperEvent, String> {
    match str_field(value, "type")? {
        "ready" => Ok(HelperEvent::Ready(decode_sample(field(value, "sample")?)?)),
        "frame" => Ok(HelperEvent::Frame(decode_sample(field(value, "sample")?)?)),
        "dropped" => Ok(HelperEvent::Dropped {
            host_ns: u64_field(value, "hostNs")?,
            count: u32_field(value, "count")?,
            interval_ms: f64_field(value, "intervalMs")?,
            reason: serde_field::<DropReason>(value, "reason")?,
        }),
        "geometry" => Ok(HelperEvent::Geometry {
            host_ns: u64_field(value, "hostNs")?,
            bounds_pt: serde_field(value, "boundsPt")?,
            scale: f64_field(value, "scale")?,
            dpi: u32_field(value, "dpi")?,
        }),
        "failed" => Ok(HelperEvent::Failed(decode_failure(field(
            value, "failure",
        )?)?)),
        "exited" => Ok(HelperEvent::Exited),
        other => Err(format!("unknown event type '{other}'")),
    }
}

fn encode_sample(sample: &FrameSample) -> Value {
    json!({
        "index": sample.index,
        "hostNs": sample.host_ns,
        "samplePtsNs": sample.sample_pts_ns,
        "rawHash": sample.raw_hash,
    })
}

fn decode_sample(value: &Value) -> Result<FrameSample, String> {
    Ok(FrameSample {
        index: u64_field(value, "index")?,
        host_ns: u64_field(value, "hostNs")?,
        sample_pts_ns: u64_field(value, "samplePtsNs")?,
        raw_hash: str_field(value, "rawHash")?.to_string(),
    })
}

/// A failure as a tagged object. `protocol` and `io` carry their detail.
pub fn encode_failure(failure: &Failure) -> Value {
    match failure {
        Failure::Protocol(detail) => json!({ "kind": "protocol", "detail": detail }),
        Failure::Io(detail) => json!({ "kind": "io", "detail": detail }),
        other => json!({ "kind": simple_failure_kind(other) }),
    }
}

pub fn decode_failure(value: &Value) -> Result<Failure, String> {
    match str_field(value, "kind")? {
        "protocol" => Ok(Failure::Protocol(str_field(value, "detail")?.to_string())),
        "io" => Ok(Failure::Io(str_field(value, "detail")?.to_string())),
        "helper_lost" => Ok(Failure::HelperLost),
        "disk_full" => Ok(Failure::DiskFull),
        "target_exited" => Ok(Failure::TargetExited),
        "permission_revoked" => Ok(Failure::PermissionRevoked),
        "mask_geometry_unknown" => Ok(Failure::MaskGeometryUnknown),
        "window_hidden" => Ok(Failure::WindowHidden),
        "window_minimized" => Ok(Failure::WindowMinimized),
        other => Err(format!("unknown failure kind '{other}'")),
    }
}

fn simple_failure_kind(failure: &Failure) -> &'static str {
    match failure {
        Failure::HelperLost => "helper_lost",
        Failure::DiskFull => "disk_full",
        Failure::TargetExited => "target_exited",
        Failure::PermissionRevoked => "permission_revoked",
        Failure::MaskGeometryUnknown => "mask_geometry_unknown",
        Failure::WindowHidden => "window_hidden",
        Failure::WindowMinimized => "window_minimized",
        // Callers encode these two with their detail.
        Failure::Protocol(_) => "protocol",
        Failure::Io(_) => "io",
    }
}

/// The frame a helper writes last, after the final response to `stop` or `abort`.
pub fn bye_frame() -> Value {
    json!({ "kind": "bye" })
}

pub fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value, String> {
    value
        .get(key)
        .ok_or_else(|| format!("missing field '{key}'"))
}

fn str_field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    field(value, key)?
        .as_str()
        .ok_or_else(|| format!("field '{key}' must be a string"))
}

fn bool_field(value: &Value, key: &str) -> Result<bool, String> {
    field(value, key)?
        .as_bool()
        .ok_or_else(|| format!("field '{key}' must be a boolean"))
}

fn u64_field(value: &Value, key: &str) -> Result<u64, String> {
    field(value, key)?
        .as_u64()
        .ok_or_else(|| format!("field '{key}' must be an unsigned integer"))
}

fn u32_field(value: &Value, key: &str) -> Result<u32, String> {
    u32::try_from(u64_field(value, key)?).map_err(|_| format!("field '{key}' is out of range"))
}

fn i64_field(value: &Value, key: &str) -> Result<i64, String> {
    field(value, key)?
        .as_i64()
        .ok_or_else(|| format!("field '{key}' must be an integer"))
}

fn f64_field(value: &Value, key: &str) -> Result<f64, String> {
    field(value, key)?
        .as_f64()
        .ok_or_else(|| format!("field '{key}' must be a number"))
}

fn serde_field<T: DeserializeOwned>(value: &Value, key: &str) -> Result<T, String> {
    serde_json::from_value(field(value, key)?.clone())
        .map_err(|e| format!("field '{key}' is malformed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(index: u64) -> FrameSample {
        FrameSample {
            index,
            host_ns: 10 + index,
            sample_pts_ns: 20 + index,
            raw_hash: format!("blake3:{index:064x}"),
        }
    }

    #[test]
    fn every_event_round_trips() {
        let events = vec![
            HelperEvent::Ready(sample(0)),
            HelperEvent::Frame(sample(1)),
            HelperEvent::Dropped {
                host_ns: 5,
                count: 3,
                interval_ms: 16.5,
                reason: DropReason::Backpressure,
            },
            HelperEvent::Geometry {
                host_ns: 6,
                bounds_pt: Rect {
                    x: 1.0,
                    y: 2.0,
                    width: 3.0,
                    height: 4.0,
                },
                scale: 2.0,
                dpi: 192,
            },
            HelperEvent::Failed(Failure::WindowHidden),
            HelperEvent::Failed(Failure::Protocol("bad".into())),
            HelperEvent::Failed(Failure::Io("disk".into())),
            HelperEvent::Exited,
        ];
        for event in events {
            assert_eq!(decode_event(&encode_event(&event)).unwrap(), event);
        }
    }

    #[test]
    fn every_failure_round_trips_with_its_stable_reason() {
        let failures = [
            Failure::HelperLost,
            Failure::DiskFull,
            Failure::TargetExited,
            Failure::PermissionRevoked,
            Failure::MaskGeometryUnknown,
            Failure::WindowHidden,
            Failure::WindowMinimized,
        ];
        for failure in failures {
            let back = decode_failure(&encode_failure(&failure)).unwrap();
            assert_eq!(back.reason(), failure.reason());
        }
        assert_eq!(Failure::WindowHidden.reason(), "window_hidden");
        assert_eq!(Failure::WindowMinimized.reason(), "window_minimized");
    }

    #[test]
    fn plan_and_start_info_round_trip() {
        let plan = BackendPlan {
            out_dir: PathBuf::from("out/take"),
            window_id: 9,
            pid: 4,
            fps: 30,
            scale: 1.0,
            audio: vec![TrackKind::AppAudio],
            visibility: VisibilityState::OffScreenVisible,
            masking: PixelMasking::Declared(vec![Rect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
            }]),
            codec: Codec::Hevc,
        };
        assert_eq!(decode_plan(&encode_plan(&plan).unwrap()).unwrap(), plan);

        let info = StartInfo {
            host_ns_at_start: 7,
            width: 640,
            height: 360,
            scale: 1.0,
            bounds_pt: Rect {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 180.0,
            },
            dpi: 96,
            codec: "hevc".into(),
            os_build: "10.0.26300.1".into(),
            visibility: VisibilityState::OffScreenVisible,
        };
        assert_eq!(decode_start_info(&encode_start_info(&info)).unwrap(), info);
    }

    #[test]
    fn unknown_codes_and_kinds_are_refused_not_guessed() {
        assert_eq!(
            decode_error(&json!({ "code": "no_such_code", "message": "x" })).code,
            Code::Helper
        );
        assert_eq!(
            decode_error(&json!({ "code": "codec_unavailable", "message": "x" })).code,
            Code::CodecUnavailable
        );
        assert!(decode_event(&json!({ "type": "smell" })).is_err());
        assert!(decode_failure(&json!({ "kind": "smell" })).is_err());
    }
}
