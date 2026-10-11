use rightkit_ort::ort;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rightkit_ort::ort::session::Session;
use rightkit_ort::{
    configure_runtime, init_environment, CandidateSource, EnvironmentOptions, GlobalPool,
    RuntimeCandidate, SessionOptions,
};
use thiserror::Error;
use tokenizers::{EncodeInput, Tokenizer};

pub const EMBEDDINGGEMMA_MODEL_ID: &str = "embeddinggemma-300m-q4";
/// EmbeddingGemma 2 text-only Q4 export (b64, accuracy level 4). Selected when
/// the model directory's top-level `config.json` `model_type` is
/// `embedding_gemma2`.
pub const EMBEDDINGGEMMA2_MODEL_ID: &str = "embeddinggemma2-q4";
/// Literal special tokens that encode to EmbeddingGemma 2's image, video and
/// audio sentinel ids; the text-only graph is exact only without them.
const GEMMA2_MODALITY_SENTINELS: [&str; 3] = ["<|image|>", "<|video|>", "<|audio|>"];
pub const EMBEDDINGGEMMA_NATIVE_DIM: usize = 768;
pub const DEFAULT_TRUNCATE_DIM: usize = 256;
pub const MAX_SEQUENCE_TOKENS: usize = 2048;
/// Default rows per ONNX call for `embed_batch` / `embed_query_batch`.
pub const DEFAULT_MAX_BATCH: usize = 16;
/// Upper bound on padded tokens (`rows * padded_len`) in one ONNX call. Keeps
/// attention memory bounded when long texts share a chunk; a single row always
/// fits regardless.
const MAX_PADDED_TOKENS_PER_CALL: usize = 16_384;
/// Fixed sequence-length buckets used on accelerators, which recompile per
/// input shape (DirectML measured 345-623 ms dynamic vs ~26 ms padded).
const ACCELERATOR_BUCKETS: [usize; 6] = [64, 128, 256, 512, 1024, 2048];
const DOCUMENT_PROMPT_PREFIX: &str = "title: none | text: ";
const QUERY_PROMPT_PREFIX: &str = "task: search result | query: ";
/// Bump when the embedding math changes in a way the other fingerprint inputs
/// do not capture (pooling, prompt assembly, truncation order).
const PIPELINE_SCHEMA: &str = "rightkit-embed/pipeline/1";
const POOLING_DESCRIPTION: &str = "model sentence_embedding [B,D] or masked mean over [B,T,D]";
const NORMALIZATION_DESCRIPTION: &str = "l2 native, matryoshka prefix truncate, l2";
const PROBE_PROMPT: &str = "title: none | text: rightkit embed provider probe";
/// An accelerator is admitted only if every parity prompt's native-dimension
/// vector has at least this cosine similarity to the CPU provider's vector for
/// the same prompt. Two correct runs of the same Q4 graph agree at about 0.999;
/// a DirectML run of EmbeddingGemma 2 Q4 measured 0.58-0.72.
pub const ACCELERATOR_PARITY_MIN_COSINE: f32 = 0.995;
/// Short query-style parity prompt (about 20 tokens with its prefix).
const PARITY_QUERY: &str = "how are invoices matched to the ledger";
/// Medium document-style parity prompt (roughly 90 tokens with its prefix).
const PARITY_DOCUMENT: &str = "The nightly billing job reads every invoice issued during the day, \
looks up the matching ledger account, and compares the invoiced total with the posted amount. \
Differences below one cent are rounded away. Larger differences are written to an exceptions \
table together with the customer, the invoice number and the date, and the finance team reviews \
that table each morning before the payment run starts.";
/// Sections in the long parity prompt (several hundred tokens).
const PARITY_LONG_SECTIONS: usize = 20;

#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("model directory is missing: {0}")]
    MissingModelDirectory(PathBuf),
    #[error("model artifact is missing: {0}")]
    MissingArtifact(PathBuf),
    #[error("invalid model configuration: {0}")]
    InvalidConfig(String),
    #[error("tokenizer: {0}")]
    Tokenizer(String),
    #[error("ONNX Runtime: {0}")]
    Runtime(String),
    #[error("model output: {0}")]
    Output(String),
    /// An explicitly requested accelerator did not reproduce the CPU
    /// provider's vectors at load (or could not be compared), so it was not
    /// admitted. `lowest_cosine` is `None` when the failure is not a low
    /// cosine (non-finite or zero output, wrong size); `detail` names the
    /// prompt length. `Auto` never returns this: it falls back to CPU with the
    /// same text in `fallback_reason`.
    #[error("{provider} failed the vector parity check against cpu: {detail}")]
    AcceleratorParity {
        provider: &'static str,
        lowest_cosine: Option<f32>,
        floor: f32,
        detail: String,
    },
}

/// Result of the load-time vector parity check an accelerator passed; see
/// [`Embedder::accelerator_parity`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AcceleratorParity {
    /// Lowest cosine to the CPU provider's vector over all compared pairs.
    pub lowest_cosine: f32,
    /// Token count of the prompt that produced it.
    pub at_tokens: usize,
    /// Pairs compared.
    pub pairs: usize,
}

/// Where inference runs.
///
/// `Cpu` is the default. `Auto` registers the platform accelerator (DirectML
/// on Windows, CoreML on macOS) and falls back to CPU, with the reason
/// recorded, when registration, model load, the probe inference or the vector
/// parity check fails. `DirectMl` / `CoreMl` request that accelerator
/// explicitly and fail loudly instead of falling back: a failed parity check
/// is [`EmbedError::AcceleratorParity`].
///
/// An accelerator is used only if it reproduces the CPU provider's vectors.
/// Whenever the session was built on anything other than CPU, `Embedder::load`
/// builds a CPU session for the same model, embeds fixed short, medium and
/// long prompts (alone and as one batch) through both sessions with the
/// crate's real inference path, and requires every pair to have cosine
/// similarity of at least [`ACCELERATOR_PARITY_MIN_COSINE`] on the full
/// native-dimension vector, with finite non-zero output. On success the CPU
/// session is dropped before `load` returns, so steady-state memory is
/// unchanged; the check costs one extra CPU session build and a few inferences
/// at load. `Cpu` does none of this. The check cannot be turned off.
///
/// Measured: DirectML (ONNX Runtime 1.24.4) returns wrong EmbeddingGemma 2 Q4
/// vectors (cosine to CPU 0.58-0.72) that are finite, non-zero and faster
/// than CPU, so a probe or speed check alone would accept them. CoreML is
/// unmeasured; it must pass the same check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionProvider {
    #[default]
    Cpu,
    Auto,
    DirectMl,
    CoreMl,
}

impl From<rightkit_ort::ExecutionProvider> for ExecutionProvider {
    fn from(value: rightkit_ort::ExecutionProvider) -> Self {
        match value {
            rightkit_ort::ExecutionProvider::Cpu => Self::Cpu,
            rightkit_ort::ExecutionProvider::DirectMl => Self::DirectMl,
            rightkit_ort::ExecutionProvider::CoreMl => Self::CoreMl,
        }
    }
}

impl ExecutionProvider {
    fn requested(self) -> (rightkit_ort::ExecutionProvider, bool) {
        match self {
            Self::Cpu => (rightkit_ort::ExecutionProvider::Cpu, false),
            Self::Auto => (
                rightkit_ort::ExecutionProvider::platform_accelerator(),
                true,
            ),
            Self::DirectMl => (rightkit_ort::ExecutionProvider::DirectMl, false),
            Self::CoreMl => (rightkit_ort::ExecutionProvider::CoreMl, false),
        }
    }
}

fn provider_name(provider: rightkit_ort::ExecutionProvider) -> &'static str {
    match provider {
        rightkit_ort::ExecutionProvider::Cpu => "cpu",
        rightkit_ort::ExecutionProvider::DirectMl => "directml",
        rightkit_ort::ExecutionProvider::CoreMl => "coreml",
    }
}

#[derive(Debug, Clone)]
pub struct EmbedderConfig {
    pub model_dir: PathBuf,
    pub runtime_path: PathBuf,
    pub provider: ExecutionProvider,
    pub truncate_dim: usize,
    pub max_tokens: usize,
    pub max_batch: usize,
    pub intra_threads: usize,
}

impl EmbedderConfig {
    pub fn new(model_dir: impl Into<PathBuf>, runtime_path: impl Into<PathBuf>) -> Self {
        Self {
            model_dir: model_dir.into(),
            runtime_path: runtime_path.into(),
            provider: ExecutionProvider::Cpu,
            truncate_dim: DEFAULT_TRUNCATE_DIM,
            max_tokens: MAX_SEQUENCE_TOKENS,
            max_batch: DEFAULT_MAX_BATCH,
            intra_threads: 1,
        }
    }

    pub fn with_provider(mut self, provider: impl Into<ExecutionProvider>) -> Self {
        self.provider = provider.into();
        self
    }

    pub fn with_truncate_dim(mut self, dim: usize) -> Self {
        self.truncate_dim = dim;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: usize) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Rows per ONNX call for the batch methods (default 16, minimum 1).
    pub fn with_max_batch(mut self, max_batch: usize) -> Self {
        self.max_batch = max_batch;
        self
    }

    /// Intra-op threads for the session (default 1, minimum 1).
    pub fn with_intra_threads(mut self, threads: usize) -> Self {
        self.intra_threads = threads;
        self
    }
}

pub struct Embedder {
    tokenizer: Tokenizer,
    session: Mutex<Session>,
    model_dir: PathBuf,
    native_dim: usize,
    truncate_dim: usize,
    max_tokens: usize,
    max_batch: usize,
    model_id: String,
    gemma2: bool,
    pad_id: i64,
    takes_token_type_ids: bool,
    provider_used: &'static str,
    fallback_reason: Option<String>,
    parity: Option<AcceleratorParity>,
    pipeline_id: OnceLock<Result<String, String>>,
}

struct BuiltEmbedSession {
    session: Session,
    provider: rightkit_ort::ExecutionProvider,
    fallback: Option<String>,
}

fn build_session(
    config: &EmbedderConfig,
    provider: rightkit_ort::ExecutionProvider,
    cpu_fallback: bool,
) -> Result<BuiltEmbedSession, EmbedError> {
    let built = SessionOptions::new(provider)
        .with_threads(Some(config.intra_threads))
        .with_cpu_fallback(cpu_fallback)
        .build_from_file_reported(&config.model_dir.join("model_q4.onnx"))
        .map_err(|e| EmbedError::Runtime(e.to_string()))?;
    Ok(BuiltEmbedSession {
        session: built.session,
        provider: built.provider,
        fallback: built.fallback,
    })
}

fn takes_token_type_ids(session: &Session) -> bool {
    session
        .inputs()
        .iter()
        .any(|input| input.name() == "token_type_ids")
}

/// Tokenizer with padding owned by this crate (masked, to the chunk maximum),
/// never by the tokenizer file's own padding strategy, and the pad id.
fn load_tokenizer(model_dir: &Path) -> Result<(Tokenizer, i64), EmbedError> {
    let mut tokenizer = Tokenizer::from_file(model_dir.join("tokenizer.json"))
        .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
    let pad_id = tokenizer
        .get_padding()
        .map(|padding| i64::from(padding.pad_id))
        .or_else(|| tokenizer.token_to_id("<pad>").map(i64::from))
        .unwrap_or(0);
    tokenizer.with_padding(None);
    Ok((tokenizer, pad_id))
}

impl Embedder {
    pub fn load(config: EmbedderConfig) -> Result<Self, EmbedError> {
        validate_config(&config)?;
        let gemma2 = bundle_is_gemma2(&config.model_dir)?;
        let (tokenizer, pad_id) = load_tokenizer(&config.model_dir)?;
        let selection = configure_runtime([RuntimeCandidate::new(
            config.runtime_path.clone(),
            CandidateSource::Explicit,
        )])
        .map_err(|e| EmbedError::Runtime(e.to_string()))?;
        init_environment(
            &selection,
            &EnvironmentOptions {
                global_pool: Some(GlobalPool::from_budget()),
            },
        )
        .map_err(EmbedError::Runtime)?;
        let (requested, auto) = config.provider.requested();
        let built = build_session(&config, requested, auto)?;
        let accelerator = built.provider;
        let mut embedder = Self::from_session(&config, tokenizer, pad_id, gemma2, built);
        if auto && accelerator != rightkit_ort::ExecutionProvider::Cpu {
            if let Err(reason) = embedder.probe() {
                let cpu = build_session(&config, rightkit_ort::ExecutionProvider::Cpu, false)?;
                embedder.takes_token_type_ids = takes_token_type_ids(&cpu.session);
                embedder.session = Mutex::new(cpu.session);
                embedder.provider_used = provider_name(cpu.provider);
                embedder.fallback_reason = Some(format!(
                    "{} -> cpu: probe inference failed: {reason}",
                    provider_name(accelerator)
                ));
            }
        }
        if embedder.provider_used != "cpu" {
            return embedder.admit_accelerator(&config, auto);
        }
        Ok(embedder)
    }

    fn from_session(
        config: &EmbedderConfig,
        tokenizer: Tokenizer,
        pad_id: i64,
        gemma2: bool,
        built: BuiltEmbedSession,
    ) -> Self {
        Self {
            tokenizer,
            takes_token_type_ids: takes_token_type_ids(&built.session),
            session: Mutex::new(built.session),
            model_dir: config.model_dir.clone(),
            native_dim: EMBEDDINGGEMMA_NATIVE_DIM,
            truncate_dim: config.truncate_dim,
            max_tokens: config.max_tokens,
            max_batch: config.max_batch,
            model_id: if gemma2 {
                EMBEDDINGGEMMA2_MODEL_ID
            } else {
                EMBEDDINGGEMMA_MODEL_ID
            }
            .to_owned(),
            gemma2,
            pad_id,
            provider_used: provider_name(built.provider),
            fallback_reason: built.fallback,
            parity: None,
            pipeline_id: OnceLock::new(),
        }
    }

    /// Vector parity gate for an embedder built on an accelerator (`self`).
    /// Builds a CPU embedder for the same model and embeds the parity prompts
    /// through both with the real inference path. Passing keeps `self` and
    /// drops the CPU embedder; failing returns the CPU embedder with the
    /// reason under `Auto`, and [`EmbedError::AcceleratorParity`] otherwise.
    fn admit_accelerator(
        mut self,
        config: &EmbedderConfig,
        auto: bool,
    ) -> Result<Self, EmbedError> {
        let provider = self.provider_used;
        let (tokenizer, pad_id) = load_tokenizer(&config.model_dir)?;
        let cpu_built = build_session(config, rightkit_ort::ExecutionProvider::Cpu, false)?;
        let mut cpu = Self::from_session(config, tokenizer, pad_id, self.gemma2, cpu_built);
        let prompts = cpu.parity_prompts();
        let tokens: Vec<usize> = cpu
            .encode_prompts(&prompts)?
            .iter()
            .map(|row| row.ids.len())
            .collect();
        let reference = cpu.parity_vectors(&prompts)?;
        let (lowest_cosine, detail) = match self.parity_vectors(&prompts) {
            Ok(candidate) => match compare_parity(
                &reference,
                &candidate,
                self.native_dim,
                ACCELERATOR_PARITY_MIN_COSINE,
            ) {
                Ok(lowest) => {
                    self.parity = Some(AcceleratorParity {
                        lowest_cosine: lowest.cosine,
                        at_tokens: tokens[lowest.index % tokens.len()],
                        pairs: reference.len(),
                    });
                    return Ok(self);
                }
                Err(miss) => parity_miss_detail(&miss, &tokens, ACCELERATOR_PARITY_MIN_COSINE),
            },
            Err(error) if auto => (None, format!("inference error: {error}")),
            Err(error) => return Err(error),
        };
        if auto {
            cpu.fallback_reason =
                Some(format!("{provider} -> cpu: vector parity failed: {detail}"));
            return Ok(cpu);
        }
        Err(EmbedError::AcceleratorParity {
            provider,
            lowest_cosine,
            floor: ACCELERATOR_PARITY_MIN_COSINE,
            detail,
        })
    }

    /// Fixed parity prompts of clearly different token lengths: a short
    /// query, a medium document and a long document, with this crate's prefixes.
    fn parity_prompts(&self) -> Vec<String> {
        vec![
            self.query_prompt(PARITY_QUERY),
            self.document_prompt(PARITY_DOCUMENT),
            self.document_prompt(&parity_long_document()),
        ]
    }

    /// Native-dimension normalized vectors for `prompts`: first each prompt
    /// alone (its own accelerator shape bucket), then all together as one
    /// batch. `2 * prompts.len()` vectors.
    fn parity_vectors(&self, prompts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut vectors = Vec::with_capacity(prompts.len() * 2);
        for prompt in prompts {
            vectors.extend(self.embed_prompts_native(vec![prompt.clone()])?);
        }
        vectors.extend(self.embed_prompts_native(prompts.to_vec())?);
        Ok(vectors)
    }

    /// One real inference through the live session; any error, shape mismatch,
    /// non-finite or all-zero output rejects the accelerator.
    fn probe(&self) -> Result<(), String> {
        let rows = self
            .encode_prompts(&[PROBE_PROMPT.to_owned()])
            .map_err(|e| e.to_string())?;
        let chunk = [&rows[0]];
        let native = self
            .run_chunk(&chunk, true)
            .map_err(|e| e.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "probe returned no row".to_owned())?;
        probe_output_ok(&native, self.native_dim)
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn native_dim(&self) -> usize {
        self.native_dim
    }

    pub fn dimension(&self) -> usize {
        self.truncate_dim
    }

    /// Provider serving inference: `"cpu"`, `"directml"` or `"coreml"`.
    pub fn provider_used(&self) -> &str {
        self.provider_used
    }

    /// Why an accelerator was replaced by CPU, when it was. A vector parity
    /// failure reads like `directml -> cpu: vector parity failed: cosine 0.579
    /// at ~20 tokens, floor 0.995`.
    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback_reason.as_deref()
    }

    /// The load-time vector parity result of the accelerator in use: lowest
    /// cosine to the CPU provider's vectors, the prompt length it occurred at
    /// and the pairs compared. `None` when the provider is CPU (no parity
    /// work is done) and when an accelerator was replaced by CPU (the failure
    /// is in [`Self::fallback_reason`]). Always at least
    /// [`ACCELERATOR_PARITY_MIN_COSINE`] when `Some`.
    pub fn accelerator_parity(&self) -> Option<AcceleratorParity> {
        self.parity
    }

    /// Stable fingerprint of everything that changes the produced vectors, so
    /// callers can detect when stored vectors need re-embedding. Fed by:
    ///
    /// - the pipeline schema version of this crate's embedding math;
    /// - model id and native dimension;
    /// - SHA-256 of `model_q4.onnx`, `model_q4.onnx_data`, `tokenizer.json`
    ///   and `config.json` (absent file recorded as absent);
    /// - the document and query prompt templates;
    /// - pooling and normalization descriptions;
    /// - Matryoshka truncate dimension and max token length;
    /// - whether Gemma 2 modality sentinels are scrubbed.
    ///
    /// Not fed by provider, thread count or batch size: those change only
    /// floating-point noise (see `max_batch` tolerance in the README), not
    /// the pipeline. The accelerator parity check is not an input either: it
    /// only decides which provider serves the same math, and an admitted
    /// accelerator reproduces the CPU vectors to the parity floor. Computed
    /// once on first call (hashes the model files) and cached; format
    /// `rkpipe1-<32 hex>`.
    pub fn pipeline_id(&self) -> String {
        match self.pipeline_id.get_or_init(|| {
            let mut files = Vec::new();
            for name in [
                "model_q4.onnx",
                "model_q4.onnx_data",
                "tokenizer.json",
                "config.json",
            ] {
                files.push((name, hash_file(&self.model_dir.join(name))?));
            }
            Ok(pipeline_fingerprint(&PipelineParts {
                model_id: &self.model_id,
                native_dim: self.native_dim,
                truncate_dim: self.truncate_dim,
                max_tokens: self.max_tokens,
                scrubs_sentinels: self.gemma2,
                files: &files,
            }))
        }) {
            Ok(id) => id.clone(),
            // Unreadable artifact: an id that can never equal a good one.
            Err(error) => format!("rkpipe1-unavailable-{}", sanitize(error)),
        }
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        self.embed_prompts(vec![self.document_prompt(text)])?
            .pop()
            .ok_or_else(|| EmbedError::Output("no embedding produced".into()))
    }

    pub fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        self.embed_prompts(vec![self.query_prompt(text)])?
            .pop()
            .ok_or_else(|| EmbedError::Output("no embedding produced".into()))
    }

    /// Embeds documents in input order. Texts are tokenized together, ordered
    /// by length so chunks pad little, run `max_batch` rows per ONNX call with
    /// the attention mask honoured, then returned in input order.
    pub fn embed_batch<'a, I>(&self, texts: I) -> Result<Vec<Vec<f32>>, EmbedError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        self.embed_prompts(texts.into_iter().map(|t| self.document_prompt(t)).collect())
    }

    /// Query-prompt counterpart of [`Self::embed_batch`].
    pub fn embed_query_batch<'a, I>(&self, texts: I) -> Result<Vec<Vec<f32>>, EmbedError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        self.embed_prompts(texts.into_iter().map(|t| self.query_prompt(t)).collect())
    }

    fn document_prompt(&self, text: &str) -> String {
        format!("{DOCUMENT_PROMPT_PREFIX}{}", self.model_text(text))
    }

    fn query_prompt(&self, text: &str) -> String {
        format!("{QUERY_PROMPT_PREFIX}{}", self.model_text(text))
    }

    /// EmbeddingGemma 2's text-only graph must never see modality sentinels;
    /// their literal spellings are defanged before tokenization.
    fn model_text<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        if !self.gemma2 || !GEMMA2_MODALITY_SENTINELS.iter().any(|s| text.contains(s)) {
            return std::borrow::Cow::Borrowed(text);
        }
        let mut owned = text.to_owned();
        for sentinel in GEMMA2_MODALITY_SENTINELS {
            owned = owned.replace(sentinel, &sentinel.replace('|', "\u{2502}"));
        }
        std::borrow::Cow::Owned(owned)
    }

    fn encode_prompts(&self, prompts: &[String]) -> Result<Vec<TokenRow>, EmbedError> {
        let inputs: Vec<EncodeInput> = prompts
            .iter()
            .map(|prompt| EncodeInput::Single(prompt.as_str().into()))
            .collect();
        let encodings = self
            .tokenizer
            .encode_batch(inputs, true)
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        encodings
            .iter()
            .map(|encoding| {
                let ids: Vec<i64> = encoding
                    .get_ids()
                    .iter()
                    .take(self.max_tokens)
                    .map(|id| i64::from(*id))
                    .collect();
                if ids.is_empty() {
                    return Err(EmbedError::Tokenizer("encoding returned no tokens".into()));
                }
                let mask: Vec<i64> = encoding
                    .get_attention_mask()
                    .iter()
                    .take(ids.len())
                    .map(|value| i64::from(*value))
                    .collect();
                Ok(TokenRow { ids, mask })
            })
            .collect()
    }

    fn embed_prompts(&self, prompts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(self
            .embed_prompts_native(prompts)?
            .into_iter()
            .map(|native| truncate_and_normalize(&native, self.truncate_dim))
            .collect())
    }

    /// Normalized native-dimension vector per prompt, in input order, before
    /// Matryoshka truncation.
    fn embed_prompts_native(&self, prompts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        if prompts.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self.encode_prompts(&prompts)?;
        let lengths: Vec<usize> = rows.iter().map(|row| row.ids.len()).collect();
        let order = length_order(&lengths);
        let sorted_lengths: Vec<usize> = order.iter().map(|&i| lengths[i]).collect();
        let accelerated = self.provider_used != "cpu";
        let mut out: Vec<Option<Vec<f32>>> = vec![None; rows.len()];
        for range in plan_chunks(
            &sorted_lengths,
            self.max_batch.max(1),
            MAX_PADDED_TOKENS_PER_CALL,
        ) {
            let chunk: Vec<&TokenRow> = order[range.clone()].iter().map(|&i| &rows[i]).collect();
            let natives = self.run_chunk(&chunk, accelerated)?;
            for (&index, native) in order[range].iter().zip(natives) {
                out[index] = Some(native);
            }
        }
        out.into_iter()
            .map(|row| row.ok_or_else(|| EmbedError::Output("missing embedding row".into())))
            .collect()
    }

    /// One ONNX call over `chunk`; returns the normalized native-dimension
    /// vector per row, in chunk order.
    fn run_chunk(
        &self,
        chunk: &[&TokenRow],
        accelerated: bool,
    ) -> Result<Vec<Vec<f32>>, EmbedError> {
        let longest = chunk.iter().map(|row| row.ids.len()).max().unwrap_or(1);
        let (batch_rows, padded_len) = padded_shape(chunk.len(), longest, accelerated);
        let padded = pad_batch(chunk, self.pad_id, padded_len, batch_rows);
        let (rows, len) = (padded.rows, padded.len);
        let tensor = |data: Vec<i64>| {
            ort::value::Tensor::from_array(([rows, len], data))
                .map_err(|e| EmbedError::Runtime(e.to_string()))
        };
        let mut inputs = ort::inputs![
            "input_ids" => tensor(padded.ids)?,
            "attention_mask" => tensor(padded.mask)?
        ];
        if self.takes_token_type_ids {
            inputs.push(("token_type_ids".into(), tensor(vec![0; rows * len])?.into()));
        }
        let mut session = self
            .session
            .lock()
            .map_err(|_| EmbedError::Runtime("model mutex poisoned".into()))?;
        let outputs = session
            .run(inputs)
            .map_err(|e| EmbedError::Runtime(e.to_string()))?;
        let output = outputs
            .get("sentence_embedding")
            .ok_or_else(|| EmbedError::Output("missing sentence_embedding output".into()))?;
        let (shape, values) = output
            .try_extract_tensor::<f32>()
            .map_err(|e| EmbedError::Output(e.to_string()))?;
        let masks: Vec<&[i64]> = chunk.iter().map(|row| row.mask.as_slice()).collect();
        pool_batch(shape.as_ref(), values, &masks, self.native_dim)
    }
}

/// One tokenized, truncated text.
struct TokenRow {
    ids: Vec<i64>,
    mask: Vec<i64>,
}

/// Row-major `[rows, len]` model inputs.
#[derive(Debug, PartialEq, Eq)]
struct PaddedBatch {
    rows: usize,
    len: usize,
    ids: Vec<i64>,
    mask: Vec<i64>,
}

/// Stable ascending-length order (ties keep input order) so chunks of similar
/// length share a call and pad little.
fn length_order(lengths: &[usize]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..lengths.len()).collect();
    order.sort_by_key(|&i| lengths[i]);
    order
}

/// Splits ascending `sorted_lengths` into contiguous ranges of at most
/// `max_rows` rows whose padded size `rows * longest` stays within
/// `token_budget`; a lone row is always allowed.
fn plan_chunks(
    sorted_lengths: &[usize],
    max_rows: usize,
    token_budget: usize,
) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < sorted_lengths.len() {
        let mut end = start + 1;
        while end < sorted_lengths.len()
            && end - start < max_rows
            && (end - start + 1) * sorted_lengths[end] <= token_budget
        {
            end += 1;
        }
        ranges.push(start..end);
        start = end;
    }
    ranges
}

/// `(rows, len)` of the tensor sent to the model. CPU uses the exact chunk
/// maximum. Accelerators recompile per shape, so length rounds up to a bucket
/// and rows to a power of two (extra rows repeat the last row and are dropped).
fn padded_shape(rows: usize, longest: usize, accelerated: bool) -> (usize, usize) {
    if !accelerated {
        return (rows, longest);
    }
    let len = ACCELERATOR_BUCKETS
        .iter()
        .copied()
        .find(|bucket| *bucket >= longest)
        .unwrap_or(longest);
    (rows.next_power_of_two(), len)
}

/// Right-pads every row to `len` with `pad_id` ids and zero mask; rows beyond
/// `chunk.len()` (up to `rows`) repeat the last row.
fn pad_batch(chunk: &[&TokenRow], pad_id: i64, len: usize, rows: usize) -> PaddedBatch {
    let mut ids = vec![pad_id; rows * len];
    let mut mask = vec![0i64; rows * len];
    for row in 0..rows {
        let source = chunk[row.min(chunk.len() - 1)];
        let used = source.ids.len().min(len);
        ids[row * len..row * len + used].copy_from_slice(&source.ids[..used]);
        mask[row * len..row * len + used].copy_from_slice(&source.mask[..used]);
    }
    PaddedBatch {
        rows,
        len,
        ids,
        mask,
    }
}

/// Turns a model output into one L2-normalized native vector per real row
/// (`masks.len()` rows; extra padding rows are ignored). Accepts the model's
/// pooled `[B, D]` output, or token states `[B, T, D]` which are mean-pooled
/// per row over the attention mask (positions beyond a row's mask are padding).
fn pool_batch(
    shape: &[i64],
    values: &[f32],
    masks: &[&[i64]],
    native_dim: usize,
) -> Result<Vec<Vec<f32>>, EmbedError> {
    let real = masks.len();
    if shape.len() == 2 && shape[1] == native_dim as i64 {
        let batch = shape[0] as usize;
        if batch >= real && values.len() == batch * native_dim {
            return Ok((0..real)
                .map(|row| normalize(values[row * native_dim..(row + 1) * native_dim].to_vec()))
                .collect());
        }
    }
    if shape.len() == 3 && shape[2] == native_dim as i64 && shape[0] > 0 && shape[1] > 0 {
        let (batch, tokens) = (shape[0] as usize, shape[1] as usize);
        if batch >= real && values.len() == batch * tokens * native_dim {
            let mut pooled_rows = Vec::with_capacity(real);
            for (row, mask) in masks.iter().enumerate() {
                let mut pooled = vec![0.0f32; native_dim];
                let mut count = 0.0f32;
                for token in 0..tokens {
                    if mask.get(token).copied().unwrap_or(0) == 0 {
                        continue;
                    }
                    count += 1.0;
                    let base = (row * tokens + token) * native_dim;
                    for (dim, value) in pooled.iter_mut().enumerate() {
                        *value += values[base + dim];
                    }
                }
                if count == 0.0 {
                    return Err(EmbedError::Output(
                        "attention mask contains no tokens".into(),
                    ));
                }
                for value in &mut pooled {
                    *value /= count;
                }
                pooled_rows.push(normalize(pooled));
            }
            return Ok(pooled_rows);
        }
    }
    Err(EmbedError::Output(format!(
        "expected [B,{native_dim}] or [B,tokens,{native_dim}] with B >= {real}, got shape {shape:?}"
    )))
}

fn probe_output_ok(native: &[f32], native_dim: usize) -> Result<(), String> {
    if native.len() != native_dim {
        return Err(format!(
            "expected {native_dim} values, got {}",
            native.len()
        ));
    }
    if native.iter().any(|value| !value.is_finite()) {
        return Err("non-finite values".into());
    }
    if native.iter().map(|value| value * value).sum::<f32>() <= f32::EPSILON {
        return Err("all-zero output".into());
    }
    Ok(())
}

/// Deterministic long parity document: `PARITY_LONG_SECTIONS` numbered
/// sections, several hundred tokens.
fn parity_long_document() -> String {
    (1..=PARITY_LONG_SECTIONS)
        .map(|n| {
            format!(
                "Section {n}: the nightly billing job reconciles every invoice against the \
                 ledger, flags mismatched totals for account {}, and sends the exceptions to \
                 the finance team for review before payment run {n}.",
                1000 + n * 7
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lowest cosine over the compared pairs and the pair it came from.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ParityLowest {
    index: usize,
    cosine: f32,
}

/// Why two vector sets are not parity-equal.
#[derive(Debug, Clone, PartialEq)]
enum ParityMiss {
    /// A pair could not be compared: wrong count or size, non-finite or zero
    /// output on either side.
    Invalid { index: usize, reason: String },
    /// The lowest pair is under the floor.
    Below(ParityLowest),
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    (dot / (na.sqrt() * nb.sqrt())) as f32
}

/// Pairwise cosine parity of `candidate` against `reference` (same prompts,
/// same order, native dimension). Every vector must have `native_dim` finite
/// values and a non-zero norm; the lowest pair must be at least `floor`.
/// Returns the lowest pair when parity holds.
fn compare_parity(
    reference: &[Vec<f32>],
    candidate: &[Vec<f32>],
    native_dim: usize,
    floor: f32,
) -> Result<ParityLowest, ParityMiss> {
    if reference.is_empty() || reference.len() != candidate.len() {
        return Err(ParityMiss::Invalid {
            index: 0,
            reason: format!(
                "expected {} vectors to compare, got {}",
                reference.len(),
                candidate.len()
            ),
        });
    }
    let mut lowest = ParityLowest {
        index: 0,
        cosine: f32::INFINITY,
    };
    for (index, (r, c)) in reference.iter().zip(candidate).enumerate() {
        probe_output_ok(r, native_dim).map_err(|reason| ParityMiss::Invalid {
            index,
            reason: format!("cpu reference: {reason}"),
        })?;
        probe_output_ok(c, native_dim).map_err(|reason| ParityMiss::Invalid {
            index,
            reason: format!("accelerator output: {reason}"),
        })?;
        let value = cosine(r, c);
        if value < lowest.cosine {
            lowest = ParityLowest {
                index,
                cosine: value,
            };
        }
    }
    if lowest.cosine >= floor {
        Ok(lowest)
    } else {
        Err(ParityMiss::Below(lowest))
    }
}

/// `(lowest cosine if any, text)` for a parity miss. `tokens` holds the token
/// count of each parity prompt; pair `i` is prompt `i % tokens.len()`, and pairs
/// past the first `tokens.len()` are the batched run.
fn parity_miss_detail(miss: &ParityMiss, tokens: &[usize], floor: f32) -> (Option<f32>, String) {
    let at = |index: usize| {
        let batched = if index >= tokens.len() {
            " (batched)"
        } else {
            ""
        };
        format!("~{} tokens{batched}", tokens[index % tokens.len()])
    };
    match miss {
        ParityMiss::Below(lowest) => (
            Some(lowest.cosine),
            format!(
                "cosine {:.3} at {}, floor {floor}",
                lowest.cosine,
                at(lowest.index)
            ),
        ),
        ParityMiss::Invalid { index, reason } => (None, format!("{reason} at {}", at(*index))),
    }
}

struct PipelineParts<'a> {
    model_id: &'a str,
    native_dim: usize,
    truncate_dim: usize,
    max_tokens: usize,
    scrubs_sentinels: bool,
    files: &'a [(&'a str, String)],
}

/// Pure fingerprint over the pipeline description; see [`Embedder::pipeline_id`].
fn pipeline_fingerprint(parts: &PipelineParts<'_>) -> String {
    let mut text = String::new();
    let mut line = |key: &str, value: &str| {
        text.push_str(key);
        text.push('=');
        text.push_str(&value.len().to_string());
        text.push(':');
        text.push_str(value);
        text.push('\n');
    };
    line("schema", PIPELINE_SCHEMA);
    line("model_id", parts.model_id);
    line("native_dim", &parts.native_dim.to_string());
    line("truncate_dim", &parts.truncate_dim.to_string());
    line("max_tokens", &parts.max_tokens.to_string());
    line("document_prompt", DOCUMENT_PROMPT_PREFIX);
    line("query_prompt", QUERY_PROMPT_PREFIX);
    line("pooling", POOLING_DESCRIPTION);
    line("normalization", NORMALIZATION_DESCRIPTION);
    line("scrubs_sentinels", &parts.scrubs_sentinels.to_string());
    for (name, hash) in parts.files {
        line(&format!("file:{name}"), hash);
    }
    // First 16 bytes of the SHA-256 = first 32 lowercase hex characters.
    let digest = rightkit_fs::digest::sha256_bytes(text.as_bytes());
    format!("rkpipe1-{}", &digest[..32])
}

fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(48)
        .collect()
}

/// SHA-256 hex of a file, or `absent` when it does not exist. Other read
/// errors fail the fingerprint rather than hashing a partial file.
fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok("absent".into()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    rightkit_fs::digest::sha256_reader(&mut file).map_err(|e| format!("{}: {e}", path.display()))
}

/// Reads only the top-level `model_type`; nested sub-configs (audio, text,
/// vision) carry their own types and are ignored. A missing config.json is the
/// original EmbeddingGemma bundle.
fn bundle_is_gemma2(model_dir: &std::path::Path) -> Result<bool, EmbedError> {
    let path = model_dir.join("config.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(false);
    };
    let config: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| EmbedError::InvalidConfig(format!("{}: {e}", path.display())))?;
    Ok(config["model_type"] == "embedding_gemma2")
}

fn validate_config(config: &EmbedderConfig) -> Result<(), EmbedError> {
    if !config.model_dir.is_dir() {
        return Err(EmbedError::MissingModelDirectory(config.model_dir.clone()));
    }
    if config.truncate_dim == 0 || config.truncate_dim > EMBEDDINGGEMMA_NATIVE_DIM {
        return Err(EmbedError::InvalidConfig(format!(
            "truncate dimension must be 1..={EMBEDDINGGEMMA_NATIVE_DIM}"
        )));
    }
    if config.max_tokens == 0 || config.max_tokens > MAX_SEQUENCE_TOKENS {
        return Err(EmbedError::InvalidConfig(format!(
            "max tokens must be 1..={MAX_SEQUENCE_TOKENS}"
        )));
    }
    if config.max_batch == 0 {
        return Err(EmbedError::InvalidConfig("max batch must be >= 1".into()));
    }
    if config.intra_threads == 0 {
        return Err(EmbedError::InvalidConfig(
            "intra threads must be >= 1".into(),
        ));
    }
    for name in ["model_q4.onnx", "model_q4.onnx_data", "tokenizer.json"] {
        let path = config.model_dir.join(name);
        if !path.is_file() {
            return Err(EmbedError::MissingArtifact(path));
        }
    }
    if !config.runtime_path.is_file() {
        return Err(EmbedError::MissingArtifact(config.runtime_path.clone()));
    }
    Ok(())
}

fn normalize(mut values: Vec<f32>) -> Vec<f32> {
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut values {
            *value /= norm;
        }
    }
    values
}

pub fn truncate_and_normalize(native: &[f32], dimension: usize) -> Vec<f32> {
    let mut values = native[..native.len().min(dimension)].to_vec();
    values.resize(dimension, 0.0);
    normalize(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ids: &[i64]) -> TokenRow {
        TokenRow {
            ids: ids.to_vec(),
            mask: vec![1; ids.len()],
        }
    }

    /// Deterministic per-token state independent of padding, like a model whose
    /// masked positions cannot influence valid ones.
    fn state(ids: &[i64], dim: usize) -> Vec<f32> {
        ids.iter()
            .enumerate()
            .flat_map(|(pos, id)| {
                (0..dim)
                    .map(move |d| ((*id as f32) * 0.37 + pos as f32 * 0.11 + d as f32 * 0.05).sin())
            })
            .collect()
    }

    #[test]
    fn pad_batch_pads_right_with_zero_mask_and_repeats_last_row() {
        let a = row(&[5, 6, 7]);
        let b = row(&[9]);
        let padded = pad_batch(&[&a, &b], 0, 4, 3);
        assert_eq!((padded.rows, padded.len), (3, 4));
        assert_eq!(padded.ids, vec![5, 6, 7, 0, 9, 0, 0, 0, 9, 0, 0, 0]);
        assert_eq!(padded.mask, vec![1, 1, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
    }

    #[test]
    fn pad_batch_uses_the_pad_id_for_masked_positions() {
        let a = row(&[5]);
        let padded = pad_batch(&[&a], 3, 3, 1);
        assert_eq!(padded.ids, vec![5, 3, 3]);
        assert_eq!(padded.mask, vec![1, 0, 0]);
    }

    #[test]
    fn padded_mean_pool_matches_unpadded_rows() {
        let dim = 8;
        let rows_ids: [&[i64]; 3] = [&[3, 4, 5, 6, 7], &[10, 11], &[20, 21, 22]];
        let rows: Vec<TokenRow> = rows_ids.iter().map(|ids| row(ids)).collect();
        let refs: Vec<&TokenRow> = rows.iter().collect();
        let padded = pad_batch(&refs, 0, 5, 3);
        // Model output for the padded batch: valid positions carry real state,
        // masked positions carry garbage that must be ignored.
        let mut values = vec![9999.0f32; padded.rows * padded.len * dim];
        for (r, ids) in rows_ids.iter().enumerate() {
            let s = state(ids, dim);
            let base = r * padded.len * dim;
            values[base..base + s.len()].copy_from_slice(&s);
        }
        let masks: Vec<&[i64]> = rows.iter().map(|r| r.mask.as_slice()).collect();
        let batched =
            pool_batch(&[3, padded.len as i64, dim as i64], &values, &masks, dim).unwrap();
        for (r, ids) in rows_ids.iter().enumerate() {
            let single = pool_batch(
                &[1, ids.len() as i64, dim as i64],
                &state(ids, dim),
                &[rows[r].mask.as_slice()],
                dim,
            )
            .unwrap();
            let diff = batched[r]
                .iter()
                .zip(&single[0])
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(diff <= 1e-6, "row {r} differs by {diff}");
            let norm: f32 = batched[r].iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() <= 1e-5);
        }
    }

    #[test]
    fn pooled_output_rows_are_normalized_and_extra_rows_dropped() {
        let dim = 4;
        let values = vec![3.0, 4.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 1.0, 1.0, 1.0, 1.0];
        let m = [1i64];
        let out = pool_batch(&[3, 4], &values, &[&m, &m], dim).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], vec![0.6, 0.8, 0.0, 0.0]);
        assert_eq!(out[1], vec![0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn pool_rejects_bad_shapes_and_empty_masks() {
        let m = [0i64, 0];
        assert!(pool_batch(&[1, 2, 2], &[1.0; 4], &[&m], 2).is_err());
        assert!(pool_batch(&[1, 3], &[1.0; 3], &[&[1]], 2).is_err());
        assert!(pool_batch(&[1, 2], &[1.0; 2], &[&[1], &[1]], 2).is_err());
    }

    #[test]
    fn chunks_respect_row_cap_token_budget_and_cover_everything() {
        let lengths = [1, 2, 3, 4, 5, 6, 7];
        assert_eq!(plan_chunks(&lengths, 3, 1000), vec![0..3, 3..6, 6..7]);
        // budget: 2 rows * 3 = 6 ok, 3 rows * 4 = 12 > 10 stops the chunk.
        assert_eq!(plan_chunks(&[1, 3, 4, 4], 8, 10), vec![0..2, 2..4]);
        // a lone oversized row is still allowed
        assert_eq!(plan_chunks(&[50, 60], 8, 10), vec![0..1, 1..2]);
        assert!(plan_chunks(&[], 4, 10).is_empty());
    }

    #[test]
    fn length_order_is_stable_ascending() {
        assert_eq!(length_order(&[5, 2, 5, 1, 2]), vec![3, 1, 4, 0, 2]);
    }

    #[test]
    fn accelerators_use_bucketed_shapes_cpu_uses_exact() {
        assert_eq!(padded_shape(3, 70, false), (3, 70));
        assert_eq!(padded_shape(3, 70, true), (4, 128));
        assert_eq!(padded_shape(1, 64, true), (1, 64));
        assert_eq!(padded_shape(16, 2048, true), (16, 2048));
    }

    #[test]
    fn probe_rejects_zero_nonfinite_and_wrong_size() {
        assert!(probe_output_ok(&[0.0; 4], 4).is_err());
        assert!(probe_output_ok(&[f32::NAN, 1.0, 0.0, 0.0], 4).is_err());
        assert!(probe_output_ok(&[1.0; 3], 4).is_err());
        assert!(probe_output_ok(&[0.5, 0.5, 0.5, 0.5], 4).is_ok());
    }

    #[test]
    fn parity_floor_is_pinned() {
        assert_eq!(ACCELERATOR_PARITY_MIN_COSINE, 0.995);
    }

    #[test]
    fn identical_vectors_pass_parity_and_report_the_lowest_pair() {
        let reference = vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]];
        let lowest = compare_parity(&reference, &reference.clone(), 3, 0.995).unwrap();
        assert!((lowest.cosine - 1.0).abs() <= 1e-6);
        // A pair exactly at the floor passes ("at or above").
        let candidate = vec![vec![1.0, 0.0, 0.0], vec![0.0, 0.6, 0.8]];
        let at_floor = cosine(&reference[1], &candidate[1]);
        let lowest = compare_parity(&reference, &candidate, 3, at_floor).unwrap();
        assert_eq!((lowest.index, lowest.cosine), (1, at_floor));
    }

    #[test]
    fn a_low_cosine_pair_fails_and_reports_value_and_index() {
        let wrong = vec![0.58, (1.0f32 - 0.58 * 0.58).sqrt()];
        let reference = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![1.0, 0.0]];
        let candidate = vec![vec![1.0, 0.0], wrong.clone(), vec![1.0, 0.0]];
        match compare_parity(&reference, &candidate, 2, 0.995) {
            Err(ParityMiss::Below(lowest)) => {
                assert_eq!(lowest.index, 1);
                assert!((lowest.cosine - 0.58).abs() <= 1e-6, "{lowest:?}");
            }
            other => panic!("expected a Below miss, got {other:?}"),
        }
        let single = compare_parity(&[vec![1.0, 0.0]], &[wrong], 2, 0.995);
        match single {
            Err(ParityMiss::Below(lowest)) => assert!((lowest.cosine - 0.58).abs() <= 1e-6),
            other => panic!("expected a Below miss, got {other:?}"),
        }
    }

    #[test]
    fn nonfinite_zero_or_wrongly_sized_vectors_fail_parity() {
        let good = vec![vec![1.0, 0.0]];
        for bad in [
            vec![f32::NAN, 1.0],
            vec![f32::INFINITY, 0.0],
            vec![0.0, 0.0],
            vec![1.0, 0.0, 0.0],
        ] {
            assert!(
                matches!(
                    compare_parity(&good, &[bad.clone()], 2, 0.995),
                    Err(ParityMiss::Invalid { index: 0, .. })
                ),
                "candidate {bad:?}"
            );
            assert!(
                matches!(
                    compare_parity(&[bad.clone()], &good, 2, 0.995),
                    Err(ParityMiss::Invalid { index: 0, .. })
                ),
                "reference {bad:?}"
            );
        }
    }

    #[test]
    fn mismatched_or_empty_parity_inputs_fail() {
        let one = vec![vec![1.0, 0.0]];
        let two = vec![vec![1.0, 0.0], vec![1.0, 0.0]];
        assert!(matches!(
            compare_parity(&one, &two, 2, 0.995),
            Err(ParityMiss::Invalid { .. })
        ));
        assert!(matches!(
            compare_parity(&two, &one, 2, 0.995),
            Err(ParityMiss::Invalid { .. })
        ));
        assert!(matches!(
            compare_parity(&[], &[], 2, 0.995),
            Err(ParityMiss::Invalid { .. })
        ));
    }

    #[test]
    fn parity_miss_detail_names_cosine_length_and_floor() {
        let tokens = [20, 100, 600];
        let miss = ParityMiss::Below(ParityLowest {
            index: 0,
            cosine: 0.5791,
        });
        assert_eq!(
            parity_miss_detail(&miss, &tokens, 0.995),
            (
                Some(0.5791),
                "cosine 0.579 at ~20 tokens, floor 0.995".to_owned()
            )
        );
        let batched = ParityMiss::Below(ParityLowest {
            index: 5,
            cosine: 0.71,
        });
        assert_eq!(
            parity_miss_detail(&batched, &tokens, 0.995).1,
            "cosine 0.710 at ~600 tokens (batched), floor 0.995"
        );
        let invalid = ParityMiss::Invalid {
            index: 1,
            reason: "accelerator output: non-finite values".into(),
        };
        assert_eq!(
            parity_miss_detail(&invalid, &tokens, 0.995),
            (
                None,
                "accelerator output: non-finite values at ~100 tokens".to_owned()
            )
        );
    }

    #[test]
    fn parity_long_document_is_deterministic_and_long() {
        let text = parity_long_document();
        assert_eq!(text, parity_long_document());
        assert!(text.split_whitespace().count() >= 400, "{}", text.len());
        assert!(PARITY_DOCUMENT.split_whitespace().count() >= 50);
    }

    #[test]
    fn parity_error_names_provider_cosine_and_floor() {
        let error = EmbedError::AcceleratorParity {
            provider: "directml",
            lowest_cosine: Some(0.579),
            floor: ACCELERATOR_PARITY_MIN_COSINE,
            detail: "cosine 0.579 at ~20 tokens, floor 0.995".into(),
        };
        assert_eq!(
            error.to_string(),
            "directml failed the vector parity check against cpu: cosine 0.579 at ~20 tokens, floor 0.995"
        );
    }

    fn parts<'a>(files: &'a [(&'a str, String)], truncate: usize) -> PipelineParts<'a> {
        PipelineParts {
            model_id: EMBEDDINGGEMMA_MODEL_ID,
            native_dim: 768,
            truncate_dim: truncate,
            max_tokens: 2048,
            scrubs_sentinels: false,
            files,
        }
    }

    #[test]
    fn pipeline_fingerprint_is_stable_and_sensitive() {
        let files = [("model_q4.onnx", "aa".to_owned())];
        let base = pipeline_fingerprint(&parts(&files, 256));
        assert_eq!(base, pipeline_fingerprint(&parts(&files, 256)));
        assert!(base.starts_with("rkpipe1-") && base.len() == "rkpipe1-".len() + 32);
        assert_ne!(base, pipeline_fingerprint(&parts(&files, 128)));
        let changed = [("model_q4.onnx", "ab".to_owned())];
        assert_ne!(base, pipeline_fingerprint(&parts(&changed, 256)));
        let mut other = parts(&files, 256);
        other.scrubs_sentinels = true;
        assert_ne!(base, pipeline_fingerprint(&other));
        other = parts(&files, 256);
        other.max_tokens = 512;
        assert_ne!(base, pipeline_fingerprint(&other));
    }

    #[test]
    fn hash_file_reports_absent_and_content() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(hash_file(&dir.path().join("none")).unwrap(), "absent");
        let path = dir.path().join("f");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            hash_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn config_defaults_and_builders() {
        let c = EmbedderConfig::new("m", "r");
        assert_eq!((c.max_batch, c.intra_threads), (16, 1));
        assert_eq!(c.provider, ExecutionProvider::Cpu);
        let c = c
            .with_max_batch(4)
            .with_intra_threads(3)
            .with_provider(ExecutionProvider::Auto);
        assert_eq!((c.max_batch, c.intra_threads), (4, 3));
        assert_eq!(c.provider, ExecutionProvider::Auto);
        let legacy =
            EmbedderConfig::new("m", "r").with_provider(rightkit_ort::ExecutionProvider::DirectMl);
        assert_eq!(legacy.provider, ExecutionProvider::DirectMl);
    }
}
