//! Retention/prune concern: the shared in-memory half of `prune_before`.
//! Both stores keep a `Vec<DeliveryRecord>` mirror ordered newest-first; this
//! drops everything older than the cutoff and reports how many were removed
//! so the caller can decide whether a persist is needed.

use crate::delivery::DeliveryRecord;

/// Remove every record with `delivered_at_ms < cutoff_ms` from the in-memory
/// mirror; returns the number of records removed.
pub(super) fn prune_records_before(records: &mut Vec<DeliveryRecord>, cutoff_ms: u64) -> usize {
    let before = records.len();
    records.retain(|record| record.delivered_at_ms >= cutoff_ms);
    before - records.len()
}
