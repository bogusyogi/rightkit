// Native CoreML Parakeet engine for Apple Silicon (the ANE).
//
// Runs the validated 4-stage static-shape `.mlmodelc` bundle
// (MelSpectrogram · AudioEncoder · TextDecoder · Joint) directly on
// CoreML.framework via `objc2-core-ml` — in-process, no Python, no subprocess.
// The `ort` CoreML EP cannot run these graphs (empty output on the static-shape
// Parakeet conformer); this drives `MLModel`/`MLMultiArray` itself.
//
// This started as a 1:1 Rust port of the proven Python harness
// `scripts/coreml/pico_eval.py::Model.transcribe` (ground-truth 2.24% WER for
// `tdt_w15_p6`, summarized in docs/ARCHITECTURE.md). The
// load-bearing details that MUST match the Python:
//   * per-window decoder-state RESET (carrying LSTM state across a hard window
//     boundary makes TDT emit all-blank dead windows — 57% WER bug);
//   * length clamp `ceil(real_samples / 160 / 8)` encoder frames (don't decode
//     the static-window silence padding, or the decoder hallucinates a tail);
//   * TDT dur==0 ⇒ emit another token at the same frame.
use std::io::Read;
use std::path::{Path, PathBuf};

use rightkit_fs::digest::Sha256Hasher;

// Generic CoreML.framework plumbing (stage load, MLMultiArray IO, stride-aware
// read, predict) lives in the shared `coreml` module — reused by whisper_coreml.
use crate::coreml::Stage;

use super::frontend_lookahead_probe::{FrontendLookaheadProbe, FrontendLookaheadStats};
use super::fused_probe::FusedProbe;

/// Engine config read from the bundle's `pipeline.json`.
#[derive(Debug, Clone)]
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
    /// TextDecoder/Joint tensor batch rows; absent or 1 = batch-1 bundle.
    pub decoder_batch: usize,
    /// `decoder_batch_flexible: true` marks a 1..=decoder_batch enumerated bundle.
    pub decoder_batch_flexible: bool,
}

impl PipelineMeta {
    /// Batch dim of the decoder's declared (default) output shapes.
    pub(crate) fn decoder_declared_batch(&self) -> usize {
        if self.decoder_batch >= 2 && !self.decoder_batch_flexible {
            self.decoder_batch
        } else {
            1
        }
    }

    pub(crate) fn from_dir(dir: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(dir.join("pipeline.json"))
            .map_err(|e| format!("read pipeline.json: {e}"))?;
        let v: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse pipeline.json: {e}"))?;
        let durations = v["tdt_durations"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| vec![0, 1, 2, 3, 4]);
        Ok(Self {
            is_tdt: v["is_tdt"].as_bool().unwrap_or(true),
            n_mel: v["n_mel"].as_u64().unwrap_or(128) as usize,
            window_sec: v["window_sec"].as_f64().unwrap_or(15.0) as f32,
            mel_frames: v["mel_frames"].as_u64().unwrap_or(1500) as usize,
            enc_frames: v["enc_frames"].as_u64().unwrap_or(187) as usize,
            pred_layers: v["pred_layers"].as_u64().unwrap_or(2) as usize,
            pred_hidden: v["pred_hidden"].as_u64().unwrap_or(640) as usize,
            blank_id: v["blank_id"].as_i64().unwrap_or(8192) as i32,
            durations,
            decoder_batch: v["decoder_batch"].as_u64().unwrap_or(1).max(1) as usize,
            decoder_batch_flexible: v["decoder_batch_flexible"].as_bool().unwrap_or(false),
        })
    }
}

/// The loaded engine: 4 stages + vocab + resolved IO names.
/// A word with its start/end time in seconds, for SRT/VTT subtitle output.
#[derive(Clone, Debug)]
pub struct TimedTok {
    pub text: String,
    pub start: f32,
    pub end: f32,
}

/// A decoded display-token piece with its emit time in seconds.
#[derive(Clone, Debug)]
pub struct TimedPiece {
    pub text: String,
    pub start: f32,
    /// Real TDT-predicted duration in seconds. `0.0` means this piece shares a
    /// frame with the piece that follows it; the last piece of that chain
    /// carries the chain's duration.
    pub duration: f32,
}

pub struct CoreMlParakeet {
    pub(crate) bundle_dir: PathBuf,
    pub(crate) meta: PipelineMeta,
    pub(crate) mel: Stage,
    pub(crate) enc: Stage,
    pub(crate) dec: Stage,
    pub(crate) joint: Stage,
    pub(crate) mel_out: String,
    pub(crate) enc_out: String,
    pub(crate) g_name: String,
    pub(crate) state_h_out: String,
    pub(crate) state_c_out: String,
    pub(crate) joint_out: String,
    pub(crate) pieces: Vec<String>,
    pub(crate) win_samples: usize,
    // Contextual biasing (mirrors the ONNX path's ContextBias) — boosts the
    // logit of the token that continues an in-progress match of a bias phrase
    // (dictionary terms + command words). `None` = greedy as before.
    pub(crate) bias: Option<ContextBias>,
    // Beam width for `decode_window` (see `decode_window.rs`). Default `2`
    // preserves one alternative hypothesis for contextual bias; `1` restores
    // the original greedy decode. Set via `set_beam_width`.
    pub(crate) beam_width: usize,
    pub(crate) model_fingerprint: String,
    pub(crate) model_dir_sha256: Option<String>,
    pub(crate) coreml_compile_cache: String,
    pub(crate) encoder_compute: String,
    pub(crate) encoder_profile: String,
    pub(crate) decoder_compute: String,
    /// Optional hash-validated decoder/joint graph beside the split TDT model.
    pub(crate) fused_probe: Option<FusedProbe>,
    /// Evaluator override is set only by explicit benchmark APIs; production
    /// uses validated fusion whenever this is `None`.
    pub(crate) evaluator_fused_enabled: Option<bool>,
    pub(crate) fused_max_rows: usize,
    /// Memory-admitted frontend overlap worker for the fused production path.
    pub(crate) frontend_lookahead: Option<FrontendLookaheadProbe>,
    pub(crate) evaluator_frontend_lookahead_enabled: Option<bool>,
    /// Last worker snapshot remains available after an error-triggered join.
    pub(crate) frontend_lookahead_last_stats: Option<FrontendLookaheadStats>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductionActivation {
    pub fused_loaded: bool,
    pub fused_max_rows: usize,
    pub all_stage_output_backings: bool,
    pub frontend_lookahead_admitted: bool,
}

/// Contextual biasing for the greedy decode. 1:1 port of the ONNX path's
/// `rightkit_asr::parakeet_core::bias::ContextBias`, shared with the ONNX and
/// Swift runtimes so every platform biases identically. Phrases are
// Phrase-aware context bias is first-party and shared:
// `rightkit_asr::parakeet_core::bias`. macOS and the vendored ONNX crate each used
// to carry their own byte-identical copy of ContextBias, matching_prefix_len,
// next_token_bonuses, normalize_phrase_for_bias and encode_phrase_greedy, kept
// in sync by hand and by comment. That was divergence #5 of the five found in
// the 2026-09 decoder audit, and the only one being actively maintained twice.
use rightkit_asr::parakeet_core::bias::ContextBias;

pub(crate) fn stage_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{stem}.mlmodelc"))
}

fn collect_model_files(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|error| format!("read model dir: {error}"))? {
        let path = entry
            .map_err(|error| format!("read model entry: {error}"))?
            .path();
        if path.is_dir() {
            collect_model_files(root, &path, files)?;
        } else if path.is_file() {
            path.strip_prefix(root)
                .map_err(|error| format!("model fingerprint path: {error}"))?;
            files.push(path);
        }
    }
    Ok(())
}

/// Full deterministic SHA-256 over every relative model path & byte.
pub(crate) fn model_dir_sha256(dir: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    collect_model_files(dir, dir, &mut files)?;
    files.sort();
    let mut digest = Sha256Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    for path in files {
        let relative = path
            .strip_prefix(dir)
            .map_err(|error| format!("model hash path: {error}"))?;
        digest.update(relative.to_string_lossy().as_bytes());
        digest.update(&[0]);
        let mut file = std::fs::File::open(&path)
            .map_err(|error| format!("model hash open {}: {error}", path.display()))?;
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("model hash read {}: {error}", path.display()))?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
    }
    Ok(digest.finalize())
}
/// Resolve the shipped bundle dir for a CoreML Parakeet model.
/// The launch model choice's bundle directory wins; else `{base}/{name}`.
pub fn bundle_dir(base: &Path, name: &str) -> PathBuf {
    if let Some(dir) = crate::engine_paths::launch_model_choice().coreml_model_dir {
        return dir;
    }
    base.join(name)
}
