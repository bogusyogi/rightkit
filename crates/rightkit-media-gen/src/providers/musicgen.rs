//! Local MusicGen (ONNX) music provider — keyless, native, no Python/Node.
//!
//! Runs the free Xenova/musicgen-small ONNX export (MIT; transformers.js
//! graph layout) through ONNX Runtime, reusing the app's existing dynamic ORT
//! discovery (`crate::qts::models::find_onnxruntime` via rightkit-onnx).
//! Pipeline: unigram tokenizer → T5 text encoder → autoregressive
//! `decoder_model_merged` with KV cache and classifier-free guidance →
//! EnCodec decode → 32 kHz mono WAV.
//!
//! Model files (installed by the model manager under the shared models dir):
//! ```text
//! <models>/musicgen-small/
//!   tokenizer.json
//!   onnx/text_encoder.onnx
//!   onnx/decoder_model_merged.onnx
//!   onnx/encodec_decode.onnx
//! ```
//! `RIGHTKIT_MUSICGEN_MODEL_DIR` overrides the location (tests, dev).

use super::{
    Durations, GenRequest, Kind, ModelInfo, Output, PollOutcome, Provider, ProviderError,
    SubmitOutcome,
};
use std::path::{Path, PathBuf};

pub const ID: &str = "local-musicgen";
pub const MODEL_SMALL: &str = "musicgen-small";

// Flat layout under models/musicgen-small/ — the model-manager installer
// places each downloaded file directly in the spec dest dir.
const ENCODER: &str = "text_encoder.onnx";
/// Prefill graph (no past-KV inputs): runs the start-token column.
const DECODER: &str = "decoder_model.onnx";
/// Cached-decode graph: runs one step against `past_key_values.*` inputs.
const DECODER_PAST: &str = "decoder_with_past_model.onnx";
const ENCODEC: &str = "encodec_decode.onnx";
const TOKENIZER: &str = "tokenizer.json";

// Pinned to the Xenova/musicgen-small export @ main (MIT, HF), measured from
// the official files on 2026-10-06.
const TOKENIZER_BYTES: u64 = 2_422_191;
const TOKENIZER_SHA: &str = "a45ba2af379e1d559dbbdf176bb6a338945019534a20e15e07d22bbd0a7544b3";
const ENCODER_BYTES: u64 = 438_689_759;
const ENCODER_SHA: &str = "f7962c2408b03ebb37252e6d1a3179517e8c6127b9fda4e0beab84dfa09aab80";
const DECODER_BYTES: u64 = 1_691_136_293;
const DECODER_SHA: &str = "8497084fd0d4f7733ae1fa481c6d39f5130e32eff27da717fa07c6e7deb1af71";
const DECODER_PAST_BYTES: u64 = 1_486_479_458;
const DECODER_PAST_SHA: &str = "05501e600138f1d09220f994f924280f5b403aab503e6989acbf8ec3456e2637";
const ENCODEC_BYTES: u64 = 118_056_304;
const ENCODEC_SHA: &str = "847918300a85c9a8f93333f482c5740e07c4e2599faed839bbec73fd0d588073";

const PAD_ID: i64 = 2048;
const NUM_CODEBOOKS: usize = 4;
/// EnCodec produces 320 samples per codebook frame at 32 kHz (50 Hz).
const FRAMES_PER_SEC: f64 = 50.0;
const SAMPLE_RATE: u32 = 32_000;

static MODELS: [ModelInfo; 1] = [ModelInfo {
    provider: ID,
    model: MODEL_SMALL,
    kind: Kind::Music,
    label: "MusicGen-small text-to-music (local ONNX, free)",
    est_usd: 0.0,
    durations: Durations::range(1, 30, Some(10)),
}];

fn bad(m: impl Into<String>) -> ProviderError {
    ProviderError::new("invalid_request", m, false)
}

/// `RIGHTKIT_MUSICGEN_MODEL_DIR` or `<data>/models/musicgen-small` — same
/// root convention as `local_lipsync::model_dir`.
pub fn model_dir() -> PathBuf {
    match std::env::var_os("RIGHTKIT_MUSICGEN_MODEL_DIR").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => crate::qts::models::data_root()
            .join("models")
            .join(MODEL_SMALL),
    }
}

/// Required model files: (relative path, exact byte size, sha256 hex).
/// Sizes/hashes are pinned to the Xenova/musicgen-small ONNX export.
const FILES: [(&str, u64, &str); 5] = [
    (TOKENIZER, TOKENIZER_BYTES, TOKENIZER_SHA),
    (ENCODER, ENCODER_BYTES, ENCODER_SHA),
    (DECODER, DECODER_BYTES, DECODER_SHA),
    (DECODER_PAST, DECODER_PAST_BYTES, DECODER_PAST_SHA),
    (ENCODEC, ENCODEC_BYTES, ENCODEC_SHA),
];

/// Metadata-only readiness: presence + byte size, never loads ORT or a model.
fn files_ready(dir: &Path) -> bool {
    FILES.iter().all(|(f, size, _)| {
        std::fs::metadata(dir.join(f))
            .map(|m| m.len() == *size)
            .unwrap_or(false)
    })
}

/// SHA-256 check of every model file — run once before first generation,
/// same contract as `local_lipsync::model_verified`.
fn verify_files(dir: &Path) -> Result<(), String> {
    for (f, size, sha) in FILES {
        let path = dir.join(f);
        let meta = std::fs::metadata(&path)
            .map_err(|_| format!("missing model file {}", path.display()))?;
        if meta.len() != size {
            return Err(format!("{f} has {} bytes, expected {size}", meta.len()));
        }
        let got = rightkit_fs::digest::sha256_file(&path).map_err(|e| e.to_string())?;
        if got != sha {
            return Err(format!("{f} failed its SHA-256 check"));
        }
    }
    Ok(())
}

pub struct LocalMusicGen;

impl LocalMusicGen {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalMusicGen {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for LocalMusicGen {
    fn id(&self) -> &'static str {
        ID
    }
    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }
    /// `seconds`: 1 to 30 seconds (fractions allowed) in `seconds`; `duration` and `seconds_total` are read as well.
    fn capabilities(&self, _model: &str) -> crate::capabilities::Capabilities {
        crate::capabilities::Capabilities::default()
            .mode("prompt")
            .setting(
                "length",
                crate::capabilities::length("seconds", "s", 1.0, 30.0, false),
            )
    }

    fn needs_key(&self) -> bool {
        false
    }
    /// Local engine: a submit has no remote charge, so a replay is free.
    fn submit_is_idempotent(&self) -> bool {
        true
    }

    fn runtime_ready(&self) -> Option<Result<(), ProviderError>> {
        let dir = model_dir();
        if !files_ready(&dir) {
            return Some(Err(ProviderError::new(
                "not_installed",
                format!(
                    "musicgen-small model files are not installed under {}",
                    dir.display()
                ),
                false,
            )));
        }
        if crate::qts::models::find_onnxruntime().is_none() {
            return Some(Err(ProviderError::new(
                "runtime_missing",
                "onnxruntime is not installed (same runtime bundle as local TTS)",
                false,
            )));
        }
        Some(Ok(()))
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        if req.model != MODEL_SMALL || req.kind != Kind::Music {
            return Err(bad(format!("unknown {ID} music model {}", req.model)));
        }
        if req.prompt.trim().is_empty() {
            return Err(bad("music prompt is empty"));
        }
        seconds(req)?;
        Ok(0.0)
    }

    /// Text route: full local generation, synchronous. Runs inside the job
    /// runner, so progress still flows through the job store.
    fn submit(&self, req: &GenRequest, _key: &str) -> Result<SubmitOutcome, ProviderError> {
        if req.model != MODEL_SMALL || req.kind != Kind::Music {
            return Err(bad("model does not match request kind"));
        }
        if req.prompt.trim().is_empty() {
            return Err(bad("music prompt is empty"));
        }
        let secs = seconds(req)?;
        let dir = model_dir();
        if !files_ready(&dir) {
            return Err(ProviderError::new(
                "not_installed",
                format!("incomplete model files under {}", dir.display()),
                true,
            ));
        }
        engine::verify_or_cached(&dir)
            .map_err(|m| ProviderError::new("models_invalid", m, true))?;
        let opts = GenOpts {
            frames: (secs * FRAMES_PER_SEC) as usize,
            guidance: f64_param(req, "guidance_scale").unwrap_or(3.0) as f32,
            temperature: f64_param(req, "temperature").unwrap_or(1.0) as f32,
            top_k: u64_param(req, "top_k").unwrap_or(250) as usize,
            seed: u64_param(req, "seed").unwrap_or(0x9e3779b97f4a7c15),
        };
        let wav = engine::generate(&dir, &req.prompt, &opts)?;
        Ok(SubmitOutcome::Completed {
            outputs: vec![Output::Bytes {
                bytes: wav,
                mime: "audio/wav".into(),
            }],
        })
    }

    fn poll(&self, _: &GenRequest, _: &str, _: &str) -> Result<PollOutcome, ProviderError> {
        Err(ProviderError::new(
            "unsupported",
            "local provider is synchronous",
            false,
        ))
    }

    fn test_key(&self, _: &str) -> Result<(), ProviderError> {
        match self.runtime_ready() {
            Some(r) => r,
            None => Ok(()),
        }
    }

    fn fetch(&self, output: &Output) -> Result<(Vec<u8>, String), ProviderError> {
        match output {
            Output::Bytes { bytes, mime } => Ok((bytes.clone(), mime.clone())),
            Output::Url { .. } => Err(ProviderError::new(
                "unsupported",
                "local provider returns bytes, not URLs",
                false,
            )),
            Output::Path { .. } | Output::Finalized { .. } => Err(ProviderError::new(
                "unsupported",
                "path outputs are adopted by the library",
                false,
            )),
        }
    }
}

fn f64_param(req: &GenRequest, k: &str) -> Option<f64> {
    req.params
        .get(k)
        .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
}
fn u64_param(req: &GenRequest, k: &str) -> Option<u64> {
    req.params
        .get(k)
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
}

fn seconds(req: &GenRequest) -> Result<f64, ProviderError> {
    let v = req
        .params
        .get("seconds")
        .or_else(|| req.params.get("duration"))
        .or_else(|| req.params.get("seconds_total"));
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(10.0);
    };
    let n = v
        .as_f64()
        .or_else(|| {
            v.as_str()
                .and_then(|s| s.trim().trim_end_matches('s').parse().ok())
        })
        .ok_or_else(|| bad("music duration must be a number of seconds"))?;
    if !(1.0..=30.0).contains(&n) {
        return Err(bad(
            "music duration must be 1-30 seconds (musicgen-small cap)",
        ));
    }
    Ok(n)
}

pub struct GenOpts {
    pub frames: usize,
    pub guidance: f32,
    pub temperature: f32,
    pub top_k: usize,
    pub seed: u64,
}

/// Minimal Unigram (T5/SentencePiece) tokenizer for `tokenizer.json`:
/// Metaspace pre-tokenization + Viterbi over piece scores + byte fallback.
mod tokenizer {
    use serde_json::Value;

    pub struct Unigram {
        /// (piece, score); index == token id.
        pieces: Vec<(String, f32)>,
        /// Byte fallback ids: `<0xNN>` → id.
        byte_ids: [Option<i64>; 256],
        unk_id: i64,
        eos_id: i64,
    }

    impl Unigram {
        pub fn load(path: &std::path::Path) -> Result<Self, String> {
            let v: Value = serde_json::from_slice(
                &std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?,
            )
            .map_err(|e| format!("parse {}: {e}", path.display()))?;
            let model = &v["model"];
            if model["type"].as_str() != Some("Unigram") {
                return Err(format!(
                    "tokenizer model is {:?}, expected Unigram",
                    model["type"]
                ));
            }
            let vocab = model["vocab"]
                .as_array()
                .ok_or("tokenizer.json has no vocab")?;
            let mut pieces = Vec::with_capacity(vocab.len());
            let mut byte_ids = [None; 256];
            let mut unk_id = model["unk_id"].as_i64().unwrap_or(0);
            let mut eos_id = 1i64;
            for (id, entry) in vocab.iter().enumerate() {
                let piece = entry[0]
                    .as_str()
                    .ok_or("vocab entry missing piece")?
                    .to_string();
                let score = entry[1].as_f64().unwrap_or(f64::NEG_INFINITY) as f32;
                if piece == "<unk>" {
                    unk_id = id as i64;
                }
                if piece == "</s>" {
                    eos_id = id as i64;
                }
                if let Some(h) = piece.strip_prefix("<0x").and_then(|h| h.strip_suffix('>')) {
                    if let Ok(b) = u8::from_str_radix(h, 16) {
                        byte_ids[b as usize] = Some(id as i64);
                    }
                }
                pieces.push((piece, score));
            }
            Ok(Self {
                pieces,
                byte_ids,
                unk_id,
                eos_id,
            })
        }

        /// Metaspace normalization: escape whitespace to `▁`, prefix `▁`.
        fn normalize(text: &str) -> Vec<u8> {
            let mut s = String::with_capacity(text.len() + 4);
            s.push('▁');
            for c in text.trim().chars() {
                if c == ' ' || c == '\t' || c == '\n' {
                    s.push('▁');
                } else {
                    s.push(c);
                }
            }
            s.into_bytes()
        }

        /// Viterbi over UTF-8 boundary positions; byte_fallback pieces cover
        /// any byte so encode never fails on unusual input.
        pub fn encode(&self, text: &str) -> Vec<i64> {
            let bytes = Self::normalize(text);
            let n = bytes.len();
            const NEG: f32 = -1e30;
            let mut best = vec![NEG; n + 1];
            let mut prev: Vec<Option<(usize, i64)>> = vec![None; n + 1];
            best[0] = 0.0;
            // Prefix index: first byte → piece ids (byte pieces included so a
            // single non-ASCII byte can still map through <0xNN>).
            let mut heads: std::collections::HashMap<u8, Vec<usize>> =
                std::collections::HashMap::new();
            for (id, (piece, _)) in self.pieces.iter().enumerate() {
                if let Some(&b) = piece.as_bytes().first() {
                    heads.entry(b).or_default().push(id);
                }
            }
            for i in 0..n {
                if best[i] <= NEG / 2.0 {
                    continue;
                }
                let mut covered = false;
                if let Some(ids) = heads.get(&bytes[i]) {
                    for &id in ids {
                        let p = self.pieces[id].0.as_bytes();
                        if p.len() == 1
                            && p[0] != bytes[i]
                            && self.byte_ids[bytes[i] as usize] != Some(id as i64)
                        {
                            continue;
                        }
                        if bytes[i..].starts_with(p) {
                            covered = true;
                            let j = i + p.len();
                            let score = best[i] + self.pieces[id].1;
                            if score > best[j] {
                                best[j] = score;
                                prev[j] = Some((i, id as i64));
                            }
                        }
                    }
                }
                // Byte fallback: always a valid one-byte step.
                if !covered || true {
                    if let Some(id) = self.byte_ids[bytes[i] as usize] {
                        let score = best[i] + self.pieces[id as usize].1;
                        if score > best[i + 1] {
                            best[i + 1] = score;
                            prev[i + 1] = Some((i, id));
                        }
                    }
                }
            }
            let mut ids = Vec::new();
            if prev[n].is_none() {
                // Degenerate path: emit unk then eos.
                ids.push(self.unk_id);
                ids.push(self.eos_id);
                return ids;
            }
            let mut j = n;
            while let Some((i, id)) = prev[j] {
                ids.push(id);
                j = i;
            }
            ids.reverse();
            ids.push(self.eos_id);
            ids
        }
    }
}

mod engine {
    use super::{tokenizer::Unigram, GenOpts, ProviderError, NUM_CODEBOOKS, PAD_ID, SAMPLE_RATE};
    use crate::providers::{model_load_failed, ort_init_failure};
    use rightkit_onnx::ort::session::builder::GraphOptimizationLevel;
    use rightkit_onnx::ort::session::Session;
    use rightkit_onnx::ort::value::{DynValue, Tensor};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    fn ort_err(e: impl std::fmt::Display) -> String {
        format!("onnxruntime: {e}")
    }

    /// SHA-256 verification of the model files, memoized per directory so the
    /// multi-GB check runs at most once per process+model.
    pub fn verify_or_cached(dir: &Path) -> Result<(), String> {
        static CACHE: OnceLock<std::sync::Mutex<HashMap<PathBuf, Result<(), String>>>> =
            OnceLock::new();
        let m = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
        match g.get(dir) {
            Some(r) => r.clone(),
            None => {
                let r = super::verify_files(dir);
                g.insert(dir.to_path_buf(), r.clone());
                r
            }
        }
    }

    /// `run_binding` needs every output bound — bind them all to CPU memory.
    fn bind_outputs(
        s: &Session,
        binding: &mut rightkit_onnx::ort::session::IoBinding,
    ) -> Result<(), String> {
        use rightkit_onnx::ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
        let cpu = MemoryInfo::new(
            AllocationDevice::CPU,
            0,
            AllocatorType::Device,
            MemoryType::CPUOutput,
        )
        .map_err(ort_err)?;
        for o in s.outputs() {
            binding
                .bind_output_to_device(o.name(), &cpu)
                .map_err(ort_err)?;
        }
        Ok(())
    }

    /// Initialize ORT against the app-found dylib once per process. A failure is classified
    /// (`not_installed` when the library is absent, `runtime_load_failed` when it exists but will not load).
    fn ort_init() -> Result<(), ProviderError> {
        static INIT: OnceLock<Result<(), ProviderError>> = OnceLock::new();
        INIT.get_or_init(|| {
            // Windows needs the DLL's directory on PATH too.
            #[cfg(windows)]
            {
                let runtime = crate::qts::models::find_onnxruntime().ok_or_else(|| {
                    ort_init_failure("ONNX Runtime shared library not configured by RightKit")
                })?;
                rightkit_onnx::prepend_runtime_dir_to_path(&runtime);
            }
            crate::qts::models::init_onnxruntime()
                .map(|_| ())
                .map_err(|e| ort_init_failure(&e))
        })
        .clone()
    }

    /// Opening a present model file failed (corrupt file, unsupported opset, ...): non-retryable.
    fn load(path: &Path, threads: usize) -> Result<Session, ProviderError> {
        let built = (|| -> Result<Session, String> {
            let mut b = Session::builder(rightkit_onnx::ort_environment().map_err(ort_err)?)
                .map_err(ort_err)?;
            b = b
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(ort_err)?
                .with_intra_threads(threads.max(1))
                .map_err(ort_err)?;
            b.commit_from_file(path).map_err(ort_err)
        })();
        built.map_err(|m| model_load_failed(path, m))
    }

    /// (name, declared dtype) for every declared input, in order.
    fn inlets(s: &Session) -> Vec<(String, rightkit_onnx::ort::value::ValueType)> {
        s.inputs()
            .iter()
            .map(|i| (i.name().to_string(), i.dtype().clone()))
            .collect()
    }

    /// XORShift64 RNG — deterministic per seed, no extra deps.
    struct Rng(u64);
    impl Rng {
        fn next_f64(&mut self) -> f64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            (x >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// Temperature + top-k sampling of one logits row.
    fn sample(logits: &[f32], temperature: f32, top_k: usize, rng: &mut Rng) -> i64 {
        let t = temperature.max(1e-4);
        let mut scored: Vec<(usize, f32)> = logits
            .iter()
            .enumerate()
            .map(|(i, &l)| (i, l / t))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k.max(1));
        let max = scored[0].1;
        let mut sum = 0.0f64;
        let probs: Vec<f64> = scored
            .iter()
            .map(|&(_, l)| {
                let p = ((l - max) as f64).exp();
                sum += p;
                p
            })
            .collect();
        let mut r = rng.next_f64() * sum;
        for (i, &p) in probs.iter().enumerate() {
            r -= p;
            if r <= 0.0 {
                return scored[i].0 as i64;
            }
        }
        scored[0].0 as i64
    }

    /// `past_key_values.*` input name ↔ `present.*` output name.
    fn past_name(present: &str) -> String {
        present.replacen("present.", "past_key_values.", 1)
    }

    /// The generation entry point: prompt → 32 kHz mono WAV bytes. Runtime and session-open
    /// failures keep their classified codes; an inference failure is `local_gen_failed`.
    pub fn generate(dir: &Path, prompt: &str, opts: &GenOpts) -> Result<Vec<u8>, ProviderError> {
        ort_init()?;
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 8))
            .unwrap_or(4);

        let tok_path = dir.join(super::TOKENIZER);
        let tok = Unigram::load(&tok_path).map_err(|m| model_load_failed(&tok_path, m))?;

        let enc = load(&dir.join(super::ENCODER), threads)?;
        let dec0 = load(&dir.join(super::DECODER), threads)?;
        let decn = load(&dir.join(super::DECODER_PAST), threads)?;
        let codec = load(&dir.join(super::ENCODEC), threads)?;

        run(opts, prompt, &tok, enc, dec0, decn, codec)
            .map_err(|m| ProviderError::new("local_gen_failed", m, true))
    }

    /// Inference over sessions already opened. Its `String` errors surface as `local_gen_failed`.
    #[allow(clippy::too_many_arguments)]
    fn run(
        opts: &GenOpts,
        prompt: &str,
        tok: &Unigram,
        mut enc: Session,
        mut dec0: Session,
        mut decn: Session,
        mut codec: Session,
    ) -> Result<Vec<u8>, String> {
        let enc_ids = tok.encode(prompt);
        let n_ctx = enc_ids.len();

        // --- text encoder → last_hidden_state [1, N, d] ---
        let encode = |enc: &mut Session, ids: Vec<i64>| -> Result<(Vec<f32>, usize), String> {
            let n = ids.len();
            let ids_v: DynValue = Tensor::<i64>::from_array(([1usize, n], ids))
                .map_err(ort_err)?
                .into_dyn();
            let mask_v: DynValue = Tensor::<i64>::from_array(([1usize, n], vec![1i64; n]))
                .map_err(ort_err)?
                .into_dyn();
            let mut binding = enc.create_binding().map_err(ort_err)?;
            for (name, _ty) in inlets(enc) {
                match name.as_str() {
                    "input_ids" => binding.bind_input(name.as_str(), &ids_v).map_err(ort_err)?,
                    "attention_mask" => binding
                        .bind_input(name.as_str(), &mask_v)
                        .map_err(ort_err)?,
                    other => return Err(format!("text_encoder wants unknown input {other}")),
                }
            }
            bind_outputs(enc, &mut binding)?;
            let outs = enc.run_binding(&binding).map_err(ort_err)?;
            let hs = outs
                .get("last_hidden_state")
                .or_else(|| outs.keys().next().and_then(|n| outs.get(n)))
                .ok_or("text_encoder returned no hidden state")?;
            let (shape, vals) = hs.try_extract_tensor::<f32>().map_err(ort_err)?;
            let d = *shape.last().unwrap_or(&0) as usize;
            let mut v = vals.to_vec();
            if let Some(n_seq) = shape.get(1).copied().filter(|s| *s > 0).map(|s| s as usize) {
                v.truncate(n_seq * d);
            }
            Ok((v, d))
        };
        let (hs_cond, d) = encode(&mut enc, enc_ids)?;
        if d == 0 {
            return Err("text_encoder hidden state has no feature dim".into());
        }

        // --- classifier-free guidance: uncond = encoded empty text ---
        let use_cfg = opts.guidance > 1.0;
        let hs_uncond: Vec<f32> = if use_cfg {
            let (v, du) = encode(&mut enc, tok.encode(""))?;
            // Broadcast to the cond sequence length.
            let mut out = Vec::with_capacity(n_ctx * du);
            while out.len() < n_ctx * du {
                out.extend_from_slice(&v);
            }
            out.truncate(n_ctx * du);
            if du != d {
                return Err(format!("uncond hidden dim {du} != cond {d}"));
            }
            out
        } else {
            Vec::new()
        };

        // batch is 2 (cond, uncond) with CFG else 1; the merged graph expands
        // encoder outputs/KV across the codebook rows itself.
        let batch = if use_cfg { 2usize } else { 1 };
        let rows = batch * NUM_CODEBOOKS;
        let mut ehs = Vec::with_capacity(batch * n_ctx * d);
        ehs.extend_from_slice(&hs_cond);
        if use_cfg {
            ehs.extend_from_slice(&hs_uncond);
        }
        let ehs_v: DynValue = Tensor::<f32>::from_array(([batch, n_ctx, d], ehs))
            .map_err(ort_err)?
            .into_dyn();
        let eam_v: DynValue =
            Tensor::<i64>::from_array(([batch, n_ctx], vec![1i64; batch * n_ctx]))
                .map_err(ort_err)?
                .into_dyn();

        // Raw generated codes per codebook; index t holds codes sampled at
        // step t. Delay pattern applied when building decoder inputs.
        let mut codes: Vec<Vec<i64>> =
            vec![Vec::with_capacity(opts.frames + NUM_CODEBOOKS); NUM_CODEBOOKS];
        // KV cache: `present.*` outputs feed `past_key_values.*` inputs on the
        // next step. DynValue is a refcounted handle — kept owned here and
        // bound by reference through IoBinding each step.
        let mut past: HashMap<String, DynValue> = HashMap::new();
        let mut rng = Rng(opts.seed ^ 0x2545F4914F6CDD1D);

        for step in 0..opts.frames {
            // Delay pattern: codebook k is shifted right by k steps; at step
            // `step`, row k sees codes[k][step - k] or PAD while behind.
            // Step 0 is the start-token column (all PAD).
            let mut col = Vec::with_capacity(rows);
            for _b in 0..batch {
                for k in 0..NUM_CODEBOOKS {
                    let idx = step as isize - 1 - k as isize;
                    col.push(if idx < 0 {
                        PAD_ID
                    } else {
                        codes[k].get(idx as usize).copied().unwrap_or(PAD_ID)
                    });
                }
            }
            let ids_v: DynValue = Tensor::<i64>::from_array(([rows, 1usize], col))
                .map_err(ort_err)?
                .into_dyn();

            let (logits, new_past) = {
                let past_model = step > 0;
                let dec = if past_model { &mut decn } else { &mut dec0 };
                let mut binding = dec.create_binding().map_err(ort_err)?;
                for (name, _ty) in inlets(dec) {
                    match name.as_str() {
                        "input_ids" => binding.bind_input(&name, &ids_v).map_err(ort_err)?,
                        "encoder_hidden_states" => {
                            binding.bind_input(&name, &ehs_v).map_err(ort_err)?
                        }
                        "encoder_attention_mask" => {
                            binding.bind_input(&name, &eam_v).map_err(ort_err)?
                        }
                        n if n.starts_with("past_key_values.") => {
                            // Only the with-past graph declares these; all
                            // entries exist after the prefill step.
                            let v = past.get(n).ok_or_else(|| {
                                format!("decoder past input {n} missing after prefill")
                            })?;
                            binding.bind_input(&name, v).map_err(ort_err)?;
                        }
                        other => return Err(format!("decoder wants unknown input {other}")),
                    }
                }
                bind_outputs(dec, &mut binding)?;
                let mut outs = dec.run_binding(&binding).map_err(ort_err)?;
                let logits_v = outs.remove("logits").ok_or("decoder returned no logits")?;
                let (_ls, logits) = logits_v.try_extract_tensor::<f32>().map_err(ort_err)?;
                let logits = logits.to_vec();
                let mut new_past = Vec::new();
                for key in outs.keys().map(|k| k.to_string()).collect::<Vec<_>>() {
                    if key.starts_with("present.") {
                        if let Some(v) = outs.remove(&key) {
                            new_past.push((past_name(&key), v));
                        }
                    }
                }
                (logits, new_past)
            };
            for (k, v) in new_past {
                past.insert(k, v);
            }

            // logits [rows, 1, vocab]: combine cond/uncond per codebook.
            let vocab = logits.len() / rows;
            for k in 0..NUM_CODEBOOKS {
                let cond = &logits[k * vocab..(k + 1) * vocab];
                let row = if use_cfg {
                    let uncond =
                        &logits[(NUM_CODEBOOKS + k) * vocab..(NUM_CODEBOOKS + k + 1) * vocab];
                    uncond
                        .iter()
                        .zip(cond)
                        .map(|(&u, &c)| u + opts.guidance * (c - u))
                        .collect::<Vec<f32>>()
                } else {
                    cond.to_vec()
                };
                codes[k].push(sample(&row, opts.temperature, opts.top_k, &mut rng));
            }
        }

        // Trim the delay overhang: the last NUM_CODEBOOKS-1 frames carry pads
        // for shifted codebooks; real frames are the first `frames` samples.
        let t = opts.frames;
        let mut flat = Vec::with_capacity(NUM_CODEBOOKS * t);
        for k in 0..NUM_CODEBOOKS {
            flat.extend_from_slice(&codes[k][..t]);
        }

        // EnCodec decode: input [1, K, T] int64 → audio [1, 1, T*320] f32.
        // This export declares audio_codes rank 4: [batch, channel, codebooks, T].
        let codes_v: DynValue = Tensor::<i64>::from_array(([1usize, 1, NUM_CODEBOOKS, t], flat))
            .map_err(ort_err)?
            .into_dyn();
        let mut binding = codec.create_binding().map_err(ort_err)?;
        for (name, _ty) in inlets(&codec) {
            binding
                .bind_input(name.as_str(), &codes_v)
                .map_err(ort_err)?;
        }
        bind_outputs(&codec, &mut binding)?;
        let outs = codec.run_binding(&binding).map_err(ort_err)?;
        let audio_v = outs
            .get("audio_values")
            .or_else(|| outs.keys().next().and_then(|n| outs.get(n)));
        let audio_v = audio_v.ok_or("encodec returned no audio output")?;
        let (_s, pcm) = audio_v.try_extract_tensor::<f32>().map_err(ort_err)?;
        let i16s: Vec<i16> = pcm
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();
        Ok(crate::qts::wav::encode_pcm16(&i16s, SAMPLE_RATE))
    }
}
