use crate::{Error, FrameScorer, Result, FRAME_SAMPLES, SAMPLE_RATE};
use ort::{
    session::{builder::GraphOptimizationLevel, Session},
    value::Value,
};
use std::path::Path;

const CONTEXT_SAMPLES: usize = 64;
const INPUT_SAMPLES: usize = FRAME_SAMPLES + CONTEXT_SAMPLES;
const STATE_LEN: usize = 2 * 128;

#[derive(Debug, Clone, Copy)]
pub struct SileroOptions {
    pub intra_threads: usize,
    pub inter_threads: usize,
}

impl Default for SileroOptions {
    /// Field-proven HeardRight defaults: one thread each, CPU only, graph
    /// optimisation disabled (tiny per-frame model; dispatch dominates).
    fn default() -> Self {
        Self { intra_threads: 1, inter_threads: 1 }
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
}

impl Silero {
    pub fn load(model: impl AsRef<Path>) -> Result<Self> {
        Self::load_with(model, SileroOptions::default())
    }

    pub fn load_with(model: impl AsRef<Path>, options: SileroOptions) -> Result<Self> {
        let model = model.as_ref();
        if !model.is_file() {
            return Err(Error::Model(format!("model not found: {}", model.display())));
        }
        let session = Session::builder()
            .map_err(m("session builder"))?
            .with_intra_threads(options.intra_threads.max(1))
            .map_err(m("intra threads"))?
            .with_inter_threads(options.inter_threads.max(1))
            .map_err(m("inter threads"))?
            .with_optimization_level(GraphOptimizationLevel::Disable)
            .map_err(m("graph optimization"))?
            .commit_from_file(model)
            .map_err(m("load"))?;
        let mut vad = Self { session, state: vec![0.0; STATE_LEN], context: [0.0; CONTEXT_SAMPLES] };
        // Warm-up frame proves the model speaks the expected protocol.
        vad.score(&[0.0; FRAME_SAMPLES])?;
        vad.reset();
        Ok(vad)
    }
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
        let (_, out) = outputs["output"].try_extract_tensor::<f32>().map_err(e("output"))?;
        let score = *out.first().ok_or_else(|| Error::Model("empty output tensor".into()))?;
        let (_, next) = outputs["stateN"].try_extract_tensor::<f32>().map_err(e("stateN"))?;
        if next.len() != STATE_LEN {
            return Err(Error::Model(format!("unexpected state length {}", next.len())));
        }
        if !score.is_finite() || !(0.0..=1.0).contains(&score) {
            return Err(Error::Model(format!("invalid probability {score}")));
        }
        self.state = next.to_vec();
        self.context.copy_from_slice(&frame[FRAME_SAMPLES - CONTEXT_SAMPLES..]);
        Ok(score)
    }
}
