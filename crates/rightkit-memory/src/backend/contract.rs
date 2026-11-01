//! Async contract shared by all the host app memory implementations.

use crate::backend::error::MemoryBackendError;
use crate::backend::types::{
    BackendBinding, FullMemoryRecord, JsonValue, MemoryEntry, MemoryId, MemoryListRow, MemoryTier,
    Score,
};
use async_trait::async_trait;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Per-call deadline and cancellation state used by concrete backends.
#[derive(Clone, Debug)]
pub struct CallContext {
    deadline: Option<Instant>,
    cancellation: CancellationToken,
}

impl CallContext {
    pub fn unbounded() -> Self {
        Self {
            deadline: None,
            cancellation: CancellationToken::new(),
        }
    }

    pub fn after(duration: Duration) -> Self {
        Self {
            deadline: Some(Instant::now() + duration),
            cancellation: CancellationToken::new(),
        }
    }

    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn check(&self) -> Result<(), MemoryBackendError> {
        if self.cancellation.is_cancelled() {
            return Err(MemoryBackendError::cancelled("memory operation cancelled"));
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(MemoryBackendError::timeout(
                "memory operation deadline elapsed",
            ));
        }
        Ok(())
    }
}

/// The single memory surface consumed by the host app routes, tools, and Legion adapters.
///
/// Methods retain source-compatible arguments and return aliases while making failure
/// explicit for service, embedded, and future migration implementations.
#[async_trait]
pub trait MemoryBackend: Send + Sync {
    fn binding(&self) -> &BackendBinding;

    async fn activity_json(
        &self,
        context: &CallContext,
        limit: usize,
    ) -> Result<JsonValue, MemoryBackendError>;
    async fn delete(&self, context: &CallContext, id: &str) -> Result<bool, MemoryBackendError>;
    async fn embedder_dim(&self, context: &CallContext) -> Result<usize, MemoryBackendError>;
    async fn entries(
        &self,
        context: &CallContext,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryBackendError>;
    async fn get_full(
        &self,
        context: &CallContext,
        id: &str,
    ) -> Result<FullMemoryRecord, MemoryBackendError>;
    #[allow(clippy::too_many_arguments)]
    async fn log_recall_with_replay(
        &self,
        context: &CallContext,
        timestamp: &str,
        scope: Option<&str>,
        query_chars: usize,
        hit_count: usize,
        full_chars: usize,
        injected_chars: usize,
        source: &str,
        query_excerpt: Option<&str>,
        client: Option<&str>,
        model: Option<&str>,
        session: Option<&str>,
        turn: Option<&str>,
        task: Option<&str>,
        actor: Option<&str>,
        mode: &str,
        replay_hits: &str,
        replay_context: &str,
    ) -> Result<(), MemoryBackendError>;
    async fn metrics_json(&self, context: &CallContext) -> Result<JsonValue, MemoryBackendError>;
    async fn put(
        &self,
        context: &CallContext,
        name: &str,
        content: &str,
        scope: &str,
        tier: MemoryTier,
    ) -> Result<MemoryId, MemoryBackendError>;
    async fn recall_scored(
        &self,
        context: &CallContext,
        query: &str,
        limit: usize,
        scopes: &[String],
    ) -> Result<Vec<(MemoryEntry, Score)>, MemoryBackendError>;
    async fn record_injections(
        &self,
        context: &CallContext,
        ids: &[String],
    ) -> Result<(), MemoryBackendError>;
    async fn remember(
        &self,
        context: &CallContext,
        content: &str,
        keywords: Vec<String>,
    ) -> Result<MemoryEntry, MemoryBackendError>;
    async fn scopes(&self, context: &CallContext) -> Result<Vec<String>, MemoryBackendError>;
    async fn search(
        &self,
        context: &CallContext,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryBackendError>;
    async fn try_put(
        &self,
        context: &CallContext,
        name: &str,
        content: &str,
        scope: &str,
        tier: MemoryTier,
    ) -> Result<MemoryId, MemoryBackendError>;

    /// Production `/list` operation recorded by CR-000, retained for route migration.
    async fn list(
        &self,
        context: &CallContext,
        scope: Option<&str>,
    ) -> Result<Vec<MemoryListRow>, MemoryBackendError>;

    /// Production consolidation operation recorded by CR-000, retained for learning migration.
    async fn remember_consolidated(
        &self,
        context: &CallContext,
        name: &str,
        content: &str,
        keywords: Vec<String>,
        threshold: Score,
    ) -> Result<Option<MemoryEntry>, MemoryBackendError>;
}
