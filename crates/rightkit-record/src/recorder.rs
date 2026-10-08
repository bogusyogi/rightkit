//! The recorder: the public lifecycle over a [`Backend`].
//!
//! A take runs `starting -> recording -> stopping`, then ends `complete`,
//! `quarantined` or `invalid`. `complete` is reached only by a clean stop with
//! an accepted readiness marker, a validated manifest, and no failure on the way.
//! Every failure ends the take in a terminal state, and `record_stop` returns
//! that outcome again on every later call.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rightkit_contracts::types::record_event::{
    DropReason, EventKind, Frame, FrameDropped as DropInfo, PolicyRef, RecordEvent,
};
use rightkit_contracts::types::record_session::{BundleStatus, RecorderOs, VisibilityState};
use rightkit_contracts::types::Rect;
use rightkit_contracts::{validate, Contract};
use serde_json::{json, Value};

use crate::backend::{
    Backend, BackendPlan, Failure, FrameSample, HelperEvent, HelperIdentity, Permission, Source,
    StartInfo, TrackOutput,
};
use crate::bundle::{
    finalize, recover_stale, strip_nulls, version, Phase, Recovered, State, Writer,
};
use crate::clock::{iso8601_utc, Clock, SystemClock};
use crate::error::{io_err, Code, RecordError, Result};
use crate::hash::is_blake3_text;
use crate::plan::{
    masking_json, scenario_json, target_json, validate as validate_request, StartRequest,
};
use crate::redact::RedactionPolicy;
use crate::support::{support, Support};
use crate::verify::{bundle_verify as verify_bundle, VerifyOptions, VerifyReport};

static NEXT_BUNDLE: AtomicU64 = AtomicU64::new(1);

const ALL_STATES: [VisibilityState; 5] = [
    VisibilityState::OffScreenVisible,
    VisibilityState::OnScreenVisible,
    VisibilityState::Occluded,
    VisibilityState::AppHidden,
    VisibilityState::Minimized,
];

/// The terminal result of a take. Idempotent: `record_stop` returns it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub bundle_id: String,
    pub dir: PathBuf,
    pub status: BundleStatus,
    pub reason: Option<String>,
    /// The first manifest problem, if the sealed manifest does not validate.
    pub manifest_problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub phase: Phase,
    pub bundle_id: String,
    pub dir: PathBuf,
    pub frames: u64,
    pub dropped: u64,
    pub failure: Option<String>,
    pub outcome: Option<Outcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSupport {
    pub state: VisibilityState,
    pub support: Support,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doctor {
    pub os: RecorderOs,
    pub os_build: String,
    pub helper: HelperIdentity,
    pub permission: Permission,
    pub states: Vec<StateSupport>,
}

pub struct Recorder<B: Backend, C: Clock = SystemClock> {
    backend: B,
    clock: C,
    stale_after_ms: u64,
    session: Option<Session>,
}

struct Pending {
    status: BundleStatus,
    reason: Option<String>,
    tracks: Vec<TrackOutput>,
}

struct Session {
    dir: PathBuf,
    bundle_id: String,
    writer: Option<Writer>,
    phase: Phase,
    redaction: RedactionPolicy,
    host_ns_at_start: u64,
    epoch: u32,
    ready: bool,
    frames: u64,
    dropped: u64,
    last_index: Option<u64>,
    last_pts: Option<u64>,
    failure: Option<Failure>,
    outcome: Option<Outcome>,
    pending: Option<Pending>,
}

impl Session {
    fn writer(&mut self) -> &mut Writer {
        self.writer
            .as_mut()
            .expect("the writer is open while a take is active")
    }

    /// Stamps, redacts and validates an event. The caller's fault, if any,
    /// is reported without ending the take.
    fn encode(&mut self, mut event: RecordEvent) -> std::result::Result<Value, String> {
        event.session_id = self.bundle_id.clone();
        event.seq = self.writer().events_written;
        self.redaction.apply(&mut event);
        let mut value = serde_json::to_value(&event).map_err(|e| e.to_string())?;
        strip_nulls(&mut value);
        validate(Contract::RecordEvent, &value).map_err(|e| e.to_string())?;
        Ok(value)
    }

    /// Writes an encoded event. A write failure is a take failure.
    fn append(&mut self, value: Value) -> std::result::Result<(), Failure> {
        let dropped = value.pointer("/frameDropped/count").and_then(Value::as_u64);
        self.writer().append_event(&value).map_err(classify_io)?;
        if let Some(count) = dropped {
            self.dropped += count;
        }
        Ok(())
    }

    fn persist(&mut self, event: RecordEvent) -> std::result::Result<(), Failure> {
        let value = self.encode(event).map_err(Failure::Protocol)?;
        self.append(value)
    }
}

impl<B: Backend, C: Clock> Recorder<B, C> {
    /// Opens the recorder and seals any stale takes under `recovery_roots`. The
    /// recovered takes are returned so the caller can report them.
    pub fn open(
        backend: B,
        clock: C,
        recovery_roots: &[PathBuf],
        stale_after_ms: u64,
    ) -> Result<(Self, Vec<Recovered>)> {
        let now = clock.unix_ms();
        let recovered = recover_stale(recovery_roots, now, stale_after_ms)
            .map_err(|e| io_err("recover stale takes", &e))?;
        Ok((
            Recorder {
                backend,
                clock,
                stale_after_ms,
                session: None,
            },
            recovered,
        ))
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn clock(&self) -> &C {
        &self.clock
    }

    pub fn stale_after_ms(&self) -> u64 {
        self.stale_after_ms
    }

    pub fn doctor(&self) -> Doctor {
        let os = self.backend.os();
        let build = self.backend.os_build();
        Doctor {
            os,
            helper: self.backend.helper(),
            permission: self.backend.permission(),
            states: ALL_STATES
                .into_iter()
                .map(|state| StateSupport {
                    state,
                    support: support(os, &build, state),
                })
                .collect(),
            os_build: build,
        }
    }

    pub fn sources_list(&self) -> Result<Vec<Source>> {
        self.backend.sources()
    }

    /// `record.start`. Creates the bundle directory and begins capture.
    pub fn record_start(&mut self, request: StartRequest) -> Result<Status> {
        if self.session.as_ref().is_some_and(|s| s.phase.is_active()) {
            return Err(RecordError::new(
                Code::AlreadyRecording,
                "a take is already active",
            ));
        }
        validate_request(&request)?;
        let os = self.backend.os();
        let os_build = self.backend.os_build();
        if let Support::Unsupported(reason) = support(os, &os_build, request.visibility) {
            return Err(RecordError::new(
                Code::Unsupported,
                format!("{:?}: {reason}", request.visibility),
            ));
        }
        let masking = masking_json(&request.masking).ok_or_else(|| {
            RecordError::new(
                Code::MaskGeometryUnknown,
                "pixel mask geometry is unknown; the take is refused before capture",
            )
        })?;

        fs::create_dir_all(&request.out).map_err(|e| io_err("create bundle directory", &e))?;
        if self.backend.permission() == Permission::Denied {
            return Err(RecordError::new(
                Code::PermissionDenied,
                "screen recording is not granted to this helper identity; nothing was written",
            ));
        }

        let now = self.clock.unix_ms();
        let bundle_id = format!(
            "bundle.{now}.{}",
            NEXT_BUNDLE.fetch_add(1, Ordering::Relaxed)
        );
        let helper = self.backend.helper();
        let state = State {
            phase: Phase::Starting,
            bundle_id: bundle_id.clone(),
            created_at: iso8601_utc(now),
            heartbeat_ms: now,
            recorder: json!({
                "engine": "rightkit-record",
                "version": env!("CARGO_PKG_VERSION"),
                "os": os,
                "osBuild": os_build,
                "helper": { "identity": helper.identity, "signed": helper.signed },
            }),
            target: target_json(&request.target),
            dependencies: serde_json::to_value(&request.dependencies).unwrap_or(Value::Null),
            pixel_masking: masking,
            geometry_epochs: Vec::new(),
            clock: None,
        };
        let scenario = scenario_json(&request.scenario, &request.redaction);
        let writer = Writer::create(&request.out, state, &scenario)
            .map_err(|e| io_err("create bundle files", &e))?;
        let mut session = Session {
            dir: request.out.clone(),
            bundle_id,
            writer: Some(writer),
            phase: Phase::Starting,
            redaction: request.redaction.clone(),
            host_ns_at_start: 0,
            epoch: 0,
            ready: false,
            frames: 0,
            dropped: 0,
            last_index: None,
            last_pts: None,
            failure: None,
            outcome: None,
            pending: None,
        };

        let plan = BackendPlan {
            out_dir: request.out.clone(),
            window_id: request.target.window_id,
            pid: request.target.pid,
            fps: request.fps,
            scale: request.scale,
            audio: request.audio.clone(),
            visibility: request.visibility,
            masking: request.masking.clone(),
        };
        let info = match self.backend.start(&plan) {
            Ok(info) => info,
            Err(err) => {
                self.session = Some(session);
                // The take never began, so nothing is recoverable. A failed seal
                // leaves the take for the next sweep, which is also safe.
                let _ = self.finish(
                    BundleStatus::Invalid,
                    Some(format!("start_failed: {}", err.message)),
                    Vec::new(),
                );
                return Err(err);
            }
        };
        session.host_ns_at_start = info.host_ns_at_start;
        let capture = capture_json(&request, &info);
        {
            let writer = session.writer();
            writer
                .write_capture(&capture)
                .map_err(|e| io_err("write capture.json", &e))?;
            writer.state.geometry_epochs.push(epoch_json(
                0,
                0,
                info.bounds_pt,
                info.scale,
                info.dpi,
            ));
            writer.state.phase = Phase::Recording;
            writer.save_state().map_err(|e| io_err("save state", &e))?;
        }
        session.phase = Phase::Recording;
        self.session = Some(session);
        self.status()
    }

    /// Appends a caller event (driver telemetry: admission, input, postcondition).
    /// Redaction runs before the event is written. Frame events are owned by the
    /// recorder and are refused here.
    pub fn append_event(&mut self, event: RecordEvent) -> Result<()> {
        if matches!(event.kind, EventKind::FrameReady | EventKind::FrameDropped) {
            return Err(RecordError::new(
                Code::ArgInvalid,
                "frame events come from the helper's frames, not from the caller",
            ));
        }
        if !self.recording() {
            return Err(RecordError::new(Code::NotRecording, "no take is recording"));
        }
        let session = self.session.as_mut().expect("recording implies a session");
        let value = session.encode(event).map_err(|message| {
            RecordError::new(
                Code::ArgInvalid,
                format!("event does not validate: {message}"),
            )
        })?;
        match session.append(value) {
            Ok(()) => Ok(()),
            Err(failure) => {
                let detail = failure.reason();
                self.end_with_failure(failure)?;
                Err(RecordError::new(
                    Code::Helper,
                    format!("the take ended while writing an event: {detail}"),
                ))
            }
        }
    }

    /// Drains helper events into the bundle. A failure event ends the take.
    pub fn pump(&mut self) -> Result<()> {
        if !self.recording() {
            return Ok(());
        }
        let batch = self.backend.poll();
        for event in batch {
            if let Some(failure) = self.apply(event) {
                self.end_with_failure(failure)?;
                return Ok(());
            }
        }
        self.heartbeat()
    }

    /// Drains pending events, then reports the current state.
    pub fn record_status(&mut self) -> Result<Status> {
        self.pump()?;
        self.status()
    }

    /// `record.stop`. Idempotent: after the take ends, returns its outcome.
    pub fn record_stop(&mut self) -> Result<Outcome> {
        let session = self.session.as_ref().ok_or_else(not_recording)?;
        if let Some(outcome) = &session.outcome {
            return Ok(outcome.clone());
        }
        self.pump()?;
        if let Some(outcome) = self.session.as_ref().and_then(|s| s.outcome.clone()) {
            return Ok(outcome);
        }
        let pending = self.session.as_mut().and_then(|s| s.pending.take());
        if let Some(pending) = pending {
            return self.finish(pending.status, pending.reason, pending.tracks);
        }

        let session = self.session.as_mut().expect("checked above");
        session.phase = Phase::Stopping;
        {
            let writer = session.writer();
            writer.state.phase = Phase::Stopping;
            writer.save_state().map_err(|e| io_err("save state", &e))?;
        }

        match self.backend.stop() {
            Err(err) => {
                let failure = if err.code == Code::Helper {
                    Failure::HelperLost
                } else {
                    Failure::Io(err.message)
                };
                self.end_with_failure(failure)
            }
            Ok(finished) => {
                for event in self.backend.poll() {
                    if let Some(failure) = self.apply(event) {
                        return self.end_with_failure(failure);
                    }
                }
                if !finished.clean {
                    return self.end_with_failure(Failure::Io("media finished unclean".into()));
                }
                if !self.session.as_ref().is_some_and(|s| s.ready) {
                    return self.finish(
                        BundleStatus::Invalid,
                        Some("no_accepted_frames".into()),
                        finished.tracks,
                    );
                }
                self.finish(BundleStatus::Complete, None, finished.tracks)
            }
        }
    }

    /// Verifies a sealed bundle. `options` lists plaintext values that must not
    /// appear in any persisted text file.
    pub fn bundle_verify(&self, dir: &Path, options: &VerifyOptions) -> Result<VerifyReport> {
        verify_bundle(dir, options)
    }

    fn recording(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.phase == Phase::Recording)
    }

    fn status(&self) -> Result<Status> {
        let session = self.session.as_ref().ok_or_else(not_recording)?;
        Ok(Status {
            phase: session.phase,
            bundle_id: session.bundle_id.clone(),
            dir: session.dir.clone(),
            frames: session.frames,
            dropped: session.dropped,
            failure: session.failure.as_ref().map(Failure::reason),
            outcome: session.outcome.clone(),
        })
    }

    fn heartbeat(&mut self) -> Result<()> {
        let now = self.clock.unix_ms();
        let session = self.session.as_mut().expect("recording implies a session");
        let writer = session.writer();
        writer.state.heartbeat_ms = now;
        writer
            .save_state()
            .map_err(|e| io_err("save heartbeat", &e))
    }

    /// Applies one helper event. Returns the failure, if the event ends the take.
    fn apply(&mut self, event: HelperEvent) -> Option<Failure> {
        match event {
            HelperEvent::Ready(frame) => self.accept(frame, true),
            HelperEvent::Frame(frame) => self.accept(frame, false),
            HelperEvent::Dropped {
                host_ns,
                count,
                interval_ms,
                reason,
            } => self.drop_frames(host_ns, count, interval_ms, reason),
            HelperEvent::Geometry {
                host_ns,
                bounds_pt,
                scale,
                dpi,
            } => self.geometry(host_ns, bounds_pt, scale, dpi),
            HelperEvent::Failed(failure) => Some(failure),
            HelperEvent::Exited => Some(Failure::HelperLost),
        }
    }

    fn accept(&mut self, frame: FrameSample, marker: bool) -> Option<Failure> {
        let host_clock = host_clock_name(self.backend.os());
        let session = self.session.as_mut()?;
        if marker && session.ready {
            return Some(Failure::Protocol("readiness marker sent twice".into()));
        }
        if !marker && !session.ready {
            return Some(Failure::Protocol(
                "frame before the readiness marker".into(),
            ));
        }
        if !is_blake3_text(&frame.raw_hash) {
            return Some(Failure::Protocol(
                "raw frame hash is not blake3:<hex>".into(),
            ));
        }
        if session.last_index.is_some_and(|last| frame.index <= last) {
            return Some(Failure::Protocol("frame index did not increase".into()));
        }
        if session
            .last_pts
            .is_some_and(|last| frame.sample_pts_ns <= last)
        {
            return Some(Failure::Protocol("sample PTS did not increase".into()));
        }
        if marker {
            let clock = clock_json(host_clock, session.host_ns_at_start, &frame);
            session.writer().state.clock = Some(clock);
            if let Err(e) = session.writer().save_state() {
                return Some(classify_io(e));
            }
        }
        let mut event = base_event(
            EventKind::FrameReady,
            frame.host_ns,
            session.epoch,
            &session.redaction,
        );
        event.frame = Some(Frame {
            index: frame.index,
            sample_pts_ns: frame.sample_pts_ns,
        });
        if let Err(failure) = session.persist(event) {
            return Some(failure);
        }
        let line = json!({
            "index": frame.index,
            "hostNs": frame.host_ns,
            "samplePtsNs": frame.sample_pts_ns,
            "rawBlake3": frame.raw_hash,
        });
        if let Err(e) = session.writer().append_frame(&line) {
            return Some(classify_io(e));
        }
        session.last_index = Some(frame.index);
        session.last_pts = Some(frame.sample_pts_ns);
        session.frames += 1;
        session.ready = true;
        None
    }

    fn drop_frames(
        &mut self,
        host_ns: u64,
        count: u32,
        interval_ms: f64,
        reason: DropReason,
    ) -> Option<Failure> {
        if count == 0 || !interval_ms.is_finite() || interval_ms < 0.0 {
            return Some(Failure::Protocol(
                "dropped-frame report is out of range".into(),
            ));
        }
        let session = self.session.as_mut()?;
        let mut event = base_event(
            EventKind::FrameDropped,
            host_ns,
            session.epoch,
            &session.redaction,
        );
        event.frame_dropped = Some(DropInfo {
            count,
            interval_ms,
            reason,
        });
        session.persist(event).err()
    }

    fn geometry(&mut self, host_ns: u64, bounds: Rect, scale: f64, dpi: u32) -> Option<Failure> {
        if !(scale.is_finite() && scale > 0.0) || !(bounds.width > 0.0 && bounds.height > 0.0) {
            return Some(Failure::Protocol("geometry report is out of range".into()));
        }
        let session = self.session.as_mut()?;
        let at_ms = host_ns.saturating_sub(session.host_ns_at_start) / 1_000_000;
        let writer = session.writer();
        let epochs = &mut writer.state.geometry_epochs;
        if let Some(last) = epochs.last_mut() {
            last["endMs"] = json!(at_ms);
        }
        let index = epochs.len() as u32;
        epochs.push(epoch_json(index, at_ms, bounds, scale, dpi));
        if let Err(e) = writer.save_state() {
            return Some(classify_io(e));
        }
        session.epoch = index;
        None
    }

    /// Ends the take with a failure. The helper is torn down first.
    fn end_with_failure(&mut self, failure: Failure) -> Result<Outcome> {
        self.backend.abort();
        let session = self.session.as_mut().ok_or_else(not_recording)?;
        let status = failure.status(session.ready);
        let reason = failure.reason();
        session.failure = Some(failure);
        self.finish(status, Some(reason), Vec::new())
    }

    /// Seals the take into a terminal state. If sealing fails, the take stays
    /// pending and `record_stop` retries it.
    fn finish(
        &mut self,
        status: BundleStatus,
        reason: Option<String>,
        tracks: Vec<TrackOutput>,
    ) -> Result<Outcome> {
        let session = self.session.as_mut().ok_or_else(not_recording)?;
        session.phase = Phase::Stopping;
        let state = session.writer().state.clone();
        let dir = session.dir.clone();
        match finalize(&dir, &state, status, reason.clone(), &tracks) {
            Ok(done) => {
                let outcome = Outcome {
                    bundle_id: session.bundle_id.clone(),
                    dir,
                    status: done.status,
                    reason: done.reason,
                    manifest_problem: done.manifest_problem,
                };
                session.phase = Phase::from_status(done.status);
                session.writer = None;
                session.pending = None;
                session.outcome = Some(outcome.clone());
                Ok(outcome)
            }
            Err(error) => {
                session.pending = Some(Pending {
                    status,
                    reason,
                    tracks,
                });
                Err(io_err("seal bundle", &error))
            }
        }
    }
}

impl Failure {
    /// The status a take ends in when this failure occurs. Permission revocation
    /// and protocol violations make the take invalid. The other failures keep the
    /// media that was written, quarantined. Without a readiness marker there is no
    /// clock, so nothing is recoverable and the take is invalid.
    pub fn status(&self, ready: bool) -> BundleStatus {
        if !ready {
            return BundleStatus::Invalid;
        }
        match self {
            Failure::PermissionRevoked | Failure::Protocol(_) => BundleStatus::Invalid,
            _ => BundleStatus::PartialQuarantined,
        }
    }
}

fn not_recording() -> RecordError {
    RecordError::new(Code::NotRecording, "no take has been started")
}

fn classify_io(error: io::Error) -> Failure {
    if is_disk_full(&error) {
        Failure::DiskFull
    } else {
        Failure::Io(error.to_string())
    }
}

/// `ENOSPC` on Unix. `ERROR_DISK_FULL` and `ERROR_HANDLE_DISK_FULL` on Windows.
fn is_disk_full(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(28)
    }
    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(112) | Some(39))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = error;
        false
    }
}

fn host_clock_name(os: RecorderOs) -> &'static str {
    match os {
        RecorderOs::Macos => "mach_absolute_time",
        RecorderOs::Windows => "qpc",
    }
}

fn base_event(kind: EventKind, host_ns: u64, epoch: u32, policy: &RedactionPolicy) -> RecordEvent {
    RecordEvent {
        contract: "record-event".into(),
        contract_version: version(Contract::RecordEvent),
        session_id: String::new(),
        seq: 0,
        kind,
        input_kind: None,
        phase: None,
        effect_id: None,
        sub_event_id: None,
        step_index: 0,
        step_id: None,
        host_ns,
        timing: None,
        geometry_epoch: epoch,
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
    }
}

/// The clock map anchored to the first accepted frame. Sample PTS equals
/// host time plus this offset, and encoded PTS equals sample PTS.
fn clock_json(host_clock: &str, host_ns_at_start: u64, first: &FrameSample) -> Value {
    json!({
        "hostClock": host_clock,
        "hostNsAtStart": host_ns_at_start,
        "firstAcceptedFrame": {
            "frameIndex": first.index,
            "hostNs": first.host_ns,
            "samplePtsNs": first.sample_pts_ns,
        },
        "mapping": {
            "hostToSamplePtsOffsetNs": first.sample_pts_ns as i64 - first.host_ns as i64,
            "samplePtsToEncodedPtsOffsetNs": 0,
            "timescale": 1_000_000_000u64,
        },
    })
}

fn epoch_json(epoch: u32, start_ms: u64, bounds: Rect, scale: f64, dpi: u32) -> Value {
    json!({
        "epoch": epoch,
        "startMs": start_ms,
        "boundsPt": {
            "x": bounds.x,
            "y": bounds.y,
            "width": bounds.width,
            "height": bounds.height,
        },
        "scale": scale,
        "dpi": dpi,
    })
}

fn capture_json(request: &StartRequest, info: &StartInfo) -> Value {
    json!({
        "fps": request.fps,
        "scale": info.scale,
        "codec": info.codec,
        "width": info.width,
        "height": info.height,
        "scope": "app_surfaces",
        "displayOptIn": false,
        "visibilityState": request.visibility,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn enospc_is_disk_full_and_other_io_is_not() {
        assert_eq!(
            classify_io(io::Error::from_raw_os_error(28)),
            Failure::DiskFull
        );
        assert!(matches!(
            classify_io(io::Error::from_raw_os_error(13)),
            Failure::Io(_)
        ));
    }
}
