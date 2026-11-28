//! Observation layer — the structured record of every agent execution, and the
//! source the evaluation/taste layer reads from (see `docs/architecture/README.md`).
//!
//! Each finished run contributes one `AgentExecutionEvent` per participating
//! role: its duration bucket, GDS, token cost, whether it melted down (MOP),
//! whether it escalated, and output quality. From the accumulated events we
//! compute a per-role [`TasteScore`]. Persisted to the `observations` table of
//! the evolution database.

use std::collections::HashMap;

use rightkit_agent_orchestration::coding::{
    compute_taste_score, propose, DurationBucket, RunRecord, TasteEngineConfig,
};
use serde::{Deserialize, Serialize};

use crate::evo_db::EvolutionDb;
use crate::membrane_binding::{BindingSurfaceV1, UnavailableReasonV1, UnavailableStateV1};

/// Host-side observation capabilities required by Membrane's closed loop.
/// These records carry facts only; they never classify or admit semantic data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostObservationCapabilityV1 {
    H7VariantExperiment,
    H9LoadedContext,
    H10PacketDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostObservationCoverageV1 {
    Complete,
    Unavailable,
}

/// Adapt a fully keyed physical-attempt row from the provider-neutral
/// telemetry contract. Its validator enforces that Complete H9/H10 values can
/// exist only for `submitted`; rejected, unknown, and unavailable rows retain
/// typed non-complete values in the payload.
pub fn provider_turn_evidence_observation(
    observation_id: impl Into<String>,
    observed_at_unix_ms: u64,
    evidence: rightkit_agent_telemetry::evidence::ProviderTurnEvidenceV1,
    receipt: HostObservationProvenanceV1,
) -> Result<HostObservationV1, &'static str> {
    evidence
        .validate()
        .map_err(|_| "invalid provider turn evidence")?;
    let session_id = Some(evidence.session_id.clone());
    let task_id = Some(evidence.task_id.clone());
    let payload =
        serde_json::to_value(evidence).map_err(|_| "provider evidence serialization failed")?;
    let observation = HostObservationV1::from_live(
        observation_id,
        HostObservationCapabilityV1::H10PacketDelivery,
        session_id,
        task_id,
        observed_at_unix_ms,
        Some(payload),
        receipt,
    );
    observation.validate()?;
    Ok(observation)
}

/// A typed value crossing the host seam. Unavailable values contain no
/// substitute payload, so absent live evidence cannot become zero or empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostObservedValueV1<T> {
    pub coverage: HostObservationCoverageV1,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<UnavailableStateV1>,
}

impl<T> HostObservedValueV1<T> {
    pub fn complete(value: T) -> Self {
        Self {
            coverage: HostObservationCoverageV1::Complete,
            value: Some(value),
            unavailable: None,
        }
    }

    pub fn unavailable(
        capability: HostObservationCapabilityV1,
        reason: UnavailableReasonV1,
    ) -> Self {
        let surface = match capability {
            HostObservationCapabilityV1::H7VariantExperiment => {
                BindingSurfaceV1::H7VariantExperiment
            }
            HostObservationCapabilityV1::H9LoadedContext => BindingSurfaceV1::H9LoadedContext,
            HostObservationCapabilityV1::H10PacketDelivery => BindingSurfaceV1::H10PacketDelivery,
        };
        Self {
            coverage: HostObservationCoverageV1::Unavailable,
            value: None,
            unavailable: Some(UnavailableStateV1::new(surface, reason, false)),
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        match self.coverage {
            HostObservationCoverageV1::Complete
                if self.value.is_some() && self.unavailable.is_none() =>
            {
                Ok(())
            }
            HostObservationCoverageV1::Unavailable
                if self.value.is_none() && self.unavailable.is_some() =>
            {
                Ok(())
            }
            HostObservationCoverageV1::Complete => {
                Err("complete observation requires value and no unavailable state")
            }
            HostObservationCoverageV1::Unavailable => {
                Err("unavailable observation requires typed reason and no value")
            }
        }
    }
}

/// One content-free, provenance-bound H7/H9/H10 observation. Payload is
/// supplied by the live host source and remains opaque to this adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostObservationV1 {
    pub schema_version: u32,
    pub observation_id: String,
    pub capability: HostObservationCapabilityV1,
    pub session_id: HostObservedValueV1<String>,
    pub task_id: HostObservedValueV1<String>,
    pub observed_at_unix_ms: u64,
    pub payload: HostObservedValueV1<serde_json::Value>,
    pub provenance: HostObservationProvenanceV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostObservationProvenanceV1 {
    pub receipt_id: String,
    pub source: String,
    pub receipt_digest: String,
}

impl HostObservationV1 {
    pub const SCHEMA_VERSION: u32 = 1;

    /// Build observation from a live payload. `None` is preserved as typed
    /// unavailability; no fallback payload is generated.
    pub fn from_live(
        observation_id: impl Into<String>,
        capability: HostObservationCapabilityV1,
        session_id: Option<String>,
        task_id: Option<String>,
        observed_at_unix_ms: u64,
        payload: Option<serde_json::Value>,
        receipt: HostObservationProvenanceV1,
    ) -> Self {
        let reason = UnavailableReasonV1::NotInstrumented;
        Self {
            schema_version: Self::SCHEMA_VERSION,
            observation_id: observation_id.into(),
            capability,
            session_id: session_id
                .map(HostObservedValueV1::complete)
                .unwrap_or_else(|| HostObservedValueV1::unavailable(capability, reason)),
            task_id: task_id
                .map(HostObservedValueV1::complete)
                .unwrap_or_else(|| HostObservedValueV1::unavailable(capability, reason)),
            observed_at_unix_ms,
            payload: payload
                .map(HostObservedValueV1::complete)
                .unwrap_or_else(|| HostObservedValueV1::unavailable(capability, reason)),
            provenance: receipt,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != Self::SCHEMA_VERSION || self.observation_id.trim().is_empty() {
            return Err("invalid host observation identity");
        }
        self.session_id.validate()?;
        self.task_id.validate()?;
        self.payload.validate()
    }
}

/// A persisted, serializable agent-execution record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExecutionEvent {
    pub role: String,
    pub model: String,
    /// 0=short,1=medium,2=long,3=very-long.
    pub bucket: u8,
    pub gds: f64,
    pub input_tokens: u32,
    pub mop_fired: bool,
    pub completed_without_escalation: bool,
    pub quality: f64,
    pub created_at: String,
}

impl AgentExecutionEvent {
    fn to_run_record(&self) -> RunRecord {
        RunRecord {
            bucket: match self.bucket {
                0 => DurationBucket::Short,
                1 => DurationBucket::Medium,
                2 => DurationBucket::Long,
                _ => DurationBucket::VeryLong,
            },
            gds: self.gds,
            input_tokens: self.input_tokens,
            mop_fired: self.mop_fired,
            completed_without_escalation: self.completed_without_escalation,
            quality: self.quality,
        }
    }
}

/// Bucket a wall-clock duration (milliseconds) into a [`DurationBucket`] index.
pub fn bucket_of_ms(ms: u128) -> u8 {
    if ms < 120_000 {
        0 // < 2 min  → short
    } else if ms < 600_000 {
        1 // < 10 min → medium
    } else if ms < 1_800_000 {
        2 // < 30 min → long
    } else {
        3 // very-long
    }
}

#[derive(Clone)]
pub struct ObservationStore {
    db: EvolutionDb,
}

impl Default for ObservationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ObservationStore {
    /// Backed by an ephemeral in-memory database (tests / no home dir).
    pub fn new() -> Self {
        Self::open(EvolutionDb::open_in_memory())
    }

    /// Backed by the shared evolution database.
    pub fn open(db: EvolutionDb) -> Self {
        Self { db }
    }

    pub fn record(&self, event: AgentExecutionEvent) {
        let _ = self.db.lock().execute(
            "INSERT INTO observations
                 (role, model, bucket, gds, input_tokens, mop_fired,
                  completed_without_escalation, quality, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rightkit_db::rusqlite::params![
                event.role,
                event.model,
                event.bucket as i64,
                event.gds,
                event.input_tokens as i64,
                event.mop_fired as i64,
                event.completed_without_escalation as i64,
                event.quality,
                event.created_at,
            ],
        );
    }

    /// Every recorded event, oldest first.
    fn all_events(&self) -> Vec<AgentExecutionEvent> {
        let conn = self.db.lock();
        let Ok(mut stmt) = conn.prepare(
            "SELECT role, model, bucket, gds, input_tokens, mop_fired,
                    completed_without_escalation, quality, created_at
             FROM observations ORDER BY id",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok(AgentExecutionEvent {
                role: r.get(0)?,
                model: r.get(1)?,
                bucket: r.get::<_, i64>(2)? as u8,
                gds: r.get(3)?,
                input_tokens: r.get::<_, i64>(4)? as u32,
                mop_fired: r.get::<_, i64>(5)? != 0,
                completed_without_escalation: r.get::<_, i64>(6)? != 0,
                quality: r.get(7)?,
                created_at: r.get(8)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// Per-role [`TasteScore`] as JSON (for `GET /runtime/taste`).
    pub fn taste_json(&self) -> Vec<serde_json::Value> {
        let events = self.all_events();
        let mut by_role: HashMap<String, Vec<RunRecord>> = HashMap::new();
        for e in events.iter() {
            by_role
                .entry(e.role.clone())
                .or_default()
                .push(e.to_run_record());
        }
        let mut rows: Vec<serde_json::Value> = by_role
            .into_iter()
            .map(|(role, recs)| {
                let ts = compute_taste_score(&recs);
                serde_json::json!({
                    "role": role,
                    "samples": recs.len(),
                    "overall": ts.overall(),
                    "gds": ts.gds,
                    "longHorizonReliability": ts.long_horizon_reliability,
                    "reliabilityDecaySlope": ts.reliability_decay_slope,
                    "quality": ts.quality,
                    "reliability": ts.reliability,
                    "consistency": ts.consistency,
                    "robustness": ts.robustness,
                    "predictability": ts.predictability,
                    "safety": ts.safety,
                    "contextEfficiency": ts.context_efficiency,
                    "vaf": ts.vaf,
                    "mopRate": ts.mop_rate,
                })
            })
            .collect();
        // Best overall first.
        rows.sort_by(|a, b| {
            let ao = a["overall"].as_f64().unwrap_or(0.0);
            let bo = b["overall"].as_f64().unwrap_or(0.0);
            bo.partial_cmp(&ao).unwrap_or(std::cmp::Ordering::Equal)
        });
        rows
    }

    /// Fine-tuning readiness per (role, model): the count of high-quality
    /// qualifying records (GDS ≥ 0.90, no meltdown, no escalation). A
    /// FineTuningProposal is "ready" once a role accumulates enough of them.
    /// For `GET /runtime/finetuning`.
    pub fn finetuning_json(&self) -> Vec<serde_json::Value> {
        const QUALIFYING_GDS: f64 = 0.90;
        const READY_THRESHOLD: usize = 500;
        let events = self.all_events();
        let mut counts: HashMap<(String, String), usize> = HashMap::new();
        for e in events.iter() {
            if e.gds >= QUALIFYING_GDS && !e.mop_fired && e.completed_without_escalation {
                *counts.entry((e.role.clone(), e.model.clone())).or_default() += 1;
            }
        }
        let mut rows: Vec<serde_json::Value> = counts
            .into_iter()
            .map(|((role, model), n)| {
                serde_json::json!({
                    "role": role,
                    "model": model,
                    "qualifyingRecords": n,
                    "threshold": READY_THRESHOLD,
                    "ready": n >= READY_THRESHOLD,
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            b["qualifyingRecords"]
                .as_u64()
                .cmp(&a["qualifyingRecords"].as_u64())
        });
        rows
    }

    /// Taste-engine improvement proposals across all roles (inert/advisory),
    /// for `GET /runtime/proposals`.
    pub fn proposals_json(&self) -> Vec<serde_json::Value> {
        let events = self.all_events();
        let mut by_role: HashMap<String, Vec<RunRecord>> = HashMap::new();
        for e in events.iter() {
            by_role
                .entry(e.role.clone())
                .or_default()
                .push(e.to_run_record());
        }
        let cfg = TasteEngineConfig::default();
        let mut out = Vec::new();
        for (role, recs) in by_role {
            let ts = compute_taste_score(&recs);
            for p in propose(&role, &ts, recs.len(), &cfg) {
                out.push(serde_json::json!({
                    "role": p.role,
                    "kind": p.kind.as_str(),
                    "rationale": p.rationale,
                    "confidence": p.confidence,
                }));
            }
        }
        out
    }
}
