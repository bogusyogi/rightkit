//! The gated service route (#1098): use the shared speech host only when it is a running host
//! belonging to another process that advertises verbatim file transcription and the input fits
//! one window; otherwise tell the caller to transcribe locally. It never launches a host and
//! never requests a takeover.

use super::{
    connect_within, read_live_record, CancelToken, ClientError, FileTranscriptResult, HostRole,
    TranscribeOptions, HANDSHAKE_TIMEOUT, SERVICE_PROTOCOL_MAJOR,
};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// How often the calling thread checks its cancel flag while the worker connects or waits.
const CANCEL_POLL: Duration = Duration::from_millis(25);

/// The caller's side of one routed request.
#[derive(Debug, Clone)]
pub struct ServiceRoute {
    /// Audio file to transcribe.
    pub path: PathBuf,
    /// The caller's own process id. A host with this pid is the caller itself, never a service.
    pub caller_pid: u32,
    /// The caller's role (reserved for host-rank policy; a running host is used as found).
    pub role: HostRole,
    /// Length of the input in milliseconds.
    pub audio_ms: u64,
    /// The host's decode window in milliseconds. Input longer than one window is transcribed
    /// locally: the shared host would split it and the caller's own stitching would be lost.
    pub window_ms: u64,
}

/// Why the caller should transcribe locally. Typed so the caller logs the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalReason {
    /// No live `service.json`, or its process is gone.
    NoRunningHost,
    /// The live host is the caller's own process.
    OwnProcess { pid: u32 },
    /// The host speaks a protocol major this client does not.
    ProtocolMismatch { major: u32 },
    /// The input is longer than one decode window.
    ExceedsWindow { audio_ms: u64, window_ms: u64 },
    /// The host is below the protocol minor that carries capabilities, so it cannot be known to
    /// serve verbatim file transcription.
    CapabilitiesUnavailable { protocol_minor: u32 },
    /// The host does not advertise file transcription.
    NoFileTranscription,
    /// The host does not advertise verbatim file transcription.
    NoVerbatim,
    /// Connecting or the handshake failed.
    ConnectFailed(ClientError),
    /// The capabilities call failed.
    CapabilitiesFailed(ClientError),
    /// The submit failed or the host returned an error; the caller falls back to local.
    SubmitFailed(ClientError),
}

impl fmt::Display for LocalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRunningHost => write!(f, "no speech host is running"),
            Self::OwnProcess { pid } => {
                write!(f, "the running speech host is this process ({pid})")
            }
            Self::ProtocolMismatch { major } => {
                write!(f, "speech host protocol major {major} is not supported")
            }
            Self::ExceedsWindow {
                audio_ms,
                window_ms,
            } => write!(
                f,
                "input is {audio_ms} ms, longer than one {window_ms} ms window"
            ),
            Self::CapabilitiesUnavailable { protocol_minor } => write!(
                f,
                "speech host protocol minor {protocol_minor} reports no capabilities"
            ),
            Self::NoFileTranscription => {
                write!(f, "speech host does not advertise file transcription")
            }
            Self::NoVerbatim => write!(
                f,
                "speech host does not advertise verbatim file transcription"
            ),
            Self::ConnectFailed(e) => write!(f, "could not connect to the speech host: {e}"),
            Self::CapabilitiesFailed(e) => write!(f, "could not read host capabilities: {e}"),
            Self::SubmitFailed(e) => write!(f, "speech host request failed: {e}"),
        }
    }
}

/// The result of [`transcribe_via_running_host`].
#[derive(Debug, Clone, PartialEq)]
pub enum RouteOutcome {
    /// The running host transcribed the file.
    Transcribed(FileTranscriptResult),
    /// Do not use the host: transcribe locally.
    UseLocal(LocalReason),
    /// The caller's [`CancelToken`] fired.
    Cancelled,
}

/// Transcribe through a running speech host only when the gate passes, in this order:
/// a live host exists; its pid is not `route.caller_pid`; its protocol major matches; the input
/// fits one window; the host advertises file transcription and verbatim. Then submit with
/// `verbatim` forced on (the route is for callers that need raw words), staging the input as
/// [`TranscribeOptions::stage`] says.
///
/// Never launches a host, never requests a takeover, never retries a handoff: any failure after
/// the gate returns [`RouteOutcome::UseLocal`] with [`LocalReason::SubmitFailed`] so the caller
/// transcribes locally. A cancel returns [`RouteOutcome::Cancelled`] within about
/// 25 ms even while the connect or the submit is blocked: the work runs on a worker thread that
/// the cancel token then stops, and the call does not wait for it.
pub fn transcribe_via_running_host(
    suite_root: &Path,
    route: ServiceRoute,
    cancel: &CancelToken,
    mut opts: TranscribeOptions,
) -> RouteOutcome {
    if cancel.is_cancelled() {
        return RouteOutcome::Cancelled;
    }
    opts.verbatim = true;
    opts.cancel = Some(cancel.clone());
    let (tx, rx) = mpsc::channel();
    let root = suite_root.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("rk-speech-route".into())
        .spawn(move || {
            let _ = tx.send(run(&root, &route, opts));
        });
    if let Err(e) = spawned {
        return RouteOutcome::UseLocal(LocalReason::ConnectFailed(ClientError::Io(e.to_string())));
    }
    loop {
        if cancel.is_cancelled() {
            return RouteOutcome::Cancelled;
        }
        match rx.recv_timeout(CANCEL_POLL) {
            Ok(outcome) => return outcome,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return RouteOutcome::UseLocal(LocalReason::SubmitFailed(ClientError::Io(
                    "route worker ended without a result".into(),
                )))
            }
        }
    }
}

fn run(suite_root: &Path, route: &ServiceRoute, mut opts: TranscribeOptions) -> RouteOutcome {
    let Some(record) = read_live_record(suite_root) else {
        return RouteOutcome::UseLocal(LocalReason::NoRunningHost);
    };
    if record.pid == route.caller_pid {
        return RouteOutcome::UseLocal(LocalReason::OwnProcess { pid: record.pid });
    }
    if record.protocol_major != SERVICE_PROTOCOL_MAJOR {
        return RouteOutcome::UseLocal(LocalReason::ProtocolMismatch {
            major: record.protocol_major,
        });
    }
    if route.audio_ms > route.window_ms {
        return RouteOutcome::UseLocal(LocalReason::ExceedsWindow {
            audio_ms: route.audio_ms,
            window_ms: route.window_ms,
        });
    }
    let mut client = match connect_within(suite_root, HANDSHAKE_TIMEOUT) {
        Ok(client) => client,
        Err(ClientError::Cancelled) => return RouteOutcome::Cancelled,
        Err(e) => return RouteOutcome::UseLocal(LocalReason::ConnectFailed(e)),
    };
    // The record can change between the check and the connect: judge the host we reached.
    if client.record.pid == route.caller_pid {
        return RouteOutcome::UseLocal(LocalReason::OwnProcess {
            pid: client.record.pid,
        });
    }
    if !client.supports_engine_selection() {
        return RouteOutcome::UseLocal(LocalReason::CapabilitiesUnavailable {
            protocol_minor: client.protocol_minor(),
        });
    }
    let caps = match client.cached_capabilities() {
        Ok(caps) => caps,
        Err(e) => return RouteOutcome::UseLocal(LocalReason::CapabilitiesFailed(e)),
    };
    if !caps.supports_file_transcription() {
        return RouteOutcome::UseLocal(LocalReason::NoFileTranscription);
    }
    if !caps.supports_verbatim() {
        return RouteOutcome::UseLocal(LocalReason::NoVerbatim);
    }
    match client.transcribe(&route.path, &mut opts) {
        Ok(result) => RouteOutcome::Transcribed(result),
        Err(ClientError::Cancelled) => RouteOutcome::Cancelled,
        Err(e) => RouteOutcome::UseLocal(LocalReason::SubmitFailed(e)),
    }
}
