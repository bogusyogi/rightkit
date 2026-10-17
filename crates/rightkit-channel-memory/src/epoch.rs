//! Review epoch of the facts a content brief cites, and re-entry into review
//! when those facts change.
//!
//! A brief records the epoch of the facts it cites when it passes review. The
//! epoch is derived from the proof bank, so it needs no stored counter: it
//! lists every cited proof as `id@revision:status`, sorted by id. When any
//! cited proof is revised, withdrawn, superseded or removed, the epoch text
//! differs and [`ChannelMemory::brief_reentry`] reports the brief as needing
//! review again, naming the proofs that changed.

use crate::model::{ChannelMemory, Proof, ProofStatus};

fn status_text(status: ProofStatus) -> &'static str {
    match status {
        ProofStatus::Collected => "collected",
        ProofStatus::Approved => "approved",
        ProofStatus::Withdrawn => "withdrawn",
        ProofStatus::Superseded => "superseded",
    }
}

fn entry(proof: &Proof) -> String {
    format!(
        "{}@{}:{}",
        proof.id,
        proof.revision,
        status_text(proof.status)
    )
}

/// Whether a brief's recorded review still covers the facts it cites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BriefReentry {
    /// Every cited proof is unchanged since the review.
    Current,
    /// The facts changed after the review. The brief re-enters review.
    NeedsReview {
        /// Cited proof ids whose revision or status differs, or that are gone.
        changed: Vec<String>,
        /// The epoch to record once the re-review passes.
        epoch: String,
    },
}

impl ChannelMemory {
    /// The review epoch of the cited proofs: `id@revision:status` entries
    /// sorted by id and joined with `;`. An id that is not in the bank is
    /// written as `id@missing`. Duplicate ids count once.
    pub fn review_epoch(&self, proof_ids: &[&str]) -> String {
        let mut ids: Vec<&str> = proof_ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        ids.iter()
            .map(
                |id| match self.proofs.iter().find(|proof| proof.id == *id) {
                    Some(proof) => entry(proof),
                    None => format!("{id}@missing"),
                },
            )
            .collect::<Vec<_>>()
            .join(";")
    }

    /// Compares the epoch recorded when a brief passed review with the
    /// current epoch of the same cited proofs. A difference sends the brief
    /// back to review, listing the proofs that changed.
    pub fn brief_reentry(&self, proof_ids: &[&str], reviewed_epoch: &str) -> BriefReentry {
        let current = self.review_epoch(proof_ids);
        if current == reviewed_epoch {
            return BriefReentry::Current;
        }
        let reviewed: Vec<&str> = reviewed_epoch.split(';').collect();
        let changed = current
            .split(';')
            .filter(|part| !part.is_empty() && !reviewed.contains(part))
            .map(|part| part.split('@').next().unwrap_or(part).to_string())
            .collect::<Vec<_>>();
        BriefReentry::NeedsReview {
            changed,
            epoch: current,
        }
    }
}
