use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::code_review::{
    adjudication::{AdjudicatedResult, Adjudication},
    error::ReviewError,
    provider::{normalize_verdict, ReviewProviderMetadata, ReviewVerdict},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedReview {
    pub schema_version: u32,
    pub request_id: rightkit_evidence_contracts::RequestId,
    pub review_id: String,
    pub candidates: Vec<crate::code_review::candidate::CandidateEvidence>,
    pub results: Vec<AdjudicatedResult>,
    pub providers: Vec<crate::code_review::provider::ReviewProviderMetadata>,
    pub gaps: Vec<String>,
    pub complete: bool,
}

impl NormalizedReview {
    pub fn digest(&self) -> Result<String, ReviewError> {
        rightkit_evidence_contracts::canonical_digest(self)
            .map_err(|error| ReviewError::Receipt(error.to_string()))
    }

    pub fn omissions(&self) -> &[String] {
        &self.gaps
    }
}

pub fn normalize(mut adjudication: Adjudication) -> Result<NormalizedReview, ReviewError> {
    if adjudication.schema_version != 1 {
        return Err(ReviewError::Invalid(
            "unsupported adjudication schema".into(),
        ));
    }
    if adjudication.review_id.trim().is_empty() {
        return Err(ReviewError::Invalid("review_id must be non-empty".into()));
    }
    if adjudication.candidates.is_empty() {
        return Err(ReviewError::Invalid(
            "review must contain at least one candidate".into(),
        ));
    }
    for candidate in &adjudication.candidates {
        candidate.validate()?;
    }
    let evidence_ids: BTreeSet<_> = adjudication
        .candidates
        .iter()
        .map(|candidate| candidate.evidence_id.as_str())
        .collect();
    let candidate_ids: BTreeSet<_> = adjudication
        .candidates
        .iter()
        .map(|candidate| candidate.candidate_id.clone())
        .collect();
    let mut adjudicators = BTreeSet::new();
    for result in &mut adjudication.results {
        if !candidate_ids.contains(&result.candidate_id) {
            return Err(ReviewError::UnknownCandidate(
                result.candidate_id.to_string(),
            ));
        }
        if result.rationale.trim().is_empty() {
            result.verdict = "unknown".into();
            let gap = format!(
                "candidate {} has provider judgment without rationale",
                result.candidate_id
            );
            if !adjudication.gaps.iter().any(|existing| existing == &gap) {
                adjudication.gaps.push(gap);
            }
        }
        let Some(candidate) = adjudication
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_id == result.candidate_id)
        else {
            return Err(ReviewError::UnknownCandidate(
                result.candidate_id.to_string(),
            ));
        };
        if candidate
            .producer_provider()
            .is_some_and(|producer| producer == result.metadata.provider.as_str())
        {
            return Err(ReviewError::SelfClosure(
                result.metadata.provider.to_string(),
            ));
        }
        if !result
            .evidence_ids
            .iter()
            .any(|evidence| evidence == &candidate.evidence_id)
        {
            return Err(ReviewError::Provenance(format!(
                "judgment for candidate {} does not cite candidate evidence {}",
                candidate.candidate_id, candidate.evidence_id
            )));
        }
        if !adjudicators.insert((result.candidate_id.clone(), result.adjudicator_id.clone())) {
            return Err(ReviewError::Duplicate(result.adjudicator_id.clone()));
        }
        if result.evidence_ids.is_empty() {
            return Err(ReviewError::Invalid(format!(
                "judgment {} has no evidence",
                result.adjudicator_id
            )));
        }
        if result
            .evidence_ids
            .iter()
            .any(|evidence| !evidence_ids.contains(evidence.as_str()))
        {
            let Some(missing) = result
                .evidence_ids
                .iter()
                .find(|evidence| !evidence_ids.contains(evidence.as_str()))
            else {
                return Err(ReviewError::Invalid(
                    "evidence validation produced no missing identifier".into(),
                ));
            };
            return Err(ReviewError::UnknownEvidence(missing.clone()));
        }
        result.verdict = normalize_verdict(&result.verdict)?;
        result.metadata.validate()?;
        if matches!(
            ReviewVerdict::parse(&result.verdict),
            ReviewVerdict::Unknown
        ) {
            let gap = format!("candidate {} returned unknown verdict", result.candidate_id);
            if !adjudication.gaps.iter().any(|existing| existing == &gap) {
                adjudication.gaps.push(gap);
            }
        }
    }
    for candidate in &adjudication.candidates {
        if !adjudication
            .results
            .iter()
            .any(|result| result.candidate_id == candidate.candidate_id)
        {
            adjudication.gaps.push(format!(
                "candidate {} has no independent provider judgment",
                candidate.candidate_id
            ));
        }
    }
    adjudication.providers.extend(
        adjudication
            .results
            .iter()
            .map(|result| result.metadata.clone()),
    );
    adjudication
        .candidates
        .sort_by(|left, right| left.candidate_id.cmp(&right.candidate_id));
    adjudication.results.sort_by(|left, right| {
        left.candidate_id
            .cmp(&right.candidate_id)
            .then(left.adjudicator_id.cmp(&right.adjudicator_id))
            .then(left.verdict.cmp(&right.verdict))
            .then(left.rationale.cmp(&right.rationale))
            .then(left.evidence_ids.cmp(&right.evidence_ids))
    });
    adjudication
        .providers
        .sort_by_key(ReviewProviderMetadata::identity_key);
    adjudication.providers.dedup();
    let mut gaps = BTreeSet::new();
    for gap in adjudication.gaps {
        let gap = gap.trim();
        if !gap.is_empty() {
            gaps.insert(gap.to_owned());
        }
    }
    let complete = adjudication.complete
        && !adjudication.results.is_empty()
        && gaps.is_empty()
        && adjudication.candidates.iter().all(|candidate| {
            adjudication.results.iter().any(|result| {
                result.candidate_id == candidate.candidate_id
                    && !matches!(
                        ReviewVerdict::parse(&result.verdict),
                        ReviewVerdict::Unknown
                    )
            })
        });
    Ok(NormalizedReview {
        schema_version: 1,
        request_id: adjudication.request_id,
        review_id: adjudication.review_id,
        candidates: adjudication.candidates,
        results: adjudication.results,
        providers: adjudication.providers,
        complete,
        gaps: gaps.into_iter().collect(),
    })
}
