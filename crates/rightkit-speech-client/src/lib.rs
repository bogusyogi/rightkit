//! Client for the Orthic Labs shared speech service.
//!
//! One process per machine hosts the speech engines (Parakeet today, Whisper and
//! others by capability). Other apps discover it through `service.json`, connect
//! to its current-user-only endpoint and exchange line-delimited v1 `EngineFrame`
//! JSON restricted to health, capabilities, info and file transcription.
//!
//! Built on `rightkit-service` (discovery files, staleness, ranking/takeover
//! files, the user-only socket/pipe transport) and wire-compatible with
//! HeardRight's v1 host: an unchanged host accepts this client. Hosting stays in
//! HeardRight's engine. The API is a superset of `orthic-speech-client` 0.1, so
//! HeardRight/CutRight swap the dependency only. See `API.md`.

mod protocol;

use std::fmt;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_framed_sidecar::{read_json_line, FrameError};
use rightkit_service::Suite;

pub use protocol::{
    endpoint_for, installed_heardright_path, is_allowed_schema, service_dir, service_record_path,
    takeover_request_path, Capabilities, EngineInfo, EngineSelector, FileOptions, FileTranscript,
    FileTranscriptResult, HostProgress, HostRole, InstalledEngine, ProgressState, ServiceEnvelope,
    ServiceErrorPayload, ServiceRecord, ServiceRequest, TakeoverRequest, TimedWord,
    ENGINE_PARAKEET, ENGINE_WHISPER, ERROR_ENGINE_UNAVAILABLE, ERROR_HOST_HANDOFF,
    ERROR_NOT_ALLOWED, SCHEMA_PROGRESS, SERVICE_ALLOWED_SCHEMAS, SERVICE_NAME,
    SERVICE_PROTOCOL_MAJOR, SERVICE_PROTOCOL_MINOR, SERVICE_PROTOCOL_MINOR_ENGINES, SPEECH_FAMILY,
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
    /// The caller's [`CancelToken`] fired; the connection was closed so the host cancels the job.
    Cancelled,
    /// The host's protocol minor cannot honour the request (for example Whisper on a v1.0 host).
    Unsupported(String),
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
    rightkit_service::pid_alive(pid)
}

/// The service record, only when it parses as a HeardRight v1 record and its
/// owner process is alive.
pub fn read_live_record(suite_root: &Path) -> Option<ServiceRecord> {
    let r = rightkit_service::read_live_record(&suite_at(suite_root)?, SERVICE_NAME)?;
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

/// Decide how the calling app (`my_role`) gets a speech host.
pub fn plan_host(suite_root: &Path, my_role: HostRole) -> HostPlan {
    if let Some(record) = read_live_record(suite_root) {
        return if my_role.outranks(record.role) {
            HostPlan::TakeOver { from: record.role }
        } else {
            HostPlan::UseRunning
        };
    }
    // HeardRight roles host themselves; they never start another copy.
    if my_role.rank() <= HostRole::HeardRightHeadless.rank() {
        return HostPlan::HostMyself;
    }
    match read_installed(suite_root) {
        Some(installed) if installed.engine_path.exists() => {
            HostPlan::StartInstalledHeardRight(installed.engine_path)
        }
        _ => HostPlan::HostMyself,
    }
}

/// Takeover is requested only against a live, lower-ranked host.
pub fn should_request_takeover(our: HostRole, record: Option<&ServiceRecord>) -> bool {
    record.is_some_and(|r| our.outranks(r.role) && r.pid != std::process::id())
}

/// A takeover request that obliges a host running as `ours` to yield.
pub fn takeover_obliges_yield(
    ours: HostRole,
    request: Option<&TakeoverRequest>,
    own_pid: u32,
    alive: &dyn Fn(u32) -> bool,
) -> bool {
    request.is_some_and(|r| r.pid != own_pid && r.role.outranks(ours) && alive(r.pid))
}

/// The pending takeover request, if it parses.
pub fn read_takeover(suite_root: &Path) -> Option<TakeoverRequest> {
    serde_json::from_slice(&std::fs::read(takeover_request_path(suite_root)).ok()?).ok()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
    rightkit_service::write_takeover(
        &suite,
        SERVICE_NAME,
        &rightkit_service::TakeoverRequest {
            role: role.as_str().into(),
            rank: role.rank(),
            pid: std::process::id(),
            requested_at_ms: now_ms(),
        },
    )
}

/// Remove `takeover.json` only if this process wrote it.
pub fn clear_takeover_if_ours(suite_root: &Path) {
    if let Some(s) = suite_at(suite_root) {
        rightkit_service::clear_takeover_if_owner(&s, SERVICE_NAME, std::process::id());
    }
}

/// Spawn `<engine_path> --speech-service` detached, then wait up to `wait` for
/// a live `service.json`.
pub fn start_installed_heardright(
    suite_root: &Path,
    engine_path: &Path,
    wait: Duration,
) -> Result<(), ClientError> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(engine_path);
    // The engine takes the shared model lease only on the Parakeet backend.
    cmd.arg("--speech-service")
        .env("HR_ASR_BACKEND", "parakeet-tdt")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    drop(cmd.spawn().map_err(io_err)?);
    let deadline = Instant::now() + wait;
    loop {
        if read_live_record(suite_root).is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ClientError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// Progress, cancel, options
// ---------------------------------------------------------------------------

/// Cooperative cancel. Clone it into the thread that may cancel; the request
/// returns [`ClientError::Cancelled`] within ~100 ms and the connection is
/// closed, which makes the host cancel the file job (HeardRight's host cancels
/// on disconnect).
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
    /// A minor-1 host's own progress frame.
    Host(HostProgress),
    HandoffRetry {
        attempt: u32,
    },
}

pub type ProgressFn = Box<dyn FnMut(&Progress) + Send>;

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
}

impl TranscribeOptions {
    pub fn new(stall_timeout: Duration) -> Self {
        Self {
            engine: None,
            language: None,
            stall_timeout,
            cancel: None,
            on_progress: None,
        }
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
    dead: bool,
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
                self.kill();
                return Err(ClientError::Cancelled);
            }
            let idle = last_frame.elapsed();
            if idle >= stall {
                self.kill();
                return Err(ClientError::Timeout);
            }
            let wait = (stall - idle).min(Duration::from_millis(100));
            match self.rx.recv_timeout(wait) {
                Ok(Incoming::Frame(f)) => {
                    last_frame = Instant::now();
                    if f.error.is_some() {
                        return Ok(*f);
                    }
                    if f.request_id == frame.request_id {
                        if let Some(p) = f.host_progress() {
                            if let Some(r) = report.as_deref_mut() {
                                r(&Progress::Host(p));
                            }
                            continue;
                        }
                        return Ok(*f);
                    }
                }
                Ok(Incoming::Closed) | Err(RecvTimeoutError::Disconnected) => {
                    self.kill();
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

    fn handshake(&mut self) -> Result<(), ClientError> {
        let frame = ServiceEnvelope::health(&next_request_id(), TRACE_ID);
        let response = self.call_raw(&frame, HANDSHAKE_TIMEOUT, None, None)?;
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
        response
            .capabilities_info()
            .ok_or_else(|| ClientError::Io("unexpected capabilities response".into()))
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
        let wire = self.wire_options(opts)?;
        let frame = ServiceEnvelope::file_request_with(
            &next_request_id(),
            TRACE_ID,
            path.to_string_lossy().into_owned(),
            &wire,
        );
        let (stall, cancel) = (opts.stall_timeout, opts.cancel.clone());
        let mut report = |p: &Progress| opts.report(p.clone());
        let response = self.call_raw(&frame, stall, cancel.as_ref(), Some(&mut report))?;
        if let Some(error) = &response.error {
            return Err(map_error(error));
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

/// Connect to the running service and complete the health handshake.
pub fn connect(suite_root: &Path) -> Result<SpeechClient, ClientError> {
    let record = read_live_record(suite_root).ok_or(ClientError::NoService)?;
    if record.protocol_major != SERVICE_PROTOCOL_MAJOR {
        return Err(ClientError::ProtocolMismatch {
            major: record.protocol_major,
        });
    }
    let stream = rightkit_service::open_stream(&record.endpoint).map_err(io_err)?;
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
        dead: false,
    };
    client.handshake()?;
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
    let mut last = ClientError::NoService;
    for attempt in 0..MAX_ATTEMPTS {
        if opts.cancelled() {
            return Err(ClientError::Cancelled);
        }
        match plan_host(suite_root, my_role) {
            HostPlan::UseRunning => {}
            HostPlan::StartInstalledHeardRight(engine) => {
                opts.report(Progress::StartingHost);
                if start_installed_heardright(suite_root, &engine, Duration::from_secs(60)).is_err()
                {
                    return Err(ClientError::NoService);
                }
            }
            HostPlan::HostMyself | HostPlan::TakeOver { .. } => return Err(ClientError::NoService),
        }
        opts.report(Progress::Connecting {
            attempt: attempt + 1,
        });
        let host_pid = read_live_record(suite_root).map(|r| r.pid).unwrap_or(0);
        match connect(suite_root).and_then(|mut c| c.transcribe(path, opts)) {
            Err(ClientError::HandoffRetry) => {
                last = ClientError::HandoffRetry;
                opts.report(Progress::HandoffRetry {
                    attempt: attempt + 1,
                });
                std::thread::sleep(Duration::from_millis(200 * (u64::from(attempt) + 1)));
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
