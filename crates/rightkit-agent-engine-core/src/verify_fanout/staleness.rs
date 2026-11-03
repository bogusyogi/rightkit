//! Content-hash staleness binding (PL-007 "content-hash binding", "discard
//! stale findings", Pi "stale-result handling").
//!
//! Every finding records the `sha256` of the file content it was produced
//! against. A finding is *stale* when the bound hash no longer matches the
//! file's current content — including the file being deleted or renamed
//! away. Stale findings are discarded before the report reaches the agent.
//! Donor shape: Pi rejects an edit whose `oldText` no longer matches current
//! file content instead of applying it (`packages/agent/src/harness/tools/
//! edit.ts`, exact-match-against-current-content contract); the same
//! current-content binding decides whether a diagnostic is still true.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::finding::VerifiedFinding;

pub(crate) const HASH_PREFIX: &str = "sha256:";

pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(bytes);
    format!("{HASH_PREFIX}{}", hasher.finalize())
}

/// Hash the given text. This binds a finding to the exact bytes that were
/// verified (used by the structured-config verifier, which parses the same
/// text it hashes).
pub fn content_hash_text(text: &str) -> String {
    hash_bytes(text.as_bytes())
}

/// Hash a file's current content, or `None` when the file cannot be read
/// (missing, deleted, or unreadable).
pub fn content_hash(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(hash_bytes(&bytes))
}

/// Hash every path, skipping unreadable ones.
pub fn content_hashes(paths: &[PathBuf]) -> HashMap<PathBuf, String> {
    let mut map = HashMap::new();
    for path in paths {
        if let Some(hash) = content_hash(path) {
            map.insert(path.clone(), hash);
        }
    }
    map
}

/// Re-check every finding against the file's current content. Returns the
/// kept findings and how many were discarded as stale.
///
/// A finding with no binding (`content_hash == None`) cannot be proven stale
/// and is kept; this only happens for findings whose file vanished before the
/// binding was taken, or workspace-level findings. Every binding this module
/// produces is taken from real content, so `None` is the exceptional case,
/// not the norm.
pub fn discard_stale_findings(findings: Vec<VerifiedFinding>) -> (Vec<VerifiedFinding>, usize) {
    let mut kept = Vec::with_capacity(findings.len());
    let mut discarded = 0usize;
    for finding in findings {
        let current = content_hash(&finding.path);
        let stale = match (&finding.content_hash, current) {
            // Bound hash no longer matches current content.
            (Some(bound), Some(current)) => bound != &current,
            // File vanished after the finding was bound: stale by definition.
            (Some(_), None) => true,
            // Nothing to compare against; keep and be honest about it.
            (None, _) => false,
        };
        if stale {
            discarded += 1;
        } else {
            kept.push(finding);
        }
    }
    (kept, discarded)
}
