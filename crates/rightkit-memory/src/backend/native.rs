//! `MemoryBackend` and Adapt client over Pulse's `membrane` CLI.
//!
//! Everything here goes through `crate::membrane::MembraneBackend`; each CLI call is a
//! child process bounded by the adapter's hard timeout, and each async call is additionally
//! bounded by the caller's [`CallContext`] deadline and cancellation.
//!
//! Mapping notes (the CLI is narrower than the old resident protocol):
//!
//! * `put`, `try_put`, `remember`: `membrane memory push <text>` stores the content bytes
//!   verbatim. Name, scope, tier and keywords have no CLI equivalent; Pulse scopes and tiers
//!   the memory itself. Nothing is dropped from the content.
//! * `remember_consolidated`: no server-side dedupe exists. An exact (trimmed) content match
//!   in the top search hit returns `Ok(None)`; anything else is pushed. The similarity
//!   `threshold` is not used, so near-duplicates are kept rather than risk losing a memory.
//! * `recall_scored`/`search`: `memory search`. The CLI takes no scope list, so `scopes` is
//!   applied client-side to items that carry a scope; items with no scope or `global` stay.
//! * `list`: `memory list` (the `scope` argument cannot be forwarded and is ignored).
//! * `entries`, `get_full`, `activity_json`, `embedder_dim`: no CLI equivalent, explicit error.
//! * `record_injections`, `log_recall_with_replay`: telemetry only; Pulse records injections
//!   itself, so these succeed as no-ops.

use crate::backend::contract::{CallContext, MemoryBackend};
use crate::backend::error::{MemoryBackendError, MemoryBackendErrorKind};
use crate::backend::types::{
    normalize_scope, BackendBinding, BackendIdentity, FullMemoryRecord, JsonValue, MemoryEntry,
    MemoryId, MemoryListRow, MemoryTier, Score,
};
use crate::membrane::{
    ContextPull, MembraneBackend, MembraneCli, MembraneError, MemoryItem, Unavailable, OFF_REASON,
};
use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Error text for operations Pulse's CLI does not expose yet.
pub const NOT_AVAILABLE_VIA_CLI: &str = "not available via Membrane CLI (Orthic-Labs/pulse#129)";

static OFF: Unavailable = Unavailable;

enum Inner {
    Cli {
        cli: MembraneCli,
        /// `membrane where --json` check, run once on first use (never at construction).
        verified: OnceLock<Result<(), String>>,
    },
    /// Pulse is not installed.
    Off,
}

impl Inner {
    fn backend(&self) -> Result<&dyn MembraneBackend, String> {
        match self {
            Inner::Off => Ok(&OFF),
            Inner::Cli { cli, verified } => {
                match verified.get_or_init(|| cli.verify().map(|_| ()).map_err(|e| e.to_string())) {
                    Ok(()) => Ok(cli),
                    Err(reason) => Err(reason.clone()),
                }
            }
        }
    }

    fn run<T>(
        &self,
        f: impl FnOnce(&dyn MembraneBackend) -> Result<T, MembraneError>,
    ) -> Result<T, MembraneError> {
        match self.backend() {
            Ok(backend) => f(backend),
            Err(reason) => Err(MembraneError::Unavailable(format!(
                "off: membrane unusable: {reason}"
            ))),
        }
    }
}

/// Shared, cheaply cloneable access to the located `membrane` binary.
#[derive(Clone)]
pub struct MembraneHandle {
    inner: Arc<Inner>,
}

impl MembraneHandle {
    /// Locate Pulse's `membrane` (env override, notch state, resident file, installed path).
    /// Touches the filesystem only; the binary is verified lazily on first call.
    pub fn connect() -> Self {
        match MembraneCli::locate() {
            Some(binary) => Self::from_cli(MembraneCli::new(binary)),
            None => Self {
                inner: Arc::new(Inner::Off),
            },
        }
    }

    /// The one process-wide handle: a single discovery and a single `membrane where` check,
    /// shared by the memory backend, `DaemonState.membrane` and the transcript exporter.
    pub fn shared() -> Self {
        static SHARED: OnceLock<MembraneHandle> = OnceLock::new();
        SHARED.get_or_init(Self::connect).clone()
    }

    /// The verified adapter, or `None` when Pulse is absent or the binary failed verification.
    pub fn verified_cli(&self) -> Option<MembraneCli> {
        match &*self.inner {
            Inner::Off => None,
            Inner::Cli { cli, .. } => self.inner.backend().ok().map(|_| cli.clone()),
        }
    }

    /// Shareable `MembraneBackend`: the verified CLI, else `Unavailable`.
    pub fn backend_arc(&self) -> Arc<dyn MembraneBackend + Send + Sync> {
        match self.verified_cli() {
            Some(cli) => Arc::new(cli),
            None => Arc::new(Unavailable),
        }
    }

    /// Use an explicit adapter (custom timeout, test binary).
    pub fn from_cli(cli: MembraneCli) -> Self {
        Self {
            inner: Arc::new(Inner::Cli {
                cli,
                verified: OnceLock::new(),
            }),
        }
    }

    /// True when no Pulse binary was found.
    pub fn is_off(&self) -> bool {
        matches!(*self.inner, Inner::Off)
    }

    /// `off: pulse not installed`, or the verify failure. May run `membrane where` (bounded).
    pub fn off_reason(&self) -> Option<String> {
        match &*self.inner {
            Inner::Off => Some(OFF_REASON.to_owned()),
            Inner::Cli { .. } => self
                .inner
                .backend()
                .err()
                .map(|reason| format!("off: membrane unusable: {reason}")),
        }
    }

    /// Synchronous call, bounded by the adapter's per-invocation timeout.
    pub fn call_blocking<T>(
        &self,
        f: impl FnOnce(&dyn MembraneBackend) -> Result<T, MembraneError>,
    ) -> Result<T, MembraneError> {
        self.inner.run(f)
    }

    /// Async call on the blocking pool, bounded by the context deadline and cancellation.
    /// A timeout or cancellation of a mutation reports `CommitUnknown`.
    pub async fn call<T, F>(
        &self,
        context: &CallContext,
        mutation: bool,
        f: F,
    ) -> Result<T, MemoryBackendError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn MembraneBackend) -> Result<T, MembraneError> + Send + 'static,
    {
        context.check()?;
        let inner = Arc::clone(&self.inner);
        let task = tokio::task::spawn_blocking(move || inner.run(f));
        let cancel = context.cancellation();
        let deadline = context.deadline();
        let waited = async {
            match deadline {
                Some(at) => tokio::time::timeout_at(tokio::time::Instant::from_std(at), task)
                    .await
                    .map_err(|_| ()),
                None => Ok(task.await),
            }
        };
        let joined = tokio::select! {
            result = waited => match result {
                Ok(joined) => joined,
                Err(()) => return Err(interrupted(mutation, true)),
            },
            _ = cancel.cancelled() => return Err(interrupted(mutation, false)),
        };
        match joined {
            Ok(result) => result.map_err(|error| map_error(error, mutation)),
            Err(error) => Err(MemoryBackendError::Internal {
                code: "membrane_task_failed".into(),
                message: format!("membrane call did not complete: {error}"),
            }),
        }
    }

    /// Bounded async `context pull`. Pulse absent yields empty text with the off reason.
    pub async fn context_pull(
        &self,
        context: &CallContext,
        cwd: &Path,
        prompt: &str,
        budget: Option<usize>,
        repo: Option<&Path>,
    ) -> Result<ContextPull, MemoryBackendError> {
        let (cwd, prompt, repo) = (
            cwd.to_path_buf(),
            prompt.to_owned(),
            repo.map(Path::to_path_buf),
        );
        self.call(context, false, move |m| {
            m.context_pull(&cwd, &prompt, budget, repo.as_deref())
        })
        .await
    }
}

fn interrupted(mutation: bool, timed_out: bool) -> MemoryBackendError {
    match (mutation, timed_out) {
        (true, _) => MemoryBackendError::commit_unknown(
            "membrane mutation interrupted before an answer; it may have committed",
        ),
        (false, true) => MemoryBackendError::timeout("memory operation deadline elapsed"),
        (false, false) => MemoryBackendError::cancelled("memory operation cancelled"),
    }
}

/// Map an adapter error. A mutation whose outcome is unknown (timeout, odd exit, unparsable
/// answer after the child ran) is `CommitUnknown`, never a plain failure.
pub fn map_error(error: MembraneError, mutation: bool) -> MemoryBackendError {
    let message = error.to_string();
    match error {
        MembraneError::Unavailable(_)
        | MembraneError::Spawn(_)
        | MembraneError::ResidentUnreachable(_) => MemoryBackendError::BackendUnavailable {
            code: "backend_unavailable".into(),
            message,
        },
        MembraneError::Timeout(_) if mutation => MemoryBackendError::commit_unknown(message),
        MembraneError::Timeout(_) => MemoryBackendError::timeout(message),
        MembraneError::Exit { .. } | MembraneError::Parse(_) if mutation => {
            MemoryBackendError::commit_unknown(message)
        }
        MembraneError::Internal(_) => MemoryBackendError::Store {
            code: "membrane_internal".into(),
            message,
        },
        MembraneError::Usage(_) => MemoryBackendError::Invalid {
            code: "membrane_usage".into(),
            message,
        },
        MembraneError::NotAvailable(_) => MemoryBackendError::Unavailable {
            code: "membrane_not_available".into(),
            message,
        },
        MembraneError::Parse(_) | MembraneError::UnsupportedSchema { .. } => {
            MemoryBackendError::Incompatible {
                code: "membrane_schema".into(),
                message,
            }
        }
        MembraneError::Exit { .. } => MemoryBackendError::Internal {
            code: "membrane_exit".into(),
            message,
        },
    }
}

fn not_available(operation: &str) -> MemoryBackendError {
    MemoryBackendError::Unavailable {
        code: "not_available_via_membrane_cli".into(),
        message: format!("{operation}: {NOT_AVAILABLE_VIA_CLI}"),
    }
}

pub fn is_unavailable(error: &MemoryBackendError) -> bool {
    matches!(
        error.kind(),
        MemoryBackendErrorKind::Unavailable | MemoryBackendErrorKind::BackendUnavailable
    )
}

/// Memory backend over Pulse's `membrane` CLI.
pub struct MembraneMemoryBackend {
    handle: MembraneHandle,
    binding: BackendBinding,
}

impl MembraneMemoryBackend {
    pub fn new(handle: MembraneHandle) -> Self {
        Self {
            handle,
            binding: BackendBinding::new(BackendIdentity::service("membrane-cli", 1, 1, None)),
        }
    }

    pub fn handle(&self) -> &MembraneHandle {
        &self.handle
    }

    async fn push(&self, context: &CallContext, text: &str) -> Result<Value, MemoryBackendError> {
        let text = text.to_owned();
        let answer = self
            .handle
            .call(context, true, move |m| m.memory_push(&text))
            .await?;
        if answer.get("stored").and_then(Value::as_bool) == Some(false) {
            return Err(MemoryBackendError::Store {
                code: "membrane_push_refused".into(),
                message: format!("membrane did not store the memory: {answer}"),
            });
        }
        Ok(answer)
    }
}

fn pushed_id(answer: &Value) -> Option<String> {
    ["id", "memory_id", "memoryId"]
        .iter()
        .find_map(|key| answer.get(*key).and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn count(value: &Value) -> u32 {
    match value {
        Value::Number(n) => n.as_u64().unwrap_or(0).min(u32::MAX as u64) as u32,
        Value::Bool(true) => 1,
        _ => 0,
    }
}

fn tier_of(raw: &str) -> MemoryTier {
    MemoryTier::parse(raw).unwrap_or(MemoryTier::Semantic)
}

fn entry_from_item(item: &MemoryItem) -> MemoryEntry {
    MemoryEntry {
        id: item.id.clone(),
        tier: tier_of(&item.tier),
        content: item.content.clone().unwrap_or_default(),
        keywords: Vec::new(),
        score: item.score.unwrap_or(0.0),
        created_at: String::new(),
        access_count: count(&item.access),
        embedding: None,
        scope_id: item
            .scope
            .as_deref()
            .map(normalize_scope)
            .unwrap_or_else(|| "global".to_owned()),
    }
}

fn in_scopes(item: &MemoryItem, scopes: &[String]) -> bool {
    if scopes.is_empty() {
        return true;
    }
    match item.scope.as_deref().map(normalize_scope) {
        None => true,
        Some(scope) => scope == "global" || scopes.iter().any(|s| normalize_scope(s) == scope),
    }
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    rightkit_time::DateTime::from_unix(secs, 0)
        .map(|dt| dt.format_rfc3339())
        .unwrap_or_default()
}

fn pushed_entry(answer: &Value, content: &str, keywords: Vec<String>) -> MemoryEntry {
    MemoryEntry {
        id: pushed_id(answer).unwrap_or_else(|| "membrane-pushed".to_owned()),
        tier: MemoryTier::Semantic,
        content: content.to_owned(),
        keywords,
        score: 1.0,
        created_at: now_rfc3339(),
        access_count: 0,
        embedding: None,
        scope_id: "global".to_owned(),
    }
}

#[async_trait]
impl MemoryBackend for MembraneMemoryBackend {
    fn binding(&self) -> &BackendBinding {
        &self.binding
    }

    async fn activity_json(
        &self,
        _: &CallContext,
        _: usize,
    ) -> Result<JsonValue, MemoryBackendError> {
        Err(not_available("activity_json"))
    }

    async fn delete(&self, context: &CallContext, id: &str) -> Result<bool, MemoryBackendError> {
        let id = id.to_owned();
        let deleted = self
            .handle
            .call(context, true, move |m| m.memory_delete(&id))
            .await?;
        Ok(deleted.deleted)
    }

    async fn embedder_dim(&self, _: &CallContext) -> Result<usize, MemoryBackendError> {
        Err(not_available("embedder_dim"))
    }

    /// `memory list` carries no content, so full entries cannot be built; use `list` for rows
    /// and `search`/`recall_scored` for content.
    async fn entries(
        &self,
        _: &CallContext,
        _: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryBackendError> {
        Err(not_available("entries (memory list has no content)"))
    }

    async fn get_full(
        &self,
        _: &CallContext,
        _: &str,
    ) -> Result<FullMemoryRecord, MemoryBackendError> {
        Err(not_available("get_full"))
    }

    #[allow(clippy::too_many_arguments)]
    async fn log_recall_with_replay(
        &self,
        context: &CallContext,
        _: &str,
        _: Option<&str>,
        _: usize,
        _: usize,
        _: usize,
        _: usize,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(), MemoryBackendError> {
        context.check()
    }

    async fn metrics_json(&self, context: &CallContext) -> Result<JsonValue, MemoryBackendError> {
        let status = self.handle.call(context, false, |m| m.status()).await?;
        Ok(status.raw)
    }

    async fn put(
        &self,
        context: &CallContext,
        _name: &str,
        content: &str,
        _scope: &str,
        _tier: MemoryTier,
    ) -> Result<MemoryId, MemoryBackendError> {
        let answer = self.push(context, content).await?;
        Ok(pushed_id(&answer)
            .or_else(|| (!_name.is_empty()).then(|| _name.to_owned()))
            .unwrap_or_else(|| "membrane-pushed".to_owned()))
    }

    async fn recall_scored(
        &self,
        context: &CallContext,
        query: &str,
        limit: usize,
        scopes: &[String],
    ) -> Result<Vec<(MemoryEntry, Score)>, MemoryBackendError> {
        // Over-fetch because scope filtering happens after the CLI answers.
        let fetch = if scopes.is_empty() {
            limit
        } else {
            limit.saturating_mul(3).clamp(1, 200)
        };
        let query = query.to_owned();
        let found = self
            .handle
            .call(context, false, move |m| {
                m.memory_search(&query, &[], fetch, None)
            })
            .await?;
        Ok(found
            .items
            .iter()
            .filter(|item| in_scopes(item, scopes))
            .take(limit)
            .map(|item| {
                let entry = entry_from_item(item);
                let score = entry.score;
                (entry, score)
            })
            .collect())
    }

    async fn record_injections(
        &self,
        context: &CallContext,
        _: &[String],
    ) -> Result<(), MemoryBackendError> {
        context.check()
    }

    async fn remember(
        &self,
        context: &CallContext,
        content: &str,
        keywords: Vec<String>,
    ) -> Result<MemoryEntry, MemoryBackendError> {
        let answer = self.push(context, content).await?;
        Ok(pushed_entry(&answer, content, keywords))
    }

    /// Membrane resolves scope server-side; `global` is always a valid member of the chain.
    async fn scopes(&self, context: &CallContext) -> Result<Vec<String>, MemoryBackendError> {
        context.check()?;
        Ok(vec!["global".to_owned()])
    }

    async fn search(
        &self,
        context: &CallContext,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryBackendError> {
        let query = query.to_owned();
        let found = self
            .handle
            .call(context, false, move |m| {
                m.memory_search(&query, &[], limit, None)
            })
            .await?;
        Ok(found.items.iter().map(entry_from_item).collect())
    }

    async fn try_put(
        &self,
        context: &CallContext,
        name: &str,
        content: &str,
        scope: &str,
        tier: MemoryTier,
    ) -> Result<MemoryId, MemoryBackendError> {
        self.put(context, name, content, scope, tier).await
    }

    async fn list(
        &self,
        context: &CallContext,
        _scope: Option<&str>,
    ) -> Result<Vec<MemoryListRow>, MemoryBackendError> {
        const LIST_LIMIT: usize = 1000;
        let list = self
            .handle
            .call(context, false, |m| m.memory_list(LIST_LIMIT, None))
            .await?;
        Ok(list
            .items
            .iter()
            .map(|item| MemoryListRow {
                id: item.id.clone(),
                tier: tier_of(&item.tier),
                chars: item.chars.unwrap_or(0) as usize,
                access_count: count(&item.access),
                inject_count: count(&item.inject),
            })
            .collect())
    }

    async fn remember_consolidated(
        &self,
        context: &CallContext,
        _name: &str,
        content: &str,
        keywords: Vec<String>,
        _threshold: Score,
    ) -> Result<Option<MemoryEntry>, MemoryBackendError> {
        let probe = content.to_owned();
        let found = self
            .handle
            .call(context, false, move |m| {
                m.memory_search(&probe, &[], 1, None)
            })
            .await?;
        let wanted = content.trim();
        if found
            .items
            .iter()
            .any(|item| item.content.as_deref().map(str::trim) == Some(wanted))
        {
            return Ok(None);
        }
        let answer = self.push(context, content).await?;
        Ok(Some(pushed_entry(&answer, content, keywords)))
    }
}

/// True when no Pulse binary was found, so context runs off (`OFF_REASON`).
pub fn context_is_off() -> bool {
    MembraneHandle::shared().is_off()
}

/// Connect to Pulse's Membrane. Pulse absent yields the explicit unavailable backend
/// (`off: pulse not installed`); sessions keep running without memory.
pub fn connect_from_environment() -> Result<Arc<dyn MemoryBackend>, MemoryBackendError> {
    let handle = MembraneHandle::shared();
    if handle.is_off() {
        return Ok(Arc::new(
            crate::backend::unavailable::UnavailableMemoryBackend::new(
                "backend_unavailable",
                OFF_REASON,
            ),
        ));
    }
    Ok(Arc::new(MembraneMemoryBackend::new(handle)))
}

/// Membrane's repository scope slug for a workspace root. Reproduced exactly from
/// `membrane_federation::root::canonical_repository_id`
/// (`context/engine/crates/membrane-federation/src/root.rs:148`) so ids stay byte-identical.
pub fn canonical_repository_scope(path: &Path) -> String {
    let mut value = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = value.strip_prefix("//?/") {
        value = rest.to_owned();
    }
    value = value.replace([':', '/'], "-");
    let value = value.trim_matches('-');
    if value.is_empty() {
        return "global".to_owned();
    }
    let bytes = value.as_bytes();
    if bytes.len() > 1 && bytes[0].is_ascii_lowercase() && bytes[1] == b'-' {
        let mut normalized = value.to_owned();
        normalized.replace_range(..1, &value[..1].to_ascii_uppercase());
        normalized
    } else {
        value.to_owned()
    }
}

/// Adapt operations over `membrane adapt ...`. `status`, `adjudicate_taste` and `mine_session`
/// have no CLI equivalent and return an explicit error.
#[derive(Clone)]
pub struct AdaptClient {
    handle: MembraneHandle,
}

impl AdaptClient {
    pub fn new(handle: MembraneHandle) -> Self {
        Self { handle }
    }

    fn unavailable(operation: &str) -> MembraneError {
        MembraneError::NotAvailable(format!("{operation}: {NOT_AVAILABLE_VIA_CLI}"))
    }

    /// `adapt observe` with an `AdaptObservationRequestV1` document.
    pub fn observe_execution(&self, request: Value) -> Result<Value, MembraneError> {
        if !request.is_object() {
            return Err(MembraneError::Usage(
                "Adapt observation request must be a JSON object".into(),
            ));
        }
        self.handle.call_blocking(|m| m.adapt_observe(&request))
    }

    pub fn status(&self, _scope: impl Into<String>) -> Result<Value, MembraneError> {
        Err(Self::unavailable("adapt status"))
    }

    pub fn review_taste(
        &self,
        input: std::path::PathBuf,
        installation_id: impl Into<String>,
        canonical_pool_sha256: Option<String>,
        created_at: impl Into<String>,
    ) -> Result<Value, MembraneError> {
        let (installation_id, created_at) = (installation_id.into(), created_at.into());
        self.handle.call_blocking(|m| {
            m.adapt_taste_review(
                &input,
                &installation_id,
                &created_at,
                canonical_pool_sha256.as_deref(),
            )
        })
    }

    pub fn adjudicate_taste(
        &self,
        _manifest: std::path::PathBuf,
        _decisions: std::path::PathBuf,
        _validated_at: impl Into<String>,
    ) -> Result<Value, MembraneError> {
        Err(Self::unavailable("adapt adjudicate_taste"))
    }

    pub fn apply_insights(
        &self,
        input: std::path::PathBuf,
        installation_id: impl Into<String>,
        dry_run: bool,
    ) -> Result<Value, MembraneError> {
        let installation_id = installation_id.into();
        self.handle
            .call_blocking(|m| m.adapt_apply_insights(&input, &installation_id, dry_run))
    }

    pub fn apply_taste(
        &self,
        manifest: std::path::PathBuf,
        dry_run: bool,
    ) -> Result<Value, MembraneError> {
        self.handle
            .call_blocking(|m| m.adapt_apply_manifest(&manifest, dry_run))
    }

    pub fn recall(
        &self,
        query: impl Into<String>,
        scope: impl Into<String>,
        k: usize,
        dimensions: Vec<String>,
    ) -> Result<Value, MembraneError> {
        let (query, scope) = (query.into(), scope.into());
        self.handle
            .call_blocking(|m| m.adapt_recall(&query, k, &scope, &dimensions))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::types::path_to_scope;

    fn item(scope: Option<&str>, tier: &str) -> MemoryItem {
        MemoryItem {
            id: "m1".into(),
            tier: tier.into(),
            chars: Some(4),
            access: serde_json::json!(3),
            inject: serde_json::json!(true),
            scope: scope.map(str::to_owned),
            score: Some(0.5),
            content: Some("text".into()),
        }
    }

    #[test]
    fn repository_scope_matches_the_old_membrane_slug() {
        assert_eq!(
            canonical_repository_scope(Path::new(r"\\?\d:\Claude\myapp")),
            "D--Claude-myapp"
        );
        assert_eq!(
            canonical_repository_scope(Path::new("/Users/a/b")),
            "Users-a-b"
        );
        assert_eq!(canonical_repository_scope(Path::new("/")), "global");
    }

    #[test]
    fn item_maps_without_losing_content_or_tier() {
        let entry = entry_from_item(&item(Some("d--Claude"), "Episodic"));
        assert_eq!(entry.content, "text");
        assert_eq!(entry.tier, MemoryTier::Episodic);
        assert_eq!(entry.scope_id, "D--Claude");
        assert_eq!(entry.access_count, 3);
        assert_eq!(
            entry_from_item(&item(None, "durable")).tier,
            MemoryTier::Semantic
        );
    }

    #[test]
    fn scope_filter_keeps_global_and_unscoped() {
        let scopes = vec!["D--Claude".to_owned()];
        assert!(in_scopes(&item(None, "x"), &scopes));
        assert!(in_scopes(&item(Some("global"), "x"), &scopes));
        assert!(in_scopes(&item(Some("d--Claude"), "x"), &scopes));
        assert!(!in_scopes(&item(Some("D--Other"), "x"), &scopes));
    }

    #[test]
    fn unknown_mutation_outcomes_are_commit_unknown() {
        let timeout = MembraneError::Timeout(std::time::Duration::from_secs(1));
        assert_eq!(
            map_error(timeout, true).kind(),
            MemoryBackendErrorKind::CommitUnknown
        );
        let timeout = MembraneError::Timeout(std::time::Duration::from_secs(1));
        assert_eq!(
            map_error(timeout, false).kind(),
            MemoryBackendErrorKind::Timeout
        );
        assert_eq!(
            map_error(MembraneError::Unavailable("x".into()), true).kind(),
            MemoryBackendErrorKind::BackendUnavailable
        );
    }

    #[test]
    fn path_scope_helper_is_still_exported() {
        assert_eq!(path_to_scope("D:/x"), "D--x");
    }
}
