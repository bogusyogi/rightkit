//! Native Parakeet TDT on CoreML (macOS; Apple Silicon ANE, GPU or CPU).
//!
//! Runs the validated 4-stage static-shape bundle (`MelSpectrogram`,
//! `AudioEncoder`, `TextDecoder`, `Joint`; `.mlmodelc`, or `.mlpackage`
//! compiled once into a caller-supplied cache) in-process through
//! CoreML.framework: no Python, no subprocess, never the ORT CoreML provider
//! (it returns empty output on the static-shape conformer).
//!
//! Donors and precedence: HeardRight `heardright-inference::coreml_asr`
//! (production decode semantics: width-2 beam, emission reward, per-hypothesis
//! context bias, predictor-output cache, N0 detokenizer) wins on every decode
//! decision; ScrapeRight `crates/speech::coreml_asr` (the greedy original) is
//! the width-1 special case and is covered bit-for-bit by `beam_width = 1`.
//! ScrapeRight's long-form contract (cancellation, progress, SRT/VTT/JSON) is
//! in [`crate::longform`]. CutRight's `cutright-speech` is a copy of the SR
//! file and is covered by the same API.
//!
//! Load-bearing details (all inherited, all asserted by the e2e tests):
//! - the decoder state is RESET per window: carrying LSTM state across a hard
//!   window boundary makes TDT emit all-blank dead windows;
//! - only `ceil(real_samples / 160 / 8)` encoder frames are decoded, never the
//!   static-window silence padding (the decoder hallucinates a tail otherwise);
//! - TDT `dur == 0` emits another token at the same frame (capped by
//!   `max_symbols`);
//! - CoreML outputs are not C-contiguous: every read honours real strides.
//!
//! Not ported from HeardRight (app/benchmark scope, recorded in matrix.md):
//! fused decoder+joint graph, hypothesis batching, frontend look-ahead worker,
//! output-backing probes, window-health telemetry, command-locator alignment.
//! They are throughput/diagnostic features that do not change decoded text.

mod decode;

use crate::error::{Error, Result};
use crate::longform::{self, LongFormOptions, LongFormProgress, SeamPolicy};
use crate::parakeet_core::detok::{assemble_timed_pieces, DetokPolicy};
use crate::parakeet_core::policy::PruningMode;
use crate::parakeet_core::tdt_beam::default_emit_reward;
use crate::parakeet_core::{ContextBias, DecodedPiece, TranscriptionResult};
use crate::timestamps::{process_timestamps, TimestampMode};
use crate::transcriber::Transcriber;
use crate::whisper::coreml::bridge::{ComputeUnits, Stage};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub use decode::{DecodeStats, Hit};

/// One encoder frame is 80 ms (160-sample hop x 8 subsampling at 16 kHz).
pub const FRAME_SECONDS: f32 = 0.08;
const ENCODER_DIM: usize = 1024;

/// Engine config read from the bundle's `pipeline.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineMeta {
    pub is_tdt: bool,
    pub n_mel: usize,
    pub window_sec: f32,
    pub mel_frames: usize,
    pub enc_frames: usize,
    pub pred_layers: usize,
    pub pred_hidden: usize,
    pub blank_id: i32,
    pub durations: Vec<i64>,
}

impl PipelineMeta {
    /// Parse `pipeline.json`. Every key falls back to the locked export's
    /// value, matching both donors; `validate` rejects geometry that cannot
    /// run so an incomplete file cannot load "successfully" with wrong shapes.
    pub fn from_json(raw: &str) -> Result<Self> {
        let v: serde_json::Value = serde_json::from_str(raw)?;
        let durations = v["tdt_durations"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| vec![0, 1, 2, 3, 4]);
        let meta = Self {
            is_tdt: v["is_tdt"].as_bool().unwrap_or(true),
            n_mel: v["n_mel"].as_u64().unwrap_or(128) as usize,
            window_sec: v["window_sec"].as_f64().unwrap_or(15.0) as f32,
            mel_frames: v["mel_frames"].as_u64().unwrap_or(1500) as usize,
            enc_frames: v["enc_frames"].as_u64().unwrap_or(187) as usize,
            pred_layers: v["pred_layers"].as_u64().unwrap_or(2) as usize,
            pred_hidden: v["pred_hidden"].as_u64().unwrap_or(640) as usize,
            blank_id: v["blank_id"].as_i64().unwrap_or(8192) as i32,
            durations: durations.clone(),
        };
        meta.validate()?;
        Ok(meta)
    }

    pub fn from_dir(dir: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(dir.join("pipeline.json"))
            .map_err(|e| Error::Model(format!("read pipeline.json: {e}")))?;
        Self::from_json(&raw)
    }

    fn validate(&self) -> Result<()> {
        let bad = |what: &str| Err(Error::Model(format!("pipeline.json: {what}")));
        if self.n_mel == 0 || self.mel_frames == 0 || self.pred_layers == 0 || self.pred_hidden == 0 {
            return bad("zero-sized geometry");
        }
        if !(self.window_sec.is_finite() && self.window_sec > 0.0) {
            return bad("window_sec must be positive");
        }
        if self.blank_id < 0 {
            return bad("blank_id must be non-negative");
        }
        if self.durations.iter().any(|d| *d < 0) {
            return bad("tdt_durations must be non-negative");
        }
        Ok(())
    }

    pub fn window_samples(&self) -> usize {
        (self.window_sec * 16_000.0) as usize
    }
}

/// Runtime options. Defaults are HeardRight's production policy.
#[derive(Debug, Clone)]
pub struct CoreMlParakeetOptions {
    /// Compute units for MelSpectrogram + AudioEncoder.
    pub encoder_compute: ComputeUnits,
    /// Compute units for TextDecoder + Joint.
    pub decoder_compute: ComputeUnits,
    /// Beam width, clamped to `1..=8`. `1` is bit-identical to greedy decode.
    pub beam_width: usize,
    /// Per-emission score reward counteracting beam under-generation
    /// (macOS 0.15, tuned on HeardRight's canonical 224-clip corpus).
    pub emit_reward: f32,
    /// Max symbols emitted at one frame before a forced advance.
    pub max_symbols: usize,
    /// `LegacyGlobal` is the shipping search; `TimeSynchronous` is the opt-in
    /// principled variant (only hypotheses reading the same frame compete).
    pub pruning: PruningMode,
    /// Reuse the TextDecoder output across blank transitions (production).
    pub predictor_cache: bool,
    /// Detokenizer policy; `canonical_rust()` includes N0 (drops `uh`/`um`).
    /// Use `DetokPolicy::sentencepiece_only(true)` plus guarded digits for
    /// verbatim transcripts (see [`CoreMlParakeetOptions::verbatim`]).
    pub detok: DetokPolicy,
    /// Writable directory for compiling `.mlpackage` stages once.
    pub compile_cache_dir: Option<PathBuf>,
    /// Window seam policy for `transcribe_long` (see [`SeamPolicy`]).
    pub seam: SeamPolicy,
}

impl Default for CoreMlParakeetOptions {
    fn default() -> Self {
        Self {
            encoder_compute: ComputeUnits::Automatic,
            decoder_compute: ComputeUnits::Automatic,
            beam_width: 2,
            emit_reward: default_emit_reward(),
            max_symbols: 10,
            pruning: PruningMode::LegacyGlobal,
            predictor_cache: true,
            detok: DetokPolicy::canonical_rust(),
            compile_cache_dir: None,
            seam: SeamPolicy::DedupOverlap,
        }
    }
}

impl CoreMlParakeetOptions {
    /// ScrapeRight's verbatim policy: canonical digit/special handling but no
    /// N0 hesitation strip (transcripts are quoted, so `uh`/`um` are kept).
    pub fn verbatim(mut self) -> Self {
        self.detok.strip_fillers = false;
        self
    }

    /// The pre-beam greedy decode (ScrapeRight / CutRight original loop).
    pub fn greedy(mut self) -> Self {
        self.beam_width = 1;
        self.emit_reward = 0.0;
        self
    }
}

/// Result of a long-form decode.
#[derive(Debug, Clone)]
pub struct LongFormTranscript {
    pub result: TranscriptionResult,
    pub stats: DecodeStats,
    pub windows: usize,
}

/// The loaded engine: 4 stages, vocabulary and resolved IO names.
pub struct CoreMlParakeet {
    bundle_dir: PathBuf,
    meta: PipelineMeta,
    options: CoreMlParakeetOptions,
    mel: Stage,
    enc: Stage,
    dec: Stage,
    joint: Stage,
    mel_out: String,
    enc_out: String,
    g_name: String,
    state_h_out: String,
    state_c_out: String,
    joint_out: String,
    pieces: Vec<String>,
    win_samples: usize,
    bias: Option<ContextBias>,
    fingerprint: String,
}

impl CoreMlParakeet {
    /// Load a bundle directory holding the 4 stages (+ `pipeline.json`,
    /// `vocab.txt`). Stage names are matched as `<Stage>.mlmodelc`, falling
    /// back to `.mlpackage` (compiled once into `compile_cache_dir`).
    pub fn load(dir: &Path, options: CoreMlParakeetOptions) -> Result<Self> {
        let meta = PipelineMeta::from_dir(dir)?;
        let cache = options.compile_cache_dir.as_deref();
        let stage = |stem: &str, units| -> Result<Stage> {
            let compiled = dir.join(format!("{stem}.mlmodelc"));
            let path = if compiled.exists() { compiled } else { dir.join(format!("{stem}.mlpackage")) };
            Stage::load_any(&path, units, cache).map_err(Error::Model)
        };
        let mel = stage("MelSpectrogram", options.encoder_compute)?;
        let enc = stage("AudioEncoder", options.encoder_compute)?;
        let dec = stage("TextDecoder", options.decoder_compute)?;
        let joint = stage("Joint", options.decoder_compute)?;

        let first_output = |s: &Stage, what: &str| -> Result<String> {
            s.output_names()
                .into_iter()
                .next()
                .ok_or_else(|| Error::Model(format!("{what} has no output")))
        };
        let mel_out = first_output(&mel, "MelSpectrogram")?;
        let enc_out = first_output(&enc, "AudioEncoder")?;
        let joint_out = first_output(&joint, "Joint")?;
        // Decoder: g = [1,1,pred_hidden]; the two LSTM states = [pred_layers,1,pred_hidden].
        let g_name = dec.output_by_shape(&[1, 1, meta.pred_hidden]).map_err(Error::Model)?;
        let states = dec.outputs_by_shape(&[meta.pred_layers, 1, meta.pred_hidden]);
        if states.len() < 2 {
            return Err(Error::Model(format!(
                "TextDecoder: expected 2 state outputs of shape [{},1,{}], found {}",
                meta.pred_layers,
                meta.pred_hidden,
                states.len()
            )));
        }

        let vocab = std::fs::read_to_string(dir.join("vocab.txt"))
            .map_err(|e| Error::Model(format!("read vocab.txt: {e}")))?;
        let pieces: Vec<String> =
            vocab.lines().map(|ln| ln.split('\t').next().unwrap_or("").to_string()).collect();
        if pieces.is_empty() || meta.blank_id as usize > pieces.len() {
            return Err(Error::Model(format!(
                "vocab.txt has {} pieces but blank_id is {}",
                pieces.len(),
                meta.blank_id
            )));
        }

        let win_samples = meta.window_samples();
        let fingerprint = sampled_model_fingerprint(dir)?;
        let mut options = options;
        options.beam_width = options.beam_width.clamp(1, 8);
        Ok(Self {
            bundle_dir: dir.to_path_buf(),
            meta,
            options,
            mel,
            enc,
            dec,
            joint,
            mel_out,
            enc_out,
            g_name,
            state_h_out: states[0].clone(),
            state_c_out: states[1].clone(),
            joint_out,
            pieces,
            win_samples,
            bias: None,
            fingerprint,
        })
    }

    pub fn bundle_dir(&self) -> &Path {
        &self.bundle_dir
    }

    pub fn meta(&self) -> &PipelineMeta {
        &self.meta
    }

    pub fn options(&self) -> &CoreMlParakeetOptions {
        &self.options
    }

    pub fn pieces(&self) -> &[String] {
        &self.pieces
    }

    /// Model window in samples (the static window; 15 s for the shipped export).
    pub fn window_samples(&self) -> usize {
        self.win_samples
    }

    /// Fast deterministic identity (path, length, first/last 4 KiB of every
    /// file) for diagnostics and cache keys. Full-content digests belong to
    /// `rightkit-models` manifests.
    pub fn model_fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Set the decode beam width (clamped to `1..=8`).
    pub fn set_beam_width(&mut self, width: usize) {
        self.options.beam_width = width.clamp(1, 8);
    }

    /// Install contextual-biasing phrases (dictionary and command terms),
    /// encoded against the loaded vocabulary. A non-finite or non-positive
    /// `score` installs nothing. Returns the number of phrases installed.
    pub fn set_context_bias_phrases<I, S>(&mut self, phrases: I, score: f32) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let bias = ContextBias::from_phrases(&self.pieces, phrases, score);
        let installed = bias.len();
        self.bias = (!bias.is_empty()).then_some(bias);
        installed
    }

    pub fn clear_context_bias(&mut self) {
        self.bias = None;
    }

    pub fn is_tdt(&self) -> bool {
        self.meta.is_tdt
    }

    /// Decode ONE window (at most `window_samples`) into text plus timed
    /// pieces. SINGLE WINDOW ONLY: use [`transcribe_long`](Self::transcribe_long)
    /// for longer audio; a raw loop skips the quiet-cut search and welds words
    /// at every window boundary. Piece times are relative to the window start.
    pub fn transcribe_window(&self, audio: &[f32]) -> Result<(TranscriptionResult, DecodeStats)> {
        self.transcribe_window_with(audio, self.options.detok)
    }

    /// [`transcribe_window`](Self::transcribe_window) with a per-call
    /// detokenizer policy, so one loaded model serves cleaned dictation
    /// (`strip_fillers: true`) and verbatim requests (`false`) alike.
    pub fn transcribe_window_with(
        &self,
        audio: &[f32],
        detok: DetokPolicy,
    ) -> Result<(TranscriptionResult, DecodeStats)> {
        if audio.len() > self.win_samples {
            return Err(Error::Audio(format!(
                "audio is {:.1}s but this primitive decodes at most one {:.1}s window; \
                 use transcribe_long (padded-window contract)",
                audio.len() as f32 / 16_000.0,
                self.win_samples as f32 / 16_000.0
            )));
        }
        let (hits, stats) = self.decode_window(audio, 0.0)?;
        Ok((self.assemble_with(&hits, detok), stats))
    }

    /// Detokenize `(token_id, start_sec, duration_frames)` hits with the
    /// configured policy (N0, guarded digit boundaries, special-token skip).
    pub fn assemble(&self, hits: &[Hit]) -> TranscriptionResult {
        self.assemble_with(hits, self.options.detok)
    }

    /// [`assemble`](Self::assemble) with an explicit policy. Text and word
    /// timings both come from this one call, so they always follow the same policy.
    pub fn assemble_with(&self, hits: &[Hit], detok: DetokPolicy) -> TranscriptionResult {
        let decoded: Vec<DecodedPiece> = hits
            .iter()
            .map(|&(id, start, dur_frames)| DecodedPiece {
                token_id: id,
                piece: self.pieces.get(id).cloned().unwrap_or_default(),
                start,
                end: start + dur_frames as f32 * FRAME_SECONDS,
            })
            .collect();
        assemble_timed_pieces(&decoded, detok)
    }

    /// The one long-form route (padded window, quiet cut, seam stitching).
    /// `cancel` is checked before every window; `progress` fires after each.
    pub fn transcribe_long(
        &self,
        audio: &[f32],
        cancel: Option<&AtomicBool>,
        progress: &dyn Fn(&LongFormProgress),
    ) -> Result<LongFormTranscript> {
        self.transcribe_long_with(audio, self.options.detok, cancel, progress)
    }

    /// [`transcribe_long`](Self::transcribe_long) with a per-request
    /// detokenizer policy applied to every window. A host serving queued jobs
    /// passes each job's own policy, so pausing for dictation never changes it.
    pub fn transcribe_long_with(
        &self,
        audio: &[f32],
        detok: DetokPolicy,
        cancel: Option<&AtomicBool>,
        progress: &dyn Fn(&LongFormProgress),
    ) -> Result<LongFormTranscript> {
        let opts = LongFormOptions {
            window_samples: self.win_samples,
            terminal_silence: cfg!(target_os = "windows"),
            seam: self.options.seam,
        };
        let mut stats = DecodeStats::default();
        let mut windows = 0usize;
        let result = longform::transcribe_padded_window(audio, &opts, cancel, progress, |segment| {
            let (r, s) = self.transcribe_window_with(segment, detok)?;
            stats.add(&s);
            windows += 1;
            Ok(r)
        })?;
        Ok(LongFormTranscript { result, stats, windows })
    }

    /// Convenience: [`transcribe_long`](Self::transcribe_long) without
    /// cancellation or progress, returning only the transcript.
    pub fn transcribe(&self, audio: &[f32]) -> Result<TranscriptionResult> {
        Ok(self.transcribe_long(audio, None, &|_| {})?.result)
    }
}

impl Transcriber for CoreMlParakeet {
    fn transcribe_samples(
        &mut self,
        audio: Vec<f32>,
        sample_rate: u32,
        channels: u16,
        mode: Option<TimestampMode>,
    ) -> Result<TranscriptionResult> {
        let audio = crate::whisper::to_mono_16k(audio, sample_rate, channels)?;
        let mut result = self.transcribe(&audio)?;
        if let Some(mode) = mode {
            result.tokens = process_timestamps(&result.tokens, mode);
        }
        Ok(result)
    }
}

// --- model identity -------------------------------------------------------

const SAMPLE_BYTES: usize = 4_096;
const FNV1A64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A64_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a64_update(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV1A64_PRIME);
    }
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

/// Hashes every relative path, file length and first/last 4 KiB. Never reads
/// transcript or user data.
pub fn sampled_model_fingerprint(dir: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(dir, &mut files)?;
    files.sort();
    let mut hash = FNV1A64_OFFSET;
    let mut sample = vec![0u8; SAMPLE_BYTES];
    for path in &files {
        let relative = path.strip_prefix(dir).map_err(|e| Error::Model(e.to_string()))?;
        fnv1a64_update(&mut hash, relative.to_string_lossy().as_bytes());
        let mut file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        fnv1a64_update(&mut hash, &len.to_le_bytes());
        let head = len.min(SAMPLE_BYTES as u64) as usize;
        file.read_exact(&mut sample[..head])?;
        fnv1a64_update(&mut hash, &sample[..head]);
        if len > SAMPLE_BYTES as u64 {
            file.seek(SeekFrom::End(-(head as i64)))?;
            file.read_exact(&mut sample[..head])?;
            fnv1a64_update(&mut hash, &sample[..head]);
        }
    }
    Ok(format!("{hash:016x}"))
}
