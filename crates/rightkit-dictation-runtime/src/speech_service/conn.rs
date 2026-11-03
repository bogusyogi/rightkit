//! Per-connection frame handling for the speech service endpoint.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rightkit_sidecar::{
    encode_json_line, read_line_bytes_observed, write_encoded_line_flush, FrameError, LimitCounts,
    ReadLimits, ReadOptions, ReceivedLine, Terminator,
};
use rightkit_speech_client::{
    is_allowed_schema, FileTranscript, HostRole, QueueReason, ServiceEnvelope, ServiceRequest,
    CAPABILITY_QUEUE, CAPABILITY_VERBATIM, ERROR_HOST_HANDOFF, ERROR_JOB_UNKNOWN,
    ERROR_NOT_ALLOWED, QUEUE_KEEPALIVE_MS, QUEUE_RESULT_RETENTION_MS, SERVICE_PROTOCOL_MINOR_QUEUE,
};

use crate::runtime::EngineRuntime;

const MAX_LINE: u64 = 1 << 20;
/// The most payload bytes an outbound frame may have. This service's own reader (`read_service_line`) counts the line
/// terminator in `MAX_LINE` (a terminated line may be `MAX_LINE` raw bytes, newline included), and the owner encoder's limit
/// counts the payload without it, so the limit for encoding and writing is one less. The peer `rightkit-speech-client`
/// reads with its own, larger frame limit (16 MiB); this cap is the conservative service-side bound.
const MAX_PAYLOAD: usize = MAX_LINE as usize - 1;

pub struct Ctx {
    pub runtime: Arc<Mutex<EngineRuntime>>,
    pub role: HostRole,
    pub accepting: AtomicBool,
    pub connections: AtomicUsize,
    pub last_activity: Mutex<Instant>,
    pub next_conn: AtomicU64,
    jobs: Mutex<Jobs>,
}

impl Ctx {
    pub fn new(runtime: Arc<Mutex<EngineRuntime>>, role: HostRole) -> Self {
        Self {
            runtime,
            role,
            accepting: AtomicBool::new(true),
            connections: AtomicUsize::new(0),
            last_activity: Mutex::new(Instant::now()),
            next_conn: AtomicU64::new(0),
            jobs: Mutex::new(Jobs::default()),
        }
    }

    pub fn touch(&self) {
        *self.last_activity.lock() = Instant::now();
    }
}

// Socket I/O must never run under the global job lock. A stalled subscriber
// loses only its connection; accepted jobs remain available for replay.
struct Outbound {
    // One owner-encoded frame: compact JSON plus its newline.
    tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    closed: Arc<AtomicBool>,
    disconnect: Arc<dyn Fn() + Send + Sync>,
}
type SharedWriter = Arc<Outbound>;

impl Outbound {
    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            (self.disconnect)();
        }
    }
}

/// Code of the terminal error a client gets when its reply could not be encoded within `MAX_LINE`.
pub const ERROR_REPLY_TOO_LARGE: &str = "E_REPLY_TOO_LARGE";

fn stamp(frame: &ServiceEnvelope) -> ServiceEnvelope {
    let mut frame = frame.clone();
    frame.protocol_minor = SERVICE_PROTOCOL_MINOR_QUEUE as u16;
    frame.engine_version = super::engine_version();
    frame
}

/// The line to enqueue for `frame`. The owner encoder refuses a frame the peer's reader could not
/// take (a line over `MAX_LINE` raw bytes with its newline, so a payload over `MAX_PAYLOAD`) or one with an embedded newline. A refused frame is never dropped silently: the client is
/// waiting on that request id, so it gets a small terminal error frame for the same id instead. `None` only when even that
/// cannot be encoded, and the caller then closes the connection so the client reconnects instead of hanging.
fn encode_outbound(frame: &ServiceEnvelope) -> Option<Vec<u8>> {
    let frame = stamp(frame);
    match encode_json_line(&frame, MAX_PAYLOAD) {
        Ok(line) => Some(line),
        Err(error) => {
            tracing::warn!(event = "service_frame_encode_failed", %error, request_id = %frame.request_id);
            let replacement = stamp(&error_frame(
                &frame.request_id,
                ERROR_REPLY_TOO_LARGE,
                "the reply is too large to send",
                false,
            ));
            encode_json_line(&replacement, MAX_PAYLOAD).ok()
        }
    }
}

fn send(writer: &SharedWriter, frame: &ServiceEnvelope) {
    let Some(line) = encode_outbound(frame) else {
        writer.close();
        return;
    };
    if writer.closed.load(Ordering::Acquire) {
        return;
    }
    if let Err(error) = writer.tx.try_send(line) {
        tracing::warn!(event = "service_client_backpressure", %error);
        writer.close();
    }
}

static TRACE: AtomicU64 = AtomicU64::new(1);

fn trace_id() -> String {
    format!("svc-{}", TRACE.fetch_add(1, Ordering::Relaxed))
}

/// Engine label echoed on file results; verbatim jobs are tagged so callers can tell the modes apart.
fn result_engine(base: &str, verbatim: bool) -> String {
    if verbatim {
        format!("{base}+verbatim")
    } else {
        base.to_string()
    }
}

/// Privacy-denied reads keep their typed code so clients can name the grant.
fn file_error_code(message: &str) -> &'static str {
    if message.starts_with("E_PERMISSION") {
        "E_PERMISSION"
    } else if message.contains(ERROR_HOST_HANDOFF) {
        ERROR_HOST_HANDOFF
    } else {
        "E_ENGINE"
    }
}

pub fn error_frame(
    request_id: &str,
    code: &str,
    message: &str,
    retryable: bool,
) -> ServiceEnvelope {
    ServiceEnvelope::error(request_id, &trace_id(), code, message, retryable)
}

/// Parse + validate one line. `Err` carries the best-effort request id.
fn parse_line(line: &str) -> Result<ServiceEnvelope, (String, String)> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| (String::new(), format!("malformed frame: {e}")))?;
    let rid = value
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let frame: ServiceEnvelope =
        serde_json::from_value(value).map_err(|e| (rid.clone(), format!("invalid frame: {e}")))?;
    frame
        .validate_envelope()
        .map_err(|e| (rid, format!("invalid frame: {e}")))?;
    Ok(frame)
}

/// Pure gate: `Some(error code)` when the schema must be rejected.
pub fn reject_code(schema: &str) -> Option<&'static str> {
    (!is_allowed_schema(schema)).then_some(ERROR_NOT_ALLOWED)
}

/// One line from a service client, as text. `None` closes the connection: EOF,
/// a line over `MAX_LINE` raw bytes (terminator included, never drained), invalid
/// UTF-8, or a read error, as the old `take(MAX_LINE).read_line` loop did. Blank
/// lines are returned for the caller to skip. `touch` runs once for every line
/// that arrives, blank or not, before the caller acts on it.
fn read_service_line<R: BufRead>(
    reader: &mut R,
    conn_id: u64,
    touch: &mut dyn FnMut(),
) -> Option<String> {
    let opts = ReadOptions {
        skip_empty_lines: false,
        skip_whitespace_only_lines: false,
        limit_counts: LimitCounts::RawLineBytes,
        limits: ReadLimits {
            max_discard: 0,
            ..ReadLimits::default()
        },
        ..ReadOptions::default()
    };
    let mut seen: Option<ReceivedLine> = None;
    let result = read_line_bytes_observed(
        reader,
        &mut Vec::new(),
        MAX_LINE as usize,
        opts,
        &mut |line| seen = Some(line),
    );
    // The old rule: a `\n`-terminated line may be MAX_LINE raw bytes; an unterminated
    // line must be shorter, since `take(MAX_LINE)` ending without `\n` means "too long".
    let within = seen.is_some_and(|line| match line.terminator {
        Terminator::None => (line.raw_len as u64) < MAX_LINE,
        _ => line.raw_len as u64 <= MAX_LINE,
    });
    let bytes = match result {
        Ok(Some(bytes)) if within => bytes,
        Ok(Some(_)) | Err(FrameError::TooLarge { .. }) => {
            tracing::warn!(event = "service_client_frame_too_large", conn_id);
            return None;
        }
        Ok(None) => return None,
        Err(FrameError::EmptyLine) if within => {
            touch();
            return Some(String::new());
        }
        Err(_) => return None,
    };
    let text = String::from_utf8(bytes).ok()?;
    touch();
    Some(text)
}

pub fn serve(
    ctx: Arc<Ctx>,
    reader: Box<dyn Read + Send>,
    mut socket_writer: Box<dyn Write + Send>,
    disconnect: Arc<dyn Fn() + Send + Sync>,
) {
    let conn_id = ctx.next_conn.fetch_add(1, Ordering::Relaxed) + 1;
    ctx.connections.fetch_add(1, Ordering::SeqCst);
    ctx.touch();
    tracing::info!(
        event = "service_client_connected",
        conn_id,
        "service client connected"
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(16);
    let closed = Arc::new(AtomicBool::new(false));
    let writer = Arc::new(Outbound {
        tx,
        closed: closed.clone(),
        disconnect: disconnect.clone(),
    });
    if let Err(error) = std::thread::Builder::new()
        .name("hr-speech-send".into())
        .spawn(move || {
            while let Ok(line) = rx.recv() {
                if closed.load(Ordering::Acquire) {
                    break;
                }
                let result = write_encoded_line_flush(&mut socket_writer, &line, MAX_PAYLOAD);
                if let Err(error) = result {
                    tracing::warn!(event = "service_client_write_failed", conn_id, %error);
                    if !closed.swap(true, Ordering::AcqRel) {
                        disconnect();
                    }
                    break;
                }
            }
        })
    {
        tracing::error!(event = "service_client_writer_failed", conn_id, %error);
        writer.close();
    }
    let mut reader = std::io::BufReader::new(reader);
    while let Some(text) = read_service_line(&mut reader, conn_id, &mut || ctx.touch()) {
        let line = text.trim();
        if line.is_empty() {
            continue;
        }
        let frame = match parse_line(line) {
            Ok(frame) => frame,
            Err((rid, message)) => {
                tracing::warn!(event = "service_client_bad_frame", conn_id, %message);
                send(&writer, &error_frame(&rid, "E_BAD_FRAME", &message, false));
                continue;
            }
        };
        let rid = frame.request_id.clone();
        if let Some(code) = reject_code(&frame.schema_name) {
            send(
                &writer,
                &error_frame(
                    &rid,
                    code,
                    "request not allowed on the speech service",
                    false,
                ),
            );
            continue;
        }
        handle_frame(&ctx, frame, &writer, &cancel);
    }
    cancel.store(true, Ordering::SeqCst);
    // Accepted jobs belong to request IDs, not sockets; reconnect can attach.
    writer.close();
    ctx.jobs.lock().entries.values_mut().for_each(|job| {
        job.listeners.retain(|w| {
            w.upgrade()
                .is_some_and(|w| !w.closed.load(Ordering::Acquire))
        })
    });
    ctx.connections.fetch_sub(1, Ordering::SeqCst);
    ctx.touch();
    tracing::info!(
        event = "service_client_closed",
        conn_id,
        "service client closed"
    );
}

fn handle_frame(
    ctx: &Arc<Ctx>,
    frame: ServiceEnvelope,
    writer: &SharedWriter,
    _cancel: &Arc<AtomicBool>,
) {
    let rid = frame.request_id.clone();
    let tid = trace_id();
    match frame.request() {
        Ok(ServiceRequest::Health) => send(writer, &ServiceEnvelope::health(&rid, &tid)),
        Ok(ServiceRequest::Capabilities) => send(
            writer,
            &ServiceEnvelope::capabilities_with(
                &rid,
                &tid,
                &[],
                &[CAPABILITY_VERBATIM, CAPABILITY_QUEUE],
            ),
        ),
        Ok(ServiceRequest::Info) => {
            let info = ctx.runtime.lock().engine_info();
            send(
                writer,
                &ServiceEnvelope::info(&rid, &tid, info.cold_load_s, Some(super::engine_version())),
            );
        }
        Ok(ServiceRequest::FileTranscription { path }) => {
            if !ctx.accepting.load(Ordering::SeqCst) {
                send(
                    writer,
                    &error_frame(
                        &rid,
                        ERROR_HOST_HANDOFF,
                        "speech service host is handing off",
                        true,
                    ),
                );
                return;
            }
            if rid.is_empty() {
                send(
                    writer,
                    &error_frame(&rid, "E_BAD_FRAME", "request_id must not be empty", false),
                );
                return;
            }
            enqueue(
                ctx,
                &rid,
                PathBuf::from(path),
                frame.file_options().verbatim,
                writer,
            );
        }
        Ok(ServiceRequest::Status { job_id }) => {
            let mut jobs = ctx.jobs.lock();
            jobs.prune();
            if let Some(job) = jobs.entries.get(&job_id) {
                let mut reply = job.frame.clone();
                reply.request_id = rid;
                send(writer, &reply);
            } else {
                send(
                    writer,
                    &error_frame(
                        &rid,
                        ERROR_JOB_UNKNOWN,
                        "file job is unknown or expired",
                        false,
                    ),
                );
            }
        }
        Err(message) => send(writer, &error_frame(&rid, "E_BAD_FRAME", message, false)),
    }
}

#[derive(Default)]
struct Jobs {
    entries: HashMap<String, Job>,
    queue: VecDeque<String>,
    dispatching: bool,
}

struct Job {
    path: PathBuf,
    verbatim: bool,
    progress: Arc<crate::worker::FileProgress>,
    frame: ServiceEnvelope,
    listeners: Vec<Weak<Outbound>>,
    finished: Option<Instant>,
    last_sent: Instant,
}

impl Jobs {
    fn prune(&mut self) {
        self.entries.retain(|_, job| {
            job.finished
                .is_none_or(|t| t.elapsed() < Duration::from_millis(QUEUE_RESULT_RETENTION_MS))
        });
    }
}

fn broadcast(job: &mut Job) {
    job.listeners.retain(|listener| {
        if let Some(writer) = listener.upgrade() {
            send(&writer, &job.frame);
            !writer.closed.load(Ordering::Acquire)
        } else {
            false
        }
    });
    job.last_sent = Instant::now();
}

fn enqueue(ctx: &Arc<Ctx>, id: &str, path: PathBuf, verbatim: bool, writer: &SharedWriter) {
    let mut jobs = ctx.jobs.lock();
    jobs.prune();
    if let Some(job) = jobs.entries.get_mut(id) {
        send(writer, &job.frame);
        if job.finished.is_none()
            && !job
                .listeners
                .iter()
                .any(|w| Weak::ptr_eq(w, &Arc::downgrade(writer)))
        {
            job.listeners.push(Arc::downgrade(writer));
        }
        return;
    }
    // Retain every admitted terminal result for ten minutes; bound admission.
    if jobs.entries.len() >= 512 {
        send(
            writer,
            &error_frame(
                id,
                "E_QUEUE_FULL",
                "speech service job capacity reached",
                true,
            ),
        );
        return;
    }
    let position = jobs.queue.len() as u32;
    let reason = if !ctx.runtime.lock().is_idle() {
        QueueReason::DictationActive
    } else if position > 0 {
        QueueReason::BehindJobs
    } else {
        QueueReason::LoadingModel
    };
    let frame = ServiceEnvelope::job_progress(
        id,
        &trace_id(),
        "queued",
        Some(position),
        Some(&reason),
        None,
    );
    send(writer, &frame);
    jobs.entries.insert(
        id.into(),
        Job {
            path,
            verbatim,
            progress: Arc::new(crate::worker::FileProgress::default()),
            frame,
            listeners: vec![Arc::downgrade(writer)],
            finished: None,
            last_sent: Instant::now(),
        },
    );
    jobs.queue.push_back(id.into());
    tracing::info!(
        event = "service_file_queued",
        job_id = id,
        position,
        verbatim,
        reason = reason.as_str()
    );
    if jobs.dispatching {
        return;
    }
    jobs.dispatching = true;
    drop(jobs);
    let thread_ctx = ctx.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("hr-speech-queue".into())
        .spawn(move || dispatch(thread_ctx))
    {
        let mut jobs = ctx.jobs.lock();
        jobs.dispatching = false;
        let ids: Vec<_> = jobs.queue.drain(..).collect();
        for id in ids {
            if let Some(job) = jobs.entries.get_mut(&id) {
                job.frame = error_frame(&id, "E_ENGINE", &error.to_string(), true);
                job.finished = Some(Instant::now());
                broadcast(job);
            }
        }
        tracing::error!(event = "service_file_dispatch_failed", %error);
    }
}

fn refresh(ctx: &Ctx) {
    let dictation = !ctx.runtime.lock().is_idle();
    let mut jobs = ctx.jobs.lock();
    let ids: Vec<_> = jobs.queue.iter().cloned().collect();
    for (position, id) in ids.iter().enumerate() {
        let job = jobs.entries.get_mut(id).expect("queued job exists");
        let paused = dictation || job.progress.paused.load(Ordering::Acquire);
        let lease_wait = position == 0 && !paused && super::lease::lease_wait_active();
        let running =
            position == 0 && job.progress.started.load(Ordering::Acquire) && !paused && !lease_wait;
        let reason = if paused {
            QueueReason::DictationActive
        } else if position > 0 {
            QueueReason::BehindJobs
        } else if lease_wait {
            QueueReason::Other("waiting_for_model_lease".into())
        } else {
            QueueReason::LoadingModel
        };
        let fraction = (job.progress.done.load(Ordering::Acquire) as f32
            / job.progress.total.load(Ordering::Acquire).max(1) as f32)
            .clamp(0.0, 1.0);
        let frame = ServiceEnvelope::job_progress(
            id,
            &job.frame.trace_id,
            if running { "running" } else { "queued" },
            (!running).then_some(position as u32),
            (!running).then_some(&reason),
            running.then_some(fraction),
        );
        let changed = job.frame.payload != frame.payload;
        if changed || job.last_sent.elapsed() >= Duration::from_millis(QUEUE_KEEPALIVE_MS / 2) {
            job.frame = frame;
            broadcast(job);
            if changed {
                tracing::info!(event = "service_file_state", job_id = %id, running, fraction, reason = reason.as_str());
            }
        }
    }
}

fn dispatch(ctx: Arc<Ctx>) {
    loop {
        let (id, path, verbatim, progress) = {
            let mut jobs = ctx.jobs.lock();
            jobs.prune();
            let Some(id) = jobs.queue.front().cloned() else {
                jobs.dispatching = false;
                return;
            };
            let job = &jobs.entries[&id];
            (id, job.path.clone(), job.verbatim, job.progress.clone())
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let runtime = ctx.runtime.clone();
        let spawned = std::thread::Builder::new()
            .name("hr-speech-file".into())
            .spawn(move || {
                let handle = runtime.lock().worker_handle();
                let result = handle.and_then(|handle| {
                    handle.transcribe_file_options(
                        path,
                        Arc::new(AtomicBool::new(false)),
                        progress,
                        verbatim,
                    )
                });
                let _ = tx.send(result);
            });
        let result = match spawned {
            Err(error) => Err(error.to_string()),
            Ok(_) => loop {
                refresh(&ctx);
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(result) => break result,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break Err("speech file worker disconnected".into()),
                }
            },
        };
        let lease_error = result
            .as_ref()
            .err()
            .filter(|message| message.starts_with("model_lease_unavailable"))
            .cloned();
        let frame = match result {
            Ok(t) => ServiceEnvelope::file_result_with(
                &id,
                &trace_id(),
                FileTranscript {
                    text: t.text,
                    srt: t.srt,
                    vtt: t.vtt,
                    words: t.words,
                },
                Some(&result_engine(&super::engine_version(), verbatim)),
                None,
            ),
            Err(message) if message.starts_with("model_lease_unavailable") => {
                error_frame(&id, "model_lease_unavailable", &message, true)
            }
            Err(message) => error_frame(
                &id,
                file_error_code(&message),
                &message,
                message.contains(ERROR_HOST_HANDOFF),
            ),
        };
        let failed = frame.error.is_some();
        let mut jobs = ctx.jobs.lock();
        let job = jobs.entries.get_mut(&id).expect("active job exists");
        job.frame = frame;
        job.finished = Some(Instant::now());
        broadcast(job);
        jobs.queue.pop_front();
        if let Some(message) = lease_error {
            let ids: Vec<_> = jobs.queue.drain(..).collect();
            for id in ids {
                let job = jobs.entries.get_mut(&id).expect("queued job exists");
                job.frame = error_frame(&id, "model_lease_unavailable", &message, true);
                job.finished = Some(Instant::now());
                broadcast(job);
                tracing::info!(event = "service_file_completed", job_id = %id, failed = true, verbatim = job.verbatim);
            }
        }
        drop(jobs);
        ctx.touch();
        tracing::info!(event = "service_file_completed", job_id = %id, failed, verbatim);
    }
}
