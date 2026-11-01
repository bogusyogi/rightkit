//! Explicit no-memory backend used when Hub is inactive or incompatible.
//!
//! This is a typed refusal surface, never a local fallback.  It keeps the host app
//! alive for non-memory routes while every memory operation remains fail-closed.

use crate::backend::contract::{CallContext, MemoryBackend};
use crate::backend::error::MemoryBackendError;
use crate::backend::types::{
    BackendBinding, BackendIdentity, FullMemoryRecord, JsonValue, MemoryEntry, MemoryId,
    MemoryListRow, MemoryTier, Score,
};
use async_trait::async_trait;

pub struct UnavailableMemoryBackend {
    binding: BackendBinding,
    error: MemoryBackendError,
}

impl UnavailableMemoryBackend {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            binding: BackendBinding::new(BackendIdentity::service(
                "membrane-unavailable",
                1,
                1,
                None,
            )),
            error: MemoryBackendError::BackendUnavailable {
                code: code.into(),
                message: message.into(),
            },
        }
    }

    fn failure(&self) -> MemoryBackendError {
        self.error.clone()
    }
}

#[async_trait]
impl MemoryBackend for UnavailableMemoryBackend {
    fn binding(&self) -> &BackendBinding {
        &self.binding
    }

    async fn activity_json(
        &self,
        _: &CallContext,
        _: usize,
    ) -> Result<JsonValue, MemoryBackendError> {
        Err(self.failure())
    }
    async fn delete(&self, _: &CallContext, _: &str) -> Result<bool, MemoryBackendError> {
        Err(self.failure())
    }
    async fn embedder_dim(&self, _: &CallContext) -> Result<usize, MemoryBackendError> {
        Err(self.failure())
    }
    async fn entries(
        &self,
        _: &CallContext,
        _: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryBackendError> {
        Err(self.failure())
    }
    async fn get_full(
        &self,
        _: &CallContext,
        _: &str,
    ) -> Result<FullMemoryRecord, MemoryBackendError> {
        Err(self.failure())
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
        Err(self.failure())
    }

    async fn metrics_json(&self, _: &CallContext) -> Result<JsonValue, MemoryBackendError> {
        Err(self.failure())
    }
    async fn put(
        &self,
        _: &CallContext,
        _: &str,
        _: &str,
        _: &str,
        _: MemoryTier,
    ) -> Result<MemoryId, MemoryBackendError> {
        Err(self.failure())
    }
    async fn recall_scored(
        &self,
        _: &CallContext,
        _: &str,
        _: usize,
        _: &[String],
    ) -> Result<Vec<(MemoryEntry, Score)>, MemoryBackendError> {
        Err(self.failure())
    }
    async fn record_injections(
        &self,
        _: &CallContext,
        _: &[String],
    ) -> Result<(), MemoryBackendError> {
        Err(self.failure())
    }
    async fn remember(
        &self,
        _: &CallContext,
        _: &str,
        _: Vec<String>,
    ) -> Result<MemoryEntry, MemoryBackendError> {
        Err(self.failure())
    }
    async fn scopes(&self, _: &CallContext) -> Result<Vec<String>, MemoryBackendError> {
        Err(self.failure())
    }
    async fn search(
        &self,
        _: &CallContext,
        _: &str,
        _: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryBackendError> {
        Err(self.failure())
    }
    async fn try_put(
        &self,
        _: &CallContext,
        _: &str,
        _: &str,
        _: &str,
        _: MemoryTier,
    ) -> Result<MemoryId, MemoryBackendError> {
        Err(self.failure())
    }
    async fn list(
        &self,
        _: &CallContext,
        _: Option<&str>,
    ) -> Result<Vec<MemoryListRow>, MemoryBackendError> {
        Err(self.failure())
    }
    async fn remember_consolidated(
        &self,
        _: &CallContext,
        _: &str,
        _: &str,
        _: Vec<String>,
        _: Score,
    ) -> Result<Option<MemoryEntry>, MemoryBackendError> {
        Err(self.failure())
    }
}
