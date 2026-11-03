//! Generation-bound task embeddings. Only Blueprint's graph database is used.
//! Inference runs on one bounded worker, so non-interruptible ONNX calls never
//! hold Recall past its allotted dense budget. Partial matrices are never ranked.

use crate::api::{CancellationToken, RequestContext};
use crate::graph::GraphGeneration;
use crate::model::GraphNode;
use crate::store;
use rightkit_context_core::fusion::{fuse, FusionBounds};
use rightkit_context_protocol::{
    CandidateV1, ContextCandidateSetV1, FreshnessV1, ProviderCeilingV1,
};
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::{Duration, Instant};

const MATRIX_GENERATIONS: usize = 4;
const DOC_LINES: usize = 40;
const DOC_CHARS: usize = 4000;

/// Raw prompts, with no second prompt wrapper. Inject this boundary in tests.
pub trait TaskEmbedder: Send {
    fn fingerprint(&self) -> String;
    fn embed_prompt(&self, prompt: &str) -> Result<Vec<f32>, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DenseOmissionReason {
    DenseIndexIncomplete,
    DenseUnavailable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DenseOmission {
    pub reason: DenseOmissionReason,
    pub lane: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DenseTaskResult {
    pub ranked_ids: Vec<String>,
    pub units: usize,
    pub embedded: usize,
    pub omissions: Vec<DenseOmission>,
}

impl DenseTaskResult {
    pub fn incomplete(detail: impl Into<String>) -> Self {
        Self {
            omissions: vec![DenseOmission {
                reason: DenseOmissionReason::DenseIndexIncomplete,
                lane: "dense_task",
                detail: detail.into(),
            }],
            ..Self::default()
        }
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        let mut result = Self::incomplete("dense coverage is unavailable");
        result.omissions.push(DenseOmission {
            reason: DenseOmissionReason::DenseUnavailable,
            lane: "dense_task",
            detail: detail.into(),
        });
        result
    }

    pub fn omission_values(&self) -> Vec<Value> {
        self.omissions.iter().map(|item| json!(item)).collect()
    }
}

#[derive(Debug, Clone)]
pub struct SymbolUnit {
    pub id: String,
    pub path: String,
    pub text: String,
}

pub fn document_prompt(path: &str, text: &str) -> String {
    format!("title: {path} | text: {text}")
}

pub fn query_prompt(task: &str) -> String {
    format!("task: search result | query: {task}")
}

/// Independent of generation/node identity, so unchanged prompts survive rebuilds.
pub fn vector_cache_key(model_fingerprint: &str, prompt: &str) -> (String, String) {
    (
        model_fingerprint.to_owned(),
        rightkit_fs::digest::sha256_bytes(prompt.as_bytes()),
    )
}

fn evidence_text<'a>(node: &'a GraphNode, key: &str) -> Option<&'a str> {
    node.evidence
        .iter()
        .find_map(|e| e.get(key).and_then(Value::as_str))
}

fn line(node: &GraphNode, key: &str) -> Option<usize> {
    node.evidence.iter().find_map(|e| {
        e.get(key)
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
    })
}

/// Same span deduplication, qualification preference & body truncation as
/// qualification/blueprint-fullrepo/make_units.py::symbols(). Its 4000-character
/// cap applies to the body, before adding the symbol/file heading.
pub fn symbol_units(
    generation: &GraphGeneration,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> Result<Vec<SymbolUnit>, String> {
    let mut best: HashMap<(String, Option<usize>, Option<usize>, bool), usize> = HashMap::new();
    let mut chosen: Vec<&GraphNode> = Vec::new();
    for node in &generation.nodes {
        if Instant::now() >= deadline || cancellation.is_cancelled() {
            return Err("unit extraction deadline reached".into());
        }
        if matches!(node.kind.as_str(), "domain" | "bridge") {
            continue;
        }
        if !node.generation_id.is_empty() && node.generation_id != generation.generation_id {
            return Err("symbol generation mismatch".into());
        }
        let Some(path) = node.path.as_deref().filter(|p| !p.is_empty()) else {
            continue;
        };
        let key = (
            path.to_owned(),
            line(node, "startLine"),
            line(node, "endLine"),
            node.kind == "file",
        );
        if let Some(index) = best.get(&key) {
            if evidence_text(node, "qualifiedName").is_some_and(|n| n.contains('.'))
                && !evidence_text(chosen[*index], "qualifiedName").is_some_and(|n| n.contains('.'))
            {
                chosen[*index] = node;
            }
        } else {
            best.insert(key, chosen.len());
            chosen.push(node);
        }
    }
    let root = Path::new(&generation.repo_root);
    let mut last_file: Option<(String, String)> = None;
    let mut units = Vec::new();
    for node in chosen {
        if Instant::now() >= deadline || cancellation.is_cancelled() {
            return Err("unit extraction deadline reached".into());
        }
        let path = node.path.as_deref().unwrap_or_default();
        if last_file.as_ref().map(|(p, _)| p.as_str()) != Some(path) {
            let physical = crate::security::is_confined_path(root, &root.join(path), false)
                .map_err(|_| format!("unit source unavailable: {path}"))?;
            let metadata = std::fs::metadata(&physical).map_err(|e| e.to_string())?;
            if !metadata.is_file() || metadata.len() > crate::graph::MAX_FILE_BYTES {
                return Err(format!("unit source unavailable: {path}"));
            }
            let bytes = std::fs::read(&physical).map_err(|e| e.to_string())?;
            // A live edit must not be embedded under a sealed generation identity.
            if let Some(expected) = evidence_text(node, "contentHash") {
                if crate::identity::content_digest(&bytes) != expected {
                    return Err(format!("unit source differs from generation: {path}"));
                }
            }
            let text = String::from_utf8_lossy(&bytes)
                .replace("\r\n", "\n")
                .replace('\r', "\n");
            last_file = Some((path.to_owned(), text));
        }
        let text = &last_file.as_ref().unwrap().1;
        let lines: Vec<&str> = text.split('\n').collect();
        let (start, end, head) = if node.kind == "file" {
            (1, lines.len().min(DOC_LINES), format!("file {path}"))
        } else {
            let (Some(start), Some(end)) = (line(node, "startLine"), line(node, "endLine")) else {
                continue;
            };
            if end < start {
                continue;
            }
            let name = evidence_text(node, "qualifiedName")
                .filter(|s| !s.is_empty())
                .or(node.name.as_deref())
                .unwrap_or("");
            (
                start,
                end.min(start.saturating_add(DOC_LINES - 1)),
                format!("{} {name}", node.kind),
            )
        };
        let body: String = lines
            .get(start - 1..end.min(lines.len()))
            .unwrap_or(&[])
            .join("\n")
            .chars()
            .take(DOC_CHARS)
            .collect();
        if body.trim().is_empty() {
            continue;
        }
        units.push(SymbolUnit {
            id: node.id.clone(),
            path: path.to_owned(),
            text: format!("{head}\n{body}"),
        });
    }
    Ok(units)
}

fn normalize(mut vector: Vec<f32>) -> Result<Vec<f32>, String> {
    if vector.is_empty() || vector.iter().any(|v| !v.is_finite()) {
        return Err("invalid dense vector".into());
    }
    let norm = vector
        .iter()
        .map(|v| f64::from(*v).powi(2))
        .sum::<f64>()
        .sqrt();
    if norm <= 0.0 || !norm.is_finite() {
        return Err("zero dense vector".into());
    }
    for value in &mut vector {
        *value = (f64::from(*value) / norm) as f32;
    }
    Ok(vector)
}

struct DenseMatrix {
    ids: Vec<String>,
    vectors: Vec<Vec<f32>>,
}

/// Owned by the serialized worker; matrices are shared immutable Arc values.
#[derive(Default)]
pub struct DenseTaskIndex {
    matrices: Vec<(String, Arc<DenseMatrix>)>,
}

fn same_generation(conn: &Connection, generation: &GraphGeneration) -> Result<(), String> {
    let envelope = store::read_generation_envelope(conn)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "generation missing".to_owned())?;
    if envelope.generation_id() != Some(generation.generation_id.as_str()) {
        return Err("dense generation mismatch".into());
    }
    Ok(())
}

impl DenseTaskIndex {
    /// Complete or resume derived vectors. Never rank partial coverage.
    #[allow(clippy::too_many_arguments)] // resume inputs are independent; a wrapper struct would not simplify callers
    pub fn run(
        &mut self,
        conn: &Connection,
        generation: &GraphGeneration,
        embedder: Option<&dyn TaskEmbedder>,
        task: Option<&str>,
        limit: usize,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> DenseTaskResult {
        let Some(embedder) = embedder else {
            return DenseTaskResult::unavailable("embedder/model missing");
        };
        if Instant::now() >= deadline || cancellation.is_cancelled() {
            return DenseTaskResult::incomplete("dense deadline reached");
        }
        if let Err(detail) = same_generation(conn, generation) {
            return DenseTaskResult::incomplete(detail);
        }
        let fingerprint = embedder.fingerprint();
        if fingerprint.is_empty() || fingerprint.contains("unavailable") {
            return DenseTaskResult::unavailable("model fingerprint unavailable");
        }
        if Instant::now() >= deadline || cancellation.is_cancelled() {
            return DenseTaskResult::incomplete("dense fingerprint deadline reached");
        }
        let key = format!(
            "{}\0{}\0{}",
            generation.repo_root, generation.generation_id, fingerprint
        );
        let matrix = if let Some((_, matrix)) = self.matrices.iter().find(|(id, _)| id == &key) {
            Arc::clone(matrix)
        } else {
            let units = match symbol_units(generation, deadline, cancellation) {
                Ok(units) => units,
                Err(detail) => return DenseTaskResult::incomplete(detail),
            };
            let mut matrix = DenseMatrix {
                ids: Vec::new(),
                vectors: Vec::new(),
            };
            let mut result = DenseTaskResult {
                units: units.len(),
                ..Default::default()
            };
            let build = (|| -> Result<(), String> {
                let mut lookup = conn.prepare_cached("SELECT dim,embedding FROM task_seed_vectors WHERE model_fingerprint=?1 AND prompt_sha256=?2")
                    .map_err(|e| e.to_string())?;
                for unit in units {
                    if Instant::now() >= deadline || cancellation.is_cancelled() {
                        return Err("dense build deadline reached".into());
                    }
                    let prompt = document_prompt(&unit.path, &unit.text);
                    let (_, digest) = vector_cache_key(&fingerprint, &prompt);
                    let cached: Option<(i64, Vec<u8>)> = lookup
                        .query_row(params![fingerprint, digest], |row| {
                            Ok((row.get(0)?, row.get(1)?))
                        })
                        .optional()
                        .map_err(|e| e.to_string())?;
                    let vector = match cached {
                        Some((dim, bytes)) if bytes.len() == (dim as usize).saturating_mul(4) => {
                            normalize(
                                bytes
                                    .as_chunks::<4>()
                                    .0
                                    .iter()
                                    .map(|b| f32::from_le_bytes(*b))
                                    .collect(),
                            )?
                        }
                        _ => {
                            let vector = normalize(embedder.embed_prompt(&prompt)?)?;
                            // Inference cannot be interrupted. Its caller is already bounded;
                            // discard late results instead of doing more work after the deadline.
                            if Instant::now() >= deadline || cancellation.is_cancelled() {
                                return Err("dense inference deadline reached".into());
                            }
                            same_generation(conn, generation)?;
                            let bytes: Vec<u8> =
                                vector.iter().flat_map(|v| v.to_le_bytes()).collect();
                            conn.execute("INSERT INTO task_seed_vectors(model_fingerprint,prompt_sha256,dim,embedding) VALUES(?1,?2,?3,?4) ON CONFLICT(model_fingerprint,prompt_sha256) DO UPDATE SET dim=excluded.dim,embedding=excluded.embedding", params![fingerprint, digest, vector.len() as i64, bytes])
                                .map_err(|e| e.to_string())?;
                            vector
                        }
                    };
                    if matrix
                        .vectors
                        .first()
                        .is_some_and(|v| v.len() != vector.len())
                    {
                        return Err("dense dimension mismatch".into());
                    }
                    matrix.ids.push(unit.id);
                    matrix.vectors.push(vector);
                    result.embedded += 1;
                }
                same_generation(conn, generation)
            })();
            if let Err(detail) = build {
                let mut fallback = if detail.contains("embedder")
                    || detail.contains("model")
                    || detail.contains("prompt API")
                {
                    DenseTaskResult::unavailable(detail)
                } else {
                    DenseTaskResult::incomplete(detail)
                };
                fallback.units = result.units;
                fallback.embedded = result.embedded;
                return fallback;
            }
            let matrix = Arc::new(matrix);
            if self.matrices.len() >= MATRIX_GENERATIONS {
                self.matrices.remove(0);
            }
            self.matrices.push((key, Arc::clone(&matrix)));
            matrix
        };
        let mut result = DenseTaskResult {
            units: matrix.ids.len(),
            embedded: matrix.ids.len(),
            ..Default::default()
        };
        let Some(task) = task.filter(|task| !task.trim().is_empty()) else {
            return result;
        };
        if Instant::now() >= deadline || cancellation.is_cancelled() {
            return DenseTaskResult::incomplete("dense query deadline reached");
        }
        let query = match embedder
            .embed_prompt(&query_prompt(task))
            .and_then(normalize)
        {
            Ok(query) => query,
            Err(detail) => return DenseTaskResult::unavailable(detail),
        };
        let mut scores = Vec::with_capacity(matrix.ids.len());
        for (index, vector) in matrix.vectors.iter().enumerate() {
            if index % 128 == 0 && (Instant::now() >= deadline || cancellation.is_cancelled()) {
                return DenseTaskResult::incomplete("dense ranking deadline reached");
            }
            if vector.len() != query.len() {
                return DenseTaskResult::unavailable("dense query dimension mismatch");
            }
            let score: f32 = vector.iter().zip(&query).map(|(a, b)| a * b).sum();
            scores.push((index, score));
        }
        scores.sort_by(|(a, x), (b, y)| {
            y.total_cmp(x)
                .then_with(|| matrix.ids[*a].cmp(&matrix.ids[*b]))
        });
        if Instant::now() >= deadline || cancellation.is_cancelled() {
            return DenseTaskResult::incomplete("dense ranking deadline reached");
        }
        if let Err(detail) = same_generation(conn, generation) {
            return DenseTaskResult::incomplete(detail);
        }
        result.ranked_ids = scores
            .into_iter()
            .take(limit)
            .map(|(i, _)| matrix.ids[i].clone())
            .collect();
        result
    }
}

/// Adapt seed ranks to Pull's existing RRF implementation, without changing
/// evidence authority: these synthetic candidates never leave seed selection.
pub fn fuse_task_ranks(bm25: &[String], dense: &[String], limit: usize) -> Vec<String> {
    let sets = [("bm25_task", bm25), ("dense_task", dense)]
        .into_iter()
        .map(|(lane, ids)| {
            let mut seen = HashSet::new();
            let candidates = ids
                .iter()
                .filter(|id| seen.insert(id.as_str()))
                .enumerate()
                .map(|(rank, id)| CandidateV1 {
                    id: id.clone(),
                    layer: 0,
                    provider: Some(lane.into()),
                    source_kind: "blueprint_task_seed".into(),
                    source_ref: id.clone(),
                    source_hash: rightkit_fs::digest::sha256_bytes(id.as_bytes()),
                    trust_class: "workspace_tracked".into(),
                    instruction_policy: "data".into(),
                    provider_score: 1.0 / (rank + 1) as f64,
                    score_components: BTreeMap::new(),
                    base_commit: None,
                    overlay_digest: None,
                    freshness_class: None,
                    snapshot_id: None,
                    estimated_tokens: 0,
                    protected: false,
                    exact: false,
                    recoverable: false,
                    resolver: String::new(),
                    text: String::new(),
                })
                .collect::<Vec<_>>();
            ContextCandidateSetV1 {
                schema_version: 1,
                trace_id: "blueprint_task_seeds".into(),
                indexed_at: String::new(),
                task: String::new(),
                mode: "seed_ranking".into(),
                provider: lane.into(),
                freshness: FreshnessV1 {
                    revision: String::new(),
                    indexed_at: String::new(),
                    stale: false,
                    graph_state: None,
                    snapshot_id: None,
                    base_commit: None,
                    overlay_digest: None,
                    expected_release_generation: None,
                    observed_release_generation: None,
                    release_generation_status: None,
                    overlay_identity: None,
                },
                provider_ceiling: ProviderCeilingV1 {
                    max_candidates: candidates.len() as u32,
                    max_estimated_tokens: 0,
                },
                candidates,
                omissions: Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    fuse(
        &sets,
        FusionBounds {
            max_items: limit as u32,
            ..Default::default()
        },
    )
    .candidates
    .into_iter()
    .map(|candidate| candidate.id)
    .collect()
}

/// Document prompting is not exposed by rightkit-embed 0.2.2, so the dense lane
/// serves query-side prompts only. The ONNX runtime comes from rightkit-onnx's
/// configured binding; this module never reads or sets ORT_DYLIB_PATH.
struct Eg2Embedder(rightkit_embed::Embedder);

impl TaskEmbedder for Eg2Embedder {
    fn fingerprint(&self) -> String {
        let model = self.0.pipeline_id();
        let pipeline =
            format!("{model}|title: <path> | text: <unit>|task: search result | query: <task>");
        rightkit_fs::digest::sha256_bytes(pipeline.as_bytes())
    }

    fn embed_prompt(&self, prompt: &str) -> Result<Vec<f32>, String> {
        if let Some(task) = prompt.strip_prefix("task: search result | query: ") {
            return self
                .0
                .embed_query(task)
                .map_err(|e| format!("embedder: {e}"));
        }
        Err("dense_unavailable: rightkit-embed 0.2.2 lacks titled-document prompts".into())
    }
}

fn load_eg2() -> Result<Box<dyn TaskEmbedder>, String> {
    let model = crate::qa_env::embed_model_dir()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "dense_unavailable: embedding model directory missing".to_owned())?;
    let config: Value = serde_json::from_slice(
        &std::fs::read(model.join("config.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if config["model_type"] != "embedding_gemma2" {
        return Err("dense_unavailable: EG2 bundle missing".into());
    }
    let runtime = rightkit_onnx::configured_runtime().ok_or_else(|| {
        "dense_unavailable: ONNX runtime missing; memory writes refused".to_owned()
    })?;
    let config = rightkit_embed::EmbedderConfig::new(model, runtime)
        .with_truncate_dim(768)
        .with_max_tokens(2048)
        .with_intra_threads(2);
    rightkit_embed::Embedder::load(config)
        .map(|e| Box::new(Eg2Embedder(e)) as Box<dyn TaskEmbedder>)
        .map_err(|e| format!("dense_unavailable: {e}"))
}

struct DenseJob {
    generation: Arc<GraphGeneration>,
    task: Option<String>,
    limit: usize,
    deadline: Instant,
    cancellation: CancellationToken,
    reply: mpsc::Sender<DenseTaskResult>,
}

fn worker() -> Option<&'static mpsc::SyncSender<DenseJob>> {
    static WORKER: OnceLock<Option<mpsc::SyncSender<DenseJob>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<DenseJob>(1);
            std::thread::Builder::new()
                .name("blueprint-task-embed".into())
                .spawn(move || {
                    let mut index = DenseTaskIndex::default();
                    let mut embedder: Option<Box<dyn TaskEmbedder>> = None;
                    while let Ok(job) = receiver.recv() {
                        if Instant::now() >= job.deadline || job.cancellation.is_cancelled() {
                            let _ = job
                                .reply
                                .send(DenseTaskResult::incomplete("dense job expired"));
                            continue;
                        }
                        if embedder.is_none() {
                            match load_eg2() {
                                Ok(loaded) => embedder = Some(loaded),
                                Err(detail) => {
                                    let _ = job.reply.send(DenseTaskResult::unavailable(detail));
                                    continue;
                                }
                            }
                        }
                        if Instant::now() >= job.deadline || job.cancellation.is_cancelled() {
                            let _ = job.reply.send(DenseTaskResult::incomplete(
                                "dense initialization deadline reached",
                            ));
                            continue;
                        }
                        let db = Path::new(&job.generation.repo_root).join(".agent/graph/graph.db");
                        if !db.is_file() {
                            let _ = job.reply.send(DenseTaskResult::incomplete(
                                "Blueprint generation database missing",
                            ));
                            continue;
                        }
                        let result = match store::open_store(Some(&db)) {
                            Ok(conn) => {
                                let _ = conn.busy_timeout(Duration::ZERO);
                                index.run(
                                    &conn,
                                    &job.generation,
                                    embedder.as_deref(),
                                    job.task.as_deref(),
                                    job.limit,
                                    job.deadline,
                                    &job.cancellation,
                                )
                            }
                            Err(detail) => DenseTaskResult::incomplete(detail.to_string()),
                        };
                        let _ = job.reply.send(result);
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}

/// Keep most of Recall's deadline for lexical selection, traversal & receipts.
/// Explicit Embed gets the remaining budget minus a response reserve instead.
pub fn request_dense(
    generation: Arc<GraphGeneration>,
    task: Option<String>,
    limit: usize,
    context: &RequestContext,
) -> DenseTaskResult {
    if task.as_deref().is_some_and(|task| task.trim().is_empty()) {
        return DenseTaskResult::default();
    }
    let remaining = context.deadline.remaining();
    let budget = if task.is_some() {
        (remaining / 3).min(Duration::from_millis(500))
    } else {
        remaining.saturating_sub(Duration::from_millis(100))
    };
    if budget.is_zero() {
        return DenseTaskResult::incomplete("no dense deadline budget");
    }
    let deadline = Instant::now() + budget;
    let Some(worker) = worker() else {
        return DenseTaskResult::unavailable("dense worker unavailable");
    };
    let (reply, receiver) = mpsc::channel();
    let job = DenseJob {
        generation,
        task,
        limit,
        deadline,
        cancellation: context.cancellation.clone(),
        reply,
    };
    if worker.try_send(job).is_err() {
        return DenseTaskResult::incomplete("dense worker busy");
    }
    // Check cancellation between bounded waits, not only after a slow inference.
    loop {
        if context.cancellation.is_cancelled() {
            return DenseTaskResult::incomplete("dense request cancelled");
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return DenseTaskResult::incomplete("dense deadline reached");
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(10))) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return DenseTaskResult::unavailable("dense worker stopped")
            }
        }
    }
}
