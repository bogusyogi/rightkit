//! Client for the Orthic Labs shared speech service.
//!
//! One process per machine hosts the speech engines (Parakeet today, Whisper and
//! others by capability). Other apps discover it through `service.json`, connect
//! to its current-user-only endpoint and exchange line-delimited v1 `EngineFrame`
//! JSON restricted to health, capabilities, info and file transcription.
//!
//! Built on `rightkit-daemon` (discovery files, staleness, ranking/takeover
//! files, the user-only socket/pipe transport) and wire-compatible with
//! HeardRight's v1 host: an unchanged host accepts this client. Hosting stays in
//! HeardRight's engine. The API is a superset of `orthic-speech-client` 0.1, so
//! HeardRight/CutRight swap the dependency only. See `API.md`.

mod capture;
mod cues;
mod preflight;
mod protocol;
mod stage;

pub use capture::{
    decide as decide_capture, AcquireOutcome, CaptureArbiter, CaptureCommand, CaptureDecision,
    CaptureOwner, CapturePhase, CaptureReply, CaptureRole, CaptureStatus, HeartbeatOutcome,
    ReleaseOutcome, CAPTURE_HEARTBEAT_MS, CAPTURE_LEASE_MS, CAPTURE_RESERVATION_MS,
};
pub use cues::{parse_srt_cues, Cue};
pub use preflight::preflight_file;
pub use stage::{StageInput, DEFAULT_STAGE_LIMIT_BYTES};

use stage::{host_could_not_read, prepare, StageRoots, Staged};

use std::fmt;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_daemon::{HostPlan as ServicePlan, Suite};
use rightkit_sidecar::{read_json_line, FrameError};

pub use protocol::{
    endpoint_for, installed_heardright_path, is_allowed_schema, is_allowed_schema_for_minor,
    service_dir, service_record_path, takeover_request_path, Capabilities, EngineInfo,
    EngineSelector, ExtendedRequest, FileOptions, FileTranscript, FileTranscriptResult,
    HostProgress, HostRole, InstalledEngine, ProgressState, QueueReason, ServiceEnvelope,
    ServiceErrorPayload, ServiceRecord, ServiceRequest, TakeoverRequest, TimedWord,
    CAPABILITY_CANCEL, CAPABILITY_CAPTURE, CAPABILITY_FILE_TRANSCRIPTION, CAPABILITY_QUEUE,
    CAPABILITY_VERBATIM, ENGINE_PARAKEET, ENGINE_WHISPER, ERROR_ENGINE_UNAVAILABLE,
    ERROR_HOST_HANDOFF, ERROR_JOB_UNKNOWN, ERROR_NOT_ALLOWED, QUEUE_KEEPALIVE_MS,
    QUEUE_RESULT_RETENTION_MS, SCHEMA_CANCEL, SCHEMA_CAPTURE, SCHEMA_PROGRESS, SCHEMA_STATUS,
    SERVICE_ALLOWED_SCHEMAS, SERVICE_ALLOWED_SCHEMAS_CAPTURE, SERVICE_NAME, SERVICE_PROTOCOL_MAJOR,
    SERVICE_PROTOCOL_MINOR, SERVICE_PROTOCOL_MINOR_CAPTURE, SERVICE_PROTOCOL_MINOR_ENGINES,
    SERVICE_PROTOCOL_MINOR_QUEUE, SERVICE_PROTOCOL_MINOR_VERBATIM, SPEECH_FAMILY,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const TRACE_ID: &str = "rightkit-speech-client";
const MAX_ATTEMPTS: u32 = 3;
const FRAME_LIMIT: usize = 16 * 1024 * 1024;
const HANDOFF_REDISCOVERY: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// No live service; the caller must host (or start HeardRight).
    NoService,
    /// The host is handing off to a higher-ranked host; re-discover and resubmit.
    HandoffRetry,
    ProtocolMismatch {
        major: u32,
    },
    Engine {
        code: String,
        message: String,
    },
    /// No frame (result or progress) arrived within the stall timeout.
    Timeout,
    Io(String),
    /// The caller's [`CancelToken`] fired and the connection was closed. The service may
    /// still finish the job: the wire has no cancel message. [`cancel_request`] reports
    /// what the service did.
    Cancelled,
    /// The host's protocol minor cannot honour the request (for example Whisper on a v1.0 host).
    Unsupported(String),
    /// Minor-3 host lost: connection closed, pid dead, or `service.json` withdrawn or
    /// replaced. Re-discover and resubmit with the same request id (idempotent).
    HostGone,
    /// [`Launch::Never`] found no live service host. Nothing was launched and no takeover
    /// was requested.
    NoLiveHost,
    /// The caller's [`SubmitGate`] refused the request after the host was connected. Nothing
    /// was submitted.
    Refused {
        reason: String,
    },
    /// The host could not read the input file: its own read check or FFmpeg was denied.
    /// Carries the path the caller passed, not a staged copy. Returned in place of the generic
    /// `Engine` error for that text. Added in 0.3.4; `ClientError` is not `#[non_exhaustive]`,
    /// so exhaustive matches need an arm.
    InputUnreadable {
        path: PathBuf,
        message: String,
    },
    /// The input file is larger than the staging limit. Nothing was staged or submitted.
    StageLimitExceeded {
        path: PathBuf,
        bytes: u64,
        limit: u64,
    },
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoService => write!(f, "no speech service is running"),
            Self::HandoffRetry => write!(f, "speech service host is changing; retry"),
            Self::ProtocolMismatch { major } => {
                write!(f, "speech service protocol major {major} is not supported")
            }
            Self::Engine { code, message } => write!(f, "speech service error {code}: {message}"),
            Self::Timeout => write!(f, "speech service timed out"),
            Self::Io(m) => write!(f, "speech service I/O error: {m}"),
            Self::Cancelled => write!(f, "speech request cancelled"),
            Self::Unsupported(m) => write!(f, "speech service cannot serve this request: {m}"),
            Self::HostGone => write!(f, "speech service host went away; resubmit"),
            Self::NoLiveHost => write!(f, "no speech service is live and launching is disabled"),
            Self::Refused { reason } => write!(f, "speech request refused: {reason}"),
            Self::InputUnreadable { path, message } => write!(
                f,
                "speech host could not read {}: {message}",
                path.display()
            ),
            Self::StageLimitExceeded { path, bytes, limit } => write!(
                f,
                "{} is {bytes} bytes, over the {limit}-byte staging limit",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ClientError {}

fn io_err(e: impl fmt::Display) -> ClientError {
    ClientError::Io(e.to_string())
}

// ---------------------------------------------------------------------------
// Suite root
// ---------------------------------------------------------------------------

/// The suite root by the shared convention: `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR`,
/// `ORTHIC_SHARED_DIR`, else `<app data>/Orthic Labs/shared`. Errors rather than
/// falling back to the working directory. Callers that already own a suite-root
/// authority (a model store) pass that instead.
pub fn default_suite_root() -> Result<PathBuf, ClientError> {
    Suite::resolve()
        .map(|s| s.root().to_path_buf())
        .map_err(io_err)
}

fn suite_at(root: &Path) -> Option<Suite> {
    Suite::at(root).ok()
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Is process `pid` alive (the staleness rule for `service.json`)?
pub fn pid_alive(pid: u32) -> bool {
    rightkit_daemon::pid_alive(pid)
}

/// The service record, only when it parses as a HeardRight v1 record and its
/// owner process is alive.
pub fn read_live_record(suite_root: &Path) -> Option<ServiceRecord> {
    let r = rightkit_daemon::read_live_record(&suite_at(suite_root)?, SERVICE_NAME)?;
    Some(ServiceRecord {
        protocol_major: r.protocol_major,
        protocol_minor: r.protocol_minor,
        role: HostRole::from_wire(&r.role)?,
        pid: r.pid,
        endpoint: r.endpoint.clone(),
        engine_version: r.host_version().to_string(),
    })
}

fn read_installed(suite_root: &Path) -> Option<InstalledEngine> {
    serde_json::from_slice(&std::fs::read(installed_heardright_path(suite_root)).ok()?).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPlan {
    UseRunning,
    StartInstalledHeardRight(PathBuf),
    HostMyself,
    TakeOver { from: HostRole },
}

/// Decide how the calling app (`my_role`) gets a speech host. The rank rules and the
/// takeover handling are `rightkit_daemon::plan_host`; this maps its answer onto the
/// speech vocabulary. HeardRight roles host themselves and never start another copy.
pub fn plan_host(suite_root: &Path, my_role: HostRole) -> HostPlan {
    let installed = if my_role.rank() <= HostRole::HeardRightHeadless.rank() {
        None
    } else {
        read_installed(suite_root).map(|i| i.engine_path)
    };
    let Some(suite) = suite_at(suite_root) else {
        return HostPlan::HostMyself;
    };
    match rightkit_daemon::plan_host(
        &suite,
        SERVICE_NAME,
        my_role.rank(),
        &HostRole::rank_table(),
        installed.as_deref(),
    ) {
        ServicePlan::UseRunning => HostPlan::UseRunning,
        // `from_role` is a name from the rank table, so it always maps back to a `HostRole`.
        ServicePlan::TakeOver { from_role } => match HostRole::from_wire(&from_role) {
            Some(from) => HostPlan::TakeOver { from },
            None => HostPlan::UseRunning,
        },
        ServicePlan::HostMyself => HostPlan::HostMyself,
        ServicePlan::StartInstalled(engine) => HostPlan::StartInstalledHeardRight(engine),
    }
}

/// Takeover is requested only against a live, lower-ranked host.
pub fn should_request_takeover(our: HostRole, record: Option<&ServiceRecord>) -> bool {
    rightkit_daemon::should_request_takeover(
        our.rank(),
        record.map(|r| (r.role.rank(), r.pid)),
        std::process::id(),
    )
}

/// A takeover request that obliges a host running as `ours` to yield.
pub fn takeover_obliges_yield(
    ours: HostRole,
    request: Option<&TakeoverRequest>,
    own_pid: u32,
    alive: &dyn Fn(u32) -> bool,
) -> bool {
    rightkit_daemon::takeover_obliges_yield(
        ours.rank(),
        request.map(|r| (r.role.rank(), r.pid)),
        own_pid,
        alive,
    )
}

/// The pending takeover request, if it parses and names a known role.
pub fn read_takeover(suite_root: &Path) -> Option<TakeoverRequest> {
    let t = rightkit_daemon::read_takeover(&suite_at(suite_root)?, SERVICE_NAME)?;
    Some(TakeoverRequest {
        role: HostRole::from_wire(&t.role)?,
        pid: t.pid,
        requested_at_ms: t.requested_at_ms,
    })
}

/// Publish `takeover.json` for this process as `role`. The file carries HeardRight's
/// fields plus `rank`, which HeardRight ignores.
pub fn request_takeover(suite_root: &Path, role: HostRole) -> std::io::Result<()> {
    let suite = suite_at(suite_root).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "suite root must be absolute",
        )
    })?;
    rightkit_daemon::publish_takeover(&suite, SERVICE_NAME, role.as_str(), role.rank())
}

/// Remove `takeover.json` only if this process wrote it.
pub fn clear_takeover_if_ours(suite_root: &Path) {
    if let Some(s) = suite_at(suite_root) {
        rightkit_daemon::clear_takeover_if_owner(&s, SERVICE_NAME, std::process::id());
    }
}

/// How to launch an installed engine as a speech host. The caller supplies it: the arguments
/// and environment are that engine's own contract, not this crate's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineLaunch {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl EngineLaunch {
    /// Launch with `args` and no extra environment.
    pub fn new<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            env: Vec::new(),
        }
    }

    /// Add one environment variable for the launched engine.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// HeardRight's installed-engine launch: `--speech-service` on the Parakeet backend
    /// (the engine takes the shared model lease only there). The default for [`plan_host`]'s
    /// installed-engine path.
    pub fn heardright() -> Self {
        Self::new(["--speech-service"]).env("HR_ASR_BACKEND", "parakeet-tdt")
    }
}

/// Spawn `engine_path` detached with the caller's `launch`, then wait up to `wait` for a live
/// `service.json`.
pub fn start_installed_engine(
    suite_root: &Path,
    engine_path: &Path,
    launch: &EngineLaunch,
    wait: Duration,
) -> Result<(), ClientError> {
    rightkit_daemon::spawn_detached(engine_path, &launch.args, &launch.env).map_err(io_err)?;
    if rightkit_daemon::poll_until(wait, || read_live_record(suite_root).is_some()) {
        Ok(())
    } else {
        Err(ClientError::Timeout)
    }
}

/// Spawn `<engine_path> --speech-service` detached, then wait up to `wait` for a live
/// `service.json`. Deprecated: call [`start_installed_engine`] with an explicit [`EngineLaunch`].
#[deprecated(
    since = "0.3.1",
    note = "use start_installed_engine with an EngineLaunch; this is HeardRight's launch preset"
)]
pub fn start_installed_heardright(
    suite_root: &Path,
    engine_path: &Path,
    wait: Duration,
) -> Result<(), ClientError> {
    start_installed_engine(suite_root, engine_path, &EngineLaunch::heardright(), wait)
}

// ---------------------------------------------------------------------------
// Launch policy
// ---------------------------------------------------------------------------

/// Whether a transcription call may start a speech host when none is live.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Launch {
    /// Start an installed HeardRight host when no service is live, as
    /// [`transcribe_via_service_with`] always has. The default.
    #[default]
    Auto,
    /// Use a live host as found, whatever its role. Never launch and never request a
    /// takeover. With no live host the call returns [`ClientError::NoLiveHost`] and does
    /// nothing else.
    Never,
}

/// Starts an installed speech host when a call finds none live. Implement it to observe
/// or replace the launch. [`HeardRightLauncher`] is the default.
pub trait HostLauncher {
    /// Start `engine_path` as a speech host and return once a live `service.json` appears.
    /// Any error makes the call return [`ClientError::NoService`].
    fn launch(&self, suite_root: &Path, engine_path: &Path) -> Result<(), ClientError>;
}

/// The default [`HostLauncher`]: starts the installed HeardRight engine detached with
/// [`EngineLaunch::heardright`] and waits up to 60 s for a live record. Errors: `Timeout`
/// or `Io` from [`start_installed_engine`].
#[derive(Debug, Clone, Copy, Default)]
pub struct HeardRightLauncher;

impl HostLauncher for HeardRightLauncher {
    fn launch(&self, suite_root: &Path, engine_path: &Path) -> Result<(), ClientError> {
        start_installed_engine(
            suite_root,
            engine_path,
            &EngineLaunch::heardright(),
            Duration::from_secs(60),
        )
    }
}

// ---------------------------------------------------------------------------
// Progress, cancel, options
// ---------------------------------------------------------------------------

/// Cooperative cancel. Clone it into the thread that may cancel; the request
/// returns [`ClientError::Cancelled`] within ~100 ms and its connection is closed.
/// Closing the connection is not a cancel on the wire. HeardRight's queue host does
/// not stop a job when its client disconnects (its dispatcher passes a cancel flag
/// that nothing sets), so the job runs to completion and its result is kept for
/// re-attach. [`cancel_request`] reports what the service did.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// What the client reports while a request runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    StartingHost,
    Connecting {
        attempt: u32,
    },
    Submitted,
    /// Heartbeat about once a second. `idle` is the time since the last frame from
    /// the host; the stall timeout fires when it reaches the configured limit.
    Waiting {
        elapsed: Duration,
        idle: Duration,
    },
    /// A minor-1/2 host's own progress frame (minor-3 hosts report `Queued`/`Running`).
    Host(HostProgress),
    /// Minor 3: the job is waiting; `position` 0 = next.
    Queued {
        job_id: String,
        position: u32,
        reason: QueueReason,
    },
    /// Minor 3: the job is decoding.
    Running {
        job_id: String,
        fraction: Option<f32>,
    },
    /// Minor 3: the host went away; re-discovering and resubmitting the same job.
    Reconnecting {
        attempt: u32,
    },
    HandoffRetry {
        attempt: u32,
    },
}

pub type ProgressFn = Box<dyn FnMut(&Progress) + Send>;

/// What a [`SubmitGate`] sees about the connected host.
#[derive(Debug, Clone, Copy)]
pub struct HostView<'a> {
    pub role: HostRole,
    pub engine_version: &'a str,
    pub protocol_minor: u32,
    /// `None` for a host that predates engine selection (protocol minor 0), which is never asked.
    pub capabilities: Option<&'a Capabilities>,
}

/// A caller's check on the connected host. [`SpeechClient::transcribe`] runs it after the host is
/// connected and its capabilities are known, and before the request is submitted. `Err(reason)`
/// refuses the request: the call returns [`ClientError::Refused`] and nothing is sent.
pub type SubmitGate = Box<dyn Fn(&HostView<'_>) -> Result<(), String> + Send>;

/// Options for [`transcribe_via_service_with`] and [`SpeechClient::transcribe`].
pub struct TranscribeOptions {
    /// `None` = send no selector: the host's default (Parakeet on a v1.0 host).
    pub engine: Option<EngineSelector>,
    /// Language hint (`en`, `es`, ...). Used for routing and passed to the engine on minor-1 hosts.
    pub language: Option<String>,
    /// Abort when no frame arrives for this long. Not a total limit: a minor-1 host's
    /// progress frames (including `paused_for_dictation`) restart it. A v1.0 host
    /// sends no frames while working, so there it bounds the whole job; keep it generous.
    pub stall_timeout: Duration,
    pub cancel: Option<CancelToken>,
    pub on_progress: Option<ProgressFn>,
    /// Idempotency key (= job id on a minor-3 host). `None`: the client picks one
    /// and keeps it across reconnects. Set it to re-attach after your own restart.
    pub request_id: Option<String>,
    /// Require raw words with no filler removal (editing apps). Refused with
    /// [`ClientError::Unsupported`] unless the host advertises verbatim.
    pub verbatim: bool,
    /// Checked before submit; see [`SubmitGate`] and [`TranscribeOptions::submit_gate`].
    pub(crate) submit_gate: Option<SubmitGate>,
    /// See [`TranscribeOptions::running_host_only`].
    pub(crate) running_host_only: bool,
    /// See [`TranscribeOptions::stage`].
    pub(crate) stage: StageInput,
    /// See [`TranscribeOptions::stage_limit`].
    pub(crate) stage_limit: u64,
}

impl TranscribeOptions {
    pub fn new(stall_timeout: Duration) -> Self {
        Self {
            engine: None,
            language: None,
            stall_timeout,
            cancel: None,
            on_progress: None,
            request_id: None,
            verbatim: false,
            submit_gate: None,
            running_host_only: false,
            stage: StageInput::Auto,
            stage_limit: DEFAULT_STAGE_LIMIT_BYTES,
        }
    }
    /// Choose when the input file is copied to a staging file before submit. The default
    /// [`StageInput::Auto`] stages a path the host cannot be expected to read. [`StageInput::Never`]
    /// submits the path as given. See `API.md`, "Input contract".
    pub fn stage(mut self, mode: StageInput) -> Self {
        self.stage = mode;
        self
    }
    /// The largest file, in bytes, that may be staged (default [`DEFAULT_STAGE_LIMIT_BYTES`]).
    /// A larger file returns [`ClientError::StageLimitExceeded`] and nothing is staged.
    pub fn stage_limit(mut self, bytes: u64) -> Self {
        self.stage_limit = bytes;
        self
    }
    /// Run `gate` on the connected host before the request is submitted. It sees the host's role,
    /// engine version, protocol minor and (from minor 1) its capabilities. A refusal returns
    /// [`ClientError::Refused`] with the gate's reason. Applies on every attempt, so a
    /// hand-off or a new host is checked again before any resubmit.
    pub fn submit_gate(
        mut self,
        gate: impl Fn(&HostView<'_>) -> Result<(), String> + Send + 'static,
    ) -> Self {
        self.submit_gate = Some(Box::new(gate));
        self
    }
    /// Use the running host only: never re-discover, relaunch, or resend after a hand-off or a
    /// host-gone event. Such an event returns its error ([`ClientError::HandoffRetry`] or
    /// [`ClientError::HostGone`]) at once. Pair with [`Launch::Never`] for a strictly
    /// no-launch call. Default: off, so the call retries as before.
    pub fn running_host_only(mut self) -> Self {
        self.running_host_only = true;
        self
    }
    pub fn request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }
    pub fn verbatim(mut self) -> Self {
        self.verbatim = true;
        self
    }
    pub fn engine(mut self, engine: EngineSelector) -> Self {
        self.engine = Some(engine);
        self
    }
    pub fn language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }
    pub fn cancel_token(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }
    pub fn on_progress(mut self, f: impl FnMut(&Progress) + Send + 'static) -> Self {
        self.on_progress = Some(Box::new(f));
        self
    }
    fn report(&mut self, p: Progress) {
        if let Some(f) = self.on_progress.as_mut() {
            f(&p);
        }
    }
    fn cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(CancelToken::is_cancelled)
    }
}

impl Default for TranscribeOptions {
    fn default() -> Self {
        Self::new(Duration::from_secs(120))
    }
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_request_id() -> String {
    format!(
        "orthic-{}-{}",
        std::process::id(),
        REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn encode_frame(frame: &ServiceEnvelope) -> Result<String, ClientError> {
    let mut line = serde_json::to_string(frame).map_err(io_err)?;
    line.push('\n');
    Ok(line)
}

/// The error a transcription reply carries. A host read denial becomes [`ClientError::InputUnreadable`]
/// with the caller's path; everything else maps as [`map_error`] does.
fn input_error(error: &ServiceErrorPayload, path: &Path) -> ClientError {
    if host_could_not_read(error) {
        ClientError::InputUnreadable {
            path: path.to_path_buf(),
            message: error.message.clone(),
        }
    } else {
        map_error(error)
    }
}

/// The path to put on the wire: the staged copy when there is one, else the caller's path.
fn submit_path<'a>(staged: &'a Option<Staged>, path: &'a Path) -> &'a Path {
    match staged {
        Some(copy) => copy.path(),
        None => path,
    }
}

fn map_error(error: &ServiceErrorPayload) -> ClientError {
    if error.code == ERROR_HOST_HANDOFF {
        ClientError::HandoffRetry
    } else {
        ClientError::Engine {
            code: error.code.clone(),
            message: error.message.clone(),
        }
    }
}

enum Incoming {
    Frame(Box<ServiceEnvelope>),
    Closed,
    Bad(String),
}

pub struct SpeechClient {
    writer: Box<dyn Write + Send>,
    rx: Receiver<Incoming>,
    abort: Arc<dyn Fn() + Send + Sync>,
    record: ServiceRecord,
    suite_root: PathBuf,
    caps: Option<Capabilities>,
    dead: bool,
}

/// How [`SpeechClient`] decides a request is dead.
#[derive(Clone, Copy)]
enum Wait {
    /// No frame for this long (v1.0-2 hosts).
    Stall(Duration),
    /// Minor 3: only the transport and the host's pid / `service.json` decide.
    HostLiveness,
}

/// A minor-3 job's state from [`SpeechClient::job_status`].
#[derive(Debug, Clone, PartialEq)]
pub enum JobStatus {
    Active(HostProgress),
    Done(FileTranscriptResult),
    Unknown,
}

impl Drop for SpeechClient {
    fn drop(&mut self) {
        (self.abort)();
    }
}

impl SpeechClient {
    pub fn role(&self) -> HostRole {
        self.record.role
    }

    pub fn engine_version(&self) -> &str {
        &self.record.engine_version
    }

    /// The host's `service.json` `protocol_minor`.
    pub fn protocol_minor(&self) -> u32 {
        self.record.protocol_minor
    }

    /// Whether the host understands minor-1 `engine`/`language` and sends progress.
    pub fn supports_engine_selection(&self) -> bool {
        self.record.protocol_minor >= SERVICE_PROTOCOL_MINOR_ENGINES
    }

    fn kill(&mut self) {
        self.dead = true;
        (self.abort)();
    }

    /// Send one frame and wait for its response (or any error frame), restarting
    /// the stall clock on every frame received. Low level; the transcribe/capabilities
    /// methods use it.
    pub fn call_raw(
        &mut self,
        frame: &ServiceEnvelope,
        stall: Duration,
        cancel: Option<&CancelToken>,
        report: Option<&mut dyn FnMut(&Progress)>,
    ) -> Result<ServiceEnvelope, ClientError> {
        self.call_inner(frame, Wait::Stall(stall), cancel, report)
    }

    /// Write one frame and do not wait for a reply.
    fn send_frame(&mut self, frame: &ServiceEnvelope) -> Result<(), ClientError> {
        if self.dead {
            return Err(ClientError::Io("connection is no longer usable".into()));
        }
        let line = encode_frame(frame)?;
        if let Err(e) = self
            .writer
            .write_all(line.as_bytes())
            .and_then(|_| self.writer.flush())
        {
            self.kill();
            return Err(io_err(e));
        }
        Ok(())
    }

    /// Minor 4, known without a round trip: the host's capabilities were fetched on this
    /// connection and list the cancel frame.
    fn cancel_frame_known(&self) -> bool {
        self.record.protocol_minor >= SERVICE_PROTOCOL_MINOR_CAPTURE
            && self
                .caps
                .as_ref()
                .is_some_and(Capabilities::supports_cancel)
    }

    /// Minor 4, known without a round trip: the host lists capture ownership.
    fn capture_known(&self) -> bool {
        self.record.protocol_minor >= SERVICE_PROTOCOL_MINOR_CAPTURE
            && self
                .caps
                .as_ref()
                .is_some_and(Capabilities::supports_capture)
    }

    /// The host accepts a cancel frame (protocol minor 4 and the `request_cancel` capability).
    pub fn supports_cancel(&mut self) -> Result<bool, ClientError> {
        if self.record.protocol_minor < SERVICE_PROTOCOL_MINOR_CAPTURE {
            return Ok(false);
        }
        Ok(self.cached_capabilities()?.supports_cancel())
    }

    /// The host arbitrates capture ownership (protocol minor 4 and the `capture_ownership` capability).
    pub fn supports_capture(&mut self) -> Result<bool, ClientError> {
        if self.record.protocol_minor < SERVICE_PROTOCOL_MINOR_CAPTURE {
            return Ok(false);
        }
        Ok(self.cached_capabilities()?.supports_capture())
    }

    /// Ask the host to stop `job_id`, from this or another connection. Fire and forget: the
    /// frame is written and nothing is awaited. `Unsupported` when the host lacks the capability
    /// (an old host never sees the frame).
    pub fn cancel_job(&mut self, job_id: &str) -> Result<(), ClientError> {
        if !self.supports_cancel()? {
            return Err(ClientError::Unsupported(
                "host does not advertise the cancel frame".into(),
            ));
        }
        let frame = ServiceEnvelope::cancel_job(&next_request_id(), TRACE_ID, job_id);
        self.send_frame(&frame)
    }

    /// Send one capture command and return the host's `capture_state` reply. `timeout` bounds the wait.
    /// `Unsupported` when the host lacks the capability.
    pub fn capture(
        &mut self,
        command: &CaptureCommand,
        timeout: Duration,
    ) -> Result<CaptureReply, ClientError> {
        if !self.supports_capture()? {
            return Err(ClientError::Unsupported(
                "host does not advertise capture ownership".into(),
            ));
        }
        let frame = ServiceEnvelope::capture_request(&next_request_id(), TRACE_ID, command);
        let response = self.call_raw(&frame, timeout, None, None)?;
        if let Some(error) = &response.error {
            return Err(map_error(error));
        }
        response
            .capture_reply()
            .ok_or_else(|| ClientError::Io("unexpected capture response".into()))
    }

    /// Local Stop. Writes a `capture_cancel` frame when the host is known to arbitrate capture
    /// (best effort, never waited on), then closes the connection. Capture and pending uploads
    /// are closed here without waiting for a server acknowledgement; the closed connection also
    /// tells a host that missed the frame. The client is unusable afterwards.
    pub fn stop_capture(&mut self, owner_id: &str) {
        if self.capture_known() {
            let frame = ServiceEnvelope::capture_request(
                &next_request_id(),
                TRACE_ID,
                &CaptureCommand::Cancel {
                    owner_id: owner_id.to_string(),
                },
            );
            let _ = self.send_frame(&frame);
        }
        self.kill();
    }

    /// Whether the host this client connected to is still the live service host.
    fn host_still_live(&self) -> bool {
        read_live_record(&self.suite_root).is_some_and(|r| r.pid == self.record.pid)
    }

    fn call_inner(
        &mut self,
        frame: &ServiceEnvelope,
        wait: Wait,
        cancel: Option<&CancelToken>,
        mut report: Option<&mut dyn FnMut(&Progress)>,
    ) -> Result<ServiceEnvelope, ClientError> {
        if self.dead {
            return Err(ClientError::Io("connection is no longer usable".into()));
        }
        let line = encode_frame(frame)?;
        if let Err(e) = self
            .writer
            .write_all(line.as_bytes())
            .and_then(|_| self.writer.flush())
        {
            self.kill();
            return Err(io_err(e));
        }
        let (started, mut last_frame, mut last_beat) =
            (Instant::now(), Instant::now(), Instant::now());
        if let Some(r) = report.as_deref_mut() {
            r(&Progress::Submitted);
        }
        loop {
            if cancel.is_some_and(CancelToken::is_cancelled) {
                // A host that lists the cancel capability is told to stop the job. This is
                // best effort and never waited on: Stop closes the connection at once.
                if frame.schema_name == "file_transcription_request" && self.cancel_frame_known() {
                    let cancel_frame = ServiceEnvelope::cancel_job(
                        &next_request_id(),
                        TRACE_ID,
                        &frame.request_id,
                    );
                    let _ = self.send_frame(&cancel_frame);
                }
                self.kill();
                return Err(ClientError::Cancelled);
            }
            let idle = last_frame.elapsed();
            let poll = match wait {
                Wait::Stall(stall) if idle >= stall => {
                    self.kill();
                    return Err(ClientError::Timeout);
                }
                Wait::Stall(stall) => (stall - idle).min(Duration::from_millis(100)),
                Wait::HostLiveness => Duration::from_millis(100),
            };
            match self.rx.recv_timeout(poll) {
                Ok(Incoming::Frame(f)) => {
                    last_frame = Instant::now();
                    if f.error.is_some() {
                        return Ok(*f);
                    }
                    if f.request_id == frame.request_id {
                        if frame.schema_name != SCHEMA_STATUS {
                            if let Some(p) = f.host_progress() {
                                if let Some(r) = report.as_deref_mut() {
                                    r(&queue_progress(p, matches!(wait, Wait::HostLiveness)));
                                }
                                continue;
                            }
                        }
                        return Ok(*f);
                    }
                }
                Ok(Incoming::Closed) | Err(RecvTimeoutError::Disconnected) => {
                    self.kill();
                    if matches!(wait, Wait::HostLiveness) {
                        return Err(ClientError::HostGone);
                    }
                    return Err(ClientError::Io(
                        "speech service closed the connection".into(),
                    ));
                }
                Ok(Incoming::Bad(m)) => {
                    self.kill();
                    return Err(ClientError::Io(format!("bad frame: {m}")));
                }
                Err(RecvTimeoutError::Timeout) => {
                    if last_beat.elapsed() >= Duration::from_secs(1) {
                        last_beat = Instant::now();
                        if matches!(wait, Wait::HostLiveness) && !self.host_still_live() {
                            self.kill();
                            return Err(ClientError::HostGone);
                        }
                        if let Some(r) = report.as_deref_mut() {
                            r(&Progress::Waiting {
                                elapsed: started.elapsed(),
                                idle: last_frame.elapsed(),
                            });
                        }
                    }
                }
            }
        }
    }

    fn handshake(&mut self, timeout: Duration) -> Result<(), ClientError> {
        let frame = ServiceEnvelope::health(&next_request_id(), TRACE_ID);
        let response = self.call_raw(&frame, timeout, None, None)?;
        if let Some(error) = &response.error {
            return Err(map_error(error));
        }
        if response.schema_name != "engine_health" || response.health_status().is_none() {
            return Err(ClientError::Io("unexpected handshake response".into()));
        }
        if response.health_unavailable() {
            Err(ClientError::Engine {
                code: "unavailable".into(),
                message: "speech engine reports unavailable".into(),
            })
        } else {
            Ok(())
        }
    }

    /// The host's engines, languages and readiness. A v1.0 host reports Parakeet only.
    pub fn capabilities(&mut self, timeout: Duration) -> Result<Capabilities, ClientError> {
        let frame = ServiceEnvelope::capabilities(&next_request_id(), TRACE_ID);
        let response = self.call_raw(&frame, timeout, None, None)?;
        if let Some(error) = &response.error {
            return Err(map_error(error));
        }
        let caps = response
            .capabilities_info()
            .ok_or_else(|| ClientError::Io("unexpected capabilities response".into()))?;
        self.caps = Some(caps.clone());
        Ok(caps)
    }

    /// Capabilities, asked once per connection (a v1.0 host is never asked: it has none of the extras).
    fn cached_capabilities(&mut self) -> Result<Capabilities, ClientError> {
        match &self.caps {
            Some(c) => Ok(c.clone()),
            None => self.capabilities(HANDSHAKE_TIMEOUT),
        }
    }

    /// The host implements the minor-3 queue contract: no stall timeout, typed queue progress.
    pub fn supports_queue(&mut self) -> Result<bool, ClientError> {
        if !self.supports_engine_selection() {
            return Ok(false);
        }
        Ok(self.cached_capabilities()?.supports_queue())
    }

    /// Minor 3: one-shot state of `job_id` (a request id this or an earlier client submitted).
    pub fn job_status(&mut self, job_id: &str) -> Result<JobStatus, ClientError> {
        match self.status_within(job_id, HANDSHAKE_TIMEOUT)? {
            RawStatus::Active(p) => Ok(JobStatus::Active(p)),
            RawStatus::Done(r) => Ok(JobStatus::Done(r)),
            RawStatus::Unknown => Ok(JobStatus::Unknown),
            RawStatus::Failed(e) => Err(map_error(&e)),
        }
    }

    /// The status of `job_id`. Each wire reply is bounded by `timeout`, and the capabilities
    /// call (made once per connection) is too.
    fn status_within(&mut self, job_id: &str, timeout: Duration) -> Result<RawStatus, ClientError> {
        if !self.supports_engine_selection() {
            return Err(unqueued());
        }
        let caps = match &self.caps {
            Some(c) => c.clone(),
            None => self.capabilities(timeout)?,
        };
        if !caps.supports_queue() {
            return Err(unqueued());
        }
        let frame = ServiceEnvelope::status_request(&next_request_id(), TRACE_ID, job_id);
        let response = self.call_raw(&frame, timeout, None, None)?;
        parse_status(&response)
    }

    /// Transcribe with the host's default engine (parity with `orthic-speech-client`).
    /// `timeout` is the stall timeout.
    pub fn transcribe_file(
        &mut self,
        path: &Path,
        timeout: Duration,
    ) -> Result<FileTranscript, ClientError> {
        self.transcribe(path, &mut TranscribeOptions::new(timeout))
            .map(|r| r.transcript)
    }

    /// Transcribe with engine/language selection, progress, cancel and stall timeout.
    pub fn transcribe(
        &mut self,
        path: &Path,
        opts: &mut TranscribeOptions,
    ) -> Result<FileTranscriptResult, ClientError> {
        self.transcribe_staged(path, opts, &StageRoots::system())
    }

    /// [`transcribe`](Self::transcribe) with the staging roots given. The staged copy, if any,
    /// lives until this call returns.
    fn transcribe_staged(
        &mut self,
        path: &Path,
        opts: &mut TranscribeOptions,
        roots: &StageRoots,
    ) -> Result<FileTranscriptResult, ClientError> {
        let staged = prepare(path, opts.stage, opts.stage_limit, roots)?;
        let submit = submit_path(&staged, path);
        self.submit_file(submit, path, opts)
    }

    /// Submit `submit` (the staged copy or `original`) and map the reply. `original` is the
    /// caller's path, which errors name.
    fn submit_file(
        &mut self,
        submit: &Path,
        original: &Path,
        opts: &mut TranscribeOptions,
    ) -> Result<FileTranscriptResult, ClientError> {
        let mut wire = self.wire_options(opts)?;
        let queue = self.supports_queue()?;
        if opts.verbatim {
            if !self.supports_engine_selection() || !self.cached_capabilities()?.supports_verbatim()
            {
                return Err(ClientError::Unsupported(
                    "host does not advertise verbatim file transcription; it would strip fillers"
                        .into(),
                ));
            }
            wire.verbatim = true;
        }
        if let Some(gate) = opts.submit_gate.as_ref() {
            let capabilities = if self.supports_engine_selection() {
                Some(self.cached_capabilities()?)
            } else {
                None
            };
            let view = HostView {
                role: self.role(),
                engine_version: self.engine_version(),
                protocol_minor: self.protocol_minor(),
                capabilities: capabilities.as_ref(),
            };
            gate(&view).map_err(|reason| ClientError::Refused { reason })?;
        }
        let request_id = opts.request_id.clone().unwrap_or_else(next_request_id);
        let frame = ServiceEnvelope::file_request_with(
            &request_id,
            TRACE_ID,
            submit.to_string_lossy().into_owned(),
            &wire,
        );
        let wait = if queue {
            Wait::HostLiveness
        } else {
            Wait::Stall(opts.stall_timeout)
        };
        let cancel = opts.cancel.clone();
        let mut report = |p: &Progress| opts.report(p.clone());
        let response = self.call_inner(&frame, wait, cancel.as_ref(), Some(&mut report))?;
        if let Some(error) = &response.error {
            return Err(input_error(error, original));
        }
        response
            .transcript_result()
            .ok_or_else(|| ClientError::Io("unexpected transcription response".into()))
    }

    /// Negotiation: minor-1 fields go only to a minor-1 host. A v1.0 host serves
    /// Parakeet, so Auto/Parakeet degrade to the plain v1.0 request and anything
    /// else is refused here instead of being silently decoded by the wrong engine.
    fn wire_options(&self, opts: &TranscribeOptions) -> Result<FileOptions, ClientError> {
        if self.supports_engine_selection() {
            return Ok(FileOptions {
                engine: opts.engine.clone(),
                language: opts.language.clone(),
                verbatim: false,
            });
        }
        match &opts.engine {
            Some(EngineSelector::Named(n)) if n != ENGINE_PARAKEET => Err(ClientError::Unsupported(format!(
                "host protocol 1.{} serves Parakeet only; engine `{n}` needs a protocol 1.{SERVICE_PROTOCOL_MINOR_ENGINES} host",
                self.record.protocol_minor
            ))),
            _ => Ok(FileOptions::default()),
        }
    }
}

/// One status reply, before the caller decides what it means.
enum RawStatus {
    Active(HostProgress),
    Done(FileTranscriptResult),
    /// The job finished with this error, which the service stored for it.
    Failed(ServiceErrorPayload),
    Unknown,
}

fn unqueued() -> ClientError {
    ClientError::Unsupported("host does not advertise the job-queue contract".into())
}

fn parse_status(response: &ServiceEnvelope) -> Result<RawStatus, ClientError> {
    match &response.error {
        Some(e) if e.code == ERROR_JOB_UNKNOWN => return Ok(RawStatus::Unknown),
        Some(e) => return Ok(RawStatus::Failed(e.clone())),
        None => {}
    }
    if let Some(p) = response.host_progress() {
        return Ok(RawStatus::Active(p));
    }
    response
        .transcript_result()
        .map(RawStatus::Done)
        .ok_or_else(|| ClientError::Io("unexpected status response".into()))
}

/// Connect to the running service and complete the health handshake.
pub fn connect(suite_root: &Path) -> Result<SpeechClient, ClientError> {
    connect_within(suite_root, HANDSHAKE_TIMEOUT)
}

/// [`connect`] with the handshake bounded by `timeout`.
fn connect_within(suite_root: &Path, timeout: Duration) -> Result<SpeechClient, ClientError> {
    let record = read_live_record(suite_root).ok_or(ClientError::NoService)?;
    if record.protocol_major != SERVICE_PROTOCOL_MAJOR {
        return Err(ClientError::ProtocolMismatch {
            major: record.protocol_major,
        });
    }
    let stream = rightkit_daemon::open_stream(&record.endpoint).map_err(io_err)?;
    let abort = stream.abort_handle();
    let (tx, rx) = mpsc::channel();
    let mut reader = BufReader::new(stream.reader);
    std::thread::Builder::new()
        .name("rk-speech-read".into())
        .spawn(move || {
            let mut buf = Vec::new();
            loop {
                let msg = match read_json_line::<_, serde_json::Value>(
                    &mut reader,
                    &mut buf,
                    FRAME_LIMIT,
                ) {
                    Ok(Some(v)) => match serde_json::from_value::<ServiceEnvelope>(v) {
                        Ok(f) => Incoming::Frame(Box::new(f)),
                        Err(e) => Incoming::Bad(e.to_string()),
                    },
                    Ok(None) | Err(FrameError::Io(_)) => Incoming::Closed,
                    Err(e) => Incoming::Bad(e.to_string()),
                };
                let stop = !matches!(msg, Incoming::Frame(_));
                if tx.send(msg).is_err() || stop {
                    return;
                }
            }
        })
        .map_err(io_err)?;
    let mut client = SpeechClient {
        writer: stream.writer,
        rx,
        abort,
        record,
        suite_root: suite_root.to_path_buf(),
        caps: None,
        dead: false,
    };
    client.handshake(timeout)?;
    Ok(client)
}

fn wait_for_new_host(suite_root: &Path, old_pid: u32, cancel: Option<&CancelToken>) {
    // After a handoff the old host exits and the outranking host binds shortly after.
    // Wait for a different live host. If the same host is still there and no takeover
    // is pending, the handoff was spurious: resubmit to it once the backoff has passed.
    let started = Instant::now();
    let deadline = started + HANDOFF_REDISCOVERY;
    while Instant::now() < deadline && !cancel.is_some_and(CancelToken::is_cancelled) {
        match read_live_record(suite_root) {
            Some(r) if r.pid != old_pid => return,
            Some(_)
                if read_takeover(suite_root).is_none()
                    && started.elapsed() >= Duration::from_millis(500) =>
            {
                return
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Connect (starting installed HeardRight headless if needed), transcribe, and
/// re-discover on host handoff (max 3 attempts), with engine selection, progress,
/// cancel and stall timeout. `NoService` means the caller must host the engine
/// itself (taking over first when [`plan_host`] says so).
pub fn transcribe_via_service_with(
    suite_root: &Path,
    my_role: HostRole,
    path: &Path,
    opts: &mut TranscribeOptions,
) -> Result<FileTranscriptResult, ClientError> {
    transcribe_via_service_policy(
        suite_root,
        my_role,
        path,
        opts,
        Launch::Auto,
        &HeardRightLauncher,
    )
}

/// [`transcribe_via_service_with`] with the launch decided by `launch` and performed by
/// `launcher`. With [`Launch::Never`] the launcher is never called and no takeover is
/// requested: a live record is used as found, or the call returns
/// [`ClientError::NoLiveHost`]. On a handoff retry the live-host check runs again.
pub fn transcribe_via_service_policy(
    suite_root: &Path,
    my_role: HostRole,
    path: &Path,
    opts: &mut TranscribeOptions,
    launch: Launch,
    launcher: &dyn HostLauncher,
) -> Result<FileTranscriptResult, ClientError> {
    // One idempotency key for every attempt: a minor-3 host attaches a resubmit to the same job.
    if opts.request_id.is_none() {
        opts.request_id = Some(next_request_id());
    }
    let mut last = ClientError::NoService;
    // Staged once, before the first submit, and kept across attempts: a resubmit names the same file.
    let mut staged: Option<Staged> = None;
    let mut prepared = false;
    for attempt in 0..MAX_ATTEMPTS {
        if opts.cancelled() {
            return Err(ClientError::Cancelled);
        }
        match launch {
            Launch::Never => {
                if read_live_record(suite_root).is_none() {
                    return Err(ClientError::NoLiveHost);
                }
            }
            Launch::Auto => match plan_host(suite_root, my_role) {
                HostPlan::UseRunning => {}
                HostPlan::StartInstalledHeardRight(engine) => {
                    opts.report(Progress::StartingHost);
                    if launcher.launch(suite_root, &engine).is_err() {
                        return Err(ClientError::NoService);
                    }
                }
                HostPlan::HostMyself | HostPlan::TakeOver { .. } => {
                    return Err(ClientError::NoService)
                }
            },
        }
        opts.report(Progress::Connecting {
            attempt: attempt + 1,
        });
        let host_pid = read_live_record(suite_root).map(|r| r.pid).unwrap_or(0);
        if !prepared {
            staged = prepare(path, opts.stage, opts.stage_limit, &StageRoots::system())?;
            prepared = true;
        }
        let submit = submit_path(&staged, path);
        match connect(suite_root).and_then(|mut c| c.submit_file(submit, path, opts)) {
            Err(ClientError::HandoffRetry) if opts.running_host_only => {
                return Err(ClientError::HandoffRetry);
            }
            Err(ClientError::HostGone) if opts.running_host_only => {
                return Err(ClientError::HostGone);
            }
            Err(ClientError::HandoffRetry) => {
                last = ClientError::HandoffRetry;
                opts.report(Progress::HandoffRetry {
                    attempt: attempt + 1,
                });
                std::thread::sleep(Duration::from_millis(200 * (u64::from(attempt) + 1)));
                wait_for_new_host(suite_root, host_pid, opts.cancel.as_ref());
            }
            Err(ClientError::HostGone) => {
                last = ClientError::HostGone;
                opts.report(Progress::Reconnecting {
                    attempt: attempt + 1,
                });
                wait_for_new_host(suite_root, host_pid, opts.cancel.as_ref());
            }
            other => return other,
        }
    }
    Err(last)
}

/// `orthic-speech-client`-compatible entry point: default engine, `timeout` is the stall timeout.
pub fn transcribe_via_service(
    suite_root: &Path,
    my_role: HostRole,
    path: &Path,
    timeout: Duration,
) -> Result<FileTranscript, ClientError> {
    transcribe_via_service_with(
        suite_root,
        my_role,
        path,
        &mut TranscribeOptions::new(timeout),
    )
    .map(|r| r.transcript)
}

// ---------------------------------------------------------------------------
// Cancel outcome
// ---------------------------------------------------------------------------

/// What [`cancel_request`] learned about a request. The protocol has no cancel
/// acknowledgement, so no variant reports that the service stopped the job.
#[derive(Debug, Clone, PartialEq)]
pub enum CancelOutcome {
    /// The service reports the job had finished before the cancel. `Ok` is its stored
    /// transcript. `Err` is the failure the service stored; `HandoffRetry` means the job
    /// ended in a host handoff, so resubmit with the same request id.
    Finished(Result<FileTranscriptResult, ClientError>),
    /// The service still holds the job queued or running. The cancel did not stop it: the
    /// wire has no cancel message, and closing the connection does not stop a job on
    /// HeardRight's queue host.
    StillActive(HostProgress),
    /// The service holds no queued, running or retained job under the id, so nothing is
    /// running for it on that service. This is absence, not an acknowledgement: the result
    /// may have expired (retained 10 minutes), the host may have restarted, or the request
    /// may never have reached it.
    NoLongerActive,
    /// The outcome cannot be learned: no live service (`NoService`), the connection failed
    /// or closed (`Io`), no reply within the bound (`Timeout`), or an empty request id or a
    /// host below protocol minor 3 (`Unsupported`).
    Unknown(ClientError),
}

/// Cancel the request that `cancel` belongs to, then ask the live service what became of
/// `request_id`: the id the request was sent with (set it with
/// [`TranscribeOptions::request_id`]). `cancel` fires at once. Each wire step gets only the
/// time left of `bound`, so the call returns within about `bound`.
pub fn cancel_request(
    suite_root: &Path,
    cancel: &CancelToken,
    request_id: &str,
    bound: Duration,
) -> CancelOutcome {
    cancel.cancel();
    if request_id.is_empty() {
        return CancelOutcome::Unknown(ClientError::Unsupported(
            "a request id is required to learn a cancel outcome".into(),
        ));
    }
    let deadline = Instant::now() + bound;
    let Some(left) = time_left(deadline) else {
        return CancelOutcome::Unknown(ClientError::Timeout);
    };
    let mut client = match connect_within(suite_root, left) {
        Ok(client) => client,
        Err(error) => return CancelOutcome::Unknown(error),
    };
    let Some(left) = time_left(deadline) else {
        return CancelOutcome::Unknown(ClientError::Timeout);
    };
    match client.status_within(request_id, left) {
        Ok(RawStatus::Active(progress)) => CancelOutcome::StillActive(progress),
        Ok(RawStatus::Done(result)) => CancelOutcome::Finished(Ok(result)),
        Ok(RawStatus::Failed(error)) => CancelOutcome::Finished(Err(map_error(&error))),
        Ok(RawStatus::Unknown) => CancelOutcome::NoLongerActive,
        Err(error) => CancelOutcome::Unknown(error),
    }
}

/// The time left before `deadline`, or `None` once it has passed.
fn time_left(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
}

/// Map a host progress frame to the typed client event. Minor-3 hosts get
/// `Queued`/`Running` (a dictation pause is a queue wait: position 0, `dictation_active`).
fn queue_progress(p: HostProgress, queue: bool) -> Progress {
    if !queue {
        return Progress::Host(p);
    }
    let job_id = p.job_id.clone().unwrap_or_default();
    match p.state {
        ProgressState::Queued => Progress::Queued {
            job_id,
            position: p.position.unwrap_or(0),
            reason: p.reason.unwrap_or(QueueReason::BehindJobs),
        },
        ProgressState::PausedForDictation => Progress::Queued {
            job_id,
            position: 0,
            reason: QueueReason::DictationActive,
        },
        ProgressState::Running => Progress::Running {
            job_id,
            fraction: p.fraction,
        },
        ProgressState::Other(_) => Progress::Host(p),
    }
}
