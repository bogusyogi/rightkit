//! The `BrowserWorkspaceServices` trait contract, the fragment-owned
//! annotation/control state, and the production daemon adapter that
//! implements the trait against `DaemonState`.
//!
//! Lane discipline (D3 — shared browser workspace), preserved verbatim from
//! the pre-decomposition monolith:
//!
//! - This module owns the trait Agent 4 implements. The fragment itself
//!   holds the per-RightKit-session annotation store and the route
//!   handlers; the trait carries the `BrowserStore`, host policy, action
//!   log, pause flag, profile kind/label, viewport, capability gate, and the
//!   dispatch seam into the live agent. Agent 4's implementation is what
//!   reaches into `LiveSession::browser_*`, not this file.
//!
//! - Security boundaries preserved by the fragment's contract (Agent 4's
//!   implementation MUST honor these when wiring the trait impls):
//!     * SSRF guard (`rightkit_agent_coding_tools::browser_fetch::ssrf_guard_reject_reason`)
//!       runs before any navigation. Host policy is additive on top.
//!     * Browser capability gate runs before any state mutation.
//!     * Page text and console output are wrapped via
//!       `rightkit_agent_coding_tools::browser_fetch::wrap_untrusted_web_text`.
//!     * Browser profiles are ephemeral by default; named shared profiles
//!       require the operator to flip `profile_kind` through the consent
//!       endpoint below.
//!     * Page text is bounded by `MAX_TEXT_BYTES`, screenshots by
//!       `MAX_SCREENSHOT_BYTES`, console by RightKit's own 500-line cap.
//!     * Action log, host policy, pause, viewport, profile state all
//!       live with the session and are dropped with it.

use axum::Router;
use rightkit_agent_coding_tools::browser::{
    BrowserNavigationTrustV1, BrowserNetworkDecisionV1, BrowserObservationV1, BrowserStore,
};
use rightkit_agent_policy::PermissionMode;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;

use crate::api::DaemonState;

use super::policy::browser_capability_verdict;
use super::types::{
    BrowserActionEntry, BrowserProfileKind, CapabilityVerdict, ConsoleLineView, HostPolicyDecision,
    StoredAnnotation, ViewportInfo,
};

/// Re-exported for callers that want the canonical browser-store type without
/// depending on `rightkit_agent_coding_tools` directly. Matches HEAD `pub type BrowserStore`
/// in `engine/crates/tools/src/browser.rs`.
pub use rightkit_agent_coding_tools::browser::BrowserSession;

/// Injection seam the route handlers call. Agent 4 implements this against
/// `LiveSession` and the central `PermissionEngine`. Every method is invoked
/// while the appropriate session-scoped mutex is held by the fragment; the
/// impl MUST NOT block awaiting a session mutex the daemon already holds.
///
/// The fragment treats the session id as opaque — Agent 4's impl resolves it
/// to `LiveSession` internally. The fragment never imports `LiveSession`.
pub trait BrowserWorkspaceServices: Send + Sync + 'static {
    /// Capability gate. MUST mirror the workspace's `Browser` permission
    /// decision from `rightkit_agent_policy::PermissionEngine`. Returning
    /// `allowed = false` short-circuits every state mutation.
    fn capability_gate(&self, session_id: &str) -> CapabilityVerdict;

    /// Per-session browser store. Fragment holds the `Arc<BrowserStore>` only
    /// for the duration of a single handler — Agent 4's impl can clone a fresh
    /// `Arc` from `LiveSession::browser_sessions` on demand.
    fn browser_store(&self, session_id: &str) -> Option<BrowserStore>;

    /// Per-session host policy. Fragment borrows the policy via an
    /// `async fn` because `LiveSession::browser_host_policy` is a `Mutex`;
    /// the impl acquires it, returns a snapshot, and the fragment acts on the
    /// snapshot. `Allow`/`Ask` short-circuit only `Block`. `None` = no opinion.
    fn host_policy_decision(&self, session_id: &str, url: &str) -> Option<HostPolicyDecision>;

    /// Per-session action log length. Used to advertise `actionLogLen` in the
    /// workspace summary; the fragment never reads the log directly.
    fn action_log_len(&self, session_id: &str) -> usize;

    /// Per-session pause flag. When `true`, navigation/click/type/press are
    /// refused with `409 browser_paused`. The fragment calls this to gate
    /// state-mutating actions.
    fn is_paused(&self, session_id: &str) -> bool;

    /// Per-session profile state. The fragment advertises it on
    /// `get /workspace`; it refuses `name=...` on `post /open` unless
    /// `kind == NamedShared`.
    fn profile_kind(&self, session_id: &str) -> BrowserProfileKind;
    fn profile_label(&self, session_id: &str) -> Option<String>;

    /// Per-session viewport. The fragment reads this to enrich summaries and
    /// overwrites it from `post /viewport`.
    fn viewport(&self, session_id: &str) -> Option<ViewportInfo>;

    /// Append an action log entry. Fragment calls this for every successful
    /// state mutation; Agent 4's impl respects the per-session cap.
    fn push_action(&self, session_id: &str, entry: BrowserActionEntry);

    fn action_log(&self, _session_id: &str) -> Vec<BrowserActionEntry> {
        Vec::new()
    }

    /// Pause transition. Implementations must also push the transition into
    /// every live browser session: click/type/press are agent MCP tool calls
    /// that never pass through this fragment, so the session-level gate is
    /// what actually stops them.
    fn set_paused(
        &self,
        _session_id: &str,
        _paused: bool,
    ) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }

    fn set_viewport(&self, _session_id: &str, _viewport: ViewportInfo) {}

    fn zoom(&self, _session_id: &str) -> Option<f64> {
        None
    }

    fn set_zoom(&self, _session_id: &str, _zoom: f64) {}

    fn host_policy_snapshot(&self, _session_id: &str) -> Vec<(String, HostPolicyDecision)> {
        Vec::new()
    }

    fn set_host_policy(&self, _session_id: &str, _host: String, _decision: HostPolicyDecision) {}

    fn navigation_trust(&self, _session_id: &str) -> BrowserNavigationTrustV1 {
        BrowserNavigationTrustV1::default()
    }

    fn set_navigation_trust(&self, _session_id: &str, _trust: BrowserNavigationTrustV1) {}

    fn set_profile(&self, _session_id: &str, _kind: BrowserProfileKind, _label: Option<String>) {}

    /// Open a Chrome session and store it under `browser_store(session_id)`.
    /// Returns the new browser session id. The fragment does not hold the
    /// Chrome lifecycle — Agent 4's impl does.
    fn open_browser_session(
        &self,
        session_id: &str,
        url: &str,
    ) -> impl std::future::Future<Output = Result<String, String>> + Send;

    /// Close a previously-opened browser session. Returns the removed id on
    /// success or `Err` if the session was not found.
    fn close_browser_session(
        &self,
        session_id: &str,
        browser_session_id: &str,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;

    /// Atomic action seams return observation captured before browser action
    /// lane release. Every adapter must implement the underlying same-session
    /// action+observation operation; no mutate-then-observe fallback exists.
    fn navigate_browser_session_with_observation(
        &self,
        session_id: &str,
        browser_session_id: &str,
        url: &str,
    ) -> impl std::future::Future<Output = Result<BrowserObservationV1, String>> + Send;

    fn reload_browser_session_with_observation(
        &self,
        session_id: &str,
        browser_session_id: &str,
    ) -> impl std::future::Future<Output = Result<BrowserObservationV1, String>> + Send;

    fn navigate_browser_history_with_observation(
        &self,
        session_id: &str,
        browser_session_id: &str,
        forward: bool,
    ) -> impl std::future::Future<Output = Result<BrowserObservationV1, String>> + Send;

    fn apply_viewport(
        &self,
        session_id: &str,
        viewport: &ViewportInfo,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;

    fn apply_zoom(
        &self,
        session_id: &str,
        zoom: f64,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;

    fn browser_session_url(
        &self,
        session_id: &str,
        browser_session_id: &str,
    ) -> impl std::future::Future<Output = Result<String, String>> + Send;

    /// Take a PNG screenshot. Returned bytes MUST be capped at
    /// `MAX_SCREENSHOT_BYTES`. Agent 4's impl returns the raw bytes; the
    /// fragment base64-encodes + wraps before responding.
    fn screenshot_browser_session(
        &self,
        session_id: &str,
        browser_session_id: &str,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, String>> + Send;

    /// Read console lines from the browser session. Returned slice MUST be
    /// bounded by RightKit's own 500-line page cap. Fragment filters by level before
    /// responding.
    fn console_lines(
        &self,
        session_id: &str,
        browser_session_id: &str,
    ) -> impl std::future::Future<Output = Result<Vec<ConsoleLineView>, String>> + Send;

    /// Send seam — Agent 4 publishes the annotation through the live conduit
    /// so the running agent picks it up exactly like a user message. Returns
    /// the timestamp the agent's transcript accepted it at.
    fn dispatch_annotation(
        &self,
        session_id: &str,
        annotation: StoredAnnotation,
    ) -> impl std::future::Future<Output = Result<u64, String>> + Send;
}

// =============================================================================
// Annotation store owned by the fragment (not the daemon)
// =============================================================================

type AnnotationStore = Arc<Mutex<HashMap<String, Vec<StoredAnnotation>>>>;

/// Owned by the fragment; lives for the duration of the workspace. Agent 4
/// shares the same `Arc` between the HTTP router and any test harness.
#[derive(Clone, Default)]
pub struct BrowserWorkspaceState {
    annotations: AnnotationStore,
    /// Serialize multi-browser control transitions across cloned route state.
    /// Per-browser action lanes protect individual sessions; this workspace
    /// lane keeps take/release operations atomic across their browser set.
    control_transition: Arc<Mutex<()>>,
}

impl BrowserWorkspaceState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn annotations(&self) -> AnnotationStore {
        Arc::clone(&self.annotations)
    }

    pub(crate) fn control_transition_lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.control_transition)
    }
}

#[derive(Default)]
struct DaemonBrowserOperatorState {
    paused: HashSet<String>,
    actions: HashMap<String, Vec<BrowserActionEntry>>,
    host_policy: HashMap<String, HashMap<String, HostPolicyDecision>>,
    navigation_trust: HashMap<String, BrowserNavigationTrustV1>,
    profiles: HashMap<String, (BrowserProfileKind, Option<String>)>,
    viewports: HashMap<String, ViewportInfo>,
    zoom: HashMap<String, f64>,
}

/// Production adapter joining the visible pane to the exact BrowserStore used
/// by agent tools in each live session.
#[derive(Clone)]
pub struct DaemonBrowserWorkspaceServices {
    daemon: DaemonState,
    operator: Arc<StdMutex<DaemonBrowserOperatorState>>,
}

impl DaemonBrowserWorkspaceServices {
    pub fn new(daemon: DaemonState) -> Self {
        Self {
            daemon,
            operator: Arc::new(StdMutex::new(DaemonBrowserOperatorState::default())),
        }
    }

    fn with_operator<R>(&self, f: impl FnOnce(&DaemonBrowserOperatorState) -> R) -> R {
        let guard = self
            .operator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&guard)
    }

    fn with_operator_mut<R>(&self, f: impl FnOnce(&mut DaemonBrowserOperatorState) -> R) -> R {
        let mut guard = self
            .operator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }

    async fn browser_session(
        &self,
        session_id: &str,
        browser_session_id: &str,
    ) -> Result<Arc<BrowserSession>, String> {
        let store = self
            .browser_store(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        let browser = store
            .lock()
            .await
            .get(browser_session_id)
            .cloned()
            .ok_or_else(|| format!("browser session not found: {browser_session_id}"))?;
        Ok(browser)
    }
}

impl BrowserWorkspaceServices for DaemonBrowserWorkspaceServices {
    fn capability_gate(&self, session_id: &str) -> CapabilityVerdict {
        match self.daemon.get_session(session_id) {
            None => CapabilityVerdict {
                allowed: false,
                reason: "session not found".to_string(),
            },
            Some(session)
                if session.workspace_trust == rightkit_agent_policy::WorkspaceTrust::Locked =>
            {
                CapabilityVerdict {
                    allowed: false,
                    reason: "workspace is locked".to_string(),
                }
            }
            Some(session) => {
                // The visible pane and the agent tools share the Browser
                // capability boundary. Trust proves the session is real; the
                // live permission sender proves this request is currently
                // allowed. Do not let a workspace route bypass AutoSafe/Ask.
                let mode = session
                    .permission_mode_tx
                    .as_ref()
                    .map(|sender| *sender.borrow())
                    .unwrap_or(PermissionMode::Ask);
                browser_capability_verdict(mode, session.workspace_trust)
            }
        }
    }

    fn browser_store(&self, session_id: &str) -> Option<BrowserStore> {
        self.daemon
            .get_session(session_id)
            .map(|session| Arc::clone(&session.browser_sessions))
    }

    fn host_policy_decision(&self, session_id: &str, url: &str) -> Option<HostPolicyDecision> {
        let host = url::Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))?;
        self.with_operator(|state| {
            state
                .host_policy
                .get(session_id)
                .and_then(|policy| policy.get(&host))
                .copied()
        })
    }

    fn action_log_len(&self, session_id: &str) -> usize {
        self.with_operator(|state| state.actions.get(session_id).map(Vec::len).unwrap_or(0))
    }

    fn action_log(&self, session_id: &str) -> Vec<BrowserActionEntry> {
        self.with_operator(|state| state.actions.get(session_id).cloned().unwrap_or_default())
    }

    fn is_paused(&self, session_id: &str) -> bool {
        self.with_operator(|state| state.paused.contains(session_id))
    }

    async fn set_paused(&self, session_id: &str, paused: bool) {
        self.with_operator_mut(|state| {
            if paused {
                state.paused.insert(session_id.to_string());
            } else {
                state.paused.remove(session_id);
            }
        });
        // Without this propagation `pause` would mute only the four workspace
        // navigation routes and leave the agent free to keep acting on the
        // page over `browser_act`. The session gate (and the human lease, when
        // take-control is involved) is what the agent path actually checks.
        if let Some(store) = self.browser_store(session_id) {
            let browsers: Vec<_> = store.lock().await.values().cloned().collect();
            for browser in browsers {
                browser.set_operator_paused(paused);
            }
        }
    }

    fn profile_kind(&self, session_id: &str) -> BrowserProfileKind {
        self.with_operator(|state| {
            state
                .profiles
                .get(session_id)
                .map(|profile| profile.0)
                .unwrap_or(BrowserProfileKind::Ephemeral)
        })
    }

    fn profile_label(&self, session_id: &str) -> Option<String> {
        self.with_operator(|state| {
            state
                .profiles
                .get(session_id)
                .and_then(|profile| profile.1.clone())
        })
    }

    fn set_profile(&self, session_id: &str, kind: BrowserProfileKind, label: Option<String>) {
        self.with_operator_mut(|state| {
            state.profiles.insert(session_id.to_string(), (kind, label));
        });
    }

    fn viewport(&self, session_id: &str) -> Option<ViewportInfo> {
        self.with_operator(|state| state.viewports.get(session_id).cloned())
    }

    fn set_viewport(&self, session_id: &str, viewport: ViewportInfo) {
        self.with_operator_mut(|state| {
            state.viewports.insert(session_id.to_string(), viewport);
        });
    }

    fn zoom(&self, session_id: &str) -> Option<f64> {
        self.with_operator(|state| state.zoom.get(session_id).copied())
    }

    fn set_zoom(&self, session_id: &str, zoom: f64) {
        self.with_operator_mut(|state| {
            state.zoom.insert(session_id.to_string(), zoom);
        });
    }

    fn host_policy_snapshot(&self, session_id: &str) -> Vec<(String, HostPolicyDecision)> {
        self.with_operator(|state| {
            state
                .host_policy
                .get(session_id)
                .map(|policy| {
                    policy
                        .iter()
                        .map(|(host, decision)| (host.clone(), *decision))
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    fn set_host_policy(&self, session_id: &str, host: String, decision: HostPolicyDecision) {
        self.with_operator_mut(|state| {
            state
                .host_policy
                .entry(session_id.to_string())
                .or_default()
                .insert(host, decision);
        });
    }

    fn navigation_trust(&self, session_id: &str) -> BrowserNavigationTrustV1 {
        self.with_operator(|state| {
            state
                .navigation_trust
                .get(session_id)
                .cloned()
                .unwrap_or_default()
        })
    }

    fn set_navigation_trust(&self, session_id: &str, trust: BrowserNavigationTrustV1) {
        self.with_operator_mut(|state| {
            state.navigation_trust.insert(session_id.to_string(), trust);
        });
    }

    fn push_action(&self, session_id: &str, entry: BrowserActionEntry) {
        self.with_operator_mut(|state| {
            let actions = state.actions.entry(session_id.to_string()).or_default();
            actions.push(entry);
            if actions.len() > 500 {
                actions.drain(..actions.len() - 500);
            }
        });
    }

    async fn open_browser_session(&self, session_id: &str, url: &str) -> Result<String, String> {
        let store = self
            .browser_store(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        let host_policy = self
            .host_policy_snapshot(session_id)
            .into_iter()
            .map(|(host, decision)| {
                (
                    host,
                    match decision {
                        HostPolicyDecision::Allow => BrowserNetworkDecisionV1::Allow,
                        HostPolicyDecision::Ask => BrowserNetworkDecisionV1::Ask,
                        HostPolicyDecision::Block => BrowserNetworkDecisionV1::Block,
                    },
                )
            })
            .collect();
        let browser = Arc::new(
            BrowserSession::open_owned_with_navigation_trust_and_host_policy(
                url,
                session_id,
                self.navigation_trust(session_id),
                host_policy,
            )
            .await?,
        );
        let id = browser.id.clone();
        store.lock().await.insert(id.clone(), browser);
        Ok(id)
    }

    async fn close_browser_session(
        &self,
        session_id: &str,
        browser_session_id: &str,
    ) -> Result<(), String> {
        let store = self
            .browser_store(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        let browser = store
            .lock()
            .await
            .remove(browser_session_id)
            .ok_or_else(|| format!("browser session not found: {browser_session_id}"))?;
        browser.shutdown().await;
        Ok(())
    }

    async fn navigate_browser_session_with_observation(
        &self,
        session_id: &str,
        browser_session_id: &str,
        url: &str,
    ) -> Result<BrowserObservationV1, String> {
        let store = self
            .browser_store(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        let browser = store
            .lock()
            .await
            .get(browser_session_id)
            .cloned()
            .ok_or_else(|| format!("browser session not found: {browser_session_id}"))?;
        browser.navigate_with_observation(url).await
    }

    async fn reload_browser_session_with_observation(
        &self,
        session_id: &str,
        browser_session_id: &str,
    ) -> Result<BrowserObservationV1, String> {
        self.browser_session(session_id, browser_session_id)
            .await?
            .reload_with_observation()
            .await
    }

    async fn navigate_browser_history_with_observation(
        &self,
        session_id: &str,
        browser_session_id: &str,
        forward: bool,
    ) -> Result<BrowserObservationV1, String> {
        let browser = self.browser_session(session_id, browser_session_id).await?;
        if forward {
            browser.forward_with_observation().await
        } else {
            browser.back_with_observation().await
        }
    }

    async fn apply_viewport(
        &self,
        session_id: &str,
        viewport: &ViewportInfo,
    ) -> Result<(), String> {
        let store = self
            .browser_store(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        let browsers: Vec<_> = store.lock().await.values().cloned().collect();
        for browser in browsers {
            browser
                .set_viewport(
                    viewport.width,
                    viewport.height,
                    viewport.device_scale_factor,
                )
                .await?;
        }
        Ok(())
    }

    async fn apply_zoom(&self, session_id: &str, zoom: f64) -> Result<(), String> {
        let store = self
            .browser_store(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        let browsers: Vec<_> = store.lock().await.values().cloned().collect();
        for browser in browsers {
            browser.set_zoom(zoom).await?;
        }
        Ok(())
    }

    async fn browser_session_url(
        &self,
        session_id: &str,
        browser_session_id: &str,
    ) -> Result<String, String> {
        Ok(self
            .browser_session(session_id, browser_session_id)
            .await?
            .url()
            .await)
    }

    async fn screenshot_browser_session(
        &self,
        session_id: &str,
        browser_session_id: &str,
    ) -> Result<Vec<u8>, String> {
        self.browser_session(session_id, browser_session_id)
            .await?
            .screenshot()
            .await
    }

    async fn console_lines(
        &self,
        session_id: &str,
        browser_session_id: &str,
    ) -> Result<Vec<ConsoleLineView>, String> {
        let browser = self.browser_session(session_id, browser_session_id).await?;
        Ok(browser
            .console_lines()
            .await
            .into_iter()
            .map(|line| ConsoleLineView {
                level: line.level,
                text: line.text,
            })
            .collect())
    }

    async fn dispatch_annotation(
        &self,
        session_id: &str,
        annotation: StoredAnnotation,
    ) -> Result<u64, String> {
        let session = self
            .daemon
            .get_session(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        session
            .send_route_bound_text(super::annotations::render_annotation_message(&annotation))
            .await?;
        Ok(super::types::now_millis())
    }
}

pub fn daemon_browser_workspace_routes(daemon: DaemonState) -> Router<DaemonState> {
    super::routes::browser_workspace_routes(
        Arc::new(DaemonBrowserWorkspaceServices::new(daemon)),
        BrowserWorkspaceState::new(),
    )
    .with_state::<DaemonState>(())
}
