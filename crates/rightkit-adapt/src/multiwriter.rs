//! Multi-writer deterministic merge & convergence (canon §8).
//!
//! Multiple writers (hosts, agents, sessions) may produce records for the
//! same RuleKey. Merge is order-independent: applying the same set of
//! records in any order yields identical store state. Conflicts between
//! equal-precedence divergent payloads are PRESERVED as conflicts, never
//! silently resolved by arrival order.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::authority::{compare_precedence, PrecedenceTier};
use crate::record::RuleKey;

fn storage_key(rule_key: &RuleKey) -> String {
    // Length-prefix scope so `(a, b/c)` cannot collide with `(a/b, c)`.
    format!(
        "{}:{}{}",
        rule_key.scope.len(),
        rule_key.scope,
        rule_key.record_id
    )
}

/// A writer-agnostic record envelope entering merge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriterRecord {
    pub writer_id: String,
    /// Logical clock for deterministic tie-breaks (writer sequence number).
    pub lamport: u64,
    pub rule_key: RuleKey,
    /// Digest of the sealed semantic payload.
    pub payload_digest: String,
    pub precedence_tier: PrecedenceTier,
}

/// A persisted writer request.  `WriterRecord` is intentionally kept as the
/// small merge primitive; this envelope adds the identities that must survive
/// the Cortex proposal boundary.  Adapt emits this value but never persists it
/// itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriterRequestV1 {
    pub repository_id: String,
    pub scope_id: String,
    pub record_id: String,
    /// Version is opaque to Adapt, but is part of request identity.  A writer
    /// may therefore retry an old version without being mistaken for a new
    /// write.
    pub version: String,
    pub writer_id: String,
    pub lamport: u64,
    pub payload_digest: String,
    pub precedence_tier: PrecedenceTier,
}

/// Descriptive alias used by runtime callers that need to make the durable
/// boundary explicit.
pub type DurableWriterRequestV1 = WriterRequestV1;

impl WriterRequestV1 {
    #[allow(clippy::too_many_arguments)] // established public constructor; a params struct would break callers
    pub fn new(
        repository_id: impl Into<String>,
        scope_id: impl Into<String>,
        record_id: impl Into<String>,
        version: impl Into<String>,
        writer_id: impl Into<String>,
        lamport: u64,
        payload_digest: impl Into<String>,
        precedence_tier: PrecedenceTier,
    ) -> Self {
        Self {
            repository_id: repository_id.into(),
            scope_id: scope_id.into(),
            record_id: record_id.into(),
            version: version.into(),
            writer_id: writer_id.into(),
            lamport,
            payload_digest: payload_digest.into(),
            precedence_tier,
        }
    }

    pub fn from_writer_record(
        repository_id: impl Into<String>,
        version: impl Into<String>,
        record: &WriterRecord,
    ) -> Self {
        Self::new(
            repository_id,
            record.rule_key.scope.clone(),
            record.rule_key.record_id.clone(),
            version,
            record.writer_id.clone(),
            record.lamport,
            record.payload_digest.clone(),
            record.precedence_tier,
        )
    }

    pub fn writer_record(&self) -> WriterRecord {
        WriterRecord {
            writer_id: self.writer_id.clone(),
            lamport: self.lamport,
            rule_key: RuleKey::new(&self.scope_id, &self.record_id),
            payload_digest: self.payload_digest.clone(),
            precedence_tier: self.precedence_tier,
        }
    }

    fn validate_binding(&self, repository: &str, scope: &str) -> Result<(), String> {
        if self.repository_id != repository {
            return Err("multiwriter_repository_binding_denied".into());
        }
        if self.scope_id != scope {
            return Err("multiwriter_scope_binding_denied".into());
        }
        if self.record_id.trim().is_empty() {
            return Err("multiwriter_record_identity_required".into());
        }
        if self.version.trim().is_empty() {
            return Err("multiwriter_version_identity_required".into());
        }
        if self.writer_id.trim().is_empty() {
            return Err("multiwriter_writer_identity_required".into());
        }
        if self.payload_digest.trim().is_empty() {
            return Err("multiwriter_payload_identity_required".into());
        }
        Ok(())
    }

    fn as_value(&self) -> Value {
        serde_json::to_value(self).expect("WriterRequestV1 is serializable")
    }
}

pub const MULTIWRITER_SCHEMA_VERSION: u64 = 1;
pub const MULTIWRITER_KIND: &str = "adapt_multiwriter";
const MAX_WRITER_REQUESTS: usize = 128;

fn sorted_requests(
    repository: &str,
    scope: &str,
    requests: &[WriterRequestV1],
) -> Result<Vec<WriterRequestV1>, String> {
    if repository.trim().is_empty() {
        return Err("multiwriter_repository_identity_required".into());
    }
    if scope.trim().is_empty() {
        return Err("multiwriter_scope_identity_required".into());
    }
    if requests.is_empty() || requests.len() > MAX_WRITER_REQUESTS {
        return Err("multiwriter_request_count_out_of_bounds".into());
    }
    let mut sorted = requests.to_vec();
    for request in &sorted {
        request.validate_binding(repository, scope)?;
    }
    sorted.sort_by(|left, right| {
        left.record_id
            .cmp(&right.record_id)
            .then_with(|| left.version.cmp(&right.version))
            .then_with(|| left.writer_id.cmp(&right.writer_id))
            .then_with(|| left.lamport.cmp(&right.lamport))
            .then_with(|| left.precedence_tier.cmp(&right.precedence_tier))
            .then_with(|| left.payload_digest.cmp(&right.payload_digest))
    });
    Ok(sorted)
}

/// Build the canonical, proposal-only Cortex emission for competing requests.
/// Sorting here makes exact retries and every request permutation derive one
/// stable proposal identity.
pub fn proposal_emission(
    repository: &str,
    scope: &str,
    requests: &[WriterRequestV1],
) -> Result<Value, String> {
    let requests = sorted_requests(repository, scope, requests)?;
    let records = requests
        .iter()
        .map(WriterRequestV1::as_value)
        .collect::<Vec<_>>();
    let merge_records = requests
        .iter()
        .map(WriterRequestV1::writer_record)
        .collect::<Vec<_>>();
    let convergence = convergence_value(&merge_records, &requests);
    let body = json!({
        "schemaVersion": MULTIWRITER_SCHEMA_VERSION,
        "kind": MULTIWRITER_KIND,
        "repositoryId": repository,
        "scopeId": scope,
        "records": records,
        "convergence": convergence,
    });
    Ok(json!({
        "schemaVersion": MULTIWRITER_SCHEMA_VERSION,
        "kind": MULTIWRITER_KIND,
        "text": crate::canonical::to_canonical_json(&body),
        "producer": "adapt_native",
        "epistemicClass": "inferred",
        "repositoryId": repository,
        "scopeId": scope,
        "records": body["records"].clone(),
        "convergence": body["convergence"].clone(),
    }))
}

/// Re-check a stored emission before Cortex admission.  This closes the
/// restart/crash path: a proposal row whose JSON was altered cannot be
/// admitted merely because its stored digest was also altered.
pub fn validate_proposal_emission(
    repository: &str,
    scope: &str,
    emission: &Value,
) -> Result<Vec<WriterRequestV1>, String> {
    let expected = emission
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| "multiwriter_text_required".to_string())?;
    let records = emission
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| "multiwriter_records_required".to_string())?;
    let requests = records
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<Vec<WriterRequestV1>, _>>()
        .map_err(|_| "multiwriter_record_invalid".to_string())?;
    let canonical = proposal_emission(repository, scope, &requests)?;
    if emission != &canonical || canonical.get("text") != Some(&Value::String(expected.to_owned()))
    {
        return Err("multiwriter_emission_binding_denied".into());
    }
    Ok(requests)
}

fn convergence_value(records: &[WriterRecord], requests: &[WriterRequestV1]) -> Value {
    let merged = converge(records);
    let mut keys = records
        .iter()
        .map(|r| r.rule_key.clone())
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    let winners = keys
        .iter()
        .filter_map(|key| {
            let (digest, writer) = merged.winner(key)?;
            let request = requests.iter().find(|r| {
                r.record_id == key.record_id
                    && r.scope_id == key.scope
                    && r.payload_digest == digest
                    && r.writer_id == writer
            })?;
            Some(json!({
                "scopeId": key.scope.clone(),
                "recordId": key.record_id.clone(),
                "version": request.version.clone(),
                "writerId": writer,
                "lamport": request.lamport,
                "payloadDigest": digest,
                "precedenceTier": request.precedence_tier,
                "conflicts": merged.conflicts_for(key),
            }))
        })
        .collect::<Vec<_>>();
    json!({
        "schemaVersion": MULTIWRITER_SCHEMA_VERSION,
        "winnerCount": winners.len(),
        "winners": winners,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// New record won; previous stored digest recorded in `superseded`.
    Applied { superseded: Option<String> },
    /// Incoming record lost to an already-stored stronger/equal record.
    Ignored,
    /// Equal-precedence divergent payload: conflict preserved.
    ConflictPreserved { with_writer: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MergedStore {
    /// rule key -> (winning digest, winning writer)
    winners: BTreeMap<String, (String, String)>,
    /// rule key -> tier of the winning record
    tiers: BTreeMap<String, PrecedenceTier>,
    /// rule key -> specificity of the winning record (scope depth)
    specificity: BTreeMap<String, usize>,
    /// rule key -> logical clock of the winning record
    #[serde(default)]
    lamport: BTreeMap<String, u64>,
    /// rule key -> preserved conflicting digests (sorted; order-independent)
    conflicts: BTreeMap<String, Vec<String>>,
}

impl MergedStore {
    pub fn apply(&mut self, rec: &WriterRecord) -> MergeOutcome {
        let key = storage_key(&rec.rule_key);
        let incoming_specificity = rec.rule_key.scope.matches('/').count();
        match self.winners.get(&key).cloned() {
            None => {
                self.winners.insert(
                    key.clone(),
                    (rec.payload_digest.clone(), rec.writer_id.clone()),
                );
                self.tiers.insert(key.clone(), rec.precedence_tier);
                self.specificity.insert(key.clone(), incoming_specificity);
                self.lamport.insert(key, rec.lamport);
                MergeOutcome::Applied { superseded: None }
            }
            Some((digest, writer)) => {
                let stored_tier = self
                    .tiers
                    .get(&key)
                    .copied()
                    .unwrap_or(PrecedenceTier::CurrentExplicitUserInstruction);
                let stored_specificity = self.specificity.get(&key).copied().unwrap_or(0);
                let stored_lamport = self.lamport.get(&key).copied().unwrap_or(0);
                let precedence = compare_precedence(
                    rec.precedence_tier,
                    incoming_specificity,
                    stored_tier,
                    stored_specificity,
                );
                let ordering = if precedence == std::cmp::Ordering::Equal {
                    rec.lamport
                        .cmp(&stored_lamport)
                        .then_with(|| rec.writer_id.cmp(&writer))
                        .then_with(|| rec.payload_digest.cmp(&digest))
                } else {
                    precedence
                };
                if digest == rec.payload_digest {
                    if ordering == std::cmp::Ordering::Less {
                        self.winners.insert(
                            key.clone(),
                            (rec.payload_digest.clone(), rec.writer_id.clone()),
                        );
                        self.tiers.insert(key.clone(), rec.precedence_tier);
                        self.specificity.insert(key.clone(), incoming_specificity);
                        self.lamport.insert(key.clone(), rec.lamport);
                    }
                    self.remove_conflict(&key, &rec.payload_digest);
                    return MergeOutcome::Ignored;
                }
                match ordering {
                    std::cmp::Ordering::Less => {
                        let old_digest = digest.clone();
                        let old = self.winners.insert(
                            key.clone(),
                            (rec.payload_digest.clone(), rec.writer_id.clone()),
                        );
                        self.tiers.insert(key.clone(), rec.precedence_tier);
                        self.specificity.insert(key.clone(), incoming_specificity);
                        self.lamport.insert(key.clone(), rec.lamport);
                        self.record_conflict(&key, &old_digest);
                        self.remove_conflict(&key, &rec.payload_digest);
                        MergeOutcome::Applied {
                            superseded: old.map(|(d, _)| d),
                        }
                    }
                    _ => {
                        self.record_conflict(&key, &rec.payload_digest);
                        MergeOutcome::ConflictPreserved {
                            with_writer: writer.clone(),
                        }
                    }
                }
            }
        }
    }

    fn record_conflict(&mut self, key: &str, digest: &str) {
        let slot = self.conflicts.entry(key.to_string()).or_default();
        if !slot.iter().any(|existing| existing == digest) {
            slot.push(digest.to_string());
            slot.sort();
        }
    }

    fn remove_conflict(&mut self, key: &str, digest: &str) {
        if let Some(slot) = self.conflicts.get_mut(key) {
            slot.retain(|existing| existing != digest);
            if slot.is_empty() {
                self.conflicts.remove(key);
            }
        }
    }

    pub fn winner(&self, rule_key: &RuleKey) -> Option<(&str, &str)> {
        self.winners
            .get(&storage_key(rule_key))
            .map(|(d, w)| (d.as_str(), w.as_str()))
    }

    pub fn conflicts_for(&self, rule_key: &RuleKey) -> &[String] {
        self.conflicts
            .get(&storage_key(rule_key))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

/// Order-independent convergence over a record set. Stronger tiers win in any
/// application order; equal-tier divergent payloads converge to one canonical
/// winner plus a sorted conflict set, so every permutation yields an
/// identical store.
pub fn converge(records: &[WriterRecord]) -> MergedStore {
    let mut ordered: Vec<&WriterRecord> = records.iter().collect();
    ordered.sort_by(|a, b| {
        a.rule_key
            .cmp(&b.rule_key)
            .then_with(|| b.precedence_tier.cmp(&a.precedence_tier))
            .then_with(|| a.lamport.cmp(&b.lamport))
            .then_with(|| a.writer_id.cmp(&b.writer_id))
            .then_with(|| a.payload_digest.cmp(&b.payload_digest))
    });
    let mut store = MergedStore::default();
    for r in ordered {
        store.apply(r);
    }
    store
}
