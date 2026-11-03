use crate::analysis::{
    error::{Result, RuleError},
    schema::{EvidenceTier, StructuralMatch, StructuralQueryResult, StructuralSelector},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub trait StructuralSource: Send + Sync {
    fn query(&self, selector: &StructuralSelector) -> Result<StructuralQueryResult>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructuralEvidence {
    pub selector_id: String,
    pub generation: String,
    pub evidence_tier: EvidenceTier,
    pub matches: Vec<StructuralMatch>,
    pub complete: bool,
    pub gaps: Vec<String>,
}

pub fn execute_selector<S: StructuralSource>(
    selector: &StructuralSelector,
    source: &S,
) -> Result<StructuralEvidence> {
    selector.validate()?;
    let mut result = source.query(selector)?;
    result
        .validate()
        .map_err(|error| RuleError::InvalidSource(error.to_string()))?;
    if let Some(expected) = &selector.expected_generation {
        if &result.generation != expected {
            return Err(RuleError::GenerationMismatch {
                expected: expected.clone(),
                actual: result.generation,
            });
        }
    }
    if result.evidence_tier != selector.expected_evidence_tier {
        result.complete = false;
        result.gaps.push("structural-evidence-tier-mismatch".into());
    }
    result.matches.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.path.cmp(&right.path))
            .then(left.symbol.cmp(&right.symbol))
            .then(left.evidence.cmp(&right.evidence))
    });
    result.gaps.sort();
    result.gaps.dedup();
    result.complete = result.complete && result.gaps.is_empty();
    Ok(StructuralEvidence {
        selector_id: selector.selector_id.clone(),
        generation: result.generation,
        evidence_tier: result.evidence_tier,
        matches: result.matches,
        complete: result.complete,
        gaps: result.gaps,
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructuralEvaluation {
    pub evidence: Vec<StructuralEvidence>,
    pub complete: bool,
    pub gaps: Vec<String>,
}

pub fn evaluate<S: StructuralSource>(
    selectors: &[StructuralSelector],
    source: &S,
) -> StructuralEvaluation {
    let mut evidence = Vec::new();
    let mut gaps = Vec::new();
    let mut ordered = selectors.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.selector_id.cmp(&right.selector_id));
    let mut selector_ids = BTreeSet::new();
    for selector in ordered {
        if !selector_ids.insert(selector.selector_id.as_str()) {
            gaps.push(format!("duplicate-selector-id:{}", selector.selector_id));
            continue;
        }
        match execute_selector(selector, source) {
            Ok(value) => {
                if !value.complete {
                    gaps.extend(value.gaps.iter().cloned());
                }
                evidence.push(value);
            }
            Err(error) => gaps.push(error.to_string()),
        }
    }
    evidence.sort_by(|left, right| left.selector_id.cmp(&right.selector_id));
    gaps.sort();
    gaps.dedup();
    StructuralEvaluation {
        complete: gaps.is_empty() && evidence.len() == selectors.len(),
        evidence,
        gaps,
    }
}

/// Evaluate a structural-source-dependent rule set while preserving typed degradation
/// when a host-published structural-evidence source is absent. The Rules crate does not
/// discover, open, or synthesize a structural-evidence store.
pub fn evaluate_optional<S: StructuralSource>(
    selectors: &[StructuralSelector],
    source: Option<&S>,
) -> StructuralEvaluation {
    source.map_or_else(
        || StructuralEvaluation::unavailable(selectors, "structural-source-unavailable"),
        |source| evaluate(selectors, source),
    )
}

impl StructuralEvaluation {
    pub fn unavailable(selectors: &[StructuralSelector], reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let mut ordered = selectors.to_vec();
        ordered.sort_by(|left, right| left.selector_id.cmp(&right.selector_id));
        let evidence = ordered
            .iter()
            .map(|selector| StructuralEvidence {
                selector_id: selector.selector_id.clone(),
                generation: String::new(),
                evidence_tier: selector.expected_evidence_tier,
                matches: Vec::new(),
                complete: false,
                gaps: vec![reason.clone()],
            })
            .collect();
        Self {
            evidence,
            complete: false,
            gaps: vec![reason],
        }
    }
}
