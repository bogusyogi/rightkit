//! Shared, implementation-neutral conformance runner.

use crate::backend::contract::{CallContext, MemoryBackend};
use crate::backend::error::{MemoryBackendError, MemoryBackendErrorKind};
use crate::backend::types::{BackendKind, MemoryTier};
use async_trait::async_trait;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const REQUIRED_OPERATIONS: &[&str] = &[
    "activity_json",
    "delete",
    "embedder_dim",
    "entries",
    "get_full",
    "log_recall_with_replay",
    "metrics_json",
    "put",
    "recall_scored",
    "record_injections",
    "remember",
    "scopes",
    "search",
    "try_put",
    "list",
    "remember_consolidated",
];

pub const REQUIRED_ERROR_KINDS: &[MemoryBackendErrorKind] = &[
    MemoryBackendErrorKind::Unavailable,
    MemoryBackendErrorKind::Invalid,
    MemoryBackendErrorKind::Denied,
    MemoryBackendErrorKind::Timeout,
    MemoryBackendErrorKind::Cancelled,
    MemoryBackendErrorKind::Incompatible,
    MemoryBackendErrorKind::Store,
    MemoryBackendErrorKind::Internal,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationObservation {
    pub operation: String,
    pub error_kind: Option<MemoryBackendErrorKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceFailure {
    pub operation: String,
    /// `None` means the operation was required to succeed.
    pub expected: Option<MemoryBackendErrorKind>,
    pub observed: Option<MemoryBackendErrorKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedErrorObservation {
    pub error_name: String,
    pub expected: MemoryBackendErrorKind,
    pub observed: Option<MemoryBackendErrorKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceReport {
    pub backend: BackendKind,
    pub operations: Vec<OperationObservation>,
    pub failures: Vec<ConformanceFailure>,
    pub typed_errors: Vec<TypedErrorObservation>,
    pub cancellation_kind: Option<MemoryBackendErrorKind>,
    pub deadline_kind: Option<MemoryBackendErrorKind>,
    pub local_open_count: usize,
    pub no_local_open: bool,
}

impl ConformanceReport {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
            && self.operations.len() == REQUIRED_OPERATIONS.len()
            && self.typed_errors.len() == REQUIRED_ERROR_KINDS.len()
            && REQUIRED_ERROR_KINDS.iter().all(|expected| {
                self.typed_errors
                    .iter()
                    .any(|case| case.expected == *expected && case.observed == Some(*expected))
            })
            && self.cancellation_kind == Some(MemoryBackendErrorKind::Cancelled)
            && self.deadline_kind == Some(MemoryBackendErrorKind::Timeout)
            && self.no_local_open
    }
}

/// Concrete backends supply their own boundary fixtures through this contract.
/// CR-010 deliberately does not construct storage or service clients.
#[async_trait]
pub trait ConformanceFactory: Send + Sync {
    type Backend: MemoryBackend;

    async fn create(&self) -> Result<Self::Backend, MemoryBackendError>;
    fn local_open_count(&self) -> usize;
    async fn typed_error_cases(&self) -> Vec<TypedErrorObservation>;
}

fn observe<T>(operation: &str, result: Result<T, MemoryBackendError>) -> OperationObservation {
    OperationObservation {
        operation: operation.to_string(),
        error_kind: result.err().map(|error| error.kind()),
    }
}

fn expected_error(
    operation: &str,
    expected: Option<MemoryBackendErrorKind>,
    observed: Option<MemoryBackendErrorKind>,
) -> Option<ConformanceFailure> {
    (observed != expected).then(|| ConformanceFailure {
        operation: operation.to_string(),
        expected,
        observed,
    })
}

/// Build one backend through supplied factory, then invoke every operation.
/// Unexpected operation errors fail the report; only missing-row `get_full`
/// Store/Invalid is accepted as an explicit fresh-backend fixture outcome.
/// Concrete factories own typed error fixtures, while cancellation, deadline,
/// operation, & no-local-open probes remain shared here.
pub async fn run_conformance<F>(factory: &F) -> Result<ConformanceReport, MemoryBackendError>
where
    F: ConformanceFactory,
{
    let backend = factory.create().await?;
    let context = CallContext::unbounded();
    let mut operations = Vec::with_capacity(REQUIRED_OPERATIONS.len());

    operations.push(observe(
        "activity_json",
        backend.activity_json(&context, 20).await,
    ));
    operations.push(observe(
        "delete",
        backend.delete(&context, "conformance-missing").await,
    ));
    operations.push(observe(
        "embedder_dim",
        backend.embedder_dim(&context).await,
    ));
    operations.push(observe("entries", backend.entries(&context, 20).await));
    operations.push(observe(
        "get_full",
        backend.get_full(&context, "conformance-missing").await,
    ));
    operations.push(observe(
        "log_recall_with_replay",
        backend
            .log_recall_with_replay(
                &context,
                "1970-01-01T00:00:00Z",
                None,
                0,
                0,
                0,
                0,
                "conformance",
                None,
                None,
                None,
                None,
                None,
                None,
                Some("actor"),
                "test",
                "[]",
                "[]",
            )
            .await,
    ));
    operations.push(observe(
        "metrics_json",
        backend.metrics_json(&context).await,
    ));
    operations.push(observe(
        "put",
        backend
            .put(
                &context,
                "conformance",
                "fixture",
                "global",
                MemoryTier::Semantic,
            )
            .await,
    ));
    operations.push(observe(
        "recall_scored",
        backend.recall_scored(&context, "fixture", 5, &[]).await,
    ));
    operations.push(observe(
        "record_injections",
        backend.record_injections(&context, &[]).await,
    ));
    operations.push(observe(
        "remember",
        backend
            .remember(&context, "fixture", vec!["conformance".into()])
            .await,
    ));
    operations.push(observe("scopes", backend.scopes(&context).await));
    operations.push(observe(
        "search",
        backend.search(&context, "fixture", 5).await,
    ));
    operations.push(observe(
        "try_put",
        backend
            .try_put(
                &context,
                "conformance",
                "fixture",
                "global",
                MemoryTier::Semantic,
            )
            .await,
    ));
    operations.push(observe("list", backend.list(&context, None).await));
    operations.push(observe(
        "remember_consolidated",
        backend
            .remember_consolidated(&context, "conformance", "fixture", vec![], 0.1)
            .await,
    ));

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let cancelled = CallContext::unbounded().with_cancellation(cancellation);
    let cancellation_kind = backend
        .metrics_json(&cancelled)
        .await
        .err()
        .map(|error| error.kind());

    let expired = CallContext::after(Duration::ZERO);
    let deadline_kind = backend
        .metrics_json(&expired)
        .await
        .err()
        .map(|error| error.kind());

    let mut failures = Vec::new();
    for observation in &operations {
        let accepted_missing_row = observation.operation == "get_full"
            && matches!(
                observation.error_kind,
                Some(MemoryBackendErrorKind::Store | MemoryBackendErrorKind::Invalid)
            );
        if observation.error_kind.is_some() && !accepted_missing_row {
            failures.push(ConformanceFailure {
                operation: observation.operation.clone(),
                expected: None,
                observed: observation.error_kind,
            });
        }
    }
    failures.extend(expected_error(
        "cancellation",
        Some(MemoryBackendErrorKind::Cancelled),
        cancellation_kind,
    ));
    failures.extend(expected_error(
        "deadline",
        Some(MemoryBackendErrorKind::Timeout),
        deadline_kind,
    ));

    let typed_errors = factory.typed_error_cases().await;
    for case in &typed_errors {
        if case.observed != Some(case.expected) {
            failures.push(ConformanceFailure {
                operation: format!("typed_error:{}", case.error_name),
                expected: Some(case.expected),
                observed: case.observed,
            });
        }
    }

    let local_open_count = factory.local_open_count();
    let no_local_open = backend.binding().kind() != BackendKind::Service || local_open_count == 0;
    if !no_local_open {
        failures.push(ConformanceFailure {
            operation: "no_local_open".into(),
            expected: Some(MemoryBackendErrorKind::Unavailable),
            observed: Some(MemoryBackendErrorKind::Store),
        });
    }

    Ok(ConformanceReport {
        backend: backend.binding().kind(),
        operations,
        failures,
        typed_errors,
        cancellation_kind,
        deadline_kind,
        local_open_count,
        no_local_open,
    })
}
