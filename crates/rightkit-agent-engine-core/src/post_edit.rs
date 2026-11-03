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
