//! Change detection + FM-179 receipt binding (Pi "change detection" leg).
//!
//! The fan-out only ever runs on mutations that actually landed on disk.
//! Change detection reads the typed `WorkspaceMutation` records carried on
//! `ToolResult::mutations` — never `arguments_json` — matching the A2
//! mutation contract the rest of the engine already enforces.
//!
//! Donor shape: Pi fires `tool_result` events for the edit/write tools and
//! the `turn_end` event carries the turn's `toolResults`
//! (`packages/coding-agent/src/core/extensions/types.ts`); the mutation
//! records here are the the host app equivalent of those turn tool results.
//! Pi additionally serializes per-canonical-path mutations
//! (`file-mutation-queue.ts`); on the the host app side that ordering already
//! happened before the results reach this module.

use std::collections::HashMap;
use std::path::PathBuf;

use rightkit_agent_tools::coding_tools::{ToolResult, WorkspaceMutation, WorkspaceMutationKind};

use crate::assurance_broker::ToolReceipt;
use crate::post_edit::{bind_mutations_to_receipt, MutationReceiptBindingError};

/// How the fan-out may trust the mutation evidence it is handed.
///
/// `post_edit.rs`'s `ToolReceipt` seam and the live Membrane
/// `mutationReceipt` seam bind different evidence (see the FM-179 decision
/// note at the top of `post_edit.rs`); this enum keeps the fan-out honest in
/// both worlds instead of force-merging them.
#[derive(Clone, Copy, Debug)]
pub enum ReceiptBinding<'a> {
    /// Consume `ToolResult::mutations` directly. Use ONLY where the mutations
    /// are already bound by the authoritative live mechanism (the daemon's
    /// Membrane workspace-transaction receipts).
    Unchecked,
    /// Verify every mutated result against the FM-179 host receipt index and
    /// fail closed on any binding error: the fan-out is fenced and runs
    /// nothing until the evidence is bound.
    Enforced {
        receipts: &'a HashMap<String, ToolReceipt>,
    },
}

/// A receipt-binding failure, converted from `post_edit`'s crate-internal
/// error type so the fan-out's own API stays self-contained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptBindingError {
    pub tool_call_id: String,
    pub reason: String,
}

impl From<MutationReceiptBindingError> for ReceiptBindingError {
    fn from(error: MutationReceiptBindingError) -> Self {
        match error {
            MutationReceiptBindingError::Missing { tool_call_id } => Self {
                tool_call_id,
                reason: "no host mutation receipt for a mutated tool result".into(),
            },
            MutationReceiptBindingError::Invalid {
                tool_call_id,
                reason,
            } => Self {
                tool_call_id,
                reason: reason.to_string(),
            },
        }
    }
}

/// The result of change detection over one turn's tool results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MutationCollection {
    /// Every workspace mutation that landed this turn (all kinds, already
    /// deduplicated by the shared normalizer when used through
    /// `run_verify_fanout`).
    pub mutations: Vec<WorkspaceMutation>,
    /// Non-empty only under `ReceiptBinding::Enforced` when a mutated result
    /// had no valid receipt. The fan-out is fenced: it runs nothing on
    /// unbound evidence.
    pub binding_errors: Vec<ReceiptBindingError>,
    /// Tool-call IDs whose results carried mutations.
    pub tool_call_ids: Vec<String>,
}

impl MutationCollection {
    pub fn is_fenced(&self) -> bool {
        !self.binding_errors.is_empty()
    }

    /// Paths that still exist and can be verified: `Created`/`Modified`
    /// records and rename targets. `Deleted` records (and the rename
    /// source) are excluded — there is nothing left to verify, and any
    /// finding about them would be stale on arrival.
    pub fn verifiable_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self
            .mutations
            .iter()
            .filter(|m| {
                matches!(
                    m.kind,
                    WorkspaceMutationKind::Created
                        | WorkspaceMutationKind::Modified
                        | WorkspaceMutationKind::Renamed { .. }
                )
            })
            .map(|m| m.path.clone())
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }
}

/// Collect one turn's mutations under the given receipt-binding policy.
///
/// Under `Unchecked`, mutations are taken as-is. Under `Enforced`, each
/// result is passed through FM-179's `bind_mutations_to_receipt`: read-only
/// and failed/no-op results bypass the requirement (their contract), mutated
/// results must present a valid host receipt or the collection records a
/// binding error and the caller must fence the fan-out.
pub fn collect_mutations(
    turn_results: &HashMap<String, ToolResult>,
    binding: ReceiptBinding<'_>,
) -> MutationCollection {
    let mut collected = MutationCollection::default();
    // Deterministic order: iterate tool-call IDs sorted, matching the way
    // the engine aggregates one turn's results.
    let mut call_ids: Vec<&String> = turn_results.keys().collect();
    call_ids.sort();
    for call_id in call_ids {
        let result = &turn_results[call_id];
        match binding {
            ReceiptBinding::Unchecked => {
                if !result.mutations.is_empty() {
                    collected.tool_call_ids.push((*call_id).clone());
                    collected.mutations.extend(result.mutations.iter().cloned());
                }
            }
            ReceiptBinding::Enforced { receipts } => {
                let bound = bind_mutations_to_receipt(call_id, result, receipts.get(call_id));
                match bound {
                    Ok(mutations) => {
                        if !mutations.is_empty() {
                            collected.tool_call_ids.push((*call_id).clone());
                            collected.mutations.extend(mutations.iter().cloned());
                        }
                    }
                    Err(error) => collected.binding_errors.push(error.into()),
                }
            }
        }
    }
    collected
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::assurance_broker::{HOST_ISSUER, SCHEMA};

    fn digest(bytes: &[u8]) -> String {
        let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
        hasher.update(bytes);
        format!("sha256:{}", hasher.finalize())
    }

    fn mutated(path: &str) -> ToolResult {
        ToolResult::ok("ok").with_mutations(vec![WorkspaceMutation {
            path: PathBuf::from(path),
            kind: WorkspaceMutationKind::Modified,
            previous_content: Some("old".into()),
        }])
    }

    // Mirrors the `base_receipt` shape from post_edit.rs's own tests so the
    // binding leg is exercised against exactly the receipt contract it
    // validates.
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

    #[test]
    fn unchecked_binding_collects_all_mutations_in_call_id_order() {
        let mut results = HashMap::new();
        results.insert("b".to_string(), mutated("/w/b.rs"));
        results.insert("a".to_string(), mutated("/w/a.rs"));
        let collected = collect_mutations(&results, ReceiptBinding::Unchecked);
        assert!(!collected.is_fenced());
        assert_eq!(
            collected.tool_call_ids,
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(collected.mutations.len(), 2);
        assert_eq!(
            collected.verifiable_paths(),
            vec![PathBuf::from("/w/a.rs"), PathBuf::from("/w/b.rs")]
        );
    }

    #[test]
    fn unchecked_binding_ignores_read_only_results() {
        let mut results = HashMap::new();
        results.insert("a".to_string(), ToolResult::ok("nothing changed"));
        let collected = collect_mutations(&results, ReceiptBinding::Unchecked);
        assert!(collected.mutations.is_empty());
        assert!(collected.tool_call_ids.is_empty());
        assert!(collected.verifiable_paths().is_empty());
    }

    #[test]
    fn deleted_mutations_are_not_verifiable_paths() {
        let mut results = HashMap::new();
        results.insert(
            "a".to_string(),
            ToolResult::ok("removed").with_mutations(vec![WorkspaceMutation {
                path: PathBuf::from("/w/gone.rs"),
                kind: WorkspaceMutationKind::Deleted,
                previous_content: Some("old".into()),
            }]),
        );
        results.insert("b".to_string(), mutated("/w/kept.rs"));
        let collected = collect_mutations(&results, ReceiptBinding::Unchecked);
        assert_eq!(collected.mutations.len(), 2, "both records are kept");
        assert_eq!(
            collected.verifiable_paths(),
            vec![PathBuf::from("/w/kept.rs")]
        );
    }

    #[test]
    fn rename_target_is_verifiable() {
        let mut results = HashMap::new();
        results.insert(
            "a".to_string(),
            ToolResult::ok("renamed").with_mutations(vec![WorkspaceMutation {
                path: PathBuf::from("/w/new.rs"),
                kind: WorkspaceMutationKind::Renamed {
                    from: PathBuf::from("/w/old.rs"),
                },
                previous_content: Some("old".into()),
            }]),
        );
        let collected = collect_mutations(&results, ReceiptBinding::Unchecked);
        assert_eq!(
            collected.verifiable_paths(),
            vec![PathBuf::from("/w/new.rs")]
        );
    }

    #[test]
    fn enforced_binding_accepts_a_valid_host_receipt() {
        let result = mutated("/w/a.rs");
        let receipt = base_receipt("call-1", &result);
        let mut receipts = HashMap::new();
        receipts.insert("call-1".to_string(), receipt);
        let mut results = HashMap::new();
        results.insert("call-1".to_string(), result);
        let collected = collect_mutations(
            &results,
            ReceiptBinding::Enforced {
                receipts: &receipts,
            },
        );
        assert!(!collected.is_fenced());
        assert_eq!(collected.mutations.len(), 1);
    }

    #[test]
    fn enforced_binding_fences_a_mutated_result_without_a_receipt() {
        let mut results = HashMap::new();
        results.insert("call-1".to_string(), mutated("/w/a.rs"));
        let receipts: HashMap<String, ToolReceipt> = HashMap::new();
        let collected = collect_mutations(
            &results,
            ReceiptBinding::Enforced {
                receipts: &receipts,
            },
        );
        assert!(collected.is_fenced());
        assert_eq!(
            collected.binding_errors,
            vec![ReceiptBindingError {
                tool_call_id: "call-1".into(),
                reason: "no host mutation receipt for a mutated tool result".into(),
            }]
        );
    }

    #[test]
    fn enforced_binding_fences_a_receipt_for_a_different_tool_call() {
        let result = mutated("/w/a.rs");
        let receipt = base_receipt("call-other", &result);
        let mut receipts = HashMap::new();
        receipts.insert("call-1".to_string(), receipt);
        let mut results = HashMap::new();
        results.insert("call-1".to_string(), result);
        let collected = collect_mutations(
            &results,
            ReceiptBinding::Enforced {
                receipts: &receipts,
            },
        );
        assert!(collected.is_fenced());
        assert_eq!(collected.binding_errors.len(), 1);
        assert_eq!(collected.binding_errors[0].tool_call_id, "call-1");
        assert_eq!(
            collected.binding_errors[0].reason,
            "receipt tool call does not match mutation result"
        );
    }

    #[test]
    fn enforced_binding_never_requires_a_receipt_for_read_only_results() {
        let mut results = HashMap::new();
        results.insert("call-1".to_string(), ToolResult::ok("nothing changed"));
        let receipts: HashMap<String, ToolReceipt> = HashMap::new();
        let collected = collect_mutations(
            &results,
            ReceiptBinding::Enforced {
                receipts: &receipts,
            },
        );
        assert!(!collected.is_fenced());
        assert!(collected.mutations.is_empty());
    }
}
