//! The three sessions and their tensor contract.

use std::path::Path;

use ndarray::{Array1, Array2, Array3};

use crate::onnx_tdt::session::SessionOptions;
use crate::onnx_tdt::{Error, Result};

/// `Value::from_array` for each concrete tensor this crate builds. A macro
/// rather than a generic function so the element-type trait never has to be
/// named — that bound moved between ORT release candidates.
macro_rules! tensor {
    ($array:expr) => {
        rightkit_onnx::ort::value::Value::from_array($array)
            .map_err(|e| Error::Runtime(format!("tensor: {e}")))?
    };
}

/// Predictor LSTM state shape, fixed by the exported decoder/joint graph.
const STATE_SHAPE: (usize, usize, usize) = (2, 1, 640);

/// `config.json` as shipped alongside the model.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct OnnxAsrConfig {
    /// Mel bins the encoder expects.
    #[serde(default = "default_features_size")]
    pub features_size: usize,
    /// Encoder time-subsampling factor, used to convert frames to seconds.
    #[serde(default = "default_subsampling")]
    pub subsampling_factor: usize,
    /// Mel frames per encoder invocation when the graph is static-shaped.
    #[serde(default)]
    pub static_window: Option<usize>,
    #[serde(default)]
    pub model_type: String,
}

fn default_features_size() -> usize {
    128
}
fn default_subsampling() -> usize {
    8
}

/// Loaded model: three sessions, the vocabulary, and the constants a decoder
/// needs. Holds no decode state — a caller may drive many hypotheses through
/// `joint_step` concurrently in its own search.
pub struct OnnxAsr {
    mel: rightkit_onnx::ort::session::Session,
    encoder: rightkit_onnx::ort::session::Session,
    decoder_joint: rightkit_onnx::ort::session::Session,
    /// Experimental reusable fixed-shape output storage for the fused graph.
    /// Initialized on first explicit call; never used by the shipping path.
    joint_preallocated_outputs: Option<PreallocatedJointOutputs>,
    /// Audit-only split copy. Loaded only when `RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT_DIR`
    /// is explicitly set; the installed fused model remains the default path.
    split_decoder: Option<SplitDecoder>,
    vocab: Vec<String>,
    config: OnnxAsrConfig,
    static_window: Option<usize>,
    /// Reusable input storage for `encode_channel_major` (ORT-16).
    encoder_signal: Vec<f32>,
    /// Reusable input storage for `joint_steps_batched_at` (ORT-16).
    joint_scratch: JointBatchScratch,
}

struct SplitDecoder {
    predictor: rightkit_onnx::ort::session::Session,
    joint: rightkit_onnx::ort::session::Session,
}

struct PreallocatedJointOutputs {
    options:
        rightkit_onnx::ort::session::RunOptions<rightkit_onnx::ort::session::HasSelectedOutputs>,
    shapes: [Vec<i64>; 3],
}

/// Predictor result that can be reused by blank descendants of one beam path.
/// Keep output projection and next recurrent state together so the cache
/// represents the complete predictor transition.
#[derive(Clone)]
pub struct PredictorOutput {
    pub projection: Array3<f32>,
    pub state_h: Array3<f32>,
    pub state_c: Array3<f32>,
}

/// One batched joint step: logits plus the next predictor state (h, c).
pub type BatchedJointStep = (Vec<f32>, Array3<f32>, Array3<f32>);

impl OnnxAsr {
    pub fn load(dir: &Path, options: SessionOptions) -> Result<Self> {
        let config: OnnxAsrConfig = {
            let path = dir.join("config.json");
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| Error::MissingArtifact(format!("{}: {e}", path.display())))?;
            serde_json::from_str(&raw).map_err(|e| Error::Malformed(format!("config.json: {e}")))?
        };

        let vocab = {
            let path = dir.join("vocab.txt");
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| Error::MissingArtifact(format!("{}: {e}", path.display())))?;
            parse_vocab(&raw)?
        };

        // The mel stage is its own graph so the log-mel matches the training
        // preprocessor exactly; there is no hand-written STFT to drift from it.
        //
        // Filenames vary by export: quantized artifacts carry an `.int8` infix
        // and older exports drop the `-model` suffix. Prefer the quantized name
        // where both exist, matching what shipped before this crate.
        let mel = options
            .cpu_only_for_stage("MEL")
            .build(&find(dir, &["mel_nemo.onnx", "mel.onnx"])?)?;
        // The encoder is the only stage worth accelerating. The decoder/joint
        // runs once per emitted symbol, where dispatch latency dominates the
        // maths, and it stays on CPU so the encoder keeps the VRAM.
        let encoder_path = find(
            dir,
            &[
                "encoder-model.int8.onnx",
                "encoder-model.onnx",
                "encoder.onnx",
            ],
        )?;
        // `static_window` is only a hint: a config sidecar can drift out of sync
        // with the model it describes, and a static encoder whose config forgot
        // the key would silently re-trigger the DirectML collapse. So commit
        // once on the hint, then ask the graph itself — the model cannot lie
        // about its own shape — and re-commit if the hint was wrong.
        let mut encoder = options.build_encoder(&encoder_path, config.static_window.is_some())?;
        let graph_window = encoder_static_window(&encoder);
        if graph_window.is_some() != config.static_window.is_some() {
            encoder = options.build_encoder(&encoder_path, graph_window.is_some())?;
        }
        // The graph wins, so decode pads to what the encoder actually declares.
        let static_window = graph_window.or(config.static_window);
        let decoder_joint = options.cpu_only_for_stage("DECODER").build(&find(
            dir,
            &[
                "decoder_joint-model.int8.onnx",
                "decoder_joint-model.onnx",
                "decoder_joint.onnx",
            ],
        )?)?;

        let split_decoder = if let Some(dir) =
            dev_var_os!("RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT_DIR")
        {
            let dir = Path::new(&dir);
            Some(SplitDecoder {
                predictor: options
                    .cpu_only_for_stage("PREDICTOR")
                    .build(&dir.join("predictor.onnx"))?,
                joint: options
                    .cpu_only_for_stage("JOINT")
                    .build(&dir.join("joint.onnx"))?,
            })
        } else {
            None
        };

        Ok(Self {
            mel,
            encoder,
            decoder_joint,
            joint_preallocated_outputs: None,
            split_decoder,
            vocab,
            config,
            static_window,
            encoder_signal: Vec::new(),
            joint_scratch: JointBatchScratch::default(),
        })
    }

    /// Frames the encoder requires per pass, when its `audio_signal` time
    /// dimension is fixed. `None` means a dynamic export that takes any length.
    pub fn static_window(&self) -> Option<usize> {
        self.static_window
    }

    pub fn vocab_pieces(&self) -> &[String] {
        &self.vocab
    }

    /// Blank is the last vocabulary row. Declared here rather than inferred at
    /// each call site, because deriving it from a length in two places is how
    /// the CoreML and ONNX paths came to disagree about which id is blank.
    pub fn blank_id(&self) -> usize {
        self.vocab.len() - 1
    }

    pub fn config(&self) -> &OnnxAsrConfig {
        &self.config
    }

    /// Seconds per encoder frame, from the model's own subsampling factor.
    pub fn frame_seconds(&self, sample_rate: usize, hop_length: usize) -> f32 {
        (self.config.subsampling_factor * hop_length) as f32 / sample_rate as f32
    }

    /// Zeroed predictor state, the starting point for a fresh hypothesis.
    pub fn initial_state(&self) -> (Array3<f32>, Array3<f32>) {
        (Array3::zeros(STATE_SHAPE), Array3::zeros(STATE_SHAPE))
    }

    /// Audio samples -> log-mel `[frames, features]`.
    pub fn mel(&mut self, audio: &[f32]) -> Result<Array2<f32>> {
        let input = Array2::from_shape_vec((1, audio.len()), audio.to_vec())
            .map_err(|e| Error::Shape(format!("mel input: {e}")))?;
        let outputs = self
            .mel
            .run(rightkit_onnx::ort::inputs!("audio" => tensor!(input)))
            .map_err(|e| Error::Runtime(format!("mel: {e}")))?;
        let (shape, data) = outputs["mel"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Runtime(format!("mel output: {e}")))?;
        if shape.len() != 3 {
            return Err(Error::Shape(format!(
                "mel rank {}, expected 3",
                shape.len()
            )));
        }
        // [1, features, frames] -> [frames, features]; the encoder takes time-major.
        let features = shape[1] as usize;
        let frames = shape[2] as usize;
        let mut out = Array2::<f32>::zeros((frames, features));
        for c in 0..features {
            for t in 0..frames {
                out[[t, c]] = data[c * frames + t];
            }
        }
        Ok(out)
    }

    /// Log-mel -> encoder output, returned flat as `[enc_dim * time_steps]`
    /// row-major with the frame count and the valid length.
    ///
    /// A static-window export declares a fixed time dimension, so the features
    /// are zero-padded up to it and the REAL frame count goes in `length`. The
    /// encoder needs both: the shape to satisfy the graph, the length to know
    /// where the audio stops. Passing short features to a static graph is either
    /// a hard shape rejection or an accepted tensor with undefined length
    /// semantics. Note macOS passes the full static window as its length and
    /// clamps decoding to the real frames instead;
    /// `RIGHTKIT_ONNX_ENCODER_LENGTH_EXPERIMENT=static` tests that here (ORT-01).
    pub fn encode(&mut self, features: &Array2<f32>) -> Result<EncoderOutput> {
        let frames = features.shape()[0];
        let feature_size = features.shape()[1];
        let width = self.encoder_width(feature_size, frames)?;
        let mut signal = Array3::<f32>::zeros((1, feature_size, width));
        for t in 0..frames {
            for c in 0..feature_size {
                signal[[0, c, t]] = features[[t, c]];
            }
        }
        self.run_encoder(tensor!(signal).into(), frames)
    }

    /// Audio samples -> log-mel kept in the graph's own `[features][frames]`
    /// layout. Same values as `mel`, without its transpose (ORT-16); feed it
    /// to `encode_channel_major`.
    pub fn mel_channel_major(&mut self, audio: &[f32]) -> Result<MelFeatures> {
        let input =
            rightkit_onnx::ort::value::TensorRef::from_array_view(([1, audio.len()], audio))
                .map_err(|e| Error::Runtime(format!("mel input: {e}")))?;
        let outputs = self
            .mel
            .run(rightkit_onnx::ort::inputs!("audio" => input))
            .map_err(|e| Error::Runtime(format!("mel: {e}")))?;
        let (shape, data) = outputs["mel"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Runtime(format!("mel output: {e}")))?;
        if shape.len() != 3 {
            return Err(Error::Shape(format!(
                "mel rank {}, expected 3",
                shape.len()
            )));
        }
        Ok(MelFeatures {
            data: data.to_vec(),
            features: shape[1] as usize,
            frames: shape[2] as usize,
        })
    }

    /// `encode` for `mel_channel_major` output. Builds the identical padded
    /// `[1, features, width]` signal in a buffer reused across calls (ORT-16).
    pub fn encode_channel_major(&mut self, mel: &MelFeatures) -> Result<EncoderOutput> {
        let width = self.encoder_width(mel.features, mel.frames)?;
        let mut signal = std::mem::take(&mut self.encoder_signal);
        fill_signal_channel_major(&mut signal, &mel.data, mel.features, mel.frames, width)?;
        let result = rightkit_onnx::ort::value::TensorRef::from_array_view((
            [1, mel.features, width],
            &signal[..],
        ))
        .map_err(|e| Error::Runtime(format!("encoder input: {e}")))
        .and_then(|input| self.run_encoder(input.into(), mel.frames));
        self.encoder_signal = signal;
        result
    }

    /// Padded time width the encoder takes for `frames` real frames.
    fn encoder_width(&self, feature_size: usize, frames: usize) -> Result<usize> {
        if feature_size != self.config.features_size {
            return Err(Error::Shape(format!(
                "mel produced {feature_size} bins, encoder expects {}",
                self.config.features_size
            )));
        }
        match self.static_window {
            Some(win) if frames > win => Err(Error::Shape(format!(
                "{frames} frames exceeds the encoder's static window of {win}"
            ))),
            Some(win) => Ok(win),
            None => Ok(frames),
        }
    }

    fn run_encoder(
        &mut self,
        signal: rightkit_onnx::ort::session::SessionInputValue<'_>,
        frames: usize,
    ) -> Result<EncoderOutput> {
        // ORT-01 experiment: `static` passes the full static window as
        // `length` (macOS semantics) and clamps decoding to the real frames
        // below. Default `actual` is the shipping behaviour.
        let length_input = match (encoder_length_mode()?, self.static_window) {
            (EncoderLengthMode::Static, Some(win)) => win,
            _ => frames,
        };
        let prior_pass_age = encoder_pass_age();
        let outputs = self
            .encoder
            .run(rightkit_onnx::ort::inputs!(
                "audio_signal" => signal,
                "length" => tensor!(Array1::from_vec(vec![length_input as i64]))
            ))
            .map_err(|e| Error::Runtime(format!("encoder: {e}")))?;
        note_encoder_pass_finished();

        let (shape, data) = outputs["outputs"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Runtime(format!("encoder output: {e}")))?;
        if shape.len() != 3 {
            return Err(Error::Shape(format!(
                "encoder rank {}, expected 3",
                shape.len()
            )));
        }
        let enc_dim = shape[1] as usize;
        let time_steps = shape[2] as usize;

        let encoded_len = outputs
            .get("encoded_lengths")
            .and_then(|v| v.try_extract_tensor::<i64>().ok())
            .and_then(|(_, d)| d.first().copied())
            .unwrap_or(time_steps as i64) as usize;
        // `encoded_lengths` is authoritative and is NOT reduced by one. The
        // vendored code subtracted a frame unconditionally to paper over
        // DirectML rounding the length up; that dropped any token emitted on
        // the final frame, and it did so on the CPU provider too, where the
        // rounding never happened. macOS never subtracted, so this is also
        // what makes the two platforms agree on the last frame.
        let mut used = encoded_len.min(time_steps);
        if length_input != frames {
            // Static-length arm: the graph reports the padded window's length,
            // so stop where the real audio stops. Conformer subsampling maps
            // L mel frames to ceil(L / factor) encoder frames, which is what
            // `encoded_lengths` returns for the real length.
            used = used.min(frames.div_ceil(self.config.subsampling_factor.max(1)));
        }

        Ok(EncoderOutput {
            data: data.to_vec(),
            enc_dim,
            time_steps,
            used,
            input_frames: frames,
            length_input,
            prior_pass_age,
        })
    }

    /// One decoder/joint invocation for one hypothesis.
    ///
    /// Returns the raw logits — `[vocab_size token logits .. n duration logits]`
    /// — and the updated predictor state. No token is chosen here.
    pub fn joint_step(
        &mut self,
        enc_frame: &[f32],
        last_token: i32,
        state_h: &Array3<f32>,
        state_c: &Array3<f32>,
    ) -> Result<JointStep> {
        let frame = Array3::from_shape_vec((1, enc_frame.len(), 1), enc_frame.to_vec())
            .map_err(|e| Error::Shape(format!("joint frame: {e}")))?;
        let targets = Array2::from_shape_vec((1, 1), vec![last_token])
            .map_err(|e| Error::Shape(format!("joint targets: {e}")))?;

        let outputs = self
            .decoder_joint
            .run(rightkit_onnx::ort::inputs!(
                "encoder_outputs" => tensor!(frame),
                "targets" => tensor!(targets),
                "target_length" => tensor!(Array1::from_vec(vec![1i32])),
                "input_states_1" => tensor!(state_h.clone()),
                "input_states_2" => tensor!(state_c.clone())
            ))
            .map_err(|e| Error::Runtime(format!("decoder/joint: {e}")))?;

        let (_, logits) = outputs["outputs"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Runtime(format!("joint logits: {e}")))?;

        Ok(JointStep {
            logits: logits.to_vec(),
            state_h: extract_state(&outputs, "output_states_1")?,
            state_c: extract_state(&outputs, "output_states_2")?,
        })
    }

    /// Run fused decoder/joint once for several independent beam hypotheses.
    /// Rows may use different encoder frames, tokens & predictor states.
    pub fn joint_steps_batched(
        &mut self,
        enc_frames: &[Vec<f32>],
        last_tokens: &[i32],
        states: &[(&Array3<f32>, &Array3<f32>)],
    ) -> Result<Vec<BatchedJointStep>> {
        let (frame, targets, lengths, states_h, states_c) =
            pack_batched_owned(enc_frames, last_tokens, states)?;
        let batch = enc_frames.len();
        let run_started = std::time::Instant::now();
        let outputs = self
            .decoder_joint
            .run(rightkit_onnx::ort::inputs!(
                "encoder_outputs" => tensor!(frame), "targets" => tensor!(targets),
                "target_length" => tensor!(lengths), "input_states_1" => tensor!(states_h),
                "input_states_2" => tensor!(states_c)
            ))
            .map_err(|e| Error::Runtime(format!("batched decoder/joint: {e}")))?;
        note_batched_run(run_started.elapsed());
        unpack_batched_joint(
            &outputs,
            batch,
            self.vocab.len(),
            self.config.model_type.contains("tdt"),
        )
    }

    /// `joint_steps_batched` with rows read straight from `encoded` at
    /// `frame_indices`, packed into input buffers reused across calls and
    /// passed to ORT as borrowed views (ORT-16). The input tensors hold the
    /// same values in the same layout, so the outputs are identical.
    pub fn joint_steps_batched_at(
        &mut self,
        encoded: &EncoderOutput,
        frame_indices: &[usize],
        last_tokens: &[i32],
        states: &[(&Array3<f32>, &Array3<f32>)],
    ) -> Result<Vec<BatchedJointStep>> {
        let batch = frame_indices.len();
        let dim = encoded.enc_dim;
        let mut scratch = std::mem::take(&mut self.joint_scratch);
        let result = scratch
            .pack(encoded, frame_indices, last_tokens, states)
            .and_then(|()| {
                let frame = rightkit_onnx::ort::value::TensorRef::from_array_view((
                    [batch, dim, 1],
                    &scratch.frames[..],
                ))
                .map_err(|e| Error::Runtime(format!("batched joint frame: {e}")))?;
                let targets = rightkit_onnx::ort::value::TensorRef::from_array_view((
                    [batch, 1],
                    &scratch.tokens[..],
                ))
                .map_err(|e| Error::Runtime(format!("batched joint targets: {e}")))?;
                let lengths = rightkit_onnx::ort::value::TensorRef::from_array_view((
                    [batch],
                    &scratch.lengths[..],
                ))
                .map_err(|e| Error::Runtime(format!("batched joint lengths: {e}")))?;
                let states_h = rightkit_onnx::ort::value::TensorRef::from_array_view((
                    [STATE_SHAPE.0, batch, STATE_SHAPE.2],
                    &scratch.h[..],
                ))
                .map_err(|e| Error::Runtime(format!("batched state h: {e}")))?;
                let states_c = rightkit_onnx::ort::value::TensorRef::from_array_view((
                    [STATE_SHAPE.0, batch, STATE_SHAPE.2],
                    &scratch.c[..],
                ))
                .map_err(|e| Error::Runtime(format!("batched state c: {e}")))?;
                let run_started = std::time::Instant::now();
                let outputs = self
                    .decoder_joint
                    .run(rightkit_onnx::ort::inputs!(
                        "encoder_outputs" => frame, "targets" => targets,
                        "target_length" => lengths, "input_states_1" => states_h,
                        "input_states_2" => states_c
                    ))
                    .map_err(|e| Error::Runtime(format!("batched decoder/joint: {e}")))?;
                note_batched_run(run_started.elapsed());
                unpack_batched_joint(
                    &outputs,
                    batch,
                    self.vocab.len(),
                    self.config.model_type.contains("tdt"),
                )
            });
        self.joint_scratch = scratch;
        result
    }

    /// Fused decoder/joint step using borrowed ORT tensor views for inputs.
    /// The shipped `joint_step` remains unchanged; this opt-in method isolates
    /// tensor-construction and input-copy overhead without splitting the graph.
    pub fn joint_step_borrowed(
        &mut self,
        enc_frame: &[f32],
        last_token: i32,
        state_h: &Array3<f32>,
        state_c: &Array3<f32>,
    ) -> Result<JointStep> {
        let frame = rightkit_onnx::ort::value::TensorRef::from_array_view((
            [1, enc_frame.len(), 1],
            enc_frame,
        ))
        .map_err(|e| Error::Runtime(format!("joint frame: {e}")))?;
        let targets = [last_token];
        let target_length = [1i32];
        let targets = rightkit_onnx::ort::value::TensorRef::from_array_view(([1, 1], &targets[..]))
            .map_err(|e| Error::Runtime(format!("joint targets: {e}")))?;
        let target_length =
            rightkit_onnx::ort::value::TensorRef::from_array_view(([1], &target_length[..]))
                .map_err(|e| Error::Runtime(format!("joint target length: {e}")))?;
        let input_states_1 = rightkit_onnx::ort::value::TensorRef::from_array_view(state_h.view())
            .map_err(|e| Error::Runtime(format!("joint state h: {e}")))?;
        let input_states_2 = rightkit_onnx::ort::value::TensorRef::from_array_view(state_c.view())
            .map_err(|e| Error::Runtime(format!("joint state c: {e}")))?;
        let outputs = self
            .decoder_joint
            .run(rightkit_onnx::ort::inputs!(
                "encoder_outputs" => frame,
                "targets" => targets,
                "target_length" => target_length,
                "input_states_1" => input_states_1,
                "input_states_2" => input_states_2
            ))
            .map_err(|e| Error::Runtime(format!("decoder/joint: {e}")))?;
        let (_, logits) = outputs["outputs"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Runtime(format!("joint logits: {e}")))?;
        Ok(JointStep {
            logits: logits.to_vec(),
            state_h: extract_state(&outputs, "output_states_1")?,
            state_c: extract_state(&outputs, "output_states_2")?,
        })
    }

    /// Fused decoder/joint step with borrowed inputs and reusable output
    /// tensors. If metadata leaves dimensions symbolic, the first synchronous
    /// run establishes concrete shapes; later runs must preserve them.
    pub fn joint_step_borrowed_preallocated(
        &mut self,
        enc_frame: &[f32],
        last_token: i32,
        state_h: &Array3<f32>,
        state_c: &Array3<f32>,
    ) -> Result<JointStep> {
        let frame = rightkit_onnx::ort::value::TensorRef::from_array_view((
            [1, enc_frame.len(), 1],
            enc_frame,
        ))
        .map_err(|e| Error::Runtime(format!("joint frame: {e}")))?;
        let targets = [last_token];
        let target_length = [1i32];
        let targets = rightkit_onnx::ort::value::TensorRef::from_array_view(([1, 1], &targets[..]))
            .map_err(|e| Error::Runtime(format!("joint targets: {e}")))?;
        let target_length =
            rightkit_onnx::ort::value::TensorRef::from_array_view(([1], &target_length[..]))
                .map_err(|e| Error::Runtime(format!("joint target length: {e}")))?;
        let input_states_1 = rightkit_onnx::ort::value::TensorRef::from_array_view(state_h.view())
            .map_err(|e| Error::Runtime(format!("joint state h: {e}")))?;
        let input_states_2 = rightkit_onnx::ort::value::TensorRef::from_array_view(state_c.view())
            .map_err(|e| Error::Runtime(format!("joint state c: {e}")))?;

        if self.joint_preallocated_outputs.is_none() {
            let metadata: Vec<_> = self
                .decoder_joint
                .outputs()
                .iter()
                .map(|output| (output.name().to_owned(), output.dtype().clone()))
                .collect();
            let outputs = self
                .decoder_joint
                .run(rightkit_onnx::ort::inputs!(
                    "encoder_outputs" => frame,
                    "targets" => targets,
                    "target_length" => target_length,
                    "input_states_1" => input_states_1,
                    "input_states_2" => input_states_2
                ))
                .map_err(|e| Error::Runtime(format!("decoder/joint preallocation warmup: {e}")))?;
            let shapes = validate_joint_output_shapes(
                &metadata,
                &outputs,
                self.vocab.len(),
                self.config.model_type.contains("tdt"),
            )?;
            self.joint_preallocated_outputs = Some(preallocate_joint_outputs(shapes.clone())?);
            return joint_step_from_outputs(&outputs);
        }

        let preallocated = self
            .joint_preallocated_outputs
            .as_ref()
            .ok_or_else(|| Error::Runtime("joint output buffers were not initialized".into()))?;
        let outputs = self
            .decoder_joint
            .run_with_options(
                rightkit_onnx::ort::inputs!(
                    "encoder_outputs" => frame,
                    "targets" => targets,
                    "target_length" => target_length,
                    "input_states_1" => input_states_1,
                    "input_states_2" => input_states_2
                ),
                &preallocated.options,
            )
            .map_err(|e| Error::Runtime(format!("decoder/joint preallocated: {e}")))?;
        // Metadata & concrete shapes were checked at initialization. Compare
        // borrowed dimensions here without allocating validation buffers in
        // the per-hypothesis hot path.
        for (name, expected) in ["outputs", "output_states_1", "output_states_2"]
            .into_iter()
            .zip(&preallocated.shapes)
        {
            let (actual, _) = outputs[name]
                .try_extract_tensor::<f32>()
                .map_err(|e| Error::Runtime(format!("joint output `{name}`: {e}")))?;
            if actual.iter().copied().ne(expected.iter().copied()) {
                return Err(Error::Shape(format!(
                    "joint output `{name}` changed after preallocation: expected {expected:?}, got {actual:?}"
                )));
            }
        }
        joint_step_from_outputs(&outputs)
    }

    /// Run only the decoder predictor branch from an audit split graph.
    pub fn predictor_step(
        &mut self,
        last_token: i32,
        state_h: &Array3<f32>,
        state_c: &Array3<f32>,
    ) -> Result<PredictorOutput> {
        let split = self.split_decoder.as_mut().ok_or_else(|| {
            Error::MissingArtifact("split predictor experiment models were not loaded".into())
        })?;
        let targets = Array2::from_shape_vec((1, 1), vec![last_token])
            .map_err(|e| Error::Shape(format!("predictor targets: {e}")))?;
        let outputs = split
            .predictor
            .run(rightkit_onnx::ort::inputs!(
                "targets" => tensor!(targets),
                "target_length" => tensor!(Array1::from_vec(vec![1i32])),
                "input_states_1" => tensor!(state_h.clone()),
                "input_states_2" => tensor!(state_c.clone())
            ))
            .map_err(|e| Error::Runtime(format!("split predictor: {e}")))?;
        let projection = extract_state(&outputs, "/joint/pred/Add_output_0")?;
        Ok(PredictorOutput {
            projection,
            state_h: extract_state(&outputs, "output_states_1")?,
            state_c: extract_state(&outputs, "output_states_2")?,
        })
    }

    /// Split predictor using borrowed ORT tensor views for its state inputs.
    /// This isolates the cost of cloning the two recurrent-state arrays into
    /// owned ORT tensors in `predictor_step`; outputs remain owned because the
    /// beam cache outlives this synchronous session call.
    pub fn predictor_step_borrowed(
        &mut self,
        last_token: i32,
        state_h: &Array3<f32>,
        state_c: &Array3<f32>,
    ) -> Result<PredictorOutput> {
        let split = self.split_decoder.as_mut().ok_or_else(|| {
            Error::MissingArtifact("split predictor experiment models were not loaded".into())
        })?;
        let targets = [last_token];
        let target_length = [1i32];
        let targets = rightkit_onnx::ort::value::TensorRef::from_array_view(([1, 1], &targets[..]))
            .map_err(|e| Error::Runtime(format!("predictor targets: {e}")))?;
        let target_length =
            rightkit_onnx::ort::value::TensorRef::from_array_view(([1], &target_length[..]))
                .map_err(|e| Error::Runtime(format!("predictor target length: {e}")))?;
        let input_states_1 = rightkit_onnx::ort::value::TensorRef::from_array_view(state_h.view())
            .map_err(|e| Error::Runtime(format!("predictor state h: {e}")))?;
        let input_states_2 = rightkit_onnx::ort::value::TensorRef::from_array_view(state_c.view())
            .map_err(|e| Error::Runtime(format!("predictor state c: {e}")))?;
        let outputs = split
            .predictor
            .run(rightkit_onnx::ort::inputs!(
                "targets" => targets,
                "target_length" => target_length,
                "input_states_1" => input_states_1,
                "input_states_2" => input_states_2
            ))
            .map_err(|e| Error::Runtime(format!("split predictor: {e}")))?;
        Ok(PredictorOutput {
            projection: extract_state(&outputs, "/joint/pred/Add_output_0")?,
            state_h: extract_state(&outputs, "output_states_1")?,
            state_c: extract_state(&outputs, "output_states_2")?,
        })
    }

    /// Run the joint branch using a cached predictor projection.
    pub fn joint_from_predictor(
        &mut self,
        enc_frame: &[f32],
        projection: &Array3<f32>,
    ) -> Result<Vec<f32>> {
        let split = self.split_decoder.as_mut().ok_or_else(|| {
            Error::MissingArtifact("split predictor experiment models were not loaded".into())
        })?;
        let frame = Array3::from_shape_vec((1, enc_frame.len(), 1), enc_frame.to_vec())
            .map_err(|e| Error::Shape(format!("joint frame: {e}")))?;
        let outputs = split
            .joint
            .run(rightkit_onnx::ort::inputs!(
                "encoder_outputs" => tensor!(frame),
                "/joint/pred/Add_output_0" => tensor!(projection.clone())
            ))
            .map_err(|e| Error::Runtime(format!("split joint: {e}")))?;
        let (_, logits) = outputs["outputs"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Runtime(format!("split joint logits: {e}")))?;
        Ok(logits.to_vec())
    }

    /// Split joint with borrowed frame & projection inputs. This avoids
    /// rebuilding the frame tensor and cloning the cached projection for each
    /// encoder frame that shares one predictor result.
    pub fn joint_from_predictor_borrowed(
        &mut self,
        enc_frame: &[f32],
        projection: &Array3<f32>,
    ) -> Result<Vec<f32>> {
        let split = self.split_decoder.as_mut().ok_or_else(|| {
            Error::MissingArtifact("split predictor experiment models were not loaded".into())
        })?;
        let frame = rightkit_onnx::ort::value::TensorRef::from_array_view((
            [1, enc_frame.len(), 1],
            enc_frame,
        ))
        .map_err(|e| Error::Runtime(format!("joint frame: {e}")))?;
        let projection = rightkit_onnx::ort::value::TensorRef::from_array_view(projection.view())
            .map_err(|e| Error::Runtime(format!("joint predictor projection: {e}")))?;
        let outputs = split
            .joint
            .run(rightkit_onnx::ort::inputs!(
                "encoder_outputs" => frame,
                "/joint/pred/Add_output_0" => projection
            ))
            .map_err(|e| Error::Runtime(format!("split joint: {e}")))?;
        let (_, logits) = outputs["outputs"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Runtime(format!("split joint logits: {e}")))?;
        Ok(logits.to_vec())
    }

    pub fn has_split_predictor_experiment(&self) -> bool {
        self.split_decoder.is_some()
    }
}

pub struct EncoderOutput {
    /// Row-major `[enc_dim][time_steps]`.
    pub data: Vec<f32>,
    pub enc_dim: usize,
    pub time_steps: usize,
    /// Frames covering real audio; decode should stop here.
    pub used: usize,
    /// Real mel frames fed to this pass (before padding to the static window).
    pub input_frames: usize,
    /// The `length` input actually passed: `input_frames`, or the static
    /// window under `RIGHTKIT_ONNX_ENCODER_LENGTH_EXPERIMENT=static`.
    pub length_input: usize,
    /// Time since the previous encoder pass in this process finished, at the
    /// start of this one. `None` for the first pass. A GPU-recency signal for
    /// the Windows final-tail slow mode (ORT-01 / ENG-01).
    pub prior_pass_age: Option<std::time::Duration>,
}

impl EncoderOutput {
    /// Gather one encoder frame. The buffer is `[enc_dim][time_steps]`, so a
    /// frame is a stride, not a slice.
    pub fn frame(&self, t: usize) -> Vec<f32> {
        (0..self.enc_dim)
            .map(|d| self.data[d * self.time_steps + t])
            .collect()
    }

    /// `frame(t)` appended to `out` without allocating.
    pub fn extend_frame(&self, t: usize, out: &mut Vec<f32>) {
        out.extend((0..self.enc_dim).map(|d| self.data[d * self.time_steps + t]));
    }
}

/// Log-mel in the mel graph's own `[features][frames]` layout.
pub struct MelFeatures {
    pub data: Vec<f32>,
    pub features: usize,
    pub frames: usize,
}

/// Copy channel-major mel into a zero-padded `[features][width]` signal,
/// reusing `signal`'s allocation. Every element is written, so stale contents
/// from a longer previous pass cannot leak into this one.
fn fill_signal_channel_major(
    signal: &mut Vec<f32>,
    mel: &[f32],
    features: usize,
    frames: usize,
    width: usize,
) -> Result<()> {
    if frames > width || mel.len() != features * frames {
        return Err(Error::Shape(format!(
            "mel {features}x{frames} ({} values) does not fit encoder width {width}",
            mel.len()
        )));
    }
    signal.resize(features * width, 0.0);
    if frames == 0 {
        signal.fill(0.0);
        return Ok(());
    }
    for (row, src) in signal.chunks_exact_mut(width).zip(mel.chunks_exact(frames)) {
        row[..frames].copy_from_slice(src);
        row[frames..].fill(0.0);
    }
    Ok(())
}

/// ORT-01: `RIGHTKIT_ONNX_ENCODER_LENGTH_EXPERIMENT=actual|static`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncoderLengthMode {
    Actual,
    Static,
}

fn encoder_length_mode() -> Result<EncoderLengthMode> {
    match dev_var!("RIGHTKIT_ONNX_ENCODER_LENGTH_EXPERIMENT")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        None | Some("") | Some("actual") => Ok(EncoderLengthMode::Actual),
        Some("static") => Ok(EncoderLengthMode::Static),
        Some(other) => Err(Error::Runtime(format!(
            "invalid RIGHTKIT_ONNX_ENCODER_LENGTH_EXPERIMENT: {other}"
        ))),
    }
}

/// When the most recent encoder pass in this process finished.
static LAST_ENCODER_PASS: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

fn note_encoder_pass_finished() {
    *LAST_ENCODER_PASS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(std::time::Instant::now());
}

/// Time since the most recent encoder pass in this process finished (any
/// window kind: opening, keep-warm, background or final). `None` before the
/// first pass. On the DirectML route this is the age of the last GPU pass;
/// the worker can stamp it at final start (ENG-01 `prior_main_pass_age_ms`).
pub fn encoder_pass_age() -> Option<std::time::Duration> {
    LAST_ENCODER_PASS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .map(|finished| finished.elapsed())
}

thread_local! {
    /// `(calls, time inside `Session::run`)` for `joint_steps_batched`, so the
    /// predictor bench can split the ORT call from tensor construction and
    /// output extraction around it. Read and reset with `take_batched_run_stats`.
    static BATCHED_RUN_STATS: std::cell::RefCell<(usize, std::time::Duration)> =
        const { std::cell::RefCell::new((0, std::time::Duration::ZERO)) };
}

/// Calls and `Session::run` time accumulated by `joint_steps_batched` on this
/// thread since the last call; resets the counter.
pub fn take_batched_run_stats() -> (usize, std::time::Duration) {
    BATCHED_RUN_STATS.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
}

fn note_batched_run(elapsed: std::time::Duration) {
    BATCHED_RUN_STATS.with(|cell| {
        let mut stats = cell.borrow_mut();
        stats.0 += 1;
        stats.1 += elapsed;
    });
}

type PackedBatch = (
    Array3<f32>,
    Array2<i32>,
    Array1<i32>,
    Array3<f32>,
    Array3<f32>,
);

fn check_batch_states(
    batch: usize,
    tokens: usize,
    states: &[(&Array3<f32>, &Array3<f32>)],
) -> Result<()> {
    if batch == 0 || tokens != batch || states.len() != batch {
        return Err(Error::Shape(
            "batched joint inputs must have same nonzero batch".into(),
        ));
    }
    let expected = [STATE_SHAPE.0, STATE_SHAPE.1, STATE_SHAPE.2];
    if states
        .iter()
        .any(|(h, c)| h.shape() != expected || c.shape() != expected)
    {
        return Err(Error::Shape("batched joint state must be [2,1,640]".into()));
    }
    Ok(())
}

/// Owned input tensors for `joint_steps_batched`: frames `[B, D, 1]`,
/// targets `[B, 1]`, lengths `[B]`, states `[2, B, 640]` (layer-major, then row).
fn pack_batched_owned(
    enc_frames: &[Vec<f32>],
    last_tokens: &[i32],
    states: &[(&Array3<f32>, &Array3<f32>)],
) -> Result<PackedBatch> {
    let batch = enc_frames.len();
    check_batch_states(batch, last_tokens.len(), states)?;
    let dim = enc_frames[0].len();
    if dim == 0 || enc_frames.iter().any(|frame| frame.len() != dim) {
        return Err(Error::Shape(
            "batched joint frames have inconsistent dimensions".into(),
        ));
    }
    let mut frame_data = Vec::with_capacity(batch * dim);
    for frame in enc_frames {
        frame_data.extend_from_slice(frame);
    }
    let frame = Array3::from_shape_vec((batch, dim, 1), frame_data)
        .map_err(|e| Error::Shape(format!("batched joint frame: {e}")))?;
    let targets = Array2::from_shape_vec((batch, 1), last_tokens.to_vec())
        .map_err(|e| Error::Shape(format!("batched joint targets: {e}")))?;
    let lengths = Array1::from_elem(batch, 1i32);
    let mut h = Vec::with_capacity(2 * batch * 640);
    let mut c = Vec::with_capacity(2 * batch * 640);
    for layer in 0..2 {
        for (state_h, _) in states {
            h.extend(state_h.index_axis(ndarray::Axis(0), layer).iter().copied());
        }
    }
    for layer in 0..2 {
        for (_, state_c) in states {
            c.extend(state_c.index_axis(ndarray::Axis(0), layer).iter().copied());
        }
    }
    let states_h = Array3::from_shape_vec((2, batch, 640), h)
        .map_err(|e| Error::Shape(format!("batched state h: {e}")))?;
    let states_c = Array3::from_shape_vec((2, batch, 640), c)
        .map_err(|e| Error::Shape(format!("batched state c: {e}")))?;
    Ok((frame, targets, lengths, states_h, states_c))
}

/// Reusable batched-joint input buffers (ORT-16). `pack` writes exactly the
/// values and layout `pack_batched_owned` builds.
#[derive(Default)]
struct JointBatchScratch {
    frames: Vec<f32>,
    tokens: Vec<i32>,
    lengths: Vec<i32>,
    h: Vec<f32>,
    c: Vec<f32>,
}

impl JointBatchScratch {
    fn pack(
        &mut self,
        encoded: &EncoderOutput,
        frame_indices: &[usize],
        last_tokens: &[i32],
        states: &[(&Array3<f32>, &Array3<f32>)],
    ) -> Result<()> {
        let batch = frame_indices.len();
        check_batch_states(batch, last_tokens.len(), states)?;
        if encoded.enc_dim == 0 || frame_indices.iter().any(|&t| t >= encoded.time_steps) {
            return Err(Error::Shape(
                "batched joint frame index outside the encoder output".into(),
            ));
        }
        self.frames.clear();
        for &t in frame_indices {
            encoded.extend_frame(t, &mut self.frames);
        }
        self.tokens.clear();
        self.tokens.extend_from_slice(last_tokens);
        self.lengths.clear();
        self.lengths.resize(batch, 1);
        self.h.clear();
        self.c.clear();
        for layer in 0..STATE_SHAPE.0 {
            for (state_h, _) in states {
                self.h
                    .extend(state_h.index_axis(ndarray::Axis(0), layer).iter().copied());
            }
        }
        for layer in 0..STATE_SHAPE.0 {
            for (_, state_c) in states {
                self.c
                    .extend(state_c.index_axis(ndarray::Axis(0), layer).iter().copied());
            }
        }
        Ok(())
    }
}

/// Split batched joint outputs into per-row logits and `[2, 1, 640]` states.
fn unpack_batched_joint(
    outputs: &rightkit_onnx::ort::session::SessionOutputs,
    batch: usize,
    vocab_len: usize,
    is_tdt: bool,
) -> Result<Vec<BatchedJointStep>> {
    let (logit_shape, logits) = outputs["outputs"]
        .try_extract_tensor::<f32>()
        .map_err(|e| Error::Runtime(format!("batched joint logits: {e}")))?;
    if logit_shape.len() < 2 {
        return Err(Error::Shape(format!(
            "batched joint logits have incompatible shape {logit_shape:?}"
        )));
    }
    let trailing = &logit_shape[1..];
    if logit_shape[0] != batch as i64
        || trailing
            .iter()
            .take(trailing.len().saturating_sub(1))
            .any(|&dim| dim != 1)
        || trailing.last().copied().unwrap_or(0) < vocab_len as i64
        || logits.len() != batch * trailing.last().copied().unwrap_or(0) as usize
        || (is_tdt && trailing.last().copied().unwrap_or(0) <= vocab_len as i64)
    {
        return Err(Error::Shape(format!(
            "batched joint logits have incompatible shape {logit_shape:?}"
        )));
    }
    let logits_per_row = trailing.last().copied().unwrap_or(0) as usize;
    let state_rows = |name: &str| -> Result<Vec<Array3<f32>>> {
        let (shape, data) = outputs[name]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Runtime(format!("batched joint output {name}: {e}")))?;
        if shape.len() != 3 || shape[0] != 2 || shape[1] != batch as i64 || shape[2] != 640 {
            return Err(Error::Shape(format!(
                "batched state {name} has shape {shape:?}"
            )));
        }
        (0..batch)
            .map(|row| {
                let mut values = Vec::with_capacity(1280);
                for layer in 0..2 {
                    let start = (layer * batch + row) * 640;
                    values.extend_from_slice(&data[start..start + 640]);
                }
                Array3::from_shape_vec(STATE_SHAPE, values)
                    .map_err(|e| Error::Shape(format!("batched state slice: {e}")))
            })
            .collect()
    };
    let hs = state_rows("output_states_1")?;
    let cs = state_rows("output_states_2")?;
    Ok(hs
        .into_iter()
        .zip(cs)
        .enumerate()
        .map(|(row, (h, c))| {
            (
                logits[row * logits_per_row..(row + 1) * logits_per_row].to_vec(),
                h,
                c,
            )
        })
        .collect())
}

pub struct JointStep {
    pub logits: Vec<f32>,
    pub state_h: Array3<f32>,
    pub state_c: Array3<f32>,
}

fn preallocate_joint_outputs(shapes: [Vec<i64>; 3]) -> Result<PreallocatedJointOutputs> {
    let [logits_shape, state_h_shape, state_c_shape] = shapes;
    validate_logits_shape(&logits_shape, 0, false)?;
    let expected_state = [
        STATE_SHAPE.0 as i64,
        STATE_SHAPE.1 as i64,
        STATE_SHAPE.2 as i64,
    ];
    if state_h_shape.as_slice() != expected_state.as_slice()
        || state_c_shape.as_slice() != expected_state.as_slice()
    {
        return Err(Error::Shape(format!(
            "joint preallocation requires recurrent outputs {expected_state:?}, got h={state_h_shape:?}, c={state_c_shape:?}"
        )));
    }

    let logits = zeroed_f32_tensor("outputs", &logits_shape)?;
    let state_h = zeroed_f32_tensor("output_states_1", &state_h_shape)?;
    let state_c = zeroed_f32_tensor("output_states_2", &state_c_shape)?;
    let selector = rightkit_onnx::ort::session::OutputSelector::no_default()
        .with("outputs")
        .preallocate("outputs", logits)
        .with("output_states_1")
        .preallocate("output_states_1", state_h)
        .with("output_states_2")
        .preallocate("output_states_2", state_c);
    let options = rightkit_onnx::ort::session::RunOptions::new()
        .map(|options| options.with_outputs(selector))
        .map_err(|e| Error::Runtime(format!("joint output run options: {e}")))?;
    Ok(PreallocatedJointOutputs {
        options,
        shapes: [logits_shape, state_h_shape, state_c_shape],
    })
}

fn validate_joint_output_shapes(
    metadata: &[(String, rightkit_onnx::ort::value::ValueType)],
    outputs: &rightkit_onnx::ort::session::SessionOutputs,
    vocab_size: usize,
    has_duration: bool,
) -> Result<[Vec<i64>; 3]> {
    let logits = actual_f32_output_shape(metadata, outputs, "outputs")?;
    let state_h = actual_f32_output_shape(metadata, outputs, "output_states_1")?;
    let state_c = actual_f32_output_shape(metadata, outputs, "output_states_2")?;
    validate_logits_shape(&logits, vocab_size, has_duration)?;
    let expected_state = [
        STATE_SHAPE.0 as i64,
        STATE_SHAPE.1 as i64,
        STATE_SHAPE.2 as i64,
    ];
    if state_h.as_slice() != expected_state.as_slice()
        || state_c.as_slice() != expected_state.as_slice()
    {
        return Err(Error::Shape(format!(
            "joint preallocation requires recurrent outputs {expected_state:?}, got h={state_h:?}, c={state_c:?}"
        )));
    }
    Ok([logits, state_h, state_c])
}

fn validate_logits_shape(shape: &[i64], vocab_size: usize, has_duration: bool) -> Result<()> {
    let mut non_singleton = shape.iter().filter(|dim| **dim != 1);
    let logits_axis = non_singleton.next().copied().unwrap_or_default();
    let total = usize::try_from(logits_axis).ok();
    let valid = !shape.is_empty()
        && shape.iter().all(|dim| *dim > 0)
        && logits_axis > 1
        && non_singleton.next().is_none()
        && total.is_some_and(|n| n >= vocab_size && (!has_duration || n > vocab_size));
    if !valid {
        return Err(Error::Shape(format!(
            "joint preallocation requires one positive logits axis (at least vocab size {vocab_size}), other axes 1; got {shape:?}"
        )));
    }
    Ok(())
}

fn actual_f32_output_shape(
    metadata: &[(String, rightkit_onnx::ort::value::ValueType)],
    outputs: &rightkit_onnx::ort::session::SessionOutputs,
    name: &str,
) -> Result<Vec<i64>> {
    let outlet = metadata
        .iter()
        .find(|(output_name, _)| output_name == name)
        .ok_or_else(|| Error::MissingArtifact(format!("joint output metadata `{name}`")))?;
    if outlet.1.tensor_type() != Some(rightkit_onnx::ort::value::TensorElementType::Float32) {
        return Err(Error::Shape(format!(
            "joint output `{name}` must be f32, got {:?}",
            outlet.1
        )));
    }
    let shape = outlet
        .1
        .tensor_shape()
        .ok_or_else(|| Error::Shape(format!("joint output `{name}` is not a tensor")))?;
    let metadata_dims: Vec<i64> = shape.iter().copied().collect();
    if metadata_dims.is_empty() {
        return Err(Error::Shape(format!(
            "joint output `{name}` has empty metadata shape"
        )));
    }
    let value = outputs
        .get(name)
        .ok_or_else(|| Error::Runtime(format!("joint output `{name}` is missing")))?;
    let (actual_shape, _) = value
        .try_extract_tensor::<f32>()
        .map_err(|e| Error::Runtime(format!("joint output `{name}`: {e}")))?;
    let dims: Vec<i64> = actual_shape.iter().copied().collect();
    if dims.len() != metadata_dims.len() || dims.iter().any(|dim| *dim <= 0) {
        return Err(Error::Shape(format!(
            "joint output `{name}` has unsupported concrete shape {dims:?} for metadata {metadata_dims:?}"
        )));
    }
    if metadata_dims
        .iter()
        .zip(&dims)
        .any(|(declared, actual)| *declared != -1 && declared != actual)
    {
        return Err(Error::Shape(format!(
            "joint output `{name}` runtime shape {dims:?} disagrees with metadata {metadata_dims:?}"
        )));
    }
    Ok(dims)
}

fn zeroed_f32_tensor(name: &str, shape: &[i64]) -> Result<rightkit_onnx::ort::value::Tensor<f32>> {
    let mut elements = 1usize;
    for &dim in shape {
        let dim = usize::try_from(dim)
            .map_err(|e| Error::Shape(format!("joint output `{name}` dimension: {e}")))?;
        elements = elements
            .checked_mul(dim)
            .ok_or_else(|| Error::Shape(format!("joint output `{name}` element count overflow")))?;
    }
    rightkit_onnx::ort::value::Tensor::from_array((
        shape,
        vec![0.0f32; elements].into_boxed_slice(),
    ))
    .map_err(|e| Error::Runtime(format!("preallocate joint output `{name}`: {e}")))
}

fn joint_step_from_outputs(
    outputs: &rightkit_onnx::ort::session::SessionOutputs,
) -> Result<JointStep> {
    let (_, logits) = outputs["outputs"]
        .try_extract_tensor::<f32>()
        .map_err(|e| Error::Runtime(format!("joint logits: {e}")))?;
    Ok(JointStep {
        logits: logits.to_vec(),
        state_h: extract_state(outputs, "output_states_1")?,
        state_c: extract_state(outputs, "output_states_2")?,
    })
}

/// The predictor state MUST come back on every step. Falling back to the
/// previous state on a shape change would keep decoding with a stale predictor
/// and produce a plausible-looking wrong transcript, so this errors instead.
fn extract_state(
    outputs: &rightkit_onnx::ort::session::SessionOutputs,
    name: &str,
) -> Result<Array3<f32>> {
    let value = outputs
        .get(name)
        .ok_or_else(|| Error::Runtime(format!("joint output `{name}` is missing")))?;
    let (shape, data) = value
        .try_extract_tensor::<f32>()
        .map_err(|e| Error::Runtime(format!("joint output `{name}`: {e}")))?;
    if shape.len() != 3 {
        return Err(Error::Shape(format!(
            "joint output `{name}`: expected rank 3, got rank {}",
            shape.len()
        )));
    }
    Array3::from_shape_vec(
        (shape[0] as usize, shape[1] as usize, shape[2] as usize),
        data.to_vec(),
    )
    .map_err(|e| Error::Shape(format!("joint output `{name}`: {e}")))
}

/// First existing candidate under `dir`.
fn find(dir: &Path, candidates: &[&str]) -> Result<std::path::PathBuf> {
    for name in candidates {
        let path = dir.join(name);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(Error::MissingArtifact(format!(
        "{}: none of {}",
        dir.display(),
        candidates.join(", ")
    )))
}

/// Read the encoder's declared `audio_signal` time dimension. A fixed positive
/// length means a static-window export.
fn encoder_static_window(session: &rightkit_onnx::ort::session::Session) -> Option<usize> {
    let input = session
        .inputs()
        .iter()
        .find(|i| i.name() == "audio_signal")?;
    let rightkit_onnx::ort::value::ValueType::Tensor { shape, .. } = input.dtype() else {
        return None;
    };
    // [batch, n_mel, time]; a dynamic dimension is negative.
    match shape.get(2) {
        Some(&t) if t > 0 => Some(t as usize),
        _ => None,
    }
}

/// TWO vocabulary formats ship and we must read both, because guessing wrong is
/// silent: each piece would keep its id as literal text ("the 42 cat 87") and
/// every context-bias phrase would fail to encode without a single error.
///
///   CoreML artifact: one piece per line, id == line number.
///   ONNX artifact:   `"<piece> <id>"`, id authoritative, possibly sparse.
///
/// A line is indexed only if it splits on the LAST space into a non-empty piece
/// and a parsable integer. The format is decided for the WHOLE file, not per
/// line, so a piece that legitimately ends in " 7" cannot flip the parse
/// mid-vocabulary.
fn parse_vocab(raw: &str) -> Result<Vec<String>> {
    fn split_indexed(line: &str) -> Option<(&str, usize)> {
        let (piece, id) = line.rsplit_once(' ')?;
        if piece.is_empty() {
            return None;
        }
        Some((piece, id.parse().ok()?))
    }

    let lines: Vec<&str> = raw.lines().filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return Err(Error::Malformed("vocab.txt is empty".into()));
    }
    if lines.iter().all(|l| split_indexed(l).is_some()) {
        let mut by_id: Vec<String> = Vec::new();
        for line in &lines {
            let (piece, id) = split_indexed(line).expect("checked above");
            if id >= by_id.len() {
                by_id.resize(id + 1, String::new());
            }
            by_id[id] = piece.to_string();
        }
        Ok(by_id)
    } else {
        Ok(lines.iter().map(|l| l.to_string()).collect())
    }
}
