//! The demo host: consent, the session state machine, and the orchestration of driver, recorder,
//! badge and kill hotkey (design, sections 1 to 3).
//!
//! Lock rule: `inner` is never held across a call into the recorder, the driver, a window or an emit.
//! Phase changes under the lock decide which caller owns a conclusion, so a stop, a kill and a
//! take that ends by itself never seal the same bundle twice.

use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use rightkit_agent_tools::backends::demo::{
    ActResult, ActStep, AppsResult, BatchResult, BundleState, CaptureState, DemoApp, DemoErrorCode,
    EndResult, RequestParams, RequestResult, SessionToken, SnapshotResult, CONSENT_TIMEOUT,
    DEMO_SERVICE, MAX_BATCH_STEPS, METHODS,
};
use rightkit_agent_tools::{CallContextExt, CLIENT_APP};
use rightkit_automation::{EvidenceSink, Indicator, Receipt, ResultKind, SessionEvent};
use rightkit_child_process::reveal::{reveal, PathScope};
use rightkit_contracts::types::record_event::{InputKind, Phase as EventPhase};
use rightkit_contracts::types::record_session::BundleStatus;
use rightkit_credentials::Secret;
use rightkit_daemon::{
    Allowlist, AppRoot, AppRootError, CallContext, Hosted, ServiceConfig, Suite,
};
use serde_json::json;
use tauri::{AppHandle, Emitter};

use super::catalog::{self, CatalogEntry};
use super::driver::{receipt_id, DemoError, Driver};
use super::recorder::{EventSpec, RecErr, RecorderHandle, Sealed, SourceInfo, StartSpec};
use super::session::{self, Phase, Step};
use super::store::{now_ms, Store};
use super::{
    badge, kill, BundleDto, ConsentDto, PrepareDto, ReceiptDto, SessionDto, StateDto, StatusDto,
    EVENT_CONSENT, EVENT_RECEIPT, EVENT_STATE,
};

/// Rank of the demo service among hosts of the same service name (lower wins a takeover).
const SERVICE_RANK: u8 = 50;
/// How long a stop waits for the recorder to finish and seal.
const STOP_WAIT: Duration = Duration::from_secs(30);
/// How long a kill waits before it aborts the helper.
const KILL_WAIT: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Agent,
    Person,
}

#[derive(Clone)]
struct Live {
    id: String,
    kind: Kind,
    entry: &'static CatalogEntry,
    agent_name: Option<String>,
    agent_exe: Option<String>,
    token: Option<Secret>,
    driver: Arc<Driver>,
    bundle_dir: Option<PathBuf>,
    started_ms: Option<u64>,
}

struct Pending {
    id: String,
    entry: &'static CatalogEntry,
    agent_name: String,
    consent: ConsentDto,
    tx: mpsc::Sender<bool>,
}

struct Ended {
    token: Secret,
    killed: bool,
}

struct Inner {
    phase: Phase,
    session: Option<Live>,
    pending: Option<Pending>,
    /// The token of the last session that ended, so a late call says `session_ended` or
    /// `session_killed` rather than `invalid_token`.
    last_ended: Option<Ended>,
    bundle: Option<BundleDto>,
    message: Option<String>,
    starting: bool,
}

impl Inner {
    fn authorize(&self, token: &str) -> Result<(Arc<Driver>, String), DemoError> {
        let token = Secret::from_string(token);
        if let Some(live) = self.session.as_ref() {
            if live.token.as_ref().is_some_and(|t| t.ct_eq(&token)) {
                if rightkit_automation::killed() || self.phase == Phase::Killed {
                    return Err(DemoError::of(
                        DemoErrorCode::SessionKilled,
                        "the session was killed",
                    ));
                }
                return Ok((live.driver.clone(), live.id.clone()));
            }
        }
        if let Some(e) = self.last_ended.as_ref() {
            if e.token.ct_eq(&token) {
                return Err(if e.killed {
                    DemoError::of(DemoErrorCode::SessionKilled, "the session was killed")
                } else {
                    DemoError::of(DemoErrorCode::SessionEnded, "the session has ended")
                });
            }
        }
        Err(DemoError::of(
            DemoErrorCode::InvalidToken,
            "unknown or expired session token",
        ))
    }
}

pub struct DemoHost {
    app: AppHandle,
    store: Store,
    suite: Suite,
    enabled: std::sync::atomic::AtomicBool,
    inner: Mutex<Inner>,
    service: Mutex<Option<Hosted>>,
    recorder: OnceLock<RecorderHandle>,
}

/// What the OS verified about the process on the other end of the connection.
struct Peer {
    pid: Option<u32>,
    exe: Option<PathBuf>,
    verified: bool,
}

/// Peer executable comes from RightKit's per-call OS identity check.
fn peer_facts(cx: &CallContext) -> Peer {
    let exe = cx.peer_executable();
    Peer {
        pid: cx.peer().and_then(|p| p.pid),
        verified: exe.is_some(),
        exe,
    }
}

fn clean_label(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty() && t.chars().count() <= 64 && t.chars().all(|c| !c.is_control()))
        .then(|| t.to_string())
}

struct Evidence(Weak<DemoHost>);
impl EvidenceSink for Evidence {
    fn record(&self, r: &Receipt) {
        if let Some(h) = self.0.upgrade() {
            h.on_receipt(r);
        }
    }
}

struct Pulse(Weak<DemoHost>);
impl Indicator for Pulse {
    fn event(&self, e: &SessionEvent) {
        if let Some(h) = self.0.upgrade() {
            h.on_indicator(e);
        }
    }
}

fn rec_err(e: RecErr) -> DemoError {
    if e.permission() {
        DemoError::of(
            DemoErrorCode::PermissionMissing,
            format!(
                "Screen Recording permission is not granted to {}",
                crate::identity().product_name
            ),
        )
    } else {
        DemoError::raw("record_failed", format!("{}: {}", e.code, e.message))
    }
}

impl DemoHost {
    pub fn new(
        app: AppHandle,
        app_root: &AppRoot,
        suite: Suite,
    ) -> Result<Arc<Self>, AppRootError> {
        let store = Store::open(app_root)?;
        let enabled = store.enabled();
        Ok(Arc::new(Self {
            app,
            store,
            suite,
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            inner: Mutex::new(Inner {
                phase: Phase::Idle,
                session: None,
                pending: None,
                last_ended: None,
                bundle: None,
                message: None,
                starting: false,
            }),
            service: Mutex::new(None),
            recorder: OnceLock::new(),
        }))
    }

    fn g(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn require_enabled(&self) -> Result<(), DemoError> {
        if self.is_enabled() {
            Ok(())
        } else {
            Err(DemoError::of(
                DemoErrorCode::DemoDisabled,
                format!(
                    "agent demo mode is switched off in {}",
                    crate::identity().product_name
                ),
            ))
        }
    }

    fn audit(&self, event: &str, fields: serde_json::Value) {
        self.store.audit(event, fields);
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    fn recorder(self: &Arc<Self>) -> &RecorderHandle {
        self.recorder.get_or_init(|| {
            let root = self.store.bundles_root();
            let weak = Arc::downgrade(self);
            RecorderHandle::spawn(
                root,
                Arc::new(move |sealed| {
                    if let Some(h) = weak.upgrade() {
                        h.on_take_ended(sealed);
                    }
                }),
            )
        })
    }

    // ------------------------------------------------------------------ state and events

    fn state_dto(&self, g: &Inner) -> StateDto {
        StateDto {
            phase: g.phase,
            session: g.session.as_ref().map(|l| SessionDto {
                id: l.id.clone(),
                kind: if l.kind == Kind::Agent {
                    "agent"
                } else {
                    "person"
                },
                app: l.entry.id.to_string(),
                app_name: l.entry.name.to_string(),
                agent_name: l.agent_name.clone(),
                started_ms: l.started_ms,
            }),
            bundle: g.bundle.clone(),
            message: g.message.clone(),
            hotkey: kill::display(),
        }
    }

    pub fn status(&self) -> StatusDto {
        let g = self.g();
        StatusDto {
            compiled: true,
            enabled: self.is_enabled(),
            hotkey: kill::display(),
            state: self.state_dto(&g),
            pending_consent: g.pending.as_ref().map(|p| p.consent.clone()),
        }
    }

    fn publish(&self) {
        let dto = {
            let g = self.g();
            self.state_dto(&g)
        };
        let _ = self.app.emit(EVENT_STATE, dto);
    }

    fn set_message(&self, message: Option<String>) {
        self.g().message = message;
    }

    // ------------------------------------------------------------------ enable and the service

    /// Starts the service when the saved flag says on. Called once from plugin setup.
    pub fn restore(self: &Arc<Self>) {
        if self.is_enabled() {
            if let Err(m) = self.start_service() {
                self.enabled
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                self.set_message(Some(m));
            }
        }
    }

    pub fn set_enabled(self: &Arc<Self>, on: bool) -> Result<(), String> {
        let store = self.store();
        if on {
            self.start_service()?;
            store.set_enabled(true).map_err(|e| e.to_string())?;
            self.enabled
                .store(true, std::sync::atomic::Ordering::SeqCst);
        } else {
            self.kill("demo mode switched off");
            self.enabled
                .store(false, std::sync::atomic::Ordering::SeqCst);
            store.set_enabled(false).map_err(|e| e.to_string())?;
            self.stop_service();
        }
        self.audit("enabled", json!({"on": on}));
        self.publish();
        Ok(())
    }

    fn start_service(self: &Arc<Self>) -> Result<(), String> {
        let mut svc = self.service.lock().unwrap_or_else(|e| e.into_inner());
        if svc.is_some() {
            return Ok(());
        }
        // Only `right-ctl` may call, and only the six demo methods. The label is a claim (the OS peer
        // check is what the consent dialog shows); the allowlist just keeps other labels out.
        let allow = Allowlist::new().allow(CLIENT_APP, METHODS.iter().map(|m| m.name()));
        let cfg = ServiceConfig::new(
            DEMO_SERVICE,
            crate::identity().slug.as_str(),
            SERVICE_RANK,
            crate::identity().version.as_str(),
            allow,
        );
        let handler = Arc::new(super::service::DemoService::new(Arc::downgrade(self)));
        let hosted = rightkit_daemon::host(&self.suite, cfg, handler)
            .map_err(|e| format!("the demo service could not start: {e:?}"))?;
        *svc = Some(hosted);
        Ok(())
    }

    fn stop_service(&self) {
        if let Some(h) = self
            .service
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            h.shutdown();
        }
    }

    /// App exit: never leave a capture or a lease running.
    pub fn shutdown(self: &Arc<Self>) {
        self.kill("app exit");
        // The kill concludes on its own thread; give the seal a moment.
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until && self.g().session.is_some() {
            std::thread::sleep(Duration::from_millis(100));
        }
        self.stop_service();
    }

    // ------------------------------------------------------------------ catalog and permissions

    pub fn catalog(&self) -> AppsResult {
        AppsResult {
            apps: catalog::CATALOG
                .iter()
                .map(|e| DemoApp {
                    id: e.id.to_string(),
                    name: e.name.to_string(),
                })
                .collect(),
        }
    }

    pub fn apps(&self) -> Result<AppsResult, DemoError> {
        self.require_enabled()?;
        Ok(self.catalog())
    }

    /// The OS grants demo mode needs, and the recorder helper's state. Never prompts by itself.
    pub fn prepare(self: &Arc<Self>) -> PrepareDto {
        let driver_status = rightkit_automation::native(rightkit_automation::Policy::new(
            std::iter::empty::<String>(),
        ))
        .status();
        let recorder = self
            .recorder()
            .permission()
            .unwrap_or_else(|e| format!("recorder unavailable: {}", e.message));
        PrepareDto {
            accessibility: driver_status.accessibility,
            screen_recording: driver_status.screen_capture,
            recorder,
            notes: driver_status.notes,
        }
    }

    // ------------------------------------------------------------------ consent

    /// `demo.request`: blocks until a person answers the consent dialog.
    pub fn request(
        self: &Arc<Self>,
        cx: &CallContext,
        p: RequestParams,
    ) -> Result<RequestResult, DemoError> {
        self.require_enabled()?;
        let entry = catalog::find(&p.app).ok_or_else(|| {
            DemoError::of(
                DemoErrorCode::UnknownApp,
                "that app is not in the demo catalog; call demo.apps",
            )
        })?;
        let agent_name = clean_label(&p.agent_name).ok_or_else(|| {
            DemoError::of(
                DemoErrorCode::InvalidArguments,
                "agent_name must be 1 to 64 printable characters",
            )
        })?;
        let peer = peer_facts(cx);
        if !peer.verified {
            return Err(DemoError::raw(
                "peer_unverified",
                "the operating system could not verify the calling program, so consent cannot be asked",
            ));
        }
        let id = session::new_session_id().map_err(|m| DemoError::raw("internal", m))?;
        let consent = ConsentDto {
            session: id.clone(),
            app: entry.id.to_string(),
            app_name: entry.name.to_string(),
            agent_name: agent_name.clone(),
            agent_exe: peer.exe.as_ref().map(|p| p.display().to_string()),
            agent_pid: peer.pid,
            will_do: format!(
                "click and type in {name}, and record {name}'s window. It cannot act on {product}, Pulse or password fields. Press {hotkey} at any time to stop it.",
                name = entry.name,
                product = crate::identity().product_name,
                hotkey = kill::display()
            ),
        };
        let (tx, rx) = mpsc::channel::<bool>();
        {
            let mut g = self.g();
            let Some(phase) = session::next(g.phase, Step::Request) else {
                return Err(DemoError::raw(
                    "session_active",
                    "another demo session is in progress",
                ));
            };
            g.phase = phase;
            g.message = None;
            g.bundle = None;
            g.pending = Some(Pending {
                id: id.clone(),
                entry,
                agent_name: agent_name.clone(),
                consent: consent.clone(),
                tx,
            });
        }
        self.audit(
            "consent_requested",
            json!({"session": id, "app": entry.id, "agent_name": agent_name, "agent_exe": consent.agent_exe, "agent_pid": consent.agent_pid}),
        );
        self.publish();
        let _ = self.app.emit(EVENT_CONSENT, consent);
        rightkit_tauri_shell::windows::reveal(&self.app, Some("main"));

        let deadline = Instant::now() + CONSENT_TIMEOUT;
        let decision = loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(v) => break Some(v),
                Err(RecvTimeoutError::Disconnected) => break Some(false),
                Err(RecvTimeoutError::Timeout) => {
                    if cx.peer_closed() || cx.handoff_requested() || Instant::now() >= deadline {
                        break None;
                    }
                }
            }
        };
        match decision {
            Some(true) => {
                let token = {
                    let g = self.g();
                    g.session
                        .as_ref()
                        .filter(|l| l.id == id)
                        .and_then(|l| l.token.clone())
                };
                let Some(token) = token else {
                    return Err(DemoError::of(
                        DemoErrorCode::ConsentDenied,
                        "the session was ended before it started",
                    ));
                };
                if cx.peer_closed() {
                    self.kill("the agent disconnected after consent");
                    return Err(DemoError::of(
                        DemoErrorCode::ConsentDenied,
                        "the agent disconnected",
                    ));
                }
                Ok(RequestResult {
                    session: id,
                    token: SessionToken::new(
                        token
                            .expose_str()
                            .map_err(|e| DemoError::raw("internal", e.to_string()))?,
                    ),
                    app: entry.id.to_string(),
                })
            }
            Some(false) => Err(DemoError::of(
                DemoErrorCode::ConsentDenied,
                "the person refused consent",
            )),
            None => {
                self.cancel_pending(&id, "no answer to the consent dialog");
                Err(DemoError::of(
                    DemoErrorCode::ConsentDenied,
                    "no one answered the consent dialog",
                ))
            }
        }
    }

    fn cancel_pending(&self, id: &str, why: &str) {
        let removed = {
            let mut g = self.g();
            if g.pending.as_ref().is_some_and(|p| p.id == id) {
                g.pending = None;
                g.phase = session::next(g.phase, Step::Deny).unwrap_or(g.phase);
                g.message = Some(why.to_string());
                true
            } else {
                false
            }
        };
        if removed {
            self.audit("consent_cancelled", json!({"session": id, "why": why}));
            self.publish();
        }
    }

    /// The person's answer to the consent dialog.
    pub fn respond(self: &Arc<Self>, session_id: &str, approve: bool) -> Result<(), String> {
        let pending = {
            let mut g = self.g();
            if g.pending.as_ref().is_some_and(|p| p.id == session_id) {
                g.pending.take()
            } else {
                None
            }
        };
        let Some(pending) = pending else {
            return Err("there is no pending consent request for that session".into());
        };
        let deny = |why: Option<String>| {
            {
                let mut g = self.g();
                g.phase = session::next(g.phase, Step::Deny).unwrap_or(g.phase);
                g.message = why;
            }
            let _ = pending.tx.send(false);
            self.audit("consent_denied", json!({"session": pending.id}));
            self.publish();
        };
        if !approve {
            deny(None);
            return Ok(());
        }
        // A fresh consent is the only thing that lifts the kill switch.
        rightkit_automation::reset_kill();
        if let Err(m) = kill::arm(&self.app, self) {
            deny(Some(m.clone()));
            return Err(m);
        }
        let token = match rightkit_credentials::Store::generate_key(32) {
            Ok(t) => t,
            Err(e) => {
                let message = e.to_string();
                kill::disarm(&self.app);
                deny(Some(message.clone()));
                return Err(message);
            }
        };
        let driver = Arc::new(Driver::new(
            pending.entry,
            Arc::new(Evidence(Arc::downgrade(self))),
            Arc::new(Pulse(Arc::downgrade(self))),
        ));
        {
            let mut g = self.g();
            g.phase = session::next(g.phase, Step::Approve).unwrap_or(g.phase);
            g.session = Some(Live {
                id: pending.id.clone(),
                kind: Kind::Agent,
                entry: pending.entry,
                agent_name: Some(pending.agent_name.clone()),
                agent_exe: pending.consent.agent_exe.clone(),
                token: Some(token),
                driver,
                bundle_dir: None,
                started_ms: None,
            });
        }
        self.audit(
            "consent_approved",
            json!({"session": pending.id, "app": pending.entry.id}),
        );
        self.publish();
        let _ = pending.tx.send(true);
        // Best effort: start recording now. A missing grant or app surfaces on the agent's first call.
        let host = self.clone();
        std::thread::spawn(move || {
            let _ = host.ensure_recording();
        });
        Ok(())
    }

    // ------------------------------------------------------------------ a person records

    /// Record-only session started by the person: no agent, no token, no consent dialog (pressing
    /// the button is the consent). The same policy, badge, hotkey and bundle rules apply.
    pub fn begin_person(self: &Arc<Self>, app: &str) -> Result<(), String> {
        self.require_enabled().map_err(|e| e.message)?;
        let entry = catalog::find(app).ok_or("that app is not in the demo catalog")?;
        let id = session::new_session_id()?;
        {
            let g = self.g();
            if session::next(g.phase, Step::Request).is_none() {
                return Err("another demo session is in progress".into());
            }
        }
        rightkit_automation::reset_kill();
        kill::arm(&self.app, self)?;
        let driver = Arc::new(Driver::new(
            entry,
            Arc::new(Evidence(Arc::downgrade(self))),
            Arc::new(Pulse(Arc::downgrade(self))),
        ));
        {
            let mut g = self.g();
            let Some(phase) =
                session::next(g.phase, Step::Request).and_then(|p| session::next(p, Step::Approve))
            else {
                drop(g);
                kill::disarm(&self.app);
                return Err("another demo session is in progress".into());
            };
            g.phase = phase;
            g.message = None;
            g.bundle = None;
            g.session = Some(Live {
                id: id.clone(),
                kind: Kind::Person,
                entry,
                agent_name: None,
                agent_exe: None,
                token: None,
                driver,
                bundle_dir: None,
                started_ms: None,
            });
        }
        self.audit("person_session", json!({"session": id, "app": entry.id}));
        self.publish();
        match self.ensure_recording() {
            Ok(()) => Ok(()),
            Err(e) => {
                if let Ok(live) = self.claim_stop() {
                    let _ = self.conclude(live, None, false);
                }
                self.set_message(Some(e.message.clone()));
                self.publish();
                Err(e.message)
            }
        }
    }

    // ------------------------------------------------------------------ begin recording

    /// Moves an armed session to recording: OS grants, the target window, the policy check, the badge,
    /// the lease, then the recorder. Idempotent while recording.
    fn ensure_recording(self: &Arc<Self>) -> Result<(), DemoError> {
        let live = {
            let mut g = self.g();
            match g.phase {
                Phase::Recording => return Ok(()),
                Phase::Armed => {}
                Phase::Killed => {
                    return Err(DemoError::of(
                        DemoErrorCode::SessionKilled,
                        "the session was killed",
                    ))
                }
                Phase::AwaitingConsent => {
                    return Err(DemoError::of(
                        DemoErrorCode::NotArmed,
                        "consent has not been given",
                    ))
                }
                Phase::Idle | Phase::Sealed | Phase::Stopping => {
                    return Err(DemoError::of(
                        DemoErrorCode::SessionEnded,
                        "the session has ended",
                    ))
                }
            }
            if g.starting {
                return Err(DemoError::of(
                    DemoErrorCode::NotArmed,
                    "the session is starting; retry in a moment",
                ));
            }
            let Some(live) = g.session.clone() else {
                return Err(DemoError::of(
                    DemoErrorCode::NotArmed,
                    "there is no session",
                ));
            };
            g.starting = true;
            live
        };
        let result = self.begin(&live);
        let started = {
            let mut g = self.g();
            g.starting = false;
            match &result {
                Ok(dir) if g.phase == Phase::Armed => {
                    g.phase = session::next(g.phase, Step::Started).unwrap_or(g.phase);
                    if let Some(l) = g.session.as_mut() {
                        l.bundle_dir = Some(dir.clone());
                        l.started_ms = Some(now_ms());
                    }
                    g.message = None;
                    true
                }
                _ => false,
            }
        };
        match result {
            Ok(dir) if started => {
                self.audit(
                    "recording_started",
                    json!({"session": live.id, "app": live.entry.id, "agent_exe": live.agent_exe}),
                );
                self.recorder().event(EventSpec::Stage {
                    description: format!(
                        "consent approved for {} by the person; agent: {}",
                        live.entry.id,
                        live.agent_name.as_deref().unwrap_or("none (person)")
                    ),
                });
                self.spawn_ticker(live.id.clone());
                self.publish();
                let _ = dir;
                Ok(())
            }
            Ok(dir) => {
                // A kill landed while the recorder was starting: stop what was started.
                let mut late = live.clone();
                late.bundle_dir = Some(dir);
                let _ = self.conclude(late, None, true);
                Err(DemoError::of(
                    DemoErrorCode::SessionKilled,
                    "the session was killed",
                ))
            }
            Err(e) => {
                self.set_message(Some(e.message.clone()));
                self.publish();
                Err(e)
            }
        }
    }

    /// The fallible part of starting. Returns the bundle directory. Undoes its own partial work.
    fn begin(self: &Arc<Self>, live: &Live) -> Result<PathBuf, DemoError> {
        let driver = &live.driver;
        let agent = live.kind == Kind::Agent;
        let status = driver.os_status();
        if agent && !status.accessibility {
            return Err(DemoError::of(
                DemoErrorCode::PermissionMissing,
                format!(
                    "Accessibility permission is not granted to {}",
                    crate::identity().product_name
                ),
            ));
        }
        if !status.screen_capture {
            return Err(DemoError::of(
                DemoErrorCode::PermissionMissing,
                format!(
                    "Screen Recording permission is not granted to {}",
                    crate::identity().product_name
                ),
            ));
        }
        let rec = self.recorder();
        let sources = rec.sources().map_err(rec_err)?;
        let entry = driver.entry();
        let source = pick_source(&sources, entry.platform_id()).ok_or_else(|| {
            DemoError::of(
                DemoErrorCode::NotArmed,
                format!("{} is not running or has no visible window", entry.name),
            )
        })?;
        let app = driver.find_app(source.pid as i32).ok_or_else(|| {
            DemoError::of(
                DemoErrorCode::NotArmed,
                format!("{} is not running", entry.name),
            )
        })?;
        // Recording has no policy hook of its own: check the resolved app here, first.
        driver.check(&app)?;
        if rightkit_automation::killed() {
            return Err(DemoError::of(
                DemoErrorCode::SessionKilled,
                "the session was killed",
            ));
        }
        let dir = self
            .store()
            .bundle_dir(&live.id)
            .ok_or_else(|| DemoError::raw("record_failed", "demo mode has no bundle folder"))?;
        // The badge first: an agent never drives without a visible badge.
        let target = rightkit_overlay::Rect {
            x: source.x,
            y: source.y,
            width: source.w,
            height: source.h,
        };
        badge::show(&self.app, target).map_err(|m| DemoError::of(DemoErrorCode::NotArmed, m))?;
        let label = format!(
            "{} demo: {}",
            crate::identity().product_name,
            live.agent_name.as_deref().unwrap_or("recording")
        );
        let holder = format!("{}-demo:{}", crate::identity().slug, live.id);
        if let Err(e) = driver.attach(&app, source.window_id, &label, &holder, agent) {
            badge::hide(&self.app);
            return Err(e);
        }
        let started = rec.start(StartSpec {
            app_id: entry.platform_id().to_string(),
            window_id: source.window_id,
            pid: source.pid,
            out: dir.clone(),
        });
        if let Err(e) = started {
            driver.detach();
            badge::hide(&self.app);
            return Err(rec_err(e));
        }
        Ok(dir)
    }

    fn spawn_ticker(self: &Arc<Self>, id: String) {
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || {
            let mut ticks = 0u32;
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let Some(host) = weak.upgrade() else { return };
                let driver = {
                    let g = host.g();
                    match (&g.phase, &g.session) {
                        (Phase::Recording, Some(l)) if l.id == id => l.driver.clone(),
                        _ => return,
                    }
                };
                ticks += 1;
                if ticks.is_multiple_of(20) {
                    driver.renew();
                }
                if ticks.is_multiple_of(4) && !driver.poll().is_empty() {
                    // The target app quit: seal what was recorded.
                    if let Ok(live) = host.claim_stop() {
                        let _ = host.conclude(live, None, false);
                    }
                    return;
                }
            }
        });
    }

    // ------------------------------------------------------------------ the agent's calls

    fn authorize(&self, token: &str) -> Result<(Arc<Driver>, String), DemoError> {
        self.g().authorize(token)
    }

    pub fn snapshot(self: &Arc<Self>, token: &SessionToken) -> Result<SnapshotResult, DemoError> {
        self.require_enabled()?;
        let (driver, _id) = self.authorize(token.expose())?;
        self.ensure_recording()?;
        // Every catalog app is declared capture-safe (catalog.rs); an app outside it never gets here.
        Ok(SnapshotResult {
            capture_state: CaptureState::Safe,
            elements: driver.snapshot()?,
        })
    }

    pub fn act(
        self: &Arc<Self>,
        token: &SessionToken,
        step: &ActStep,
    ) -> Result<ActResult, DemoError> {
        self.require_enabled()?;
        let (driver, id) = self.authorize(token.expose())?;
        self.ensure_recording()?;
        driver
            .act(&id, step)
            .map(|receipt_id| ActResult { receipt_id })
    }

    pub fn batch(
        self: &Arc<Self>,
        token: &SessionToken,
        steps: &[ActStep],
    ) -> Result<BatchResult, DemoError> {
        self.require_enabled()?;
        if steps.is_empty() || steps.len() > MAX_BATCH_STEPS {
            return Err(DemoError::of(
                DemoErrorCode::InvalidArguments,
                format!(
                    "demo.batch takes 1 to {MAX_BATCH_STEPS} steps, got {}",
                    steps.len()
                ),
            ));
        }
        let (driver, id) = self.authorize(token.expose())?;
        self.ensure_recording()?;
        let mut results = Vec::new();
        for (i, step) in steps.iter().enumerate() {
            match driver.act(&id, step) {
                Ok(receipt_id) => results.push(ActResult { receipt_id }),
                Err(e) => {
                    let error_code = serde_json::from_value(serde_json::Value::String(e.code)).ok();
                    return Ok(BatchResult {
                        completed: results.len(),
                        results,
                        failed_step: Some(i),
                        error_code,
                    });
                }
            }
        }
        Ok(BatchResult {
            completed: results.len(),
            results,
            failed_step: None,
            error_code: None,
        })
    }

    /// `demo.end`.
    pub fn end(self: &Arc<Self>, token: &SessionToken) -> Result<EndResult, DemoError> {
        self.require_enabled()?;
        self.authorize(token.expose())?;
        let live = self.claim_stop()?;
        match self.conclude(live, None, false)? {
            Some(b) => {
                if !(b.verified && b.state == "complete") {
                    return Err(DemoError::raw(
                        "bundle_unverified",
                        format!(
                            "the bundle ended as {} and verification found: {}",
                            b.state,
                            b.problems.join("; ")
                        ),
                    ));
                }
                Ok(EndResult {
                    ended: true,
                    bundle_id: b.id,
                    bundle_state: BundleState::Complete,
                })
            }
            None => Err(DemoError::raw(
                "no_bundle",
                "the session ended before anything was recorded",
            )),
        }
    }

    // ------------------------------------------------------------------ receipts

    fn on_receipt(self: &Arc<Self>, r: &Receipt) {
        let session = self
            .g()
            .session
            .as_ref()
            .map(|l| l.id.clone())
            .unwrap_or_else(|| "none".into());
        let id = receipt_id(&session, r.seq);
        let result = match r.result {
            ResultKind::Done => "done",
            ResultKind::Changed => "changed",
            ResultKind::Blocked => "blocked",
            ResultKind::Failed => "failed",
        };
        let dto = ReceiptDto {
            receipt_id: id.clone(),
            session: session.clone(),
            action: r.action.clone(),
            result,
            denied_by: r.denied_by.map(|d| d.label()),
            tier: r.tier_used.map(|t| t.label()),
            ts_ms: r.ts_ms,
        };
        // Content-free by construction: no value, no element label, no text.
        self.audit(
            "act",
            json!({"session": session, "receipt_id": id, "action": r.action, "result": result, "denied_by": dto.denied_by, "tier": dto.tier}),
        );
        let _ = self.app.emit(EVENT_RECEIPT, dto);
        let (kind, phase) = match r.action.as_str() {
            "type_text" | "set_value" => (InputKind::Type, EventPhase::Text),
            "keys" => (InputKind::Key, EventPhase::Press),
            "focus" => (InputKind::Focus, EventPhase::Press),
            _ => (InputKind::Click, EventPhase::Down),
        };
        if let Some(rec) = self.recorder.get() {
            rec.event(EventSpec::Input {
                kind,
                phase,
                effect_id: id,
                admitted: r.result == ResultKind::Done,
            });
        }
    }

    fn on_indicator(&self, e: &SessionEvent) {
        // The badge renders from `demo://state`; the engine's own events only matter as an audit trail.
        if let SessionEvent::Ended { session } = e {
            self.audit("engine_session_ended", json!({"engine_session": session}));
        }
    }

    // ------------------------------------------------------------------ stop, kill, seal

    /// Takes ownership of ending the active session (recording or armed). `Err` when there is none.
    fn claim_stop(&self) -> Result<Live, DemoError> {
        let mut g = self.g();
        match g.phase {
            Phase::Recording | Phase::Armed => {
                let Some(live) = g.session.clone() else {
                    return Err(DemoError::of(
                        DemoErrorCode::NotArmed,
                        "there is no session",
                    ));
                };
                g.phase = session::next(g.phase, Step::Stop).unwrap_or(g.phase);
                // An armed session never recorded: it ends at once, with nothing to seal.
                if g.phase == Phase::Idle {
                    g.phase = Phase::Stopping;
                }
                Ok(live)
            }
            Phase::Killed => Err(DemoError::of(
                DemoErrorCode::SessionKilled,
                "the session was killed",
            )),
            _ => Err(DemoError::of(
                DemoErrorCode::SessionEnded,
                "the session has ended",
            )),
        }
    }

    /// The Settings and badge Stop button.
    pub fn stop(self: &Arc<Self>) -> Result<Option<BundleDto>, String> {
        let live = self.claim_stop().map_err(|e| e.message)?;
        self.conclude(live, None, false).map_err(|e| e.message)
    }

    /// The kill control. Engages the process-wide switch first, then ends the session on its own
    /// thread so a hotkey press never blocks.
    pub fn kill(self: &Arc<Self>, reason: &str) {
        rightkit_automation::kill();
        let (owner, live) = {
            let mut g = self.g();
            let was = g.phase;
            let owner = matches!(
                was,
                Phase::AwaitingConsent | Phase::Armed | Phase::Recording
            );
            g.phase = session::next(was, Step::Kill).unwrap_or(was);
            // An unanswered consent is withdrawn: dropping the sender wakes the waiting request.
            g.pending = None;
            if owner {
                g.message = Some(format!("Killed ({reason})"));
            }
            (owner, if owner { g.session.clone() } else { None })
        };
        self.audit("kill", json!({"reason": reason, "owned": owner}));
        self.publish();
        if let Some(live) = live {
            let host = self.clone();
            std::thread::spawn(move || {
                let _ = host.conclude(live, None, true);
            });
        } else if owner {
            kill::disarm(&self.app);
            badge::hide(&self.app);
        }
    }

    /// A take ended without a stop (the target quit, the helper died). Called on the recorder thread,
    /// so the conclusion runs on another one.
    fn on_take_ended(self: &Arc<Self>, sealed: Sealed) {
        let live = {
            let mut g = self.g();
            if g.phase == Phase::Recording {
                g.phase = Phase::Stopping;
                g.session.clone()
            } else {
                None
            }
        };
        if let Some(live) = live {
            let host = self.clone();
            std::thread::spawn(move || {
                let _ = host.conclude(live, Some(sealed), false);
            });
        }
    }

    /// Ends a session this caller owns: stop (or abort) the recorder, tear down the lease, badge and
    /// hotkey, verify the bundle against the typed canaries, record the result. `ended` is a take the
    /// recorder already finished. Returns the bundle, or `None` when nothing was recorded.
    fn conclude(
        self: &Arc<Self>,
        live: Live,
        ended: Option<Sealed>,
        killed: bool,
    ) -> Result<Option<BundleDto>, DemoError> {
        let rec = self.recorder();
        let sealed: Result<Option<Sealed>, RecErr> = if live.bundle_dir.is_none() {
            Ok(None)
        } else if let Some(s) = ended {
            Ok(Some(s))
        } else if killed {
            // Kill order (design, section 3): stop, and abort the helper if that fails.
            match rec.stop(KILL_WAIT) {
                Ok(s) => Ok(Some(s)),
                Err(_) => {
                    rec.abort();
                    rec.stop(KILL_WAIT).map(Some)
                }
            }
        } else {
            rec.stop(STOP_WAIT).map(Some)
        };
        live.driver.detach();
        badge::hide(&self.app);
        kill::disarm(&self.app);

        let canaries = live.driver.canaries();
        let (bundle, outcome): (Option<BundleDto>, Result<(), DemoError>) = match sealed {
            Ok(None) => (None, Ok(())),
            Ok(Some(s)) => {
                let verified = rec.verify(s.dir.clone(), canaries);
                let (ok, problems) = match verified {
                    Ok(v) => (v.ok, v.problems),
                    Err(e) => (
                        false,
                        vec![format!("verification did not run: {}", e.message)],
                    ),
                };
                let state = if killed {
                    "killed"
                } else {
                    match s.status {
                        BundleStatus::Complete => "complete",
                        BundleStatus::PartialQuarantined => "quarantined",
                        BundleStatus::Invalid => "invalid",
                    }
                };
                let name = s
                    .dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                (
                    Some(BundleDto {
                        name,
                        id: s.bundle_id,
                        dir: s.dir.display().to_string(),
                        state: state.to_string(),
                        verified: ok,
                        problems,
                        reason: s.reason,
                    }),
                    Ok(()),
                )
            }
            Err(e) => (None, Err(rec_err(e))),
        };

        {
            let mut g = self.g();
            let was_killed = killed || g.phase == Phase::Killed;
            g.phase = if was_killed {
                Phase::Killed
            } else if live.bundle_dir.is_none() {
                // An armed session that never recorded has nothing sealed.
                Phase::Idle
            } else {
                session::next(Phase::Stopping, Step::Sealed).unwrap_or(Phase::Sealed)
            };
            g.session = None;
            g.starting = false;
            if let Some(t) = live.token.clone() {
                g.last_ended = Some(Ended {
                    token: t,
                    killed: was_killed,
                });
            }
            if let Some(b) = &bundle {
                g.bundle = Some(b.clone());
            }
            if let Err(e) = &outcome {
                g.message = Some(e.message.clone());
            } else if !was_killed {
                g.message = None;
            }
        }
        self.audit(
            "session_ended",
            json!({"session": live.id, "killed": killed, "bundle": bundle.as_ref().map(|b| &b.name), "state": bundle.as_ref().map(|b| &b.state), "verified": bundle.as_ref().map(|b| b.verified)}),
        );
        self.publish();
        outcome.map(|()| bundle)
    }

    // ------------------------------------------------------------------ bundles

    pub fn bundle_reveal(&self, name: &str) -> Result<(), String> {
        let store = self.store();
        let dir = store.bundle_dir(name).ok_or("that is not a bundle name")?;
        if !dir.is_dir() {
            return Err("that bundle is not there".into());
        }
        let scope = PathScope::new([store.bundles_root()]).map_err(|e| e.to_string())?;
        reveal(&dir, &scope).map_err(|e| e.to_string())
    }

    pub fn bundle_discard(&self, name: &str) -> Result<(), String> {
        {
            let g = self.g();
            if g.session
                .as_ref()
                .and_then(|l| l.bundle_dir.as_ref())
                .and_then(|d| d.file_name())
                .is_some_and(|n| n.to_string_lossy() == name)
            {
                return Err("that bundle belongs to the active session".into());
            }
        }
        self.store().remove_bundle(name)?;
        {
            let mut g = self.g();
            if g.bundle.as_ref().is_some_and(|b| b.name == name) {
                g.bundle = None;
            }
        }
        self.audit("bundle_discarded", json!({"bundle": name}));
        self.publish();
        Ok(())
    }
}

/// The largest visible window of the consented app.
fn pick_source<'a>(sources: &'a [SourceInfo], platform_id: &str) -> Option<&'a SourceInfo> {
    sources
        .iter()
        .filter(|s| {
            s.app_id.eq_ignore_ascii_case(platform_id) && !s.minimized && s.w > 0.0 && s.h > 0.0
        })
        .max_by(|a, b| {
            (a.w * a.h)
                .partial_cmp(&(b.w * b.h))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Token parsing/replay refusal is checked below the OS driver: no real consent window,
    // recorder or process-wide kill is needed to exercise malformed & terminal credentials.
    #[test]
    fn wrong_ended_and_killed_session_tokens_have_distinct_errors() {
        let token = rightkit_credentials::Store::generate_key(32).unwrap();
        let mut inner = Inner {
            phase: Phase::Sealed,
            session: None,
            pending: None,
            last_ended: Some(Ended {
                token: token.clone(),
                killed: false,
            }),
            bundle: None,
            message: None,
            starting: false,
        };
        let wrong = inner
            .authorize("wrong-token")
            .err()
            .expect("wrong token refused");
        assert_eq!(wrong.code, "invalid_token");
        let ended = inner
            .authorize(token.expose_str().unwrap())
            .err()
            .expect("ended token refused");
        assert_eq!(ended.code, "session_ended");
        inner.phase = Phase::Killed;
        inner.last_ended.as_mut().unwrap().killed = true;
        let killed = inner
            .authorize(token.expose_str().unwrap())
            .err()
            .expect("killed token refused");
        assert_eq!(killed.code, "session_killed");
    }
}
