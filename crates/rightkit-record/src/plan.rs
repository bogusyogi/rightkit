//! The request a caller makes to start a take, its validation, and the JSON
//! documents the core derives from it.

use std::collections::BTreeSet;
use std::path::PathBuf;

use rightkit_contracts::types::record_session::{Dependency, Surface, TrackKind, VisibilityState};
use rightkit_contracts::types::Rect;
use serde_json::{json, Value};

use crate::backend::Codec;
use crate::error::{Code, RecordError, Result};
use crate::redact::{HashKey, KeyHandle, RedactionPolicy};

/// How the frame pixels are protected before encoding.
#[derive(Debug, Clone, PartialEq)]
pub enum PixelMasking {
    /// Declared secret regions, masked before the encoder sees them.
    Declared(Vec<Rect>),
    /// The application is in a capture-safe state enforced before readiness.
    CaptureSafeState,
    /// The helper cannot establish the mask geometry. The take is refused.
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetSpec {
    pub app_id: String,
    pub window_id: u64,
    pub pid: u32,
    /// Native surfaces the take may show, with window lineage.
    pub surfaces: Vec<Surface>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioStep {
    pub step_id: String,
    pub label: String,
    pub say: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioSpec {
    pub scenario_id: String,
    pub steps: Vec<ScenarioStep>,
}

/// `record.start {target, fps, scale, cursor, audio, out}`, plus the scenario,
/// visibility, masking and redaction the bundle must carry.
#[derive(Debug, Clone, PartialEq)]
pub struct StartRequest {
    pub target: TargetSpec,
    pub scenario: ScenarioSpec,
    pub fps: u32,
    pub scale: f64,
    pub codec: Codec,
    pub cursor: bool,
    pub audio: Vec<TrackKind>,
    pub visibility: VisibilityState,
    pub masking: PixelMasking,
    pub redaction: RedactionPolicy,
    /// Where the per-installation key for `hash` fields comes from. Read only when
    /// the policy hashes a field. Defaults to the OS credential store.
    pub key_source: KeyHandle,
    /// The bundle directory to create. It must not exist.
    pub out: PathBuf,
    pub dependencies: Vec<Dependency>,
}

impl StartRequest {
    /// A request with the recording defaults: 60 fps, scale 1, H.264, no cursor,
    /// no audio, off-screen-visible, no declared masks, the strict redaction policy.
    pub fn new(target: TargetSpec, scenario: ScenarioSpec, out: impl Into<PathBuf>) -> Self {
        StartRequest {
            target,
            scenario,
            fps: 60,
            scale: 1.0,
            codec: Codec::H264,
            cursor: false,
            audio: Vec::new(),
            visibility: VisibilityState::OffScreenVisible,
            masking: PixelMasking::Declared(Vec::new()),
            redaction: RedactionPolicy::strict(),
            key_source: KeyHandle::system(),
            out: out.into(),
            dependencies: Vec::new(),
        }
    }
}

/// An id a bundle, app or step may use: letters, digits and `_.:-`, starting
/// with an alphanumeric, at most 128 bytes. It matches the contract's id pattern.
pub fn valid_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

pub fn validate(request: &StartRequest) -> Result<()> {
    let bad = |message: &str| RecordError::new(Code::ArgInvalid, message);
    if !matches!(request.fps, 30 | 60) {
        return Err(bad("fps must be 30 or 60"));
    }
    if !(request.scale.is_finite() && request.scale > 0.0) {
        return Err(bad("scale must be a positive number"));
    }
    if request.cursor {
        return Err(bad(
            "cursor pixels are excluded from every recording; cursor telemetry is recorded as events",
        ));
    }
    if !valid_id(&request.target.app_id) {
        return Err(bad("target app_id is not a valid id"));
    }
    if request.target.window_id == 0 {
        return Err(bad("target window_id must be non-zero"));
    }
    if request.target.surfaces.is_empty() {
        return Err(bad("target must list at least one surface"));
    }
    if !valid_id(&request.scenario.scenario_id) {
        return Err(bad("scenario_id is not a valid id"));
    }
    let steps = &request.scenario.steps;
    if steps.is_empty() || steps.len() > 200 {
        return Err(bad("a scenario has between 1 and 200 steps"));
    }
    let mut seen = BTreeSet::new();
    for step in steps {
        if !valid_id(&step.step_id) || !seen.insert(step.step_id.as_str()) {
            return Err(bad("step ids must be valid and unique"));
        }
    }
    for (index, kind) in request.audio.iter().enumerate() {
        if request.audio[..index].contains(kind) {
            return Err(bad("each audio track may be requested once"));
        }
    }
    if let PixelMasking::Declared(regions) = &request.masking {
        if regions.iter().any(|r| !(r.width > 0.0 && r.height > 0.0)) {
            return Err(bad("declared mask regions must have a positive size"));
        }
    }
    if request.out.exists() {
        return Err(RecordError::new(
            Code::OutExists,
            format!("{} already exists", request.out.display()),
        ));
    }
    Ok(())
}

pub(crate) fn scenario_json(
    spec: &ScenarioSpec,
    redaction: &RedactionPolicy,
    key: Option<&HashKey>,
) -> Value {
    let steps: Vec<Value> = spec
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let mut value = json!({ "index": index, "stepId": step.step_id, "label": step.label });
            if let Some(say) = &step.say {
                value["say"] = json!(say);
            }
            value
        })
        .collect();
    json!({
        "scenarioId": spec.scenario_id,
        "steps": steps,
        "redaction": redaction.to_json_with_key(key),
    })
}

/// `None` for an unknown mask geometry, which must never reach a bundle.
pub(crate) fn masking_json(masking: &PixelMasking) -> Option<Value> {
    match masking {
        PixelMasking::Declared(regions) => {
            Some(json!({ "mode": "declared_regions", "regionCount": regions.len() }))
        }
        PixelMasking::CaptureSafeState => {
            Some(json!({ "mode": "capture_safe_state", "regionCount": 0 }))
        }
        PixelMasking::Unknown => None,
    }
}

pub(crate) fn target_json(target: &TargetSpec) -> Value {
    json!({
        "appId": target.app_id,
        "surfaces": serde_json::to_value(&target.surfaces).unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> TargetSpec {
        TargetSpec {
            app_id: "cutright".into(),
            window_id: 42,
            pid: 7,
            surfaces: vec![],
        }
    }

    fn scenario() -> ScenarioSpec {
        ScenarioSpec {
            scenario_id: "demo.one".into(),
            steps: vec![ScenarioStep {
                step_id: "step-1".into(),
                label: "Open".into(),
                say: None,
            }],
        }
    }

    #[test]
    fn ids_follow_the_contract_pattern() {
        assert!(valid_id("bundle.1-a_b:c"));
        assert!(!valid_id(""));
        assert!(!valid_id("-leading"));
        assert!(!valid_id("has space"));
        assert!(!valid_id(&"a".repeat(129)));
    }

    #[test]
    fn rejects_cursor_pixels_and_bad_fps() {
        let out = std::env::temp_dir().join("rk-record-plan-unit-missing");
        let mut request = StartRequest::new(target(), scenario(), &out);
        request.fps = 24;
        assert!(validate(&request).is_err());
        request.fps = 60;
        request.cursor = true;
        assert!(validate(&request).is_err());
    }
}
