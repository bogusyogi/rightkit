//! `tool_search` — let the agent discover available tools.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::registry::CapabilityDescriptor;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};

pub const TOOL_SEARCH_DESCRIPTION: &str =
    "Discover available tools with a ranked semantic shortlist; falls back to case-insensitive substring matching when semantic retrieval is unavailable.";

const SHORTLIST_MIN: usize = 5;
const SHORTLIST_DEFAULT: usize = 7;
const SHORTLIST_MAX: usize = 9;
/// Byte cap on the no-query catalog index so an uncapped catalog can never
/// dump itself wholesale into the prompt.
const CATALOG_DUMP_MAX_BYTES: usize = 2048;
const QUERY_RESULT_MAX_BYTES: usize = 8192;
const MAX_QUERY_BYTES: usize = 1024;

/// Every query that degrades to the substring fallback is counted so the
/// token cost of retrieval without embeddings is attributable.
static EMBEDDER_UNAVAILABLE: AtomicU64 = AtomicU64::new(0);

/// Number of queries served by the substring fallback because no semantic
/// embedder was available (feature off, init failure, or embedding error).
pub fn embedder_unavailable_count() -> u64 {
    EMBEDDER_UNAVAILABLE.load(AtomicOrdering::Relaxed)
}

trait SemanticEmbedder: Send + Sync {
    fn try_embed_document(&self, text: &str) -> Result<Vec<f32>, String>;
    fn try_embed_query(&self, text: &str) -> Result<Vec<f32>, String>;
    fn model_id(&self) -> &'static str;
}

type CachedEmbedding = Arc<OnceLock<Result<Vec<f32>, String>>>;
static DOCUMENT_EMBEDDINGS: OnceLock<Mutex<HashMap<String, CachedEmbedding>>> = OnceLock::new();

fn cached_document_embedding(
    embedder: &dyn SemanticEmbedder,
    text: &str,
) -> Result<Vec<f32>, String> {
    let key = format!("{}\0{text}", embedder.model_id());
    let cell = {
        let mut cache = DOCUMENT_EMBEDDINGS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache
            .entry(key)
            .or_insert_with(|| Arc::new(OnceLock::new()))
            .clone()
    };
    cell.get_or_init(|| embedder.try_embed_document(text))
        .clone()
}

/// RightKit-backed semantic embedder (`rightkit-embed`, EmbeddingGemma). Thin adapter:
/// RightKit owns tokenisation, ONNX session setup, accelerator parity and normalisation.
/// the host app only supplies the model directory and ONNX Runtime path, which startup
/// resolves into `RIGHTKIT_EMBED_MODEL_DIR` / `ORT_DYLIB_PATH` (`host_ort_common`).
#[cfg(feature = "coding_tools-embeddings")]
struct RightKitEmbedder(rightkit_embed::Embedder);

#[cfg(feature = "coding_tools-embeddings")]
impl SemanticEmbedder for RightKitEmbedder {
    fn try_embed_document(&self, text: &str) -> Result<Vec<f32>, String> {
        self.0.embed(text).map_err(|error| error.to_string())
    }
    fn try_embed_query(&self, text: &str) -> Result<Vec<f32>, String> {
        self.0.embed_query(text).map_err(|error| error.to_string())
    }
    fn model_id(&self) -> &'static str {
        // One embedder per process; the document cache is process-local.
        "rightkit-embed"
    }
}

#[cfg(feature = "coding_tools-embeddings")]
fn default_semantic_embedder() -> Option<Arc<dyn SemanticEmbedder>> {
    static EMBEDDER: OnceLock<Option<Arc<dyn SemanticEmbedder>>> = OnceLock::new();
    EMBEDDER
        .get_or_init(|| {
            let env_path = |name: &str| {
                std::env::var(name)
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .map(std::path::PathBuf::from)
            };
            let model_dir = env_path("RIGHTKIT_EMBED_MODEL_DIR")?;
            let runtime = env_path("ORT_DYLIB_PATH")?;
            match rightkit_embed::Embedder::load(rightkit_embed::EmbedderConfig::new(
                model_dir, runtime,
            )) {
                Ok(embedder) => Some(Arc::new(RightKitEmbedder(embedder)) as Arc<dyn SemanticEmbedder>),
                Err(error) => {
                    tracing::warn!(%error, "tool_search embedder unavailable; using substring fallback");
                    None
                }
            }
        })
        .clone()
}

#[cfg(not(feature = "coding_tools-embeddings"))]
fn default_semantic_embedder() -> Option<Arc<dyn SemanticEmbedder>> {
    None
}

pub struct ToolSearchTool {
    descriptors: Vec<CapabilityDescriptor>,
    detailed: bool,
    semantic_embedder: Option<Arc<dyn SemanticEmbedder>>,
    use_default_embedder: bool,
    semantic_index: Arc<OnceLock<Result<Vec<Vec<f32>>, String>>>,
    /// `WARM_IDLE`: nothing started, a query may build the index inline.
    /// `WARMING`: a background thread is loading the model/index; queries
    /// answer lexically instead of blocking. `WARM_DONE`: index is ready (or
    /// permanently unavailable) and the normal path applies.
    warm_state: Arc<AtomicU8>,
}

const WARM_IDLE: u8 = 0;
const WARMING: u8 = 1;
const WARM_DONE: u8 = 2;

fn index_texts(descriptors: &[CapabilityDescriptor]) -> Vec<String> {
    descriptors
        .iter()
        .map(|descriptor| {
            format!(
                "Tool: {}. Capability: {}",
                descriptor.name, descriptor.description
            )
        })
        .collect()
}

fn build_semantic_index(
    embedder: &dyn SemanticEmbedder,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    texts
        .iter()
        .map(|text| cached_document_embedding(embedder, text))
        .collect()
}

fn placeholder_descriptor(name: String, description: String) -> CapabilityDescriptor {
    let input_schema = json!({"type": "object"});
    CapabilityDescriptor {
        id: format!("tool:{name}"),
        name,
        description,
        schema_digest: crate::coding_tools::registry::capability_schema_digest(&input_schema),
        input_schema,
        read_only: false,
        availability: "available".to_string(),
    }
}

impl ToolSearchTool {
    pub fn new(tools: Vec<(String, String)>) -> Self {
        Self::from_descriptors(
            tools
                .into_iter()
                .map(|(name, description)| placeholder_descriptor(name, description))
                .collect(),
            false,
        )
    }

    pub(crate) fn from_descriptors(descriptors: Vec<CapabilityDescriptor>, detailed: bool) -> Self {
        Self {
            descriptors,
            detailed,
            semantic_embedder: None,
            use_default_embedder: true,
            semantic_index: Arc::new(OnceLock::new()),
            warm_state: Arc::new(AtomicU8::new(WARM_IDLE)),
        }
    }

    /// Load the embedder and embed every descriptor on a background thread so
    /// the first `tool_search` query never pays the model-load cost. Until it
    /// finishes, queries are served by the lexical path. No-op when no
    /// embedder can exist (feature off) or a warm-up is already running.
    pub(crate) fn start_warmup(&self) {
        let embedder_possible = if self.use_default_embedder {
            cfg!(feature = "coding_tools-embeddings")
        } else {
            self.semantic_embedder.is_some()
        };
        if !embedder_possible {
            return;
        }
        if self
            .warm_state
            .compare_exchange(
                WARM_IDLE,
                WARMING,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            )
            .is_err()
        {
            return;
        }
        let use_default = self.use_default_embedder;
        let injected = self.semantic_embedder.clone();
        let texts = index_texts(&self.descriptors);
        let index = Arc::clone(&self.semantic_index);
        let state = Arc::clone(&self.warm_state);
        let spawned = std::thread::Builder::new()
            .name("tool-search-warmup".into())
            .spawn({
                let state = Arc::clone(&state);
                move || {
                    let embedder = if use_default {
                        default_semantic_embedder()
                    } else {
                        injected
                    };
                    if let Some(embedder) = embedder {
                        let built = index
                            .get_or_init(|| build_semantic_index(embedder.as_ref(), &texts))
                            .is_ok();
                        if built {
                            // Warm the query path (first query embed is also lazy).
                            let _ = embedder.try_embed_query("warmup");
                        }
                    }
                    state.store(WARM_DONE, AtomicOrdering::Release);
                }
            });
        if spawned.is_err() {
            state.store(WARM_IDLE, AtomicOrdering::Release);
        }
    }

    /// True while a background warm-up has not finished.
    fn is_warming(&self) -> bool {
        self.warm_state.load(AtomicOrdering::Acquire) == WARMING
    }

    pub(crate) fn self_descriptor() -> CapabilityDescriptor {
        let input_schema = tool_search_input_schema();
        CapabilityDescriptor {
            id: "tool:tool_search".to_string(),
            name: "tool_search".to_string(),
            description: TOOL_SEARCH_DESCRIPTION.to_string(),
            schema_digest: crate::coding_tools::registry::capability_schema_digest(&input_schema),
            input_schema,
            read_only: true,
            availability: "available".to_string(),
        }
    }

    pub fn with_defaults() -> Self {
        let defaults = vec![
            ("read_file", "Read the contents of a file."),
            ("write_file", "Write content to a file."),
            ("apply_patch", "Apply a custom patch to files."),
            ("shell", "Execute a shell command."),
            ("search", "Search for a pattern in files."),
            ("git_read", "Read git repository status and log."),
            ("git_diff", "Show a unified diff of working tree changes."),
            (
                "browser_fetch",
                "Fetch a URL and return readable text content.",
            ),
            ("web_search", "Search the web for a query."),
            (
                "tool_search",
                "Discover available tools by name or description.",
            ),
            ("attach_verdict", "Attach a trust verdict to an artifact."),
            (
                "finalize_artifact",
                "Mark an artifact as finalized/shippable.",
            ),
            ("github", "Interact with GitHub (PRs, files, comments)."),
        ];
        Self::new(
            defaults
                .into_iter()
                .map(|(name, description)| (name.to_string(), description.to_string()))
                .collect(),
        )
    }

    fn semantic_ranked_indices(&self, query: &str) -> Option<Vec<usize>> {
        if self.is_warming() {
            // Never block a query on model load; the lexical path answers.
            return None;
        }
        let embedder = if self.use_default_embedder {
            default_semantic_embedder()
        } else {
            self.semantic_embedder.clone()
        }?;
        let index = self
            .semantic_index
            .get_or_init(|| {
                build_semantic_index(embedder.as_ref(), &index_texts(&self.descriptors))
            })
            .as_ref()
            .map_err(|error| {
                tracing::warn!(
                    error,
                    "tool_search semantic index unavailable; using substring fallback"
                );
            })
            .ok()?;
        let query_embedding = embedder.try_embed_query(query).map_err(|error| {
            tracing::warn!(
                error,
                "tool_search query embedding failed; using substring fallback"
            );
        });
        let query_embedding = query_embedding.ok()?;
        let normalized_query = normalize_text(query);
        let mut ranked: Vec<(usize, f32)> = index
            .iter()
            .enumerate()
            .map(|(index, embedding)| {
                let descriptor = &self.descriptors[index];
                let score = rightkit_embed::cosine_similarity(&query_embedding, embedding)
                    + lexical_bonus(&normalized_query, &descriptor.name, &descriptor.description);
                (index, score)
            })
            .collect();
        ranked.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    self.descriptors[left.0]
                        .name
                        .cmp(&self.descriptors[right.0].name)
                })
        });
        let scores: Vec<f32> = ranked.iter().map(|(_, score)| *score).collect();
        ranked.truncate(adaptive_shortlist_len(&scores));
        Some(ranked.into_iter().map(|(index, _)| index).collect())
    }

    /// Bounded substring fallback. Matches are scored with the same lexical
    /// tiers as the semantic path so the adaptive shortlist applies, and the
    /// (score, name) ordering is deterministic across calls.
    fn substring_indices(&self, query: &str) -> Vec<usize> {
        let normalized_query = normalize_text(query);
        let query_lower = query.to_lowercase();
        let mut matched: Vec<(usize, f32)> = Vec::new();
        // Multi-word queries ("git history diff") match per term: a tool
        // scores by the share of terms its name or description contains,
        // plus the whole-phrase bonus.
        let terms: Vec<String> = query_lower
            .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .filter(|term| term.len() >= 2)
            .map(str::to_string)
            .collect();
        for (index, descriptor) in self.descriptors.iter().enumerate() {
            let name = descriptor.name.to_lowercase();
            let description = descriptor.description.to_lowercase();
            let phrase = name.contains(&query_lower) || description.contains(&query_lower);
            let hits = terms
                .iter()
                .filter(|term| name.contains(term.as_str()) || description.contains(term.as_str()))
                .count();
            if phrase || hits > 0 {
                let coverage = if terms.is_empty() {
                    0.0
                } else {
                    hits as f32 / terms.len() as f32
                };
                let score = coverage
                    + lexical_bonus(&normalized_query, &descriptor.name, &descriptor.description);
                matched.push((index, score));
            }
        }
        matched.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    self.descriptors[left.0]
                        .name
                        .cmp(&self.descriptors[right.0].name)
                })
        });
        let scores: Vec<f32> = matched.iter().map(|(_, score)| *score).collect();
        matched.truncate(adaptive_shortlist_len(&scores));
        matched.into_iter().map(|(index, _)| index).collect()
    }

    fn result_values(&self, indices: impl IntoIterator<Item = usize>) -> Vec<Value> {
        indices
            .into_iter()
            .map(|index| {
                let descriptor = &self.descriptors[index];
                if self.detailed {
                    json!({
                        "name": descriptor.name,
                        "description": descriptor.description,
                        "capability": descriptor.id,
                        "schema_digest": descriptor.schema_digest,
                        "input_schema": descriptor.input_schema,
                        "read_only": descriptor.read_only,
                        "availability": descriptor.availability,
                    })
                } else {
                    // Discovery must retain the tool name even when its live
                    // description contains a large generated API (run_program).
                    // The complete description ships with the promoted schema.
                    let description = if descriptor.description.len() > 1024 {
                        format!(
                            "{}\n[description truncated; full description in promoted tool schema]",
                            truncate_str_utf8_safe(&descriptor.description, 1024)
                        )
                    } else {
                        descriptor.description.clone()
                    };
                    json!({
                        "name": descriptor.name,
                        "description": description,
                    })
                }
            })
            .collect()
    }
}

fn tool_search_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "Optional natural-language description of the capability needed" }
        }
    })
}

fn normalize_text(text: &str) -> String {
    text.to_lowercase().replace(['_', '-'], " ")
}

fn lexical_bonus(normalized_query: &str, name: &str, description: &str) -> f32 {
    let normalized_name = normalize_text(name);
    let normalized_description = description.to_lowercase();
    if normalized_query == normalized_name {
        0.30
    } else if normalized_query.contains(&normalized_name)
        || normalized_name.contains(normalized_query)
    {
        0.15
    } else if normalized_description.contains(normalized_query) {
        0.08
    } else {
        0.0
    }
}

fn adaptive_shortlist_len(sorted_scores: &[f32]) -> usize {
    if sorted_scores.len() <= SHORTLIST_DEFAULT {
        return sorted_scores.len();
    }
    let top = sorted_scores[0];
    let fifth = sorted_scores[SHORTLIST_MIN - 1];
    let seventh = sorted_scores[SHORTLIST_DEFAULT - 1];
    if top - fifth >= 0.12 {
        SHORTLIST_MIN
    } else if top - seventh <= 0.035 {
        SHORTLIST_MAX.min(sorted_scores.len())
    } else {
        SHORTLIST_DEFAULT
    }
}

#[async_trait::async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        "tool_search"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        TOOL_SEARCH_DESCRIPTION
    }

    fn input_schema(&self) -> Value {
        tool_search_input_schema()
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let query = match args.get("query") {
            None | Some(Value::Null) => None,
            Some(Value::String(q)) => Some(q.trim()),
            Some(_) => {
                return ToolResult::error("invalid_argument: 'query' must be a string");
            }
        };
        if query.is_some_and(|q| q.len() > MAX_QUERY_BYTES) {
            return ToolResult::error(format!(
                "invalid_argument: 'query' exceeds {MAX_QUERY_BYTES} bytes"
            ));
        }
        match query {
            None | Some("") => ToolResult::ok(compact_catalog_index(&self.descriptors)),
            Some(query) => {
                let indices = match self.semantic_ranked_indices(query) {
                    Some(indices) => indices,
                    None => {
                        EMBEDDER_UNAVAILABLE.fetch_add(1, AtomicOrdering::Relaxed);
                        self.substring_indices(query)
                    }
                };
                let results = self.result_values(indices);
                let cap = if self.detailed {
                    QUERY_RESULT_MAX_BYTES
                } else {
                    CATALOG_DUMP_MAX_BYTES
                };
                ToolResult::ok(serialize_bounded(results, cap))
            }
        }
    }
}

fn truncation_marker(omitted: usize, original_bytes: usize, cap_bytes: usize) -> Value {
    json!({
        "truncated": true,
        "omitted": omitted,
        "original_bytes": original_bytes,
        "cap_bytes": cap_bytes
    })
}

/// Serialize a JSON array of entries under `cap_bytes` with a
/// typed truncation marker. Both queried and no-query paths share this
/// exact byte-limit semantics: the cap bounds the *final* serialized string
/// including the marker. Truncation drops whole entries so the byte cut is
/// UTF-8 safe (no slicing inside a code point). Ordering is caller-owned
/// and preserved.
fn serialize_bounded(entries: Vec<Value>, cap_bytes: usize) -> String {
    // Original size before any truncation — stated in the marker on overrun.
    let original_bytes = serde_json::to_string(&entries).unwrap_or_default().len();
    if original_bytes <= cap_bytes {
        return serde_json::to_string(&entries).unwrap_or_default();
    }
    // Overrun: reserve marker bytes before filling. We hard-truncate by
    // popping whole entries and re-measuring with the marker included each
    // iteration (marker size varies with digit width of `omitted` and
    // `original_bytes`), so the final string never exceeds the cap.
    let mut truncated = entries;
    let mut omitted = 0usize;
    loop {
        // Pop one entry per iteration; omitted counts total dropped.
        if truncated.is_empty() {
            let marker = truncation_marker(omitted, original_bytes, cap_bytes);
            let candidate = serde_json::to_string(&vec![marker]).unwrap_or_default();
            // If even the marker alone exceeds cap (only possible with tiny cap),
            // hard-truncate the serialized marker at a UTF-8-safe boundary.
            if candidate.len() <= cap_bytes {
                return candidate;
            }
            return truncate_str_utf8_safe(&candidate, cap_bytes);
        }
        truncated.pop();
        omitted += 1;
        let marker = truncation_marker(omitted, original_bytes, cap_bytes);
        let mut with_marker = truncated.clone();
        with_marker.push(marker);
        let candidate = serde_json::to_string(&with_marker).unwrap_or_default();
        if candidate.len() <= cap_bytes {
            return candidate;
        }
        // Continue popping; loop re-measures next iteration.
    }
}

fn truncate_str_utf8_safe(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Compact, categorized replacement for the old pretty-printed catalog dump:
/// one `{category, name}` entry per tool, descriptions omitted (a follow-up
/// query retrieves them), ordered by category then name, serialized compact
/// and truncated under `CATALOG_DUMP_MAX_BYTES` with an explicit marker.
fn compact_catalog_index(descriptors: &[CapabilityDescriptor]) -> String {
    let mut categories: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for descriptor in descriptors {
        let category = descriptor
            .name
            .split('_')
            .next()
            .unwrap_or(&descriptor.name)
            .to_string();
        categories
            .entry(category)
            .or_default()
            .insert(descriptor.name.clone());
    }
    let mut entries: Vec<Value> = Vec::new();
    for (category, names) in &categories {
        for name in names {
            entries.push(json!({"category": category, "name": name}));
        }
    }
    serialize_bounded(entries, CATALOG_DUMP_MAX_BYTES)
}
