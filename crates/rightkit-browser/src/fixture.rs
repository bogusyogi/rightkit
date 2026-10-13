//! Source-only browser fixture runtime.
//!
//! This API intentionally owns its browser, profile, proxy, fixture server, and action list.
//! It exposes neither raw CDP nor arbitrary JavaScript and can emit a passed outcome only
//! after the document identity remains bound and all disposable resources are cleaned up.

use crate::fixture_protocol::{FixtureAction, FixtureError, FixtureIntent, FixtureKey};
use crate::fixture_server::FixtureServer;
use crate::page::{BrowserPage, Target};
use crate::proxy::FixtureProxyPolicy;
use crate::session::BrowserSession;
use chromiumoxide::cdp::browser_protocol::emulation::SetScriptExecutionDisabledParams;
#[cfg(windows)]
use chromiumoxide::cdp::browser_protocol::fetch::{
    EnableParams as FetchEnableParams, EventRequestPaused, FailRequestParams, FulfillRequestParams,
    HeaderEntry, RequestPattern,
};
use chromiumoxide::cdp::browser_protocol::inspector::EventTargetCrashed;
use chromiumoxide::cdp::browser_protocol::page::{
    CreateIsolatedWorldParams, EnableParams as PageEnableParams, EventDocumentOpened,
    EventFrameNavigated, EventNavigatedWithinDocument, GetFrameTreeParams,
    SetWebLifecycleStateParams, SetWebLifecycleStateState, StopLoadingParams,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    CallArgument, CallFunctionOnParams, EnableParams as RuntimeEnableParams,
    EventExecutionContextCreated, EventExecutionContextDestroyed, EventExecutionContextsCleared,
};
use futures::{FutureExt, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[cfg(windows)]
use tokio::task::JoinHandle;

const ATTESTATION_SCHEMA: &str = "rightkit.browser.fixture-attestation.v1";
const OUTCOME_SCHEMA: &str = "rightkit.browser.fixture-outcome.v1";
const ASSERT_POLL: Duration = Duration::from_millis(25);
static BINDING_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureCleanupEvidence {
    pub browser_stopped: bool,
    pub transport_stopped: bool,
    pub proxy_stopped: bool,
    pub server_stopped: bool,
    pub profile_removed: bool,
}

impl FixtureCleanupEvidence {
    fn pending() -> Self {
        Self {
            browser_stopped: false,
            transport_stopped: false,
            proxy_stopped: false,
            server_stopped: false,
            profile_removed: false,
        }
    }

    fn complete(&self) -> bool {
        self.browser_stopped
            && self.transport_stopped
            && self.proxy_stopped
            && self.server_stopped
            && self.profile_removed
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FixtureConfinementBackend {
    MacosSeatbeltProxy,
    WindowsAppcontainerPipeFulfillment,
}

impl FixtureConfinementBackend {
    fn policy_name(self) -> &'static str {
        match self {
            Self::MacosSeatbeltProxy => "macosSeatbeltProxy",
            Self::WindowsAppcontainerPipeFulfillment => "windowsAppcontainerPipeFulfillment",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureAttestation {
    pub schema: String,
    pub host_binding_id: String,
    pub server_endpoint: String,
    pub server_instance_id: String,
    pub fixture_id: String,
    pub fixture_source_digest: String,
    pub confinement_backend: FixtureConfinementBackend,
    pub browser_process_instance: String,
    pub browser_pid: u32,
    pub profile_identity: String,
    pub browser_session_id: String,
    pub page_id: String,
    pub frame_id: String,
    pub loader_id: String,
    pub unique_context_id: String,
    pub exact_url: String,
    pub probe_id: String,
    pub actions_digest: String,
    pub policy_digest: String,
    pub expires_at_unix_ms: u128,
    pub output_cleanup: FixtureCleanupEvidence,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FixtureOutcomeStatus {
    Passed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureOutcome {
    pub schema: String,
    pub status: FixtureOutcomeStatus,
    pub fixture_id: String,
    pub probe_id: String,
    pub completed_actions: usize,
    pub attestation: FixtureAttestation,
    pub cleanup_complete: bool,
}

/// Sticky, cloneable cancellation signal for a fixture run.
#[derive(Clone)]
pub struct FixtureCancellation {
    cancelled: tokio::sync::watch::Sender<bool>,
}

impl FixtureCancellation {
    pub fn new() -> Self {
        let (cancelled, _) = tokio::sync::watch::channel(false);
        Self { cancelled }
    }

    pub fn cancel(&self) {
        self.cancelled.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }

    async fn wait(&self) {
        let mut cancelled = self.cancelled.subscribe();
        loop {
            if *cancelled.borrow_and_update() {
                return;
            }
            if cancelled.changed().await.is_err() {
                return;
            }
        }
    }
}

impl Default for FixtureCancellation {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[error("fixture run failed: {cause}")]
pub struct FixtureRunFailure {
    pub cause: FixtureError,
    pub completed_actions: usize,
    pub attestation: Box<FixtureAttestation>,
    pub cleanup_complete: bool,
    pub cleanup_error: Option<FixtureError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FixtureDocument {
    frame_id: String,
    loader_id: String,
    unique_context_id: Option<String>,
}

#[derive(Default)]
struct MonitorData {
    document: Option<FixtureDocument>,
    revoked: Option<String>,
    expected_shutdown: bool,
}

#[derive(Default)]
struct FixtureMonitor {
    data: Mutex<MonitorData>,
    crashed: AtomicBool,
}

impl FixtureMonitor {
    fn bind_main_document(&self, frame_id: &str, loader_id: &str, url: &str, exact_url: &str) {
        let mut data = self.data.lock().unwrap_or_else(|error| error.into_inner());
        if data.revoked.is_some() {
            return;
        }
        if data.document.is_none() && url == exact_url {
            data.document = Some(FixtureDocument {
                frame_id: frame_id.into(),
                loader_id: loader_id.into(),
                unique_context_id: None,
            });
        } else if data.document.is_some() || url != "about:blank" {
            Self::revoke_locked(&mut data, "frame navigated after fixture binding");
        }
    }

    fn frame_navigated(&self, frame_id: &str, loader_id: &str, url: &str, exact_url: &str) {
        let mut data = self.data.lock().unwrap_or_else(|error| error.into_inner());
        if data.document.is_none() {
            drop(data);
            self.bind_main_document(frame_id, loader_id, url, exact_url);
            return;
        }
        Self::revoke_locked(&mut data, "document or frame navigation detected");
    }

    fn navigated_within_document(&self) {
        let mut data = self.data.lock().unwrap_or_else(|error| error.into_inner());
        if data.document.is_some() {
            Self::revoke_locked(&mut data, "same-document navigation detected");
        }
    }

    fn context_created(&self, frame_id: &str, unique_context_id: &str) {
        let mut data = self.data.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(document) = &mut data.document {
            if document.frame_id == frame_id {
                if document.unique_context_id.is_some() {
                    Self::revoke_locked(&mut data, "fixture execution context was replaced");
                } else {
                    document.unique_context_id = Some(unique_context_id.into());
                }
            }
        }
    }

    fn context_destroyed(&self, unique_context_id: &str) {
        let mut data = self.data.lock().unwrap_or_else(|error| error.into_inner());
        if data.expected_shutdown {
            return;
        }
        if data
            .document
            .as_ref()
            .and_then(|document| document.unique_context_id.as_deref())
            == Some(unique_context_id)
        {
            Self::revoke_locked(&mut data, "fixture execution context was destroyed");
        }
    }

    fn contexts_cleared(&self) {
        let mut data = self.data.lock().unwrap_or_else(|error| error.into_inner());
        if data.expected_shutdown {
            return;
        }
        if data
            .document
            .as_ref()
            .is_some_and(|document| document.unique_context_id.is_some())
        {
            Self::revoke_locked(&mut data, "execution contexts were cleared");
        }
    }

    fn crashed(&self) {
        self.crashed.store(true, Ordering::SeqCst);
        self.revoke("browser target crashed");
    }

    fn revoke(&self, reason: &str) {
        let mut data = self.data.lock().unwrap_or_else(|error| error.into_inner());
        Self::revoke_locked(&mut data, reason);
    }

    fn revoke_locked(data: &mut MonitorData, reason: &str) {
        if data.revoked.is_none() {
            data.revoked = Some(reason.into());
        }
    }

    fn expected_shutdown(&self) -> bool {
        self.data
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .expected_shutdown
    }

    fn begin_expected_shutdown(&self) {
        self.data
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .expected_shutdown = true;
    }

    fn snapshot(&self) -> std::result::Result<FixtureDocument, FixtureError> {
        if self.crashed.load(Ordering::SeqCst) {
            return Err(FixtureError::Revoked("browser target crashed".into()));
        }
        let data = self.data.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(reason) = &data.revoked {
            return Err(FixtureError::Revoked(reason.clone()));
        }
        let document = data.document.clone().ok_or_else(|| {
            FixtureError::Revoked("fixture document identity is not bound".into())
        })?;
        if document.unique_context_id.is_none() {
            return Err(FixtureError::Revoked(
                "fixture isolated execution context is not bound".into(),
            ));
        }
        Ok(document)
    }

    fn bound_frame(&self) -> std::result::Result<FixtureDocument, FixtureError> {
        if self.crashed.load(Ordering::SeqCst) {
            return Err(FixtureError::Revoked("browser target crashed".into()));
        }
        let data = self.data.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(reason) = &data.revoked {
            return Err(FixtureError::Revoked(reason.clone()));
        }
        data.document
            .clone()
            .ok_or_else(|| FixtureError::Revoked("fixture frame identity is not bound".into()))
    }
}

/// Opaque execution authority for one immutable source bundle and one closed action list.
///
/// Test/QA only: not for shipped app sessions. Its Chrome runs with `--no-sandbox` because the
/// outer confinement (Seatbelt on macOS, AppContainer on Windows) confines the whole tree;
/// ordinary `BrowserSession::launch` never does.
pub struct FixtureSession {
    server: Arc<tokio::sync::Mutex<Option<FixtureServer>>>,
    browser: Arc<BrowserSession>,
    cleanup_lock: Arc<tokio::sync::Mutex<()>>,
    cleanup_evidence: Arc<Mutex<FixtureCleanupEvidence>>,
    page: Arc<BrowserPage>,
    intent: FixtureIntent,
    monitor: Arc<FixtureMonitor>,
    event_tasks: EventTasks,
    deadline: Instant,
    attestation: FixtureAttestation,
    timer_cancel: Option<tokio::sync::oneshot::Sender<()>>,
    timer: Option<tokio::task::JoinHandle<()>>,
    #[cfg(windows)]
    fixture_fetch: Option<JoinHandle<()>>,
}

#[derive(Clone)]
struct EventBarrier(Vec<tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>>);

struct EventTasks {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    barrier: EventBarrier,
}

impl EventBarrier {
    async fn drain(&self) -> std::result::Result<(), FixtureError> {
        let mut acknowledgements = Vec::with_capacity(self.0.len());
        for sender in &self.0 {
            let (acknowledge, acknowledged) = tokio::sync::oneshot::channel();
            sender.send(acknowledge).map_err(|_| {
                FixtureError::Revoked(
                    "fixture event monitor stopped before identity barrier".into(),
                )
            })?;
            acknowledgements.push(acknowledged);
        }
        for acknowledged in acknowledgements {
            acknowledged.await.map_err(|_| {
                FixtureError::Revoked("fixture event monitor failed identity barrier".into())
            })?;
        }
        Ok(())
    }
}

impl Drop for EventTasks {
    fn drop(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }
}

fn monitor_events<T, S, F>(
    mut events: S,
    monitor: Arc<FixtureMonitor>,
    mut handle: F,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
)
where
    T: chromiumoxide::cdp::IntoEventKind + Unpin + Send + 'static,
    S: Stream<Item = Arc<T>> + Unpin + Send + 'static,
    F: FnMut(Arc<T>, &FixtureMonitor) + Send + 'static,
{
    let (barrier_sender, mut barriers) =
        tokio::sync::mpsc::unbounded_channel::<tokio::sync::oneshot::Sender<()>>();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                event = events.next() => match event {
                    Some(event) => handle(event, &monitor),
                    None => {
                        if !monitor.expected_shutdown() {
                            monitor.revoke("fixture event stream ended");
                        }
                        return;
                    }
                },
                barrier = barriers.recv() => match barrier {
                    Some(acknowledge) => {
                        loop {
                            match events.next().now_or_never() {
                                Some(Some(event)) => handle(event, &monitor),
                                Some(None) => {
                                    if !monitor.expected_shutdown() {
                                        monitor.revoke("fixture event stream ended during identity barrier");
                                    }
                                    return;
                                }
                                None => break,
                            }
                        }
                        let _ = acknowledge.send(());
                    }
                    None => return,
                }
            }
        }
    });
    (task, barrier_sender)
}

async fn cleanup_resources(
    browser: &BrowserSession,
    server: &tokio::sync::Mutex<Option<FixtureServer>>,
    lock: &tokio::sync::Mutex<()>,
    evidence: &Mutex<FixtureCleanupEvidence>,
) -> std::result::Result<(), FixtureError> {
    let _guard = lock.lock().await;
    let mut failures = Vec::new();
    let mut state = evidence
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    if !state.browser_stopped {
        match browser.stop_fixture_process().await {
            Ok(()) => {
                state.browser_stopped = true;
                // Windows only reports browser stop after Job-empty plus bridge
                // quiesce/finalize succeeds; macOS has no separate pipe transport.
                state.transport_stopped = true;
                *evidence.lock().unwrap_or_else(|error| error.into_inner()) = state.clone();
            }
            Err(error) => failures.push(error.to_string()),
        }
    }
    if !state.proxy_stopped {
        match browser.stop_fixture_proxy().await {
            Ok(()) => {
                state.proxy_stopped = true;
                *evidence.lock().unwrap_or_else(|error| error.into_inner()) = state.clone();
            }
            Err(error) => failures.push(error.to_string()),
        }
    }
    if !state.server_stopped {
        let mut server = server.lock().await;
        match server.as_mut() {
            Some(server) => match server.shutdown().await {
                Ok(()) => state.server_stopped = true,
                Err(error) => failures.push(error.to_string()),
            },
            None => state.server_stopped = true,
        }
        if state.server_stopped {
            server.take();
            *evidence.lock().unwrap_or_else(|error| error.into_inner()) = state.clone();
        }
    }
    if !state.profile_removed
        && state.browser_stopped
        && state.proxy_stopped
        && state.server_stopped
    {
        match browser.remove_fixture_profile() {
            Ok(()) => {
                state.profile_removed = true;
                *evidence.lock().unwrap_or_else(|error| error.into_inner()) = state.clone();
            }
            Err(error) => failures.push(error.to_string()),
        }
    }
    let complete = state.complete();
    if !failures.is_empty() {
        Err(FixtureError::Cleanup(failures.join("; ")))
    } else if complete {
        Ok(())
    } else {
        Err(FixtureError::Cleanup(
            "fixture cleanup is incomplete".into(),
        ))
    }
}

async fn cleanup_launch_failure(
    browser: Option<BrowserSession>,
    server: &tokio::sync::Mutex<Option<FixtureServer>>,
    lock: &tokio::sync::Mutex<()>,
    evidence: &Mutex<FixtureCleanupEvidence>,
) -> std::result::Result<(), FixtureError> {
    if let Some(browser) = browser {
        cleanup_resources(&Arc::new(browser), server, lock, evidence).await
    } else {
        let _guard = lock.lock().await;
        let mut server = server.lock().await;
        if let Some(server) = server.as_mut() {
            server.shutdown().await?;
        }
        server.take();
        Ok(())
    }
}

impl FixtureSession {
    pub async fn launch(
        server: FixtureServer,
        intent: FixtureIntent,
    ) -> std::result::Result<Self, FixtureError> {
        if !server.is_alive() {
            return Err(FixtureError::FixtureServerNotOwned(
                "fixture server accept task is not alive".into(),
            ));
        }
        intent.validate(server.bundle())?;
        let path = crate::fixture_protocol::parse_fixture_url(&intent.exact_url)?
            .path()
            .to_string();
        if server.url(&path)? != intent.exact_url {
            return Err(FixtureError::FixtureServerNotOwned(
                "exact_url is not served by this owned fixture instance".into(),
            ));
        }

        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = (server, intent);
            return Err(FixtureError::ConfinementUnsupported(
                "source-only browser fixtures require macOS Seatbelt confinement".into(),
            ));
        }

        #[cfg(target_os = "macos")]
        {
            let now = Instant::now();
            let deadline = now + Duration::from_millis(intent.expires_after_ms);
            let expires_at_unix_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .saturating_add(u128::from(intent.expires_after_ms));
            let server = Arc::new(tokio::sync::Mutex::new(Some(server)));
            let setup_server = server.clone();
            let intent_for_setup = intent.clone();
            let cleanup_lock = Arc::new(tokio::sync::Mutex::new(()));
            let cleanup_evidence = Arc::new(Mutex::new(FixtureCleanupEvidence::pending()));
            let mut browser_slot: Option<BrowserSession> = None;
            let setup = async {
                let server_ref = setup_server.lock().await;
                let server = server_ref.as_ref().ok_or_else(|| {
                    FixtureError::FixtureServerNotOwned("server is closed".into())
                })?;
                let intent = intent_for_setup.clone();
                let binding_id = binding_id();
                let policy_digest = policy_digest(
                    &intent.exact_url,
                    &intent.fixture_source_digest,
                    server.instance_id(),
                    FixtureConfinementBackend::MacosSeatbeltProxy,
                );
                let proxy_policy = FixtureProxyPolicy {
                    server_endpoint: server.addr(),
                    permitted_urls: server.permitted_urls(),
                    expires_at: deadline,
                };
                browser_slot = Some(BrowserSession::launch_fixture(proxy_policy).await?);
                let browser = browser_slot.as_ref().ok_or_else(|| {
                    FixtureError::Browser("owned browser was lost during fixture launch".into())
                })?;
                browser.configure_fixture_downloads().await?;
                let profile_identity = browser.profile_dir().to_string_lossy().into_owned();
                let browser_pid = browser.process_id().ok_or_else(|| {
                    FixtureError::Browser("owned browser process has no pid".into())
                })?;
                let browser_process_instance =
                    format!("{}:{}:{}", browser.id(), browser_pid, binding_id);
                let page = browser
                    .new_page("about:blank")
                    .await
                    .map_err(|error| FixtureError::Browser(error.to_string()))?;
                let world_name = format!("rightkit-fixture-{binding_id}");
                let (monitor, event_tasks) =
                    install_monitor(&page, &world_name, &intent.exact_url).await?;
                page.settle_initial_document().await;
                setup_event_fence(&page, &event_tasks).await?;
                check_deadline(deadline)?;
                page.goto(&intent.exact_url)
                    .await
                    .map_err(|error| FixtureError::Browser(error.to_string()))?;
                let frame_id = wait_for_bound_frame(&monitor, deadline).await?;
                let navigated_frame = setup_event_fence(&page, &event_tasks).await?;
                let bound_frame = monitor.bound_frame()?;
                if navigated_frame.id.as_ref() != bound_frame.frame_id
                    || navigated_frame.loader_id.as_ref() != bound_frame.loader_id.as_str()
                    || navigated_frame.url != intent.exact_url
                    || frame_id != bound_frame.frame_id
                {
                    monitor.revoke("fixture frame changed during setup event fence");
                    return Err(FixtureError::Revoked(
                        "fixture frame changed during setup event fence".into(),
                    ));
                }
                let mut isolated = CreateIsolatedWorldParams::new(frame_id.clone());
                isolated.world_name = Some(world_name.clone());
                isolated.grant_univeral_access = Some(false);
                page.raw()
                    .execute(isolated)
                    .await
                    .map_err(|error| FixtureError::Browser(error.to_string()))?;
                let document = wait_for_context(&monitor, deadline).await?;
                verify_document(&page, &monitor, &document, &intent.exact_url).await?;
                let attestation = FixtureAttestation {
                    schema: ATTESTATION_SCHEMA.into(),
                    host_binding_id: binding_id,
                    server_endpoint: server.addr().to_string(),
                    server_instance_id: server.instance_id().into(),
                    fixture_id: intent.fixture_id.clone(),
                    fixture_source_digest: intent.fixture_source_digest.clone(),
                    confinement_backend: FixtureConfinementBackend::MacosSeatbeltProxy,
                    browser_process_instance,
                    browser_pid,
                    profile_identity,
                    browser_session_id: browser.id().into(),
                    page_id: page.id().into(),
                    frame_id: document.frame_id,
                    loader_id: document.loader_id,
                    unique_context_id: document.unique_context_id.unwrap_or_default(),
                    exact_url: intent.exact_url.clone(),
                    probe_id: intent.probe_id.clone(),
                    actions_digest: intent.actions_digest.clone(),
                    policy_digest,
                    expires_at_unix_ms,
                    output_cleanup: FixtureCleanupEvidence::pending(),
                };
                drop(server_ref);
                let browser = browser_slot.take().ok_or_else(|| {
                    FixtureError::Browser("owned browser was lost during fixture binding".into())
                })?;
                Ok(Self {
                    server: setup_server.clone(),
                    browser: Arc::new(browser),
                    cleanup_lock: cleanup_lock.clone(),
                    cleanup_evidence: cleanup_evidence.clone(),
                    page,
                    intent,
                    monitor,
                    event_tasks,
                    deadline,
                    attestation,
                    timer_cancel: None,
                    timer: None,
                    #[cfg(windows)]
                    fixture_fetch: None,
                })
            };
            let setup_result =
                tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), setup).await;
            let mut session = match setup_result {
                Ok(Ok(session)) => session,
                Ok(Err(error)) => {
                    let cleanup = cleanup_launch_failure(
                        browser_slot.take(),
                        &server,
                        &cleanup_lock,
                        &cleanup_evidence,
                    )
                    .await;
                    return Err(match cleanup {
                        Ok(()) => error,
                        Err(cleanup_error) => {
                            FixtureError::Cleanup(format!("{error}; {cleanup_error}"))
                        }
                    });
                }
                Err(_) => {
                    let error =
                        FixtureError::Revoked("fixture expired while launching browser".into());
                    let cleanup = cleanup_launch_failure(
                        browser_slot.take(),
                        &server,
                        &cleanup_lock,
                        &cleanup_evidence,
                    )
                    .await;
                    return Err(match cleanup {
                        Ok(()) => error,
                        Err(cleanup_error) => {
                            FixtureError::Cleanup(format!("{error}; {cleanup_error}"))
                        }
                    });
                }
            };
            let (cancel, mut cancelled) = tokio::sync::oneshot::channel();
            let browser = session.browser.clone();
            let server = session.server.clone();
            let cleanup_lock = session.cleanup_lock.clone();
            let cleanup_evidence = session.cleanup_evidence.clone();
            let monitor = session.monitor.clone();
            let deadline = session.deadline;
            session.timer = Some(tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                        monitor.revoke("fixture lease expired");
                    }
                    _ = &mut cancelled => {}
                }
                let _ =
                    cleanup_resources(&browser, &server, &cleanup_lock, &cleanup_evidence).await;
            }));
            session.timer_cancel = Some(cancel);
            Ok(session)
        }
        #[cfg(windows)]
        {
            let now = Instant::now();
            let deadline = now + Duration::from_millis(intent.expires_after_ms);
            let expires_at_unix_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .saturating_add(u128::from(intent.expires_after_ms));
            let server = Arc::new(tokio::sync::Mutex::new(Some(server)));
            // Snapshot immutable metadata before Chrome launch. Holding this lock
            // through navigation deadlocks Fetch fulfillment's sole request pump.
            let (server_endpoint, server_instance_id) = {
                let server_ref = server.lock().await;
                let server = server_ref.as_ref().ok_or_else(|| {
                    FixtureError::FixtureServerNotOwned("server is closed".into())
                })?;
                (server.addr(), server.instance_id().to_owned())
            };
            let cleanup_lock = Arc::new(tokio::sync::Mutex::new(()));
            let cleanup_evidence = Arc::new(Mutex::new(FixtureCleanupEvidence::pending()));
            let mut browser_slot: Option<BrowserSession> = None;
            let mut fetch_slot: Option<JoinHandle<()>> = None;
            let setup = async {
                let binding_id = binding_id();
                let policy_digest = policy_digest(
                    &intent.exact_url,
                    &intent.fixture_source_digest,
                    &server_instance_id,
                    FixtureConfinementBackend::WindowsAppcontainerPipeFulfillment,
                );
                browser_slot = Some(
                    BrowserSession::launch_fixture(FixtureProxyPolicy {
                        server_endpoint,
                        permitted_urls: Arc::default(),
                        expires_at: deadline,
                    })
                    .await?,
                );
                let browser = browser_slot.as_ref().ok_or_else(|| {
                    FixtureError::Browser("owned browser was lost during fixture launch".into())
                })?;
                browser.configure_fixture_downloads().await?;
                let profile_identity = browser.profile_dir().to_string_lossy().into_owned();
                let browser_pid = browser.process_id().ok_or_else(|| {
                    FixtureError::Browser("owned browser process has no pid".into())
                })?;
                let browser_process_instance =
                    format!("{}:{}:{}", browser.id(), browser_pid, binding_id);
                let page = browser
                    .new_page("about:blank")
                    .await
                    .map_err(|error| FixtureError::Browser(error.to_string()))?;
                let world_name = format!("rightkit-fixture-{binding_id}");
                let (monitor, event_tasks) =
                    install_monitor(&page, &world_name, &intent.exact_url).await?;
                fetch_slot = Some(
                    install_windows_fixture_fetch(&page, server.clone(), monitor.clone(), deadline)
                        .await?,
                );
                page.settle_initial_document().await;
                setup_event_fence(&page, &event_tasks).await?;
                check_deadline(deadline)?;
                page.goto(&intent.exact_url)
                    .await
                    .map_err(|error| FixtureError::Browser(error.to_string()))?;
                let frame_id = wait_for_bound_frame(&monitor, deadline).await?;
                let navigated_frame = setup_event_fence(&page, &event_tasks).await?;
                let bound_frame = monitor.bound_frame()?;
                if navigated_frame.id.as_ref() != bound_frame.frame_id
                    || navigated_frame.loader_id.as_ref() != bound_frame.loader_id.as_str()
                    || navigated_frame.url != intent.exact_url
                    || frame_id != bound_frame.frame_id
                {
                    monitor.revoke("fixture frame changed during setup event fence");
                    return Err(FixtureError::Revoked(
                        "fixture frame changed during setup event fence".into(),
                    ));
                }
                let mut isolated = CreateIsolatedWorldParams::new(frame_id.clone());
                isolated.world_name = Some(world_name.clone());
                isolated.grant_univeral_access = Some(false);
                page.raw()
                    .execute(isolated)
                    .await
                    .map_err(|error| FixtureError::Browser(error.to_string()))?;
                let document = wait_for_context(&monitor, deadline).await?;
                verify_document(&page, &monitor, &document, &intent.exact_url).await?;
                let attestation = FixtureAttestation {
                    schema: ATTESTATION_SCHEMA.into(),
                    host_binding_id: binding_id,
                    server_endpoint: server_endpoint.to_string(),
                    server_instance_id,
                    fixture_id: intent.fixture_id.clone(),
                    fixture_source_digest: intent.fixture_source_digest.clone(),
                    confinement_backend:
                        FixtureConfinementBackend::WindowsAppcontainerPipeFulfillment,
                    browser_process_instance,
                    browser_pid,
                    profile_identity,
                    browser_session_id: browser.id().into(),
                    page_id: page.id().into(),
                    frame_id: document.frame_id,
                    loader_id: document.loader_id,
                    unique_context_id: document.unique_context_id.unwrap_or_default(),
                    exact_url: intent.exact_url.clone(),
                    probe_id: intent.probe_id.clone(),
                    actions_digest: intent.actions_digest.clone(),
                    policy_digest,
                    expires_at_unix_ms,
                    output_cleanup: FixtureCleanupEvidence::pending(),
                };
                let browser = browser_slot.take().ok_or_else(|| {
                    FixtureError::Browser("owned browser was lost during fixture binding".into())
                })?;
                let fetch = fetch_slot.take().ok_or_else(|| {
                    FixtureError::Browser("Windows fixture Fetch ownership was lost".into())
                })?;
                Ok(Self {
                    server: server.clone(),
                    browser: Arc::new(browser),
                    cleanup_lock: cleanup_lock.clone(),
                    cleanup_evidence: cleanup_evidence.clone(),
                    page,
                    intent,
                    monitor,
                    event_tasks,
                    deadline,
                    attestation,
                    timer_cancel: None,
                    timer: None,
                    fixture_fetch: Some(fetch),
                })
            };
            let setup_result =
                tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), setup).await;
            let mut session = match setup_result {
                Ok(Ok(session)) => session,
                Ok(Err(error)) => {
                    if let Some(fetch) = fetch_slot.take() {
                        fetch.abort();
                        let _ = fetch.await;
                    }
                    let cleanup = cleanup_launch_failure(
                        browser_slot.take(),
                        &server,
                        &cleanup_lock,
                        &cleanup_evidence,
                    )
                    .await;
                    return Err(match cleanup {
                        Ok(()) => error,
                        Err(cleanup_error) => {
                            FixtureError::Cleanup(format!("{error}; {cleanup_error}"))
                        }
                    });
                }
                Err(_) => {
                    if let Some(fetch) = fetch_slot.take() {
                        fetch.abort();
                        let _ = fetch.await;
                    }
                    let error =
                        FixtureError::Revoked("fixture expired while launching browser".into());
                    let cleanup = cleanup_launch_failure(
                        browser_slot.take(),
                        &server,
                        &cleanup_lock,
                        &cleanup_evidence,
                    )
                    .await;
                    return Err(match cleanup {
                        Ok(()) => error,
                        Err(cleanup_error) => {
                            FixtureError::Cleanup(format!("{error}; {cleanup_error}"))
                        }
                    });
                }
            };
            let (cancel, mut cancelled) = tokio::sync::oneshot::channel();
            let browser = session.browser.clone();
            let server = session.server.clone();
            let cleanup_lock = session.cleanup_lock.clone();
            let cleanup_evidence = session.cleanup_evidence.clone();
            let monitor = session.monitor.clone();
            session.timer = Some(tokio::spawn(async move {
                tokio::select! { _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => monitor.revoke("fixture lease expired"), _ = &mut cancelled => {} }
                let _ =
                    cleanup_resources(&browser, &server, &cleanup_lock, &cleanup_evidence).await;
            }));
            session.timer_cancel = Some(cancel);
            Ok(session)
        }
    }

    pub fn attestation(&self) -> &FixtureAttestation {
        &self.attestation
    }

    pub async fn run(self) -> std::result::Result<FixtureOutcome, FixtureError> {
        self.run_with_cancel(FixtureCancellation::default())
            .await
            .map_err(|failure| match failure.cleanup_error {
                Some(cleanup_error) => {
                    FixtureError::Cleanup(format!("{}; {}", failure.cause, cleanup_error))
                }
                None => failure.cause,
            })
    }

    pub async fn run_with_cancel(
        mut self,
        cancellation: FixtureCancellation,
    ) -> std::result::Result<FixtureOutcome, FixtureRunFailure> {
        let completed_actions = AtomicUsize::new(0);
        let actions = tokio::select! {
            biased;
            _ = cancellation.wait() => Err(FixtureError::Cancelled("fixture execution cancelled".into())),
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(self.deadline)) => {
                Err(FixtureError::Expired("fixture execution deadline expired".into()))
            }
            result = self.run_actions(&completed_actions) => result,
        };
        if let Some(cancel) = self.timer_cancel.take() {
            let _ = cancel.send(());
        }
        let cleanup = self.cleanup().await;
        if let Some(timer) = self.timer.take() {
            let _ = timer.await;
        }
        self.attestation.output_cleanup = self
            .cleanup_evidence
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let completed_actions = completed_actions.load(Ordering::SeqCst);
        let cleanup_error = cleanup.err();
        let final_check = if actions.is_ok() && cleanup_error.is_none() {
            if cancellation.is_cancelled() {
                Err(FixtureError::Cancelled(
                    "fixture execution cancelled before completion".into(),
                ))
            } else if Instant::now() >= self.deadline {
                Err(FixtureError::Expired(
                    "fixture execution deadline expired before completion".into(),
                ))
            } else {
                self.verify_terminal_identity().await
            }
        } else {
            Ok(())
        };
        let cause = match (actions, cleanup_error.as_ref(), final_check) {
            (Err(cause), _, _) => Some(cause),
            (Ok(_), Some(cleanup_error), _) => Some(cleanup_error.clone()),
            (Ok(_), None, Err(cause)) => Some(cause),
            (Ok(_), None, Ok(())) if !self.attestation.output_cleanup.complete() => {
                Some(FixtureError::Cleanup(
                    "cleanup evidence is incomplete; passed outcome withheld".into(),
                ))
            }
            (Ok(_), None, Ok(())) => None,
        };
        if let Some(cause) = cause {
            return Err(FixtureRunFailure {
                cause,
                completed_actions,
                cleanup_complete: self.attestation.output_cleanup.complete(),
                attestation: Box::new(self.attestation.clone()),
                cleanup_error,
            });
        }

        Ok(FixtureOutcome {
            schema: OUTCOME_SCHEMA.into(),
            status: FixtureOutcomeStatus::Passed,
            fixture_id: self.intent.fixture_id.clone(),
            probe_id: self.intent.probe_id.clone(),
            completed_actions,
            attestation: self.attestation.clone(),
            cleanup_complete: true,
        })
    }

    async fn run_actions(
        &self,
        completed_actions: &AtomicUsize,
    ) -> std::result::Result<(), FixtureError> {
        for (index, action) in self.intent.actions.iter().enumerate() {
            self.verify().await?;
            match action {
                FixtureAction::Click { selector } => self
                    .page
                    .click(&Target::Selector(selector.clone()))
                    .await
                    .map_err(|error| FixtureError::Browser(error.to_string()))?,
                FixtureAction::Type { text } => self
                    .page
                    .type_text(text)
                    .await
                    .map_err(|error| FixtureError::Browser(error.to_string()))?,
                FixtureAction::Press { key } => self
                    .page
                    .press(key_name(*key))
                    .await
                    .map_err(|error| FixtureError::Browser(error.to_string()))?,
                FixtureAction::AssertVisible { selector, timeout } => {
                    let timeout = Duration::from_millis(timeout.unwrap_or(1000));
                    let end = Instant::now() + timeout;
                    loop {
                        self.verify().await?;
                        if self
                            .assertion("function(selector) { const e=document.querySelector(selector); if(!e) return false; const s=getComputedStyle(e); const r=e.getBoundingClientRect(); return s.visibility !== 'hidden' && s.display !== 'none' && Number(s.opacity) > 0 && r.width > 0 && r.height > 0; }", &[serde_json::Value::String(selector.clone())])
                            .await?
                        {
                            break;
                        }
                        if Instant::now() >= end {
                            return Err(FixtureError::Browser(format!(
                                "action {index}: selector did not become visible"
                            )));
                        }
                        tokio::time::sleep(ASSERT_POLL).await;
                    }
                }
                FixtureAction::AssertText { selector, text } => {
                    self.verify().await?;
                    let matched = self
                        .assertion(
                            "function(selector, expected) { const e=document.querySelector(selector); return !!e && e.innerText === expected; }",
                            &[
                                serde_json::Value::String(selector.clone()),
                                serde_json::Value::String(text.clone()),
                            ],
                        )
                        .await?;
                    if !matched {
                        return Err(FixtureError::Browser(format!(
                            "action {index}: selector text did not match"
                        )));
                    }
                }
            }
            self.verify().await?;
            completed_actions.fetch_add(1, Ordering::SeqCst);
        }
        self.terminal_barrier().await?;
        Ok(())
    }

    async fn assertion(
        &self,
        function: &str,
        args: &[serde_json::Value],
    ) -> std::result::Result<bool, FixtureError> {
        let document = self.monitor.snapshot()?;
        let unique_context_id = document
            .unique_context_id
            .ok_or_else(|| FixtureError::Revoked("unique context id is absent".into()))?;
        let params = CallFunctionOnParams {
            function_declaration: function.into(),
            object_id: None,
            arguments: Some(
                args.iter()
                    .cloned()
                    .map(|value| CallArgument {
                        value: Some(value),
                        unserializable_value: None,
                        object_id: None,
                    })
                    .collect(),
            ),
            silent: Some(false),
            return_by_value: Some(true),
            generate_preview: Some(false),
            user_gesture: Some(false),
            await_promise: Some(false),
            execution_context_id: None,
            object_group: None,
            throw_on_side_effect: None,
            unique_context_id: Some(unique_context_id),
            serialization_options: None,
        };
        let response = self
            .page
            .raw()
            .execute(params)
            .await
            .map_err(|error| FixtureError::Browser(error.to_string()))?;
        if response.result.exception_details.is_some() {
            return Err(FixtureError::Browser(
                "fixture assertion execution failed".into(),
            ));
        }
        response
            .result
            .result
            .value
            .as_ref()
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| FixtureError::Browser("fixture assertion returned no boolean".into()))
    }

    async fn verify(&self) -> std::result::Result<(), FixtureError> {
        check_deadline(self.deadline)?;
        let server_guard = self.server.lock().await;
        let server = server_guard
            .as_ref()
            .ok_or_else(|| FixtureError::FixtureServerNotOwned("server is closed".into()))?;
        if !server.is_alive() {
            return Err(FixtureError::FixtureServerNotOwned(
                "owned fixture server stopped during execution".into(),
            ));
        }
        if !self.browser.process_alive() {
            return Err(FixtureError::Revoked(
                "owned browser process exited during execution".into(),
            ));
        }
        let document = self.monitor.snapshot()?;
        verify_document(&self.page, &self.monitor, &document, &self.intent.exact_url).await
    }

    async fn terminal_barrier(&self) -> std::result::Result<(), FixtureError> {
        self.verify().await?;
        let page = self.page.raw();
        page.execute(SetScriptExecutionDisabledParams::new(true))
            .await
            .map_err(|error| {
                FixtureError::Revoked(format!("cannot freeze fixture script execution: {error}"))
            })?;
        page.execute(StopLoadingParams::default())
            .await
            .map_err(|error| {
                FixtureError::Revoked(format!("cannot stop fixture loading: {error}"))
            })?;
        page.execute(SetWebLifecycleStateParams::new(
            SetWebLifecycleStateState::Frozen,
        ))
        .await
        .map_err(|error| FixtureError::Revoked(format!("cannot freeze fixture page: {error}")))?;
        let mut terminal_frame = None;
        for _ in 0..2 {
            terminal_frame = Some(
                page.execute(GetFrameTreeParams::default())
                    .await
                    .map_err(|error| {
                        FixtureError::Revoked(format!("cannot fence fixture events: {error}"))
                    })?
                    .result
                    .frame_tree
                    .frame,
            );
        }
        let frame = terminal_frame.ok_or_else(|| {
            FixtureError::Revoked("terminal frame identity was not returned".into())
        })?;
        if frame.id.as_ref() != self.attestation.frame_id
            || frame.loader_id.as_ref() != self.attestation.loader_id.as_str()
            || frame.url != self.intent.exact_url
        {
            self.monitor
                .revoke("fixture frame changed at terminal barrier");
            return Err(FixtureError::Revoked(
                "fixture frame changed at terminal barrier".into(),
            ));
        }
        self.event_tasks.barrier.drain().await?;
        self.verify_terminal_identity().await?;
        let _cleanup_guard = self.cleanup_lock.lock().await;
        self.monitor.begin_expected_shutdown();
        self.browser.stop_fixture_process().await?;
        {
            let mut cleanup = self
                .cleanup_evidence
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            cleanup.browser_stopped = true;
            // `stop_fixture_process` has already received its platform transport
            // stop receipt: macOS has no separate bridge, Windows has Job-empty plus
            // bridge finalization.
            cleanup.transport_stopped = true;
        }
        self.verify_terminal_identity().await
    }

    async fn verify_terminal_identity(&self) -> std::result::Result<(), FixtureError> {
        check_deadline(self.deadline)?;
        let document = self.monitor.snapshot()?;
        if document.frame_id != self.attestation.frame_id
            || document.loader_id != self.attestation.loader_id
            || document.unique_context_id.as_deref()
                != Some(self.attestation.unique_context_id.as_str())
        {
            self.monitor
                .revoke("fixture identity changed at terminal barrier");
            return Err(FixtureError::Revoked(
                "fixture identity changed at terminal barrier".into(),
            ));
        }
        Ok(())
    }

    async fn cleanup(&mut self) -> std::result::Result<(), FixtureError> {
        #[cfg(windows)]
        if let Some(fetch) = self.fixture_fetch.take() {
            fetch.abort();
            let _ = fetch.await;
        }
        cleanup_resources(
            &self.browser,
            &self.server,
            &self.cleanup_lock,
            &self.cleanup_evidence,
        )
        .await
    }
}

impl Drop for FixtureSession {
    fn drop(&mut self) {
        if let Some(cancel) = self.timer_cancel.take() {
            let _ = cancel.send(());
        }
        #[cfg(windows)]
        if let Some(fetch) = self.fixture_fetch.take() {
            fetch.abort();
        }
    }
}

async fn install_monitor(
    page: &BrowserPage,
    world_name: &str,
    exact_url: &str,
) -> std::result::Result<(Arc<FixtureMonitor>, EventTasks), FixtureError> {
    let raw = page.raw();
    let frames = raw
        .event_listener::<EventFrameNavigated>()
        .await
        .map_err(|error| FixtureError::Browser(error.to_string()))?;
    let documents_opened = raw
        .event_listener::<EventDocumentOpened>()
        .await
        .map_err(|error| FixtureError::Browser(error.to_string()))?;
    let within_document = raw
        .event_listener::<EventNavigatedWithinDocument>()
        .await
        .map_err(|error| FixtureError::Browser(error.to_string()))?;
    let created = raw
        .event_listener::<EventExecutionContextCreated>()
        .await
        .map_err(|error| FixtureError::Browser(error.to_string()))?;
    let destroyed = raw
        .event_listener::<EventExecutionContextDestroyed>()
        .await
        .map_err(|error| FixtureError::Browser(error.to_string()))?;
    let cleared = raw
        .event_listener::<EventExecutionContextsCleared>()
        .await
        .map_err(|error| FixtureError::Browser(error.to_string()))?;
    let crashed = raw
        .event_listener::<EventTargetCrashed>()
        .await
        .map_err(|error| FixtureError::Browser(error.to_string()))?;
    raw.execute(PageEnableParams::default())
        .await
        .map_err(|error| FixtureError::Browser(error.to_string()))?;
    raw.execute(RuntimeEnableParams::default())
        .await
        .map_err(|error| FixtureError::Browser(error.to_string()))?;

    let monitor = Arc::new(FixtureMonitor::default());
    let mut tasks = Vec::new();
    let mut barriers = Vec::new();
    let exact_url = exact_url.to_owned();
    let (task, barrier) = monitor_events(frames, monitor.clone(), move |event, monitor| {
        let frame = &event.frame;
        if frame.parent_id.is_none() {
            monitor.frame_navigated(
                frame.id.as_ref(),
                frame.loader_id.as_ref(),
                &frame.url,
                &exact_url,
            );
        } else {
            monitor.revoke("unexpected child frame navigation detected");
        }
    });
    tasks.push(task);
    barriers.push(barrier);
    let (task, barrier) = monitor_events(documents_opened, monitor.clone(), |event, monitor| {
        if event.frame.parent_id.is_none() {
            monitor.revoke("document.open/write replacement detected");
        } else {
            monitor.revoke("child frame document.open/write detected");
        }
    });
    tasks.push(task);
    barriers.push(barrier);
    let (task, barrier) = monitor_events(within_document, monitor.clone(), |_, monitor| {
        monitor.navigated_within_document();
    });
    tasks.push(task);
    barriers.push(barrier);
    let world_name = world_name.to_owned();
    let (task, barrier) = monitor_events(created, monitor.clone(), move |event, monitor| {
        let context = &event.context;
        if context.name != world_name {
            return;
        }
        let frame_id = context
            .aux_data
            .as_ref()
            .and_then(|value| value.get("frameId"))
            .and_then(serde_json::Value::as_str);
        if let Some(frame_id) = frame_id {
            monitor.context_created(frame_id, &context.unique_id);
        } else {
            monitor.revoke("isolated execution context omitted its frame identity");
        }
    });
    tasks.push(task);
    barriers.push(barrier);
    let (task, barrier) = monitor_events(destroyed, monitor.clone(), |event, monitor| {
        monitor.context_destroyed(&event.execution_context_unique_id);
    });
    tasks.push(task);
    barriers.push(barrier);
    let (task, barrier) = monitor_events(cleared, monitor.clone(), |_, monitor| {
        monitor.contexts_cleared();
    });
    tasks.push(task);
    barriers.push(barrier);
    let (task, barrier) = monitor_events(crashed, monitor.clone(), |_, monitor| {
        monitor.crashed();
    });
    tasks.push(task);
    barriers.push(barrier);
    Ok((
        monitor,
        EventTasks {
            tasks,
            barrier: EventBarrier(barriers),
        },
    ))
}

async fn setup_event_fence(
    page: &BrowserPage,
    event_tasks: &EventTasks,
) -> std::result::Result<chromiumoxide::cdp::browser_protocol::page::Frame, FixtureError> {
    let mut frame = None;
    for _ in 0..2 {
        frame = Some(
            page.raw()
                .execute(GetFrameTreeParams::default())
                .await
                .map_err(|error| {
                    FixtureError::Revoked(format!("cannot fence fixture setup events: {error}"))
                })?
                .result
                .frame_tree
                .frame,
        );
    }
    event_tasks.barrier.drain().await?;
    frame.ok_or_else(|| FixtureError::Revoked("fixture setup frame was not returned".into()))
}

async fn wait_for_bound_frame(
    monitor: &FixtureMonitor,
    deadline: Instant,
) -> std::result::Result<String, FixtureError> {
    loop {
        check_deadline(deadline)?;
        let frame_id = {
            let data = monitor
                .data
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(reason) = &data.revoked {
                return Err(FixtureError::Revoked(reason.clone()));
            }
            data.document
                .as_ref()
                .map(|document| document.frame_id.clone())
        };
        if let Some(frame_id) = frame_id {
            return Ok(frame_id);
        }
        tokio::time::sleep(ASSERT_POLL).await;
    }
}

async fn wait_for_context(
    monitor: &FixtureMonitor,
    deadline: Instant,
) -> std::result::Result<FixtureDocument, FixtureError> {
    loop {
        check_deadline(deadline)?;
        match monitor.snapshot() {
            Ok(document) => return Ok(document),
            Err(FixtureError::Revoked(message)) if message.contains("not bound") => {}
            Err(error) => return Err(error),
        }
        tokio::time::sleep(ASSERT_POLL).await;
    }
}

async fn verify_document(
    page: &BrowserPage,
    monitor: &FixtureMonitor,
    expected: &FixtureDocument,
    exact_url: &str,
) -> std::result::Result<(), FixtureError> {
    let current = page
        .raw()
        .execute(GetFrameTreeParams::default())
        .await
        .map_err(|error| FixtureError::Revoked(format!("cannot verify current frame: {error}")))?;
    let frame = &current.result.frame_tree.frame;
    if frame.id.as_ref() != expected.frame_id
        || frame.loader_id.as_ref() != expected.loader_id
        || frame.url != exact_url
    {
        monitor.revoke("current frame identity or exact URL changed");
        return Err(FixtureError::Revoked(
            "current frame identity or exact URL changed".into(),
        ));
    }
    let latest = monitor.snapshot()?;
    if latest != *expected {
        monitor.revoke("bound document identity changed");
        return Err(FixtureError::Revoked(
            "bound document identity changed".into(),
        ));
    }
    Ok(())
}

fn key_name(key: FixtureKey) -> &'static str {
    match key {
        FixtureKey::Tab => "Tab",
        FixtureKey::Escape => "Escape",
        FixtureKey::ArrowUp => "ArrowUp",
        FixtureKey::ArrowDown => "ArrowDown",
        FixtureKey::ArrowLeft => "ArrowLeft",
        FixtureKey::ArrowRight => "ArrowRight",
    }
}

fn check_deadline(deadline: Instant) -> std::result::Result<(), FixtureError> {
    if Instant::now() >= deadline {
        Err(FixtureError::Expired(
            "fixture execution deadline expired".into(),
        ))
    } else {
        Ok(())
    }
}

fn binding_id() -> String {
    let seq = BINDING_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("fixture-{}-{nanos:x}-{seq:x}", std::process::id())
}

#[cfg(windows)]
const WINDOWS_FIXTURE_REQUEST_BUDGET: usize = 256;

#[cfg(windows)]
async fn install_windows_fixture_fetch(
    page: &BrowserPage,
    server: Arc<tokio::sync::Mutex<Option<FixtureServer>>>,
    monitor: Arc<FixtureMonitor>,
    deadline: Instant,
) -> std::result::Result<JoinHandle<()>, FixtureError> {
    let raw = page.raw().clone();
    let mut paused = raw
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(|error| FixtureError::Browser(error.to_string()))?;
    raw.execute(FetchEnableParams {
        patterns: Some(vec![RequestPattern {
            url_pattern: Some("*".into()),
            resource_type: None,
            request_stage: None,
        }]),
        ..Default::default()
    })
    .await
    .map_err(|error| FixtureError::Browser(error.to_string()))?;
    let budget = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    Ok(tokio::spawn(async move {
        while let Some(event) = paused.next().await {
            if Instant::now() >= deadline
                || budget.fetch_add(1, Ordering::SeqCst) >= WINDOWS_FIXTURE_REQUEST_BUDGET
            {
                let _ = raw
                    .execute(FailRequestParams::new(
                        event.request_id.clone(),
                        chromiumoxide::cdp::browser_protocol::network::ErrorReason::BlockedByClient,
                    ))
                    .await;
                monitor.revoke("Windows fixture Fetch request deadline or budget exceeded");
                return;
            }
            let method = event.request.method.as_str();
            let response = if matches!(method, "GET" | "HEAD") {
                let guard = server.lock().await;
                guard
                    .as_ref()
                    .ok_or_else(|| {
                        FixtureError::FixtureServerNotOwned("fixture server is closed".into())
                    })
                    .and_then(|server| server.response_for(method, &event.request.url))
            } else {
                Err(FixtureError::Revoked(
                    "Windows fixture request is outside immutable GET/HEAD policy".into(),
                ))
            };
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    let _ = raw
                        .execute(FailRequestParams::new(
                            event.request_id.clone(),
                            chromiumoxide::cdp::browser_protocol::network::ErrorReason::BlockedByClient,
                        ))
                        .await;
                    monitor.revoke(&format!("Windows fixture request denied: {error}"));
                    return;
                }
            };
            let mut fulfill =
                FulfillRequestParams::new(event.request_id.clone(), i64::from(response.status));
            fulfill.response_headers = Some(
                response
                    .headers
                    .iter()
                    .map(|(name, value)| HeaderEntry::new(name.clone(), value.clone()))
                    .collect(),
            );
            fulfill.body = Some(base64_encode(&response.body).into());
            let result = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                raw.execute(fulfill),
            )
            .await;
            if !matches!(result, Ok(Ok(_))) {
                monitor.revoke("Windows fixture Fetch fulfillment failed");
                return;
            }
        }
        if !monitor.expected_shutdown() {
            monitor.revoke("Windows fixture Fetch event stream ended");
        }
    }))
}

#[cfg(windows)]
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[((first & 0x03) << 4 | second >> 4) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((second & 0x0f) << 2 | third >> 6) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn policy_digest(
    exact_url: &str,
    fixture_source_digest: &str,
    server_instance: &str,
    backend: FixtureConfinementBackend,
) -> String {
    let canonical = format!(
        "rightkit.browser.fixture-policy.v1\n{}\n{fixture_source_digest}\n{server_instance}\n{exact_url}",
        backend.policy_name()
    );
    format!("sha256:{}", hex(&Sha256::digest(canonical.as_bytes())))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_with_cancel_future_is_send() {
        fn assert_send<T: Send>(_: T) {}

        let assert_run_with_cancel = |session: FixtureSession| {
            assert_send(session.run_with_cancel(FixtureCancellation::default()));
        };
        let _ = assert_run_with_cancel;
    }

    #[test]
    fn document_replacement_is_sticky_and_never_rebinds() {
        let monitor = FixtureMonitor::default();
        monitor.bind_main_document(
            "frame-a",
            "loader-a",
            "http://127.0.0.1:1/index.html",
            "http://127.0.0.1:1/index.html",
        );
        monitor.context_created("frame-a", "context-a");
        monitor.frame_navigated(
            "frame-a",
            "loader-b",
            "http://127.0.0.1:1/index.html",
            "http://127.0.0.1:1/index.html",
        );
        monitor.bind_main_document(
            "frame-a",
            "loader-a",
            "http://127.0.0.1:1/index.html",
            "http://127.0.0.1:1/index.html",
        );
        assert!(matches!(monitor.snapshot(), Err(FixtureError::Revoked(_))));
    }

    #[test]
    fn context_destruction_after_binding_revokes() {
        let monitor = FixtureMonitor::default();
        monitor.bind_main_document(
            "frame-a",
            "loader-a",
            "http://127.0.0.1:1/index.html",
            "http://127.0.0.1:1/index.html",
        );
        monitor.context_created("frame-a", "context-a");
        monitor.context_destroyed("context-a");
        assert!(matches!(monitor.snapshot(), Err(FixtureError::Revoked(_))));
    }

    #[test]
    fn incomplete_cleanup_cannot_form_pass_evidence() {
        let cleanup = FixtureCleanupEvidence::pending();
        assert!(!cleanup.complete());
        let mut cleanup = cleanup;
        cleanup.browser_stopped = true;
        cleanup.proxy_stopped = true;
        cleanup.server_stopped = true;
        assert!(!cleanup.complete());
        cleanup.profile_removed = true;
        cleanup.transport_stopped = true;
        assert!(cleanup.complete());
    }

    #[tokio::test]
    async fn cancellation_is_sticky_idempotent_and_race_safe() {
        let cancellation = FixtureCancellation::new();
        cancellation.cancel();
        cancellation.cancel();
        assert!(cancellation.is_cancelled());
        cancellation.wait().await;

        let cancellation = FixtureCancellation::new();
        let waiter_token = cancellation.clone();
        let waiter = tokio::spawn(async move { waiter_token.wait().await });
        cancellation.cancel();
        cancellation.cancel();
        waiter.await.expect("cancellation waiter should finish");
        assert!(cancellation.is_cancelled());
    }
}
