use crate::scorer::Backend;
use crate::{Error, FrameScorer, Result, FRAME_SAMPLES, SAMPLE_RATE};
use rightkit_onnx::ort;
use rightkit_onnx::ort::{
    session::{
        builder::{GraphOptimizationLevel, SessionBuilder},
        Session,
    },
    value::Value,
};
use std::path::Path;

const CONTEXT_SAMPLES: usize = 64;
const INPUT_SAMPLES: usize = FRAME_SAMPLES + CONTEXT_SAMPLES;
const STATE_LEN: usize = 2 * 128;

/// Where the Silero session runs. Asking for a provider this OS cannot host is
/// an error; it is never quietly run on the CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionProvider {
    /// ONNX Runtime CPU on every platform.
    #[default]
    Cpu,
    /// ONNX Runtime's CoreML execution provider (the `ort` CoreML EP) with
    /// `MLComputeUnits = CPUOnly`, the setting CutRight uses for its per-frame VAD
    /// stage. It runs the ONNX model through ONNX Runtime. It is not a native
    /// `.mlmodelc` or `.mlpackage` stage; HeardRight's native CoreML VAD stage stays
    /// in the application. Apple targets only.
    CoreMl,
    /// ONNX Runtime DirectML EP on [`SileroOptions::device_id`]. Windows only.
    DirectMl,
}

impl ExecutionProvider {
    /// Whether this build target can host the provider: CPU everywhere, CoreML on Apple
    /// targets, DirectML on Windows. On Android only `Cpu` is hosted; the others are refused with
    /// a "not available on this OS" error, never run on the CPU instead.
    pub fn supported_on_this_os(self) -> bool {
        self.supported_on(cfg!(target_vendor = "apple"), cfg!(target_os = "windows"))
    }

    /// The rule behind [`Self::supported_on_this_os`], with the target facts passed in so it is
    /// testable on any host.
    const fn supported_on(self, apple: bool, windows: bool) -> bool {
        match self {
            Self::Cpu => true,
            Self::CoreMl => apple,
            Self::DirectMl => windows,
        }
    }

    fn backend(self) -> Backend {
        match self {
            Self::Cpu => Backend::SileroCpu,
            Self::CoreMl => Backend::SileroCoreMl,
            Self::DirectMl => Backend::SileroDirectMl,
        }
    }
}

/// What the warm-up frame leaves behind. Loading runs one all-zero frame to
/// prove the model speaks the protocol; that frame changes the recurrent state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Warmup {
    /// Clear state and context after the warm-up frame, so the first real
    /// frame sees a fresh model. Default.
    #[default]
    ResetAfter,
    /// Keep the state the warm-up frame produced, as HeardRight does. The first
    /// real frames then score differently from `ResetAfter`; the difference
    /// decays as the recurrent state adapts to the audio.
    KeepState,
}

/// Session and fallback options for [`Silero`] and [`crate::SpeechScorer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SileroOptions {
    /// Intra-op threads. The session runs on its own thread pool, so this
    /// takes effect even when the process has a global ONNX Runtime pool.
    pub intra_threads: usize,
    /// Inter-op threads, with the same own-pool behaviour as `intra_threads`.
    pub inter_threads: usize,
    /// Where the session runs.
    pub execution_provider: ExecutionProvider,
    /// DirectML adapter index; `None` lets the runtime pick. Other providers ignore it.
    pub device_id: Option<i32>,
    /// Read by [`crate::SpeechScorer::from_path`] and [`crate::SpeechScorer::from_bytes`]
    /// only: when Silero cannot be built, score with the energy fallback and report
    /// why. The [`Silero`] constructors always return the error. Off by default.
    pub energy_fallback: bool,
    /// State left by the warm-up frame. See [`Warmup`].
    pub warmup: Warmup,
    /// ONNX Runtime intra-op spinning. See [`Spinning`]; the default is `Off`.
    pub intra_op_spinning: Spinning,
    /// Whether the session asks for its own thread pool. `None` keeps the default, which
    /// is on everywhere. `Some(false)` makes no thread-pool call at all, so the session
    /// keeps ONNX Runtime's default pooling; that is also the "leave it to the runtime"
    /// setting for this option.
    pub own_thread_pool: Option<bool>,
    /// ONNX Runtime inter-op spinning. See [`Spinning`]; the default is `Off`.
    pub inter_op_spinning: Spinning,
}

/// How one of the session's spinning settings (intra-op or inter-op) is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Spinning {
    /// Explicitly disable it. This is what every release before this option did.
    #[default]
    Off,
    /// Explicitly enable it.
    On,
    /// Do not touch the setting: the session keeps whatever ONNX Runtime defaults to.
    /// HeardRight runs this way on macOS and Linux.
    RuntimeDefault,
}

impl Default for SileroOptions {
    /// HeardRight and CutRight defaults: one thread each, CPU, graph optimisation
    /// disabled (a tiny per-frame model; dispatch dominates), no fallback.
    fn default() -> Self {
        Self {
            intra_threads: 1,
            inter_threads: 1,
            execution_provider: ExecutionProvider::Cpu,
            device_id: None,
            energy_fallback: false,
            warmup: Warmup::ResetAfter,
            intra_op_spinning: Spinning::Off,
            own_thread_pool: None,
            inter_op_spinning: Spinning::Off,
        }
    }
}

fn m<T: std::fmt::Display>(what: &'static str) -> impl FnOnce(T) -> Error {
    move |e| Error::Model(format!("{what}: {e}"))
}

/// Silero VAD v5 (16 kHz, op15 ONNX: inputs `input`/`state`/`sr`, outputs
/// `output`/`stateN`). Each frame is prefixed with the previous frame's last
/// 64 samples; recurrent state `[2,1,128]`.
pub struct Silero {
    session: Session,
    state: Vec<f32>,
    context: [f32; CONTEXT_SAMPLES],
    backend: Backend,
}

impl Silero {
    /// Load from an ONNX file with [`SileroOptions::default`] (CPU, no fallback).
    pub fn load(model: impl AsRef<Path>) -> Result<Self> {
        Self::load_with(model, SileroOptions::default())
    }

    /// Load from an ONNX file path.
    pub fn load_with(model: impl AsRef<Path>, options: SileroOptions) -> Result<Self> {
        let model = model.as_ref();
        ensure_supported(&options)?;
        if !model.is_file() {
            return Err(Error::Model(format!(
                "model not found: {}",
                model.display()
            )));
        }
        let session = configure(&options)?
            .commit_from_file(model)
            .map_err(m("load"))?;
        Self::warm(
            session,
            options.execution_provider.backend(),
            options.warmup,
        )
    }

    /// Load from ONNX bytes the app embeds (for example `include_bytes!`).
    /// ONNX Runtime must already be bound; see `rightkit-onnx`.
    pub fn from_bytes(model: &[u8], options: SileroOptions) -> Result<Self> {
        ensure_supported(&options)?;
        let session = configure(&options)?
            .commit_from_memory(model)
            .map_err(m("load"))?;
        Self::warm(
            session,
            options.execution_provider.backend(),
            options.warmup,
        )
    }

    /// The execution provider this scorer runs on.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    fn warm(session: Session, backend: Backend, warmup: Warmup) -> Result<Self> {
        let mut vad = Self {
            session,
            state: vec![0.0; STATE_LEN],
            context: [0.0; CONTEXT_SAMPLES],
            backend,
        };
        // Warm-up frame proves the model speaks the expected protocol.
        vad.score(&[0.0; FRAME_SAMPLES])?;
        if warmup == Warmup::ResetAfter {
            vad.reset();
        }
        Ok(vad)
    }
}

fn ensure_supported(options: &SileroOptions) -> Result<()> {
    if options.execution_provider.supported_on_this_os() {
        Ok(())
    } else {
        Err(Error::Model(format!(
            "execution provider {:?} is not available on this OS",
            options.execution_provider
        )))
    }
}

/// Whether the session asks ONNX Runtime for its own thread pool. Default on.
fn own_pool_enabled(options: &SileroOptions) -> bool {
    options.own_thread_pool.unwrap_or(true)
}

/// The value to set for a spinning option, or `None` to make no call and leave the
/// runtime default alone.
fn spinning_setting(setting: Spinning) -> Option<bool> {
    match setting {
        Spinning::Off => Some(false),
        Spinning::On => Some(true),
        Spinning::RuntimeDefault => None,
    }
}

/// Session builder shared by every load path. The CPU settings are the ones
/// CutRight's VAD session uses by default: an own pool, spinning off, graph optimisation
/// off. `own_thread_pool` and `intra_op_spinning` override the first two.
fn configure(options: &SileroOptions) -> Result<SessionBuilder> {
    let builder = Session::builder().map_err(m("session builder"))?;
    let builder = if own_pool_enabled(options) {
        builder
            .with_independent_thread_pool()
            .map_err(m("thread pool"))?
    } else {
        builder
    };
    let builder = builder
        .with_intra_threads(options.intra_threads.max(1))
        .map_err(m("intra threads"))?
        .with_inter_threads(options.inter_threads.max(1))
        .map_err(m("inter threads"))?;
    let builder = match spinning_setting(options.intra_op_spinning) {
        Some(enabled) => builder
            .with_intra_op_spinning(enabled)
            .map_err(m("intra spinning"))?,
        None => builder,
    };
    let builder = match spinning_setting(options.inter_op_spinning) {
        Some(enabled) => builder
            .with_inter_op_spinning(enabled)
            .map_err(m("inter spinning"))?,
        None => builder,
    };
    let builder = builder
        .with_optimization_level(GraphOptimizationLevel::Disable)
        .map_err(m("graph optimization"))?;
    match options.execution_provider {
        ExecutionProvider::Cpu => Ok(builder),
        ExecutionProvider::CoreMl => register_coreml(builder),
        ExecutionProvider::DirectMl => register_directml(builder, options.device_id),
    }
}

#[cfg(target_vendor = "apple")]
fn register_coreml(builder: SessionBuilder) -> Result<SessionBuilder> {
    use ort::ep::{coreml::ComputeUnits, CoreML};
    builder
        .with_execution_providers([CoreML::default()
            .with_compute_units(ComputeUnits::CPUOnly)
            .build()
            .error_on_failure()])
        .map_err(m("coreml"))
}

#[cfg(not(target_vendor = "apple"))]
fn register_coreml(_builder: SessionBuilder) -> Result<SessionBuilder> {
    Err(Error::Model(
        "execution provider CoreMl is not available on this OS".into(),
    ))
}

#[cfg(target_os = "windows")]
fn register_directml(builder: SessionBuilder, device_id: Option<i32>) -> Result<SessionBuilder> {
    let mut ep = ort::ep::DirectML::default();
    if let Some(id) = device_id {
        ep = ep.with_device_id(id);
    }
    builder
        .with_execution_providers([ep.build().error_on_failure()])
        .map_err(m("directml"))
}

#[cfg(not(target_os = "windows"))]
fn register_directml(_builder: SessionBuilder, _device_id: Option<i32>) -> Result<SessionBuilder> {
    Err(Error::Model(
        "execution provider DirectMl is not available on this OS".into(),
    ))
}

impl FrameScorer for Silero {
    fn reset(&mut self) {
        self.state.fill(0.0);
        self.context.fill(0.0);
    }

    fn score(&mut self, frame: &[f32; FRAME_SAMPLES]) -> Result<f32> {
        let mut input = Vec::with_capacity(INPUT_SAMPLES);
        input.extend_from_slice(&self.context);
        input.extend(frame.iter().map(|s| s.clamp(-1.0, 1.0)));
        let e = m;
        let outputs = self
            .session
            .run(ort::inputs!(
                "input" => Value::from_array(([1, INPUT_SAMPLES], input)).map_err(e("input"))?,
                "state" => Value::from_array(([2, 1, 128], self.state.clone())).map_err(e("state"))?,
                "sr" => Value::from_array(([] as [usize; 0], vec![SAMPLE_RATE as i64])).map_err(e("sr"))?
            ))
            .map_err(e("run"))?;
        let (_, out) = outputs["output"]
            .try_extract_tensor::<f32>()
            .map_err(e("output"))?;
        let score = *out
            .first()
            .ok_or_else(|| Error::Model("empty output tensor".into()))?;
        let (_, next) = outputs["stateN"]
            .try_extract_tensor::<f32>()
            .map_err(e("stateN"))?;
        if next.len() != STATE_LEN {
            return Err(Error::Model(format!(
                "unexpected state length {}",
                next.len()
            )));
        }
        if !score.is_finite() || !(0.0..=1.0).contains(&score) {
            return Err(Error::Model(format!("invalid probability {score}")));
        }
        self.state = next.to_vec();
        self.context
            .copy_from_slice(&frame[FRAME_SAMPLES - CONTEXT_SAMPLES..]);
        Ok(score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinning_defaults_to_off_and_can_be_left_to_the_runtime() {
        let options = SileroOptions::default();
        assert_eq!(options.intra_op_spinning, Spinning::Off);
        assert_eq!(options.inter_op_spinning, Spinning::Off);
        assert_eq!(spinning_setting(Spinning::Off), Some(false));
        assert_eq!(spinning_setting(Spinning::On), Some(true));
        assert_eq!(spinning_setting(Spinning::RuntimeDefault), None);
    }

    #[test]
    fn own_thread_pool_defaults_on_and_some_false_makes_no_call() {
        let options = SileroOptions::default();
        assert_eq!(options.own_thread_pool, None);
        assert!(own_pool_enabled(&options));
        let runtime_pool = SileroOptions {
            own_thread_pool: Some(false),
            ..SileroOptions::default()
        };
        assert!(!own_pool_enabled(&runtime_pool));
        let explicit = SileroOptions {
            own_thread_pool: Some(true),
            ..SileroOptions::default()
        };
        assert!(own_pool_enabled(&explicit));
    }
}

#[cfg(test)]
mod provider_support_tests {
    use super::ExecutionProvider::{CoreMl, Cpu, DirectMl};

    #[test]
    fn android_like_targets_host_only_the_cpu_provider() {
        // Neither Apple nor Windows: Android, Linux.
        assert!(Cpu.supported_on(false, false));
        assert!(!CoreMl.supported_on(false, false));
        assert!(!DirectMl.supported_on(false, false));
        assert!(CoreMl.supported_on(true, false) && !DirectMl.supported_on(true, false));
        assert!(DirectMl.supported_on(false, true) && !CoreMl.supported_on(false, true));
    }

    #[test]
    fn this_os_answer_follows_the_compile_target() {
        assert!(Cpu.supported_on_this_os());
        assert_eq!(CoreMl.supported_on_this_os(), cfg!(target_vendor = "apple"));
        assert_eq!(DirectMl.supported_on_this_os(), cfg!(target_os = "windows"));
    }
}
