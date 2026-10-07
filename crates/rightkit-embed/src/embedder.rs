use std::path::PathBuf;
use std::sync::Mutex;

use ort::session::Session;
use rightkit_ort::{
    configure_runtime, init_environment, CandidateSource, EnvironmentOptions, ExecutionProvider,
    GlobalPool, RuntimeCandidate, SessionOptions,
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
}

#[derive(Debug, Clone)]
pub struct EmbedderConfig {
    pub model_dir: PathBuf,
    pub runtime_path: PathBuf,
    pub provider: ExecutionProvider,
    pub truncate_dim: usize,
    pub max_tokens: usize,
}

impl EmbedderConfig {
    pub fn new(model_dir: impl Into<PathBuf>, runtime_path: impl Into<PathBuf>) -> Self {
        Self {
            model_dir: model_dir.into(),
            runtime_path: runtime_path.into(),
            provider: ExecutionProvider::Cpu,
            truncate_dim: DEFAULT_TRUNCATE_DIM,
            max_tokens: MAX_SEQUENCE_TOKENS,
        }
    }

    pub fn with_provider(mut self, provider: ExecutionProvider) -> Self {
        self.provider = provider;
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
}

pub struct Embedder {
    tokenizer: Tokenizer,
    session: Mutex<Session>,
    native_dim: usize,
    truncate_dim: usize,
    max_tokens: usize,
    model_id: String,
    gemma2: bool,
}

impl Embedder {
    pub fn load(config: EmbedderConfig) -> Result<Self, EmbedError> {
        validate_config(&config)?;
        let gemma2 = bundle_is_gemma2(&config.model_dir)?;
        let tokenizer = Tokenizer::from_file(config.model_dir.join("tokenizer.json"))
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
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
        let session = SessionOptions::new(config.provider)
            .with_threads(Some(1))
            .build_from_file(&config.model_dir.join("model_q4.onnx"))
            .map_err(|e| EmbedError::Runtime(e.to_string()))?;
        Ok(Self {
            tokenizer,
            session: Mutex::new(session),
            native_dim: EMBEDDINGGEMMA_NATIVE_DIM,
            truncate_dim: config.truncate_dim,
            max_tokens: config.max_tokens,
            model_id: if gemma2 {
                EMBEDDINGGEMMA2_MODEL_ID
            } else {
                EMBEDDINGGEMMA_MODEL_ID
            }
            .to_owned(),
            gemma2,
        })
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

    pub fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        self.embed_prompt(&format!("title: none | text: {}", self.model_text(text)))
    }

    pub fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        self.embed_prompt(&format!(
            "task: search result | query: {}",
            self.model_text(text)
        ))
    }

    pub fn embed_batch<'a, I>(&self, texts: I) -> Result<Vec<Vec<f32>>, EmbedError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        texts.into_iter().map(|text| self.embed(text)).collect()
    }

    pub fn embed_query_batch<'a, I>(&self, texts: I) -> Result<Vec<Vec<f32>>, EmbedError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        texts
            .into_iter()
            .map(|text| self.embed_query(text))
            .collect()
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

    fn embed_prompt(&self, prompt: &str) -> Result<Vec<f32>, EmbedError> {
        let encoding = self
            .tokenizer
            .encode(EncodeInput::Single(prompt.to_owned().into()), true)
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
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
        let mut session = self
            .session
            .lock()
            .map_err(|_| EmbedError::Runtime("model mutex poisoned".into()))?;
        let outputs = session
            .run(ort::inputs![
                "input_ids" => ort::value::Tensor::from_array(([1usize, ids.len()], ids))
                    .map_err(|e| EmbedError::Runtime(e.to_string()))?,
                "attention_mask" => ort::value::Tensor::from_array(([1usize, mask.len()], mask.clone()))
                    .map_err(|e| EmbedError::Runtime(e.to_string()))?
            ])
            .map_err(|e| EmbedError::Runtime(e.to_string()))?;
        let output = outputs
            .get("sentence_embedding")
            .ok_or_else(|| EmbedError::Output("missing sentence_embedding output".into()))?;
        let (shape, values) = output
            .try_extract_tensor::<f32>()
            .map_err(|e| EmbedError::Output(e.to_string()))?;
        let native = sentence_embedding(shape.as_ref(), values, &mask, self.native_dim)?;
        Ok(truncate_and_normalize(&native, self.truncate_dim))
    }
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

fn sentence_embedding(
    shape: &[i64],
    values: &[f32],
    mask: &[i64],
    native_dim: usize,
) -> Result<Vec<f32>, EmbedError> {
    if shape == [1, native_dim as i64] && values.len() == native_dim {
        return Ok(normalize(values.to_vec()));
    }
    if shape.len() == 3
        && shape[0] == 1
        && shape[2] == native_dim as i64
        && shape[1] > 0
        && values.len() == shape[1] as usize * native_dim
    {
        let tokens = shape[1] as usize;
        let mut pooled = vec![0.0; native_dim];
        let mut count = 0.0f32;
        for token in 0..tokens {
            if mask.get(token).copied().unwrap_or(1) == 0 {
                continue;
            }
            count += 1.0;
            for (dim, value) in pooled.iter_mut().enumerate() {
                *value += values[token * native_dim + dim];
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
        return Ok(normalize(pooled));
    }
    Err(EmbedError::Output(format!(
        "expected [1,{native_dim}] or [1,tokens,{native_dim}], got shape {shape:?}"
    )))
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
