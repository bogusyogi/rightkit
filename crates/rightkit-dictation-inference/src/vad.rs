use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use rightkit_vad::{FrameScorer, FrameStream};

const FRAME_SAMPLES: usize = 512;
#[cfg(target_os = "macos")]
const CONTEXT_SAMPLES: usize = 64;
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);
/// Cap on samples queued while the model loads (30 s); `FrameStream` applies
/// the same cap once Ready.
const MAX_PENDING_SAMPLES: usize = 16_000 * 30;
// Runtime VAD threshold. Real machines vary wildly — Windows WASAPI shared
// mode produced peak Silero scores far below the original 0.5 threshold, so
// the default is the lower field-proven floor. Optional calibration can raise
// it for fixed-room experiments, but it is not enabled by default because PTT
// often captures the user's first syllable rather than ambient noise.
const MIN_SPEECH_THRESHOLD: f32 = 0.20;
const SPEECH_THRESHOLD_RMS_MULTIPLIER: f32 = 8.0;
/// Cap on noise-derived threshold so a totally silent room (RMS ≈ 0)
/// doesn't push the threshold to 0 and fire on anything.
const MIN_NOISE_FLOOR_FOR_CALIBRATION: f32 = 0.005;

const MODEL_FILE: &str = "silero_vad_16k_op15.onnx";
#[cfg(target_os = "windows")]
const ORT_DYLIB_FILE: &str = "onnxruntime.dll";
#[cfg(target_os = "macos")]
const ORT_DYLIB_FILE: &str = "libonnxruntime.dylib";
#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
const ORT_DYLIB_FILE: &str = "libonnxruntime.so";

pub struct SpeechVad {
    state: LoadState,
    model_path: Option<PathBuf>,
    /// Samples captured while the model was still loading. Handed to the
    /// stream, in order, on the first observation after it becomes Ready.
    loading_pending: Vec<f32>,
    /// Runtime speech threshold. Initialized to MIN_SPEECH_THRESHOLD; optional
    /// calibration can raise it from observed ambient noise.
    speech_threshold: f32,
    /// Runtime-derived RMS noise floor. Consumed by `rightkit-dictation-runtime`'s
    /// worker (`worker/worker_main.rs`) via
    /// `noise_floor_rms()` so the energy gate scales with the room instead
    /// of being a magic constant.
    noise_floor_rms: f32,
    /// Aggregate kept for when the stream no longer exists (Disabled).
    retired_aggregate: (u64, u64),
}

/// Controls may use VAD only once inference is genuinely available. Loading or
/// failed VAD must never be silently interpreted as silence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VadStatus {
    Loading,
    Ready,
    Failed,
}

enum LoadState {
    Loading {
        started: Instant,
        rx: Receiver<Result<(FrameStream<SileroVad>, &'static str), String>>,
    },
    Ready {
        stream: FrameStream<SileroVad>,
        backend: &'static str,
    },
    Disabled,
}

/// Inference backend. Production uses the shared `rightkit-vad` Silero scorer (ORT CPU) on
/// every desktop platform: it owns its state and context and runs its own warm-up frame,
/// which keeps the state. The macOS Core ML variant remains explicit-only for the parity
/// harness (the app's native Core ML VAD stage stays app-owned); keeping live VAD outside
/// Core ML prevents it from sharing a failure domain with the concurrent ASR probe runtime.
enum SileroVad {
    Shared(rightkit_vad::Silero),
    #[cfg(target_os = "macos")]
    CoreMl {
        stage: crate::coreml::Stage,
        state: Vec<f32>,
        context: [f32; CONTEXT_SAMPLES],
    },
}

impl SpeechVad {
    pub fn new() -> Self {
        Self::with_model_path(default_model_path())
    }

    pub fn with_model_path(model_path: Option<PathBuf>) -> Self {
        let mut vad = Self {
            state: LoadState::Disabled,
            model_path,
            loading_pending: Vec::new(),
            speech_threshold: MIN_SPEECH_THRESHOLD,
            noise_floor_rms: 0.0,
            retired_aggregate: (0, 0),
        };
        vad.start_loading();
        vad
    }

    pub fn reset(&mut self) {
        self.loading_pending.clear();
        self.retired_aggregate = (0, 0);
        if let LoadState::Ready { stream, .. } = &mut self.state {
            stream.reset();
        }
    }

    /// Set the speech detection threshold from observed ambient audio.
    /// This is opt-in only; the normal PTT path often captures the user's
    /// first syllable, which is not a valid noise sample.
    pub fn calibrate(&mut self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let rms = rightkit_dictation_core::audio_conditioning::rms(samples);
        // Clamp to MIN_NOISE_FLOOR_FOR_CALIBRATION so a totally silent room
        // doesn't push the threshold to zero (false-fire on anything).
        self.noise_floor_rms = rms.max(MIN_NOISE_FLOOR_FOR_CALIBRATION);
        self.speech_threshold =
            MIN_SPEECH_THRESHOLD.max(self.noise_floor_rms * SPEECH_THRESHOLD_RMS_MULTIPLIER);
        tracing::info!(
            "silero VAD calibrated: noise_floor_rms={:.5} speech_threshold={:.4}",
            self.noise_floor_rms,
            self.speech_threshold,
        );
    }

    /// Current speech threshold (post-calibration). Worker consumes this to
    /// override the energy gate floor too.
    pub fn speech_threshold(&self) -> f32 {
        self.speech_threshold
    }

    /// Measured noise floor RMS. Worker uses this for the energy gate.
    pub fn noise_floor_rms(&self) -> f32 {
        self.noise_floor_rms
    }

    pub fn observe(&mut self, samples: &[f32]) -> bool {
        self.observe_frames(samples)
            .is_some_and(|frames| frames.into_iter().any(|speech| speech))
    }

    /// Observe contiguous PCM and return one verdict for every complete
    /// 32 ms Silero frame consumed. `None` means VAD was not available, so a
    /// caller must treat that region as unknown rather than as silence.
    ///
    /// Terminal KWS cuts use these frame verdicts as their silence authority:
    /// energy can rank already-silent windows, but it cannot itself certify
    /// that command-bearing audio is safe to hand to final ASR.
    pub fn observe_frames(&mut self, samples: &[f32]) -> Option<Vec<bool>> {
        if samples.is_empty() {
            return Some(Vec::new());
        }
        self.poll_loading();
        let threshold = self.speech_threshold;
        let (verdicts, before) = match &mut self.state {
            LoadState::Loading { .. } => {
                // Capture starts before the asynchronous model load can
                // necessarily finish. Preserve those first syllables and feed
                // them once Silero is ready instead of dropping them.
                self.loading_pending.extend_from_slice(samples);
                if self.loading_pending.len() > MAX_PENDING_SAMPLES {
                    let extra = self.loading_pending.len() - MAX_PENDING_SAMPLES;
                    self.loading_pending.drain(..extra);
                }
                return None;
            }
            LoadState::Disabled => {
                self.loading_pending.clear();
                return None;
            }
            LoadState::Ready { stream, .. } => {
                stream.set_threshold(threshold);
                let before = stream.aggregate().unwrap_or((0, 0));
                let verdicts = if self.loading_pending.is_empty() {
                    stream.push(samples)
                } else {
                    self.loading_pending.extend_from_slice(samples);
                    let queued = std::mem::take(&mut self.loading_pending);
                    stream.push(&queued)
                };
                (verdicts, before)
            }
        };
        match verdicts {
            Some(verdicts) => Some(verdicts.speech),
            None => {
                // The scorer logged its error. A failed batch contributes
                // nothing to the aggregate, as before.
                self.retired_aggregate = before;
                self.state = LoadState::Disabled;
                None
            }
        }
    }

    pub fn recording_aggregate(&self) -> Option<(u64, u64)> {
        match &self.state {
            LoadState::Ready { stream, .. } => stream.aggregate(),
            _ => (self.retired_aggregate.0 > 0).then_some(self.retired_aggregate),
        }
    }

    pub fn status(&mut self) -> VadStatus {
        self.poll_loading();
        match self.state {
            LoadState::Loading { .. } => VadStatus::Loading,
            LoadState::Ready { .. } => VadStatus::Ready,
            LoadState::Disabled => VadStatus::Failed,
        }
    }

    pub fn is_ready(&mut self) -> bool {
        self.status() == VadStatus::Ready
    }

    fn start_loading(&mut self) {
        let Some(path) = self.model_path.clone() else {
            tracing::warn!("silero VAD disabled: {MODEL_FILE} not found");
            self.state = LoadState::Disabled;
            return;
        };
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("silero_vad_load".into())
            .spawn(move || {
                let started = Instant::now();
                let result = SileroVad::load(path).map(|vad| {
                    let backend = vad.backend_name();
                    (FrameStream::new(vad, MIN_SPEECH_THRESHOLD), backend)
                });
                tracing::info!(
                    "silero VAD load finished in {} ms ok={}",
                    started.elapsed().as_millis(),
                    result.is_ok()
                );
                let _ = tx.send(result);
            })
            .expect("spawn silero VAD loader");
        self.state = LoadState::Loading {
            started: Instant::now(),
            rx,
        };
    }

    fn poll_loading(&mut self) {
        let LoadState::Loading { started, rx } = &self.state else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok((stream, backend))) => {
                tracing::info!("silero VAD ready");
                self.state = LoadState::Ready { stream, backend };
            }
            Ok(Err(err)) => {
                tracing::warn!("silero VAD disabled: {err}");
                self.state = LoadState::Disabled;
            }
            Err(mpsc::TryRecvError::Empty) if started.elapsed() > LOAD_TIMEOUT => {
                tracing::warn!(
                    "silero VAD disabled: model load exceeded {} ms",
                    LOAD_TIMEOUT.as_millis()
                );
                self.state = LoadState::Disabled;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                tracing::warn!("silero VAD disabled: loader disconnected");
                self.state = LoadState::Disabled;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    /// Backend name once loading finished (`None` while loading/disabled).
    /// Diagnostic only — used by `examples/vad_parity_check.rs`.
    pub fn backend_name(&self) -> Option<&'static str> {
        match &self.state {
            LoadState::Ready { backend, .. } => Some(*backend),
            _ => None,
        }
    }

    #[cfg(test)]
    fn is_ready_for_test(&self) -> bool {
        matches!(self.state, LoadState::Ready { .. })
    }
}

impl Default for SpeechVad {
    fn default() -> Self {
        Self::new()
    }
}

impl SileroVad {
    fn load(path: PathBuf) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("mlmodelc") | Some("mlpackage")
        ) {
            // Tiny model, one 32 ms frame per call: CPU-only avoids per-call
            // ANE/GPU dispatch overhead that would dwarf the compute.
            let stage = crate::coreml::Stage::load_cpu_only(&path)?;
            let mut vad = Self::CoreMl {
                stage,
                state: vec![0.0; 2 * 128],
                context: [0.0; CONTEXT_SAMPLES],
            };
            vad.score_frame(&[0.0; FRAME_SAMPLES])?;
            return Ok(vad);
        }
        init_ort()?;
        let silero = rightkit_vad::Silero::load_with(&path, session_options())
            .map_err(|err| format!("load {}: {err}", path.display()))?;
        Ok(Self::Shared(silero))
    }

    fn reset(&mut self) {
        match self {
            Self::Shared(silero) => silero.reset(),
            #[cfg(target_os = "macos")]
            Self::CoreMl { state, context, .. } => {
                state.fill(0.0);
                context.fill(0.0);
            }
        }
    }

    fn backend_name(&self) -> &'static str {
        match self {
            Self::Shared(_) => "ort",
            #[cfg(target_os = "macos")]
            Self::CoreMl { .. } => "coreml",
        }
    }

    /// Score one 512-sample frame and advance state and context.
    fn score_frame(&mut self, frame: &[f32]) -> Result<f32, String> {
        let frame: &[f32; FRAME_SAMPLES] = frame
            .try_into()
            .map_err(|_| format!("frame of {} samples", frame.len()))?;
        match self {
            // rightkit-vad rejects non-finite or out-of-range scores as errors. Silero's
            // sigmoid output cannot leave 0..=1, so the only effect is that a corrupt model
            // disables VAD.
            Self::Shared(silero) => silero.score(frame).map_err(|err| format!("run: {err}")),
            #[cfg(target_os = "macos")]
            Self::CoreMl {
                stage,
                state,
                context,
            } => {
                let mut input = Vec::with_capacity(CONTEXT_SAMPLES + FRAME_SAMPLES);
                input.extend_from_slice(context);
                input.extend(frame.iter().map(|sample| sample.clamp(-1.0, 1.0)));
                let input = crate::coreml::ml_f32(&[1, CONTEXT_SAMPLES + FRAME_SAMPLES], &input)?;
                let state_in = crate::coreml::ml_f32(&[2, 1, 128], state)?;
                let outs = stage.predict_multi(
                    &[("input", &input), ("state_in", &state_in)],
                    &["output", "state_out"],
                )?;
                let score = *crate::coreml::read_f32(&outs[0])
                    .first()
                    .ok_or_else(|| "empty output tensor".to_string())?;
                let state_n = crate::coreml::read_f32(&outs[1]);
                if state_n.len() != 2 * 128 {
                    return Err(format!("unexpected state length {}", state_n.len()));
                }
                tracing::trace!("silero VAD score={:.4}", score);
                *state = state_n;
                context.copy_from_slice(&frame[FRAME_SAMPLES - CONTEXT_SAMPLES..]);
                Ok(score)
            }
        }
    }
}

impl FrameScorer for SileroVad {
    fn reset(&mut self) {
        SileroVad::reset(self);
    }

    fn score(&mut self, frame: &[f32; FRAME_SAMPLES]) -> rightkit_vad::Result<f32> {
        self.score_frame(frame).map_err(|err| {
            tracing::warn!("silero VAD inference failed: {err}");
            rightkit_vad::Error::Model(err)
        })
    }
}

/// Session options for the shared scorer: the app's 1/1 threads and kept warm-up state.
/// Windows asks for an independent pool with spinning off; elsewhere the app sets none of
/// the three, so the session keeps the ORT defaults. The `RIGHTKIT_VAD_ORT_*` probe variables
/// override each knob for experiments.
fn session_options() -> rightkit_vad::SileroOptions {
    let windows = cfg!(target_os = "windows");
    let threads = |specific: &str| {
        ort_probe_threads(specific)
            .or_else(|| ort_probe_threads("RIGHTKIT_VAD_ORT_THREADS"))
            .unwrap_or(1)
    };
    let spinning = |name: &str| match ort_probe_bool(name) {
        Some(true) => rightkit_vad::Spinning::On,
        Some(false) => rightkit_vad::Spinning::Off,
        None if windows => rightkit_vad::Spinning::Off,
        None => rightkit_vad::Spinning::RuntimeDefault,
    };
    rightkit_vad::SileroOptions {
        intra_threads: threads("RIGHTKIT_VAD_ORT_INTRA_THREADS"),
        inter_threads: threads("RIGHTKIT_VAD_ORT_INTER_THREADS"),
        warmup: rightkit_vad::Warmup::KeepState,
        own_thread_pool: Some(
            ort_probe_bool("RIGHTKIT_VAD_ORT_INDEPENDENT_POOL").unwrap_or(windows),
        ),
        intra_op_spinning: spinning("RIGHTKIT_VAD_ORT_INTRA_SPINNING"),
        inter_op_spinning: spinning("RIGHTKIT_VAD_ORT_INTER_SPINNING"),
        ..rightkit_vad::SileroOptions::default()
    }
}

fn ort_probe_threads(name: &str) -> Option<usize> {
    let value = rightkit_dictation_core::dev_var!(name).ok()?;
    match value.parse::<usize>() {
        Ok(threads) if threads > 0 => Some(threads),
        _ => {
            tracing::warn!("ignoring invalid {name}={value:?}; expected a positive integer");
            None
        }
    }
}

fn ort_probe_bool(name: &str) -> Option<bool> {
    let value = rightkit_dictation_core::dev_var!(name).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Some(true),
        "0" | "false" | "off" | "no" => Some(false),
        _ => {
            tracing::warn!("ignoring invalid {name}={value:?}; expected 0/1 or true/false");
            None
        }
    }
}

/// Initialize ORT before a VAD session for startup-order diagnostics.
/// Production startup continues to initialize lazily from the VAD loader.
#[doc(hidden)]
pub fn initialize_ort_for_probe() -> Result<(), String> {
    init_ort()
}

/// Bind the VAD runtime file, overwriting any inherited ORT_DYLIB_PATH (as the
/// previous set_var did). Refuses non-regular files and System32 copies.
fn bind_vad_runtime(path: &Path) -> Result<rightkit_onnx::RuntimeSelection, String> {
    rightkit_onnx::configure_runtime_with_options(
        [rightkit_onnx::RuntimeCandidate::new(
            path,
            rightkit_onnx::CandidateSource::Explicit,
        )],
        &rightkit_onnx::RuntimeBindingOptions {
            force_replace: true,
            ..Default::default()
        },
    )
    .map(|binding| binding.selection)
    .map_err(|err| format!("bind ORT {}: {err}", path.display()))
}

#[cfg(target_os = "windows")]
fn init_ort() -> Result<(), String> {
    if let Some(path) = ort_dylib_path() {
        bind_vad_runtime(&path)?;
    }
    rightkit_asr::onnx_tdt::init_windows_ort().map_err(|err| err.to_string())
}

#[cfg(not(target_os = "windows"))]
fn init_ort() -> Result<(), String> {
    if let Some(path) = ort_dylib_path() {
        tracing::info!("silero VAD loading ORT dylib={}", path.display());
        let selection = bind_vad_runtime(&path)?;
        // Deferred (options committed, C environment on first use), telemetry off,
        // and an environment configured elsewhere is tolerated, as before.
        // The environment-level CPU EP is not expressible in rightkit-onnx; the
        // VAD session still requests CPU explicitly (see with_execution_providers below).
        let report = rightkit_onnx::init_environment_with_options(
            &selection,
            &rightkit_onnx::EnvironmentInitOptions {
                defer_initialization: true,
                tolerate_already_initialized: true,
                telemetry: Some(false),
                ..Default::default()
            },
        )
        .map_err(|err| format!("load ORT {}: {err}", path.display()))?;
        tracing::info!(
            "silero VAD ORT dylib={} committed={}",
            path.display(),
            report.status != rightkit_onnx::EnvironmentStatus::AlreadyInitialized
        );
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    return Err(format!(
        "{ORT_DYLIB_FILE} not found; packaged macOS VAD requires the bundled ORT runtime"
    ));
    #[cfg(not(target_os = "macos"))]
    {
        // No bundled runtime resolved: let rightkit-onnx hand the platform file name to the
        // OS loader, with telemetry off, as the direct ort initialisation did before.
        tracing::warn!("silero VAD: no ORT dylib resolved; deferring to the default loader");
        rightkit_onnx::init_environment_default_loader(&rightkit_onnx::EnvironmentInitOptions {
            defer_initialization: true,
            tolerate_already_initialized: true,
            telemetry: Some(false),
            ..Default::default()
        })
        .map_err(|err| format!("load ORT through the default loader: {err}"))?;
        Ok(())
    }
}

fn default_model_path() -> Option<PathBuf> {
    if let Some(path) =
        rightkit_dictation_core::dev_var_os!("RIGHTKIT_VAD_MODEL").map(PathBuf::from)
    {
        // A .onnx file or a .mlmodelc directory — either backend.
        return path.exists().then_some(path);
    }
    model_path_candidates()
        .into_iter()
        .find(|candidate| candidate.is_file())
}

fn model_path_candidates() -> Vec<PathBuf> {
    resource_candidates("vad", MODEL_FILE)
}

fn ort_dylib_path() -> Option<PathBuf> {
    // The runtime the engine bound at startup (rightkit-onnx records it; a test-mode
    // build may name another with the dev override). An invalid path must not suppress
    // The app-bundled runtime: ort's dynamic loader can otherwise recurse while
    // formatting its load error.
    if let Some(path) = rightkit_dictation_core::dev_var_os!("RIGHTKIT_VAD_ORT_DYLIB")
        .map(PathBuf::from)
        .or_else(rightkit_onnx::configured_runtime)
    {
        if path.is_file() {
            return Some(path);
        }
        tracing::warn!(
            "ignoring missing ORT dylib override and checking bundled runtime: {}",
            path.display()
        );
    }
    resource_candidates("runtime", ORT_DYLIB_FILE)
        .into_iter()
        .find(|candidate| candidate.is_file())
}

fn resource_candidates(dir: &str, file: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            push_resource_candidates(&mut paths, exe_dir, dir, file);
            #[cfg(target_os = "macos")]
            if let Some(contents) = exe_dir.parent() {
                push_resource_candidates(&mut paths, &contents.join("Resources"), dir, file);
            }
        }
    }
    #[cfg(feature = "test-mode")]
    push_resource_candidates(
        &mut paths,
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources"),
        dir,
        file,
    );
    paths
}

fn push_resource_candidates(paths: &mut Vec<PathBuf>, base: &Path, dir: &str, file: &str) {
    paths.push(base.join(dir).join(file));
    paths.push(base.join("resources").join(dir).join(file));
}

#[cfg(test)]
#[path = "vad_tests.rs"]
mod tests;
