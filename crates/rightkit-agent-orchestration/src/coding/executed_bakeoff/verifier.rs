use std::collections::BTreeSet;

use async_trait::async_trait;

use super::runner::Verifier;
use super::types::{
    BakeoffError, ExecutionRecord, StableTaskId, VerificationContract, VerificationVerdict,
};
use crate::coding::failure::FailureKind;

/// Deterministic verifier for private corpora whose acceptance contract is a
/// set of exact artifact digests. It never trusts an executor success claim.
#[derive(Debug, Default)]
pub struct ArtifactDigestVerifier;

#[async_trait]
impl Verifier for ArtifactDigestVerifier {
    fn evaluator_id(&self) -> Option<&str> {
        Some("artifact_digest_verifier")
    }

    fn evaluator_version(&self) -> Option<&str> {
        Some("1")
    }

    async fn verify(
        &self,
        _task_id: &StableTaskId,
        contract: &VerificationContract,
        execution: &ExecutionRecord,
    ) -> Result<VerificationVerdict, BakeoffError> {
        let mut confirmed_defects = BTreeSet::new();
        let mut useful_tool_calls = BTreeSet::new();

        for requirement in &contract.required_artifacts {
            let satisfying = execution.artifacts.iter().find(|artifact| {
                artifact.name == requirement.artifact_name
                    && digest_hex(artifact.body.as_bytes()) == requirement.sha256_hex
            });
            if let Some(artifact) = satisfying {
                if let Some(tool_call_id) = &artifact.produced_by_tool_call_id {
                    useful_tool_calls.insert(tool_call_id.clone());
                }
            } else {
                confirmed_defects.insert(requirement.defect_id.clone());
            }
        }

        let passed = execution.execution_failure.is_none() && confirmed_defects.is_empty();
        Ok(VerificationVerdict {
            passed,
            failure_kind: if passed {
                None
            } else {
                execution
                    .execution_failure
                    .clone()
                    .or(Some(FailureKind::WrongOutput))
            },
            confirmed_defect_ids: confirmed_defects.into_iter().collect(),
            useful_tool_call_ids: useful_tool_calls.into_iter().collect(),
        })
    }
}

fn digest_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(64);
    for byte in rightkit_crypto::sha256(bytes) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
