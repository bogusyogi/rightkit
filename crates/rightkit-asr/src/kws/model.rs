//! Streaming Zipformer2 transducer, ported from Sherpa-ONNX 1.13.4
//! `online-zipformer2-transducer-model.cc` for batch size one.
//!
//! The encoder takes `x [1, T, feature_dim]` followed by its cached states in model
//! input order and returns `encoder_out` followed by the next states in the same order.
//! Initial states are built from the model metadata exactly as Sherpa builds them and
//! are checked against the shapes the graph declares.

use std::path::Path;

use rightkit_onnx::ort;
use rightkit_onnx::ort::session::{Session, SessionInputValue};
use rightkit_onnx::ort::value::{Tensor, TensorRef};

use crate::error::{Error, Result};

/// One cached encoder state carried from chunk to chunk.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StateTensor {
    F32 { shape: Vec<i64>, data: Vec<f32> },
    I64 { shape: Vec<i64>, data: Vec<i64> },
}

impl StateTensor {
    fn zeros_f32(shape: Vec<i64>) -> Self {
        let n = shape.iter().product::<i64>() as usize;
        Self::F32 {
            shape,
            data: vec![0.0; n],
        }
    }

    fn shape(&self) -> &[i64] {
        match self {
            Self::F32 { shape, .. } | Self::I64 { shape, .. } => shape,
        }
    }

    fn input(&self) -> Result<SessionInputValue<'_>> {
        Ok(match self {
            Self::F32 { shape, data } => {
                TensorRef::from_array_view((shape.as_slice(), data.as_slice()))?.into()
            }
            Self::I64 { shape, data } => {
                TensorRef::from_array_view((shape.as_slice(), data.as_slice()))?.into()
            }
        })
    }
}

/// Encoder output for one chunk: `frames` rows of `dim` values.
pub(crate) struct EncoderOut {
    pub frames: usize,
    pub dim: usize,
    pub data: Vec<f32>,
}

impl EncoderOut {
    pub(crate) fn frame(&self, t: usize) -> &[f32] {
        &self.data[t * self.dim..(t + 1) * self.dim]
    }
}

pub(crate) struct Zipformer2Transducer {
    encoder: Session,
    decoder: Session,
    joiner: Session,
    /// `T`: feature frames fed per encoder call.
    chunk_size: usize,
    /// `decode_chunk_len`: feature frames the stream advances per call.
    chunk_shift: usize,
    feature_dim: usize,
    vocab_size: usize,
    context_size: usize,
    init_states: Vec<StateTensor>,
}

fn session(path: &Path, threads: usize) -> Result<Session> {
    rightkit_onnx::SessionOptions::new(rightkit_onnx::ExecutionProvider::Cpu)
        .with_threads(Some(threads.max(1)))
        .build_from_file(path)
        .map_err(|e| Error::Model(format!("load {}: {e}", path.display())))
}

fn meta(session: &Session, key: &str, file: &Path) -> Result<String> {
    session
        .metadata()?
        .custom(key)
        .ok_or_else(|| Error::Model(format!("{}: metadata `{key}` missing", file.display())))
}

fn meta_int(session: &Session, key: &str, file: &Path) -> Result<i64> {
    let raw = meta(session, key, file)?;
    raw.trim()
        .parse()
        .map_err(|_| Error::Model(format!("{}: metadata `{key}` = `{raw}`", file.display())))
}

fn meta_vec(session: &Session, key: &str, file: &Path) -> Result<Vec<i64>> {
    let raw = meta(session, key, file)?;
    raw.split(',')
        .map(|v| v.trim().parse())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| Error::Model(format!("{}: metadata `{key}` = `{raw}`", file.display())))
}

impl Zipformer2Transducer {
    pub(crate) fn load(
        encoder: &Path,
        decoder: &Path,
        joiner: &Path,
        feature_dim: usize,
        threads: usize,
    ) -> Result<Self> {
        let enc = session(encoder, threads)?;
        let model_type = meta(&enc, "model_type", encoder)?;
        if model_type != "zipformer2" {
            return Err(Error::Model(format!(
                "{}: model_type `{model_type}` is not zipformer2",
                encoder.display()
            )));
        }
        let encoder_dims = meta_vec(&enc, "encoder_dims", encoder)?;
        let query_head_dims = meta_vec(&enc, "query_head_dims", encoder)?;
        let value_head_dims = meta_vec(&enc, "value_head_dims", encoder)?;
        let num_heads = meta_vec(&enc, "num_heads", encoder)?;
        let num_layers = meta_vec(&enc, "num_encoder_layers", encoder)?;
        let kernels = meta_vec(&enc, "cnn_module_kernels", encoder)?;
        let left_context = meta_vec(&enc, "left_context_len", encoder)?;
        let chunk_size = meta_int(&enc, "T", encoder)? as usize;
        let chunk_shift = meta_int(&enc, "decode_chunk_len", encoder)? as usize;
        let stacks = encoder_dims.len();
        for (name, v) in [
            ("query_head_dims", &query_head_dims),
            ("value_head_dims", &value_head_dims),
            ("num_heads", &num_heads),
            ("num_encoder_layers", &num_layers),
            ("cnn_module_kernels", &kernels),
            ("left_context_len", &left_context),
        ] {
            if v.len() != stacks {
                return Err(Error::Model(format!(
                    "{}: metadata `{name}` has {} entries for {stacks} stacks",
                    encoder.display(),
                    v.len()
                )));
            }
        }
        if chunk_shift == 0 || chunk_shift > chunk_size {
            return Err(Error::Model(format!(
                "{}: decode_chunk_len {chunk_shift} invalid for T {chunk_size}",
                encoder.display()
            )));
        }

        // GetEncoderInitStates.
        let mut init_states = Vec::new();
        for i in 0..stacks {
            let key_dim = query_head_dims[i] * num_heads[i];
            let value_dim = value_head_dims[i] * num_heads[i];
            let nonlin_attn_head_dim = 3 * encoder_dims[i] / 4;
            for _ in 0..num_layers[i] {
                init_states.push(StateTensor::zeros_f32(vec![left_context[i], 1, key_dim]));
                init_states.push(StateTensor::zeros_f32(vec![
                    1,
                    1,
                    left_context[i],
                    nonlin_attn_head_dim,
                ]));
                init_states.push(StateTensor::zeros_f32(vec![left_context[i], 1, value_dim]));
                init_states.push(StateTensor::zeros_f32(vec![left_context[i], 1, value_dim]));
                init_states.push(StateTensor::zeros_f32(vec![1, encoder_dims[i], kernels[i] / 2]));
                init_states.push(StateTensor::zeros_f32(vec![1, encoder_dims[i], kernels[i] / 2]));
            }
        }
        let embed_dim = (((feature_dim as i64 - 1) / 2) - 1) / 2;
        init_states.push(StateTensor::zeros_f32(vec![1, 128, 3, embed_dim]));
        init_states.push(StateTensor::I64 {
            shape: vec![1],
            data: vec![0],
        });

        let inputs = enc.inputs();
        if inputs.len() != init_states.len() + 1 || enc.outputs().len() != inputs.len() {
            return Err(Error::Model(format!(
                "{}: {} inputs / {} outputs, expected {} of each",
                encoder.display(),
                inputs.len(),
                enc.outputs().len(),
                init_states.len() + 1
            )));
        }
        for (outlet, state) in inputs[1..].iter().zip(&init_states) {
            let declared = outlet.dtype().tensor_shape().map(|s| s.to_vec());
            let compatible = declared.as_ref().is_some_and(|d| {
                d.len() == state.shape().len()
                    && d.iter().zip(state.shape()).all(|(want, got)| *want < 0 || want == got)
            });
            if !compatible {
                return Err(Error::Model(format!(
                    "{}: input `{}` declares {declared:?}; metadata gives {:?}",
                    encoder.display(),
                    outlet.name(),
                    state.shape()
                )));
            }
        }

        let dec = session(decoder, threads)?;
        let vocab_size = meta_int(&dec, "vocab_size", decoder)? as usize;
        let context_size = meta_int(&dec, "context_size", decoder)? as usize;
        if context_size == 0 {
            return Err(Error::Model(format!("{}: context_size 0", decoder.display())));
        }
        let join = session(joiner, threads)?;
        Ok(Self {
            encoder: enc,
            decoder: dec,
            joiner: join,
            chunk_size,
            chunk_shift,
            feature_dim,
            vocab_size,
            context_size,
            init_states,
        })
    }

    pub(crate) fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    pub(crate) fn chunk_shift(&self) -> usize {
        self.chunk_shift
    }

    pub(crate) fn vocab_size(&self) -> usize {
        self.vocab_size
    }

    pub(crate) fn context_size(&self) -> usize {
        self.context_size
    }

    pub(crate) fn init_states(&self) -> Vec<StateTensor> {
        self.init_states.clone()
    }

    /// One encoder call on `chunk_size` frames; `states` is replaced by the next states.
    pub(crate) fn run_encoder(
        &mut self,
        features: &[f32],
        states: &mut Vec<StateTensor>,
    ) -> Result<EncoderOut> {
        let x_shape = [1i64, self.chunk_size as i64, self.feature_dim as i64];
        let mut inputs: Vec<SessionInputValue<'_>> = Vec::with_capacity(states.len() + 1);
        inputs.push(TensorRef::from_array_view((x_shape, features))?.into());
        for state in states.iter() {
            inputs.push(state.input()?);
        }
        let outputs = self.encoder.run(inputs.as_slice())?;
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        if shape.len() != 3 || shape[0] != 1 {
            return Err(Error::Model(format!("encoder_out shape {shape:?}")));
        }
        let out = EncoderOut {
            frames: shape[1] as usize,
            dim: shape[2] as usize,
            data: data.to_vec(),
        };
        let mut next = Vec::with_capacity(states.len());
        for (i, previous) in states.iter().enumerate() {
            let value = &outputs[i + 1];
            next.push(match previous {
                StateTensor::F32 { .. } => {
                    let (shape, data) = value.try_extract_tensor::<f32>()?;
                    StateTensor::F32 {
                        shape: shape.to_vec(),
                        data: data.to_vec(),
                    }
                }
                StateTensor::I64 { .. } => {
                    let (shape, data) = value.try_extract_tensor::<i64>()?;
                    StateTensor::I64 {
                        shape: shape.to_vec(),
                        data: data.to_vec(),
                    }
                }
            });
        }
        *states = next;
        Ok(out)
    }

    /// Decoder output for `rows` contexts (`rows * context_size` token ids).
    pub(crate) fn run_decoder(&mut self, contexts: Vec<i64>) -> Result<(usize, Vec<f32>)> {
        let rows = contexts.len() / self.context_size;
        let y = Tensor::from_array(([rows as i64, self.context_size as i64], contexts))?;
        let outputs = self.decoder.run(ort::inputs![y])?;
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        Ok((shape[shape.len() - 1] as usize, data.to_vec()))
    }

    /// Joiner logits `[rows, vocab_size]` for paired encoder and decoder rows.
    pub(crate) fn run_joiner(
        &mut self,
        encoder_rows: &[f32],
        decoder_rows: &[f32],
        rows: usize,
    ) -> Result<Vec<f32>> {
        let enc_dim = encoder_rows.len() / rows;
        let dec_dim = decoder_rows.len() / rows;
        let enc = TensorRef::from_array_view(([rows as i64, enc_dim as i64], encoder_rows))?;
        let dec = TensorRef::from_array_view(([rows as i64, dec_dim as i64], decoder_rows))?;
        let outputs = self.joiner.run(ort::inputs![enc, dec])?;
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        if shape.len() != 2 || shape[1] as usize != self.vocab_size {
            return Err(Error::Model(format!("joiner logit shape {shape:?}")));
        }
        Ok(data.to_vec())
    }
}
