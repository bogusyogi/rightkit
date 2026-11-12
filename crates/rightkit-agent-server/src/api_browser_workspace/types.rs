//! DTOs, wire-format enums, and the response/error envelope helpers shared by
//! every browser-workspace route handler.
//!
//! Extracted from `api_browser_workspace.rs` as part of ra-020 (decomposition
//! only — no field, JSON tag, or error-code change). Every type here keeps
//! its original `#[serde(...)]` attributes so wire formats are byte-for-byte
//! identical to the monolithic file.

use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Lease owner written by every human take/release transition. The desktop
/// lease strip (`BrowserPane`) gates its "You control this session" state on
/// `controlLease.owner === "workspace-user"`, so this string is a cross-
/// surface contract, not an internal label. The exclusivity semantics stay in
/// `BrowserControlState` (A15f): one owner at a time, and taking or releasing
/// invalidates queued action tickets.
pub(crate) const WORKSPACE_USER_OWNER: &str = "workspace-user";

/// Workspace viewport — CDP width/height + device pixel ratio. Returned by
/// `get /viewport` and required by `post /viewport`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewportInfo {
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: f64,
}

/// Whether the session uses the throwaway ephemeral profile (default — safe,
/// isolated, deleted on session drop) or a named shared profile the operator
/// has explicitly consented to share. Named profiles are an opt-in escape
/// hatch and must not silently weaken the ephemeral default.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserProfileKind {
    Ephemeral,
    NamedShared,
}

/// Per-host decision applied AFTER the SSRF guard. Additive — the SSRF guard
/// remains the gatekeeper for private-network and metadata targets; the host
/// policy governs ordinary targets the guard accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPolicyDecision {
    Allow,
    Ask,
    Block,
}

/// One entry in the workspace action log. Records who did what — the agent
/// (via a tool call) or the operator (via take-control) — so the transcript
/// of activity is visible independent of the live tool-call history.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserActionEntry {
    pub at_ms: u64,
    pub actor: BrowserActionActor,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserActionActor {
    Agent,
    Human,
    Workspace,
}

/// Annotation captured by the operator from the workspace UI and shipped to
/// the active agent. The structured fields (screenshot, locator, viewport)
/// are the payload the agent consumes; `note` is free-form context.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserAnnotation {
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<ViewportInfo>,
    pub note: String,
}

/// Capability verdict returned by the workspace's permission gate. Mirrors
/// `rightkit_agent_policy::PermissionVerdict` without taking a hard dependency on
/// `rightkit_agent_policy` from the trait surface.
#[derive(Clone, Debug)]
pub struct CapabilityVerdict {
    pub allowed: bool,
    pub reason: String,
}

/// Per-session annotation the standalone fragment owns itself. Agent 4 does
/// not have to back this with `LiveSession`; the fragment holds a private map.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAnnotation {
    pub id: String,
    pub at_ms: u64,
    pub screenshot_base64: Option<String>,
    pub locator: Option<String>,
    pub viewport: Option<ViewportInfo>,
    pub note: String,
    pub dispatched: bool,
}

/// Console line projection — what the fragment needs from the live console.
/// Matches `rightkit_agent_coding_tools::browser::ConsoleLine` shape but the fragment
/// never imports the live type, so it can be satisfied by either the live
/// store or a test stub.
#[derive(Clone, Debug)]
pub struct ConsoleLineView {
    pub level: String,
    pub text: String,
}

// =============================================================================
// Request / response DTOs
// =============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OpenWorkspaceRequest {
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NavigateRequest {
    pub(crate) url: String,
    pub(crate) browser_session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ViewportRequest {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) device_scale_factor: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ZoomRequest {
    pub(crate) zoom: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostPolicyRequest {
    pub(crate) host: String,
    pub(crate) decision: HostPolicyDecision,
    #[serde(default)]
    pub(crate) trusted_private_network: Option<bool>,
    #[serde(default)]
    pub(crate) trusted_cidrs: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProfileConsentRequest {
    pub(crate) kind: BrowserProfileKind,
    #[serde(default)]
    pub(crate) label: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceErrorBody {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScreenshotEnvelope {
    pub(crate) session_id: String,
    pub(crate) url: String,
    pub(crate) captured_at_ms: u64,
    pub(crate) mime: &'static str,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bytes: usize,
    pub(crate) data_base64: String,
    pub(crate) untrusted_wrapped: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnnotationSent {
    pub(crate) annotation_id: String,
    pub(crate) dispatched_to_session: String,
    pub(crate) accepted_at_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CloseRequest {
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReloadRequest {
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScreenshotRequest {
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConsoleRequest {
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

// =============================================================================
// Response/error envelope helpers
// =============================================================================

pub(crate) fn workspace_error(
    status: StatusCode,
    code: &'static str,
    message: String,
) -> (StatusCode, Json<Value>) {
    (status, Json(json!(WorkspaceErrorBody { code, message })))
}

pub(crate) fn paused_response() -> (StatusCode, Json<Value>) {
    workspace_error(
        StatusCode::CONFLICT,
        "browser_paused",
        "The browser workspace is paused. Take control to act on the page.".to_string(),
    )
}

pub(crate) fn named_shared_profile_unsupported_response() -> (StatusCode, Json<Value>) {
    workspace_error(
        StatusCode::NOT_IMPLEMENTED,
        "named_shared_profile_unsupported",
        "Named shared browser profiles are not implemented; this session remains ephemeral."
            .to_string(),
    )
}

pub(crate) fn host_policy_blocked_response(host: &str) -> (StatusCode, Json<Value>) {
    workspace_error(
        StatusCode::FORBIDDEN,
        "host_policy_blocked",
        format!("Host '{host}' is blocked by the workspace host policy."),
    )
}

pub(crate) fn host_policy_approval_required_response(host: &str) -> (StatusCode, Json<Value>) {
    workspace_error(
        StatusCode::CONFLICT,
        "host_policy_approval_required",
        format!("Host '{host}' requires explicit approval before browser navigation."),
    )
}

pub(crate) fn missing_field(name: &str) -> (StatusCode, Json<Value>) {
    workspace_error(
        StatusCode::BAD_REQUEST,
        "ssrf_refused",
        format!("Missing required field: {name}."),
    )
}

pub(crate) fn internal_error(msg: &str) -> (StatusCode, Json<Value>) {
    workspace_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "ssrf_refused",
        format!("Browser action failed: {msg}"),
    )
}

pub(crate) fn capability_denied_response(verdict: &CapabilityVerdict) -> (StatusCode, Json<Value>) {
    workspace_error(
        StatusCode::FORBIDDEN,
        "capability_denied",
        format!("Permission denied: {}", verdict.reason),
    )
}

pub(crate) fn session_not_found(id: &str) -> (StatusCode, Json<Value>) {
    workspace_error(
        StatusCode::NOT_FOUND,
        "session_not_found",
        format!("No '{id}'."),
    )
}

pub(crate) fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let signature = bytes.get(..8)?;
    if signature != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    Some((width, height))
}
