//! Browser control for a client-owned surface — ENG-98's mobile counterpart to `browser.rs`.
//!
//! `browser.rs` drives a real, daemon-local Chrome over CDP. That has no mobile analogue:
//! there is no desktop Chrome/Edge to launch next to an iOS daemon, and CDP cannot reach a
//! process that does not exist. On mobile the "browser" is a `WKWebView` the client itself
//! owns (see `BrowserControlSurfaceView.swift`) — the daemon can never touch its cookie jar,
//! its process, or its rendering directly. So instead of opening a page, these tools issue a
//! bounded, typed *request* for one interactive step (navigate, read, click, type, screenshot)
//! and await a *relayed* observation the client reports back after performing it locally.
//!
//! Hardening matches `browser.rs` deliberately, not coincidentally: bounded response text
//! (`MAX_TEXT_BYTES`), a bounded screenshot cap, a request timeout so a stalled client can
//! never hang a tool call forever, URL length/shape validation before anything is sent to the
//! client, and `wrap_untrusted_web_text` around anything the page rendered — page content is
//! attacker-controlled data, not instructions, exactly as in `browser.rs`.
//!
//! ENG-113 boundary: a page can render a link to a non-http(s) scheme (`mailto:`, `tel:`,
//! a custom app scheme, ...) or the model can ask to "navigate" there. Neither this module nor
//! the client-owned surface ever opens such a target on its own. `ClientBrowserObservation`
//! only ever *reports* an encountered non-http(s) target as `pending_external_scheme`; the
//! daemon does not act on it, and the client is contractually required (see
//! `ExternalSchemeConsentStore`/`ExternalSchemeConsentPrompt` on iOS) to resolve it through a
//! fresh, remembered-per-scheme user decision before any handoff happens. A page cannot spend
//! a stale consent, and an agent cannot spend the user's consent on its behalf.

use crate::browser_fetch::wrap_untrusted_web_text;
use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use async_trait::async_trait;
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Whether a client-owned browser surface (an iOS/desktop webview) is attached to
/// this process. The `browser_control_*` tools are advertised to the model only
/// while one is: a plain CLI/TUI session has no such surface, and offering tools
/// that can only answer "no client browser surface attached" wastes the model's
/// tool budget. The client attach seam sets this when its relay connects and
/// clears it on detach.
static CLIENT_SURFACE_ATTACHED: AtomicBool = AtomicBool::new(false);

/// Record that a client-owned browser surface attached (`true`) or detached.
pub fn set_client_browser_surface_attached(attached: bool) {
    CLIENT_SURFACE_ATTACHED.store(attached, Ordering::SeqCst);
}

/// Availability probe for the `browser_control_*` tool family.
pub fn client_browser_surface_attached() -> bool {
    CLIENT_SURFACE_ATTACHED.load(Ordering::SeqCst)
}

/// Same bound `browser.rs` uses for rendered page text — a heavy mobile page cannot blow the
/// context window in one read either.
const MAX_TEXT_BYTES: usize = 50_000;
/// Same bound `browser.rs` uses for screenshots; a viewport PNG that exceeds this is a client
/// bug, not a legitimate screenshot.
pub const MAX_SCREENSHOT_BYTES: usize = 4 * 1024 * 1024;
const MAX_NAVIGATION_URL_BYTES: usize = 8 * 1024;
const MAX_INPUT_TEXT_BYTES: usize = 8 * 1024;
/// A client-relayed step still has to return within the same tool-call budget a local CDP
/// action would; the client surface is expected to be interactive, so this is generous but
/// finite — a hung relay must fail the tool call rather than hang it.
const RELAY_TIMEOUT: Duration = Duration::from_secs(45);

/// One interactive step the daemon asks the client-owned browser surface to perform.
/// Deliberately narrower than `browser.rs`'s `BrowserActTool` action set: this is the mobile
/// surface's first cut (navigate/read/click/type/screenshot), not the full desktop action list.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientBrowserAction {
    Navigate { url: String },
    Read,
    Click { selector: String },
    Type { selector: String, text: String },
    Screenshot,
}

/// Request handed to the relay. `session` scopes the client-owned surface exactly the way
/// `browser.rs`'s `BrowserSession` id scopes a CDP session — one client surface, addressed by
/// id, driven across separate tool calls.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClientBrowserRequest {
    pub session: String,
    pub action: ClientBrowserAction,
}

/// What the client reports back after performing one step. `pending_external_scheme` is the
/// ENG-113 seam: set only when the step encountered (or was asked to reach) a non-http(s)
/// target, and never a signal this module or the client surface may act on unattended.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClientBrowserObservation {
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub elements: Option<Value>,
    #[serde(default)]
    pub screenshot_base64: Option<String>,
    /// Present only when the client surface saw a non-http(s) target and stopped short of it.
    /// The daemon must never treat this as consent-equivalent; it is a fact for the model to
    /// report to the operator, nothing more.
    #[serde(default)]
    pub pending_external_scheme: Option<String>,
}

/// Seam the daemon-side transport implements to actually reach the client. Kept separate from
/// `ToolContext` construction here: wiring a live implementation onto `ToolContext` and the
/// mobile transport is out of this module's file allowlist (see gap report).
#[async_trait]
pub trait ClientBrowserRelay: Send + Sync {
    async fn dispatch(
        &self,
        request: ClientBrowserRequest,
    ) -> Result<ClientBrowserObservation, String>;
}

async fn gate(ctx: &ToolContext) -> Option<ToolResult> {
    // Same gate shape every other network-class tool uses (see `cloudflare.rs`):
    // a fresh engine evaluated against this context's mode and workspace trust.
    let verdict = PermissionEngine::new().evaluate(
        CapabilityClass::Browser,
        ctx.permission_mode,
        ctx.workspace_trust,
        None,
        false,
    );
    match verdict.decision {
        PermissionDecision::Allow => None,
        PermissionDecision::Ask => Some(ToolResult::error(format!(
            "Tool requires approval: {}",
            verdict.reason
        ))),
        PermissionDecision::Deny => Some(ToolResult::error(format!(
            "Permission denied: {}",
            verdict.reason
        ))),
    }
}

fn validate_url(url: &str) -> Result<(), String> {
    if url.is_empty() || url.len() > MAX_NAVIGATION_URL_BYTES {
        return Err(format!(
            "navigation URL is empty or exceeds {MAX_NAVIGATION_URL_BYTES} bytes"
        ));
    }
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid URL '{url}': {e}"))?;
    // Scheme classification only — this module never opens a non-http(s) target itself, it
    // just refuses to forward a step that pretends navigation is the action when it is really
    // an external-scheme handoff request in disguise. Genuine external-scheme encounters are
    // reported back via `pending_external_scheme`, not requested up front.
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "navigate only accepts http(s) URLs; '{}' is a non-http(s) scheme and must go \
             through external-scheme consent on the client, not browser_control_act",
            parsed.scheme()
        ));
    }
    Ok(())
}

fn validate_selector(selector: &str) -> Result<(), String> {
    if selector.is_empty() || selector.len() > 2_048 {
        return Err("selector is empty or unreasonably long".into());
    }
    Ok(())
}

fn validate_type_text(text: &str) -> Result<(), String> {
    if text.len() > MAX_INPUT_TEXT_BYTES {
        return Err(format!("typed text exceeds {MAX_INPUT_TEXT_BYTES} bytes"));
    }
    Ok(())
}

async fn relay_of(ctx: &ToolContext) -> Result<std::sync::Arc<dyn ClientBrowserRelay>, ToolResult> {
    // `ToolContext` does not yet carry a client-browser-relay handle; see the gap report in
    // the dispatch summary. This lookup is written against the field the wiring lane needs to
    // add (`ctx.client_browser_relay: Option<Arc<dyn ClientBrowserRelay>>`) so the tool bodies
    // below need no further change once it lands.
    ctx.client_browser_relay.clone().ok_or_else(|| {
        ToolResult::error("client-owned browser surface is not available in this context")
    })
}

async fn dispatch_bounded(
    relay: &dyn ClientBrowserRelay,
    request: ClientBrowserRequest,
) -> Result<ClientBrowserObservation, String> {
    match tokio::time::timeout(RELAY_TIMEOUT, relay.dispatch(request)).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "client-owned browser surface did not respond within {}s",
            RELAY_TIMEOUT.as_secs()
        )),
    }
}

fn bound_observation(
    mut observation: ClientBrowserObservation,
) -> Result<ClientBrowserObservation, String> {
    if let Some(text) = observation.text.as_mut() {
        *text = crate::output_compaction::mask_secrets(text);
        if text.len() > MAX_TEXT_BYTES {
            text.truncate(MAX_TEXT_BYTES);
            text.push_str("\n… [truncated]");
        }
    }
    if let Some(shot) = observation.screenshot_base64.as_ref() {
        // base64 is ~4/3 the raw size; bound the encoded string, not just the decoded bytes,
        // so a hostile or buggy client cannot force a huge allocation before we ever decode it.
        if shot.len() > (MAX_SCREENSHOT_BYTES * 4) / 3 + 64 {
            return Err(format!(
                "screenshot exceeds the {MAX_SCREENSHOT_BYTES}-byte cap"
            ));
        }
    }
    observation.url = crate::output_compaction::mask_secrets(&observation.url);
    observation.title = crate::output_compaction::mask_secrets(&observation.title);
    Ok(observation)
}

fn render_observation(observation: &ClientBrowserObservation) -> String {
    let mut body = format!("url: {}\ntitle: {}", observation.url, observation.title);
    if let Some(text) = &observation.text {
        body.push_str("\n\n--- text ---\n");
        body.push_str(text);
    }
    if let Some(elements) = &observation.elements {
        body.push_str("\n\n--- interactive elements ---\n");
        body.push_str(&serde_json::to_string_pretty(elements).unwrap_or_else(|_| "[]".into()));
    }
    if let Some(scheme) = &observation.pending_external_scheme {
        body.push_str(&format!(
            "\n\n--- pending external scheme ---\nThe page pointed at '{scheme}', a non-http(s) target. \
             This was NOT opened. Ask the operator; the client will only hand off after they \
             confirm through the external-scheme consent prompt for this exact scheme."
        ));
    }
    body
}

pub struct BrowserControlNavigateTool;

#[async_trait]
impl Tool for BrowserControlNavigateTool {
    fn name(&self) -> &str {
        "browser_control_navigate"
    }

    fn description(&self) -> &str {
        "Navigate the client-owned browser surface (a WKWebView on the user's device, not a daemon-local Chrome) to an http(s) URL and return what rendered. Never accepts non-http(s) schemes — those require the client's external-scheme consent flow, never this tool. Fail: no client browser surface attached, invalid URL, or the client refused."
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Browser)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["session", "url"],
            "properties": {
                "session": { "type": "string", "description": "Client browser surface session id" },
                "url": { "type": "string", "description": "http(s) URL to navigate to" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if let Some(denied) = gate(ctx).await {
            return denied;
        }
        let Some(session) = args["session"].as_str() else {
            return ToolResult::error("Missing required argument: session");
        };
        let Some(url) = args["url"].as_str() else {
            return ToolResult::error("Missing required argument: url");
        };
        if let Err(reason) = validate_url(url) {
            return ToolResult::error(reason);
        }
        let relay = match relay_of(ctx).await {
            Ok(relay) => relay,
            Err(result) => return result,
        };
        let request = ClientBrowserRequest {
            session: session.to_string(),
            action: ClientBrowserAction::Navigate {
                url: url.to_string(),
            },
        };
        let observation = match dispatch_bounded(relay.as_ref(), request).await {
            Ok(observation) => observation,
            Err(error) => return ToolResult::error(error),
        };
        let observation = match bound_observation(observation) {
            Ok(observation) => observation,
            Err(error) => return ToolResult::error(error),
        };
        ToolResult::ok(wrap_untrusted_web_text(
            "browser_control_navigate",
            &render_observation(&observation),
        ))
    }
}

pub struct BrowserControlReadTool;

#[async_trait]
impl Tool for BrowserControlReadTool {
    fn name(&self) -> &str {
        "browser_control_read"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Read what the client-owned browser surface is currently showing: rendered page text and interactive elements with their selectors. Fail: no client browser surface attached, or unknown session."
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Browser)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["session"],
            "properties": {
                "session": { "type": "string", "description": "Client browser surface session id" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let Some(session) = args["session"].as_str() else {
            return ToolResult::error("Missing required argument: session");
        };
        let relay = match relay_of(ctx).await {
            Ok(relay) => relay,
            Err(result) => return result,
        };
        let request = ClientBrowserRequest {
            session: session.to_string(),
            action: ClientBrowserAction::Read,
        };
        let observation = match dispatch_bounded(relay.as_ref(), request).await {
            Ok(observation) => observation,
            Err(error) => return ToolResult::error(error),
        };
        let observation = match bound_observation(observation) {
            Ok(observation) => observation,
            Err(error) => return ToolResult::error(error),
        };
        ToolResult::ok(wrap_untrusted_web_text(
            "browser_control_read",
            &render_observation(&observation),
        ))
    }
}

pub struct BrowserControlClickTool;

#[async_trait]
impl Tool for BrowserControlClickTool {
    fn name(&self) -> &str {
        "browser_control_click"
    }

    fn description(&self) -> &str {
        "Click one element in the client-owned browser surface by CSS selector (get selectors from browser_control_read). If the element is a link to a non-http(s) scheme, the client stops short and reports it via pending_external_scheme rather than opening it. Fail: unknown session, ambiguous or missing selector."
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Browser)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["session", "selector"],
            "properties": {
                "session": { "type": "string", "description": "Client browser surface session id" },
                "selector": { "type": "string", "description": "Unique CSS selector to click" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if let Some(denied) = gate(ctx).await {
            return denied;
        }
        let Some(session) = args["session"].as_str() else {
            return ToolResult::error("Missing required argument: session");
        };
        let Some(selector) = args["selector"].as_str() else {
            return ToolResult::error("Missing required argument: selector");
        };
        if let Err(reason) = validate_selector(selector) {
            return ToolResult::error(reason);
        }
        let relay = match relay_of(ctx).await {
            Ok(relay) => relay,
            Err(result) => return result,
        };
        let request = ClientBrowserRequest {
            session: session.to_string(),
            action: ClientBrowserAction::Click {
                selector: selector.to_string(),
            },
        };
        let observation = match dispatch_bounded(relay.as_ref(), request).await {
            Ok(observation) => observation,
            Err(error) => return ToolResult::error(error),
        };
        let observation = match bound_observation(observation) {
            Ok(observation) => observation,
            Err(error) => return ToolResult::error(error),
        };
        ToolResult::ok(wrap_untrusted_web_text(
            "browser_control_click",
            &render_observation(&observation),
        ))
    }
}

pub struct BrowserControlTypeTool;

#[async_trait]
impl Tool for BrowserControlTypeTool {
    fn name(&self) -> &str {
        "browser_control_type"
    }

    fn description(&self) -> &str {
        "Type text into one element in the client-owned browser surface by CSS selector. Fail: unknown session, ambiguous or missing selector, or text exceeds the input bound."
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Browser)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["session", "selector", "text"],
            "properties": {
                "session": { "type": "string", "description": "Client browser surface session id" },
                "selector": { "type": "string", "description": "Unique CSS selector to type into" },
                "text": { "type": "string", "description": "Text to type" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if let Some(denied) = gate(ctx).await {
            return denied;
        }
        let Some(session) = args["session"].as_str() else {
            return ToolResult::error("Missing required argument: session");
        };
        let Some(selector) = args["selector"].as_str() else {
            return ToolResult::error("Missing required argument: selector");
        };
        let Some(text) = args["text"].as_str() else {
            return ToolResult::error("Missing required argument: text");
        };
        if let Err(reason) = validate_selector(selector) {
            return ToolResult::error(reason);
        }
        if let Err(reason) = validate_type_text(text) {
            return ToolResult::error(reason);
        }
        let relay = match relay_of(ctx).await {
            Ok(relay) => relay,
            Err(result) => return result,
        };
        let request = ClientBrowserRequest {
            session: session.to_string(),
            action: ClientBrowserAction::Type {
                selector: selector.to_string(),
                text: text.to_string(),
            },
        };
        let observation = match dispatch_bounded(relay.as_ref(), request).await {
            Ok(observation) => observation,
            Err(error) => return ToolResult::error(error),
        };
        let observation = match bound_observation(observation) {
            Ok(observation) => observation,
            Err(error) => return ToolResult::error(error),
        };
        ToolResult::ok(wrap_untrusted_web_text(
            "browser_control_type",
            &render_observation(&observation),
        ))
    }
}

pub struct BrowserControlScreenshotTool;

#[async_trait]
impl Tool for BrowserControlScreenshotTool {
    fn name(&self) -> &str {
        "browser_control_screenshot"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Capture what the client-owned browser surface currently shows as a PNG screenshot, base64-encoded and capped like browser_screenshot. Fail: no client browser surface attached, or the capture exceeds the size cap."
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Browser)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["session"],
            "properties": {
                "session": { "type": "string", "description": "Client browser surface session id" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let Some(session) = args["session"].as_str() else {
            return ToolResult::error("Missing required argument: session");
        };
        let relay = match relay_of(ctx).await {
            Ok(relay) => relay,
            Err(result) => return result,
        };
        let request = ClientBrowserRequest {
            session: session.to_string(),
            action: ClientBrowserAction::Screenshot,
        };
        let observation = match dispatch_bounded(relay.as_ref(), request).await {
            Ok(observation) => observation,
            Err(error) => return ToolResult::error(error),
        };
        let observation = match bound_observation(observation) {
            Ok(observation) => observation,
            Err(error) => return ToolResult::error(error),
        };
        let Some(screenshot) = observation.screenshot_base64 else {
            return ToolResult::error("client browser surface did not return a screenshot");
        };
        ToolResult::ok(json!({ "base64_png": screenshot }).to_string())
    }
}
