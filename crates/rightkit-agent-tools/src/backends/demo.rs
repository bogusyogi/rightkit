//! `demo.*` tools: the agent side of CutRight's agent demo mode.
//!
//! Compiled only with the non-default `demo` feature. A default or release build
//! of this crate contains none of this code. The wire contract is defined here
//! as Rust types and documented as JSON Schema in `docs/demo-contract.md`.
//!
//! The host (CutRight) owns consent, the session state machine, the policy and
//! the recorder. This module is a thin client: it connects to the host's
//! `rightkit.agent.demo` service over `rightkit-daemon`, checks each call against
//! the contract, and returns the host's answer. It never drives an app itself.
//!
//! Session rules the contract carries (enforced by the host):
//! * consent is asked per session and never remembered;
//! * `demo.request` returns a per-session token; every later session call carries it;
//! * `demo.end` invalidates the token;
//! * a target that cannot report a capture-safe state is refused, never masked.

use crate::backends::control::CLIENT_APP;
use crate::spec::*;
use rightkit_daemon::{Client, ServiceError, Suite};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

/// Daemon service name the host registers for demo sessions.
pub const DEMO_SERVICE: &str = "rightkit.agent.demo";
/// `demo.request` blocks until a person answers the consent dialog.
pub const CONSENT_TIMEOUT: Duration = Duration::from_secs(600);
/// Every other demo call.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Most steps one `demo.batch` may carry.
pub const MAX_BATCH_STEPS: usize = 20;

/// The per-session token `demo.request` issues. Opaque to the agent. Debug output is redacted
/// so a token cannot reach a log through `{:?}`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionToken(String);

impl SessionToken {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}

/// Typed failure codes the host returns. Each maps to one `ToolError.code` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoErrorCode {
    /// The user has the demo feature switched off in the host.
    DemoDisabled,
    /// The person refused consent. Nothing ran.
    ConsentDenied,
    /// Token missing, unknown, or from an ended session.
    InvalidToken,
    /// The session ended normally (`demo.end`).
    SessionEnded,
    /// The session ended by the kill control. Only a fresh consent starts another.
    SessionKilled,
    /// The app id is not in the host's demo catalog.
    UnknownApp,
    /// The session is not armed (consent given, OS grants missing or not yet started).
    NotArmed,
    /// A required OS permission is not granted.
    PermissionMissing,
    /// The host's gates refused the target: secure or credential field, a denied app, or the host itself.
    TargetBlocked,
    /// The target cannot report a capture-safe state. The host refuses; it never masks.
    CaptureUnsafe,
    /// Arguments fail the contract.
    InvalidArguments,
    /// The host's demo service is not running.
    ServiceUnavailable,
    /// The host answered with a shape the contract does not allow.
    MalformedResponse,
}

impl DemoErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DemoDisabled => "demo_disabled",
            Self::ConsentDenied => "consent_denied",
            Self::InvalidToken => "invalid_token",
            Self::SessionEnded => "session_ended",
            Self::SessionKilled => "session_killed",
            Self::UnknownApp => "unknown_app",
            Self::NotArmed => "not_armed",
            Self::PermissionMissing => "permission_missing",
            Self::TargetBlocked => "target_blocked",
            Self::CaptureUnsafe => "capture_unsafe",
            Self::InvalidArguments => "invalid_arguments",
            Self::ServiceUnavailable => "service_unavailable",
            Self::MalformedResponse => "malformed_response",
        }
    }
}

/// `demo.request` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestParams {
    pub app: String,
    /// The agent's own name. The host shows it next to the verified process facts.
    pub agent_name: String,
}

/// `demo.request` result, after consent is approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestResult {
    pub session: String,
    pub token: SessionToken,
    pub app: String,
}

/// One catalog entry in `demo.apps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DemoApp {
    pub id: String,
    pub name: String,
}

/// `demo.apps` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppsResult {
    pub apps: Vec<DemoApp>,
}

/// The operations an agent may perform on an element. There is no eval, DOM, clipboard or
/// generic screenshot operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElementOp {
    Click,
    Type,
    Press,
}

/// One element in a snapshot. `ref` is valid only for the session that issued it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotElement {
    #[serde(rename = "ref")]
    pub element_ref: String,
    pub role: String,
    pub label: String,
    pub ops: Vec<ElementOp>,
}

/// The only capture state the host returns. Any other state is an error (`capture_unsafe`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    Safe,
}

/// `demo.snapshot` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotResult {
    pub capture_state: CaptureState,
    pub elements: Vec<SnapshotElement>,
}

/// One action step. Shared by `demo.act` (flattened) and each `demo.batch` step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActStep {
    #[serde(rename = "ref")]
    pub element_ref: String,
    pub op: ElementOp,
    /// Required for `type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Required for `press`, e.g. `enter` or `cmd+s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// `demo.act` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActParams {
    pub token: SessionToken,
    #[serde(flatten)]
    pub step: ActStep,
}

/// `demo.act` result. The receipt is content-free and is also written to the bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActResult {
    pub receipt_id: String,
}

/// `demo.batch` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchParams {
    pub token: SessionToken,
    pub steps: Vec<ActStep>,
}

/// `demo.batch` result. The host stops at the first failing step and returns `Ok` with
/// `failed_step` set, the same way `right-ctl <app> batch` reports failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchResult {
    pub completed: usize,
    pub results: Vec<ActResult>,
    pub failed_step: Option<usize>,
    pub error_code: Option<DemoErrorCode>,
}

/// How the recording ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleState {
    Complete,
    Killed,
}

/// `demo.end` result. The bundle is local and sealed read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndResult {
    pub ended: bool,
    pub bundle_id: String,
    pub bundle_state: BundleState,
}

/// The six demo methods. The tool name and the daemon method name are the same string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Request,
    Apps,
    Snapshot,
    Act,
    Batch,
    End,
}

pub const METHODS: [Method; 6] = [
    Method::Request,
    Method::Apps,
    Method::Snapshot,
    Method::Act,
    Method::Batch,
    Method::End,
];

impl Method {
    pub fn name(self) -> &'static str {
        match self {
            Method::Request => "demo.request",
            Method::Apps => "demo.apps",
            Method::Snapshot => "demo.snapshot",
            Method::Act => "demo.act",
            Method::Batch => "demo.batch",
            Method::End => "demo.end",
        }
    }

    pub fn effect(self) -> Effect {
        match self {
            Method::Request | Method::End => Effect::Launch,
            Method::Apps | Method::Snapshot => Effect::Observe,
            Method::Act | Method::Batch => Effect::Act,
        }
    }

    /// Every method except `demo.request` and `demo.apps` carries the session token. `apps`
    /// lists the catalog and needs no session.
    pub fn needs_token(self) -> bool {
        !matches!(self, Method::Request | Method::Apps)
    }

    pub fn timeout(self) -> Duration {
        match self {
            Method::Request => CONSENT_TIMEOUT,
            _ => CALL_TIMEOUT,
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Method::Request => "Ask the person to consent to an agent demo of one catalog app. Blocks until they answer. On approval returns the session token every later demo call must carry.",
            Method::Apps => "List the apps the demo catalog allows.",
            Method::Snapshot => "List the elements of the demo target that can be acted on, each with a ref valid for this session. Refused unless the target reports a capture-safe state.",
            Method::Act => "Click, type into, or press a key on one element by ref. Every host gate applies; a refused action names the gate in its error code.",
            Method::Batch => "Run up to 20 act steps in one call, stopping at the first failure.",
            Method::End => "End the demo session, seal the local bundle, and invalidate the token.",
        }
    }

    pub fn input_schema(self) -> Value {
        let token =
            json!({"type": "string", "description": "The session token from demo.request."});
        let step = json!({
            "type": "object",
            "properties": {
                "ref": {"type": "string"},
                "op": {"type": "string", "enum": ["click", "type", "press"]},
                "text": {"type": "string"},
                "key": {"type": "string"}
            },
            "required": ["ref", "op"]
        });
        match self {
            Method::Request => json!({
                "type": "object",
                "properties": {"app": {"type": "string"}, "agent_name": {"type": "string"}},
                "required": ["app", "agent_name"]
            }),
            Method::Apps => json!({"type": "object", "properties": {}}),
            Method::Snapshot | Method::End => json!({
                "type": "object",
                "properties": {"token": token},
                "required": ["token"]
            }),
            Method::Act => {
                let mut s = step.clone();
                s["properties"]["token"] = token;
                s["required"] = json!(["token", "ref", "op"]);
                s
            }
            Method::Batch => json!({
                "type": "object",
                "properties": {
                    "token": token,
                    "steps": {"type": "array", "items": step, "maxItems": MAX_BATCH_STEPS}
                },
                "required": ["token", "steps"]
            }),
        }
    }

    /// The typed response shape this method's result must match.
    fn check_response(self, v: &Value) -> Result<(), ToolError> {
        // A host that reports any capture state other than `safe` has refused the target. The
        // client surfaces that refusal as `capture_unsafe`, never as a masked success.
        if self == Method::Snapshot && v.get("capture_state").is_some_and(|s| s != "safe") {
            return Err(ToolError::new(
                DemoErrorCode::CaptureUnsafe.as_str(),
                "the target cannot report a capture-safe state; refused, not masked",
            ));
        }
        let ok = match self {
            Method::Request => shape::<RequestResult>(v),
            Method::Apps => shape::<AppsResult>(v),
            Method::Snapshot => shape::<SnapshotResult>(v),
            Method::Act => shape::<ActResult>(v),
            Method::Batch => shape::<BatchResult>(v),
            Method::End => shape::<EndResult>(v),
        };
        ok.map_err(|e| {
            ToolError::new(
                DemoErrorCode::MalformedResponse.as_str(),
                format!("{}: {e}", self.name()),
            )
        })
    }
}

fn shape<T: DeserializeOwned>(v: &Value) -> Result<(), serde_json::Error> {
    T::deserialize(v).map(|_: T| ())
}

/// Argument keys whose values are always text. A `k=v` value for one of these is never
/// JSON-parsed, so `text=1e3` types `1e3`, not `1000.0`.
const TEXT_KEYS: &[&str] = &["token", "ref", "text", "key", "app", "agent_name"];

/// CLI arguments for a demo tool: one JSON object, or `key=value` pairs. Text keys keep their
/// value verbatim. Other values parse as JSON when they are JSON, and stay strings otherwise.
pub fn parse_demo_args(rest: &[String]) -> Result<Value, ToolError> {
    let invalid = |m: String| ToolError::new(DemoErrorCode::InvalidArguments.as_str(), m);
    if rest.len() == 1 && rest[0].trim_start().starts_with('{') {
        return serde_json::from_str(&rest[0]).map_err(|e| invalid(format!("bad JSON: {e}")));
    }
    let mut m = serde_json::Map::new();
    for r in rest {
        let (k, v) = r
            .split_once('=')
            .ok_or_else(|| invalid(format!("expected key=value or a JSON object, got `{r}`")))?;
        let value = if TEXT_KEYS.contains(&k) {
            Value::String(v.to_string())
        } else {
            serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.to_string()))
        };
        m.insert(k.to_string(), value);
    }
    Ok(Value::Object(m))
}

/// The demo fixture app: a button, a text field and a password field, used by the released-build
/// acceptance test. Defined only with the `demo` feature. It is not a production catalog entry,
/// and a default build cannot name it.
pub const FIXTURE_APP_ID: &str = "app.orthiclabs.demo-fixture";

/// Client-side checks that the schema cannot express. The host checks them again.
pub fn check_batch_size(args: &Value) -> Result<(), ToolError> {
    let n = args["steps"].as_array().map_or(0, Vec::len);
    if n == 0 || n > MAX_BATCH_STEPS {
        return Err(ToolError::new(
            DemoErrorCode::InvalidArguments.as_str(),
            format!("demo.batch takes 1 to {MAX_BATCH_STEPS} steps, got {n}"),
        ));
    }
    Ok(())
}

struct DemoTool {
    spec: ToolSpec,
    suite: Suite,
    method: Method,
}

fn map_err(e: ServiceError) -> ToolError {
    ToolError {
        code: e.code,
        message: e.message,
        retryable: e.retryable,
    }
}

impl Tool for DemoTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn call(&self, _ctx: &CallCtx, args: Value) -> Result<Value, ToolError> {
        if self.method == Method::Batch {
            check_batch_size(&args)?;
        }
        let client = Client::connect_with(
            &self.suite,
            DEMO_SERVICE,
            CLIENT_APP,
            Some(self.method.timeout()),
        )
        .map_err(map_err)?;
        let v = client.call(self.method.name(), args).map_err(map_err)?;
        self.method.check_response(&v)?;
        Ok(v)
    }
}

/// The six `demo.*` tools, each with its schema and effect class, on `suite`.
pub fn demo_tools(suite: &Suite) -> Vec<Arc<dyn Tool>> {
    METHODS
        .iter()
        .map(|m| {
            Arc::new(DemoTool {
                spec: ToolSpec::new(m.name(), m.description(), m.effect(), m.input_schema()),
                suite: suite.clone(),
                method: *m,
            }) as Arc<dyn Tool>
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exposure;

    fn names() -> Vec<&'static str> {
        METHODS.iter().map(|m| m.name()).collect()
    }

    #[test]
    fn exactly_the_six_contract_tools_exist() {
        assert_eq!(
            names(),
            [
                "demo.request",
                "demo.apps",
                "demo.snapshot",
                "demo.act",
                "demo.batch",
                "demo.end"
            ]
        );
    }

    #[test]
    fn no_demo_tool_name_is_forbidden_by_the_exposure_rules() {
        for m in METHODS {
            assert!(exposure::forbidden(m.name()).is_none(), "{}", m.name());
        }
    }

    #[test]
    fn only_request_and_apps_are_session_free() {
        for m in METHODS {
            let free = matches!(m, Method::Request | Method::Apps);
            assert_eq!(m.needs_token(), !free, "{}", m.name());
            let schema = m.input_schema();
            let required = schema["required"].as_array().cloned().unwrap_or_default();
            let has_token = required.iter().any(|r| r == "token");
            assert_eq!(has_token, m.needs_token(), "{}", m.name());
        }
    }

    #[test]
    fn effects_match_the_contract() {
        assert_eq!(Method::Request.effect(), Effect::Launch);
        assert_eq!(Method::End.effect(), Effect::Launch);
        assert_eq!(Method::Apps.effect(), Effect::Observe);
        assert_eq!(Method::Snapshot.effect(), Effect::Observe);
        assert_eq!(Method::Act.effect(), Effect::Act);
        assert_eq!(Method::Batch.effect(), Effect::Act);
    }

    #[test]
    fn request_needs_app_and_agent_name() {
        let s = Method::Request.input_schema();
        assert!(validate_args(&s, &json!({"app": "x", "agent_name": "a"})).is_ok());
        assert!(validate_args(&s, &json!({"app": "x"})).is_err());
    }

    #[test]
    fn act_has_no_generic_operation() {
        let s = Method::Act.input_schema();
        assert!(validate_args(&s, &json!({"token": "t", "ref": "r", "op": "click"})).is_ok());
        assert!(validate_args(&s, &json!({"token": "t", "ref": "r", "op": "eval"})).is_err());
        assert!(validate_args(&s, &json!({"ref": "r", "op": "click"})).is_err());
    }

    #[test]
    fn token_is_redacted_in_debug_and_round_trips_as_a_string() {
        let t = SessionToken::new("s3cr3t");
        assert!(!format!("{t:?}").contains("s3cr3t"));
        assert_eq!(serde_json::to_value(&t).unwrap(), json!("s3cr3t"));
        assert_eq!(t.expose(), "s3cr3t");
    }

    #[test]
    fn error_codes_are_distinct_snake_case_and_match_serde() {
        let all = [
            DemoErrorCode::DemoDisabled,
            DemoErrorCode::ConsentDenied,
            DemoErrorCode::InvalidToken,
            DemoErrorCode::SessionEnded,
            DemoErrorCode::SessionKilled,
            DemoErrorCode::UnknownApp,
            DemoErrorCode::NotArmed,
            DemoErrorCode::PermissionMissing,
            DemoErrorCode::TargetBlocked,
            DemoErrorCode::CaptureUnsafe,
            DemoErrorCode::InvalidArguments,
            DemoErrorCode::ServiceUnavailable,
            DemoErrorCode::MalformedResponse,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for c in all {
            let s = c.as_str();
            assert!(seen.insert(s), "duplicate {s}");
            assert!(
                s.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'),
                "{s}"
            );
            assert_eq!(serde_json::to_value(c).unwrap(), json!(s));
            let back: DemoErrorCode = serde_json::from_value(json!(s)).unwrap();
            assert_eq!(back, c);
        }
    }

    #[test]
    fn snapshot_accepts_only_the_safe_capture_state() {
        let ok = json!({"capture_state": "safe", "elements": [
            {"ref": "e1", "role": "button", "label": "Save", "ops": ["click"]}
        ]});
        assert!(Method::Snapshot.check_response(&ok).is_ok());
        let masked = json!({"capture_state": "masked", "elements": []});
        let err = Method::Snapshot.check_response(&masked).unwrap_err();
        assert_eq!(err.code, "capture_unsafe");
        let unknown = json!({"capture_state": "unknown", "elements": []});
        assert_eq!(
            Method::Snapshot.check_response(&unknown).unwrap_err().code,
            "capture_unsafe"
        );
        let no_state = json!({"elements": []});
        assert_eq!(
            Method::Snapshot.check_response(&no_state).unwrap_err().code,
            "malformed_response"
        );
    }

    #[test]
    fn a_host_refusal_of_a_snapshot_is_not_a_success() {
        // The contract has no success value except `safe`: any other state fails closed.
        for state in ["masked", "redacted", "", "SAFE"] {
            let v = json!({"capture_state": state, "elements": []});
            assert!(Method::Snapshot.check_response(&v).is_err(), "{state}");
        }
    }

    #[test]
    fn text_keys_keep_their_value_verbatim() {
        let args: Vec<String> = ["token=123", "text=1e3", "ref=e1", "op=type", "count=2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let v = parse_demo_args(&args).unwrap();
        assert_eq!(v["token"], json!("123"));
        assert_eq!(v["text"], json!("1e3"));
        assert_eq!(v["ref"], json!("e1"));
        assert_eq!(v["op"], json!("type"));
        assert_eq!(v["count"], json!(2));
    }

    #[test]
    fn one_json_object_is_taken_as_is() {
        let args = vec![r#"{"token":"t","ref":"e","op":"click"}"#.to_string()];
        let v = parse_demo_args(&args).unwrap();
        assert_eq!(v, json!({"token": "t", "ref": "e", "op": "click"}));
        let bad = vec!["{not json".to_string()];
        assert_eq!(parse_demo_args(&bad).unwrap_err().code, "invalid_arguments");
        let no_eq = vec!["token".to_string()];
        assert_eq!(
            parse_demo_args(&no_eq).unwrap_err().code,
            "invalid_arguments"
        );
    }

    #[test]
    fn fixture_id_is_defined_only_under_this_feature() {
        // This module compiles only with the `demo` feature; the default build has no such item
        // (see tests/demo_absent.rs).
        assert_eq!(FIXTURE_APP_ID, "app.orthiclabs.demo-fixture");
    }

    #[test]
    fn end_result_bundle_state_is_closed() {
        let ok = json!({"ended": true, "bundle_id": "b1", "bundle_state": "killed"});
        assert!(Method::End.check_response(&ok).is_ok());
        let bad = json!({"ended": true, "bundle_id": "b1", "bundle_state": "uploaded"});
        assert!(Method::End.check_response(&bad).is_err());
    }

    #[test]
    fn batch_takes_one_to_twenty_steps() {
        assert!(check_batch_size(&json!({"steps": []})).is_err());
        let one = json!({"steps": [{"ref": "e", "op": "click"}]});
        assert!(check_batch_size(&one).is_ok());
        let many: Vec<Value> = (0..=MAX_BATCH_STEPS)
            .map(|_| json!({"ref": "e", "op": "click"}))
            .collect();
        assert!(check_batch_size(&json!({"steps": many})).is_err());
    }

    #[test]
    fn act_params_flatten_the_step_with_its_token() {
        let p: ActParams =
            serde_json::from_value(json!({"token": "t", "ref": "e", "op": "type", "text": "hi"}))
                .unwrap();
        assert_eq!(p.step.element_ref, "e");
        assert_eq!(p.step.op, ElementOp::Type);
        assert_eq!(p.step.text.as_deref(), Some("hi"));
    }

    #[test]
    fn demo_tools_expose_only_the_six_names() {
        let suite = Suite::at(std::env::temp_dir()).unwrap();
        let tools = demo_tools(&suite);
        let got: Vec<String> = tools.iter().map(|t| t.spec().name.clone()).collect();
        assert_eq!(got, names());
    }
}
