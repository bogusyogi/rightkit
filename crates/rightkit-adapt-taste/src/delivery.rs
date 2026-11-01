//! Preference-specific delivery & effectiveness receipts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::authority::{detect_rule_contradictions, PrecedenceTier, StoredRule};
use crate::record::{InfluenceClass, LifecycleState, PreferenceRecordV1, RecordClass};
use crate::scope::ScopeDimensions;

pub const DELIVERY_RECEIPT_SCHEMA: &str = "adapt.preference-delivery.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferenceDeliveryReceiptV1 {
    pub schema_version: String,
    pub receipt_id: String,
    pub record_id: String,
    pub selected: bool,
    pub applicability_reason: String,
    pub model: Option<String>,
    pub machine: Option<String>,
    pub client: Option<String>,
    pub session_id: String,
    pub trace_id: String,
    #[serde(default)]
    pub request_id: String,
    pub rendered_sha256: Option<String>,
    pub rendered_chars: Option<usize>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreferenceDeliveryCandidateV1 {
    pub record_id: String,
    pub rule: String,
    pub class: RecordClass,
    pub scope: String,
    pub scope_dimensions: ScopeDimensions,
    pub machine_binding: Option<String>,
    pub authority_tier: PrecedenceTier,
    pub lifecycle_state: LifecycleState,
    pub lifecycle_eligible: bool,
    pub influence_class: InfluenceClass,
    pub semantic_verified: bool,
    /// Evidence metadata only; this never changes preference authority or
    /// selection precedence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterfactual: Option<crate::taste::TasteCounterfactualV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreferenceDeliveryContextV1 {
    pub allowed_scopes: Vec<String>,
    pub dimensions: ScopeDimensions,
    pub machine: Option<String>,
    pub max_core_records: usize,
    pub max_scoped_records: usize,
    pub max_total_records: usize,
    pub max_rendered_chars: usize,
    pub timestamp: String,
    pub session_id: String,
    pub trace_id: String,
    pub request_id: String,
    pub client: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveredPreferenceV1 {
    pub record_id: String,
    pub rule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterfactual: Option<crate::taste::TasteCounterfactualV1>,
    pub receipt: PreferenceDeliveryReceiptV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreferenceDeliveryPlanV1 {
    pub delivered: Vec<DeliveredPreferenceV1>,
    pub receipts: Vec<PreferenceDeliveryReceiptV1>,
}

fn base_omission_reason(
    candidate: &PreferenceDeliveryCandidateV1,
    context: &PreferenceDeliveryContextV1,
) -> Option<&'static str> {
    if candidate.lifecycle_state != LifecycleState::Active || !candidate.lifecycle_eligible {
        Some("inactive_lifecycle")
    } else if candidate.influence_class != InfluenceClass::BehavioralDirective {
        Some("non_directive_influence")
    } else if !candidate.semantic_verified {
        Some("invalid_semantic_seal")
    } else if candidate
        .machine_binding
        .as_ref()
        .is_some_and(|binding| context.machine.as_ref() != Some(binding))
    {
        Some("machine_nonmatch")
    } else if !delivery_scope_matches(candidate, context) {
        Some("scope_nonmatch")
    } else {
        None
    }
}

/// Selected-transcript scopes are content-addressed from their narrowing
/// dimensions. Callers cannot know that opaque id in advance, so delivery is
/// governed by the sealed dimensions themselves. Recomputing the id here
/// prevents a hand-authored `dimensions:*` scope from bypassing ordinary
/// exact-scope admission or widening into another dimension set.
fn delivery_scope_matches(
    candidate: &PreferenceDeliveryCandidateV1,
    context: &PreferenceDeliveryContextV1,
) -> bool {
    if let Some(digest) = candidate.scope.strip_prefix("dimensions:") {
        if candidate.scope_dimensions.is_empty() {
            return false;
        }
        let dimensions = candidate
            .scope_dimensions
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let canonical_digest = crate::canonical::sha256_canonical(
            &serde_json::to_value(dimensions).expect("scope dimensions serialize"),
        );
        digest == &canonical_digest[..24] && candidate.scope_dimensions.matches(&context.dimensions)
    } else {
        context
            .allowed_scopes
            .iter()
            .any(|scope| scope == &candidate.scope)
            && candidate.scope_dimensions.matches(&context.dimensions)
    }
}

fn delivery_receipt(
    candidate: &PreferenceDeliveryCandidateV1,
    selected: bool,
    reason: &str,
    context: &PreferenceDeliveryContextV1,
) -> PreferenceDeliveryReceiptV1 {
    let rendered_sha256 = selected.then(|| crate::canonical::sha256_hex(candidate.rule.as_bytes()));
    let identity = format!(
        "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
        candidate.record_id,
        selected,
        reason,
        context.timestamp,
        context.session_id,
        context.trace_id,
        context.request_id,
        context.client,
        context.model.as_deref().unwrap_or(""),
        context.machine.as_deref().unwrap_or(""),
        rendered_sha256.as_deref().unwrap_or("")
    );
    PreferenceDeliveryReceiptV1 {
        schema_version: DELIVERY_RECEIPT_SCHEMA.into(),
        receipt_id: format!("pdr.{}", crate::canonical::sha256_hex(identity.as_bytes())),
        record_id: candidate.record_id.clone(),
        selected,
        applicability_reason: reason.into(),
        model: context.model.clone(),
        machine: context.machine.clone(),
        client: Some(context.client.clone()),
        session_id: context.session_id.clone(),
        trace_id: context.trace_id.clone(),
        request_id: context.request_id.clone(),
        rendered_sha256,
        rendered_chars: selected.then(|| candidate.rule.chars().count()),
        timestamp: context.timestamp.clone(),
    }
}

/// Canonical two-layer Taste delivery. Broad root standing preferences are
/// considered before scoped preferences, but both share one record and exact
/// rendered-character budget. Every observed candidate receives a receipt;
/// only selected records carry rendered bytes.
pub fn select_delivery_candidates(
    candidates: &[PreferenceDeliveryCandidateV1],
    context: &PreferenceDeliveryContextV1,
) -> PreferenceDeliveryPlanV1 {
    let mut ordered: Vec<&PreferenceDeliveryCandidateV1> = candidates.iter().collect();
    ordered.sort_by_key(|candidate| {
        let core = candidate.class == RecordClass::StandingPreference
            && candidate.scope == "global"
            && candidate.scope_dimensions.is_empty();
        (!core, candidate.record_id.as_str())
    });
    let mut delivered = Vec::new();
    let mut receipts = Vec::with_capacity(ordered.len());
    let applicable = ordered
        .iter()
        .copied()
        .filter(|candidate| base_omission_reason(candidate, context).is_none())
        .collect::<Vec<_>>();
    let mut conflict_reasons = BTreeMap::<String, &'static str>::new();
    for (index, candidate) in applicable.iter().enumerate() {
        for other in applicable.iter().skip(index + 1) {
            let same_rule = crate::canonical::normalize_text(&candidate.rule)
                == crate::canonical::normalize_text(&other.rule);
            let stored = StoredRule {
                id: other.record_id.clone(),
                rule: other.rule.clone(),
                scope: other.scope.clone(),
                lifecycle_state: "active".into(),
            };
            let contradictory =
                !detect_rule_contradictions(&candidate.rule, &candidate.scope, [&stored])
                    .is_empty();
            if !same_rule && !contradictory {
                continue;
            }
            let ordering = candidate
                .authority_tier
                .cmp(&other.authority_tier)
                .then_with(|| {
                    other
                        .scope_dimensions
                        .len()
                        .cmp(&candidate.scope_dimensions.len())
                });
            match ordering {
                std::cmp::Ordering::Less => {
                    conflict_reasons.insert(
                        other.record_id.clone(),
                        if same_rule {
                            "duplicate_delivery"
                        } else {
                            "shadowed_by_precedence"
                        },
                    );
                }
                std::cmp::Ordering::Greater => {
                    conflict_reasons.insert(
                        candidate.record_id.clone(),
                        if same_rule {
                            "duplicate_delivery"
                        } else {
                            "shadowed_by_precedence"
                        },
                    );
                }
                std::cmp::Ordering::Equal if same_rule => {
                    let loser = if candidate.record_id < other.record_id {
                        other.record_id.clone()
                    } else {
                        candidate.record_id.clone()
                    };
                    conflict_reasons.insert(loser, "duplicate_delivery");
                }
                std::cmp::Ordering::Equal => {
                    conflict_reasons.insert(candidate.record_id.clone(), "unresolved_conflict");
                    conflict_reasons.insert(other.record_id.clone(), "unresolved_conflict");
                }
            }
        }
    }
    let mut core_count = 0usize;
    let mut scoped_count = 0usize;
    let mut rendered_chars = 0usize;
    for candidate in ordered {
        let core = candidate.class == RecordClass::StandingPreference
            && candidate.scope == "global"
            && candidate.scope_dimensions.is_empty();
        // Current Cortex lifecycle/influence truth is the most actionable
        // omission reason. A bad seal still fails closed whenever the envelope
        // would otherwise permit directive delivery.
        let base_reason = base_omission_reason(candidate, context)
            .or_else(|| conflict_reasons.get(&candidate.record_id).copied());
        let rule_chars = candidate.rule.chars().count();
        let layer_full = if core {
            core_count >= context.max_core_records
        } else {
            scoped_count >= context.max_scoped_records
        };
        let budget_full = delivered.len() >= context.max_total_records
            || rendered_chars.saturating_add(rule_chars) > context.max_rendered_chars;
        let reason = base_reason.unwrap_or(if layer_full || budget_full {
            "selection_budget"
        } else {
            "applicable"
        });
        let selected = reason == "applicable";
        let receipt = delivery_receipt(candidate, selected, reason, context);
        if selected {
            rendered_chars += rule_chars;
            if core {
                core_count += 1;
            } else {
                scoped_count += 1;
            }
            delivered.push(DeliveredPreferenceV1 {
                record_id: candidate.record_id.clone(),
                rule: candidate.rule.clone(),
                counterfactual: candidate.counterfactual.clone(),
                receipt: receipt.clone(),
            });
        }
        receipts.push(receipt);
    }
    PreferenceDeliveryPlanV1 {
        delivered,
        receipts,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliverySelectionV1 {
    pub records: Vec<PreferenceRecordV1>,
    pub receipts: Vec<PreferenceDeliveryReceiptV1>,
}

/// Cheap structured selection. No semantic search participates; inactive,
/// provisional, nonmatching, or excess records receive explicit omission
/// receipts.
pub fn select_preferences(
    records: &[PreferenceRecordV1],
    context: &ScopeDimensions,
    max_records: usize,
    timestamp: &str,
) -> DeliverySelectionV1 {
    let candidates = records
        .iter()
        .map(|record| PreferenceDeliveryCandidateV1 {
            record_id: record.id.clone(),
            rule: record.rule.clone(),
            class: record.class,
            scope: record.scope.clone(),
            scope_dimensions: record.scope_dimensions.clone(),
            machine_binding: None,
            authority_tier: if record.scope == "global" {
                PrecedenceTier::ExplicitGlobalUserPreference
            } else {
                PrecedenceTier::ExplicitScopedUserPreference
            },
            lifecycle_state: record.lifecycle_state,
            lifecycle_eligible: true,
            influence_class: record.influence_class,
            semantic_verified: true,
            counterfactual: None,
        })
        .collect::<Vec<_>>();
    let plan = select_delivery_candidates(
        &candidates,
        &PreferenceDeliveryContextV1 {
            allowed_scopes: records.iter().map(|record| record.scope.clone()).collect(),
            dimensions: context.clone(),
            machine: None,
            max_core_records: max_records,
            max_scoped_records: max_records,
            max_total_records: max_records,
            max_rendered_chars: usize::MAX,
            timestamp: timestamp.into(),
            session_id: String::new(),
            trace_id: String::new(),
            request_id: format!("legacy-{timestamp}"),
            client: context.get("client").unwrap_or("").into(),
            model: context.get("model").map(str::to_string),
        },
    );
    let selected_ids = plan
        .delivered
        .iter()
        .map(|delivery| delivery.record_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let selected = records
        .iter()
        .filter(|record| selected_ids.contains(record.id.as_str()))
        .cloned()
        .collect();
    DeliverySelectionV1 {
        records: selected,
        receipts: plan.receipts,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectivenessEventKind {
    Selected,
    Adhered,
    Corrected,
    Overridden,
    Retired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectivenessEventV1 {
    pub record_id: String,
    pub delivery_receipt_id: String,
    pub kind: EffectivenessEventKind,
    pub model: Option<String>,
    pub client: Option<String>,
    pub correctness_preserved: Option<bool>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EffectivenessMetricsV1 {
    pub selections: u32,
    pub adherences: u32,
    pub corrections: u32,
    pub overrides: u32,
    pub retirements: u32,
    pub correctness_regressions: u32,
}

impl EffectivenessMetricsV1 {
    pub fn adherence_rate(&self) -> Option<f64> {
        (self.selections > 0).then(|| self.adherences as f64 / self.selections as f64)
    }

    pub fn correction_rate(&self) -> Option<f64> {
        (self.selections > 0).then(|| self.corrections as f64 / self.selections as f64)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EffectivenessLedgerV1 {
    pub events: Vec<EffectivenessEventV1>,
}

impl EffectivenessLedgerV1 {
    pub fn record(&mut self, event: EffectivenessEventV1) {
        self.events.push(event);
    }

    pub fn metrics(
        &self,
        record_id: &str,
        model: Option<&str>,
        client: Option<&str>,
    ) -> EffectivenessMetricsV1 {
        let mut metrics = EffectivenessMetricsV1::default();
        for event in self.events.iter().filter(|event| {
            event.record_id == record_id
                && model.is_none_or(|value| event.model.as_deref() == Some(value))
                && client.is_none_or(|value| event.client.as_deref() == Some(value))
        }) {
            match event.kind {
                EffectivenessEventKind::Selected => metrics.selections += 1,
                EffectivenessEventKind::Adhered => metrics.adherences += 1,
                EffectivenessEventKind::Corrected => metrics.corrections += 1,
                EffectivenessEventKind::Overridden => metrics.overrides += 1,
                EffectivenessEventKind::Retired => metrics.retirements += 1,
            }
            if event.correctness_preserved == Some(false) {
                metrics.correctness_regressions += 1;
            }
        }
        metrics
    }

    pub fn by_surface(
        &self,
        record_id: &str,
    ) -> BTreeMap<(String, String), EffectivenessMetricsV1> {
        let mut surfaces = BTreeMap::new();
        for event in self
            .events
            .iter()
            .filter(|event| event.record_id == record_id)
        {
            let key = (
                event.model.clone().unwrap_or_else(|| "unknown".into()),
                event.client.clone().unwrap_or_else(|| "unknown".into()),
            );
            surfaces.insert(
                key.clone(),
                self.metrics(record_id, Some(&key.0), Some(&key.1)),
            );
        }
        surfaces
    }
}
