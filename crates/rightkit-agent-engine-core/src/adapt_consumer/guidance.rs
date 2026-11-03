//! Guidance-store seam (PL-011).
//!
//! The engine crate does not depend on the daemon's skills/prompt/doc-policy
//! stores. The host wires a concrete [`GuidanceStore`] over them; the
//! in-memory implementation here is the reference model used by the focused
//! tests.
//!
//! ## CAS contract (the "never silently applies" rule)
//!
//! [`GuidanceStore::replace_if_unchanged`] is a compare-and-swap: the write
//! commits only if the record's *current* digest still equals
//! `expected_digest`. The compare and the write MUST be atomic with respect to
//! every other writer (the in-memory reference holds one mutex across both).
//! A store that writes first and checks later violates this contract; the
//! apply pipeline treats the resulting [`GuidanceStoreError::CasMismatch`] as
//! a typed stale rejection with the store-reported `observed_base_digest`.
//!
//! The append side has its own CAS: [`GuidanceStore::append_if_surface_unchanged`]
//! commits a new record only while the whole-surface digest for the record's
//! kind ([`surface_digest`]) still equals the expected value. Its default
//! implementation fails closed — a store that cannot make the surface compare
//! and the append atomic must refuse appends rather than apply them behind a
//! stale base.
//!
//! ## Replace-first geometry
//!
//! Guidance records own subjects on a dot-path (`review` covers
//! `review.errors`). [`resolve_owners`] finds the owners covering a subject
//! and the *narrowest* one (longest covering subject, exact match wins, ties
//! broken by lexicographically smallest `guidance_id`). The apply pipeline
//! uses this to enforce: refine the narrowest owner, append only when nothing
//! covers the subject.

use super::proposal::{guidance_content_digest, GuidanceKind};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Mutex;

/// A guidance artifact known to the consumer. `digest` is always the lowercase
/// SHA-256 of `content` (see [`super::proposal::guidance_content_digest`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuidanceRecord {
    pub guidance_id: String,
    pub kind: GuidanceKind,
    pub subject: String,
    pub content: String,
    pub digest: String,
}

/// A brand-new record for [`GuidanceStore::append_record`]; the store computes
/// the digest from `content`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGuidanceRecord {
    pub guidance_id: String,
    pub kind: GuidanceKind,
    pub subject: String,
    pub content: String,
}

/// Typed store failures. `CasMismatch` is the stale-base signal: it carries
/// the digest the store observed at commit time so the rejection receipt can
/// point the rebase at the real base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuidanceStoreError {
    NotFound {
        guidance_id: String,
    },
    CasMismatch {
        guidance_id: String,
        observed_base_digest: String,
    },
    AlreadyExists {
        guidance_id: String,
    },
    Backend(String),
}

impl fmt::Display for GuidanceStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GuidanceStoreError::NotFound { guidance_id } => {
                write!(f, "guidance record {guidance_id:?} not found")
            }
            GuidanceStoreError::CasMismatch {
                guidance_id,
                observed_base_digest,
            } => write!(
                f,
                "stale base for guidance record {guidance_id:?}: store observes {observed_base_digest}, expected digest did not match"
            ),
            GuidanceStoreError::AlreadyExists { guidance_id } => {
                write!(f, "guidance record {guidance_id:?} already exists")
            }
            GuidanceStoreError::Backend(e) => write!(f, "guidance store backend failure: {e}"),
        }
    }
}

impl std::error::Error for GuidanceStoreError {}

/// The mutable guidance surface the consumer applies proposals to.
///
/// Implementations MUST make the compare-and-write in
/// [`GuidanceStore::replace_if_unchanged`] atomic against every other writer,
/// and MUST derive record digests exclusively from record content via
/// [`super::proposal::guidance_content_digest`].
pub trait GuidanceStore: Send + Sync {
    /// All records visible to the consumer. Order is unspecified; callers that
    /// need determinism sort by `guidance_id`.
    fn list(&self) -> Result<Vec<GuidanceRecord>, GuidanceStoreError>;

    /// CAS replace of one record's content. Commits only when the record's
    /// current digest equals `expected_digest`; returns the stored record
    /// (with the new content digest) on success.
    fn replace_if_unchanged(
        &self,
        guidance_id: &str,
        expected_digest: &str,
        new_content: &str,
    ) -> Result<GuidanceRecord, GuidanceStoreError>;

    /// Create a new record. Refuses an existing `guidance_id` (the consumer
    /// derives append ids from kind+subject+content digest, so an id collision
    /// implies identical content already exists).
    fn append_record(
        &self,
        record: NewGuidanceRecord,
    ) -> Result<GuidanceRecord, GuidanceStoreError>;

    /// CAS append: commit `record` only while the current surface digest for
    /// `kind` (see [`surface_digest`]) still equals `expected_surface_digest`.
    /// This is the append-side compare-and-swap — an append racing another
    /// surface mutation is rejected as
    /// [`GuidanceStoreError::CasMismatch`] (whose `observed_base_digest`
    /// carries the surface digest observed at commit time; `guidance_id`
    /// carries the id of the refused append), never silently applied. An
    /// existing `guidance_id` is [`GuidanceStoreError::AlreadyExists`].
    ///
    /// The default implementation fails closed: stores without a transactional
    /// surface-compare-and-append refuse every append instead of applying one
    /// behind an unverified base. Stores that can hold the compare and the
    /// append atomically (as the in-memory reference does under one lock)
    /// override this method.
    fn append_if_surface_unchanged(
        &self,
        expected_surface_digest: &str,
        kind: super::proposal::GuidanceKind,
        record: NewGuidanceRecord,
    ) -> Result<GuidanceRecord, GuidanceStoreError> {
        let _ = (expected_surface_digest, kind, &record);
        Err(GuidanceStoreError::Backend(
            "guidance store does not implement CAS append (append_if_surface_unchanged); refusing to append fail-closed".into(),
        ))
    }
}

/// Reference in-memory store. Compare-and-write is atomic (single mutex held
/// across both), matching the CAS contract.
#[derive(Debug, Default)]
pub struct InMemoryGuidanceStore {
    records: Mutex<Vec<GuidanceRecord>>,
}

impl InMemoryGuidanceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed with records (digests are recomputed from content, so callers
    /// cannot seed a content/digest mismatch).
    pub fn from_records(records: Vec<NewGuidanceRecord>) -> Self {
        let store = Self::new();
        for r in records {
            store
                .append_record(r)
                .expect("from_records seeds unique ids");
        }
        store
    }
}

impl GuidanceStore for InMemoryGuidanceStore {
    fn list(&self) -> Result<Vec<GuidanceRecord>, GuidanceStoreError> {
        let records = self
            .records
            .lock()
            .map_err(|_| GuidanceStoreError::Backend("guidance store mutex poisoned".into()))?;
        let mut out = records.clone();
        out.sort_by(|a, b| a.guidance_id.cmp(&b.guidance_id));
        Ok(out)
    }

    fn replace_if_unchanged(
        &self,
        guidance_id: &str,
        expected_digest: &str,
        new_content: &str,
    ) -> Result<GuidanceRecord, GuidanceStoreError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| GuidanceStoreError::Backend("guidance store mutex poisoned".into()))?;
        let record = records
            .iter_mut()
            .find(|r| r.guidance_id == guidance_id)
            .ok_or_else(|| GuidanceStoreError::NotFound {
                guidance_id: guidance_id.to_string(),
            })?;
        // Compare-and-write under one lock: atomic per the trait contract.
        if record.digest != expected_digest {
            return Err(GuidanceStoreError::CasMismatch {
                guidance_id: record.guidance_id.clone(),
                observed_base_digest: record.digest.clone(),
            });
        }
        record.content = new_content.to_string();
        record.digest = guidance_content_digest(new_content);
        Ok(record.clone())
    }

    fn append_record(
        &self,
        record: NewGuidanceRecord,
    ) -> Result<GuidanceRecord, GuidanceStoreError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| GuidanceStoreError::Backend("guidance store mutex poisoned".into()))?;
        if records.iter().any(|r| r.guidance_id == record.guidance_id) {
            return Err(GuidanceStoreError::AlreadyExists {
                guidance_id: record.guidance_id.clone(),
            });
        }
        let digest = guidance_content_digest(&record.content);
        let stored = GuidanceRecord {
            guidance_id: record.guidance_id,
            kind: record.kind,
            subject: record.subject,
            content: record.content,
            digest,
        };
        let _digest = stored.digest.clone();
        records.push(stored.clone());
        Ok(stored)
    }

    fn append_if_surface_unchanged(
        &self,
        expected_surface_digest: &str,
        kind: GuidanceKind,
        record: NewGuidanceRecord,
    ) -> Result<GuidanceRecord, GuidanceStoreError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| GuidanceStoreError::Backend("guidance store mutex poisoned".into()))?;
        // Compare-and-append under one lock: atomic per the trait contract.
        let observed = surface_digest(&records, kind);
        if observed != expected_surface_digest {
            return Err(GuidanceStoreError::CasMismatch {
                guidance_id: record.guidance_id.clone(),
                observed_base_digest: observed,
            });
        }
        if records.iter().any(|r| r.guidance_id == record.guidance_id) {
            return Err(GuidanceStoreError::AlreadyExists {
                guidance_id: record.guidance_id,
            });
        }
        let digest = guidance_content_digest(&record.content);
        let stored = GuidanceRecord {
            guidance_id: record.guidance_id,
            kind: record.kind,
            subject: record.subject,
            content: record.content,
            digest,
        };
        records.push(stored.clone());
        Ok(stored)
    }
}

/// Digest binding a proposal to a whole guidance surface: SHA-256 over the
/// sorted `guidance_id\0digest\n` lines of every record of `kind`. An append
/// proposal's `expected_base_digest` binds this value.
pub fn surface_digest(records: &[GuidanceRecord], kind: GuidanceKind) -> String {
    let mut lines: Vec<String> = records
        .iter()
        .filter(|r| r.kind == kind)
        .map(|r| format!("{}\0{}\n", r.guidance_id, r.digest))
        .collect();
    lines.sort();
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    for line in &lines {
        hasher.update(line.as_bytes());
    }
    hasher.finalize()
}

/// Does `owner_subject` cover `subject`? Exact equality, or `subject` lies
/// strictly below `owner_subject` on the dot-path (`review` covers
/// `review.errors`, not `reviews.x`).
pub fn covers(owner_subject: &str, subject: &str) -> bool {
    if owner_subject == subject {
        return true;
    }
    match subject.strip_prefix(owner_subject) {
        Some(rest) => rest.starts_with('.'),
        None => false,
    }
}

/// Owners covering a subject on one guidance surface.
pub struct OwnerResolution<'a> {
    /// Records whose subject exactly equals the requested subject. More than
    /// one is itself a contradiction (two owners claiming one contract).
    pub exact: Vec<&'a GuidanceRecord>,
    /// The narrowest covering owner, if any: longest covering subject, ties
    /// broken by lexicographically smallest `guidance_id`. Exact matches are
    /// always narrower than any prefix match.
    pub narrowest: Option<&'a GuidanceRecord>,
}

/// Resolve owners of `subject` among records of `kind`.
pub fn resolve_owners<'a>(
    records: &'a [GuidanceRecord],
    kind: GuidanceKind,
    subject: &str,
) -> OwnerResolution<'a> {
    let mut exact: Vec<&GuidanceRecord> = Vec::new();
    let mut covering: Vec<&GuidanceRecord> = Vec::new();
    for record in records {
        if record.kind != kind {
            continue;
        }
        if record.subject == subject {
            exact.push(record);
            covering.push(record);
        } else if covers(&record.subject, subject) {
            covering.push(record);
        }
    }
    let narrowest = covering.iter().copied().min_by(|a, b| {
        // Reverse length: longer subject = narrower = first.
        b.subject
            .len()
            .cmp(&a.subject.len())
            .then_with(|| a.guidance_id.cmp(&b.guidance_id))
    });
    OwnerResolution { exact, narrowest }
}
