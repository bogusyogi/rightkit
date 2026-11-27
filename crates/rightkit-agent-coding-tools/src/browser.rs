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

use crate::browser_fetch::wrap_untrusted_web_text;
use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use rightkit_agent_policy::net_guard::{
    resolve_browser_target_with_trust_and_allowlist,
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
    _resource_admission: crate::process_resource_authority::ProcessAdmission,
    _context_admission: crate::process_resource_authority::SpecificProcessAdmission,
    /// Chrome's only route to non-loopback hosts; stops when the session drops.
    _egress_proxy: EgressProxy,
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
        consent: crate::browser_consent::ConsentedLaunch,
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
        crate::browser_consent::register_session(&receipt_id, &session);
        Ok(session)
    }

    async fn launch(
        url: &str,
        owner_session_id: &str,
        navigation_trust: BrowserNavigationTrustV1,
        initial_host_policy: Vec<(String, BrowserNetworkDecisionV1)>,
        consent: Option<crate::browser_consent::ConsentedLaunch>,
    ) -> Result<Self, String> {
        navigation_trust.validate()?;
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
                    crate::browser_consent::ConsentedMode::Import => BrowserModeV1::Headless,
                    crate::browser_consent::ConsentedMode::Attach => BrowserModeV1::Visible,
                },
            ),
        };
        control
            .host_policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(initial_host_policy);
        let egress_proxy = EgressProxy::start(
            Arc::clone(&navigation_trust),
            Arc::clone(&control.host_policy),
        )
        .await?;
        let profile_prefix = format!("{}-browser-", rightkit_agent_config::host_slug());
        let profile = tempfile::Builder::new()
            .prefix(&profile_prefix)
            .tempdir()
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
        // Every non-loopback connection goes through the egress proxy, which
        // resolves and validates the name once and connects to those addresses.
        // Chrome never resolves proxied names itself (SOCKS5 sends the hostname),
        // so a rebinding answer cannot differ from the checked one. Chrome never
        // proxies loopback, which the guard allows anyway.
        options.extra_args.push(format!(
            "--proxy-server=socks5://127.0.0.1:{}",
            egress_proxy.port
        ));
        // Chrome cannot run its own sandbox as root. Containers and CI run as root,
        // so the operator may opt out of Chrome's sandbox there explicitly; without
        // that opt-in a root launch is refused with the reason, never weakened.
        if running_as_root() {
            let no_sandbox_variable = rightkit_agent_config::env_name("BROWSER_NO_SANDBOX");
            if std::env::var(&no_sandbox_variable).as_deref() == Ok("1") {
                options.extra_args.push("--no-sandbox".to_string());
            } else {
                return Err(format!(
                    "the browser cannot start as root: Chrome refuses to run its sandbox for \
                     the root user. Run the host app as a normal user, or, in a disposable container \
                     or CI, set {no_sandbox_variable}=1 to run Chrome without its sandbox."
                ));
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
        let resource_admission = crate::process_resource_authority::reserve_process(
            crate::process_resource_authority::ResourceKind::BrowserProcess,
            rightkit_agent_engine_runtime_resources::ServiceScope::Session(
                owner_session_id.to_string(),
            ),
            true,
        )
        .map_err(|error| format!("browser resource admission failed before launch: {error}"))?;
        // A browser session is one isolated CDP context. Pages within it are
        // deliberately uncounted. Acquire this specific permit only after the
        // process admission so aggregate ExternalExecution is charged once.
        let context_admission = match crate::process_resource_authority::reserve_specific_process(
            crate::process_resource_authority::ResourceKind::BrowserContext,
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
            _egress_proxy: egress_proxy,
        };
        // Every arm launches Chrome as this process's child (the consented "attach" plan too: it starts a
        // private copy of the profile), so any session with a process id is ours to close.
        let pid = session.browser.process_id();
        register_owned_browser(OwnedBrowser {
            pid,
            profile_dir: session._profile.path().to_path_buf(),
            #[cfg(windows)]
            handle: pid.and_then(open_owned_process),
        });
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
            crate::output_compaction::mask_secrets(&text),
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
        // Chrome is closed; the profile dir stays registered until Drop deletes it.
        for entry in OWNED_BROWSERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter_mut()
            .filter(|e| e.profile_dir == self._profile.path())
        {
            entry.pid = None;
        }
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        // Field drop order (browser before _profile) closes Chrome, then deletes the profile dir; neither is
        // ours to clean at exit any more.
        unregister_owned_browser(self._profile.path());
    }
}

/// A browser session the host app launched and has not cleaned up: Chrome's process id (None once it was
/// closed) and the session's temporary profile dir.
struct OwnedBrowser {
    pid: Option<u32>,
    profile_dir: std::path::PathBuf,
    /// Windows: a handle to Chrome's browser process, opened right after launch. Holding it keeps the pid
    /// from being reused, so the exit hook terminates exactly the process we launched. Closed on drop.
    #[cfg(windows)]
    handle: Option<std::os::windows::io::OwnedHandle>,
}

/// Open Chrome's browser process for termination and waiting, or `None` when it cannot be opened.
#[cfg(windows)]
fn open_owned_process(pid: u32) -> Option<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::FromRawHandle;
    use winapi::um::winnt::{PROCESS_TERMINATE, SYNCHRONIZE};
    // SAFETY: OpenProcess takes plain values and returns a new handle or null.
    let raw = unsafe {
        winapi::um::processthreadsapi::OpenProcess(PROCESS_TERMINATE | SYNCHRONIZE, 0, pid)
    };
    if raw.is_null() {
        tracing::warn!(
            pid,
            "could not open Chrome's process; its profile dir may be left at exit"
        );
        return None;
    }
    // SAFETY: `raw` is a valid process handle we own and close exactly once, through OwnedHandle.
    Some(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw.cast()) })
}

/// Browsers the host app launched and has not cleaned up. A browser session is closed when it is dropped or
/// shut down, but a process that ends with `std::process::exit` (every CLI exit does) runs no destructor,
/// so Chrome would outlive the host app, orphaned (issue 465), and `TempDir`'s destructor would not delete the
/// profile dir either. The registry therefore also holds each session's temporary profile dir. It holds ONLY
/// that `TempDir` path, never the consented imported profile under `profiles_root`, which
/// `purge_imported_profile` owns.
static OWNED_BROWSERS: std::sync::Mutex<Vec<OwnedBrowser>> = std::sync::Mutex::new(Vec::new());

fn register_owned_browser(entry: OwnedBrowser) {
    OWNED_BROWSERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(entry);
}

fn unregister_owned_browser(profile_dir: &std::path::Path) {
    OWNED_BROWSERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|e| e.profile_dir != profile_dir);
}

/// Close every browser this process launched and has not closed: SIGTERM to each Chrome process group,
/// up to `grace` for them to exit, then SIGKILL. For the process exit path, where destructors do not run.
/// A group is signalled only while its leader is still this process's child (an unreaped child cannot have
/// had its pid reused), so a Chrome that already exited and was reaped never leads to signalling a stranger.
/// On Windows the kill-on-close job object would end Chrome only after this process exits, too late for the
/// profile dir, so each still-open browser process is terminated through the handle taken at launch, when one
/// could be opened (no pid reuse is possible while it is held), and waited for within `grace`. Only the browser
/// process is terminated; its renderer, GPU, utility and crashpad children exit on their own once it is gone.
/// Then every registered temporary profile dir is removed, best effort. On Windows those children may still
/// hold profile files for a moment, so removal is retried until the same `grace` deadline; a child that
/// outlives it leaves the dir behind.
pub fn close_owned_browsers_at_exit(grace: std::time::Duration) {
    #[cfg(windows)]
    let deadline = std::time::Instant::now() + grace;
    let entries: Vec<OwnedBrowser> = std::mem::take(
        &mut *OWNED_BROWSERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    #[cfg(unix)]
    {
        let pids: Vec<u32> = entries.iter().filter_map(|e| e.pid).collect();
        // waitpid(WNOHANG): 0 = our child, still running; pid = our child, exited now (reaped here, its
        // group may still have members); -1 (ECHILD) = not our child any more, so the pid may be reused.
        let still_ours = |pid: libc::pid_t| {
            let mut status = 0;
            // SAFETY: waitpid with WNOHANG on a single pid; `status` is a valid out pointer.
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) >= 0 }
        };
        let groups: Vec<libc::pid_t> = pids
            .into_iter()
            .filter_map(|pid| libc::pid_t::try_from(pid).ok())
            .filter(|pid| *pid > 1 && still_ours(*pid))
            .collect();
        // SAFETY: kill(2) with a negative pid signals that process group; no memory is involved.
        let signal = |pgid: libc::pid_t, sig: libc::c_int| unsafe { libc::kill(-pgid, sig) } == 0;
        for pgid in &groups {
            signal(*pgid, libc::SIGTERM);
        }
        let deadline = std::time::Instant::now() + grace;
        let alive = |pgid: libc::pid_t| {
            // Reap the leader first: an unreaped zombie leader still answers kill(-pgid, 0).
            let mut status = 0;
            // SAFETY: as above.
            unsafe { libc::waitpid(pgid, &mut status, libc::WNOHANG) };
            signal(pgid, 0)
        };
        while std::time::Instant::now() < deadline && groups.iter().any(|g| alive(*g)) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let mut killed: Vec<libc::pid_t> = groups.iter().copied().filter(|g| alive(*g)).collect();
        for pgid in &killed {
            signal(*pgid, libc::SIGKILL);
        }
        // wait (bounded) for killed leaders to die so they stop writing to the profile dir before it is removed
        let reap_deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !killed.is_empty() && std::time::Instant::now() < reap_deadline {
            killed.retain(|pgid| {
                let mut status = 0;
                // SAFETY: waitpid with WNOHANG on a single pid; `status` is a valid out pointer.
                // 0 = still running; the pgid (reaped) or -1 (not our child) = done.
                unsafe { libc::waitpid(*pgid, &mut status, libc::WNOHANG) == 0 }
            });
            if !killed.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let live = || {
            entries
                .iter()
                .filter(|e| e.pid.is_some())
                .filter_map(|e| e.handle.as_ref())
        };
        for handle in live() {
            // SAFETY: a live process handle with PROCESS_TERMINATE access.
            unsafe {
                winapi::um::processthreadsapi::TerminateProcess(handle.as_raw_handle().cast(), 1)
            };
        }
        for handle in live() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let millis = u32::try_from(left.as_millis()).unwrap_or(u32::MAX);
            // SAFETY: a live process handle with SYNCHRONIZE access; the wait is bounded by the remaining grace.
            unsafe {
                winapi::um::synchapi::WaitForSingleObject(handle.as_raw_handle().cast(), millis)
            };
        }
    }
    for entry in &entries {
        #[cfg(windows)]
        while std::fs::remove_dir_all(&entry.profile_dir).is_err()
            && entry.profile_dir.exists()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        #[cfg(not(windows))]
        if let Err(error) = std::fs::remove_dir_all(&entry.profile_dir) {
            tracing::debug!(
                %error,
                profile_dir = %entry.profile_dir.display(),
                "browser temporary profile dir was not removed at exit"
            );
        }
    }
}

fn mask_snapshot_text(text: &str) -> String {
    crate::output_compaction::mask_secrets(text)
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

/// A loopback SOCKS5 proxy (RFC 1928, CONNECT only, no auth) that is the single
/// resolver for one browser session. Each connection's name is resolved once,
/// checked by the same navigation guard and trust policy the interceptor uses,
/// and connected to exactly the validated addresses. This closes DNS rebinding:
/// the interceptor's own lookup only decides whether a request may start.
///
/// Exposure: the listener binds 127.0.0.1 only (never 0.0.0.0 or ::), lives
/// exactly as long as its `BrowserSession`, and is unauthenticated because
/// Chrome cannot send SOCKS5 credentials from launch flags. Any local process
/// that finds the port while the session lives can use it, but it gets nothing
/// the guard would not allow the session itself: every target is still resolved
/// and checked here. When an upstream HTTP proxy is configured, such a process
/// could reach guard-approved destinations through that proxy's credentials.
///
/// Limits: the SOCKS greeting and request must finish within
/// `EGRESS_HANDSHAKE_TIMEOUT`, resolving and connecting within
/// `EGRESS_CONNECT_TIMEOUT`, a relay closes after `EGRESS_IDLE_TIMEOUT` without
/// bytes in either direction, and at most `EGRESS_MAX_RELAYS` relays run at once
/// (further clients are closed at accept).
struct EgressProxy {
    port: u16,
    task: tokio::task::JoinHandle<()>,
}

const EGRESS_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const EGRESS_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const EGRESS_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const EGRESS_MAX_RELAYS: usize = 256;
/// Upper bound on an upstream proxy's CONNECT response head.
const EGRESS_MAX_PROXY_RESPONSE: usize = 16 * 1024;

impl Drop for EgressProxy {
    fn drop(&mut self) {
        // Aborting the accept loop drops its JoinSet, which aborts every relay.
        self.task.abort();
    }
}

impl EgressProxy {
    async fn start(
        navigation_trust: Arc<std::sync::Mutex<BrowserNavigationTrustV1>>,
        host_policy: Arc<std::sync::Mutex<HashMap<String, BrowserNetworkDecisionV1>>>,
    ) -> Result<Self, String> {
        // `--proxy-server` makes Chrome ignore HTTP(S)_PROXY/ALL_PROXY/PAC, so the
        // relay itself must honour them. A set but unusable value fails closed.
        let upstream = Arc::new(UpstreamProxyConfig::from_env()?);
        // Loopback only: the proxy must never be reachable from another host.
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|e| format!("could not start the browser egress proxy: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("could not start the browser egress proxy: {e}"))?
            .port();
        let task = tokio::spawn(async move {
            let mut relays = tokio::task::JoinSet::new();
            loop {
                let accepted = listener.accept().await;
                while relays.try_join_next().is_some() {}
                let Ok((client, _)) = accepted else {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    continue;
                };
                if relays.len() >= EGRESS_MAX_RELAYS {
                    tracing::warn!(
                        limit = EGRESS_MAX_RELAYS,
                        "browser egress proxy at its relay limit; closing new connection"
                    );
                    drop(client);
                    continue;
                }
                let trust = Arc::clone(&navigation_trust);
                let policy = Arc::clone(&host_policy);
                let upstream = Arc::clone(&upstream);
                relays.spawn(async move {
                    if let Err(error) = socks5_relay(client, trust, policy, upstream).await {
                        tracing::debug!(%error, "browser egress relay ended with an error");
                    }
                });
            }
        });
        Ok(Self { port, task })
    }
}

/// Reads the SOCKS5 greeting and CONNECT request. `Ok(None)` means the client
/// was already answered with a refusal.
async fn socks5_read_request(
    client: &mut tokio::net::TcpStream,
) -> std::io::Result<Option<(String, u16)>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const UNSUPPORTED: [u8; 10] = [5, 7, 0, 1, 0, 0, 0, 0, 0, 0]; // command not supported

    let mut greeting = [0u8; 2];
    client.read_exact(&mut greeting).await?;
    let mut methods = vec![0u8; usize::from(greeting[1])];
    client.read_exact(&mut methods).await?;
    if greeting[0] != 5 || !methods.contains(&0) {
        client.write_all(&[5, 0xff]).await?;
        return Ok(None);
    }
    client.write_all(&[5, 0]).await?;

    let mut request = [0u8; 4];
    client.read_exact(&mut request).await?;
    // Hosts are spelled as `url::Url::host_str` spells them, so host-policy
    // keys and trusted domains match the interceptor's view of the same request.
    let host = match request[3] {
        1 => {
            let mut ip = [0u8; 4];
            client.read_exact(&mut ip).await?;
            std::net::Ipv4Addr::from(ip).to_string()
        }
        3 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len).await?;
            let mut name = vec![0u8; usize::from(len[0])];
            client.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).to_ascii_lowercase()
        }
        4 => {
            let mut ip = [0u8; 16];
            client.read_exact(&mut ip).await?;
            format!("[{}]", std::net::Ipv6Addr::from(ip))
        }
        _ => {
            client.write_all(&UNSUPPORTED).await?;
            return Ok(None);
        }
    };
    let port = client.read_u16().await?;
    if request[0] != 5 || request[1] != 1 {
        client.write_all(&UNSUPPORTED).await?;
        return Ok(None);
    }
    Ok(Some((host, port)))
}

async fn socks5_relay(
    mut client: tokio::net::TcpStream,
    navigation_trust: Arc<std::sync::Mutex<BrowserNavigationTrustV1>>,
    host_policy: Arc<std::sync::Mutex<HashMap<String, BrowserNetworkDecisionV1>>>,
    upstream_proxy: Arc<UpstreamProxyConfig>,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    const REFUSED: [u8; 10] = [5, 2, 0, 1, 0, 0, 0, 0, 0, 0]; // not allowed by ruleset
    const UNREACHABLE: [u8; 10] = [5, 5, 0, 1, 0, 0, 0, 0, 0, 0]; // connection refused
    const TIMED_OUT: [u8; 10] = [5, 4, 0, 1, 0, 0, 0, 0, 0, 0]; // host unreachable

    let request = tokio::time::timeout(EGRESS_HANDSHAKE_TIMEOUT, socks5_read_request(&mut client))
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "SOCKS5 handshake timed out")
        })??;
    let Some((host, port)) = request else {
        return Ok(());
    };

    let trust = navigation_trust
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let Ok(policy) = trust.policy() else {
        return client.write_all(&REFUSED).await;
    };
    let host_allowlisted = host_policy
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&host)
        .copied()
        == Some(BrowserNetworkDecisionV1::Allow);
    // The guard always resolves and checks the name here, with or without an
    // upstream proxy; only the checked addresses are ever dialled or named.
    let addrs = match tokio::time::timeout(
        EGRESS_CONNECT_TIMEOUT,
        resolve_browser_target_with_trust_and_allowlist(&host, port, &policy, host_allowlisted),
    )
    .await
    {
        Ok(Ok(addrs)) if !addrs.is_empty() => addrs,
        Ok(_) => return client.write_all(&REFUSED).await,
        Err(_) => return client.write_all(&TIMED_OUT).await,
    };
    let route = upstream_proxy.route(&host, port);
    let connected = tokio::time::timeout(EGRESS_CONNECT_TIMEOUT, async {
        match route {
            None => tokio::net::TcpStream::connect(&addrs[..]).await,
            Some(proxy) => proxy.connect_tunnel(&addrs).await,
        }
    })
    .await;
    let upstream = match connected {
        Ok(Ok(upstream)) => upstream,
        Ok(Err(_)) => return client.write_all(&UNREACHABLE).await,
        Err(_) => return client.write_all(&TIMED_OUT).await,
    };
    client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    relay_with_idle_timeout(client, upstream, EGRESS_IDLE_TIMEOUT).await
}

/// Copies bytes both ways until both sides close, an error occurs, or `idle`
/// passes with no bytes moving in either direction (reads or writes).
async fn relay_with_idle_timeout(
    a: tokio::net::TcpStream,
    b: tokio::net::TcpStream,
    idle: std::time::Duration,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let timed_out =
        || std::io::Error::new(std::io::ErrorKind::TimedOut, "browser egress relay idle");
    let (mut a_read, mut a_write) = a.into_split();
    let (mut b_read, mut b_write) = b.into_split();
    let mut a_buf = vec![0u8; 16 * 1024];
    let mut b_buf = vec![0u8; 16 * 1024];
    let (mut a_open, mut b_open) = (true, true);
    while a_open || b_open {
        // A fresh sleep per iteration: any completed read restarts the clock.
        tokio::select! {
            read = a_read.read(&mut a_buf), if a_open => {
                let n = read?;
                if n == 0 {
                    a_open = false;
                    if let Err(error) = b_write.shutdown().await {
                        tracing::debug!(%error, "browser egress relay: closing the write side failed");
                    }
                } else {
                    tokio::time::timeout(idle, b_write.write_all(&a_buf[..n]))
                        .await
                        .map_err(|_| timed_out())??;
                }
            }
            read = b_read.read(&mut b_buf), if b_open => {
                let n = read?;
                if n == 0 {
                    b_open = false;
                    if let Err(error) = a_write.shutdown().await {
                        tracing::debug!(%error, "browser egress relay: closing the write side failed");
                    }
                } else {
                    tokio::time::timeout(idle, a_write.write_all(&b_buf[..n]))
                        .await
                        .map_err(|_| timed_out())??;
                }
            }
            () = tokio::time::sleep(idle) => return Err(timed_out()),
        }
    }
    Ok(())
}

/// An HTTP proxy the relay tunnels through with `CONNECT`. The proxy itself is
/// operator configuration and is dialled without the navigation guard (it is
/// usually loopback or private); the guard governs only the destinations.
#[derive(Clone, Debug, PartialEq, Eq)]
struct UpstreamProxy {
    host: String,
    port: u16,
    /// `Proxy-Authorization` value from the URL's userinfo; never logged.
    authorization: Option<String>,
}

impl UpstreamProxy {
    /// Parses one proxy variable. A bare `host:port` means `http://host:port`.
    /// Only `http://` is supported; anything else fails closed. Errors name the
    /// variable, never its value, which may carry credentials.
    fn parse(variable: &str, value: &str) -> Result<Self, String> {
        let value = value.trim();
        let with_scheme = if value.contains("://") {
            value.to_string()
        } else {
            format!("http://{value}")
        };
        let invalid = || {
            format!(
                "{variable} is set but is not a usable proxy URL; the browser will not start \
                 rather than bypass the configured proxy"
            )
        };
        let url = url::Url::parse(&with_scheme).map_err(|_| invalid())?;
        if url.scheme() != "http" {
            return Err(format!(
                "{variable} uses the `{}` scheme; the browser egress proxy can only tunnel \
                 through an http:// proxy, so the browser will not start rather than bypass it",
                url.scheme()
            ));
        }
        let host = match url.host().ok_or_else(invalid)? {
            url::Host::Domain(domain) if !domain.is_empty() => domain.to_string(),
            url::Host::Domain(_) => return Err(invalid()),
            url::Host::Ipv4(ip) => ip.to_string(),
            url::Host::Ipv6(ip) => ip.to_string(),
        };
        let port = url.port_or_known_default().ok_or_else(invalid)?;
        let authorization = if url.username().is_empty() && url.password().is_none() {
            None
        } else {
            let user = percent_decode(url.username()).ok_or_else(invalid)?;
            let password = percent_decode(url.password().unwrap_or("")).ok_or_else(invalid)?;
            let mut credentials = user;
            credentials.push(b':');
            credentials.extend_from_slice(&password);
            Some(format!(
                "Basic {}",
                rightkit_crypto::base64_encode(&credentials)
            ))
        };
        Ok(Self {
            host,
            port,
            authorization,
        })
    }

    /// Opens a `CONNECT` tunnel to the first checked address the proxy accepts.
    /// The request names the checked IP literal, never the hostname, so the
    /// proxy cannot resolve the name to a different address. Chrome runs TLS end
    /// to end through the tunnel.
    async fn connect_tunnel(
        &self,
        targets: &[std::net::SocketAddr],
    ) -> std::io::Result<tokio::net::TcpStream> {
        let mut last_error = std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no checked address to connect to",
        );
        for target in targets {
            match self.connect_one(*target).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    async fn connect_one(
        &self,
        target: std::net::SocketAddr,
    ) -> std::io::Result<tokio::net::TcpStream> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect((self.host.as_str(), self.port)).await?;
        let authority = connect_authority(target);
        let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
        if let Some(authorization) = &self.authorization {
            request.push_str("Proxy-Authorization: ");
            request.push_str(authorization);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).await?;

        // Read the response head one byte at a time so no tunnel bytes that
        // follow it are consumed here.
        let mut head = Vec::with_capacity(256);
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() >= EGRESS_MAX_PROXY_RESPONSE {
                return Err(std::io::Error::other(
                    "upstream proxy response head too large",
                ));
            }
            if stream.read(&mut byte).await? == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "upstream proxy closed during CONNECT",
                ));
            }
            head.push(byte[0]);
        }
        let status_line = head.split(|b| *b == b'\n').next().unwrap_or_default();
        let status = String::from_utf8_lossy(status_line);
        let mut parts = status.split_whitespace();
        let version_ok = parts.next().is_some_and(|v| v.starts_with("HTTP/1."));
        let code = parts.next().unwrap_or_default();
        if version_ok && code.len() == 3 && code.starts_with('2') {
            Ok(stream)
        } else {
            Err(std::io::Error::other(format!(
                "upstream proxy refused CONNECT: {}",
                status.trim()
            )))
        }
    }
}

/// `ip:port`, bracketing IPv6 as RFC 9110 authority form requires.
fn connect_authority(target: std::net::SocketAddr) -> String {
    match target.ip() {
        std::net::IpAddr::V4(ip) => format!("{ip}:{}", target.port()),
        std::net::IpAddr::V6(ip) => format!("[{ip}]:{}", target.port()),
    }
}

fn percent_decode(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// One `NO_PROXY` entry, matched against the hostname Chrome asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum NoProxyEntry {
    /// `*`: no destination uses the upstream proxy.
    All,
    /// `example.com`, `.example.com` or `*.example.com`: the name and its subdomains.
    Domain(String),
    /// An IP literal, matched only against an IP-literal destination.
    Ip(std::net::IpAddr),
    /// `10.0.0.0/8` style, matched only against an IP-literal destination.
    Cidr(std::net::IpAddr, u8),
}

impl NoProxyEntry {
    /// Parses one entry. Unparseable entries are dropped, which sends more
    /// traffic through the proxy, never less.
    fn parse(raw: &str) -> Option<Self> {
        let entry = raw.trim().to_ascii_lowercase();
        if entry.is_empty() {
            return None;
        }
        if entry == "*" {
            return Some(Self::All);
        }
        if let Some((addr, len)) = entry.split_once('/') {
            let ip = addr
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .ok()?;
            let len = len.parse::<u8>().ok()?;
            let max = if ip.is_ipv4() { 32 } else { 128 };
            return (len <= max).then_some(Self::Cidr(ip, len));
        }
        let unbracketed = entry.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = unbracketed.parse::<std::net::IpAddr>() {
            return Some(Self::Ip(ip));
        }
        // `[v6]:port` or `name:port`: the port is ignored, as curl does.
        if let Some((bracketed, _)) = entry.strip_prefix('[').and_then(|e| e.split_once("]:")) {
            return bracketed.parse().ok().map(Self::Ip);
        }
        let name = match entry.rsplit_once(':') {
            Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
            _ => entry.as_str(),
        };
        if let Ok(ip) = name.parse::<std::net::IpAddr>() {
            return Some(Self::Ip(ip));
        }
        let domain = name.trim_start_matches('*').trim_start_matches('.');
        (!domain.is_empty()).then(|| Self::Domain(domain.to_string()))
    }

    /// `host` is spelled as the SOCKS request gives it (lowercase name, or an
    /// IP literal with IPv6 in brackets).
    fn matches(&self, host: &str) -> bool {
        let ip = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .ok();
        match (self, ip) {
            (Self::All, _) => true,
            (Self::Ip(entry), Some(ip)) => *entry == ip,
            (Self::Cidr(net, len), Some(ip)) => ip_in_cidr(ip, *net, *len),
            (Self::Domain(domain), None) => {
                let host = host.trim_end_matches('.');
                host == domain
                    || host
                        .strip_suffix(domain.as_str())
                        .is_some_and(|prefix| prefix.ends_with('.'))
            }
            _ => false,
        }
    }
}

fn ip_in_cidr(ip: std::net::IpAddr, net: std::net::IpAddr, len: u8) -> bool {
    match (ip, net) {
        (std::net::IpAddr::V4(ip), std::net::IpAddr::V4(net)) => {
            len == 0 || (u32::from(ip) ^ u32::from(net)) >> (32 - u32::from(len)) == 0
        }
        (std::net::IpAddr::V6(ip), std::net::IpAddr::V6(net)) => {
            len == 0 || (u128::from(ip) ^ u128::from(net)) >> (128 - u32::from(len)) == 0
        }
        _ => false,
    }
}

/// The upstream proxies Chrome would have used without `--proxy-server`, read
/// from the standard variables (lowercase wins over uppercase, as curl does).
/// Port 80 uses `http_proxy`, every other port `https_proxy`; each falls back to
/// `all_proxy`. `no_proxy` destinations connect directly, exactly as with no
/// upstream configured.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct UpstreamProxyConfig {
    http: Option<UpstreamProxy>,
    https: Option<UpstreamProxy>,
    all: Option<UpstreamProxy>,
    no_proxy: Vec<NoProxyEntry>,
}

impl UpstreamProxyConfig {
    fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let first_set = |names: [&'static str; 2]| {
            names.into_iter().find_map(|name| {
                lookup(name)
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| (name, value))
            })
        };
        let proxy = |names: [&'static str; 2]| {
            first_set(names)
                .map(|(name, value)| UpstreamProxy::parse(name, &value))
                .transpose()
        };
        Ok(Self {
            http: proxy(["http_proxy", "HTTP_PROXY"])?,
            https: proxy(["https_proxy", "HTTPS_PROXY"])?,
            all: proxy(["all_proxy", "ALL_PROXY"])?,
            no_proxy: first_set(["no_proxy", "NO_PROXY"])
                .map(|(_, value)| {
                    value
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter_map(NoProxyEntry::parse)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// The proxy for a checked destination, or `None` to connect directly.
    fn route(&self, host: &str, port: u16) -> Option<&UpstreamProxy> {
        if self.no_proxy.iter().any(|entry| entry.matches(host)) {
            return None;
        }
        let scheme_proxy = if port == 80 { &self.http } else { &self.https };
        scheme_proxy.as_ref().or(self.all.as_ref())
    }
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
    if let Some(reason) = crate::browser_fetch::blocked_fetch_host(url) {
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
        // `String::truncate` panics inside a multi-byte character; page text
        // is often non-ASCII, so cut at the nearest boundary at or below `max`.
        let mut cut = max;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
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

/// What a consented browser launch does with the user's profile.
enum ConsentedPlan {
    Import,
    Attach,
}

/// Point the launch options at the consented browser's executable and a private copy of
/// its profile. Chromium-family only; the consent must still be active.
fn consented_launch_options(
    consent: &crate::browser_consent::ConsentedLaunch,
    options: &mut rightkit_browser::LaunchOptions,
) -> Result<ConsentedPlan, String> {
    use crate::browser_consent::ConsentedMode;
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
                name: crate::browser_consent::imported_profile_name(receipt),
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
                    None => crate::browser_consent::ConsentedMode::Import,
                    Some(raw) => match crate::browser_consent::ConsentedMode::parse(raw) {
                        Some(mode) => mode,
                        None => {
                            return ToolResult::error(format!(
                                "Unknown mode '{raw}': use import or attach"
                            ))
                        }
                    },
                };
                let launch = match crate::browser_consent::ConsentStore::product()
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
        let observation_json =
            serde_json::to_string_pretty(&observation).unwrap_or_else(|_| "{}".to_string());

        // Page content is attacker-controlled — the classic prompt-injection vector.
        let body = if crate::tool_options::browser_compact_observations() {
            // The observation already carries `dom_fallback`, so print it once, there.
            format!(
                "url: {}\ntitle: {}\n\n--- text ---\n{text}\n\n--- observation (interactive elements are in `domFallback`) ---\n{observation_json}",
                observation.url,
                observation.title
            )
        } else {
            let elements = serde_json::to_string_pretty(&observation.dom_fallback)
                .unwrap_or_else(|_| "[]".to_string());
            format!(
                "url: {}\ntitle: {}\n\n--- text ---\n{text}\n\n--- interactive elements ---\n{elements}\n\n--- observation ---\n{observation_json}",
                observation.url,
                observation.title
            )
        };
        ToolResult::ok(wrap_untrusted_web_text("browser_read", &body))
    }
}

/// After an action, keep only the added/changed elements when the page changed
/// a little; the full snapshot stays when there is no baseline or the page
/// changed wholesale. Returns a one-line note for the reply (empty if unchanged).
fn shrink_observation_to_diff(observation: &mut BrowserObservationV1) -> String {
    let diff = &observation.diff;
    let changes = diff.added.len() + diff.removed.len() + diff.changed.len();
    let total = observation.dom_fallback.len() + observation.ax_nodes.len();
    let no_baseline = diff.removed.is_empty()
        && diff.changed.is_empty()
        && !diff.added.is_empty()
        && diff.added.len() >= total;
    if no_baseline || changes * 2 > total {
        return String::new();
    }
    let keep: std::collections::HashSet<String> = diff
        .added
        .iter()
        .chain(diff.changed.iter())
        .cloned()
        .collect();
    observation
        .dom_fallback
        .retain(|node| keep.contains(&snapshot_dom_key(&node.frame_id, &node.selector)));
    observation.ax_nodes.clear();
    "page changed a little: showing only added/changed elements (see `diff`); unchanged elements keep their selectors, call browser_read for the full list.\n"
        .to_string()
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
                    Ok(mut observation) => {
                        let note = if crate::tool_options::browser_compact_observations() {
                            shrink_observation_to_diff(&mut observation)
                        } else {
                            String::new()
                        };
                        ToolResult::ok(format!(
                            "{done}\n{note}{}{}",
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
                        ))
                    }
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
        let body = crate::output_compaction::mask_secrets(&body);
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
