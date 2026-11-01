//! Ephemeral discovery state for acknowledgement-authorized Pull reuse.
//!
//! This cache never authorizes an effect. Every suppression or prefix reuse
//! revalidates its cached pending publication through persisted H10 plus
//! current H9 identities. Cache loss, response loss, compaction, & restart
//! therefore leave evidence eligible.

use crate::catalog::{ContextCatalog, PendingPullPublicationV1};
use crate::pull::delivery_acknowledgement::suppression_eligible;
use rightkit_context_protocol::LoadedContextIdentitiesV1;
use rightkit_memory_core::planner::ContextPacketV1;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

const MAX_SESSIONS: usize = 256;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuppressionReceiptV1 {
    pub schema_version: u32,
    pub evidence_id: String,
    pub reason: String,
    pub source_hash: String,
    pub restore_eligibility: Vec<&'static str>,
}

#[derive(Clone, Debug)]
struct SelectedPublication {
    publication: PendingPullPublicationV1,
    packet: Value,
    acknowledgement_hint: Option<PendingPullPublicationV1>,
}

#[derive(Default)]
struct DeliveryLedger {
    sessions: BTreeMap<(String, String), SelectedPublication>,
}

static LEDGER: OnceLock<Mutex<DeliveryLedger>> = OnceLock::new();

fn ledger() -> &'static Mutex<DeliveryLedger> {
    LEDGER.get_or_init(|| Mutex::new(DeliveryLedger::default()))
}

/// Cache a Pull-recorded pending publication only to find a candidate on a
/// later request. Persistence & H9 validation still decide eligibility.
pub fn record_selected_packet(selected_content: &Value, publication: PendingPullPublicationV1) {
    let Ok(packet) = serde_json::from_value::<ContextPacketV1>(selected_content.clone()) else {
        return;
    };
    if !publication.is_complete() {
        return;
    }
    let key = (
        publication.repository_id.clone(),
        publication.session_id.clone(),
    );
    let Ok(mut guard) = ledger().lock() else {
        return;
    };
    if guard.sessions.len() >= MAX_SESSIONS && !guard.sessions.contains_key(&key) {
        if let Some(first) = guard.sessions.keys().next().cloned() {
            guard.sessions.remove(&first);
        }
    }
    guard.sessions.insert(
        key,
        SelectedPublication {
            publication,
            packet: serde_json::to_value(packet).expect("context packet serializes"),
            acknowledgement_hint: None,
        },
    );
}

/// Mark only an H10/H9-validated pending publication as discoverable. This is
/// deliberately crate-visible: acknowledgement bridge invokes it only after
/// `acknowledge_delivery` succeeds. This hint alone grants nothing.
pub(crate) fn record_acknowledged_publication(publication: PendingPullPublicationV1) {
    let key = (
        publication.repository_id.clone(),
        publication.session_id.clone(),
    );
    let Ok(mut guard) = ledger().lock() else {
        return;
    };
    let Some(selected) = guard.sessions.get_mut(&key) else {
        return;
    };
    if selected.publication == publication {
        selected.acknowledgement_hint = Some(publication);
    }
}

fn acknowledged_selected(
    catalog: &ContextCatalog,
    repository_id: &str,
    session_id: &str,
    loaded: &LoadedContextIdentitiesV1,
) -> Option<SelectedPublication> {
    let selected = ledger().lock().ok().and_then(|guard| {
        guard
            .sessions
            .get(&(repository_id.to_owned(), session_id.to_owned()))
            .cloned()
    })?;
    (selected.acknowledgement_hint.as_ref() == Some(&selected.publication)
        && selected.publication.repository_id == repository_id
        && selected.publication.session_id == session_id
        && suppression_eligible(catalog, &selected.publication, loaded).unwrap_or(false))
    .then_some(selected)
}

fn same_evidence(
    current: &rightkit_memory_core::planner::BlockV1,
    previous: &rightkit_memory_core::planner::BlockV1,
) -> bool {
    current.id == previous.id
        && current.provider == previous.provider
        && current.source_ref == previous.source_ref
        && current.source_hash == previous.source_hash
}

/// Suppress only evidence from exact persisted acknowledgement retained in
/// current H9. Unknown cache, rejected/absent acknowledgement, compaction,
/// restart, response loss, & explicit refresh all fail open to evidence.
pub fn suppress_packet(
    packet: &mut ContextPacketV1,
    catalog: &ContextCatalog,
    repository_id: &str,
    session_id: &str,
    loaded: &LoadedContextIdentitiesV1,
    explicit_refresh: bool,
) -> Vec<SuppressionReceiptV1> {
    if explicit_refresh {
        return Vec::new();
    }
    let Some(selected) = acknowledged_selected(catalog, repository_id, session_id, loaded) else {
        return Vec::new();
    };
    let Ok(previous) = serde_json::from_value::<ContextPacketV1>(selected.packet) else {
        return Vec::new();
    };
    let mut receipts = Vec::new();
    packet.blocks.retain(|block| {
        if block.protected {
            return true;
        }
        if previous
            .blocks
            .iter()
            .any(|prior| same_evidence(block, prior))
        {
            receipts.push(SuppressionReceiptV1 {
                schema_version: 1,
                evidence_id: block.id.clone(),
                reason: "exact_acknowledged_representation".to_owned(),
                source_hash: block.source_hash.clone(),
                restore_eligibility: vec![
                    "content_change",
                    "restart",
                    "compaction",
                    "response_loss",
                    "budget_or_fence_drop",
                    "unknown_acknowledgement",
                    "explicit_refresh",
                ],
            });
            false
        } else {
            true
        }
    });
    receipts
}

/// Return previous packet only when exact acknowledgement remains retained in
/// current H9; otherwise callers cannot reuse its prefix.
pub fn previous_packet(
    catalog: &ContextCatalog,
    repository_id: &str,
    session_id: &str,
    loaded: &LoadedContextIdentitiesV1,
) -> Option<Value> {
    acknowledged_selected(catalog, repository_id, session_id, loaded)
        .map(|selected| selected.packet)
}
