//! Daemon-owned Adapt application host (PL-011).
//!
//! The route must never create a fresh store or receipt log: doing so loses
//! replay receipts & makes every request observe an empty guidance surface.
//! This process singleton owns one application host until daemon startup
//! installs concrete durable adapters. Its fallback remains fail-closed.

use std::sync::{Arc, OnceLock};

use crate::api::{now_iso, SharedStorage};
use rightkit_agent_engine_core::adapt_consumer::proposal::GuidanceKind;
use rightkit_agent_engine_core::adapt_consumer::{
    apply_proposal, ApplicationAuthorityGate, ApplicationReceipt, ApplicationReceiptLog,
    DenyAllAuthority, GuidanceRecord, GuidanceStore, GuidanceStoreError,
    InMemoryApplicationReceiptLog, InMemoryGuidanceStore, NewGuidanceRecord, ProjectionRegistry,
    SealedRemediationProposalV1,
};
use rightkit_agent_engine_storage::{AdaptApplicationReceiptRow, AdaptGuidanceRow, AdaptGuidanceWrite};

/// One coherent application surface: guidance, policy, projections & receipt
/// log have identical daemon lifetime.
pub struct AdaptApplicationHost {
    guidance: Arc<dyn GuidanceStore>,
    authority: Arc<dyn ApplicationAuthorityGate>,
    projections: ProjectionRegistry,
    receipts: Arc<dyn ApplicationReceiptLog>,
}

/// Durable daemon adapter for PL-011 guidance rows. The engine consumer owns
/// policy/shape checks; SQLite owns the atomic CAS boundary.
pub struct StorageGuidanceStore {
    storage: SharedStorage,
}

impl StorageGuidanceStore {
    pub fn new(storage: SharedStorage) -> Self {
        Self { storage }
    }
}

fn guidance_kind(value: &str) -> Result<GuidanceKind, GuidanceStoreError> {
    match value {
        "skill" => Ok(GuidanceKind::Skill),
        "procedure" => Ok(GuidanceKind::Procedure),
        "system_prompt" => Ok(GuidanceKind::SystemPrompt),
        "tool_description" => Ok(GuidanceKind::ToolDescription),
        "documentation_policy" => Ok(GuidanceKind::DocumentationPolicy),
        other => Err(GuidanceStoreError::Backend(format!(
            "unknown guidance kind {other}"
        ))),
    }
}

fn guidance_record(row: AdaptGuidanceRow) -> Result<GuidanceRecord, GuidanceStoreError> {
    Ok(GuidanceRecord {
        guidance_id: row.guidance_id,
        kind: guidance_kind(&row.kind)?,
        subject: row.subject,
        content: row.content,
        digest: row.digest,
    })
}

impl GuidanceStore for StorageGuidanceStore {
    fn list(&self) -> Result<Vec<GuidanceRecord>, GuidanceStoreError> {
        self.storage
            .blocking_lock()
            .list_adapt_guidance()
            .map_err(|e| GuidanceStoreError::Backend(e.to_string()))?
            .into_iter()
            .map(guidance_record)
            .collect()
    }

    fn replace_if_unchanged(
        &self,
        guidance_id: &str,
        expected_digest: &str,
        new_content: &str,
    ) -> Result<GuidanceRecord, GuidanceStoreError> {
        let result = self
            .storage
            .blocking_lock()
            .replace_adapt_guidance_if_unchanged(guidance_id, expected_digest, new_content)
            .map_err(|e| GuidanceStoreError::Backend(e.to_string()))?;
        match result {
            AdaptGuidanceWrite::Applied(row) => guidance_record(row),
            AdaptGuidanceWrite::Missing => Err(GuidanceStoreError::NotFound {
                guidance_id: guidance_id.into(),
            }),
            AdaptGuidanceWrite::Stale { observed_digest } => Err(GuidanceStoreError::CasMismatch {
                guidance_id: guidance_id.into(),
                observed_base_digest: observed_digest,
            }),
            AdaptGuidanceWrite::Duplicate => Err(GuidanceStoreError::AlreadyExists {
                guidance_id: guidance_id.into(),
            }),
        }
    }

    fn append_record(
        &self,
        _record: NewGuidanceRecord,
    ) -> Result<GuidanceRecord, GuidanceStoreError> {
        Err(GuidanceStoreError::Backend(
            "durable guidance store requires append_if_surface_unchanged".into(),
        ))
    }
}

/// Durable daemon adapter for PL-011 application receipts. Backs
/// [`ApplicationReceiptLog`] with the `adapt_application_receipts` table so
/// replay dedup and the stale-base CAS race guard survive a daemon restart —
/// an in-memory log would otherwise silently reopen every prior receipt to
/// re-application after every restart.
pub struct StorageApplicationReceiptLog {
    storage: SharedStorage,
}

impl StorageApplicationReceiptLog {
    pub fn new(storage: SharedStorage) -> Self {
        Self { storage }
    }
}

impl ApplicationReceiptLog for StorageApplicationReceiptLog {
    fn get(&self, receipt_id: &str) -> Option<ApplicationReceipt> {
        let row = self
            .storage
            .blocking_lock()
            .get_adapt_application_receipt(receipt_id)
            .ok()??;
        // A row that fails to deserialize is treated as absent rather than
        // panicking: the pipeline re-runs and produces a fresh receipt, which
        // is safe because `apply_proposal` never mutates before its CAS
        // checks pass.
        serde_json::from_str(&row.receipt_json).ok()
    }

    fn record(&self, receipt: ApplicationReceipt) {
        let Ok(receipt_json) = serde_json::to_string(&receipt) else {
            return;
        };
        let row = AdaptApplicationReceiptRow {
            receipt_id: receipt.receipt_id.clone(),
            proposal_id: receipt.proposal_id.clone(),
            surface: receipt.surface.as_str().to_string(),
            subject: receipt.subject.clone(),
            receipt_json,
            created_at: now_iso(),
        };
        // `append_adapt_application_receipt` is itself idempotent (Existing /
        // Contradiction both leave the durable row untouched relative to what
        // is already there), so a failure here never needs to be surfaced to
        // the caller — the in-process receipt the pipeline just computed
        // remains correct even if durable persistence could not land it.
        let _ = self
            .storage
            .blocking_lock()
            .append_adapt_application_receipt(&row);
    }
}

impl AdaptApplicationHost {
    /// Builds a host from daemon-owned adapters. Authority is supplied
    /// separately from proposal intake, so proposal approval cannot grant a
    /// mutation by itself.
    pub fn new(
        guidance: Arc<dyn GuidanceStore>,
        authority: Arc<dyn ApplicationAuthorityGate>,
        projections: ProjectionRegistry,
        receipts: Arc<dyn ApplicationReceiptLog>,
    ) -> Self {
        Self {
            guidance,
            authority,
            projections,
            receipts,
        }
    }

    /// Build the live host over daemon SQLite storage. Projection and receipt
    /// adapters remain explicit so startup cannot accidentally fall back to a
    /// second, process-local guidance surface.
    pub fn from_storage(
        storage: SharedStorage,
        authority: Arc<dyn ApplicationAuthorityGate>,
        projections: ProjectionRegistry,
        receipts: Arc<dyn ApplicationReceiptLog>,
    ) -> Self {
        Self::new(
            Arc::new(StorageGuidanceStore::new(storage)),
            authority,
            projections,
            receipts,
        )
    }

    /// Safe bootstrap until startup supplies durable storage & policy.
    pub fn fail_closed() -> Self {
        Self::new(
            Arc::new(InMemoryGuidanceStore::new()),
            Arc::new(DenyAllAuthority),
            ProjectionRegistry::new(),
            Arc::new(InMemoryApplicationReceiptLog::new()),
        )
    }

    pub fn apply(&self, proposal: &SealedRemediationProposalV1) -> ApplicationReceipt {
        apply_proposal(
            proposal,
            self.guidance.as_ref(),
            self.authority.as_ref(),
            &self.projections,
            self.receipts.as_ref(),
        )
    }
}

static DAEMON_ADAPT_APPLICATION_HOST: OnceLock<AdaptApplicationHost> = OnceLock::new();

/// Installs startup's durable application host before first intake.
///
/// Installation is intentionally one-shot: replacing guidance, authority, or
/// receipts mid-process could defeat replay deduplication or stale-base CAS.
pub fn install_adapt_application_host(
    host: AdaptApplicationHost,
) -> Result<(), AdaptApplicationHost> {
    DAEMON_ADAPT_APPLICATION_HOST.set(host)
}

/// Returns one daemon-lifetime host. Absent startup wiring is explicitly
/// fail-closed, while still retaining receipts across route calls.
pub fn daemon_adapt_application_host() -> &'static AdaptApplicationHost {
    DAEMON_ADAPT_APPLICATION_HOST.get_or_init(AdaptApplicationHost::fail_closed)
}

#[cfg(test)]
mod pl011_host_tests {
    use super::*;
    use rightkit_agent_engine_core::adapt_consumer::{ApplicationOutcome, MutationKind, RejectionKind};
    use rightkit_agent_engine_storage::Storage;

    fn shared_storage() -> SharedStorage {
        SharedStorage::new(Storage::open_in_memory().expect("open in-memory storage"))
    }

    fn sample_receipt(receipt_id: &str, proposal_id: &str) -> ApplicationReceipt {
        ApplicationReceipt {
            receipt_id: receipt_id.to_string(),
            proposal_id: proposal_id.to_string(),
            surface: GuidanceKind::Skill,
            subject: "review".to_string(),
            outcome: ApplicationOutcome::Applied {
                guidance_id: "skill/review".to_string(),
                digest: "0".repeat(64),
                mutation_kind: MutationKind::Refine,
                projections: Vec::new(),
            },
        }
    }

    #[test]
    fn storage_receipt_log_round_trips_a_recorded_receipt() {
        let log = StorageApplicationReceiptLog::new(shared_storage());
        assert!(log.get("r-1").is_none());

        let receipt = sample_receipt("r-1", "p-1");
        log.record(receipt.clone());

        let reloaded = log.get("r-1").expect("receipt persisted");
        assert_eq!(reloaded, receipt);
    }

    #[test]
    fn storage_receipt_log_survives_a_fresh_handle_over_the_same_storage() {
        // A second `StorageApplicationReceiptLog` built over the same
        // underlying `Storage` stands in for "the daemon restarted": the
        // durable row must still resolve, unlike an in-memory log which
        // would silently reopen this receipt_id to re-application.
        let storage = Storage::open_in_memory().expect("open in-memory storage");
        let shared = SharedStorage::new(storage);
        let first_handle = StorageApplicationReceiptLog::new(shared.clone());
        let receipt = sample_receipt("r-2", "p-2");
        first_handle.record(receipt.clone());

        let second_handle = StorageApplicationReceiptLog::new(shared);
        assert_eq!(second_handle.get("r-2"), Some(receipt));
    }

    #[test]
    fn storage_receipt_log_reports_a_rejected_receipt_unchanged() {
        let log = StorageApplicationReceiptLog::new(shared_storage());
        let receipt = ApplicationReceipt {
            receipt_id: "r-3".to_string(),
            proposal_id: "p-3".to_string(),
            surface: GuidanceKind::Skill,
            subject: "review".to_string(),
            outcome: ApplicationOutcome::Rejected {
                rejection: RejectionKind::Duplicate,
            },
        };
        log.record(receipt.clone());
        assert_eq!(log.get("r-3"), Some(receipt));
    }

    #[test]
    fn storage_receipt_log_recording_the_same_id_twice_is_idempotent() {
        // `derived_receipt_id` guarantees a byte-identical proposal always
        // recomputes to the same receipt, so a replayed `record` call for the
        // same id carries the identical payload; the durable row must still
        // resolve to that one receipt rather than erroring.
        let log = StorageApplicationReceiptLog::new(shared_storage());
        let receipt = sample_receipt("r-4", "p-4");
        log.record(receipt.clone());
        log.record(receipt.clone());
        assert_eq!(log.get("r-4"), Some(receipt));
    }
}
