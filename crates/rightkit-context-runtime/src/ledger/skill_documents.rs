//! Portable skill-document adapter over Ledger's existing registration,
//! index and exact-resolution mechanisms (LDG-032).
//!
//! Skill bodies live in ordinary repository source files under
//! `tools/skills/<name>/SKILL.md`. Once `doc_spine` has synced the enrolled
//! worktree they are already source-bound Ledger projections: registration
//! stays in `ledger_doc_artifacts`, spans in `ledger_nodes`, and exact reads
//! stay on the ticketed `membrane_source_read` path. This module is only the
//! narrow lane that enumerates those registrations and scopes a query to
//! them — it adds no storage, no second index and no body copies.
//!
//! The adapter inherits every existing safeguard rather than re-implementing
//! them: source policy, erasure fences, generation/revision/span-hash
//! binding, grant narrowing, drift refusal at resolution, and bounded work.
//! Cortex may retain separately admitted durable skill insights; it never
//! owns the authoritative skill-body index.

use super::{
    limits::WorkBudget,
    policy::SourcePolicy,
    query::{self, LedgerHit},
    resolve, LedgerDb,
};
use rightkit_context_protocol::ReadPathV1;
use rightkit_db::rusqlite::params;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Only the skill entrypoint is a skill document; auxiliary files inside a
/// skill directory remain ordinary documents.
pub const SKILL_FILE_NAME: &str = "SKILL.md";
/// Matches the skills provider's published snapshot bound; the adapter never
/// widens it.
pub const MAX_SKILL_DOCUMENTS: usize = 512;
const MAX_SKILL_ID_BYTES: usize = 128;

/// Resolve a registered document path to its portable skill identity.
///
/// Returns `None` for anything that is not exactly
/// `tools/skills/<name>/SKILL.md` under the repository root: a deeper path,
/// a different file name, an empty or traversal-shaped name, or a name that
/// could not travel safely inside resolver arguments.
pub fn skill_id_from_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("tools/skills/")?;
    let (name, file) = rest.split_once('/')?;
    if file != SKILL_FILE_NAME
        || name.is_empty()
        || name.len() > MAX_SKILL_ID_BYTES
        || matches!(name, "." | "..")
        || name.contains('/')
        || name.chars().any(|c| c.is_control() || c == '\\')
    {
        return None;
    }
    Some(name.to_owned())
}

/// One registered skill document's index metadata. Source identity, revision,
/// content hash and publication generation are bound here; bodies are never
/// copied into this catalog.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDocumentEntryV1 {
    pub skill_id: String,
    pub doc_id: String,
    pub path: String,
    pub title: String,
    pub summary: String,
    pub keywords: Vec<String>,
    pub revision: String,
    pub content_hash: String,
    pub ledger_generation: i64,
}

/// A query-lane hit on a registered skill document. The wrapped
/// [`LedgerHit`] carries the complete `membrane_source_read` binding —
/// document/node identity, source reference, expected content hash, expected
/// revision, expected span hash and Ledger generation — so the owner can
/// issue a resolution ticket over it unchanged.
#[derive(Clone, Debug)]
pub struct SkillDocumentHitV1 {
    pub skill_id: String,
    pub title: String,
    pub hit: LedgerHit,
}

/// One skill hit plus its owner-issued resolution ticket — the pair a Pull
/// candidate must carry so exact recovery stays ticket-bound.
#[derive(Clone, Debug)]
pub struct TicketedSkillDocumentV1 {
    pub skill_id: String,
    pub title: String,
    pub hit: LedgerHit,
    pub ticket: String,
}

/// Scoped skill-document search outcome: the underlying [`query::QueryResult`]
/// keeps its completeness, omission, lane and policy receipts while `skills`
/// carries only registered skill-document hits.
#[derive(Debug)]
pub struct SkillDocumentQueryV1 {
    pub result: query::QueryResult,
    pub skills: Vec<SkillDocumentHitV1>,
}

type SkillDocumentRow = (String, String, String, String, String, String, String, i64);

/// Enumerate registered skill documents for one canonical repository root.
///
/// The catalog reuses the same eligibility gates as document recall: the
/// artifact must be active and normal-sensitivity, its path must pass the
/// effective repository policy, and an erasure fence removes it. When the
/// caller carries a task grant, `ranges` narrows the catalog to granted paths
/// — enumeration may never disclose ungranted documents. Rows are index
/// metadata only; bodies are never loaded or returned.
pub(crate) fn catalog(
    db: &LedgerDb,
    root: &str,
    ranges: Option<&[ReadPathV1]>,
    budget: &WorkBudget,
) -> Result<Vec<SkillDocumentEntryV1>, String> {
    let normalized = resolve::normalized_root(Path::new(root)).map_err(|e| e.to_string())?;
    if normalized != root {
        return Err("ledger_root_binding_changed".into());
    }
    let mut policy = SourcePolicy::new(Path::new(&normalized))?;
    let rows: Vec<SkillDocumentRow> = {
        let conn = db.lock();
        let mut statement = conn
            .prepare(
                "SELECT doc_id,path,title,summary,keywords_json,revision,content_hash,
                        index_generation
                 FROM ledger_doc_artifacts
                 WHERE repository_root=?1 AND lifecycle_state='active' AND sensitivity='normal'
                 ORDER BY path",
            )
            .map_err(|e| e.to_string())?;
        let collected = statement
            .query_map([&normalized], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
        collected?
    };
    let mut entries = Vec::new();
    for (doc_id, path, title, summary, keywords_json, revision, content_hash, generation) in rows {
        budget.visit()?;
        let Some(skill_id) = skill_id_from_path(&path) else {
            continue;
        };
        // A caller grant narrows enumeration to granted paths; it can never
        // widen the catalog past registered skill documents.
        if let Some(granted) = ranges {
            if !granted.iter().any(|range| range.path == path) {
                continue;
            }
        }
        if entries.len() >= MAX_SKILL_DOCUMENTS {
            return Err("ledger_skill_catalog_budget_exhausted".into());
        }
        let erased: bool = {
            let conn = db.lock();
            super::erasure::ensure_read_exclusions(&conn)?;
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM ledger_erasure_fences
                 WHERE repository_root=?1 AND path_digest=?2)
                 OR EXISTS(SELECT 1 FROM ledger_read_exclusions WHERE path_digest=?2)",
                params![normalized, resolve::digest(path.as_bytes())],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?
        };
        if erased || !policy.allows(&path, false, budget)? {
            continue;
        }
        entries.push(SkillDocumentEntryV1 {
            skill_id,
            doc_id,
            path,
            title,
            summary,
            keywords: serde_json::from_str(&keywords_json).unwrap_or_default(),
            revision,
            content_hash,
            ledger_generation: generation,
        });
    }
    policy.revalidate(budget)?;
    Ok(entries)
}

/// Search only registered skill documents through the existing scoped query
/// path (LDG-032).
///
/// The adapter's own source set is expressed as a grant-shaped narrowing: with
/// no caller grant the enrolled skill documents are the whole eligible set;
/// with one, only granted ranges on skill paths remain and their line bounds
/// still bind. An empty intersection is fail-closed — it yields no hits, it
/// never widens into the general document set. Eligibility, policy, erasure,
/// source loading, span-hash verification, ranking and graph receipts all run
/// inside [`query::search`]; this adapter only restricts the corpus and
/// re-attaches skill identity to each emitted hit.
pub(crate) fn search(
    db: &LedgerDb,
    root: &str,
    task: &str,
    k: usize,
    grant_ranges: Option<Vec<ReadPathV1>>,
    budget: &WorkBudget,
) -> Result<SkillDocumentQueryV1, String> {
    let entries = catalog(db, root, grant_ranges.as_deref(), budget)?;
    let by_doc: BTreeMap<&str, &SkillDocumentEntryV1> = entries
        .iter()
        .map(|entry| (entry.doc_id.as_str(), entry))
        .collect();
    let ranges: Vec<ReadPathV1> = match grant_ranges {
        // Repository enrollment authorizes every registered skill document.
        None => entries
            .iter()
            .map(|entry| ReadPathV1 {
                path: entry.path.clone(),
                start_line: 1,
                end_line: u32::MAX,
            })
            .collect(),
        // A caller grant narrows further; it can never widen past registered
        // skill documents or past its own line ranges.
        Some(granted) => granted
            .into_iter()
            .filter(|range| by_doc.values().any(|entry| entry.path == range.path))
            .collect(),
    };
    let scope = query::QueryScope {
        root: root.to_owned(),
        ranges: Some(ranges),
    };
    let mut result = query::search(db, &scope, task, k.clamp(1, query::MAX_HITS), false, budget)?;
    let mut skills = Vec::new();
    for hit in result.hits.iter() {
        budget.visit()?;
        match by_doc.get(hit.doc_id.as_str()) {
            Some(entry) => skills.push(SkillDocumentHitV1 {
                skill_id: entry.skill_id.clone(),
                title: if entry.title.is_empty() {
                    entry.skill_id.clone()
                } else {
                    entry.title.clone()
                },
                hit: hit.clone(),
            }),
            // The installed scope already restricts candidates to registered
            // skill paths; a stray hit is recorded as an omission, never
            // silently emitted.
            None => result.omissions.push("skill_hit_unbound".into()),
        }
    }
    if !result.omissions.is_empty() {
        result.omissions.sort();
        result.omissions.dedup();
        result.complete = false;
    }
    Ok(SkillDocumentQueryV1 { result, skills })
}
