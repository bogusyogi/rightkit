//! The recorder worker: one thread that owns the `rightkit-record` backend and its `Recorder`.
//!
//! The backend (a Swift helper process on macOS, in-process capture on Windows) is built and used on
//! this thread only, so nothing here has to be `Send` beyond plain request and reply data. The same
//! thread is the pump timer (design, step 6): every 250 ms it drains the helper, and when a take ends
//! by itself (the target quit, the helper died) it reports the sealed outcome through `on_end`.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_child_process::AdoptedIdentity;
#[cfg(unix)]
use rightkit_child_process::AdoptedSignal;
use rightkit_contracts::types::record_event::{
    Denied, DenyReason, EventKind, InputKind, Phase as EventPhase, PolicyRef, Postcondition,
    RecordEvent,
};
use rightkit_contracts::types::record_session::{
    BundleStatus, Surface, SurfaceKind, VisibilityState,
};
use rightkit_contracts::Contract;
use rightkit_record::{
    PixelMasking, RecordError, Recorder, RedactionPolicy, ScenarioSpec, ScenarioStep, Source,
    StartRequest, SystemClock, TargetSpec, VerifyOptions,
};

#[cfg(target_os = "macos")]
type NativeBackend = rightkit_record::macos::MacBackend;
#[cfg(windows)]
type NativeBackend = rightkit_record::windows::WindowsBackend;

/// Step id every demo event carries. The scenario is one step: the agent's whole take.
pub const STEP_ID: &str = "agent-demo";
const PUMP_INTERVAL: Duration = Duration::from_millis(250);
/// A bundle that has not heartbeated for this long is sealed by the next `open` as a stale take.
const STALE_AFTER_MS: u64 = 30_000;

#[derive(Debug, Clone)]
pub struct RecErr {
    pub code: String,
    pub message: String,
}

impl RecErr {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn permission(&self) -> bool {
        self.code == "permission_denied"
    }
}

impl From<RecordError> for RecErr {
    fn from(e: RecordError) -> Self {
        RecErr::new(e.code.as_str(), e.message)
    }
}

/// One capturable window, as plain data.
#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub window_id: u64,
    pub pid: u32,
    pub app_id: String,
    pub title: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub minimized: bool,
}

pub struct StartSpec {
    pub app_id: String,
    pub window_id: u64,
    pub pid: u32,
    /// The bundle directory to create. It must not exist.
    pub out: PathBuf,
}

/// The sealed result of a take.
#[derive(Debug, Clone)]
pub struct Sealed {
    pub bundle_id: String,
    pub dir: PathBuf,
    pub status: BundleStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Verified {
    pub ok: bool,
    pub problems: Vec<String>,
}

/// Driver telemetry for the bundle's `events.ndjson`. Content-free: no typed text, no values.
pub enum EventSpec {
    Input {
        kind: InputKind,
        phase: EventPhase,
        effect_id: String,
        admitted: bool,
    },
    Stage {
        description: String,
    },
}

enum Cmd {
    Sources(Sender<Result<Vec<SourceInfo>, RecErr>>),
    Permission(Sender<Result<String, RecErr>>),
    Start(StartSpec, Sender<Result<(), RecErr>>),
    Event(EventSpec),
    Stop(Sender<Result<Sealed, RecErr>>),
    Verify(PathBuf, Vec<String>, Sender<Result<Verified, RecErr>>),
}

#[derive(Clone)]
pub struct RecorderHandle {
    /// Behind a mutex so a Windows abort can swap in a fresh worker (see `abort`).
    tx: Arc<Mutex<Sender<Cmd>>>,
    /// Starts a new worker thread and returns its queue.
    #[cfg_attr(not(windows), allow(dead_code))]
    respawn: Arc<dyn Fn() -> Sender<Cmd> + Send + Sync>,
    /// Identity of the live macOS capture helper. The worker publishes it so `abort`
    /// can reach the helper without going through the worker's queue (see `abort`).
    helper: Arc<Mutex<Option<AdoptedIdentity>>>,
}

fn dead() -> RecErr {
    RecErr::new("recorder_stopped", "the recorder worker is not running")
}

impl RecorderHandle {
    /// Starts the worker. `root` is the folder bundles live under; `on_end` is called from the worker
    /// when a take ends without a `stop` (the target quit, the helper died).
    pub fn spawn(root: PathBuf, on_end: Arc<dyn Fn(Sealed) + Send + Sync>) -> Self {
        let helper = Arc::new(Mutex::new(None));
        let identity = helper.clone();
        let respawn: Arc<dyn Fn() -> Sender<Cmd> + Send + Sync> = Arc::new(move || {
            let (tx, rx) = mpsc::channel();
            let (root, on_end, identity) = (root.clone(), on_end.clone(), identity.clone());
            let _ = std::thread::Builder::new()
                .name(format!("{}-demo-recorder", crate::identity().slug))
                .spawn(move || run(rx, root, on_end, identity));
            tx
        });
        let tx = Arc::new(Mutex::new(respawn()));
        Self {
            tx,
            respawn,
            helper,
        }
    }

    fn ask<T>(
        &self,
        make: impl FnOnce(Sender<Result<T, RecErr>>) -> Cmd,
        wait: Duration,
    ) -> Result<T, RecErr> {
        let (reply, answer) = mpsc::channel();
        let tx = self.tx.lock().map_err(|_| dead())?.clone();
        tx.send(make(reply)).map_err(|_| dead())?;
        answer
            .recv_timeout(wait)
            .map_err(|_| RecErr::new("recorder_timeout", "the recorder did not answer in time"))?
    }

    pub fn sources(&self) -> Result<Vec<SourceInfo>, RecErr> {
        self.ask(Cmd::Sources, Duration::from_secs(10))
    }

    /// The helper identity and screen-recording permission, as text for the Settings page.
    pub fn permission(&self) -> Result<String, RecErr> {
        self.ask(Cmd::Permission, Duration::from_secs(10))
    }

    pub fn start(&self, spec: StartSpec) -> Result<(), RecErr> {
        // `Backend::start` may show the OS Screen Recording prompt, which waits for a person.
        self.ask(|r| Cmd::Start(spec, r), Duration::from_secs(60))
    }

    pub fn event(&self, spec: EventSpec) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(Cmd::Event(spec));
        }
    }

    pub fn stop(&self, wait: Duration) -> Result<Sealed, RecErr> {
        self.ask(Cmd::Stop, wait)
    }

    /// Ends the helper without a clean finish. Not queued: a `stop` that hangs blocks the worker
    /// thread, so an abort sent through the same channel would wait behind it and never run. On macOS
    /// this kills the helper process directly, which fails the blocked `stop` and frees the worker.
    /// On Windows capture is in-process with no helper pid, so abort flips the process-wide kill
    /// switch and abandons the worker: the queue is swapped for a fresh worker (new backend), and the
    /// old thread, stuck inside the hung stop, is left holding the capture until the call returns,
    /// when it finds its queue closed and exits. Nothing waits on that thread.
    pub fn abort(&self) {
        #[cfg(windows)]
        {
            rightkit_automation::kill();
            if let Ok(mut tx) = self.tx.lock() {
                *tx = (self.respawn)();
            }
        }
        let helper = self.helper.lock().unwrap_or_else(|e| e.into_inner()).take();
        #[cfg(unix)]
        if let Some(helper) = helper {
            let _ = helper.signal(AdoptedSignal::Force);
        }
        #[cfg(not(unix))]
        let _ = helper;
    }

    /// `bundle_verify` with the plaintext values the agent typed as canaries.
    pub fn verify(&self, dir: PathBuf, forbidden: Vec<String>) -> Result<Verified, RecErr> {
        self.ask(|r| Cmd::Verify(dir, forbidden, r), Duration::from_secs(120))
    }
}

fn open_backend() -> Result<NativeBackend, String> {
    #[cfg(target_os = "macos")]
    {
        NativeBackend::launch_default().map_err(|e| format!("{}: {}", e.code.as_str(), e.message))
    }
    #[cfg(windows)]
    {
        Ok(NativeBackend::new())
    }
}

type Rec = Recorder<NativeBackend, SystemClock>;

struct Worker {
    rec: Result<Rec, String>,
    taking: bool,
    started: Instant,
    on_end: Arc<dyn Fn(Sealed) + Send + Sync>,
    helper: Arc<Mutex<Option<AdoptedIdentity>>>,
}

fn run(
    rx: Receiver<Cmd>,
    root: PathBuf,
    on_end: Arc<dyn Fn(Sealed) + Send + Sync>,
    helper: Arc<Mutex<Option<AdoptedIdentity>>>,
) {
    let _ = std::fs::create_dir_all(&root);
    let rec = open_backend().and_then(|backend| {
        #[cfg(target_os = "macos")]
        {
            let identity = std::num::NonZeroU32::new(backend.helper_pid())
                .map(AdoptedIdentity::capture)
                .transpose()
                .map_err(|e| format!("cannot identify the recorder helper: {e}"))?
                .flatten();
            *helper.lock().unwrap_or_else(|e| e.into_inner()) = identity;
        }
        Recorder::open(
            backend,
            SystemClock,
            std::slice::from_ref(&root),
            STALE_AFTER_MS,
        )
        .map(|(rec, _recovered)| rec)
        .map_err(|e| format!("{}: {}", e.code.as_str(), e.message))
    });
    let mut w = Worker {
        rec,
        taking: false,
        started: Instant::now(),
        on_end,
        helper,
    };
    loop {
        match rx.recv_timeout(PUMP_INTERVAL) {
            Ok(cmd) => w.handle(cmd),
            Err(RecvTimeoutError::Timeout) => w.tick(),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // The host is gone: never leave a capture running.
    if w.taking {
        if let Ok(rec) = w.rec.as_mut() {
            let _ = rec.record_stop();
        }
    }
}

impl Worker {
    fn rec(&mut self) -> Result<&mut Rec, RecErr> {
        self.rec
            .as_mut()
            .map_err(|e| RecErr::new("recorder_unavailable", e.clone()))
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Sources(reply) => {
                let r = self
                    .rec()
                    .and_then(|rec| rec.sources_list().map_err(RecErr::from));
                let _ = reply.send(r.map(|v| v.iter().map(info).collect()));
            }
            Cmd::Permission(reply) => {
                let r = self.rec().map(|rec| {
                    let d = rec.doctor();
                    format!(
                        "helper {} (signed: {}), screen recording {:?}",
                        d.helper.identity, d.helper.signed, d.permission
                    )
                });
                let _ = reply.send(r);
            }
            Cmd::Start(spec, reply) => {
                let r = self.start(spec);
                let _ = reply.send(r);
            }
            Cmd::Event(spec) => {
                if self.taking {
                    let event = self.event(spec);
                    if let Ok(rec) = self.rec.as_mut() {
                        // An event the bundle schema refuses is dropped; the host's audit log has it.
                        let _ = rec.append_event(event);
                    }
                }
            }
            Cmd::Stop(reply) => {
                let r = self.stop();
                let _ = reply.send(r);
            }
            Cmd::Verify(dir, forbidden, reply) => {
                let r = self.rec().and_then(|rec| {
                    rec.bundle_verify(
                        &dir,
                        &VerifyOptions {
                            forbidden_plaintext: forbidden,
                        },
                    )
                    .map(|report| Verified {
                        ok: report.ok(),
                        problems: report.problems,
                    })
                    .map_err(RecErr::from)
                });
                let _ = reply.send(r);
            }
        }
    }

    fn start(&mut self, spec: StartSpec) -> Result<(), RecErr> {
        let rec = self.rec()?;
        let visibility = rec
            .sources_list()?
            .into_iter()
            .find(|s| s.window_id == spec.window_id && s.pid == spec.pid)
            .map(|s| s.visibility)
            .unwrap_or(VisibilityState::OnScreenVisible);
        let target = TargetSpec {
            app_id: spec.app_id,
            window_id: spec.window_id,
            pid: spec.pid,
            surfaces: vec![Surface {
                surface_id: "main".into(),
                kind: SurfaceKind::MainWindow,
                parent_surface_id: None,
                native_id: spec.window_id.to_string(),
                supported: true,
                unsupported_reason: None,
            }],
        };
        let scenario = ScenarioSpec {
            scenario_id: "demo.agent".into(),
            steps: vec![ScenarioStep {
                step_id: STEP_ID.into(),
                label: "Agent demo".into(),
                say: None,
            }],
        };
        let mut request = StartRequest::new(target, scenario, spec.out);
        request.fps = 30;
        request.visibility = visibility;
        // The catalog declares each app capture-safe (catalog.rs); the cursor is never captured and
        // the strict redaction policy hashes target identity and omits typed text.
        request.masking = PixelMasking::CaptureSafeState;
        request.redaction = RedactionPolicy::strict();
        rec.record_start(request)?;
        self.taking = true;
        self.started = Instant::now();
        Ok(())
    }

    fn stop(&mut self) -> Result<Sealed, RecErr> {
        let rec = self.rec()?;
        let outcome = rec.record_stop();
        // The helper is reaped by the stop whatever it answered; its pid must not be killed later.
        self.helper.lock().unwrap_or_else(|e| e.into_inner()).take();
        let outcome = outcome?;
        self.taking = false;
        Ok(Sealed {
            bundle_id: outcome.bundle_id,
            dir: outcome.dir,
            status: outcome.status,
            reason: outcome.reason,
        })
    }

    fn tick(&mut self) {
        if !self.taking {
            return;
        }
        let Ok(rec) = self.rec.as_mut() else { return };
        match rec.record_status() {
            Ok(status) => {
                if let Some(outcome) = status.outcome {
                    self.taking = false;
                    self.helper.lock().unwrap_or_else(|e| e.into_inner()).take();
                    (self.on_end)(Sealed {
                        bundle_id: outcome.bundle_id,
                        dir: outcome.dir,
                        status: outcome.status,
                        reason: outcome.reason,
                    });
                }
            }
            Err(_) => self.taking = false,
        }
    }

    /// Host time for telemetry is the time since the take started. Mapping it onto the helper's frame
    /// clock is deferred (the recorder exposes no host clock to the caller), so the reader's
    /// event-to-frame resolution is not meaningful for these events yet.
    fn event(&self, spec: EventSpec) -> RecordEvent {
        let host_ns = self.started.elapsed().as_nanos() as u64;
        let policy = RedactionPolicy::strict();
        let mut e = RecordEvent {
            contract: "record-event".into(),
            contract_version: Contract::RecordEvent.supported_version().to_string(),
            session_id: String::new(),
            seq: 0,
            kind: EventKind::StageCheck,
            input_kind: None,
            phase: None,
            effect_id: None,
            sub_event_id: None,
            step_index: 0,
            step_id: Some(STEP_ID.into()),
            host_ns,
            timing: None,
            geometry_epoch: 0,
            dispatch: None,
            pointer: None,
            target: None,
            text: None,
            key: None,
            modifiers: Vec::new(),
            delta: None,
            admitted: None,
            denied: None,
            postcondition: None,
            frame: None,
            frame_dropped: None,
            intruders: Vec::new(),
            redaction_policy: PolicyRef {
                policy_id: policy.policy_id().to_string(),
                policy_version: policy.policy_version().to_string(),
            },
        };
        match spec {
            EventSpec::Input {
                kind,
                phase,
                effect_id,
                admitted,
            } => {
                e.kind = EventKind::Input;
                e.input_kind = Some(kind);
                e.phase = Some(phase);
                e.effect_id = Some(effect_id);
                e.admitted = Some(admitted);
                if !admitted {
                    e.denied = Some(Denied {
                        reason: DenyReason::PermissionDenied,
                    });
                }
            }
            EventSpec::Stage { description } => {
                e.postcondition = Some(Postcondition {
                    observed: true,
                    observed_ns: host_ns,
                    description,
                });
            }
        }
        e
    }
}

fn info(s: &Source) -> SourceInfo {
    SourceInfo {
        window_id: s.window_id,
        pid: s.pid,
        app_id: s.app_id.clone(),
        title: s.title.clone(),
        x: s.bounds_pt.x,
        y: s.bounds_pt.y,
        w: s.bounds_pt.width,
        h: s.bounds_pt.height,
        minimized: s.visibility == VisibilityState::Minimized,
    }
}
