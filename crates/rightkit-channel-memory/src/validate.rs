//! Parse pipeline and consumer rules. Order: contract name and version, then
//! the JSON Schema, then the typed model, then the semantic rules that JSON
//! Schema cannot express. Each stage refuses before the next one runs.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::model::{ApprovalStatus, ChannelMemory, Proof, Scope};
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
        if let Some(to) = &proof.observed_to {
            if to < &proof.observed_from {
                return fail(format!(
                    "proof `{}` observed_to is before observed_from",
                    proof.id
                ));
            }
        }
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

fn scope_label(scope: &Scope) -> String {
    format!("{}/{}/{}", scope.venture, scope.channel, scope.account)
}

impl ChannelMemory {
    /// Refuses a lookup made for another venture, channel or account.
    pub fn ensure_scope(&self, expected: &Scope) -> Result<(), MemoryError> {
        if &self.scope == expected {
            Ok(())
        } else {
            Err(MemoryError::ForeignScope {
                expected: scope_label(&self.scope),
                found: scope_label(expected),
            })
        }
    }

    /// Proofs that may be cited as facts: approved ones only.
    pub fn approved_proofs(&self) -> impl Iterator<Item = &Proof> {
        self.proofs
            .iter()
            .filter(|proof| proof.approval.status == ApprovalStatus::Approved)
    }

    /// Resolves a cited proof. The citation must name the same value and units
    /// the proof records, so a valid id attached to the wrong number fails.
    pub fn cite_proof(&self, id: &str, value: f64, units: &str) -> Result<&Proof, MemoryError> {
        let proof = self
            .proofs
            .iter()
            .find(|proof| proof.id == id)
            .ok_or_else(|| MemoryError::UnknownProof { id: id.to_string() })?;
        if proof.approval.status != ApprovalStatus::Approved {
            return Err(MemoryError::ProofNotApproved {
                id: id.to_string(),
                status: format!("{:?}", proof.approval.status).to_lowercase(),
            });
        }
        if (proof.value - value).abs() > 1e-9 {
            return Err(MemoryError::ProofMismatch {
                id: id.to_string(),
                reason: format!(
                    "proof records {} but the citation states {value}",
                    proof.value
                ),
            });
        }
        if proof.units != units {
            return Err(MemoryError::ProofMismatch {
                id: id.to_string(),
                reason: format!("proof units are `{}`, citation says `{units}`", proof.units),
            });
        }
        Ok(proof)
    }
}
