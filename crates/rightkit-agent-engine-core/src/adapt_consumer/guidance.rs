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

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, kind: GuidanceKind, subject: &str, content: &str) -> NewGuidanceRecord {
        NewGuidanceRecord {
            guidance_id: id.into(),
            kind,
            subject: subject.into(),
            content: content.into(),
        }
    }

    #[test]
    fn in_memory_store_roundtrips_records() {
        let store = InMemoryGuidanceStore::from_records(vec![record(
            "skill/review",
            GuidanceKind::Skill,
            "review",
            "<!-- adapt:section:core -->\nReview code.\n",
        )]);
        let listed = store.list().expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].guidance_id, "skill/review");
        assert_eq!(
            listed[0].digest,
            guidance_content_digest(&listed[0].content)
        );
    }

    #[test]
    fn cas_replace_commits_on_matching_digest() {
        let store = InMemoryGuidanceStore::from_records(vec![record(
            "skill/review",
            GuidanceKind::Skill,
            "review",
            "<!-- adapt:section:core -->\nReview code.\n",
        )]);
        let listed = store.list().expect("list");
        let before = &listed[0];
        let stored = store
            .replace_if_unchanged(
                "skill/review",
                &before.digest,
                "<!-- adapt:section:core -->\nReview carefully.\n",
            )
            .expect("CAS commit");
        assert_ne!(stored.digest, before.digest);
        assert_eq!(stored.digest, guidance_content_digest(&stored.content));
        assert_eq!(store.list().expect("list")[0].content, stored.content);
    }

    #[test]
    fn cas_replace_rejects_stale_digest_and_leaves_content_unchanged() {
        let store = InMemoryGuidanceStore::from_records(vec![record(
            "skill/review",
            GuidanceKind::Skill,
            "review",
            "<!-- adapt:section:core -->\nReview code.\n",
        )]);
        let original = store.list().expect("list")[0].clone();
        let stale = "0".repeat(64);
        let err = store
            .replace_if_unchanged(
                "skill/review",
                &stale,
                "<!-- adapt:section:core -->\nOverwrite!\n",
            )
            .expect_err("stale CAS must reject");
        match err {
            GuidanceStoreError::CasMismatch {
                guidance_id,
                observed_base_digest,
            } => {
                assert_eq!(guidance_id, "skill/review");
                assert_eq!(observed_base_digest, original.digest);
            }
            other => panic!("expected CasMismatch, got {other:?}"),
        }
        // Never silently applied: content is byte-identical to before.
        assert_eq!(store.list().expect("list")[0], original);
    }

    #[test]
    fn cas_replace_and_append_report_not_found_and_already_exists() {
        let store = InMemoryGuidanceStore::new();
        assert!(matches!(
            store.replace_if_unchanged("nope", &"0".repeat(64), "x"),
            Err(GuidanceStoreError::NotFound { .. })
        ));
        store
            .append_record(record("a", GuidanceKind::Skill, "s", "content"))
            .expect("first append");
        assert!(matches!(
            store.append_record(record("a", GuidanceKind::Skill, "s", "other")),
            Err(GuidanceStoreError::AlreadyExists { .. })
        ));
    }

    #[test]
    fn cas_append_commits_on_matching_surface_digest() {
        let store = InMemoryGuidanceStore::from_records(vec![record(
            "skill/a",
            GuidanceKind::Skill,
            "s.a",
            "A",
        )]);
        let expected = surface_digest(&store.list().expect("list"), GuidanceKind::Skill);
        let stored = store
            .append_if_surface_unchanged(
                &expected,
                GuidanceKind::Skill,
                record("skill/b", GuidanceKind::Skill, "s.b", "B"),
            )
            .expect("CAS append");
        assert_eq!(stored.digest, guidance_content_digest("B"));
        assert_eq!(store.list().expect("list").len(), 2);
    }

    #[test]
    fn cas_append_rejects_stale_surface_digest_and_leaves_store_unchanged() {
        let store = InMemoryGuidanceStore::from_records(vec![record(
            "skill/a",
            GuidanceKind::Skill,
            "s.a",
            "A",
        )]);
        let before = store.list().expect("list");
        let stale = "0".repeat(64);
        let err = store
            .append_if_surface_unchanged(
                &stale,
                GuidanceKind::Skill,
                record("skill/b", GuidanceKind::Skill, "s.b", "Sneaky"),
            )
            .expect_err("stale surface must reject the append");
        match err {
            GuidanceStoreError::CasMismatch {
                guidance_id,
                observed_base_digest,
            } => {
                assert_eq!(
                    guidance_id, "skill/b",
                    "the refused append's id is reported"
                );
                assert_eq!(
                    observed_base_digest,
                    surface_digest(&before, GuidanceKind::Skill)
                );
            }
            other => panic!("expected CasMismatch, got {other:?}"),
        }
        // Never silently applied: the store is byte-identical to before.
        assert_eq!(store.list().expect("list"), before);
    }

    #[test]
    fn cas_append_default_implementation_fails_closed() {
        /// A store that relies on the trait's default CAS append (i.e. one
        /// without a transactional surface-compare-and-append).
        struct MinimalStore {
            inner: InMemoryGuidanceStore,
        }

        impl GuidanceStore for MinimalStore {
            fn list(&self) -> Result<Vec<GuidanceRecord>, GuidanceStoreError> {
                self.inner.list()
            }

            fn replace_if_unchanged(
                &self,
                guidance_id: &str,
                expected_digest: &str,
                new_content: &str,
            ) -> Result<GuidanceRecord, GuidanceStoreError> {
                self.inner
                    .replace_if_unchanged(guidance_id, expected_digest, new_content)
            }

            fn append_record(
                &self,
                record: NewGuidanceRecord,
            ) -> Result<GuidanceRecord, GuidanceStoreError> {
                self.inner.append_record(record)
            }
        }

        let store = MinimalStore {
            inner: InMemoryGuidanceStore::from_records(vec![record(
                "skill/a",
                GuidanceKind::Skill,
                "s.a",
                "A",
            )]),
        };
        let expected = surface_digest(&store.list().expect("list"), GuidanceKind::Skill);
        let err = store
            .append_if_surface_unchanged(
                &expected,
                GuidanceKind::Skill,
                record("skill/b", GuidanceKind::Skill, "s.b", "B"),
            )
            .expect_err("default CAS append must fail closed");
        assert!(
            matches!(&err, GuidanceStoreError::Backend(e) if e.contains("fail-closed")),
            "expected fail-closed backend error, got {err:?}"
        );
        assert_eq!(store.list().expect("list").len(), 1, "nothing was appended");
    }

    #[test]
    fn surface_digest_changes_when_any_record_of_kind_changes() {
        let mut records = vec![
            GuidanceRecord {
                guidance_id: "a".into(),
                kind: GuidanceKind::Skill,
                subject: "s.a".into(),
                content: "A".into(),
                digest: guidance_content_digest("A"),
            },
            GuidanceRecord {
                guidance_id: "b".into(),
                kind: GuidanceKind::SystemPrompt,
                subject: "s.b".into(),
                content: "B".into(),
                digest: guidance_content_digest("B"),
            },
        ];
        let base = surface_digest(&records, GuidanceKind::Skill);
        // Unrelated kind does not move the surface digest.
        records[1].content = "B2".to_string();
        records[1].digest = guidance_content_digest("B2");
        assert_eq!(surface_digest(&records, GuidanceKind::Skill), base);
        // Mutating the kind's record does.
        records[0].content = "A2".to_string();
        records[0].digest = guidance_content_digest("A2");
        assert_ne!(surface_digest(&records, GuidanceKind::Skill), base);
    }

    #[test]
    fn covers_follows_dot_path_semantics() {
        assert!(covers("review", "review"));
        assert!(covers("review", "review.errors"));
        assert!(covers("review", "review.errors.network"));
        assert!(
            !covers("review", "reviews.x"),
            "prefix must respect the dot boundary"
        );
        assert!(!covers("review.errors", "review"));
        assert!(!covers("", "anything"));
    }

    #[test]
    fn resolve_owners_finds_exact_and_narrowest() {
        let records = vec![
            GuidanceRecord {
                guidance_id: "skill/broad".into(),
                kind: GuidanceKind::Skill,
                subject: "review".into(),
                content: "broad".into(),
                digest: guidance_content_digest("broad"),
            },
            GuidanceRecord {
                guidance_id: "skill/narrow".into(),
                kind: GuidanceKind::Skill,
                subject: "review.errors".into(),
                content: "narrow".into(),
                digest: guidance_content_digest("narrow"),
            },
            GuidanceRecord {
                guidance_id: "skill/unrelated".into(),
                kind: GuidanceKind::Skill,
                subject: "deploy".into(),
                content: "deploy".into(),
                digest: guidance_content_digest("deploy"),
            },
            GuidanceRecord {
                guidance_id: "prompt/other-kind".into(),
                kind: GuidanceKind::SystemPrompt,
                subject: "review.errors".into(),
                content: "other kind".into(),
                digest: guidance_content_digest("other kind"),
            },
        ];

        let deep = resolve_owners(&records, GuidanceKind::Skill, "review.errors.network");
        assert_eq!(deep.exact.len(), 0);
        assert_eq!(
            deep.narrowest.map(|r| r.guidance_id.as_str()),
            Some("skill/narrow"),
            "longest covering subject wins"
        );

        let exact = resolve_owners(&records, GuidanceKind::Skill, "review.errors");
        assert_eq!(exact.exact.len(), 1);
        assert_eq!(exact.exact[0].guidance_id, "skill/narrow");
        assert_eq!(
            exact.narrowest.map(|r| r.guidance_id.as_str()),
            Some("skill/narrow")
        );

        let unowned = resolve_owners(&records, GuidanceKind::Skill, "monitoring");
        assert!(unowned.exact.is_empty());
        assert!(unowned.narrowest.is_none());

        // Same subject on a different surface is not an owner here.
        let other_kind = resolve_owners(&records, GuidanceKind::SystemPrompt, "review.errors");
        assert_eq!(other_kind.exact.len(), 1);
    }
}
