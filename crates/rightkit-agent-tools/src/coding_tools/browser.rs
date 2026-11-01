//! Browser control — a live Chrome session the model can drive.
//!
//! This is the capability `browser_fetch` deliberately does not have. `browser_fetch`
//! reads a page; these tools *operate* one: navigate, click, type, read what rendered,
//! screenshot it, and see what the page logged. The loop it exists for is the agent
//! writing frontend code, opening its own dev server, and looking at the result.
//!
//! Chrome runs locally (Chrome for Testing, then system Chrome or Edge) and is driven over
//! CDP. Local matters: a remote browser cannot reach the `localhost:5173` the agent just
//! started, which is the entire point.
//!
//! Sessions are held in a per-the host app-session store — the same `Arc<Mutex<HashMap<..>>>`
//! shape `background_shell` uses for long-lived processes — because a browser is stateful:
//! you navigate, then click, then read, across separate tool calls.
//!
//! S:A15g action-loop watchdog adapts Browser Use
//! `agent/views.py::{ActionLoopDetector, ActionResult}@9a2db2d2`: normalized
//! action signatures stay in a bounded rolling window, repeated cycles and
//! stagnant observation fingerprints escalate at tiers 5/8/12, and a human
//! control handoff resets the watchdog. the host app retains its daemon session,
//! permission, and control-lease seams around that mechanism.

use crate::coding_tools::browser_fetch::wrap_untrusted_web_text;
use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_policy::net_guard::{
    resolve_then_reject_private_allow_loopback_with_trust_and_allowlist, TrustedNavigationPolicy,
};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use rightkit_browser::{
    BrowserPage, DownloadBehavior, InterceptDecision, InterceptFailure, Target,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Per-the host app-session map of live browser sessions, keyed by the id handed back
/// from `browser_open`.
pub type BrowserStore = Arc<Mutex<HashMap<String, Arc<BrowserSession>>>>;

static SESSION_SEQ: AtomicU64 = AtomicU64::new(0);

/// Page text is capped so a heavy app can't blow the context window in one read.
const MAX_TEXT_BYTES: usize = 50_000;
/// Screenshots are capped well below `view_image`'s 20 MB — a viewport PNG that
/// exceeds this is a bug, not a screenshot.
pub const MAX_SCREENSHOT_BYTES: usize = 4 * 1024 * 1024;
const MAX_SEMANTIC_NODES: usize = 300;
const MAX_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_NAVIGATION_URL_BYTES: usize = 8 * 1024;
const ACTION_LOOP_WINDOW: usize = 20;
const MAX_STATIC_REFS: usize = MAX_SEMANTIC_NODES * 8;
const SENSITIVE_FIELD_MASK: &str = "••••••••";

/// Runtime mode for one daemon-owned browser session. A visible handoff does
/// not create a second cookie jar or automation session; it changes control of
/// this exact session and invalidates every outstanding model action ticket.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserModeV1 {
    Headless,
    Visible,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserProfileV1 {
    pub kind: String,
    pub label: Option<String>,
    pub ephemeral: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserControlLeaseV1 {
    pub owner: Option<String>,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticBoundsV1 {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticNodeV1 {
    pub node_ref: String,
    pub role: String,
    pub name: String,
    pub tag: String,
    pub selector: String,
    pub frame_id: String,
    pub visible: bool,
    pub disabled: bool,
    pub bounds: SemanticBoundsV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AxNodeSummaryV1 {
    pub role: String,
    pub name: String,
    pub frame_id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationDiffV1 {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserObservationV1 {
    pub schema_version: u8,
    pub browser_session_id: String,
    pub owner_session_id: String,
    pub tab_id: String,
    pub frame_ids: Vec<String>,
    pub epoch: u64,
    pub url: String,
    pub title: String,
    pub viewport: SemanticBoundsV1,
    pub ax_nodes: Vec<AxNodeSummaryV1>,
    pub dom_fallback: Vec<SemanticNodeV1>,
    pub diff: ObservationDiffV1,
    pub screenshot_artifact: Option<String>,
    pub console_errors: usize,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSemanticNode {
    role: String,
    name: String,
    tag: String,
    selector: String,
    frame_id: String,
    visible: bool,
    disabled: bool,
    #[serde(default)]
    sensitive: bool,
    bounds: SemanticBoundsV1,
}

#[derive(Default)]
struct ObservationState {
    epoch: u64,
    fingerprint: Option<String>,
    previous: BTreeMap<String, (String, String, String, bool, bool)>,
    selectors: HashMap<String, String>,
    static_refs: HashMap<String, u64>,
    static_ref_order: VecDeque<String>,
    next_static_ref: u64,
}

/// Bounded intent history plus same-observation streak used by action-loop
/// escalation. Signatures are normalized before insertion.
#[derive(Default)]
struct ActionLoopState {
    recent: VecDeque<String>,
    last_observation: Option<String>,
    stagnant_observations: u8,
}

struct BrowserControlState {
    owner_session_id: String,
    profile: BrowserProfileV1,
    mode: std::sync::Mutex<BrowserModeV1>,
    lease_owner: std::sync::Mutex<Option<String>>,
    /// Operator pause gate. Unlike the lease this blocks agent actions even
    /// when no human owns the lease; the lease stays the exclusive-control
    /// boundary (A15f) and pausing never implies control transfer.
    operator_paused: std::sync::Mutex<bool>,
    generation: AtomicU64,
    observation: std::sync::Mutex<ObservationState>,
    host_policy: Arc<std::sync::Mutex<HashMap<String, BrowserNetworkDecisionV1>>>,
    action_loop: std::sync::Mutex<ActionLoopState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserNetworkDecisionV1 {
    Allow,
    Ask,
    Block,
}

/// Explicit operator-approved private-target exceptions. Host allow/ask/block
/// policy stays separate: ordinary host allowance cannot weaken SSRF policy.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserNavigationTrustV1 {
    pub trusted_domains: Vec<String>,
    pub trusted_cidrs: Vec<String>,
}

impl BrowserNavigationTrustV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.policy().map(|_| ())
    }

    fn policy(&self) -> Result<TrustedNavigationPolicy, String> {
        TrustedNavigationPolicy::from_parts(
            self.trusted_domains.clone(),
            self.trusted_cidrs.clone(),
        )
        .map_err(|error| error.to_string())
    }
}

impl BrowserControlState {
    fn new(owner_session_id: String) -> Self {
        Self::with_profile(
            owner_session_id,
            BrowserProfileV1 {
                kind: "ephemeral".to_string(),
                label: None,
                ephemeral: true,
            },
            BrowserModeV1::Headless,
        )
    }

    fn with_profile(
        owner_session_id: String,
        profile: BrowserProfileV1,
        mode: BrowserModeV1,
    ) -> Self {
        Self {
            owner_session_id,
            profile,
            mode: std::sync::Mutex::new(mode),
            lease_owner: std::sync::Mutex::new(None),
            operator_paused: std::sync::Mutex::new(false),
            generation: AtomicU64::new(1),
            observation: std::sync::Mutex::new(ObservationState::default()),
            host_policy: Arc::new(std::sync::Mutex::new(HashMap::new())),
            action_loop: std::sync::Mutex::new(ActionLoopState::default()),
        }
    }

    fn action_ticket(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn ensure_agent_control(&self, ticket: u64) -> Result<(), String> {
        let current = self.generation.load(Ordering::Acquire);
        if current != ticket {
            return Err("browser action invalidated by navigation or control transition".into());
        }
        let owner = self
            .lease_owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if owner.is_some() {
            return Err("browser control is leased to the user".into());
        }
        if *self
            .operator_paused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return Err(
                "browser actions are paused by the operator; resume the workspace or take control to continue"
                    .into(),
            );
        }
        Ok(())
    }

    fn set_operator_paused(&self, paused: bool) {
        *self
            .operator_paused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = paused;
    }

    fn operator_paused(&self) -> bool {
        *self
            .operator_paused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn invalidate_actions(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        let mut observation = self
            .observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        observation.epoch = observation.epoch.saturating_add(1).max(1);
        observation.fingerprint = None;
        observation.selectors.clear();
    }

    fn record_action(&self, signature: String) -> Result<Option<String>, String> {
        let mut state = self
            .action_loop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.recent.push_back(signature);
        if state.recent.len() > ACTION_LOOP_WINDOW {
            state.recent.pop_front();
        }
        let level = repeated_action_cycle_depth(&state.recent)
            .max(usize::from(state.stagnant_observations));
        if level >= 12 {
            return Err(
                "browser action loop hard-stop at tier 12; take control or observe a changed page before continuing"
                    .to_string(),
            );
        }
        if level >= 8 {
            return Ok(Some(
                "browser action-loop escalation tier 8: request human control before continuing"
                    .to_string(),
            ));
        }
        if level >= 5 {
            return Ok(Some(
                "browser action-loop warning tier 5: observe a changed page before repeating this pattern"
                    .to_string(),
            ));
        }
        Ok(None)
    }

    fn record_observation_fingerprint(&self, fingerprint: String) {
        let mut state = self
            .action_loop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.last_observation.as_deref() == Some(&fingerprint) {
            state.stagnant_observations = state.stagnant_observations.saturating_add(1);
        } else {
            state.last_observation = Some(fingerprint);
            state.stagnant_observations = 0;
        }
    }

    fn clear_action_loop(&self) {
        *self
            .action_loop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = ActionLoopState::default();
    }

    fn resolve_ref(&self, node_ref: &str) -> Result<String, String> {
        self.observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .selectors
            .get(node_ref)
            .cloned()
            .ok_or_else(|| "stale or foreign browser ref; observe again before acting".to_string())
    }

    fn take_control(&self, owner: String) -> Result<BrowserControlLeaseV1, String> {
        let mut current = self
            .lease_owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(current_owner) = current.as_deref() {
            if current_owner == owner {
                return Ok(BrowserControlLeaseV1 {
                    owner: Some(owner),
                    generation: self.generation.load(Ordering::Acquire),
                });
            }
            return Err("browser control lease is already owned by another actor".into());
        }
        *current = Some(owner.clone());
        drop(current);
        *self
            .mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = BrowserModeV1::Visible;
        self.invalidate_actions();
        self.clear_action_loop();
        Ok(BrowserControlLeaseV1 {
            owner: Some(owner),
            generation: self.generation.load(Ordering::Acquire),
        })
    }

    fn release_control(&self, owner: &str) -> Result<BrowserControlLeaseV1, String> {
        let mut current = self
            .lease_owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.as_deref() != Some(owner) {
            return Err("browser control lease is owned by another actor".into());
        }
        *current = None;
        drop(current);
        self.invalidate_actions();
        self.clear_action_loop();
        Ok(BrowserControlLeaseV1 {
            owner: None,
            generation: self.generation.load(Ordering::Acquire),
        })
    }
}

/// One line the page logged — a `console.*` call or an uncaught exception.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ConsoleLine {
    pub level: String,
    pub text: String,
}

/// A live browser: one Chrome process, one page, and the console it has produced
/// since the session opened.
pub struct BrowserSession {
    pub id: String,
    control: BrowserControlState,
    /// One serialized CDP mutation lane per tab. The model may issue calls in
    /// parallel, but they commit in order and re-check the lease generation.
    action_lane: Mutex<()>,
    browser: rightkit_browser::BrowserSession,
    /// RightKit owns the page's console, exception and request-interception pumps.
    page: Arc<BrowserPage>,
    /// The only top-level origin this session may navigate to. Every Document
    /// request is intercepted and revalidated, including redirects.
    approved_origin: Arc<tokio::sync::RwLock<Option<String>>>,
    navigation_trust: Arc<std::sync::Mutex<BrowserNavigationTrustV1>>,
    /// Chrome refuses remote debugging against the default profile, and two sessions
    /// sharing one profile fight over it. An ordinary session therefore gets a throwaway
    /// profile in this directory, deleted when the session drops. A consented session
    /// (R22) runs on a private copy of the user's profile under the consent folder and
    /// keeps only its downloads here. Either way a browser the host app opens never touches
    /// the user's live Chrome profile.
    _profile: tempfile::TempDir,
    /// The consent receipt this session runs under, when it reuses the user's sign-ins.
    consent_id: Option<String>,
    download_root: PathBuf,
    /// ResourceGovernor permits are owned by the same object that owns Chrome.
    /// Dropping the BrowserSession therefore releases the BrowserProcess,
    /// BrowserContext, and aggregate ExternalExecution slots only after the
    /// actual session lifetime ends.
    _resource_admission: crate::coding_tools::process_resource_authority::ProcessAdmission,
    _context_admission: crate::coding_tools::process_resource_authority::SpecificProcessAdmission,
}

impl BrowserSession {
    /// Launch Chrome, start pumping its event stream, and navigate to `url`.
    ///
    /// The console listeners are attached *before* the first navigation on purpose:
    /// a script that throws on load fires immediately, and a listener registered
    /// after `goto` races it and silently misses the very error we opened the page
    /// to find.
    pub async fn open(url: &str) -> Result<Self, String> {
        Self::open_owned(url, "unbound").await
    }

    pub async fn open_owned(url: &str, owner_session_id: &str) -> Result<Self, String> {
        Self::open_owned_with_navigation_trust(
            url,
            owner_session_id,
            BrowserNavigationTrustV1::default(),
        )
        .await
    }

    pub async fn open_owned_with_navigation_trust(
        url: &str,
        owner_session_id: &str,
        navigation_trust: BrowserNavigationTrustV1,
    ) -> Result<Self, String> {
        Self::open_owned_with_navigation_trust_and_host_policy(
            url,
            owner_session_id,
            navigation_trust,
            Vec::new(),
        )
        .await
    }

    /// Launch Chrome with daemon-approved host policy installed before first navigation.
    pub async fn open_owned_with_navigation_trust_and_host_policy(
        url: &str,
        owner_session_id: &str,
        navigation_trust: BrowserNavigationTrustV1,
        initial_host_policy: Vec<(String, BrowserNetworkDecisionV1)>,
    ) -> Result<Self, String> {
        Self::launch(
            url,
            owner_session_id,
            navigation_trust,
            initial_host_policy,
            None,
        )
        .await
    }

    /// Launch a session that reuses the user's real sign-ins under a consent receipt
    /// (R3, R22, R25): a private copy of the consented profile, confined to the consented
    /// domains, stopped when the consent is revoked.
    pub async fn open_consented(
        url: &str,
        owner_session_id: &str,
        navigation_trust: BrowserNavigationTrustV1,
        initial_host_policy: Vec<(String, BrowserNetworkDecisionV1)>,
        consent: crate::coding_tools::browser_consent::ConsentedLaunch,
    ) -> Result<Arc<Self>, String> {
        let receipt_id = consent.receipt.id.clone();
        let session = Arc::new(
            Self::launch(
                url,
                owner_session_id,
                navigation_trust,
                initial_host_policy,
                Some(consent),
            )
            .await?,
        );
        crate::coding_tools::browser_consent::register_session(&receipt_id, &session);
        Ok(session)
    }

    async fn launch(
        url: &str,
        owner_session_id: &str,
        navigation_trust: BrowserNavigationTrustV1,
        initial_host_policy: Vec<(String, BrowserNetworkDecisionV1)>,
        consent: Option<crate::coding_tools::browser_consent::ConsentedLaunch>,
    ) -> Result<Self, String> {
        navigation_trust.validate()?;
        let profile = tempfile::tempdir()
            .map_err(|e| format!("could not create a browser profile directory: {e}"))?;
        let download_root = profile.path().join("downloads");
        std::fs::create_dir(&download_root)
            .map_err(|e| format!("could not create browser download directory: {e}"))?;

        // Resolve workspace junctions and strip Windows device-path spelling
        // before passing the isolated directory to Chromium.
        let profile_path = browser_profile_path(profile.path());
        // the host app's existing document guard owns SSRF, navigation trust &
        // host policy. Keep one Fetch interceptor while RightKit owns Chrome's
        // discovery, process tree & bounded graceful shutdown.
        let mut options = rightkit_browser::LaunchOptions::default()
            .network(rightkit_browser::NetworkPolicy::unrestricted())
            .on_event(|event| tracing::debug!(?event, "browser lifecycle"));
        options.download_dir = Some(download_root.clone());
        // Chrome cannot run its own sandbox as root. Containers and CI run as root,
        // so the operator may opt out of Chrome's sandbox there explicitly; without
        // that opt-in a root launch is refused with the reason, never weakened.
        if running_as_root() {
            if std::env::var("RIGHTKIT_BROWSER_NO_SANDBOX").as_deref() == Ok("1") {
                options.extra_args.push("--no-sandbox".to_string());
            } else {
                return Err(
                    "the browser cannot start as root: Chrome refuses to run its sandbox for \
                     the root user. Run the host app as a normal user, or, in a disposable container \
                     or CI, set RIGHTKIT_BROWSER_NO_SANDBOX=1 to run Chrome without its sandbox."
                        .to_string(),
                );
            }
        }
        let consent_plan = match &consent {
            None => {
                options = options
                    .system_chrome()
                    .named_profile(&profile_path, "chrome");
                None
            }
            Some(consent) => Some(consented_launch_options(consent, &mut options)?),
        };

        // Chromium bypasses the generic ProcessLaunchAdapter, so acquire its
        // process reservation immediately before the only actual spawn point.
        // Missing host composition and profile saturation both fail closed here.
        let resource_admission = crate::coding_tools::process_resource_authority::reserve_process(
            crate::coding_tools::process_resource_authority::ResourceKind::BrowserProcess,
            rightkit_agent_engine_runtime_resources::ServiceScope::Session(
                owner_session_id.to_string(),
            ),
            true,
        )
        .map_err(|error| format!("browser resource admission failed before launch: {error}"))?;
        // A browser session is one isolated CDP context. Pages within it are
        // deliberately uncounted. Acquire this specific permit only after the
        // process admission so aggregate ExternalExecution is charged once.
        let context_admission =
            match crate::coding_tools::process_resource_authority::reserve_specific_process(
                crate::coding_tools::process_resource_authority::ResourceKind::BrowserContext,
                rightkit_agent_engine_runtime_resources::ServiceScope::Session(
                    owner_session_id.to_string(),
                ),
                true,
            ) {
                Ok(admission) => admission,
                Err(error) => {
                    drop(resource_admission);
                    return Err(format!(
                        "browser context admission failed before launch: {error}"
                    ));
                }
            };

        let browser = match (&consent, consent_plan) {
            (Some(consent), Some(ConsentedPlan::Attach)) => {
                rightkit_browser::BrowserSession::attach_user_browser_revocable(
                    consent.receipt.clone(),
                    consent.revocation.clone(),
                    consent.profiles_root.clone(),
                    options,
                )
                .await
                .map_err(|e| {
                    format!("failed to attach to a private copy of the consented profile: {e}")
                })?
            }
            (Some(_), _) => rightkit_browser::BrowserSession::launch(options)
                .await
                .map_err(|e| {
                    format!("failed to open a private copy of the consented profile: {e}")
                })?,
            (None, _) => rightkit_browser::BrowserSession::launch(options)
                .await
                .map_err(|e| {
                    format!(
                        "failed to launch Chrome with isolated profile {}: {e}",
                        profile_path.display()
                    )
                })?,
        };

        let page = browser
            .new_page("about:blank")
            .await
            .map_err(|e| format!("failed to open a page: {e}"))?;
        page.set_download_behavior(&DownloadBehavior::AllowAndName {
            dir: download_root.clone(),
        })
        .await
        .map_err(|e| format!("could not contain browser downloads: {e}"))?;

        let approved_origin = Arc::new(tokio::sync::RwLock::new(None));
        let navigation_trust = Arc::new(std::sync::Mutex::new(navigation_trust));
        let control = match &consent {
            None => BrowserControlState::new(owner_session_id.to_string()),
            Some(consent) => BrowserControlState::with_profile(
                owner_session_id.to_string(),
                BrowserProfileV1 {
                    kind: "imported".to_string(),
                    label: Some(consent.receipt.id.clone()),
                    ephemeral: false,
                },
                match consent.mode {
                    crate::coding_tools::browser_consent::ConsentedMode::Import => {
                        BrowserModeV1::Headless
                    }
                    crate::coding_tools::browser_consent::ConsentedMode::Attach => {
                        BrowserModeV1::Visible
                    }
                },
            ),
        };
        control
            .host_policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(initial_host_policy);
        let host_policy = Arc::clone(&control.host_policy);
        install_document_guard(
            &page,
            Arc::clone(&approved_origin),
            Arc::clone(&navigation_trust),
            host_policy,
        )
        .await?;

        let session = Self {
            id: format!("bs-{}", SESSION_SEQ.fetch_add(1, Ordering::Relaxed)),
            control,
            action_lane: Mutex::new(()),
            browser,
            page,
            approved_origin,
            navigation_trust,
            _profile: profile,
            consent_id: consent.as_ref().map(|c| c.receipt.id.clone()),
            download_root,
            _resource_admission: resource_admission,
            _context_admission: context_admission,
        };
        session.navigate(url).await?;
        Ok(session)
    }

    pub fn owner_session_id(&self) -> &str {
        &self.control.owner_session_id
    }

    /// The consent receipt this session runs under, or `None` for an ephemeral session.
    pub fn consent_id(&self) -> Option<&str> {
        self.consent_id.as_deref()
    }

    pub fn profile(&self) -> BrowserProfileV1 {
        self.control.profile.clone()
    }

    pub fn mode(&self) -> BrowserModeV1 {
        *self
            .control
            .mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn control_lease(&self) -> BrowserControlLeaseV1 {
        BrowserControlLeaseV1 {
            owner: self
                .control
                .lease_owner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            generation: self.control.generation.load(Ordering::Acquire),
        }
    }

    pub async fn take_human_control(
        &self,
        owner: impl Into<String>,
    ) -> Result<BrowserControlLeaseV1, String> {
        let _lane = self.action_lane.lock().await;
        self.control.take_control(owner.into())
    }

    pub async fn release_human_control(
        &self,
        owner: &str,
    ) -> Result<BrowserControlLeaseV1, String> {
        let _lane = self.action_lane.lock().await;
        self.control.release_control(owner)
    }

    /// Operator pause transition. Propagated by the daemon workspace when the
    /// operator pauses or resumes automation, so the agent act path refuses
    /// page mutations even without a human lease. Human take-control stays
    /// available while paused — pausing never removes the escape hatch.
    pub fn set_operator_paused(&self, paused: bool) {
        self.control.set_operator_paused(paused);
    }

    pub fn operator_paused(&self) -> bool {
        self.control.operator_paused()
    }

    pub fn set_host_policy(&self, host: impl Into<String>, decision: BrowserNetworkDecisionV1) {
        self.control
            .host_policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(host.into().to_ascii_lowercase(), decision);
    }

    pub fn navigation_trust(&self) -> BrowserNavigationTrustV1 {
        self.navigation_trust
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_navigation_trust(&self, trust: BrowserNavigationTrustV1) -> Result<(), String> {
        trust.validate()?;
        *self
            .navigation_trust
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = trust;
        Ok(())
    }

    pub async fn navigate(&self, url: &str) -> Result<(), String> {
        self.navigate_with_observation(url).await.map(|_| ())
    }

    /// Navigate and capture post-action state while retaining this session's
    /// action lane, so no concurrent action can split mutation from observe.
    pub async fn navigate_with_observation(
        &self,
        url: &str,
    ) -> Result<BrowserObservationV1, String> {
        let ticket = self.control.action_ticket();
        let _lane = self.action_lane.lock().await;
        self.control.ensure_agent_control(ticket)?;
        self.control.record_action(format!("navigate:{url}"))?;
        self.navigate_locked(url).await?;
        self.observe_locked().await
    }

    async fn navigate_locked(&self, url: &str) -> Result<(), String> {
        let trust = self.navigation_trust();
        let host_allowlisted = self.host_allowlisted(url);
        // SSRF guard first: every outbound navigation resolves here — literal-IP
        // fast path or DNS — and must clear the private-target check before the
        // `goto` below hands the URL to Chrome. It also stays ahead of the
        // approved-origin write, so a refused target cannot widen the origin.
        if let Some(reason) = browser_navigation_reject_reason_with_allowlist(
            "navigate to",
            url,
            &trust,
            host_allowlisted,
        )
        .await
        {
            return Err(reason);
        }
        let origin = http_origin(url)?;
        let host = url::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .ok_or_else(|| "browser navigation URL has no host".to_string())?;
        match self
            .control
            .host_policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&host)
            .copied()
        {
            Some(BrowserNetworkDecisionV1::Block) => {
                return Err(format!("browser host policy blocks '{host}'"));
            }
            Some(BrowserNetworkDecisionV1::Ask) => {
                return Err(format!(
                    "browser host policy requires approval for '{host}'"
                ));
            }
            _ => {}
        }
        *self.approved_origin.write().await = Some(origin);
        // RightKit's goto waits for the navigation to settle.
        self.page
            .goto(url)
            .await
            .map_err(|e| format!("navigation to '{url}' failed: {e}"))?;
        self.control.invalidate_actions();
        Ok(())
    }

    fn host_allowlisted(&self, url: &str) -> bool {
        url::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .and_then(|host| {
                self.control
                    .host_policy
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&host)
                    .copied()
            })
            == Some(BrowserNetworkDecisionV1::Allow)
    }

    /// Reload the current page without replacing it with a placeholder URL.
    pub async fn reload(&self) -> Result<(), String> {
        self.reload_with_observation().await.map(|_| ())
    }

    pub async fn reload_with_observation(&self) -> Result<BrowserObservationV1, String> {
        let ticket = self.control.action_ticket();
        let _lane = self.action_lane.lock().await;
        self.control.ensure_agent_control(ticket)?;
        self.control.record_action("reload".to_string())?;
        self.reload_locked().await?;
        self.observe_locked().await
    }

    async fn reload_locked(&self) -> Result<(), String> {
        let url = self.url().await;
        if url.is_empty() {
            return Err("cannot reload a browser session with no current URL".to_string());
        }
        self.navigate_locked(&url).await
    }

    pub async fn back(&self) -> Result<(), String> {
        self.back_with_observation().await.map(|_| ())
    }

    pub async fn back_with_observation(&self) -> Result<BrowserObservationV1, String> {
        let ticket = self.control.action_ticket();
        let _lane = self.action_lane.lock().await;
        self.control.ensure_agent_control(ticket)?;
        self.control.record_action("back".to_string())?;
        self.navigate_history_locked("back").await?;
        self.observe_locked().await
    }

    pub async fn forward(&self) -> Result<(), String> {
        self.forward_with_observation().await.map(|_| ())
    }

    pub async fn forward_with_observation(&self) -> Result<BrowserObservationV1, String> {
        let ticket = self.control.action_ticket();
        let _lane = self.action_lane.lock().await;
        self.control.ensure_agent_control(ticket)?;
        self.control.record_action("forward".to_string())?;
        self.navigate_history_locked("forward").await?;
        self.observe_locked().await
    }

    async fn navigate_history_locked(&self, direction: &str) -> Result<(), String> {
        let delta = if direction == "back" { -1 } else { 1 };
        // `BrowserPage::history` bounds its own navigation wait (10 s, rightkit-browser
        // 0.3.4): a back-forward-cache or same-document traversal returns and the
        // observation reads the settled page.
        self.page
            .history(delta)
            .await
            .map_err(|error| format!("could not navigate browser history {direction}: {error}"))?;
        self.control.invalidate_actions();
        Ok(())
    }

    /// Apply the visible workspace viewport to the actual CDP page.
    pub async fn set_viewport(
        &self,
        width: u32,
        height: u32,
        device_scale_factor: f64,
    ) -> Result<(), String> {
        self.page
            .set_viewport(width, height, device_scale_factor)
            .await
            .map_err(|error| format!("could not set browser viewport: {error}"))?;
        Ok(())
    }

    /// Apply the visible workspace zoom to the actual CDP page.
    pub async fn set_zoom(&self, zoom: f64) -> Result<(), String> {
        self.page
            .set_page_scale(zoom)
            .await
            .map_err(|error| format!("could not set browser zoom: {error}"))?;
        Ok(())
    }

    pub async fn url(&self) -> String {
        self.page.url().await
    }

    pub async fn title(&self) -> String {
        self.page.title().await
    }

    /// The rendered text of the page — what a human would actually read, not the HTML.
    async fn text(&self) -> Result<String, String> {
        let text: String = self
            .page
            .text()
            .await
            .map_err(|e| format!("could not read page text: {e}"))?;
        // 7b: a rendered page can echo back a key/token from a query string,
        // dev console, or debug panel — mask before it's bounded.
        Ok(truncate(
            crate::coding_tools::output_compaction::mask_secrets(&text),
            MAX_TEXT_BYTES,
        ))
    }

    /// Capture the semantic and visual state under the same per-tab lane used
    /// by actions. Refs are never selectors in disguise: they are scoped to
    /// session, tab, frame and epoch and resolve only through this snapshot.
    pub async fn observe(&self) -> Result<BrowserObservationV1, String> {
        let _lane = self.action_lane.lock().await;
        self.observe_locked().await
    }

    /// Read rendered text and semantic state without allowing an action to
    /// interleave between them. This makes a `browser_read` response one
    /// coherent observation rather than two independently sampled states.
    pub async fn read_observation(&self) -> Result<(String, BrowserObservationV1), String> {
        let _lane = self.action_lane.lock().await;
        let text = self.text().await?;
        let observation = self.observe_locked().await?;
        Ok((text, observation))
    }

    async fn observe_locked(&self) -> Result<BrowserObservationV1, String> {
        // The semantic capture is `rightkit_browser::BrowserPage::observe` (main document only;
        // iframe contents are not enumerated). the host app keeps its own scoped refs, epochs and
        // diffs on top of the nodes it returns.
        let observed = self
            .page
            .observe()
            .await
            .map_err(|e| format!("could not capture semantic DOM: {e}"))?;
        let viewport = SemanticBoundsV1 {
            x: observed.viewport.x,
            y: observed.viewport.y,
            width: observed.viewport.width,
            height: observed.viewport.height,
        };
        let raw = observed
            .nodes
            .into_iter()
            .map(|node| RawSemanticNode {
                role: node.role,
                name: sanitized_snapshot_name(&node.name, node.sensitive),
                tag: node.tag,
                selector: node.selector,
                frame_id: node.frame_id,
                visible: node.visible,
                disabled: node.disabled,
                sensitive: node.sensitive,
                bounds: SemanticBoundsV1 {
                    x: node.bounds.x,
                    y: node.bounds.y,
                    width: node.bounds.width,
                    height: node.bounds.height,
                },
            })
            .collect::<Vec<_>>();

        let mut frame_ids: Vec<String> = Vec::new();
        for frame in observed.ax.iter().filter(|node| !node.frame_id.is_empty()) {
            if !frame_ids.contains(&frame.frame_id) {
                frame_ids.push(frame.frame_id.clone());
            }
        }
        let ax_nodes = observed
            .ax
            .iter()
            .take(MAX_SEMANTIC_NODES)
            .map(|node| AxNodeSummaryV1 {
                role: node.role.clone(),
                name: mask_snapshot_text(&node.name),
                frame_id: if node.frame_id.is_empty() {
                    "ax-main".to_string()
                } else {
                    ax_frame_id(&node.frame_id)
                },
            })
            .collect::<Vec<_>>();

        // The CDP target id is the tab's stable identity (`BrowserPage::target_id`); the
        // session's own `id()` is a sequence that can renumber.
        let tab_id = self.page.target_id().to_string();

        let (epoch, diff, dom_fallback, observation_fingerprint) = {
            let mut state = self
                .control
                .observation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let fingerprint = snapshot_fingerprint(&raw, &ax_nodes);
            let epoch = advance_snapshot_epoch(&mut state, fingerprint.clone());
            let mut current = raw
                .iter()
                .filter(|node| snapshot_node_is_diffable(node))
                .map(|node| {
                    (
                        snapshot_dom_key(&node.frame_id, &node.selector),
                        (
                            node.role.clone(),
                            node.name.clone(),
                            node.frame_id.clone(),
                            node.visible,
                            node.disabled,
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            for node in &ax_nodes {
                current
                    .entry(snapshot_ax_key(&node.frame_id, &node.role, &node.name))
                    .or_insert_with(|| {
                        (
                            node.role.clone(),
                            node.name.clone(),
                            node.frame_id.clone(),
                            true,
                            false,
                        )
                    });
            }
            let diff = observation_diff(&state.previous, &current);
            state.selectors.clear();
            let dom_fallback = raw
                .into_iter()
                .filter_map(|node| {
                    if !snapshot_node_is_refable(&node) {
                        return None;
                    }
                    let static_ref =
                        stable_snapshot_ref(&mut state, &node.frame_id, &node.selector);
                    let node_ref =
                        snapshot_node_ref(&self.id, &tab_id, &node.frame_id, epoch, static_ref);
                    state
                        .selectors
                        .insert(node_ref.clone(), node.selector.clone());
                    Some(SemanticNodeV1 {
                        node_ref,
                        role: node.role,
                        name: node.name,
                        tag: node.tag,
                        selector: node.selector,
                        frame_id: node.frame_id,
                        visible: node.visible,
                        disabled: node.disabled,
                        bounds: node.bounds,
                    })
                })
                .collect::<Vec<_>>();
            state.previous = current;
            (epoch, diff, dom_fallback, fingerprint)
        };
        self.control
            .record_observation_fingerprint(observation_fingerprint);

        let console_errors = self
            .console_lines()
            .await
            .into_iter()
            .filter(|line| matches!(line.level.as_str(), "error" | "exception"))
            .count();
        // Observation cannot advertise an artifact until screenshot bytes are
        // persisted in the canonical artifact store under a resolvable key.
        let screenshot_artifact = None;

        let url = mask_snapshot_text(&self.url().await);
        let title = mask_snapshot_text(&self.title().await);
        Ok(BrowserObservationV1 {
            schema_version: 1,
            browser_session_id: self.id.clone(),
            owner_session_id: self.owner_session_id().to_string(),
            tab_id,
            frame_ids,
            epoch,
            url,
            title,
            viewport,
            ax_nodes,
            dom_fallback,
            diff,
            screenshot_artifact,
            console_errors,
        })
    }

    fn resolve_ref(&self, node_ref: &str) -> Result<String, String> {
        self.control.resolve_ref(node_ref)
    }

    pub fn download_root(&self) -> &Path {
        &self.download_root
    }

    pub fn contained_download_path(&self, suggested_name: &str) -> Result<PathBuf, String> {
        let sanitized = sanitize_download_name(suggested_name)?;
        Ok(self.download_root.join(sanitized))
    }

    async fn unique_selector(&self, selector: &str) -> Result<(), String> {
        let count: u64 = self
            .page
            .eval(&format!(
                "document.querySelectorAll({}).length",
                serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into())
            ))
            .await
            .map_err(|e| format!("could not verify selector '{selector}': {e}"))?
            .as_u64()
            .ok_or_else(|| format!("could not verify selector '{selector}': non-numeric count"))?;
        match count {
            1 => Ok(()),
            0 => Err(format!("no element matched '{selector}'")),
            n => Err(format!("selector '{selector}' is ambiguous ({n} matches)")),
        }
    }

    async fn upload_file(
        &self,
        selector: &str,
        file: &Path,
        workspace: &Path,
    ) -> Result<(), String> {
        let workspace = workspace
            .canonicalize()
            .map_err(|e| format!("could not resolve workspace: {e}"))?;
        let file = file
            .canonicalize()
            .map_err(|e| format!("could not resolve upload: {e}"))?;
        if !file.starts_with(&workspace) || !file.is_file() {
            return Err("upload must be a regular file inside the workspace".into());
        }
        let metadata =
            std::fs::metadata(&file).map_err(|e| format!("could not stat upload: {e}"))?;
        if metadata.len() > MAX_UPLOAD_BYTES {
            return Err(format!("upload exceeds {MAX_UPLOAD_BYTES}-byte limit"));
        }
        self.unique_selector(selector).await?;
        self.page
            .upload(&Target::css(selector), &file)
            .await
            .map_err(|e| format!("could not upload file: {e}"))?;
        Ok(())
    }

    async fn click(&self, selector: &str) -> Result<(), String> {
        self.page
            .click(&Target::css(selector))
            .await
            .map_err(|e| format!("could not click '{selector}': {e}"))
    }

    async fn type_into(&self, selector: &str, text: &str) -> Result<(), String> {
        self.page
            .fill(&Target::css(selector), text, false)
            .await
            .map_err(|e| format!("could not type into '{selector}': {e}"))
    }

    async fn press(&self, key: &str) -> Result<(), String> {
        self.page
            .press(key)
            .await
            .map_err(|e| format!("could not press '{key}': {e}"))
    }

    /// Scroll the page, or an element, by a pixel delta.
    ///
    /// Without this the agent could only reach whatever happened to be in the
    /// initial viewport: an infinite feed, a long settings page or a lazy-loaded
    /// list was simply unreachable, and `browser_read` would keep reporting the
    /// same first screen.
    async fn scroll(&self, selector: Option<&str>, dx: f64, dy: f64) -> Result<(), String> {
        let target = selector.map(Target::css);
        self.page
            .wheel(target.as_ref(), dx, dy)
            .await
            .map_err(|e| format!("could not scroll: {e}"))
    }

    /// Wait until a selector is present, or time out.
    ///
    /// The alternative was a blind `sleep` between acting and reading, which
    /// either wasted seconds or raced the page and read a stale DOM.
    async fn wait_for(&self, selector: &str, timeout_ms: u64) -> Result<(), String> {
        self.page
            .wait_for(selector, std::time::Duration::from_millis(timeout_ms))
            .await
            .map_err(|_| format!("'{selector}' did not appear within {timeout_ms}ms"))
    }

    /// Choose an option in a `<select>` by value or visible label, firing the
    /// `change` event a framework listens for.
    async fn select_option(&self, selector: &str, value: &str) -> Result<(), String> {
        self.page
            .select_option(&Target::css(selector), value)
            .await
            .map_err(|e| format!("could not select in '{selector}': {e}"))
    }

    /// Hover an element, for menus and tooltips that only exist on pointer-over.
    async fn hover(&self, selector: &str) -> Result<(), String> {
        self.page
            .hover(&Target::css(selector))
            .await
            .map_err(|e| format!("could not hover '{selector}': {e}"))
    }

    pub async fn screenshot(&self) -> Result<Vec<u8>, String> {
        let bytes = self
            .page
            .screenshot()
            .await
            .map_err(|e| format!("screenshot failed: {e}"))?;
        if bytes.len() > MAX_SCREENSHOT_BYTES {
            return Err(format!(
                "screenshot is {} bytes, over the {MAX_SCREENSHOT_BYTES}-byte cap",
                bytes.len()
            ));
        }
        Ok(bytes)
    }

    /// Console lines and uncaught exceptions, as collected (and capped) by RightKit.
    pub async fn console_lines(&self) -> Vec<ConsoleLine> {
        self.page
            .console()
            .await
            .into_iter()
            .map(|line| ConsoleLine {
                level: line.level,
                text: line.text,
            })
            .collect()
    }

    /// Close Chrome and stop the page pumps. Safe to call twice.
    pub async fn shutdown(&self) {
        self.browser.shutdown().await;
    }
}

fn mask_snapshot_text(text: &str) -> String {
    crate::coding_tools::output_compaction::mask_secrets(text)
}

/// Sensitive DOM controls must use a fixed-width mask before observation
/// fingerprints, diffs, or tool output are produced. The fixed width follows
/// PinchTab's snapshot contract and avoids leaking secret length.
fn sanitized_snapshot_name(name: &str, sensitive: bool) -> String {
    if sensitive {
        SENSITIVE_FIELD_MASK.to_string()
    } else {
        mask_snapshot_text(name)
    }
}

fn advance_snapshot_epoch(state: &mut ObservationState, fingerprint: String) -> u64 {
    if state.epoch == 0 {
        state.epoch = 1;
    } else if state
        .fingerprint
        .as_deref()
        .is_some_and(|old| old != fingerprint)
    {
        state.epoch = state.epoch.saturating_add(1);
    }
    state.fingerprint = Some(fingerprint);
    state.epoch
}

fn snapshot_fingerprint(raw: &[RawSemanticNode], ax_nodes: &[AxNodeSummaryV1]) -> String {
    let mut fingerprint = String::new();
    for node in raw.iter().filter(|node| snapshot_node_is_diffable(node)) {
        let fields = [
            "dom".to_string(),
            node.selector.clone(),
            node.role.clone(),
            node.name.clone(),
            node.frame_id.clone(),
            node.visible.to_string(),
            node.disabled.to_string(),
        ];
        push_fingerprint_record(&mut fingerprint, &fields);
    }
    for node in ax_nodes {
        let fields = [
            "ax".to_string(),
            node.frame_id.clone(),
            node.role.clone(),
            node.name.clone(),
        ];
        push_fingerprint_record(&mut fingerprint, &fields);
    }
    fingerprint
}

fn push_fingerprint_record(output: &mut String, fields: &[String]) {
    output.push_str(&fields.len().to_string());
    output.push(':');
    for field in fields {
        output.push_str(&field.len().to_string());
        output.push(':');
        output.push_str(field);
    }
}

fn ax_frame_id(frame_id: &str) -> String {
    format!("ax-cdp:{frame_id}")
}

fn snapshot_node_is_refable(node: &RawSemanticNode) -> bool {
    snapshot_node_is_diffable(node) && node.visible
}

fn snapshot_node_is_diffable(node: &RawSemanticNode) -> bool {
    !node.selector.is_empty()
}

fn snapshot_dom_key(frame_id: &str, selector: &str) -> String {
    format!("dom:{frame_id}:{selector}")
}

fn snapshot_ax_key(frame_id: &str, role: &str, name: &str) -> String {
    format!("ax:{frame_id}:{role}:{name}")
}

fn snapshot_node_ref(
    browser_id: &str,
    tab_id: &str,
    frame_id: &str,
    epoch: u64,
    static_ref: u64,
) -> String {
    format!("br:{browser_id}:{tab_id}:{frame_id}:e{epoch}:s{static_ref}")
}

fn stable_snapshot_ref(state: &mut ObservationState, frame_id: &str, selector: &str) -> u64 {
    let static_key = format!("{frame_id}:{selector}");
    if let Some(reference) = state.static_refs.get(&static_key).copied() {
        if let Some(index) = state
            .static_ref_order
            .iter()
            .position(|key| key == &static_key)
        {
            state.static_ref_order.remove(index);
        }
        state.static_ref_order.push_back(static_key);
        return reference;
    }

    if state.static_refs.len() >= MAX_STATIC_REFS {
        if let Some(evicted) = state.static_ref_order.pop_front() {
            state.static_refs.remove(&evicted);
        } else if let Some(evicted) = state.static_refs.keys().next().cloned() {
            state.static_refs.remove(&evicted);
        }
    }
    let reference = state.next_static_ref;
    state.next_static_ref = state.next_static_ref.saturating_add(1);
    state.static_refs.insert(static_key.clone(), reference);
    state.static_ref_order.push_back(static_key);
    reference
}

fn observation_diff(
    previous: &BTreeMap<String, (String, String, String, bool, bool)>,
    current: &BTreeMap<String, (String, String, String, bool, bool)>,
) -> ObservationDiffV1 {
    ObservationDiffV1 {
        added: current
            .keys()
            .filter(|key| !previous.contains_key(*key))
            .cloned()
            .collect(),
        removed: previous
            .keys()
            .filter(|key| !current.contains_key(*key))
            .cloned()
            .collect(),
        changed: current
            .iter()
            .filter(|(key, value)| previous.get(*key).is_some_and(|old| old != *value))
            .map(|(key, _)| key.clone())
            .collect(),
    }
}

fn sanitize_download_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 240 {
        return Err("download name is empty or too long".into());
    }
    if Path::new(trimmed).components().count() != 1
        || trimmed == "."
        || trimmed == ".."
        || trimmed.contains(['/', '\\', '\0'])
    {
        return Err("download name must be one sanitized path component".into());
    }
    let safe = trimmed
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if safe.starts_with('.') {
        return Err("hidden download names are refused".into());
    }
    Ok(safe)
}

/// Intercept every request so page subresources cannot reach private networks.
/// Documents must also stay on the approved origin across redirect hops.
/// RightKit owns the Fetch plumbing; this hook only supplies the host app's
/// origin pin, trust policy and host allowlist.
async fn install_document_guard(
    page: &BrowserPage,
    approved_origin: Arc<tokio::sync::RwLock<Option<String>>>,
    navigation_trust: Arc<std::sync::Mutex<BrowserNavigationTrustV1>>,
    host_policy: Arc<std::sync::Mutex<HashMap<String, BrowserNetworkDecisionV1>>>,
) -> Result<(), String> {
    let hook: rightkit_browser::RequestInterceptor = Arc::new(move |request| {
        let approved_origin = Arc::clone(&approved_origin);
        let navigation_trust = Arc::clone(&navigation_trust);
        let host_policy = Arc::clone(&host_policy);
        let decision: futures::future::BoxFuture<'static, InterceptDecision> =
            Box::pin(async move {
                let url = request.url.as_str();
                let is_document = request.resource_type == "Document";
                let expected = approved_origin.read().await.clone();
                let origin_allowed = !is_document
                    || expected
                        .as_deref()
                        .is_some_and(|origin| http_origin(url).ok().as_deref() == Some(origin));
                let trust = navigation_trust
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let host_allowlisted = url::Url::parse(url)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
                    .and_then(|host| {
                        host_policy
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .get(&host)
                            .copied()
                    })
                    == Some(BrowserNetworkDecisionV1::Allow);
                // Re-validated per hop: redirects and page-initiated requests were
                // never seen by the navigate-time check, so each one clears the
                // guard independently before it is continued.
                let ssrf_allowed = if !origin_allowed {
                    false
                } else {
                    match browser_request_guard_url(url, is_document) {
                        Ok(None) => true,
                        Ok(Some(guard_url)) => browser_navigation_reject_reason_with_allowlist(
                            "request",
                            &guard_url,
                            &trust,
                            host_allowlisted,
                        )
                        .await
                        .is_none(),
                        Err(()) => false,
                    }
                };
                if ssrf_allowed {
                    InterceptDecision::Continue
                } else {
                    InterceptDecision::Fail(InterceptFailure::BlockedByClient)
                }
            });
        decision
    });
    page.set_request_interceptor(Some(hook))
        .await
        .map_err(|error| format!("could not enable browser request interception: {error}"))
}

fn browser_request_guard_url(url: &str, is_document: bool) -> Result<Option<String>, ()> {
    let mut parsed = url::Url::parse(url).map_err(|_| ())?;
    match parsed.scheme() {
        "http" | "https" => Ok(Some(parsed.to_string())),
        "ws" => {
            parsed.set_scheme("http").map_err(|_| ())?;
            Ok(Some(parsed.to_string()))
        }
        "wss" => {
            parsed.set_scheme("https").map_err(|_| ())?;
            Ok(Some(parsed.to_string()))
        }
        "data" | "blob" | "about" if !is_document => Ok(None),
        _ => Err(()),
    }
}

/// Navigation keeps local development available, but private LAN targets are
/// accepted only through a separately configured, explicit trust policy. This
/// is called again by document interception, including each redirect hop.
pub async fn browser_navigation_reject_reason(
    verb: &str,
    url: &str,
    trust: &BrowserNavigationTrustV1,
) -> Option<String> {
    browser_navigation_reject_reason_with_allowlist(verb, url, trust, false).await
}

pub async fn browser_navigation_reject_reason_with_allowlist(
    verb: &str,
    url: &str,
    trust: &BrowserNavigationTrustV1,
    host_allowlisted: bool,
) -> Option<String> {
    if url.len() > MAX_NAVIGATION_URL_BYTES {
        return Some(format!(
            "Refusing to {verb}: browser navigation URL exceeds the {MAX_NAVIGATION_URL_BYTES}-byte limit."
        ));
    }
    if let Some(reason) = crate::coding_tools::browser_fetch::blocked_fetch_host(url) {
        return Some(format!(
            "Refusing to {verb} '{url}': {reason}. This protects against SSRF to instance-metadata endpoints."
        ));
    }
    let parsed = match url::Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => parsed,
        Ok(_) => {
            return Some(format!(
                "Refusing to {verb} '{url}': browser navigation is limited to http and https URLs."
            ))
        }
        Err(error) => {
            return Some(format!(
                "Refusing to {verb} '{url}': invalid browser URL: {error}"
            ))
        }
    };
    let host = match parsed.host_str() {
        Some(host) => host,
        None => {
            return Some(format!(
                "Refusing to {verb} '{url}': browser navigation URL has no host."
            ))
        }
    };
    let policy = match trust.policy() {
        Ok(policy) => policy,
        Err(error) => {
            return Some(format!(
                "Refusing to {verb}: invalid trusted navigation policy: {error}"
            ))
        }
    };
    match resolve_then_reject_private_allow_loopback_with_trust_and_allowlist(
        host,
        parsed.port_or_known_default().unwrap_or(80),
        &policy,
        |_| host_allowlisted,
    )
    .await
    {
        Ok(()) => None,
        Err(error) => Some(format!(
            "Refusing to {verb} '{url}': {error}. This protects against DNS-rebinding SSRF to internal networks."
        )),
    }
}

fn http_origin(raw: &str) -> Result<String, String> {
    let url = url::Url::parse(raw).map_err(|error| format!("invalid browser URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("browser navigation is limited to http and https URLs".to_string());
    }
    let origin = url.origin().ascii_serialization();
    if origin == "null" {
        return Err("browser navigation URL has no network origin".to_string());
    }
    Ok(origin)
}

fn truncate(mut text: String, max: usize) -> String {
    if text.len() > max {
        // 7b: state the real size and cap before the "…[truncated]" marker
        // callers already match on; the suffix stays intact so existing
        // callers/tests that check `ends_with("… [truncated]")` still hold.
        let original_bytes = text.len();
        text.truncate(max);
        text.push_str(&format!(
            "\n[full {original_bytes}B, shown {max}B] … [truncated]"
        ));
    }
    text
}

fn action_signature(args: &Value) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for field in [
        "action", "ref", "selector", "text", "key", "url", "dx", "dy", "path",
    ] {
        field.hash(&mut hasher);
        let value = args.get(field).map(|value| {
            if field == "ref" {
                value
                    .as_str()
                    .map(normalize_browser_ref)
                    .unwrap_or_else(|| value.to_string())
            } else {
                value.to_string()
            }
        });
        value.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Epoch values deliberately rotate after every completed action; watchdog
/// fingerprints must not mistake that safety rotation for a new intent.
fn normalize_browser_ref(raw: &str) -> String {
    let mut parts = raw.split(':').collect::<Vec<_>>();
    if let Some(index) = parts.len().checked_sub(2) {
        let part = parts[index];
        if part.len() > 1
            && part.starts_with('e')
            && part[1..].chars().all(|ch| ch.is_ascii_digit())
        {
            parts.remove(index);
        }
    }
    parts.join(":")
}

fn repeated_action_cycle_depth(recent: &VecDeque<String>) -> usize {
    let items = recent.iter().collect::<Vec<_>>();
    if items.len() < 4 {
        return 0;
    }
    let mut depth = 0;
    for width in 1..=items.len() / 2 {
        for start in 0..=items.len() - width * 2 {
            let span = items.len() - start;
            if span < width * 2 {
                continue;
            }
            let periodic = (start..items.len())
                .all(|index| items[index] == items[start + (index - start) % width]);
            if periodic {
                depth = depth.max(span);
            }
        }
    }
    depth
}

enum ConsentedPlan {
    Import,
    Attach,
}

/// Point the launch options at the consented browser's executable and a private copy of
/// its profile. Chromium-family only; the consent must still be active.
fn consented_launch_options(
    consent: &crate::coding_tools::browser_consent::ConsentedLaunch,
    options: &mut rightkit_browser::LaunchOptions,
) -> Result<ConsentedPlan, String> {
    use crate::coding_tools::browser_consent::ConsentedMode;
    use rightkit_browser::{BrowserKind, BrowserSource, ImportSpec};

    let receipt = &consent.receipt;
    receipt.check_active().map_err(|e| e.to_string())?;
    let source = match receipt.browser {
        BrowserKind::Chrome => BrowserSource::Chrome,
        BrowserKind::Edge => BrowserSource::Edge,
        other => {
            return Err(format!(
                "reusing sign-ins supports Chrome and Edge; {} is not covered",
                other.name()
            ))
        }
    };
    let exe = rightkit_browser::browser_executable(receipt.browser)
        .ok_or_else(|| format!("{} is not installed", receipt.browser.name()))?;
    options.chrome_path = Some(exe);
    options.use_system_chrome_only = false;
    options.allow_system_chrome = false;
    match consent.mode {
        ConsentedMode::Attach => {
            options.headless = false;
            Ok(ConsentedPlan::Attach)
        }
        ConsentedMode::Import => {
            options.headless = true;
            options.profile = rightkit_browser::ProfileSpec::ImportedFrom {
                root: consent.profiles_root.clone(),
                name: crate::coding_tools::browser_consent::imported_profile_name(receipt),
                import: ImportSpec {
                    browser: source,
                    source_profile: Some(receipt.profile.clone()),
                    domains: receipt.domains.clone(),
                    consent: receipt.clone(),
                    revocation: consent.revocation.clone(),
                    reimport: false,
                },
            };
            Ok(ConsentedPlan::Import)
        }
    }
}

/// Use the ordinary Windows spelling for Chromium command-line paths.
fn browser_profile_path(path: &Path) -> PathBuf {
    let physical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        let text = physical.to_string_lossy();
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        if let Some(plain) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(plain);
        }
    }
    physical
}

/// Match RightKit launch discovery for browser-dependent tests.
#[cfg(test)]
fn find_chrome() -> Option<std::path::PathBuf> {
    rightkit_browser::chrome_for_testing_path(None)
        .ok()
        .or_else(rightkit_browser::find_chrome)
}

async fn gate(ctx: &ToolContext) -> Option<ToolResult> {
    let verdict = PermissionEngine::new().evaluate(
        CapabilityClass::Browser,
        ctx.permission_mode,
        ctx.workspace_trust,
        None,
        false,
    );
    match verdict.decision {
        PermissionDecision::Deny => Some(ToolResult::error(format!(
            "Permission denied: {} (policy: {})",
            verdict.reason, verdict.policy_source
        ))),
        PermissionDecision::Ask => Some(ToolResult::error(format!(
            "Tool requires approval: {} (policy: {})",
            verdict.reason, verdict.policy_source
        ))),
        PermissionDecision::Allow => None,
    }
}

async fn session_of(ctx: &ToolContext, id: &str) -> Result<Arc<BrowserSession>, ToolResult> {
    let store = ctx
        .browser_sessions
        .as_ref()
        .ok_or_else(|| ToolResult::error("Browser sessions are not available in this context."))?;
    store.lock().await.get(id).cloned().ok_or_else(|| {
        ToolResult::error(format!(
            "No open browser session '{id}'. Open one with browser_open, or it was closed."
        ))
    })
}

/// What every tool returns after doing its thing: where the page is now, and whether it
/// logged anything alarming. Without this the model has to call `browser_read` after
/// every single action just to find out whether it worked.
async fn state_summary(session: &BrowserSession, action: &str) -> String {
    let problems: Vec<_> = session
        .console_lines()
        .await
        .into_iter()
        .filter(|l| l.level == "error" || l.level == "exception")
        .collect();
    // The URL and title come from the page, so they are wrapped as untrusted text (R9); the
    // session id is ours and stays outside the wrapper where the model can read it.
    let mut out = format!(
        "{action}\nsession: {}\n{}",
        session.id,
        wrap_untrusted_web_text(
            "browser_open",
            &format!(
                "url: {}\ntitle: {}",
                session.url().await,
                session.title().await
            )
        )
    );
    if !problems.is_empty() {
        out.push_str(&format!(
            "\nconsole errors: {} (call browser_console to read them)",
            problems.len()
        ));
    }
    out
}

pub struct BrowserOpenTool;

#[async_trait::async_trait]
impl Tool for BrowserOpenTool {
    fn name(&self) -> &str {
        "browser_open"
    }

    fn description(&self) -> &str {
        "Open a real Chrome browser at a URL and return a session id. Use this to look at a running app — including a local dev server (http://localhost:5173) — then drive it with browser_read, browser_act, browser_screenshot, and browser_console. Close it with browser_close when done. To reuse the user's signed-in session, pass `consent` with the id of a consent the user already granted (you cannot grant one); the page then runs on a private copy of their profile, limited to the domains they consented to. Fail: no Chrome or Edge installed, the URL does not load, or the consent is unknown, revoked, expired or does not cover the URL."
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Browser)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": { "type": "string", "description": "URL to open, e.g. http://localhost:5173" },
                "consent": { "type": "string", "description": "Id of a browser consent the user granted. Reuses their signed-in session for the consented domains only." },
                "mode": { "type": "string", "enum": ["import", "attach"], "description": "With `consent`: `import` (default) is headless on a private copy of their sign-ins; `attach` is a visible window on the same private copy. Never the live browser." }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if let Some(denied) = gate(ctx).await {
            return denied;
        }
        let Some(url) = args["url"].as_str() else {
            return ToolResult::error("Missing required argument: url");
        };
        if let Some(reason) =
            browser_navigation_reject_reason("open", url, &BrowserNavigationTrustV1::default())
                .await
        {
            return ToolResult::error(reason);
        }
        let Some(store) = ctx.browser_sessions.clone() else {
            return ToolResult::error("Browser sessions are not available in this context.");
        };

        let session = match args["consent"].as_str() {
            None => match BrowserSession::open_owned(url, &ctx.session_id).await {
                Ok(s) => Arc::new(s),
                Err(e) => return ToolResult::error(e),
            },
            Some(consent_id) => {
                let mode = match args["mode"].as_str() {
                    None => crate::coding_tools::browser_consent::ConsentedMode::Import,
                    Some(raw) => {
                        match crate::coding_tools::browser_consent::ConsentedMode::parse(raw) {
                            Some(mode) => mode,
                            None => {
                                return ToolResult::error(format!(
                                    "Unknown mode '{raw}': use import or attach"
                                ))
                            }
                        }
                    }
                };
                let launch = match crate::coding_tools::browser_consent::ConsentStore::product()
                    .and_then(|store| store.launch_for(consent_id, mode))
                {
                    Ok(launch) => launch,
                    Err(e) => return ToolResult::error(e),
                };
                if !launch.receipt.permits_url(url) {
                    return ToolResult::error(format!(
                        "Refusing to open '{url}': it is outside the domains the user consented to."
                    ));
                }
                match BrowserSession::open_consented(
                    url,
                    &ctx.session_id,
                    BrowserNavigationTrustV1::default(),
                    Vec::new(),
                    launch,
                )
                .await
                {
                    Ok(s) => s,
                    Err(e) => return ToolResult::error(e),
                }
            }
        };
        let summary = state_summary(&session, "Opened.").await;
        store
            .lock()
            .await
            .insert(session.id.clone(), Arc::clone(&session));
        ToolResult::ok(summary)
    }
}

pub struct BrowserReadTool;

#[async_trait::async_trait]
impl Tool for BrowserReadTool {
    fn name(&self) -> &str {
        "browser_read"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Read what an open browser session is currently showing: the rendered page text, and every interactive element with the CSS selector needed to act on it. Pass those selectors to browser_act. Fail: unknown session id."
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Browser)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["session"],
            "properties": {
                "session": { "type": "string", "description": "Session id from browser_open" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if let Some(denied) = gate(ctx).await {
            return denied;
        }
        let Some(id) = args["session"].as_str() else {
            return ToolResult::error("Missing required argument: session");
        };
        let session = match session_of(ctx, id).await {
            Ok(s) => s,
            Err(e) => return e,
        };

        let (text, observation) = match session.read_observation().await {
            Ok(result) => result,
            Err(e) => return ToolResult::error(e),
        };
        let elements = serde_json::to_string_pretty(&observation.dom_fallback)
            .unwrap_or_else(|_| "[]".to_string());
        let observation_json =
            serde_json::to_string_pretty(&observation).unwrap_or_else(|_| "{}".to_string());

        // Page content is attacker-controlled — the classic prompt-injection vector.
        let body = format!(
            "url: {}\ntitle: {}\n\n--- text ---\n{text}\n\n--- interactive elements ---\n{elements}\n\n--- observation ---\n{observation_json}",
            observation.url,
            observation.title
        );
        ToolResult::ok(wrap_untrusted_web_text("browser_read", &body))
    }
}

pub struct BrowserActTool;

#[async_trait::async_trait]
impl Tool for BrowserActTool {
    fn name(&self) -> &str {
        "browser_act"
    }

    fn description(&self) -> &str {
        "Act on an open browser session: click, type, press a key, navigate, scroll, wait for an element to appear, choose a dropdown option, hover, or go back/forward/reload. Get selectors from browser_read first. Returns the page state after the action, flagging any console errors it triggered. Fail: unknown session, or no element matches the selector."
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Browser)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["session", "action"],
            "properties": {
                "session": { "type": "string", "description": "Session id from browser_open" },
                "action": {
                    "type": "string",
                    "enum": ["click", "type", "press", "navigate", "scroll", "wait_for", "select", "hover", "upload", "back", "forward", "reload"],
                    "description": "click: click `selector`. type: type `text` into `selector`. press: send `key` to the focused element. navigate: go to `url`. scroll: scroll the page — or `selector` if given — by `dx`/`dy` pixels. wait_for: block until `selector` exists or `timeout_ms` elapses. select: choose `text` (option value or visible label) in the <select> at `selector`. hover: pointer-over `selector`, for menus that only appear on hover. back / forward / reload: history and refresh."
                },
                "ref": { "type": "string", "description": "Same-epoch scoped ref from browser_read. Preferred over selector." },
                "selector": { "type": "string", "description": "Unique CSS selector: click, type, scroll (optional), wait_for, select, hover, upload" },
                "text": { "type": "string", "description": "Text to type (action=type), or the option to choose (action=select)" },
                "key": { "type": "string", "description": "Key name, e.g. Enter or Escape, for action=press" },
                "url": { "type": "string", "description": "URL, for action=navigate" },
                "dx": { "type": "number", "description": "Horizontal scroll in px (default 0)" },
                "dy": { "type": "number", "description": "Vertical scroll in px (default 600, about one screen)" },
                "timeout_ms": { "type": "number", "description": "How long wait_for waits before failing (default 5000)" },
                "path": { "type": "string", "description": "Workspace-contained regular file for action=upload" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if let Some(denied) = gate(ctx).await {
            return denied;
        }
        let Some(id) = args["session"].as_str() else {
            return ToolResult::error("Missing required argument: session");
        };
        let session = match session_of(ctx, id).await {
            Ok(s) => s,
            Err(e) => return e,
        };

        let action = args["action"].as_str().unwrap_or_default();
        let ticket = session.control.action_ticket();
        let _lane = session.action_lane.lock().await;
        if let Err(error) = session.control.ensure_agent_control(ticket) {
            return ToolResult::error(error);
        }
        let watchdog_notice = match session.control.record_action(action_signature(&args)) {
            Ok(notice) => notice,
            Err(error) => return ToolResult::error(error),
        };
        let type_text = if action == "type" {
            args["text"].as_str().map(str::to_string)
        } else {
            None
        };
        let selector = match args["ref"].as_str() {
            Some(node_ref) => match session.resolve_ref(node_ref) {
                Ok(selector) => Some(selector),
                Err(error) => return ToolResult::error(error),
            },
            None => args["selector"].as_str().map(str::to_string),
        };
        if let Some(selector) = selector.as_deref() {
            if !matches!(action, "scroll" | "wait_for") {
                if let Err(error) = session.unique_selector(selector).await {
                    return ToolResult::error(error);
                }
            }
        }
        let outcome = match action {
            "click" => match selector.as_deref() {
                Some(sel) => session
                    .click(sel)
                    .await
                    .map(|_| format!("Clicked '{sel}'.")),
                None => Err("action=click needs a selector".to_string()),
            },
            "type" => match (selector.as_deref(), type_text.as_deref()) {
                (Some(sel), Some(text)) => session
                    .type_into(sel, text)
                    .await
                    .map(|_| format!("Typed into '{sel}'.")),
                _ => Err("action=type needs a selector and text".to_string()),
            },
            "press" => match args["key"].as_str() {
                Some(key) => session
                    .press(key)
                    .await
                    .map(|_| format!("Pressed '{key}'.")),
                None => Err("action=press needs a key".to_string()),
            },
            "navigate" => match args["url"].as_str() {
                Some(url) => match browser_navigation_reject_reason_with_allowlist(
                    "navigate to",
                    url,
                    &session.navigation_trust(),
                    session.host_allowlisted(url),
                )
                .await
                {
                    Some(reason) => Err(reason),
                    None => session
                        .navigate_locked(url)
                        .await
                        .map(|_| format!("Navigated to '{url}'.")),
                },
                None => Err("action=navigate needs a url".to_string()),
            },
            "scroll" => {
                let dx = args["dx"].as_f64().unwrap_or(0.0);
                // One screen down is the overwhelmingly common intent.
                let dy = args["dy"].as_f64().unwrap_or(600.0);
                let sel = selector.as_deref();
                session.scroll(sel, dx, dy).await.map(|_| match sel {
                    Some(s) => format!("Scrolled '{s}' by ({dx}, {dy})."),
                    None => format!("Scrolled page by ({dx}, {dy})."),
                })
            }
            "wait_for" => match selector.as_deref() {
                Some(sel) => {
                    let timeout = args["timeout_ms"].as_u64().unwrap_or(5_000).min(60_000);
                    session
                        .wait_for(sel, timeout)
                        .await
                        .map(|_| format!("'{sel}' appeared."))
                }
                None => Err("action=wait_for needs a selector".to_string()),
            },
            "select" => match (selector.as_deref(), args["text"].as_str()) {
                (Some(sel), Some(value)) => session
                    .select_option(sel, value)
                    .await
                    .map(|_| format!("Selected '{value}' in '{sel}'.")),
                _ => Err("action=select needs a selector and text".to_string()),
            },
            "hover" => match selector.as_deref() {
                Some(sel) => session
                    .hover(sel)
                    .await
                    .map(|_| format!("Hovered '{sel}'.")),
                None => Err("action=hover needs a selector".to_string()),
            },
            "back" => session
                .navigate_history_locked("back")
                .await
                .map(|_| "Went back.".to_string()),
            "forward" => session
                .navigate_history_locked("forward")
                .await
                .map(|_| "Went forward.".to_string()),
            "reload" => session
                .reload_locked()
                .await
                .map(|_| "Reloaded.".to_string()),
            "upload" => match (selector.as_deref(), args["path"].as_str()) {
                (Some(sel), Some(path)) => session
                    .upload_file(sel, Path::new(path), &ctx.workspace_root)
                    .await
                    .map(|_| format!("Uploaded a workspace file into '{sel}'.")),
                _ => Err("action=upload needs a ref/selector and path".to_string()),
            },
            other => Err(format!(
                "Unknown action '{other}'. Supported: click, type, press, navigate, scroll, \
                 wait_for, select, hover, upload, back, forward, reload."
            )),
        };

        match outcome {
            Ok(done) => {
                // Navigation/history helpers already rotate generation + epoch
                // after CDP settles; DOM actions need rotation here.
                if !matches!(action, "navigate" | "back" | "forward" | "reload") {
                    session.control.invalidate_actions();
                }
                match session.observe_locked().await {
                    Ok(observation) => ToolResult::ok(format!(
                        "{done}\n{}{}",
                        // Compact: an action result rides in every later
                        // request, so whitespace is pure token cost. Page-derived text goes
                        // through the injection scan and the untrusted wrapper like
                        // browser_read does (contract R9).
                        wrap_untrusted_web_text(
                            "browser_act",
                            &serde_json::to_string(&observation).unwrap_or_default()
                        ),
                        watchdog_notice
                            .as_deref()
                            .map(|notice| format!("\nwatchdog: {notice}"))
                            .unwrap_or_default(),
                    )),
                    Err(error) => ToolResult::error(format!(
                        "action committed but post-action observation failed: {error}"
                    )),
                }
            }
            Err(e) => ToolResult::error(e),
        }
    }
}

pub struct BrowserScreenshotTool;

#[async_trait::async_trait]
impl Tool for BrowserScreenshotTool {
    fn name(&self) -> &str {
        "browser_screenshot"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Capture what an open browser session looks like right now, as a PNG you can see. Use it to check layout, styling, and visual regressions — things page text cannot tell you. Fail: unknown session id."
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Browser)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["session"],
            "properties": {
                "session": { "type": "string", "description": "Session id from browser_open" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if let Some(denied) = gate(ctx).await {
            return denied;
        }
        let Some(id) = args["session"].as_str() else {
            return ToolResult::error("Missing required argument: session");
        };
        let session = match session_of(ctx, id).await {
            Ok(s) => s,
            Err(e) => return e,
        };
        let bytes = match session.screenshot().await {
            Ok(b) => b,
            Err(e) => return ToolResult::error(e),
        };

        // `[image-block]` is the marker the Tauri transcript renders inline — the same
        // contract `view_image` uses. Without it this is just a base64 blob.
        let encoded = rightkit_crypto::base64_encode(&bytes);
        ToolResult::ok(format!(
            "[image-block]\npath: {} ({})\nsize: {} bytes\nmime: image/png\ndata_url: data:image/png;base64,{encoded}",
            session.url().await,
            session.id,
            bytes.len(),
        ))
    }
}

pub struct BrowserConsoleTool;

#[async_trait::async_trait]
impl Tool for BrowserConsoleTool {
    fn name(&self) -> &str {
        "browser_console"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Read everything an open browser session has logged since it opened — console output and uncaught exceptions. This is where a broken frontend actually tells you what is wrong. Fail: unknown session id."
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Browser)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["session"],
            "properties": {
                "session": { "type": "string", "description": "Session id from browser_open" },
                "errors_only": { "type": "boolean", "description": "Return only errors and exceptions (default: false)" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if let Some(denied) = gate(ctx).await {
            return denied;
        }
        let Some(id) = args["session"].as_str() else {
            return ToolResult::error("Missing required argument: session");
        };
        let session = match session_of(ctx, id).await {
            Ok(s) => s,
            Err(e) => return e,
        };

        let errors_only = args["errors_only"].as_bool().unwrap_or(false);
        let lines: Vec<_> = session
            .console_lines()
            .await
            .into_iter()
            .filter(|l| !errors_only || l.level == "error" || l.level == "exception")
            .collect();

        if lines.is_empty() {
            return ToolResult::ok(if errors_only {
                "No console errors or exceptions."
            } else {
                "The page has logged nothing."
            });
        }

        // The page wrote these, so they are untrusted like any other page content.
        let body = lines
            .iter()
            .map(|l| format!("[{}] {}", l.level, l.text))
            .collect::<Vec<_>>()
            .join("\n");
        let body = crate::coding_tools::output_compaction::mask_secrets(&body);
        ToolResult::ok(wrap_untrusted_web_text("browser_console", &body))
    }
}

pub struct BrowserCloseTool;

#[async_trait::async_trait]
impl Tool for BrowserCloseTool {
    fn name(&self) -> &str {
        "browser_close"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Close a browser session and shut down its Chrome process. Call this when done — an open session holds a real browser."
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Browser)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["session"],
            "properties": {
                "session": { "type": "string", "description": "Session id from browser_open" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if let Some(denied) = gate(ctx).await {
            return denied;
        }
        let Some(id) = args["session"].as_str() else {
            return ToolResult::error("Missing required argument: session");
        };
        let Some(store) = ctx.browser_sessions.clone() else {
            return ToolResult::error("Browser sessions are not available in this context.");
        };
        let removed = store.lock().await.remove(id);
        match removed {
            Some(session) => {
                session.shutdown().await;
                ToolResult::ok(format!("Closed browser session '{id}'."))
            }
            None => ToolResult::error(format!("No open browser session '{id}'.")),
        }
    }
}

/// True when this process runs as the Linux root user.
fn running_as_root() -> bool {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_engine_runtime_resources::{
        ResourceGovernor, ResourceKind, ResourceLimits, ServiceScope,
    };
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use tempfile::tempdir;

    fn install_live_browser_resource_profile() {
        use crate::coding_tools::process_resource_authority::{
            install_process_resource_profile, ResourceProfile,
        };
        install_process_resource_profile(ResourceProfile::Expanded)
            .expect("live browser tests require the Expanded process-resource profile");
    }

    /// The live tests spawn real Chrome. In the full parallel suite (hundreds of
    /// tests) Chrome starves for CPU and its DevTools socket times out — a harness
    /// artifact, not a product bug. So they are opt-in:
    ///   RIGHTKIT_BROWSER_TESTS=1 cargo test -p host_tools --lib browser::
    /// runs them (proven green: 7/7 isolated, 352/352 at --test-threads=4).
    fn live_browser_tests_enabled() -> bool {
        match std::env::var("RIGHTKIT_BROWSER_TESTS").as_deref() {
            Ok("1") => {
                install_live_browser_resource_profile();
                assert!(
                    find_chrome().is_some(),
                    "RIGHTKIT_BROWSER_TESTS=1 requested live browser proof, but Chrome was not found"
                );
                true
            }
            _ => false,
        }
    }

    fn ctx_with_store() -> (ToolContext, BrowserStore) {
        let dir = tempdir().unwrap();
        let store: BrowserStore = Arc::new(Mutex::new(HashMap::new()));
        let ctx = ToolContext::new(
            "s1",
            dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_browser_sessions(Arc::clone(&store));
        (ctx, store)
    }

    #[test]
    fn browser_context_saturation_denies_before_launch_effect() {
        let limits = ResourceLimits {
            browser_processes: 2,
            browser_contexts: 1,
            ..ResourceLimits::default()
        };
        let governor = ResourceGovernor::new(limits);
        let scope = ServiceScope::Session("browser-test".to_string());
        let process = crate::coding_tools::process_resource_authority::reserve_with_governor(
            &governor,
            ResourceKind::BrowserProcess,
            scope.clone(),
            true,
        )
        .unwrap();
        let context = governor
            .acquire(ResourceKind::BrowserContext, scope.clone(), 1, true)
            .unwrap();
        assert_eq!(governor.usage(ResourceKind::BrowserContext, &scope), 1);

        // The second process reservation succeeds, but its context reservation
        // fails before any browser launch/effect can occur.
        let second_process =
            crate::coding_tools::process_resource_authority::reserve_with_governor(
                &governor,
                ResourceKind::BrowserProcess,
                scope.clone(),
                true,
            )
            .unwrap();
        assert!(governor
            .acquire(ResourceKind::BrowserContext, scope.clone(), 1, true)
            .is_err());
        drop(second_process);
        drop(context);
        drop(process);
        assert_eq!(governor.usage(ResourceKind::BrowserProcess, &scope), 0);
        assert_eq!(governor.usage(ResourceKind::BrowserContext, &scope), 0);
        assert_eq!(governor.usage(ResourceKind::ExternalExecution, &scope), 0);
    }

    #[test]
    fn browser_admission_charges_aggregate_once_and_releases_for_next_session() {
        let governor = ResourceGovernor::new(ResourceLimits::default());
        let scope = ServiceScope::Session("browser-test".to_string());
        {
            let process = crate::coding_tools::process_resource_authority::reserve_with_governor(
                &governor,
                ResourceKind::BrowserProcess,
                scope.clone(),
                true,
            )
            .unwrap();
            let context = governor
                .acquire(ResourceKind::BrowserContext, scope.clone(), 1, true)
                .unwrap();
            assert_eq!(governor.usage(ResourceKind::ExternalExecution, &scope), 1);
            assert_eq!(governor.usage(ResourceKind::BrowserProcess, &scope), 1);
            assert_eq!(governor.usage(ResourceKind::BrowserContext, &scope), 1);

            // Browser pages/actions do not acquire another context.
            let control = BrowserControlState::new("browser-test".to_string());
            assert!(control.record_action("click:#save".to_string()).is_ok());
            assert_eq!(governor.usage(ResourceKind::BrowserContext, &scope), 1);
            drop(context);
            drop(process);
        }
        assert_eq!(governor.usage(ResourceKind::ExternalExecution, &scope), 0);
        assert_eq!(governor.usage(ResourceKind::BrowserProcess, &scope), 0);
        assert_eq!(governor.usage(ResourceKind::BrowserContext, &scope), 0);

        let next_process = crate::coding_tools::process_resource_authority::reserve_with_governor(
            &governor,
            ResourceKind::BrowserProcess,
            scope.clone(),
            true,
        )
        .unwrap();
        let next_context = governor
            .acquire(ResourceKind::BrowserContext, scope.clone(), 1, true)
            .unwrap();
        assert_eq!(governor.usage(ResourceKind::ExternalExecution, &scope), 1);
        drop(next_context);
        drop(next_process);
    }

    /// The whole point of the tool: open a page, look at it, click something, and see
    /// what the page said. Skipped where no browser is installed (CI).
    #[tokio::test]
    async fn drives_a_real_page_end_to_end() {
        if !live_browser_tests_enabled() {
            eprintln!("skipping: set RIGHTKIT_BROWSER_TESTS=1 to run live-Chrome tests");
            return;
        }
        let (ctx, _store) = ctx_with_store();
        let page = "data:text/html,<title>T</title><h1>Hello</h1>\
                    <button id=go onclick=\"document.querySelector('h1').innerText='Clicked'\">Go</button>\
                    <script>console.error('boom')</script>";

        let opened = BrowserOpenTool.execute(json!({ "url": page }), &ctx).await;
        assert!(!opened.is_error, "open failed: {}", opened.content);
        let id = opened
            .content
            .lines()
            .find_map(|l| l.strip_prefix("session: "))
            .expect("open must return a session id")
            .to_string();

        let read = BrowserReadTool
            .execute(json!({ "session": id }), &ctx)
            .await;
        assert!(read.content.contains("Hello"));
        assert!(
            read.content.contains("#go"),
            "must expose a usable selector"
        );

        let clicked = BrowserActTool
            .execute(
                json!({ "session": id, "action": "click", "selector": "#go" }),
                &ctx,
            )
            .await;
        assert!(!clicked.is_error, "click failed: {}", clicked.content);

        let after = BrowserReadTool
            .execute(json!({ "session": id }), &ctx)
            .await;
        assert!(
            after.content.contains("Clicked"),
            "the click must have changed the page: {}",
            after.content
        );

        let console = BrowserConsoleTool
            .execute(json!({ "session": id, "errors_only": true }), &ctx)
            .await;
        assert!(
            console.content.contains("boom"),
            "the page's console.error must be visible: {}",
            console.content
        );

        let shot = BrowserScreenshotTool
            .execute(json!({ "session": id }), &ctx)
            .await;
        assert!(!shot.is_error, "screenshot failed: {}", shot.content);
        assert!(shot.content.starts_with("[image-block]"));
        assert!(shot.content.contains("data:image/png;base64,"));

        let closed = BrowserCloseTool
            .execute(json!({ "session": id }), &ctx)
            .await;
        assert!(!closed.is_error);

        let gone = BrowserReadTool
            .execute(json!({ "session": id }), &ctx)
            .await;
        assert!(gone.is_error, "a closed session must not still be readable");
    }

    /// The reason this tool exists: the agent starts a dev server on loopback and
    /// looks at it. If the SSRF guard refused loopback, the whole capability would be
    /// useless — so prove it does not.
    #[tokio::test]
    #[ignore = "requires local Chrome; run this atom proof explicitly with --ignored"]
    async fn opens_a_local_dev_server_on_loopback() {
        install_live_browser_resource_profile();
        assert!(
            find_chrome().is_some(),
            "FM-017 live loopback proof requires local Chrome"
        );
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/")
            .with_header("content-type", "text/html")
            .with_body("<title>Dev</title><h1>dev server is up</h1>")
            .create_async()
            .await;

        let (ctx, _store) = ctx_with_store();
        let opened = BrowserOpenTool
            .execute(json!({ "url": server.url() }), &ctx)
            .await;
        assert!(
            !opened.is_error,
            "loopback must not be refused: {}",
            opened.content
        );
        let id = opened
            .content
            .lines()
            .find_map(|l| l.strip_prefix("session: "))
            .unwrap()
            .to_string();

        let read = BrowserReadTool
            .execute(json!({ "session": id }), &ctx)
            .await;
        assert!(
            read.content.contains("dev server is up"),
            "must read the page the local server rendered: {}",
            read.content
        );

        let _ = BrowserCloseTool
            .execute(json!({ "session": id }), &ctx)
            .await;
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn browser_tools_gate_reads_and_cleanup_before_touching_session_state() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(
            "s1",
            dir.path(),
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted,
        );
        for result in [
            BrowserReadTool
                .execute(json!({"session": "missing"}), &ctx)
                .await,
            BrowserScreenshotTool
                .execute(json!({"session": "missing"}), &ctx)
                .await,
            BrowserConsoleTool
                .execute(json!({"session": "missing"}), &ctx)
                .await,
            BrowserCloseTool
                .execute(json!({"session": "missing"}), &ctx)
                .await,
        ] {
            assert!(result.is_error);
            assert!(
                result.content.contains("requires approval"),
                "browser capability must gate every browser surface: {}",
                result.content
            );
        }
    }

    #[tokio::test]
    async fn unknown_session_is_an_error_not_a_panic() {
        let (ctx, _store) = ctx_with_store();
        let result = BrowserReadTool
            .execute(json!({ "session": "bs-nope" }), &ctx)
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("No open browser session"));
    }

    #[tokio::test]
    async fn unknown_action_is_rejected() {
        let (ctx, store) = ctx_with_store();
        // No real session needed: the store lookup must fail before the action parse,
        // so seed one id to reach the action branch only if a browser exists.
        assert!(store.lock().await.is_empty());
        let result = BrowserActTool
            .execute(json!({ "session": "bs-nope", "action": "teleport" }), &ctx)
            .await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn open_refuses_cloud_metadata_endpoints() {
        let (ctx, _store) = ctx_with_store();
        let result = BrowserOpenTool
            .execute(
                json!({ "url": "http://169.254.169.254/latest/meta-data/" }),
                &ctx,
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("Refusing to open"));
    }

    #[test]
    fn long_text_is_truncated_with_a_marker() {
        let out = truncate("x".repeat(MAX_TEXT_BYTES + 100), MAX_TEXT_BYTES);
        assert!(out.ends_with("… [truncated]"));
    }

    #[test]
    fn browser_origins_normalize_default_ports_and_reject_non_network_urls() {
        assert_eq!(
            http_origin("https://example.com:443/path").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            http_origin("http://127.0.0.1:5173/a").unwrap(),
            "http://127.0.0.1:5173"
        );
        assert!(http_origin("file:///etc/passwd").is_err());
        assert!(http_origin("data:text/plain,hello").is_err());
    }

    #[test]
    fn request_guard_covers_subresources_and_preserves_inline_schemes() {
        assert_eq!(
            browser_request_guard_url("https://cdn.example/app.js", false).unwrap(),
            Some("https://cdn.example/app.js".to_string())
        );
        assert_eq!(
            browser_request_guard_url("ws://127.0.0.1/socket", false).unwrap(),
            Some("http://127.0.0.1/socket".to_string())
        );
        assert_eq!(
            browser_request_guard_url("data:text/plain,hello", false).unwrap(),
            None
        );
        assert!(browser_request_guard_url("data:text/plain,hello", true).is_err());
        assert!(browser_request_guard_url("file:///etc/passwd", false).is_err());
    }

    #[test]
    fn document_and_subresource_urls_are_normalized_for_guard() {
        assert!(browser_request_guard_url("https://other.example/frame", true).is_ok());
        assert!(browser_request_guard_url("https://other.example/image.png", false).is_ok());
        assert_eq!(
            browser_request_guard_url("about:blank", false).unwrap(),
            None
        );
    }

    #[test]
    fn human_lease_is_exclusive_and_invalidates_queued_action_ticket() {
        let control = BrowserControlState::new("session-1".to_string());
        let queued_ticket = control.action_ticket();
        let lease = control.take_control("user-1".to_string()).unwrap();
        assert_eq!(lease.owner.as_deref(), Some("user-1"));
        assert!(lease.generation > queued_ticket);
        let reconnected = control.take_control("user-1".to_string()).unwrap();
        assert_eq!(reconnected.generation, lease.generation);
        assert!(control.ensure_agent_control(queued_ticket).is_err());
        assert!(control.ensure_agent_control(lease.generation).is_err());
        assert!(control.take_control("other-user".to_string()).is_err());
        assert!(control.release_control("other-user").is_err());
        let released = control.release_control("user-1").unwrap();
        assert_eq!(released.owner, None);
        assert!(control.ensure_agent_control(released.generation).is_ok());
    }

    #[test]
    fn operator_pause_blocks_agent_actions_without_touching_the_lease() {
        let control = BrowserControlState::new("session-1".to_string());
        let ticket = control.action_ticket();
        control.set_operator_paused(true);
        assert!(control.operator_paused());
        let error = control.ensure_agent_control(ticket).unwrap_err();
        assert!(error.contains("paused by the operator"));
        // Pausing must never remove the human escape hatch.
        assert!(control.take_control("workspace-user".to_string()).is_ok());
        assert!(control.release_control("workspace-user").is_ok());
        control.set_operator_paused(false);
        assert!(control
            .ensure_agent_control(control.action_ticket())
            .is_ok());
    }

    #[test]
    fn watchdog_normalizes_only_epoch_ref_components() {
        assert_eq!(
            normalize_browser_ref("br:bs-1:tab:main:e12:s3"),
            "br:bs-1:tab:main:s3"
        );
        assert_eq!(
            normalize_browser_ref("br:east:tab:main:e12:s3"),
            "br:east:tab:main:s3"
        );
        assert_eq!(
            action_signature(&json!({
                "action": "click",
                "ref": "br:bs-1:tab:main:e12:s3"
            })),
            action_signature(&json!({
                "action": "click",
                "ref": "br:bs-1:tab:main:e13:s3"
            }))
        );
        assert_ne!(
            action_signature(&json!({
                "action": "click",
                "ref": "br:bs-1:tab:main:e12:s3"
            })),
            action_signature(&json!({
                "action": "click",
                "ref": "br:bs-1:tab:main:e13:s4"
            }))
        );
    }

    #[test]
    fn watchdog_detects_sliding_cycles_without_prefix_false_positive() {
        let cycle = VecDeque::from([
            "a".to_string(),
            "b".to_string(),
            "a".to_string(),
            "b".to_string(),
        ]);
        assert_eq!(repeated_action_cycle_depth(&cycle), 4);

        let prefixed = VecDeque::from([
            "other".to_string(),
            "a".to_string(),
            "b".to_string(),
            "a".to_string(),
            "b".to_string(),
            "a".to_string(),
        ]);
        assert_eq!(repeated_action_cycle_depth(&prefixed), 5);

        let broken = VecDeque::from([
            "a".to_string(),
            "b".to_string(),
            "a".to_string(),
            "c".to_string(),
        ]);
        assert_eq!(repeated_action_cycle_depth(&broken), 0);

        let bounded = VecDeque::from(vec!["a".to_string(); ACTION_LOOP_WINDOW]);
        assert_eq!(repeated_action_cycle_depth(&bounded), ACTION_LOOP_WINDOW);

        let control = BrowserControlState::new("session-1".to_string());
        for index in 0..(ACTION_LOOP_WINDOW + 3) {
            control.record_action(format!("unique-{index}")).unwrap();
        }
        let state = control
            .action_loop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.recent.len(), ACTION_LOOP_WINDOW);
        assert_eq!(state.recent.front().map(String::as_str), Some("unique-3"));
    }

    #[test]
    fn watchdog_escalates_at_five_eight_and_twelve_actions() {
        let control = BrowserControlState::new("session-1".to_string());
        for _ in 0..4 {
            assert!(control
                .record_action("same-action".to_string())
                .unwrap()
                .is_none());
        }
        assert!(control
            .record_action("same-action".to_string())
            .unwrap()
            .is_some_and(|notice| notice.contains("tier 5")));
        for _ in 0..2 {
            assert!(control
                .record_action("same-action".to_string())
                .unwrap()
                .is_some());
        }
        assert!(control
            .record_action("same-action".to_string())
            .unwrap()
            .is_some_and(|notice| notice.contains("tier 8")));
        for _ in 0..3 {
            assert!(control
                .record_action("same-action".to_string())
                .unwrap()
                .is_some());
        }
        assert!(control.record_action("same-action".to_string()).is_err());
    }

    #[test]
    fn repeated_identical_actions_trip_loop_watchdog_and_handoff_resets_it() {
        let control = BrowserControlState::new("session-1".to_string());
        for _ in 0..11 {
            control.record_action("same-action".to_string()).unwrap();
        }
        assert!(control.record_action("same-action".to_string()).is_err());
        control.take_control("user-1".to_string()).unwrap();
        control.release_control("user-1").unwrap();
        assert!(control.record_action("same-action".to_string()).is_ok());
    }

    #[test]
    fn alternating_actions_trip_rolling_watchdog_at_hard_stop() {
        let control = BrowserControlState::new("session-1".to_string());
        for index in 0..4 {
            let action = if index % 2 == 0 { "click:a" } else { "click:b" };
            assert!(control.record_action(action.to_string()).unwrap().is_none());
        }
        assert!(control
            .record_action("click:a".to_string())
            .unwrap()
            .is_some_and(|notice| notice.contains("tier 5")));
        for action in ["click:b", "click:a"] {
            assert!(control.record_action(action.to_string()).unwrap().is_some());
        }
        assert!(control
            .record_action("click:b".to_string())
            .unwrap()
            .is_some_and(|notice| notice.contains("tier 8")));
        for action in ["click:a", "click:b", "click:a"] {
            assert!(control.record_action(action.to_string()).unwrap().is_some());
        }
        assert!(control.record_action("click:b".to_string()).is_err());
    }

    #[test]
    fn stagnant_observation_fingerprints_escalate_through_same_tiers() {
        let control = BrowserControlState::new("session-1".to_string());
        for _ in 0..5 {
            control.record_observation_fingerprint("stable".to_string());
        }
        assert!(control
            .record_action("first".to_string())
            .unwrap()
            .is_none());

        control.record_observation_fingerprint("stable".to_string());
        assert!(control
            .record_action("second".to_string())
            .unwrap()
            .is_some_and(|notice| notice.contains("tier 5")));
        for _ in 0..3 {
            control.record_observation_fingerprint("stable".to_string());
        }
        assert!(control
            .record_action("third".to_string())
            .unwrap()
            .is_some_and(|notice| notice.contains("tier 8")));
        for _ in 0..4 {
            control.record_observation_fingerprint("stable".to_string());
        }
        assert!(control.record_action("fourth".to_string()).is_err());
    }

    #[tokio::test]
    async fn navigation_guard_rejects_overlong_urls_before_parse_or_dns() {
        let url = format!(
            "https://example.test/{}",
            "x".repeat(MAX_NAVIGATION_URL_BYTES)
        );
        assert!(browser_navigation_reject_reason(
            "open",
            &url,
            &BrowserNavigationTrustV1::default(),
        )
        .await
        .is_some());
    }

    /// The guard `navigate_locked` runs ahead of `goto`. Literal targets take
    /// net_guard's no-DNS fast path, so this proves the refusal happens before
    /// any Chrome request exists without needing a socket or a live process.
    #[tokio::test]
    async fn navigation_guard_refuses_private_literals_before_any_request() {
        let trust = BrowserNavigationTrustV1::default();
        for url in [
            "http://192.168.1.10/",
            "http://10.0.0.5/app",
            "http://172.16.0.9:8080/",
            "http://100.64.0.1/",
            "http://[fd00::5]/",
        ] {
            let reason = browser_navigation_reject_reason("navigate to", url, &trust)
                .await
                .unwrap_or_else(|| panic!("{url} must be refused before navigation"));
            assert!(
                reason.contains("Refusing to navigate to") && reason.contains(url),
                "a refused navigation must name its target: {reason}"
            );
            assert!(
                reason.contains("private/reserved"),
                "reason must state the range, not fail generically: {reason}"
            );
        }
    }

    /// The capability exists so the agent can look at its own dev server, so
    /// loopback has to survive the same guard that refuses the private LAN.
    #[tokio::test]
    async fn navigation_guard_still_allows_loopback_dev_servers() {
        let trust = BrowserNavigationTrustV1::default();
        for url in ["http://127.0.0.1:5173/", "http://[::1]:5173/"] {
            let verdict = browser_navigation_reject_reason("open", url, &trust).await;
            assert!(verdict.is_none(), "{url} must stay reachable: {verdict:?}");
        }
    }

    /// An operator-approved CIDR is scoped to itself: a neighbouring private
    /// address stays blocked, so one exception cannot widen into the whole LAN.
    #[tokio::test]
    async fn navigation_guard_honours_only_the_approved_trusted_cidr() {
        let trust = BrowserNavigationTrustV1 {
            trusted_domains: Vec::new(),
            trusted_cidrs: vec!["192.168.50.0/24".to_string()],
        };
        // Inside the approved /24 the target may pass; one address over, the
        // exception buys nothing.
        let url = "http://192.168.50.7/";
        let approved = browser_navigation_reject_reason("open", url, &trust).await;
        assert!(approved.is_none(), "approved CIDR refused: {approved:?}");
        let url = "http://192.168.51.7/";
        let neighbour = browser_navigation_reject_reason("open", url, &trust).await;
        assert!(
            neighbour.is_some(),
            "a private address just outside the approved CIDR must stay refused"
        );
    }

    /// Scheme filtering precedes any resolution, refusing non-network URLs
    /// without a DNS attempt.
    #[tokio::test]
    async fn navigation_guard_refuses_non_http_urls_before_resolution() {
        let trust = BrowserNavigationTrustV1::default();
        for url in ["file:///etc/passwd", "ftp://internal.example/"] {
            let verdict = browser_navigation_reject_reason("open", url, &trust).await;
            assert!(verdict.is_some(), "{url} must be refused: {verdict:?}");
        }
    }

    #[test]
    fn scoped_ref_fails_closed_after_epoch_invalidation() {
        let control = BrowserControlState::new("session-1".to_string());
        control
            .observation
            .lock()
            .unwrap()
            .selectors
            .insert("br:bs-1:tab:main:1:e0".into(), "#submit".into());
        assert_eq!(
            control.resolve_ref("br:bs-1:tab:main:1:e0").unwrap(),
            "#submit"
        );
        let prior_epoch = control.observation.lock().unwrap().epoch;
        control.invalidate_actions();
        assert!(control.resolve_ref("br:bs-1:tab:main:1:e0").is_err());
        assert_eq!(
            control.observation.lock().unwrap().epoch,
            prior_epoch.saturating_add(1)
        );
    }

    #[test]
    fn snapshot_refs_filter_sparse_nodes_and_preserve_static_identity() {
        let visible = RawSemanticNode {
            role: "button".into(),
            name: "Save".into(),
            tag: "button".into(),
            selector: "#save".into(),
            frame_id: "main".into(),
            visible: true,
            disabled: false,
            sensitive: false,
            bounds: SemanticBoundsV1 {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        };
        let hidden = RawSemanticNode {
            visible: false,
            ..visible.clone()
        };
        let sparse = RawSemanticNode {
            selector: String::new(),
            ..visible.clone()
        };
        assert!(snapshot_node_is_refable(&visible));
        assert!(snapshot_node_is_diffable(&visible));
        assert!(!snapshot_node_is_refable(&hidden));
        assert!(snapshot_node_is_diffable(&hidden));
        assert!(!snapshot_node_is_refable(&sparse));
        assert!(!snapshot_node_is_diffable(&sparse));

        let mut state = ObservationState::default();
        let first = stable_snapshot_ref(&mut state, "main", "#save");
        assert_eq!(stable_snapshot_ref(&mut state, "main", "#save"), first);
        assert_ne!(stable_snapshot_ref(&mut state, "frame-1", "#save"), first);
        assert_eq!(state.static_refs.len(), 2);

        // A filtered/scoped reread keeps node identity keyed by frame+selector;
        // it must not receive a fresh dense ref merely because another node was
        // omitted from this view.
        let filtered = stable_snapshot_ref(&mut state, "frame-1", "#save");
        assert_eq!(filtered, first + 1);
    }

    #[test]
    fn snapshot_fingerprint_rotates_for_ax_only_changes() {
        let before = [AxNodeSummaryV1 {
            role: "button".into(),
            name: "Save".into(),
            frame_id: "ax-main".into(),
        }];
        let after = [AxNodeSummaryV1 {
            role: "button".into(),
            name: "Save as".into(),
            frame_id: "ax-main".into(),
        }];
        assert_ne!(
            snapshot_fingerprint(&[], &before),
            snapshot_fingerprint(&[], &after)
        );
    }

    #[test]
    fn sensitive_snapshot_names_use_fixed_width_mask_before_fingerprint() {
        assert_eq!(
            sanitized_snapshot_name("supersecret", true),
            SENSITIVE_FIELD_MASK
        );
        assert_eq!(
            sanitized_snapshot_name("Authorization: Bearer abcdefghijklmnop", false),
            "Authorization: Bearer [REDACTED]"
        );

        let fingerprint = snapshot_fingerprint(
            &[RawSemanticNode {
                role: "textbox".into(),
                name: sanitized_snapshot_name("supersecret", true),
                tag: "input".into(),
                selector: "#password".into(),
                frame_id: "dom-main".into(),
                visible: true,
                disabled: false,
                sensitive: true,
                bounds: SemanticBoundsV1 {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
            }],
            &[],
        );
        assert!(!fingerprint.contains("supersecret"));
    }

    #[test]
    fn fresh_snapshot_epoch_supersedes_old_ref_but_keeps_static_component() {
        let mut state = ObservationState::default();
        let first_epoch = advance_snapshot_epoch(&mut state, "document-one".into());
        let first = snapshot_node_ref("bs-1", "tab-1", "dom-main", first_epoch, 4);
        let second_epoch = advance_snapshot_epoch(&mut state, "document-two".into());
        let second = snapshot_node_ref("bs-1", "tab-1", "dom-main", second_epoch, 4);

        assert_eq!(first_epoch, 1);
        assert_eq!(second_epoch, 2);
        assert_ne!(first, second);
        assert!(first.ends_with(":e1:s4"));
        assert!(second.ends_with(":e2:s4"));
    }

    #[test]
    fn escalation_seam_reuses_stable_ref_component_across_epoch_transition() {
        let mut state = ObservationState::default();
        let static_ref = stable_snapshot_ref(&mut state, "dom-main", "#save");
        let first_epoch = advance_snapshot_epoch(&mut state, "static-observation".into());
        let second_epoch = advance_snapshot_epoch(&mut state, "chrome-observation".into());

        let first = snapshot_node_ref("bs-1", "tab-1", "dom-main", first_epoch, static_ref);
        let second = snapshot_node_ref("bs-1", "tab-1", "dom-main", second_epoch, static_ref);
        assert_ne!(first, second, "epoch transition must supersede old refs");
        assert!(first.ends_with(&format!(":s{static_ref}")));
        assert!(second.ends_with(&format!(":s{static_ref}")));
    }

    #[test]
    fn snapshot_fingerprint_length_prefixes_attacker_controlled_text() {
        let first = RawSemanticNode {
            role: "button".into(),
            name: "left\u{1e}right".into(),
            tag: "button".into(),
            selector: "#save".into(),
            frame_id: "dom-main".into(),
            visible: true,
            disabled: false,
            sensitive: false,
            bounds: SemanticBoundsV1 {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        };
        let second = RawSemanticNode {
            name: "left".into(),
            selector: "#save\u{1e}right".into(),
            ..first.clone()
        };
        assert_ne!(
            snapshot_fingerprint(&[first], &[]),
            snapshot_fingerprint(&[second], &[])
        );
    }

    #[test]
    fn static_refs_are_bounded_with_lru_pruning() {
        let mut state = ObservationState::default();
        for index in 0..(MAX_STATIC_REFS + 1) {
            stable_snapshot_ref(&mut state, "dom-main", &format!("#node-{index}"));
        }
        assert_eq!(state.static_refs.len(), MAX_STATIC_REFS);
        assert!(!state.static_refs.contains_key("dom-main:#node-0"));
        let newest_selector = format!("#node-{}", MAX_STATIC_REFS);
        let newest = stable_snapshot_ref(&mut state, "dom-main", &newest_selector);
        assert_eq!(
            stable_snapshot_ref(&mut state, "dom-main", &newest_selector),
            newest
        );
    }

    #[test]
    fn dom_and_ax_frame_ids_use_distinct_namespaces() {
        assert_eq!(ax_frame_id("frame-1"), "ax-cdp:frame-1");
        assert_ne!(ax_frame_id("frame-1"), "dom-iframe:frame-1");
        assert_ne!("ax-main", "dom-main");
        assert_ne!("dom-iframe:iframe:0", "dom-iframe:iframe:1");
        assert_ne!(
            "dom-iframe:html:0/body:0/iframe:0",
            "dom-iframe:html:0/body:0/iframe:1"
        );
    }

    #[test]
    fn fresh_epoch_supersedes_refs_without_renumbering_static_identity() {
        let old = snapshot_node_ref("bs-1", "tab-1", "main", 4, 12);
        let fresh = snapshot_node_ref("bs-1", "tab-1", "main", 5, 12);
        assert_ne!(old, fresh);
        assert!(old.contains(":e4:s12"));
        assert!(fresh.contains(":e5:s12"));

        let main = snapshot_dom_key("main", "#save");
        let child = snapshot_dom_key("frame-1", "#save");
        assert_ne!(main, child);
        assert_eq!(
            snapshot_ax_key("frame-1", "button", "Save"),
            "ax:frame-1:button:Save"
        );
    }

    #[test]
    fn snapshot_text_masks_secrets_before_fingerprint_or_output() {
        let leaked = "Authorization: Bearer abcdefghijklmnop.qrstuvwxyz0123";
        let masked = mask_snapshot_text(leaked);
        assert!(masked.contains(crate::coding_tools::output_compaction::SECRET_MASK));
        assert!(!masked.contains("abcdefghijklmnop"));
        assert_eq!(mask_snapshot_text("Save document"), "Save document");
    }

    #[test]
    fn semantic_diff_reports_added_removed_and_changed_nodes() {
        let previous = BTreeMap::from([
            (
                "#old".into(),
                ("button".into(), "Old".into(), "main".into(), true, false),
            ),
            (
                "#same".into(),
                ("link".into(), "Before".into(), "main".into(), true, false),
            ),
        ]);
        let current = BTreeMap::from([
            (
                "#new".into(),
                ("button".into(), "New".into(), "main".into(), true, false),
            ),
            (
                "#same".into(),
                ("link".into(), "After".into(), "main".into(), true, false),
            ),
        ]);
        let diff = observation_diff(&previous, &current);
        assert_eq!(diff.added, ["#new"]);
        assert_eq!(diff.removed, ["#old"]);
        assert_eq!(diff.changed, ["#same"]);
    }

    #[test]
    fn semantic_diff_merges_frames_and_propagates_hidden_changes() {
        let previous = BTreeMap::from([
            (
                snapshot_dom_key("main", "#save"),
                ("button".into(), "Save".into(), "main".into(), true, false),
            ),
            (
                snapshot_dom_key("frame-1", "#save"),
                (
                    "button".into(),
                    "Save".into(),
                    "frame-1".into(),
                    true,
                    false,
                ),
            ),
        ]);
        let current = BTreeMap::from([
            (
                snapshot_dom_key("main", "#save"),
                ("button".into(), "Save".into(), "main".into(), false, false),
            ),
            (
                snapshot_dom_key("frame-1", "#save"),
                (
                    "button".into(),
                    "Save".into(),
                    "frame-1".into(),
                    true,
                    false,
                ),
            ),
        ]);
        let diff = observation_diff(&previous, &current);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert_eq!(diff.changed, [snapshot_dom_key("main", "#save")]);
    }

    #[test]
    fn frame_merge_keeps_same_selector_distinct_and_hidden_state_diffable() {
        let previous = BTreeMap::from([
            (
                snapshot_dom_key("main", "#save"),
                ("button".into(), "Save".into(), "main".into(), true, false),
            ),
            (
                snapshot_dom_key("frame-1", "#save"),
                (
                    "button".into(),
                    "Save".into(),
                    "frame-1".into(),
                    true,
                    false,
                ),
            ),
        ]);
        let current = BTreeMap::from([
            (
                snapshot_dom_key("main", "#save"),
                ("button".into(), "Save".into(), "main".into(), true, false),
            ),
            (
                snapshot_dom_key("frame-1", "#save"),
                (
                    "button".into(),
                    "Save".into(),
                    "frame-1".into(),
                    false,
                    false,
                ),
            ),
        ]);

        let diff = observation_diff(&previous, &current);
        assert_eq!(diff.changed, [snapshot_dom_key("frame-1", "#save")]);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn download_names_are_single_component_non_hidden_and_collision_neutral() {
        assert_eq!(
            sanitize_download_name("report (final).pdf").unwrap(),
            "report__final_.pdf"
        );
        for bad in ["../secret", "dir/file", ".cookies", ""] {
            assert!(sanitize_download_name(bad).is_err(), "{bad:?} must fail");
        }
    }
}
