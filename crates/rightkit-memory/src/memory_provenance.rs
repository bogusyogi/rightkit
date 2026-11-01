//! RightKit-owned causal provenance keyed by backend memory id.
//!
//! A pending record is recorded before content write, so an interrupted write
//! remains conservative & cannot expose unlabeled content.

use crate::backend::{CallContext, MemoryBackend, MemoryBackendError, MemoryTier};
use rightkit_agent_policy::{ContentOrigin, ContentProvenance};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const PROVENANCE_SCOPE: &str = "__coderight_provenance";

pub fn is_provenance_id(memory_id: &str) -> bool {
    memory_id == PROVENANCE_SCOPE || memory_id.starts_with(&format!("{PROVENANCE_SCOPE}/"))
}

fn provenance_storage_id(memory_id: &str) -> String {
    format!("{PROVENANCE_SCOPE}/{memory_id}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryCausalOrigin {
    Legacy,
    SessionOrigin,
    ToolDerived,
    WebDerived,
    McpDerived,
}

impl MemoryCausalOrigin {
    fn from_content_origin(origin: ContentOrigin) -> Self {
        match origin {
            ContentOrigin::SessionOrigin => Self::SessionOrigin,
            ContentOrigin::ToolDerived => Self::ToolDerived,
            ContentOrigin::WebDerived => Self::WebDerived,
            ContentOrigin::McpDerived => Self::McpDerived,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::SessionOrigin => "session-origin",
            Self::ToolDerived => "tool-derived",
            Self::WebDerived => "web-derived",
            Self::McpDerived => "mcp-derived",
        }
    }

    /// Conservative conflict precedence for causal metadata only. This is not
    /// semantic context ranking and never orders or admits retrieved content.
    fn risk_rank(&self) -> u8 {
        match self {
            // Unknown legacy metadata must not be upgraded while a rewrite is pending.
            Self::Legacy => 3,
            Self::SessionOrigin => 0,
            Self::ToolDerived => 1,
            Self::WebDerived | Self::McpDerived => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryCausalProvenance {
    pub origin: MemoryCausalOrigin,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub approved: bool,
}

impl Default for MemoryCausalProvenance {
    fn default() -> Self {
        Self {
            origin: MemoryCausalOrigin::Legacy,
            source: "legacy-unlabelled".into(),
            session_id: None,
            approved: false,
        }
    }
}

impl MemoryCausalProvenance {
    pub fn from_tool_context(
        provenance: &ContentProvenance,
        session_id: &str,
        approved: bool,
    ) -> Self {
        Self {
            origin: MemoryCausalOrigin::from_content_origin(provenance.origin),
            source: provenance.source.clone(),
            session_id: (!session_id.trim().is_empty()).then(|| session_id.to_string()),
            approved,
        }
    }

    pub fn origin_label(&self) -> &'static str {
        self.origin.label()
    }

    pub fn trusted_operator(source: impl Into<String>) -> Self {
        Self {
            origin: MemoryCausalOrigin::SessionOrigin,
            source: source.into(),
            session_id: None,
            approved: true,
        }
    }

    /// Compare only pending-vs-committed provenance safety. Membrane owns
    /// semantic ranking/admission; this value must never be used for recall.
    fn risk_rank(&self) -> u8 {
        self.origin.risk_rank()
    }
}

#[derive(Clone)]
pub struct MemoryProvenanceStore {
    backend: Option<Arc<dyn MemoryBackend>>,
    unavailable_reason: Option<Arc<str>>,
    write_lock: Arc<tokio::sync::Mutex<()>>,
    /// False once the backend reports it cannot read a sidecar row back (the
    /// Membrane CLI has no `get_full`). Sidecar writes are then skipped so a
    /// content write is neither blocked nor polluted with provenance rows;
    /// reads fail closed to legacy/untrusted.
    sidecar_supported: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProvenanceRecord {
    committed: Option<MemoryCausalProvenance>,
    pending: Option<MemoryCausalProvenance>,
}

impl MemoryProvenanceStore {
    pub fn new() -> Self {
        Self {
            backend: None,
            unavailable_reason: Some(Arc::from("memory backend was not injected")),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
            sidecar_supported: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    pub fn from_backend(backend: Arc<dyn MemoryBackend>) -> Self {
        Self {
            backend: Some(backend),
            unavailable_reason: None,
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
            sidecar_supported: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            backend: None,
            unavailable_reason: Some(Arc::from(reason.into())),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
            sidecar_supported: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    fn ensure_available(&self) -> Result<(), String> {
        self.unavailable_reason
            .as_deref()
            .map(|reason| Err(reason.to_string()))
            .unwrap_or(Ok(()))
    }

    fn sidecar_supported(&self) -> bool {
        self.sidecar_supported
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn backend(&self) -> Result<&Arc<dyn MemoryBackend>, String> {
        self.backend.as_ref().ok_or_else(|| {
            self.unavailable_reason
                .as_deref()
                .unwrap_or("memory backend unavailable")
                .to_string()
        })
    }

    async fn load_record(
        &self,
        context: &CallContext,
        memory_id: &str,
    ) -> Result<ProvenanceRecord, String> {
        let backend = self.backend()?;
        match backend
            .get_full(context, &provenance_storage_id(memory_id))
            .await
        {
            Ok(record) => serde_json::from_str(&record.content)
                .map_err(|error| format!("memory provenance decode failed: {error}")),
            // The Membrane CLI cannot read rows back by id; there is no sidecar.
            Err(error) if error.code() == "not_available_via_membrane_cli" => {
                self.sidecar_supported
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                Ok(ProvenanceRecord::default())
            }
            // A missing sidecar is the only expected read miss. Cancellation,
            // unavailability, store failures, & malformed responses must remain
            // visible so callers cannot silently treat an unknown write as legacy.
            Err(error)
                if error.code() == "not_found"
                    || error.message().to_ascii_lowercase().contains("not found")
                    || error
                        .message()
                        .to_ascii_lowercase()
                        .contains("no memory with id") =>
            {
                Ok(ProvenanceRecord::default())
            }
            Err(error) => Err(format!("memory provenance load failed: {error}")),
        }
    }

    async fn save_record(
        &self,
        context: &CallContext,
        memory_id: &str,
        record: &ProvenanceRecord,
    ) -> Result<(), String> {
        let backend = self.backend()?;
        if !self.sidecar_supported() {
            return Ok(());
        }
        let content = serde_json::to_string(record)
            .map_err(|error| format!("memory provenance encode failed: {error}"))?;
        backend
            .put(
                context,
                memory_id,
                &content,
                PROVENANCE_SCOPE,
                MemoryTier::Semantic,
            )
            .await
            .map(|_| ())
            .map_err(|error| format!("memory provenance persist failed: {error}"))
    }

    async fn begin_write(
        &self,
        context: &CallContext,
        memory_id: &str,
        provenance: MemoryCausalProvenance,
    ) -> Result<(), String> {
        let mut record = self.load_record(context, memory_id).await?;
        if record.committed.is_none() {
            record.committed = Some(MemoryCausalProvenance::default());
        }
        record.pending = Some(provenance);
        self.save_record(context, memory_id, &record).await
    }

    async fn complete_write(
        &self,
        context: &CallContext,
        memory_id: &str,
        provenance: &MemoryCausalProvenance,
    ) -> Result<(), String> {
        let mut record = self.load_record(context, memory_id).await?;
        if !self.sidecar_supported() {
            return Ok(());
        }
        if record.pending.as_ref() != Some(provenance) {
            return Err("memory provenance pending record changed unexpectedly".into());
        }
        record.committed = Some(provenance.clone());
        record.pending = None;
        self.save_record(context, memory_id, &record).await
    }

    async fn abort_write(
        &self,
        context: &CallContext,
        memory_id: &str,
        provenance: &MemoryCausalProvenance,
    ) -> Result<(), String> {
        let mut record = self.load_record(context, memory_id).await?;
        if record.pending.as_ref() == Some(provenance) {
            record.pending = None;
        }
        self.save_record(context, memory_id, &record).await
    }

    /// Backend-aware variant used by migrated routes. The provenance intent is
    /// recorded before invoking the trait operation and remains conservative
    /// if finalization cannot complete.
    pub async fn write_with_backend<T, F, Fut>(
        &self,
        context: CallContext,
        memory_id: &str,
        provenance: &MemoryCausalProvenance,
        write: F,
    ) -> Result<T, String>
    where
        F: FnOnce(&CallContext) -> Fut,
        Fut: std::future::Future<Output = Result<T, MemoryBackendError>> + Send,
        T: Send,
    {
        // Keep intent, content write, & finalization in one critical section.
        // Releasing between these steps lets two writes to one ID exchange
        // pending provenance and commit a label for the wrong content.
        let _guard = self.write_lock.lock().await;
        self.begin_write(&context, memory_id, provenance.clone())
            .await?;
        match write(&context).await {
            Ok(value) => {
                if let Err(error) = self.complete_write(&context, memory_id, provenance).await {
                    tracing::warn!(memory_id, %error, "memory provenance remained pending");
                }
                Ok(value)
            }
            Err(error) => {
                if let Err(cleanup_error) = self.abort_write(&context, memory_id, provenance).await
                {
                    return Err(format!(
                        "{error}; memory provenance cleanup failed conservatively: {cleanup_error}"
                    ));
                }
                Err(error.to_string())
            }
        }
    }

    /// Missing, malformed, or unavailable metadata is always legacy/untrusted.
    pub async fn effective_for(
        &self,
        context: &CallContext,
        memory_id: &str,
    ) -> MemoryCausalProvenance {
        let record = match self.load_record(context, memory_id).await {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(memory_id, %error, "memory provenance unavailable; failing closed");
                return MemoryCausalProvenance {
                    origin: MemoryCausalOrigin::Legacy,
                    source: "provenance-unavailable".into(),
                    session_id: None,
                    approved: false,
                };
            }
        };
        let committed = record.committed;
        let pending = record.pending;
        match (committed, pending) {
            (Some(committed), Some(pending)) if committed.risk_rank() > pending.risk_rank() => {
                committed
            }
            (_, Some(pending)) => pending,
            (Some(committed), None) => committed,
            (None, None) => MemoryCausalProvenance::default(),
        }
    }

    pub async fn remove(&self, context: &CallContext, memory_id: &str) -> Result<(), String> {
        self.ensure_available()?;
        self.backend()?
            .delete(context, &provenance_storage_id(memory_id))
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory stand-in for the Membrane-backed store: only id -> content rows.
    struct MapBackend {
        binding: crate::backend::BackendBinding,
        rows: std::sync::Mutex<std::collections::HashMap<String, String>>,
    }

    impl MapBackend {
        fn new() -> Self {
            Self {
                binding: crate::backend::BackendBinding::new(
                    crate::backend::BackendIdentity::explicit("test-map", 1, 1, None),
                ),
                rows: Default::default(),
            }
        }
        fn unsupported<T>() -> Result<T, MemoryBackendError> {
            Err(MemoryBackendError::backend_unavailable(
                "not used by this test",
            ))
        }
    }

    #[async_trait::async_trait]
    impl MemoryBackend for MapBackend {
        fn binding(&self) -> &crate::backend::BackendBinding {
            &self.binding
        }
        async fn activity_json(
            &self,
            _: &CallContext,
            _: usize,
        ) -> Result<serde_json::Value, MemoryBackendError> {
            Self::unsupported()
        }
        async fn delete(&self, _: &CallContext, id: &str) -> Result<bool, MemoryBackendError> {
            Ok(self.rows.lock().unwrap().remove(id).is_some())
        }
        async fn embedder_dim(&self, _: &CallContext) -> Result<usize, MemoryBackendError> {
            Self::unsupported()
        }
        async fn entries(
            &self,
            _: &CallContext,
            _: usize,
        ) -> Result<Vec<crate::backend::MemoryEntry>, MemoryBackendError> {
            Self::unsupported()
        }
        async fn get_full(
            &self,
            _: &CallContext,
            id: &str,
        ) -> Result<crate::backend::FullMemoryRecord, MemoryBackendError> {
            match self.rows.lock().unwrap().get(id) {
                Some(content) => Ok(crate::backend::FullMemoryRecord {
                    id: id.to_string(),
                    content: content.clone(),
                    access_count: 0,
                    lifecycle: None,
                }),
                None => Err(MemoryBackendError::Invalid {
                    code: "not_found".into(),
                    message: "memory not found".into(),
                }),
            }
        }
        #[allow(clippy::too_many_arguments)]
        async fn log_recall_with_replay(
            &self,
            _: &CallContext,
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
            Self::unsupported()
        }
        async fn metrics_json(
            &self,
            _: &CallContext,
        ) -> Result<serde_json::Value, MemoryBackendError> {
            Self::unsupported()
        }
        async fn put(
            &self,
            _: &CallContext,
            name: &str,
            content: &str,
            scope: &str,
            _: MemoryTier,
        ) -> Result<String, MemoryBackendError> {
            // Real backends id a row `{scope}/{name}`; `load_record` reads that id.
            let id = format!("{scope}/{name}");
            self.rows
                .lock()
                .unwrap()
                .insert(id.clone(), content.to_string());
            Ok(id)
        }
        async fn recall_scored(
            &self,
            _: &CallContext,
            _: &str,
            _: usize,
            _: &[String],
        ) -> Result<Vec<(crate::backend::MemoryEntry, f64)>, MemoryBackendError> {
            Self::unsupported()
        }
        async fn record_injections(
            &self,
            _: &CallContext,
            _: &[String],
        ) -> Result<(), MemoryBackendError> {
            Self::unsupported()
        }
        async fn remember(
            &self,
            _: &CallContext,
            _: &str,
            _: Vec<String>,
        ) -> Result<crate::backend::MemoryEntry, MemoryBackendError> {
            Self::unsupported()
        }
        async fn scopes(&self, _: &CallContext) -> Result<Vec<String>, MemoryBackendError> {
            Self::unsupported()
        }
        async fn search(
            &self,
            _: &CallContext,
            _: &str,
            _: usize,
        ) -> Result<Vec<crate::backend::MemoryEntry>, MemoryBackendError> {
            Self::unsupported()
        }
        async fn try_put(
            &self,
            context: &CallContext,
            name: &str,
            content: &str,
            scope: &str,
            tier: MemoryTier,
        ) -> Result<String, MemoryBackendError> {
            self.put(context, name, content, scope, tier).await
        }
        async fn list(
            &self,
            _: &CallContext,
            _: Option<&str>,
        ) -> Result<Vec<crate::backend::MemoryListRow>, MemoryBackendError> {
            Self::unsupported()
        }
        async fn remember_consolidated(
            &self,
            _: &CallContext,
            _: &str,
            _: &str,
            _: Vec<String>,
            _: f64,
        ) -> Result<Option<crate::backend::MemoryEntry>, MemoryBackendError> {
            Self::unsupported()
        }
    }

    #[tokio::test]
    async fn unavailable_backend_is_explicitly_untrusted() {
        let provenance = MemoryProvenanceStore::new();
        assert_eq!(
            provenance
                .effective_for(
                    &CallContext::after(std::time::Duration::from_secs(30)),
                    "global/legacy",
                )
                .await,
            MemoryCausalProvenance {
                origin: MemoryCausalOrigin::Legacy,
                source: "provenance-unavailable".into(),
                session_id: None,
                approved: false,
            }
        );
    }

    /// CRA-02 negative control: "Cached snapshot accepting revoked identity fails."
    ///
    /// `effective_for` is called twice for the same memory id within what a caller
    /// treats as one request-local lookup. Between the two calls the committed
    /// identity is revoked (re-written as unapproved/legacy). The second call must
    /// observe the revocation, not silently keep serving the first (approved)
    /// result. If a request-local snapshot were ever memoized past the point of a
    /// write, this test fails.
    #[tokio::test]
    async fn reused_snapshot_still_rejects_revoked_identity() {
        let backend: Arc<dyn MemoryBackend> = Arc::new(MapBackend::new());
        let provenance = MemoryProvenanceStore::from_backend(backend);
        let ctx = CallContext::unbounded();
        let memory_id = "revocation-check";

        let approved = MemoryCausalProvenance {
            origin: MemoryCausalOrigin::SessionOrigin,
            source: "operator-approved".into(),
            session_id: Some("session-a".into()),
            approved: true,
        };
        provenance
            .begin_write(&ctx, memory_id, approved.clone())
            .await
            .unwrap();
        provenance
            .complete_write(&ctx, memory_id, &approved)
            .await
            .unwrap();

        let first = provenance.effective_for(&ctx, memory_id).await;
        assert_eq!(
            first, approved,
            "first lookup should see the approved identity"
        );

        // Identity is revoked/altered after the first lookup, still inside the
        // same logical request the caller is reusing a snapshot for.
        let revoked = MemoryCausalProvenance {
            origin: MemoryCausalOrigin::Legacy,
            source: "revoked".into(),
            session_id: Some("session-a".into()),
            approved: false,
        };
        provenance
            .begin_write(&ctx, memory_id, revoked.clone())
            .await
            .unwrap();
        provenance
            .complete_write(&ctx, memory_id, &revoked)
            .await
            .unwrap();

        let second = provenance.effective_for(&ctx, memory_id).await;
        assert_ne!(
            second, first,
            "a reused snapshot must not keep authorizing a revoked identity"
        );
        assert!(
            !second.approved,
            "revoked identity must not remain approved after reuse"
        );
        assert_eq!(second, revoked);
    }

    /// CRA-02 negative control: "TTL identity cache present fails."
    ///
    /// `MemoryProvenanceStore` must hold no time-based identity cache: identity
    /// state comes only from `backend` via a fresh read in `load_record`. This
    /// struct literal enumerates every field the store is allowed to have; adding
    /// a cache/TTL field (e.g. `identity_cache`, `cached_until`) makes this test
    /// stop compiling, which is a failure under the acceptance registry's
    /// "fails if the protection is removed" requirement.
    #[test]
    fn provenance_store_has_no_ttl_identity_cache_field() {
        let _store = MemoryProvenanceStore {
            backend: None,
            unavailable_reason: None,
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
            sidecar_supported: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
    }
}
