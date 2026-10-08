//! record-event.v1 and record-session.v1 documents, built from the shared
//! `rightkit-contracts` types and checked by its validator.

use rightkit_contracts::types::record_event::{DropReason, EventKind, Frame, FrameDropped, PolicyRef, RecordEvent};
use rightkit_contracts::{validate, Contract};
use serde_json::{json, Value};

pub const CONTRACT_VERSION: &str = "1.1.0";
/// No masking is applied in this cut. The policy id names that honestly.
pub const REDACTION_POLICY_ID: &str = "redaction.none";
pub const REDACTION_POLICY_VERSION: &str = "1.0.0";
pub const HELPER_IDENTITY: &str = "rightkit-record-windows";

/// A record-event with only the base fields set. Each builder adds its own kind-specific field.
fn base_event(session_id: &str, seq: u64, kind: EventKind, host_ns: u64) -> RecordEvent {
    RecordEvent {
        contract: "record-event".into(),
        contract_version: CONTRACT_VERSION.into(),
        session_id: session_id.into(),
        seq,
        kind,
        input_kind: None,
        phase: None,
        effect_id: None,
        sub_event_id: None,
        step_index: 0,
        step_id: None,
        host_ns,
        timing: None,
        geometry_epoch: 0,
        dispatch: None,
        pointer: None,
        target: None,
        text: None,
        key: None,
        modifiers: Vec::new(),
        delta: None,
        admitted: None,
        denied: None,
        postcondition: None,
        frame: None,
        frame_dropped: None,
        intruders: Vec::new(),
        redaction_policy: PolicyRef {
            policy_id: REDACTION_POLICY_ID.into(),
            policy_version: REDACTION_POLICY_VERSION.into(),
        },
    }
}

/// The shared types serialise absent options as null. The schema wants the
/// keys absent, so drop the top-level nulls.
fn to_value(event: &RecordEvent) -> Value {
    let mut value = serde_json::to_value(event).unwrap_or_default();
    if let Some(map) = value.as_object_mut() {
        map.retain(|_, v| !v.is_null());
    }
    value
}

/// `session_start`: written before the first frame, so every bundle is bracketed.
pub fn session_start(session_id: &str, seq: u64, host_ns: u64) -> Value {
    to_value(&base_event(session_id, seq, EventKind::SessionStart, host_ns))
}

/// `session_end`: written on every exit path, including refusal and error.
pub fn session_end(session_id: &str, seq: u64, host_ns: u64) -> Value {
    to_value(&base_event(session_id, seq, EventKind::SessionEnd, host_ns))
}

/// A `frame_ready` event for one kept frame.
pub fn frame_ready(session_id: &str, seq: u64, index: u64, host_ns: u64, sample_pts_ns: u64) -> Value {
    let mut event = base_event(session_id, seq, EventKind::FrameReady, host_ns);
    event.frame = Some(Frame { index, sample_pts_ns });
    to_value(&event)
}

/// A `frame_dropped` event for one aggregated run of drops. `interval_ms` is the
/// time the run spans, so one dropped frame reads as one frame interval.
pub fn frame_dropped(session_id: &str, seq: u64, host_ns: u64, count: u32, interval_ms: f64, reason: DropReason) -> Value {
    let mut event = base_event(session_id, seq, EventKind::FrameDropped, host_ns);
    event.frame_dropped = Some(FrameDropped { count, interval_ms, reason });
    to_value(&event)
}

pub fn validate_event(value: &Value) -> Result<(), String> {
    validate(Contract::RecordEvent, value).map_err(|e| e.to_string())
}

pub struct ClockDraft {
    pub host_ns_at_start: u64,
    pub first_host_ns: u64,
}

pub struct DraftInput<'a> {
    pub bundle_id: &'a str,
    pub created_at: &'a str,
    pub reason: &'a str,
    pub os_build: &'a str,
    pub fps: u32,
    /// record-session `capture.codec` (`h264` or `hevc`). Unset for the bmp debug path.
    pub codec: Option<&'a str>,
    pub scale: f64,
    pub width: u32,
    pub height: u32,
    /// record-session `capture.visibilityState`, which the schema requires:
    /// `on_screen_visible` when the window is on a monitor, else `off_screen_visible`.
    pub visibility_state: &'a str,
    /// x, y, width, height in points.
    pub bounds_pt: [f64; 4],
    pub dpi: u32,
    pub clock: Option<ClockDraft>,
    pub frames_count: u64,
    pub frames_dropped: u64,
    pub events_count: u64,
}

/// The record-session draft. It is always `partial_quarantined`. It omits the
/// keys this cut cannot produce (scenario, target, files, dependencies,
/// redaction), so it does not validate as a complete bundle.
pub fn session_draft(d: &DraftInput) -> Value {
    let mut capture = json!({
        "fps": d.fps,
        "scale": d.scale,
        "width": d.width,
        "height": d.height,
        "scope": "app_surfaces",
        "displayOptIn": false,
        "visibilityState": d.visibility_state,
    });
    if let Some(codec) = d.codec {
        capture["codec"] = json!(codec);
    }
    let [x, y, width, height] = d.bounds_pt;
    let mut doc = json!({
        "contract": "record-session",
        "contractVersion": CONTRACT_VERSION,
        "bundleId": d.bundle_id,
        "createdAt": d.created_at,
        "status": "partial_quarantined",
        "reason": d.reason,
        "recorder": {
            "engine": "rightkit-record",
            "version": env!("CARGO_PKG_VERSION"),
            "os": "windows",
            "osBuild": d.os_build,
            "helper": { "identity": HELPER_IDENTITY, "signed": false },
        },
        "capture": capture,
        "geometryEpochs": [{
            "epoch": 0,
            "startMs": 0,
            "boundsPt": { "x": x, "y": y, "width": width, "height": height },
            "scale": d.scale,
            "dpi": d.dpi,
        }],
        "frames": { "count": d.frames_count, "droppedCount": d.frames_dropped },
        "events": { "count": d.events_count, "contract": "record-event", "contractVersion": CONTRACT_VERSION },
        "donorDirectoryRequired": false,
    });
    if let Some(clock) = &d.clock {
        doc["clock"] = json!({
            "hostClock": "qpc",
            "hostNsAtStart": clock.host_ns_at_start,
            "firstAcceptedFrame": {
                "frameIndex": 0,
                "hostNs": clock.first_host_ns,
                "samplePtsNs": 0,
            },
            "mapping": {
                "hostToSamplePtsOffsetNs": -(clock.first_host_ns as i64),
                "samplePtsToEncodedPtsOffsetNs": 0,
                "timescale": 1_000_000_000u64,
            },
        });
    }
    doc
}

/// Runs the shared validator. The error names the first schema violation.
pub fn validate_draft(doc: &Value) -> Result<(), String> {
    validate(Contract::RecordSession, doc).map_err(|e| e.to_string())
}

/// UTC timestamp `YYYY-MM-DDTHH:MM:SSZ`, matching the record-session timestamp pattern.
pub fn iso8601_utc(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400);
    // Days-from-civil inverse (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_contracts::parse;
    use rightkit_contracts::types::RecordEvent;

    fn draft(visibility: &str, codec: Option<&str>) -> Value {
        session_draft(&DraftInput {
            bundle_id: "bundle.win-1",
            created_at: "2026-10-08T12:00:00Z",
            reason: "test",
            os_build: "26300.9550",
            fps: 60,
            codec,
            scale: 2.5,
            width: 1964,
            height: 1282,
            visibility_state: visibility,
            bounds_pt: [-10_000.0, -10_000.0, 800.0, 520.0],
            dpi: 240,
            clock: Some(ClockDraft { host_ns_at_start: 100, first_host_ns: 150 }),
            frames_count: 10,
            frames_dropped: 5,
            events_count: 12,
        })
    }

    #[test]
    fn frame_ready_validates_against_record_event_v1() {
        let value = frame_ready("bundle.win-1", 4, 2, 9_000_000_000, 16_666_666);
        validate_event(&value).expect("frame_ready must validate");
        let typed: RecordEvent = parse(Contract::RecordEvent, &value.to_string()).expect("typed parse");
        assert_eq!(typed.kind, EventKind::FrameReady);
        assert_eq!(typed.frame.map(|f| (f.index, f.sample_pts_ns)), Some((2, 16_666_666)));
    }

    #[test]
    fn frame_ready_has_no_null_top_level_fields() {
        let value = frame_ready("bundle.win-1", 0, 0, 1, 0);
        let map = value.as_object().unwrap();
        assert!(map.values().all(|v| !v.is_null()));
        assert!(!map.contains_key("inputKind"));
        assert!(!map.contains_key("frameDropped"));
    }

    #[test]
    fn session_start_and_end_have_the_v1_1_shape() {
        for (value, kind) in [
            (session_start("bundle.win-1", 0, 100), "session_start"),
            (session_end("bundle.win-1", 9, 200), "session_end"),
        ] {
            validate_event(&value).unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert_eq!(value["kind"], kind);
            assert_eq!(value["contractVersion"], "1.1.0");
            assert_eq!(value["stepIndex"], 0);
            assert!(value.get("frame").is_none() && value.get("frameDropped").is_none());
            let typed: RecordEvent = parse(Contract::RecordEvent, &value.to_string()).expect("typed parse");
            assert!(matches!(typed.kind, EventKind::SessionStart | EventKind::SessionEnd));
        }
        assert_eq!(session_end("bundle.win-1", 9, 200)["seq"], 9);
    }

    #[test]
    fn frame_dropped_validates_for_every_reason() {
        for (reason, name) in [
            (DropReason::Backpressure, "backpressure"),
            (DropReason::Encoder, "encoder"),
            (DropReason::CaptureGap, "capture_gap"),
            (DropReason::Unknown, "unknown"),
        ] {
            let value = frame_dropped("bundle.win-1", 3, 300, 2, 33.33, reason);
            validate_event(&value).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(value["kind"], "frame_dropped");
            assert_eq!(value["frameDropped"]["reason"], name);
            assert_eq!(value["frameDropped"]["count"], 2);
            assert!((value["frameDropped"]["intervalMs"].as_f64().unwrap() - 33.33).abs() < 1e-9);
            assert!(value.get("frame").is_none());
            let typed: RecordEvent = parse(Contract::RecordEvent, &value.to_string()).expect("typed parse");
            assert_eq!(typed.frame_dropped.map(|f| (f.count, f.reason)), Some((2, reason)));
        }
    }

    #[test]
    fn frame_dropped_with_zero_count_is_rejected_by_the_validator() {
        let value = frame_dropped("bundle.win-1", 3, 300, 0, 0.0, DropReason::Unknown);
        assert!(validate_event(&value).is_err(), "count must be at least 1");
    }

    #[test]
    fn session_draft_is_marked_partial_and_not_claimed_valid() {
        let doc = draft("on_screen_visible", Some("h264"));
        assert_eq!(doc["status"], "partial_quarantined");
        assert_eq!(doc["contractVersion"], "1.1.0");
        assert_eq!(doc["capture"]["codec"], "h264");
        assert_eq!(doc["capture"]["visibilityState"], "on_screen_visible");
        assert_eq!(doc["events"]["count"], 12);
        assert_eq!(doc["clock"]["mapping"]["hostToSamplePtsOffsetNs"], -150);
        assert!(validate_draft(&doc).is_err(), "draft lacks scenario/target/files and must not validate");
    }

    #[test]
    fn session_draft_reports_off_screen_visibility_when_not_on_a_monitor() {
        let doc = draft("off_screen_visible", Some("hevc"));
        assert_eq!(doc["capture"]["visibilityState"], "off_screen_visible");
        assert_eq!(doc["capture"]["codec"], "hevc");
    }

    #[test]
    fn session_draft_leaves_codec_unset_for_bmp() {
        let doc = draft("on_screen_visible", None);
        assert!(doc["capture"].get("codec").is_none());
    }

    #[test]
    fn iso8601_known_values() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(951_868_800), "2000-03-01T00:00:00Z");
        assert_eq!(iso8601_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601_utc(1_791_417_600), "2026-10-08T00:00:00Z");
    }
}
