//! What a calibration session reports to a setup step: the [`Outcome`] of
//! [`finish`](super::calibration_session::finish) mapped to a result status, a failure code, a
//! selected route and a WER, plus the wire shapes a setup step sends its UI.
//!
//! The mapping is owned here; the words are the host's. [`settle`] never builds a message: it
//! returns a [`Situation`] that names which of the host's messages applies, the host text a hook
//! returned ([`Settled::detail`]), and the [`SetupSummary`] the host turns into a [`SetupResult`]
//! with its own message ([`SetupSummary::into_result`]).
//!
//! * [`CalibrationSummary`]: what the mapping needs of the host's engine result beyond
//!   [`CalibrationReport`] (its status, selected route and failure code).
//! * [`settle`]: `Outcome` to [`Settled`], or a [`Rejection`] for the outcomes that are errors
//!   of the call itself (no capture, another session, a PCM file that could not be written).
//! * [`SetupStarted`], [`SetupProgress`], [`SetupResult`]: the camelCase wire shapes.
//! * [`start_error_message`], [`cancel_accepted`], [`resident_failure_text`]: the small mappings
//!   around start, cancel and the resident restart.
//!
//! Wire codes that mean the same in every host live here ([`STATUS_PASSED`], [`STATUS_FAILED`],
//! [`STATUS_CAPTURE_FAILED`], [`CODE_CAPTURE_TOO_SHORT`], [`CODE_RECOGNITION_PROVIDER_FAILED`]).
//! The two codes that name a host's own model and engine ([`SetupCodes`]) come from the host.
//! No copy, event name, directory or file name lives here.

use serde::Serialize;

use super::calibration_capture::{CancelOutcome, CaptureSettings, StartError};
use super::calibration_session::{CalibrationReport, Cleanup, Outcome, ResidentFailure};

/// `status` of a setup result whose calibration passed.
pub const STATUS_PASSED: &str = "passed";
/// `status` of a setup result that did not pass, or whose model or engine did not start.
pub const STATUS_FAILED: &str = "failed";
/// `status` of a setup result whose capture failed or was too short.
pub const STATUS_CAPTURE_FAILED: &str = "capture_failed";
/// Failure code of a capture shorter than the minimum length.
pub const CODE_CAPTURE_TOO_SHORT: &str = "capture_too_short";
/// Failure code of a calibration engine that could not be run or decoded.
pub const CODE_RECOGNITION_PROVIDER_FAILED: &str = "recognition_provider_failed";

/// Progress phase while the capture is recording.
pub const PHASE_RECORDING: &str = "recording";
/// Progress phase once the capture was accepted and the engine test starts.
pub const PHASE_CAPTURED: &str = "captured";
/// Progress phase of a setup that passed.
pub const PHASE_COMPLETE: &str = "complete";
/// Progress phase of a setup that failed.
pub const PHASE_FAILED: &str = "failed";

/// The host's engine result as far as the result mapping needs it.
pub trait CalibrationSummary: CalibrationReport {
    /// The host's route type, handed back unchanged in the setup result.
    type Route: Clone;
    /// The engine's own status; [`STATUS_PASSED`] means the calibration passed.
    fn status(&self) -> &str;
    /// The route the engine selected, when it selected one.
    fn selected_route(&self) -> Option<&Self::Route>;
    /// The engine's failure code, when it reported one.
    fn failure_code(&self) -> Option<&str>;
}

/// The two failure codes that name the host's own model and engine.
#[derive(Debug, Clone, Copy)]
pub struct SetupCodes<'a> {
    /// Code of a speech model that is not available.
    pub model_unavailable: &'a str,
    /// Code of a resident engine that did not start after a passing calibration.
    pub engine_start_failed: &'a str,
}

/// Which message of the host applies. Every variant but `Passed` and `NotPassed` is a failure the
/// host words itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Situation {
    /// The capture worker recorded a failure ([`Settled::summary`] carries its code).
    CaptureFailed,
    /// The recording was shorter than the minimum.
    TooShort,
    /// `activate_model` refused ([`Settled::detail`] carries the host text).
    ModelUnavailable,
    /// The engine could not be run or decoded.
    EngineFailed,
    /// The engine ran and its status is [`STATUS_PASSED`]; the resident engine restarted.
    Passed,
    /// The engine ran and its status is anything else.
    NotPassed,
    /// The report passed but the resident restart found no model ([`Settled::detail`]).
    ResidentModelUnavailable,
    /// The report passed but the resident engine did not start ([`Settled::detail`]).
    ResidentStartFailed,
}

impl Situation {
    /// The progress phase this situation ends in: [`PHASE_COMPLETE`] for `Passed`, otherwise
    /// [`PHASE_FAILED`].
    pub fn phase(self) -> &'static str {
        if self == Self::Passed {
            PHASE_COMPLETE
        } else {
            PHASE_FAILED
        }
    }
}

/// The outcomes that are errors of the call itself; the host words each as an error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// No capture is active.
    NotRecording,
    /// A capture is active but belongs to another session.
    SessionMismatch,
    /// The calibration directory could not be created.
    PcmDirFailed,
    /// The PCM file could not be reserved or written; `cleanup` says what became of it.
    PcmWriteFailed { cleanup: Cleanup },
}

/// A setup result before the host words its message.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupSummary<R> {
    pub status: String,
    pub selected_route: Option<R>,
    pub wer: Option<f32>,
    pub failure_code: Option<String>,
}

impl<R> SetupSummary<R> {
    /// The wire result with the host's message.
    pub fn into_result(self, message: String) -> SetupResult<R> {
        SetupResult {
            status: self.status,
            selected_route: self.selected_route,
            wer: self.wer,
            failure_code: self.failure_code,
            message,
        }
    }
}

/// What [`settle`] found for an outcome that is a result.
#[derive(Debug, Clone, PartialEq)]
pub struct Settled<R> {
    /// Which of the host's messages applies.
    pub situation: Situation,
    /// The host text a hook returned (`ModelUnavailable`, `ResidentModelUnavailable`,
    /// `ResidentStartFailed`); `None` otherwise.
    pub detail: Option<String>,
    /// What became of the PCM file; [`Cleanup::NotNeeded`] when none was reserved.
    pub cleanup: Cleanup,
    /// The result without its message.
    pub summary: SetupSummary<R>,
}

/// Map `outcome` to the result a setup step shows.
///
/// The mapping: a capture error is `capture_failed` with the worker's code; too short is
/// `capture_failed` with [`CODE_CAPTURE_TOO_SHORT`]; a model refusal (at activation or at the
/// resident restart) is `failed` with `codes.model_unavailable` and no route or WER; an engine
/// failure is `failed` with [`CODE_RECOGNITION_PROVIDER_FAILED`]; a finished run carries the
/// engine's own status, route and failure code and the selected WER; a resident engine that did
/// not start after a pass is `failed` with `codes.engine_start_failed`, keeping the selected
/// route and the WER. A finished run is [`Situation::Passed`] exactly when the engine's status is
/// [`STATUS_PASSED`].
pub fn settle<T: CalibrationSummary>(
    outcome: Outcome<T>,
    codes: &SetupCodes<'_>,
) -> Result<Settled<T::Route>, Rejection> {
    fn failed<R>(
        situation: Situation,
        status: &str,
        code: &str,
        detail: Option<String>,
        cleanup: Cleanup,
    ) -> Settled<R> {
        Settled {
            situation,
            detail,
            cleanup,
            summary: SetupSummary {
                status: status.to_string(),
                selected_route: None,
                wer: None,
                failure_code: Some(code.to_string()),
            },
        }
    }
    Ok(match outcome {
        Outcome::NotRecording => return Err(Rejection::NotRecording),
        Outcome::SessionMismatch => return Err(Rejection::SessionMismatch),
        Outcome::PcmDirFailed => return Err(Rejection::PcmDirFailed),
        Outcome::PcmWriteFailed { cleanup } => return Err(Rejection::PcmWriteFailed { cleanup }),
        Outcome::CaptureError(error) => failed(
            Situation::CaptureFailed,
            STATUS_CAPTURE_FAILED,
            &error,
            None,
            Cleanup::NotNeeded,
        ),
        Outcome::TooShort => failed(
            Situation::TooShort,
            STATUS_CAPTURE_FAILED,
            CODE_CAPTURE_TOO_SHORT,
            None,
            Cleanup::NotNeeded,
        ),
        Outcome::ModelUnavailable { error, cleanup } => failed(
            Situation::ModelUnavailable,
            STATUS_FAILED,
            codes.model_unavailable,
            Some(error),
            cleanup,
        ),
        Outcome::ResidentModelUnavailable { error, cleanup } => failed(
            Situation::ResidentModelUnavailable,
            STATUS_FAILED,
            codes.model_unavailable,
            Some(error),
            cleanup,
        ),
        Outcome::EngineFailed { cleanup } => failed(
            Situation::EngineFailed,
            STATUS_FAILED,
            CODE_RECOGNITION_PROVIDER_FAILED,
            None,
            cleanup,
        ),
        Outcome::Finished {
            result,
            wer,
            cleanup,
        } => Settled {
            situation: if result.status() == STATUS_PASSED {
                Situation::Passed
            } else {
                Situation::NotPassed
            },
            detail: None,
            cleanup,
            summary: SetupSummary {
                status: result.status().to_string(),
                selected_route: result.selected_route().cloned(),
                wer,
                failure_code: result.failure_code().map(str::to_string),
            },
        },
        Outcome::ResidentStartFailed {
            result,
            wer,
            error,
            cleanup,
        } => Settled {
            situation: Situation::ResidentStartFailed,
            detail: Some(error),
            cleanup,
            summary: SetupSummary {
                status: STATUS_FAILED.to_string(),
                selected_route: result.selected_route().cloned(),
                wer,
                failure_code: Some(codes.engine_start_failed.to_string()),
            },
        },
    })
}

/// The error text of a capture that did not start: the host's own refusal text for a device
/// refusal, `{spawn_prefix}: {os error}` for a worker that could not be spawned.
pub fn start_error_message(error: StartError, spawn_prefix: &str) -> String {
    match error {
        StartError::Device(message) => message,
        StartError::Spawn(error) => format!("{spawn_prefix}: {error}"),
    }
}

/// Whether a cancel succeeded: a cancelled capture and an idle recorder both do, another
/// session's capture does not.
pub fn cancel_accepted(outcome: &CancelOutcome) -> bool {
    matches!(outcome, CancelOutcome::Cancelled | CancelOutcome::Idle)
}

/// The diagnostic text of a resident restart failure: `{code}: {error}` with `model_code` for a
/// model failure and `engine_code` for an engine failure.
pub fn resident_failure_text(
    failure: &ResidentFailure,
    model_code: &str,
    engine_code: &str,
) -> String {
    match failure {
        ResidentFailure::Model(error) => format!("{model_code}: {error}"),
        ResidentFailure::Engine(error) => format!("{engine_code}: {error}"),
    }
}

/// The result a setup step returns to its UI (camelCase on the wire).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupResult<R> {
    pub status: String,
    pub selected_route: Option<R>,
    pub wer: Option<f32>,
    pub failure_code: Option<String>,
    pub message: String,
}

/// What a setup step returns when its capture started (camelCase on the wire).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupStarted {
    pub session_id: String,
    pub sample_rate: u32,
    pub reference_version: &'static str,
    pub min_seconds: u32,
    pub target_seconds: u32,
}

impl SetupStarted {
    /// The started record of `session_id`, with the capture limits taken from `settings`.
    pub fn new(
        session_id: String,
        settings: &CaptureSettings,
        reference_version: &'static str,
    ) -> Self {
        Self {
            session_id,
            sample_rate: settings.sample_rate,
            reference_version,
            min_seconds: settings.min_seconds,
            target_seconds: settings.target_seconds,
        }
    }
}

/// A progress event payload of a setup step (camelCase on the wire). The route fields stay
/// `null`: the setup step reports phases, not routes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupProgress<'a> {
    pub session_id: &'a str,
    pub phase: &'a str,
    pub route_index: Option<usize>,
    pub route_count: Option<usize>,
    pub route_id: Option<&'a str>,
    pub message: &'a str,
}

impl<'a> SetupProgress<'a> {
    /// A phase update with no route fields.
    pub fn new(session_id: &'a str, phase: &'a str, message: &'a str) -> Self {
        Self {
            session_id,
            phase,
            route_index: None,
            route_count: None,
            route_id: None,
            message,
        }
    }
}
