//! Receipt-bound seam for post-edit safeguards.
//!
//! Typed mutations describe what landed on disk; host-issued receipts prove
//! which tool result produced that change.  Post-edit consumers must use this
//! seam instead of treating an unbound mutation list as evidence.
//!
//! FM-179 reconciliation (A12): a second, independent receipt-binding
//! mechanism exists in `crates/engine/src/mutation_diagnostics.rs`
//! (`MutationDiagnostics::seal_and_evaluate`, called from
//! `engine_tool_dispatch_single.rs`), which binds the live `mutationReceipt`
//! (mutationId/epoch) as part of the workspace-transaction seal/reconcile
//! protocol on the daemon's native mutation path. This module's
//! `ToolReceipt`/`bind_mutations_to_receipt` seam binds a different receipt
//! shape (host tool-call receipts) and is not called from anywhere in the
//! live request path. Decision: the Membrane `mutationReceipt` mechanism is
//! authoritative for the live daemon mutation path; this seam is retained
//! but dormant rather than force-merged into the Membrane seal — the two
//! bind different evidence to different receipt types, and building a
//! correct adapter between them (or deciding this seam should run as a
//! defense-in-depth pre-check before Membrane sealing) is a Sage-level
//! ownership call, not one to make unilaterally inside this file. See
//! `docs/canon/daemon-master-canon-atoms.md`.

use crate::assurance_broker::{ToolReceipt, HOST_ISSUER, SCHEMA};
use rightkit_agent_tools::coding_tools::{ToolResult, WorkspaceMutation};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MutationReceiptBindingError {
    Missing {
        tool_call_id: String,
    },
    Invalid {
        tool_call_id: String,
        reason: &'static str,
    },
}

impl fmt::Display for MutationReceiptBindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { tool_call_id } => write!(
                f,
                "post-edit safeguard has no host mutation receipt for tool call {tool_call_id}"
            ),
            Self::Invalid {
                tool_call_id,
                reason,
            } => write!(
                f,
                "post-edit safeguard rejected mutation receipt for tool call {tool_call_id}: {reason}"
            ),
        }
    }
}

/// Bind typed workspace mutations to one host-issued receipt.
///
/// Read-only results return an empty slice without requiring a receipt.  Once
/// a result carries mutations, missing, stale, or inconsistent receipt data
/// is an error: callers must fail closed rather than derive evidence from
/// arguments, Git state, or current files.
pub(crate) fn bind_mutations_to_receipt<'a>(
    tool_call_id: &str,
    result: &'a ToolResult,
    receipt: Option<&ToolReceipt>,
) -> Result<&'a [WorkspaceMutation], MutationReceiptBindingError> {
    if result.mutations.is_empty() {
        return Ok(&[]);
    }

    let Some(receipt) = receipt else {
        return Err(MutationReceiptBindingError::Missing {
            tool_call_id: tool_call_id.to_owned(),
        });
    };

    let invalid = |reason| MutationReceiptBindingError::Invalid {
        tool_call_id: tool_call_id.to_owned(),
        reason,
    };
    if receipt.schema != SCHEMA {
        return Err(invalid("receipt schema is not host mutation schema"));
    }
    if receipt.issuer_id != HOST_ISSUER {
        return Err(invalid("receipt issuer is not the Agent host"));
    }
    if receipt.tool_call_id != tool_call_id {
        return Err(invalid("receipt tool call does not match mutation result"));
    }
    if receipt.invalidated {
        return Err(invalid("receipt was invalidated"));
    }
    let expected_exit_status = if result.is_error { 1 } else { 0 };
    if receipt.exit_status != expected_exit_status {
        return Err(invalid("receipt status does not match mutation result"));
    }
    if receipt.output_digest != digest(result.content.as_bytes()) {
        return Err(invalid("receipt output is not bound to mutation result"));
    }

    Ok(&result.mutations)
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(bytes);
    format!("sha256:{}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_tools::coding_tools::{WorkspaceMutation, WorkspaceMutationKind};
    use std::path::PathBuf;

    fn base_receipt(tool_call_id: &str, result: &ToolResult) -> ToolReceipt {
        ToolReceipt {
            schema: SCHEMA.into(),
            task_id: "task-1".into(),
            turn_id: "turn-1".into(),
            tool_call_id: tool_call_id.into(),
            issuer_id: HOST_ISSUER.into(),
            issuer_capability_digest: "digest".into(),
            tool_class: "write_file".into(),
            operation: "write".into(),
            scope: vec!["workspace".into()],
            input_digest: "input".into(),
            output_digest: digest(result.content.as_bytes()),
            exit_status: if result.is_error { 1 } else { 0 },
            started_at: "2026-01-01T00:00:00Z".into(),
            completed_at: "2026-01-01T00:00:01Z".into(),
            source_generation_id: None,
            signature_or_mac: "sig".into(),
            criterion_ids: vec![],
            invalidated: false,
        }
    }

    fn mutated_result(is_error: bool) -> ToolResult {
        let result = if is_error {
            ToolResult::error("boom")
        } else {
            ToolResult::ok("ok")
        };
        result.with_mutations(vec![WorkspaceMutation {
            path: PathBuf::from("/workspace/file.txt"),
            kind: WorkspaceMutationKind::Modified,
            previous_content: Some("old".into()),
        }])
    }

    // Negative case: read-only (no-mutation) results never require a receipt,
    // even when no receipt is provided at all.
    #[test]
    fn read_only_result_bypasses_receipt_requirement() {
        let result = ToolResult::ok("nothing changed");
        let bound = bind_mutations_to_receipt("call-1", &result, None);
        assert_eq!(bound, Ok(&[][..]));
    }

    // Negative case: a failed/no-op edit that reports no mutations also
    // bypasses the receipt requirement -- safeguards must not trigger.
    #[test]
    fn failed_no_op_edit_bypasses_receipt_requirement() {
        let result = ToolResult::error("could not write");
        assert!(result.mutations.is_empty());
        let bound = bind_mutations_to_receipt("call-1", &result, None);
        assert_eq!(bound, Ok(&[][..]));
    }

    #[test]
    fn missing_receipt_for_mutated_result_is_error() {
        let result = mutated_result(false);
        let err = bind_mutations_to_receipt("call-1", &result, None).unwrap_err();
        assert_eq!(
            err,
            MutationReceiptBindingError::Missing {
                tool_call_id: "call-1".into(),
            }
        );
    }

    #[test]
    fn valid_receipt_binds_mutations_for_successful_edit() {
        let result = mutated_result(false);
        let receipt = base_receipt("call-1", &result);
        let bound = bind_mutations_to_receipt("call-1", &result, Some(&receipt)).unwrap();
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].path, PathBuf::from("/workspace/file.txt"));
    }

    #[test]
    fn receipt_with_wrong_schema_is_rejected() {
        let result = mutated_result(false);
        let mut receipt = base_receipt("call-1", &result);
        receipt.schema = "other.schema.v1".into();
        let err = bind_mutations_to_receipt("call-1", &result, Some(&receipt)).unwrap_err();
        assert_eq!(
            err,
            MutationReceiptBindingError::Invalid {
                tool_call_id: "call-1".into(),
                reason: "receipt schema is not host mutation schema",
            }
        );
    }

    #[test]
    fn receipt_with_wrong_issuer_is_rejected() {
        let result = mutated_result(false);
        let mut receipt = base_receipt("call-1", &result);
        receipt.issuer_id = "some-other-issuer".into();
        let err = bind_mutations_to_receipt("call-1", &result, Some(&receipt)).unwrap_err();
        assert_eq!(
            err,
            MutationReceiptBindingError::Invalid {
                tool_call_id: "call-1".into(),
                reason: "receipt issuer is not the Agent host",
            }
        );
    }

    #[test]
    fn receipt_for_different_tool_call_is_rejected() {
        let result = mutated_result(false);
        let receipt = base_receipt("call-other", &result);
        let err = bind_mutations_to_receipt("call-1", &result, Some(&receipt)).unwrap_err();
        assert_eq!(
            err,
            MutationReceiptBindingError::Invalid {
                tool_call_id: "call-1".into(),
                reason: "receipt tool call does not match mutation result",
            }
        );
    }

    #[test]
    fn invalidated_receipt_is_rejected() {
        let result = mutated_result(false);
        let mut receipt = base_receipt("call-1", &result);
        receipt.invalidated = true;
        let err = bind_mutations_to_receipt("call-1", &result, Some(&receipt)).unwrap_err();
        assert_eq!(
            err,
            MutationReceiptBindingError::Invalid {
                tool_call_id: "call-1".into(),
                reason: "receipt was invalidated",
            }
        );
    }

    #[test]
    fn exit_status_mismatch_is_rejected() {
        let result = mutated_result(false);
        let mut receipt = base_receipt("call-1", &result);
        receipt.exit_status = 1;
        let err = bind_mutations_to_receipt("call-1", &result, Some(&receipt)).unwrap_err();
        assert_eq!(
            err,
            MutationReceiptBindingError::Invalid {
                tool_call_id: "call-1".into(),
                reason: "receipt status does not match mutation result",
            }
        );
    }

    #[test]
    fn error_result_requires_matching_exit_status_one() {
        let result = mutated_result(true);
        let receipt = base_receipt("call-1", &result);
        let bound = bind_mutations_to_receipt("call-1", &result, Some(&receipt)).unwrap();
        assert_eq!(bound.len(), 1);
    }

    #[test]
    fn output_digest_mismatch_is_rejected() {
        let result = mutated_result(false);
        let mut receipt = base_receipt("call-1", &result);
        receipt.output_digest = "sha256:deadbeef".into();
        let err = bind_mutations_to_receipt("call-1", &result, Some(&receipt)).unwrap_err();
        assert_eq!(
            err,
            MutationReceiptBindingError::Invalid {
                tool_call_id: "call-1".into(),
                reason: "receipt output is not bound to mutation result",
            }
        );
    }

    #[test]
    fn read_only_result_ignores_even_valid_receipt() {
        let result = ToolResult::ok("nothing changed");
        let receipt = base_receipt("call-1", &result);
        let bound = bind_mutations_to_receipt("call-1", &result, Some(&receipt)).unwrap();
        assert!(bound.is_empty());
    }

    #[test]
    fn successful_mutation_binding_is_idempotent_across_calls() {
        let result = mutated_result(false);
        let receipt = base_receipt("call-1", &result);
        let first = bind_mutations_to_receipt("call-1", &result, Some(&receipt)).unwrap();
        let second = bind_mutations_to_receipt("call-1", &result, Some(&receipt)).unwrap();
        assert_eq!(first, second);
    }
}
