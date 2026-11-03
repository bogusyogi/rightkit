//! Caller-facing CDP surface added in 0.4.0: request interception, the accessibility tree,
//! exception events, page scale and per-page download behaviour.
//!
//! Everything here that decodes or builds a CDP message is a pure function over JSON or over the
//! generated parameter types, so the unit tests feed recorded wire messages through it without a
//! browser. The `BrowserPage` methods in `page.rs` only send what these functions build.

use crate::error::{BrowserError, Result};
use chromiumoxide::cdp::browser_protocol::browser::{
    SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
};
use chromiumoxide::cdp::browser_protocol::emulation::SetPageScaleFactorParams;
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, FailRequestParams, FulfillRequestParams, HeaderEntry, RequestId,
};
use chromiumoxide::cdp::browser_protocol::network::ErrorReason;
use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How long an interceptor may take before the request is failed (fail closed).
pub(crate) const INTERCEPTOR_TIMEOUT: Duration = Duration::from_secs(30);

/// Structured exceptions kept per page (oldest dropped first).
pub const MAX_EXCEPTIONS: usize = 200;

// ---- request interception ------------------------------------------------------------------

/// Which stage a paused request is at.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum InterceptStage {
    /// Before the request leaves the browser. Redirect hops arrive here too.
    Request,
    /// After response headers arrived (only when response-stage patterns are enabled).
    Response,
}

/// One `Fetch.requestPaused` event, decoded.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InterceptedRequest {
    /// CDP fetch request id (what the answer is addressed to).
    pub request_id: String,
    pub url: String,
    pub method: String,
    /// Request headers, in the order CDP reported them.
    pub headers: Vec<(String, String)>,
    /// CDP resource type, e.g. `Document`, `XHR`, `Image`.
    pub resource_type: String,
    /// CDP frame id of the frame that issued the request.
    pub frame_id: String,
    pub has_post_data: bool,
    /// Set when this request is a redirect hop: the id of the request whose redirect caused it.
    /// Revalidate the new `url` here (origin pin) exactly as for a first request.
    pub redirected_from: Option<String>,
    pub stage: InterceptStage,
    pub response_status: Option<i64>,
    pub response_headers: Vec<(String, String)>,
}

impl InterceptedRequest {
    /// True for a request created by following a redirect.
    pub fn is_redirect(&self) -> bool {
        self.redirected_from.is_some()
    }

    /// First header value with this name (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Decode the params of a `Fetch.requestPaused` event (CDP wire form).
    pub fn from_cdp_value(v: &Value) -> Option<Self> {
        let request = v.get("request")?;
        let text = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
        let response_status = v.get("responseStatusCode").and_then(Value::as_i64);
        let stage = if response_status.is_some() || v.get("responseErrorReason").is_some() {
            InterceptStage::Response
        } else {
            InterceptStage::Request
        };
        Some(Self {
            request_id: text(v, "requestId")?,
            url: text(request, "url")?,
            method: text(request, "method").unwrap_or_else(|| "GET".into()),
            headers: request
                .get("headers")
                .map(header_object)
                .unwrap_or_default(),
            resource_type: text(v, "resourceType").unwrap_or_default(),
            frame_id: text(v, "frameId").unwrap_or_default(),
            has_post_data: request
                .get("hasPostData")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            redirected_from: text(v, "redirectedRequestId"),
            stage,
            response_status,
            response_headers: v
                .get("responseHeaders")
                .map(header_entries)
                .unwrap_or_default(),
        })
    }
}

/// Overrides for a continued request. Anything left `None` is unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContinueOverrides {
    /// Rewrites the URL without the page seeing it. Not re-checked against the network policy.
    pub url: Option<String>,
    pub method: Option<String>,
    /// Replaces the request headers entirely.
    pub headers: Option<Vec<(String, String)>>,
    pub post_data: Option<Vec<u8>>,
}

/// Why a request is failed; maps to the CDP `Network.ErrorReason` the page sees.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterceptFailure {
    Failed,
    Aborted,
    AccessDenied,
    ConnectionRefused,
    NameNotResolved,
    BlockedByClient,
}

impl InterceptFailure {
    fn reason(self) -> ErrorReason {
        match self {
            Self::Failed => ErrorReason::Failed,
            Self::Aborted => ErrorReason::Aborted,
            Self::AccessDenied => ErrorReason::AccessDenied,
            Self::ConnectionRefused => ErrorReason::ConnectionRefused,
            Self::NameNotResolved => ErrorReason::NameNotResolved,
            Self::BlockedByClient => ErrorReason::BlockedByClient,
        }
    }
}

/// The answer to one paused request.
#[derive(Clone, Debug, PartialEq)]
pub enum InterceptDecision {
    /// Let it through unchanged.
    Continue,
    /// Let it through with changes.
    ContinueWith(ContinueOverrides),
    /// Answer it from here; nothing reaches the network.
    Fulfill {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        phrase: Option<String>,
    },
    /// Fail it with a network error.
    Fail(InterceptFailure),
}

/// Caller-supplied hook, consulted for every request the page makes (redirect hops included).
///
/// It runs after the session's [`crate::NetworkPolicy`]: a request the policy refuses is failed
/// without the hook seeing it, so a hook can only narrow what the policy allows. A hook that
/// takes longer than 30 s fails the request.
pub type RequestInterceptor =
    Arc<dyn Fn(InterceptedRequest) -> BoxFuture<'static, InterceptDecision> + Send + Sync>;

/// The CDP command that answers a paused request.
#[derive(Debug)]
pub(crate) enum PausedCommand {
    Continue(ContinueRequestParams),
    Fulfill(FulfillRequestParams),
    Fail(FailRequestParams),
}

pub(crate) fn command_for(id: RequestId, decision: InterceptDecision) -> PausedCommand {
    match decision {
        InterceptDecision::Continue => PausedCommand::Continue(ContinueRequestParams::new(id)),
        InterceptDecision::ContinueWith(o) => {
            let mut p = ContinueRequestParams::new(id);
            p.url = o.url;
            p.method = o.method;
            p.headers = o.headers.map(header_entries_out);
            p.post_data = o.post_data.map(|b| base64(&b).into());
            PausedCommand::Continue(p)
        }
        InterceptDecision::Fulfill {
            status,
            headers,
            body,
            phrase,
        } => {
            let mut p = FulfillRequestParams::new(id, i64::from(status));
            p.response_headers = Some(header_entries_out(headers));
            if !body.is_empty() {
                p.body = Some(base64(&body).into());
            }
            p.response_phrase = phrase;
            PausedCommand::Fulfill(p)
        }
        InterceptDecision::Fail(f) => PausedCommand::Fail(FailRequestParams::new(id, f.reason())),
    }
}

/// Ask the hook, failing the request when it does not answer in `limit`.
pub(crate) async fn run_interceptor(
    hook: &RequestInterceptor,
    request: InterceptedRequest,
    limit: Duration,
) -> InterceptDecision {
    match tokio::time::timeout(limit, hook(request)).await {
        Ok(d) => d,
        Err(_) => InterceptDecision::Fail(InterceptFailure::Failed),
    }
}

// ---- accessibility tree --------------------------------------------------------------------

/// One node of `Accessibility.getFullAXTree`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AxNode {
    pub node_id: String,
    pub ignored: bool,
    pub role: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    /// The node's value (text fields, sliders). Chrome masks password values itself.
    pub value: Option<Value>,
    /// Other AX properties (`focused`, `disabled`, `level`, ...) as name and raw value.
    pub properties: Vec<(String, Value)>,
    pub parent_id: Option<String>,
    pub child_ids: Vec<String>,
    /// Backend DOM node id, usable with `DOM.resolveNode`.
    pub backend_dom_node_id: Option<i64>,
    pub frame_id: Option<String>,
}

impl AxNode {
    /// Property by name.
    pub fn property(&self, name: &str) -> Option<&Value> {
        self.properties
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
    }
}

/// One non-ignored node of an accessibility tree that has a role, as carried by an observation.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AxSummary {
    pub role: String,
    /// Accessible name; empty when the node has none.
    pub name: String,
    /// CDP frame id of the node's document (empty when Chrome reports none).
    pub frame_id: String,
}

/// Summarize an accessibility tree: ignored nodes and nodes without a role are dropped.
pub(crate) fn summarize_ax(nodes: &[AxNode]) -> Vec<AxSummary> {
    nodes
        .iter()
        .filter(|n| !n.ignored)
        .filter_map(|n| {
            let role = n.role.as_ref().filter(|r| !r.is_empty())?;
            Some(AxSummary {
                role: role.clone(),
                name: n.name.clone().unwrap_or_default(),
                frame_id: n.frame_id.clone().unwrap_or_default(),
            })
        })
        .collect()
}

/// Decode the result of `Accessibility.getFullAXTree` (`{"nodes":[...]}`).
pub(crate) fn parse_ax_tree(result: &Value) -> Vec<AxNode> {
    let ax_value =
        |n: &Value, k: &str| -> Option<Value> { n.get(k).and_then(|x| x.get("value")).cloned() };
    let ax_text = |n: &Value, k: &str| -> Option<String> {
        ax_value(n, k).and_then(|v| match v {
            Value::String(s) => Some(s),
            Value::Null => None,
            other => Some(other.to_string()),
        })
    };
    result
        .get("nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| {
                    Some(AxNode {
                        node_id: n.get("nodeId").and_then(Value::as_str)?.to_string(),
                        ignored: n.get("ignored").and_then(Value::as_bool).unwrap_or(false),
                        role: ax_text(n, "role"),
                        name: ax_text(n, "name"),
                        description: ax_text(n, "description"),
                        value: ax_value(n, "value"),
                        properties: n
                            .get("properties")
                            .and_then(Value::as_array)
                            .map(|ps| {
                                ps.iter()
                                    .filter_map(|p| {
                                        Some((
                                            p.get("name")?.as_str()?.to_string(),
                                            p.get("value")
                                                .and_then(|v| v.get("value"))
                                                .cloned()
                                                .unwrap_or(Value::Null),
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        parent_id: n
                            .get("parentId")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        child_ids: n
                            .get("childIds")
                            .and_then(Value::as_array)
                            .map(|c| {
                                c.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        backend_dom_node_id: n.get("backendDOMNodeId").and_then(Value::as_i64),
                        frame_id: n.get("frameId").and_then(Value::as_str).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---- exceptions ----------------------------------------------------------------------------

/// One frame of an exception's stack.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExceptionFrame {
    pub function_name: String,
    pub url: String,
    /// Zero-based, as CDP reports it.
    pub line: i64,
    pub column: i64,
}

/// One `Runtime.exceptionThrown` event: an uncaught exception or unhandled rejection.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PageException {
    /// `exceptionDetails.text`, usually `Uncaught`.
    pub text: String,
    /// The thrown value's description (`Error: boom\n    at ...`) when CDP gave one.
    pub description: Option<String>,
    pub url: Option<String>,
    pub line: i64,
    pub column: i64,
    /// Milliseconds since the epoch, as CDP reports it.
    pub timestamp_ms: f64,
    pub stack: Vec<ExceptionFrame>,
}

impl PageException {
    /// Best single-line-or-more message: the description, else the text.
    pub fn message(&self) -> &str {
        self.description.as_deref().unwrap_or(&self.text)
    }

    /// Decode the params of a `Runtime.exceptionThrown` event (CDP wire form).
    pub fn from_cdp_value(v: &Value) -> Option<Self> {
        let d = v.get("exceptionDetails")?;
        Some(Self {
            text: d.get("text").and_then(Value::as_str)?.to_string(),
            description: d
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(Value::as_str)
                .map(str::to_string),
            url: d.get("url").and_then(Value::as_str).map(str::to_string),
            line: d.get("lineNumber").and_then(Value::as_i64).unwrap_or(0),
            column: d.get("columnNumber").and_then(Value::as_i64).unwrap_or(0),
            timestamp_ms: v.get("timestamp").and_then(Value::as_f64).unwrap_or(0.0),
            stack: d
                .get("stackTrace")
                .and_then(|s| s.get("callFrames"))
                .and_then(Value::as_array)
                .map(|frames| {
                    frames
                        .iter()
                        .map(|f| ExceptionFrame {
                            function_name: f
                                .get("functionName")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            url: f
                                .get("url")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            line: f.get("lineNumber").and_then(Value::as_i64).unwrap_or(0),
                            column: f.get("columnNumber").and_then(Value::as_i64).unwrap_or(0),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

// ---- page scale and downloads ----------------------------------------------------------------

/// Validated `Emulation.setPageScaleFactor` parameters.
pub(crate) fn page_scale_params(scale: f64) -> Result<SetPageScaleFactorParams> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(BrowserError::Invalid(format!(
            "page scale must be a positive finite number, got {scale}"
        )));
    }
    Ok(SetPageScaleFactorParams::new(scale))
}

/// What the page does with a download.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadBehavior {
    /// Refuse every download.
    Deny,
    /// Save under the file's own name into `dir`.
    Allow { dir: PathBuf },
    /// Save into `dir` named by the download's GUID (no name collisions, no caller-controlled names).
    AllowAndName { dir: PathBuf },
    /// Chrome's default behaviour.
    Default,
}

/// Validated `Browser.setDownloadBehavior` parameters. A directory must be absolute and exist.
pub(crate) fn download_params(behavior: &DownloadBehavior) -> Result<SetDownloadBehaviorParams> {
    let dir_text = |dir: &PathBuf| -> Result<String> {
        if !dir.is_absolute() {
            return Err(BrowserError::Invalid(format!(
                "download directory must be absolute: {}",
                dir.display()
            )));
        }
        if !dir.is_dir() {
            return Err(BrowserError::Invalid(format!(
                "download directory does not exist: {}",
                dir.display()
            )));
        }
        Ok(dir.to_string_lossy().into_owned())
    };
    Ok(match behavior {
        DownloadBehavior::Deny => SetDownloadBehaviorParams::new(SetDownloadBehaviorBehavior::Deny),
        DownloadBehavior::Default => {
            SetDownloadBehaviorParams::new(SetDownloadBehaviorBehavior::Default)
        }
        DownloadBehavior::Allow { dir } => {
            let mut p = SetDownloadBehaviorParams::new(SetDownloadBehaviorBehavior::Allow);
            p.download_path = Some(dir_text(dir)?);
            p
        }
        DownloadBehavior::AllowAndName { dir } => {
            let mut p = SetDownloadBehaviorParams::new(SetDownloadBehaviorBehavior::AllowAndName);
            p.download_path = Some(dir_text(dir)?);
            p
        }
    })
}

// ---- helpers ---------------------------------------------------------------------------------

/// `Network.Headers`: a JSON object of name to value.
fn header_object(v: &Value) -> Vec<(String, String)> {
    v.as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `Fetch.HeaderEntry` array.
fn header_entries(v: &Value) -> Vec<(String, String)> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|h| {
                    Some((
                        h.get("name")?.as_str()?.to_string(),
                        h.get("value")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn header_entries_out(h: Vec<(String, String)>) -> Vec<HeaderEntry> {
    h.into_iter().map(|(n, v)| HeaderEntry::new(n, v)).collect()
}

/// Standard base64 with padding (CDP `binary`).
pub(crate) fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
