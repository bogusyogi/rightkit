//! Finish a calibration capture: stop the recorder, hand the PCM to the host engine, and report
//! what happened as a typed [`Outcome`].
//!
//! The order is owned here; the effects are the host hooks:
//!
//! 1. stop the recorder for `session` ([`Recorder::stop`]);
//! 2. `on_captured` (the host tells its UI the capture succeeded);
//! 3. reserve an owned per-run file beside `{session}.{extension}` ([`OwnedTemp`]: an exclusive
//!    empty file plus an ownership record naming this process) and write the PCM into it;
//! 4. `activate_model`, then `run_engine` over that owned file;
//! 5. discard the owned file explicitly (`OwnedTemp::discard`, fallible) on every normal path:
//!    after a failed write, before a model-unavailable outcome is returned (so a host that logs
//!    on that outcome logs after the attempt), and before any resident restart. A discard that
//!    fails is never reported as removal: the outcome carries [`Cleanup::Failed`], the file and
//!    its owner record are kept (eligible for recovery after this process exits, see
//!    [`sweep_dead_runs`]), and the flow still continues in the old
//!    order (the resident restart then runs while the file may still exist). A panic falls back
//!    to the `OwnedTemp` drop;
//! 6. only when the report [`passed`](CalibrationReport::passed), `restart_resident`.
//!
//! The PCM is never published under `{session}.{extension}`: the engine reads the owned file
//! (named `.{session}.{extension}.rkowned-<pid>-<n>`, so it has no `{extension}` suffix). The
//! engine path in `rightkit-dictation-runtime` reads the path with `std::fs::read` and does not
//! look at the extension; a host whose engine needs the suffix cannot use this function as is.
//!
//! Ownership and cleanup: the reservation is exclusive, so a file this run holds is provably its
//! own, and two runs never share a path. The record stays until the file is removed, so it is
//! there for the whole engine read. If the process dies mid-run, [`sweep_dead_runs`] removes the
//! file when a later start finds the recorded process confirmed dead; it keeps the files of this
//! process and of any live or unprovable process, so a start never removes a file a finishing run
//! is still reading. Nothing here sweeps by extension.
//!
//! Privacy: `pcm_dir` is made owner-only with `rightkit_fs::ensure_private_dir` (Unix mode 0700,
//! Windows protected owner-only ACL; a symlinked leaf is refused, and on macOS a parent ACL that
//! would give the directory an inheritable allow entry is refused, both as `PcmDirFailed`).
//! Newly created ancestors are owner-only; existing ancestors are left as they are. The file
//! inside is created by the standard library with the platform default permissions, so what
//! protects it is the owner-only directory, not a mode on the file. The write is not flushed to
//! disk (the reader is a child of this process on the same machine).
//!
//! The first passing attempt's WER is selected here ([`selected_wer`]) from the host's neutral
//! [`AttemptSummary`] values.
//!
//! No copy, wire code, event name, directory or file name lives here. [`Outcome`] carries no crate
//! text: the only strings in it are ones a host hook returned (the capture worker's error text,
//! the host's model-unavailable text, the host's resident start error). The host matches every
//! variant and words its own messages. The engine's own error text is dropped on purpose (it can
//! carry paths); the host's `run_engine` hook is the place to log it.
//!
//! The PCM file is plain `f32` little-endian ([`f32le_bytes`]), unconditioned.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path};

use rightkit_fs::{ensure_private_dir, OwnedTemp};

use super::calibration_capture::{f32le_bytes, CaptureSettings, Recorder, StopOutcome};

/// One engine attempt, as far as WER selection needs it. The host maps its wire type to this.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttemptSummary {
    /// Whether this attempt passed.
    pub passed: bool,
    /// The attempt's WER, when it reported one.
    pub wer: Option<f32>,
}

/// The host's engine result, seen only as far as the order needs it.
pub trait CalibrationReport {
    /// Whether the calibration passed. Only a passing report restarts the resident engine.
    fn passed(&self) -> bool;
    /// The attempts in the order the engine made them.
    fn attempts(&self) -> impl Iterator<Item = AttemptSummary> + '_;
}

/// The WER of the first attempt that passed. A first passing attempt without a WER is `None` (a
/// later passing attempt is not consulted); no passing attempt is `None`.
pub fn selected_wer<R: CalibrationReport>(report: &R) -> Option<f32> {
    report
        .attempts()
        .find(|attempt| attempt.passed)
        .and_then(|attempt| attempt.wer)
}

/// What happened to this run's PCM file when [`finish`] tried to discard it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleanup {
    /// No file had been reserved.
    NotNeeded,
    /// The file and its owner record are gone.
    Removed,
    /// The discard failed: the captured audio may still be on disk, and its owner record is kept
    /// so it is eligible for recovery after this process exits ([`sweep_dead_runs`] keeps the
    /// files of a live process, this one included). The host words and logs
    /// this itself.
    Failed,
}

/// Why the resident engine could not be restarted after a passing calibration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResidentFailure {
    /// The speech model is not available; carries the host text.
    Model(String),
    /// The resident engine did not start; carries the host text.
    Engine(String),
}

/// The host side of [`finish`].
pub struct SessionHooks<'a, T> {
    /// Directory the owned PCM files are created in. Created if missing.
    pub pcm_dir: &'a Path,
    /// Ownership label recorded with each run and matched by [`sweep_dead_runs`]. Non-empty, one
    /// line, no control characters; otherwise the write is refused (`PcmWriteFailed`).
    pub owner: &'a str,
    /// File extension without a dot (for example the raw-PCM extension the engine expects).
    /// Must make a single plain file name with the session id; otherwise the write is refused
    /// (`PcmWriteFailed`).
    pub extension: &'a str,
    /// Reason handed to `Recorder::stop` for the media restore diagnostic.
    pub restore_reason: &'a str,
    /// Called once, after a capture of at least the minimum length and before anything is written.
    pub on_captured: &'a dyn Fn(),
    /// Make the speech model usable in this process. `Err` carries the host text and ends the
    /// session as [`Outcome::ModelUnavailable`] with no engine run. Do not log the failure in the
    /// hook: log on the outcome, which is returned after the PCM file is removed.
    pub activate_model: &'a dyn Fn() -> Result<(), String>,
    /// Run the engine over the PCM file. Any `Err` is [`Outcome::EngineFailed`]; its text is
    /// dropped.
    pub run_engine: &'a dyn Fn(&Path) -> Result<T, String>,
    /// Restart the resident engine on the settled route. Called only after a passing report, after
    /// PCM discard has been attempted. The host may log its own failure inside the hook.
    pub restart_resident: &'a dyn Fn() -> Result<(), ResidentFailure>,
}

/// What [`finish`] found. Every variant is the host to word; none carries crate copy.
pub enum Outcome<T> {
    /// No capture is active.
    NotRecording,
    /// A capture is active but belongs to another session; it is left in place.
    SessionMismatch,
    /// The capture worker recorded a failure; carries its code text.
    CaptureError(String),
    /// Shorter than the minimum length.
    TooShort,
    /// `pcm_dir` could not be created.
    PcmDirFailed,
    /// The PCM file name was not a single plain component, the owned file could not be reserved,
    /// or the write failed. A reserved file is removed; nothing else is touched.
    PcmWriteFailed { cleanup: Cleanup },
    /// `activate_model` refused; carries the host text. The discard was attempted before this
    /// return.
    ModelUnavailable { error: String, cleanup: Cleanup },
    /// `run_engine` failed.
    EngineFailed { cleanup: Cleanup },
    /// The engine ran. Passing or not is the host to read from `result`; a passing report's
    /// resident engine restarted. `wer` is [`selected_wer`] of the report. The discard was
    /// attempted before the restart; `cleanup` says whether it worked.
    Finished {
        result: T,
        wer: Option<f32>,
        cleanup: Cleanup,
    },
    /// The report passed but the resident restart model step refused; carries the host text.
    ResidentModelUnavailable { error: String, cleanup: Cleanup },
    /// The report passed but the resident engine could not start; carries the host text.
    ResidentStartFailed {
        result: T,
        wer: Option<f32>,
        error: String,
        cleanup: Cleanup,
    },
}

/// Discard `pcm` explicitly and report whether it is gone. On failure the owner record is kept
/// (the fallible discard of `rightkit-fs`), never dropped silently.
fn discard(pcm: OwnedTemp) -> Cleanup {
    match pcm.discard() {
        Ok(()) => Cleanup::Removed,
        Err(_) => Cleanup::Failed,
    }
}

/// Remove the PCM files of earlier runs whose process is confirmed dead: only files reserved by
/// [`finish`] with this `owner` label, only when their record names a dead process and the file
/// is a regular file. Files of this process, of live processes and of processes whose liveness
/// cannot be established are kept, as is anything without a record. Returns how many files were
/// removed; an unreadable directory removes none. Call it from the recorder start hook instead of
/// a sweep by extension.
pub fn sweep_dead_runs(dir: &Path, owner: &str) -> usize {
    rightkit_fs::sweep_owned_temps(dir, owner)
        .map(|report| report.removed)
        .unwrap_or(0)
}

fn single_plain_component(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(part)), None) if part == std::ffi::OsStr::new(name)
    )
}

/// Stop the capture for `session`, run the host engine over it and return what happened.
/// Blocking (joins the capture worker and runs the engine): call it off the async runtime.
pub fn finish<T: CalibrationReport>(
    recorder: &Recorder,
    settings: &CaptureSettings,
    session: &str,
    hooks: &SessionHooks<'_, T>,
) -> Outcome<T> {
    let samples = match recorder.stop(session, settings, hooks.restore_reason) {
        StopOutcome::Captured(samples) => samples,
        StopOutcome::NotRecording => return Outcome::NotRecording,
        StopOutcome::SessionMismatch => return Outcome::SessionMismatch,
        StopOutcome::CaptureError(error) => return Outcome::CaptureError(error),
        StopOutcome::TooShort => return Outcome::TooShort,
    };
    (hooks.on_captured)();
    if ensure_private_dir(hooks.pcm_dir).is_err() {
        return Outcome::PcmDirFailed;
    }
    let file_name = format!("{session}.{}", hooks.extension);
    if !single_plain_component(&file_name) {
        return Outcome::PcmWriteFailed {
            cleanup: Cleanup::NotNeeded,
        };
    }
    let target = hooks.pcm_dir.join(file_name);
    // The reservation is exclusive, so everything this run holds is provably its own. Every
    // normal path below discards `pcm` explicitly; the drop only covers a panic.
    let Ok(pcm) = OwnedTemp::create(&target, hooks.owner) else {
        return Outcome::PcmWriteFailed {
            cleanup: Cleanup::NotNeeded,
        };
    };
    let bytes = f32le_bytes(&samples);
    let written = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(pcm.path())
        .and_then(|mut file| file.write_all(&bytes));
    if written.is_err() {
        return Outcome::PcmWriteFailed {
            cleanup: discard(pcm),
        };
    }
    // Activate the model in this process first, so the engine run and the resident engine that
    // follows resolve the same model.
    if let Err(error) = (hooks.activate_model)() {
        return Outcome::ModelUnavailable {
            error,
            cleanup: discard(pcm),
        };
    }
    let result = (hooks.run_engine)(pcm.path());
    // Discarded here, ahead of any resident restart, exactly where the host removed it before. A
    // failed discard is reported, not hidden.
    let cleanup = discard(pcm);
    let Ok(result) = result else {
        return Outcome::EngineFailed { cleanup };
    };
    let wer = selected_wer(&result);
    if !result.passed() {
        return Outcome::Finished {
            result,
            wer,
            cleanup,
        };
    }
    match (hooks.restart_resident)() {
        Ok(()) => Outcome::Finished {
            result,
            wer,
            cleanup,
        },
        Err(ResidentFailure::Model(error)) => Outcome::ResidentModelUnavailable { error, cleanup },
        Err(ResidentFailure::Engine(error)) => Outcome::ResidentStartFailed {
            result,
            wer,
            error,
            cleanup,
        },
    }
}
