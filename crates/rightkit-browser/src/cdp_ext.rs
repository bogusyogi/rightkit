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

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use serde_json::json;

    // Recorded from Chrome 13x: a main-frame navigation, then the redirect hop it produced.
    fn paused_document() -> Value {
        json!({
            "requestId": "interception-job-1.0",
            "request": {
                "url": "https://example.test/start",
                "method": "GET",
                "headers": {
                    "Upgrade-Insecure-Requests": "1",
                    "User-Agent": "Mozilla/5.0 HeadlessChrome/130.0.0.0",
                    "sec-ch-ua": "\"Chromium\";v=\"130\""
                },
                "initialPriority": "VeryHigh",
                "referrerPolicy": "strict-origin-when-cross-origin"
            },
            "frameId": "FRAME-1",
            "resourceType": "Document",
            "networkId": "1234.5"
        })
    }

    fn paused_redirect_hop() -> Value {
        json!({
            "requestId": "interception-job-2.0",
            "request": {
                "url": "http://169.254.169.254/latest/meta-data/",
                "method": "POST",
                "hasPostData": true,
                "headers": {"Content-Type": "application/json"},
                "initialPriority": "High",
                "referrerPolicy": "no-referrer"
            },
            "frameId": "FRAME-1",
            "resourceType": "XHR",
            "networkId": "1234.6",
            "redirectedRequestId": "interception-job-1.0"
        })
    }

    fn paused_response_stage() -> Value {
        json!({
            "requestId": "interception-job-3.0",
            "request": {"url": "https://example.test/a.png", "method": "GET", "headers": {}},
            "frameId": "FRAME-1",
            "resourceType": "Image",
            "responseStatusCode": 302,
            "responseStatusText": "Found",
            "responseHeaders": [
                {"name": "Location", "value": "https://elsewhere.test/a.png"},
                {"name": "Content-Length", "value": "0"}
            ]
        })
    }

    #[test]
    fn decodes_a_document_request() {
        let r = InterceptedRequest::from_cdp_value(&paused_document()).unwrap();
        assert_eq!(r.request_id, "interception-job-1.0");
        assert_eq!(r.url, "https://example.test/start");
        assert_eq!(r.method, "GET");
        assert_eq!(r.resource_type, "Document");
        assert_eq!(r.frame_id, "FRAME-1");
        assert_eq!(r.stage, InterceptStage::Request);
        assert!(!r.is_redirect());
        assert!(!r.has_post_data);
        assert_eq!(r.header("upgrade-insecure-requests"), Some("1"));
        assert_eq!(r.header("x-missing"), None);
    }

    #[test]
    fn decodes_a_redirect_hop_so_the_new_url_can_be_revalidated() {
        let r = InterceptedRequest::from_cdp_value(&paused_redirect_hop()).unwrap();
        assert!(r.is_redirect());
        assert_eq!(r.redirected_from.as_deref(), Some("interception-job-1.0"));
        assert_eq!(r.url, "http://169.254.169.254/latest/meta-data/");
        assert_eq!(r.method, "POST");
        assert!(r.has_post_data);
    }

    #[test]
    fn decodes_the_response_stage() {
        let r = InterceptedRequest::from_cdp_value(&paused_response_stage()).unwrap();
        assert_eq!(r.stage, InterceptStage::Response);
        assert_eq!(r.response_status, Some(302));
        assert_eq!(
            r.response_headers[0],
            ("Location".into(), "https://elsewhere.test/a.png".into())
        );
    }

    #[test]
    fn rejects_a_paused_event_without_a_request() {
        assert!(InterceptedRequest::from_cdp_value(&json!({"requestId": "x"})).is_none());
        assert!(InterceptedRequest::from_cdp_value(&json!({"request": {"url": "u"}})).is_none());
    }

    #[test]
    fn the_typed_event_round_trips_through_the_decoder() {
        use chromiumoxide::cdp::browser_protocol::fetch::EventRequestPaused;
        let typed: EventRequestPaused = serde_json::from_value(paused_document()).unwrap();
        let wire = serde_json::to_value(&typed).unwrap();
        let r = InterceptedRequest::from_cdp_value(&wire).unwrap();
        assert_eq!(r.url, "https://example.test/start");
        assert_eq!(r.resource_type, "Document");
        assert_eq!(r.request_id, "interception-job-1.0");
    }

    fn id() -> RequestId {
        RequestId::new("interception-job-1.0")
    }

    #[test]
    fn continue_builds_a_bare_continue() {
        let PausedCommand::Continue(p) = command_for(id(), InterceptDecision::Continue) else {
            panic!("expected continue");
        };
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({"requestId": "interception-job-1.0"})
        );
    }

    #[test]
    fn continue_with_overrides_sets_only_what_was_given() {
        let d = InterceptDecision::ContinueWith(ContinueOverrides {
            url: Some("https://example.test/other".into()),
            headers: Some(vec![("X-Pin".into(), "1".into())]),
            post_data: Some(b"ab".to_vec()),
            ..Default::default()
        });
        let PausedCommand::Continue(p) = command_for(id(), d) else {
            panic!("expected continue");
        };
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({
                "requestId": "interception-job-1.0",
                "url": "https://example.test/other",
                "headers": [{"name": "X-Pin", "value": "1"}],
                "postData": "YWI="
            })
        );
    }

    #[test]
    fn fulfill_carries_status_headers_and_a_base64_body() {
        let d = InterceptDecision::Fulfill {
            status: 200,
            headers: vec![("Content-Type".into(), "text/plain".into())],
            body: b"hello".to_vec(),
            phrase: Some("OK".into()),
        };
        let PausedCommand::Fulfill(p) = command_for(id(), d) else {
            panic!("expected fulfill");
        };
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({
                "requestId": "interception-job-1.0",
                "responseCode": 200,
                "responseHeaders": [{"name": "Content-Type", "value": "text/plain"}],
                "body": "aGVsbG8=",
                "responsePhrase": "OK"
            })
        );
    }

    #[test]
    fn fulfill_with_an_empty_body_omits_it() {
        let d = InterceptDecision::Fulfill {
            status: 204,
            headers: vec![],
            body: vec![],
            phrase: None,
        };
        let PausedCommand::Fulfill(p) = command_for(id(), d) else {
            panic!("expected fulfill");
        };
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("body").is_none());
        assert_eq!(v["responseCode"], 204);
    }

    #[test]
    fn fail_maps_to_the_network_error_reason() {
        for (f, name) in [
            (InterceptFailure::BlockedByClient, "BlockedByClient"),
            (InterceptFailure::AccessDenied, "AccessDenied"),
            (InterceptFailure::Aborted, "Aborted"),
            (InterceptFailure::NameNotResolved, "NameNotResolved"),
        ] {
            let PausedCommand::Fail(p) = command_for(id(), InterceptDecision::Fail(f)) else {
                panic!("expected fail");
            };
            assert_eq!(
                serde_json::to_value(&p).unwrap(),
                json!({"requestId": "interception-job-1.0", "errorReason": name})
            );
        }
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    fn request(url: &str) -> InterceptedRequest {
        InterceptedRequest {
            url: url.into(),
            ..InterceptedRequest::from_cdp_value(&paused_document()).unwrap()
        }
    }

    #[tokio::test]
    async fn the_hook_decides_by_url_including_redirect_hops() {
        // An origin pin: only example.test passes, every other hop fails.
        let hook: RequestInterceptor = Arc::new(|r: InterceptedRequest| {
            async move {
                if r.url.starts_with("https://example.test/") {
                    InterceptDecision::Continue
                } else {
                    InterceptDecision::Fail(InterceptFailure::BlockedByClient)
                }
            }
            .boxed()
        });
        let limit = Duration::from_secs(5);
        assert_eq!(
            run_interceptor(&hook, request("https://example.test/x"), limit).await,
            InterceptDecision::Continue
        );
        let hop = InterceptedRequest::from_cdp_value(&paused_redirect_hop()).unwrap();
        assert_eq!(
            run_interceptor(&hook, hop, limit).await,
            InterceptDecision::Fail(InterceptFailure::BlockedByClient)
        );
    }

    #[tokio::test]
    async fn a_hook_that_never_answers_fails_the_request() {
        let hook: RequestInterceptor = Arc::new(|_: InterceptedRequest| {
            futures::future::pending::<InterceptDecision>().boxed()
        });
        let d = run_interceptor(
            &hook,
            request("https://example.test/x"),
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(d, InterceptDecision::Fail(InterceptFailure::Failed));
    }

    // Recorded Accessibility.getFullAXTree result (trimmed): a page with a heading and a button.
    fn ax_tree() -> Value {
        json!({"nodes": [
            {
                "nodeId": "1", "ignored": false,
                "role": {"type": "internalRole", "value": "RootWebArea"},
                "name": {"type": "computedString", "value": "Demo"},
                "properties": [
                    {"name": "focusable", "value": {"type": "booleanOrUndefined", "value": true}}
                ],
                "childIds": ["2", "3", "4"],
                "backendDOMNodeId": 1,
                "frameId": "FRAME-1"
            },
            {
                "nodeId": "2", "ignored": false,
                "role": {"type": "role", "value": "heading"},
                "name": {"type": "computedString", "value": "Welcome"},
                "properties": [
                    {"name": "level", "value": {"type": "integer", "value": 1}}
                ],
                "parentId": "1", "childIds": [], "backendDOMNodeId": 7
            },
            {
                "nodeId": "3", "ignored": false,
                "role": {"type": "role", "value": "button"},
                "name": {"type": "computedString", "value": "Save"},
                "value": {"type": "string", "value": "go"},
                "properties": [
                    {"name": "disabled", "value": {"type": "boolean", "value": true}}
                ],
                "parentId": "1", "backendDOMNodeId": 9
            },
            {
                "nodeId": "4", "ignored": true,
                "ignoredReasons": [{"name": "notRendered", "value": {"type": "boolean", "value": true}}],
                "role": {"type": "role", "value": "none"},
                "parentId": "1"
            },
            {"ignored": false}
        ]})
    }

    #[test]
    fn decodes_the_ax_tree() {
        let nodes = parse_ax_tree(&ax_tree());
        assert_eq!(nodes.len(), 4, "a node without an id is dropped");
        let root = &nodes[0];
        assert_eq!(root.role.as_deref(), Some("RootWebArea"));
        assert_eq!(root.name.as_deref(), Some("Demo"));
        assert_eq!(root.child_ids, vec!["2", "3", "4"]);
        assert_eq!(root.frame_id.as_deref(), Some("FRAME-1"));
        assert_eq!(root.property("focusable"), Some(&json!(true)));

        let heading = &nodes[1];
        assert_eq!(heading.role.as_deref(), Some("heading"));
        assert_eq!(heading.property("level"), Some(&json!(1)));
        assert_eq!(heading.parent_id.as_deref(), Some("1"));
        assert_eq!(heading.backend_dom_node_id, Some(7));

        let button = &nodes[2];
        assert_eq!(button.name.as_deref(), Some("Save"));
        assert_eq!(button.value, Some(json!("go")));
        assert_eq!(button.property("disabled"), Some(&json!(true)));

        assert!(nodes[3].ignored);
        assert_eq!(nodes[3].name, None);
    }

    #[test]
    fn an_empty_or_malformed_ax_result_is_an_empty_tree() {
        assert!(parse_ax_tree(&json!({})).is_empty());
        assert!(parse_ax_tree(&json!({"nodes": "x"})).is_empty());
    }

    #[test]
    fn the_typed_ax_result_round_trips_through_the_decoder() {
        use chromiumoxide::cdp::browser_protocol::accessibility::GetFullAxTreeReturns;
        let typed: GetFullAxTreeReturns = serde_json::from_value(ax_tree_typed_safe()).unwrap();
        let nodes = parse_ax_tree(&serde_json::to_value(&typed).unwrap());
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].name.as_deref(), Some("Welcome"));
    }

    fn ax_tree_typed_safe() -> Value {
        json!({"nodes": [
            {"nodeId": "1", "ignored": false,
             "role": {"type": "internalRole", "value": "RootWebArea"},
             "childIds": ["2"]},
            {"nodeId": "2", "ignored": false,
             "role": {"type": "role", "value": "heading"},
             "name": {"type": "computedString", "value": "Welcome"},
             "parentId": "1"}
        ]})
    }

    // Recorded Runtime.exceptionThrown for `setTimeout(() => { throw new Error("boom") })`.
    fn exception_event() -> Value {
        json!({
            "timestamp": 1760000000123.5,
            "exceptionDetails": {
                "exceptionId": 1,
                "text": "Uncaught",
                "lineNumber": 11,
                "columnNumber": 14,
                "scriptId": "42",
                "url": "https://example.test/app.js",
                "stackTrace": {"callFrames": [
                    {"functionName": "tick", "scriptId": "42",
                     "url": "https://example.test/app.js", "lineNumber": 11, "columnNumber": 14},
                    {"functionName": "", "scriptId": "42",
                     "url": "https://example.test/app.js", "lineNumber": 20, "columnNumber": 2}
                ]},
                "exception": {
                    "type": "object", "subtype": "error", "className": "Error",
                    "description": "Error: boom\n    at tick (https://example.test/app.js:12:15)",
                    "objectId": "1.2.3"
                },
                "executionContextId": 3
            }
        })
    }

    #[test]
    fn decodes_an_exception_event() {
        let e = PageException::from_cdp_value(&exception_event()).unwrap();
        assert_eq!(e.text, "Uncaught");
        assert!(e.message().starts_with("Error: boom"));
        assert_eq!(e.url.as_deref(), Some("https://example.test/app.js"));
        assert_eq!((e.line, e.column), (11, 14));
        assert_eq!(e.timestamp_ms, 1760000000123.5);
        assert_eq!(e.stack.len(), 2);
        assert_eq!(e.stack[0].function_name, "tick");
        assert_eq!(e.stack[1].function_name, "");
    }

    #[test]
    fn an_exception_without_a_thrown_value_falls_back_to_the_text() {
        let e = PageException::from_cdp_value(&json!({
            "timestamp": 1.0,
            "exceptionDetails": {"exceptionId": 2, "text": "Uncaught (in promise)",
                                  "lineNumber": 0, "columnNumber": 0}
        }))
        .unwrap();
        assert_eq!(e.message(), "Uncaught (in promise)");
        assert!(e.stack.is_empty());
        assert!(PageException::from_cdp_value(&json!({"timestamp": 1.0})).is_none());
    }

    #[test]
    fn the_typed_exception_round_trips_through_the_decoder() {
        use chromiumoxide::cdp::js_protocol::runtime::EventExceptionThrown;
        let typed: EventExceptionThrown = serde_json::from_value(exception_event()).unwrap();
        let e = PageException::from_cdp_value(&serde_json::to_value(&typed).unwrap()).unwrap();
        assert!(e.message().starts_with("Error: boom"));
        assert_eq!(e.stack.len(), 2);
        assert_eq!(e.timestamp_ms, 1760000000123.5);
    }

    #[test]
    fn page_scale_must_be_positive_and_finite() {
        assert_eq!(
            serde_json::to_value(page_scale_params(1.5).unwrap()).unwrap(),
            json!({"pageScaleFactor": 1.5})
        );
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                page_scale_params(bad),
                Err(BrowserError::Invalid(_))
            ));
        }
    }

    #[test]
    fn download_behaviour_carries_the_explicit_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let v = serde_json::to_value(
            download_params(&DownloadBehavior::Allow { dir: path.clone() }).unwrap(),
        )
        .unwrap();
        assert_eq!(v["behavior"], "allow");
        assert_eq!(v["downloadPath"], &*path.to_string_lossy());
        let v = serde_json::to_value(
            download_params(&DownloadBehavior::AllowAndName { dir: path }).unwrap(),
        )
        .unwrap();
        assert_eq!(v["behavior"], "allowAndName");
        assert_eq!(
            serde_json::to_value(download_params(&DownloadBehavior::Deny).unwrap()).unwrap(),
            json!({"behavior": "deny"})
        );
        assert_eq!(
            serde_json::to_value(download_params(&DownloadBehavior::Default).unwrap()).unwrap(),
            json!({"behavior": "default"})
        );
    }

    #[test]
    fn download_directory_must_be_absolute_and_exist() {
        let rel = DownloadBehavior::Allow {
            dir: PathBuf::from("downloads"),
        };
        assert!(matches!(
            download_params(&rel),
            Err(BrowserError::Invalid(_))
        ));
        let missing = tempfile::tempdir().unwrap().path().join("nope");
        let gone = DownloadBehavior::AllowAndName { dir: missing };
        assert!(matches!(
            download_params(&gone),
            Err(BrowserError::Invalid(_))
        ));
    }
}
