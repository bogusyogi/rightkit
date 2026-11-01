//! Native Live Diagnostics transaction seam for workspace mutations.
//!
//! Engine retains no transport dependency. Hosts bind this trait to their
//! authenticated Membrane-native client; failures stop result publication.

use async_trait::async_trait;
use serde_json::Value;
use std::fmt;
use std::path::Path;

/// Engine-owned classification for failures around a host diagnostics
/// transaction. Hosts retain their existing string error boundary, while the
/// dispatch seam keeps phase information instead of flattening every failure
/// into an indistinguishable result message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationDiagnosticsFailureKind {
    Unavailable,
    Begin,
    Abort,
    SealAndEvaluate,
}

impl MutationDiagnosticsFailureKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Begin => "begin",
            Self::Abort => "abort",
            Self::SealAndEvaluate => "seal-and-evaluate",
        }
    }
}

/// A typed engine failure with host detail retained for durable tool output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationDiagnosticsFailure {
    pub kind: MutationDiagnosticsFailureKind,
    pub detail: String,
}

impl MutationDiagnosticsFailure {
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            kind: MutationDiagnosticsFailureKind::Unavailable,
            detail: detail.into(),
        }
    }

    pub fn host(kind: MutationDiagnosticsFailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for MutationDiagnosticsFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "mutation-diagnostics:{}: {}",
            self.kind.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for MutationDiagnosticsFailure {}

#[async_trait]
pub trait MutationDiagnostics: Send + Sync {
    async fn begin(
        &self,
        workspace_root: &Path,
        operation_id: &str,
        metadata: Value,
    ) -> Result<(), String>;
    async fn abort(&self, workspace_root: &Path, operation_id: &str) -> Result<(), String>;
    async fn seal_and_evaluate(
        &self,
        workspace_root: &Path,
        operation_id: &str,
        changed_paths: Vec<std::path::PathBuf>,
        metadata: Value,
    ) -> Result<(), String>;
}
