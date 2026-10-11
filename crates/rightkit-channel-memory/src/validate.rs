//! Parse pipeline and document rules. Order: contract name and version, then
//! the JSON Schema, then the typed model, then the semantic rules that JSON
//! Schema cannot express. Each stage refuses before the next one runs.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::model::{ChannelMemory, ConflictRecord, ConflictStatus, Proof, ProofStatus};
use crate::proof::{conflicts_with, supersession_rule};
use crate::ReasonCode;
use crate::{schema, MemoryError};

/// Parses and validates one channel-memory revision.
pub fn parse(text: &str) -> Result<ChannelMemory, MemoryError> {
    let value: Value = serde_json::from_str(text).map_err(|error| MemoryError::Parse {
        message: error.to_string(),
    })?;
    negotiate(&value)?;
    schema::check(&value)?;
    let memory: ChannelMemory =
        serde_json::from_value(value).map_err(|error| MemoryError::Parse {
            message: error.to_string(),
        })?;
    semantic(&memory)?;
    Ok(memory)
}

/// Accepts major 1 with minor 0 or below. Patch never gates acceptance.
fn negotiate(value: &Value) -> Result<(), MemoryError> {
    let contract = value
        .get("contract")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if contract != "channel-memory" {
        return Err(MemoryError::WrongContract {
            found: contract.to_string(),
        });
    }
    let text = value
        .get("contractVersion")
        .and_then(Value::as_str)
        .ok_or(MemoryError::MissingVersion)?;
    let parts: Vec<&str> = text.split('.').collect();
    let number = |part: &str| -> Option<u64> {
        let digits = !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
        if digits && (part.len() == 1 || !part.starts_with('0')) {
            part.parse().ok()
        } else {
            None
        }
    };
    let parsed = match parts.as_slice() {
        [major, minor, patch] => match (number(major), number(minor), number(patch)) {
            (Some(major), Some(minor), Some(_)) => (major, minor),
            _ => {
                return Err(MemoryError::InvalidVersion {
                    text: text.to_string(),
                })
            }
        },
        _ => {
            return Err(MemoryError::InvalidVersion {
                text: text.to_string(),
            })
        }
    };
    if parsed.0 != 1 || parsed.1 > 0 {
        return Err(MemoryError::UnsupportedVersion {
            offered: text.to_string(),
        });
    }
    Ok(())
}

fn semantic(memory: &ChannelMemory) -> Result<(), MemoryError> {
    let fail = |rule: String| Err(MemoryError::Semantic { rule });
    match (memory.revision, memory.supersedes) {
        (1, None) => {}
        (1, Some(_)) => return fail("revision 1 supersedes nothing".to_string()),
        (revision, Some(previous)) if previous + 1 == revision => {}
        (revision, _) => {
            return fail(format!(
                "revision {revision} must supersede revision {}",
                revision.saturating_sub(1)
            ))
        }
    }
    unique(
        "pillars",
        memory.pillars.iter().map(|item| item.id.as_str()),
    )?;
    unique(
        "bannedMoves",
        memory.banned_moves.iter().map(|item| item.id.as_str()),
    )?;
    unique("proofs", memory.proofs.iter().map(|item| item.id.as_str()))?;
    unique(
        "conflicts",
        memory.conflicts.iter().map(|item| item.id.as_str()),
    )?;
    unique(
        "observations",
        memory.observations.iter().map(|item| item.id.as_str()),
    )?;
    unique(
        "lessons",
        memory.lessons.iter().map(|item| item.id.as_str()),
    )?;
    unique(
        "performanceNotes",
        memory.performance_notes.iter().map(|item| item.id.as_str()),
    )?;
    unique(
        "references",
        memory.references.iter().map(|item| item.id.as_str()),
    )?;
    if let Some(bindings) = &memory.identity_bindings {
        if let Some(characters) = &bindings.characters {
            unique("characters", characters.iter().map(|item| item.id.as_str()))?;
        }
        if let Some(products) = &bindings.product_references {
            unique(
                "productReferences",
                products.iter().map(|item| item.id.as_str()),
            )?;
        }
        if let Some(assets) = &bindings.brand_assets {
            unique("brandAssets", assets.iter().map(|item| item.id.as_str()))?;
        }
    }
    for proof in &memory.proofs {
        check_proof(proof, memory)?;
    }
    check_approved_scope(memory)?;
    for conflict in &memory.conflicts {
        check_conflict(conflict, memory)?;
    }
    for note in &memory.performance_notes {
        if note.window.to < note.window.from {
            return fail(format!(
                "performance note `{}` window ends before it starts",
                note.id
            ));
        }
    }
    Ok(())
}

/// Proof rules that need the whole document: observation references that
/// exist, and a superseded proof whose successor is approved in this revision.
/// The record rules run first, through `check_proof_record`.
fn check_proof(proof: &Proof, memory: &ChannelMemory) -> Result<(), MemoryError> {
    let fail = |rule: String| Err(MemoryError::Semantic { rule });
    check_proof_record(proof)?;
    for reference in &proof.observation_refs {
        if !memory.observations.iter().any(|item| &item.id == reference) {
            return fail(format!(
                "proof `{}` cites observation `{reference}`, which this revision does not hold",
                proof.id
            ));
        }
    }
    if proof.status == ProofStatus::Superseded {
        let successor = memory
            .proofs
            .iter()
            .find(|item| Some(&item.id) == proof.superseded_by.as_ref());
        supersession_rule(proof, successor).map_err(|(code, detail)| MemoryError::Semantic {
            rule: format!("{code}: {detail}"),
        })?;
    }
    Ok(())
}

/// Rules that apply to one proof record in isolation: date order, unique claim
/// ids and claims that mirror the proof. Used by the document path and by
/// `validate_proof`, so both return the same error for the same defect.
pub(crate) fn check_proof_record(proof: &Proof) -> Result<(), MemoryError> {
    let fail = |rule: String| Err(MemoryError::Semantic { rule });
    if proof.period.to < proof.period.from {
        return fail(format!("proof `{}` period ends before it starts", proof.id));
    }
    if let Some(until) = &proof.valid_until {
        if until < &proof.valid_from {
            return fail(format!(
                "proof `{}` validUntil is before validFrom",
                proof.id
            ));
        }
    }
    let mut claim_ids = BTreeSet::new();
    for claim in &proof.permitted_claims {
        if !claim_ids.insert(claim.claim_id.as_str()) {
            return fail(format!(
                "E_DUPLICATE_CLAIM: claim `{}` repeated in proof `{}`",
                claim.claim_id, proof.id
            ));
        }
        let differs = if claim.value != proof.value {
            Some("value")
        } else if claim.units != proof.units {
            Some("units")
        } else if claim.subject != proof.subject {
            Some("subject")
        } else if claim.period != proof.period {
            Some("period")
        } else {
            None
        };
        if let Some(field) = differs {
            return fail(format!(
                "claim `{}` must mirror proof `{}`: {field} differs",
                claim.claim_id, proof.id
            ));
        }
    }
    Ok(())
}

/// Two approved proofs for the same subject, period and units must agree.
fn check_approved_scope(memory: &ChannelMemory) -> Result<(), MemoryError> {
    let approved: Vec<&Proof> = memory
        .proofs
        .iter()
        .filter(|proof| proof.status == ProofStatus::Approved)
        .collect();
    for (index, first) in approved.iter().enumerate() {
        for second in &approved[index + 1..] {
            if conflicts_with(first, second) {
                return Err(MemoryError::Semantic {
                    rule: format!(
                        "{}: approved proofs `{}` and `{}` disagree for subject `{}`",
                        ReasonCode::ProofConflictScope,
                        first.id,
                        second.id,
                        first.subject
                    ),
                });
            }
        }
    }
    Ok(())
}

/// A conflict holds exactly two bodies from two devices. Both come from the
/// same base revision: revision base + 1, superseding base, and differ.
fn check_conflict(conflict: &ConflictRecord, memory: &ChannelMemory) -> Result<(), MemoryError> {
    let fail = |rule: String| Err(MemoryError::Semantic { rule });
    if !memory
        .proofs
        .iter()
        .any(|proof| proof.id == conflict.proof_id)
    {
        return fail(format!(
            "conflict `{}` names proof `{}`, which this revision does not hold",
            conflict.id, conflict.proof_id
        ));
    }
    let (first, second) = match conflict.bodies.as_slice() {
        [first, second] => (first, second),
        _ => return fail(format!("conflict `{}` must hold two bodies", conflict.id)),
    };
    if first.device == second.device {
        return fail(format!(
            "conflict `{}` holds two bodies from device `{}`",
            conflict.id, first.device
        ));
    }
    for body in [first, second] {
        let base = conflict.base_revision;
        if body.proof.id != conflict.proof_id
            || body.proof.revision != base + 1
            || body.proof.supersedes_revision != Some(base)
        {
            return fail(format!(
                "conflict `{}` body from `{}` must be revision {} of `{}` superseding {base}",
                conflict.id,
                body.device,
                base + 1,
                conflict.proof_id
            ));
        }
    }
    if first.proof == second.proof {
        return fail(format!(
            "conflict `{}` holds identical bodies; there is nothing to resolve",
            conflict.id
        ));
    }
    match (conflict.status, &conflict.chosen_device) {
        (ConflictStatus::Resolved, Some(device))
            if device == &first.device || device == &second.device => {}
        (ConflictStatus::Resolved, _) => {
            return fail(format!(
                "conflict `{}` is resolved but does not choose one of its devices",
                conflict.id
            ))
        }
        (ConflictStatus::Open, Some(_)) => {
            return fail(format!(
                "open conflict `{}` names a chosen device",
                conflict.id
            ))
        }
        (ConflictStatus::Open, None) => {}
    }
    Ok(())
}

fn unique<'a>(collection: &str, ids: impl Iterator<Item = &'a str>) -> Result<(), MemoryError> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(MemoryError::Semantic {
                rule: format!("duplicate `{id}` in {collection}"),
            });
        }
    }
    Ok(())
}
