//! Write-once audit log — an append-only, hash-chained record of
//! governance-relevant events (goal completions, model promotions, safety
//! escalations), per the evidence posture in `docs/architecture/README.md`.
//!
//! Each entry stores the hash of the previous entry plus its own content, so any
//! after-the-fact edit or deletion breaks the chain and is detectable
//! ([`AuditLog::verify`]). Stored in the `audit_log` table of the evolution
//! database.

use crate::api::lock_recover;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::evo_db::EvolutionDb;

/// One immutable audit record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq: u64,
    pub created_at: String,
    pub kind: String,
    pub subject: String,
    pub summary: String,
    /// Hash of the previous entry ("genesis" for the first).
    pub prev_hash: String,
    /// Hash of this entry's content chained onto `prev_hash`.
    pub hash: String,
    /// The agent version that caused this audit event, if known.
    pub agent_version: Option<String>,
}

#[derive(Clone)]
pub struct AuditLog {
    db: EvolutionDb,
    /// (next_seq, last_hash) — kept in memory to chain without re-reading.
    head: Arc<Mutex<(u64, String)>>,
}

const GENESIS: &str = "genesis";

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditLog {
    /// Backed by an ephemeral in-memory database (tests / no home dir).
    pub fn new() -> Self {
        Self::open(EvolutionDb::open_in_memory())
    }

    /// Load chain head from the database (if any) and append future entries.
    pub fn open(db: EvolutionDb) -> Self {
        let (next_seq, last_hash) = {
            let conn = db.lock();
            conn.query_row(
                "SELECT seq, hash FROM audit_log ORDER BY seq DESC LIMIT 1",
                [],
                |r| Ok((r.get::<_, i64>(0)? as u64 + 1, r.get::<_, String>(1)?)),
            )
            .unwrap_or((0, GENESIS.to_string()))
        };
        Self {
            db,
            head: Arc::new(Mutex::new((next_seq, last_hash))),
        }
    }

    /// Append an entry. Returns the recorded entry.
    pub fn append(&self, kind: &str, subject: &str, summary: &str) -> AuditEntry {
        self.append_with_version(kind, subject, summary, None)
    }

    /// Append an entry with an optional agent version. Returns the recorded entry.
    pub fn append_with_version(
        &self,
        kind: &str,
        subject: &str,
        summary: &str,
        agent_version: Option<String>,
    ) -> AuditEntry {
        let created_at = crate::api::now_iso();
        let mut head = lock_recover(&self.head, "self.head");
        let (seq, prev_hash) = (head.0, head.1.clone());
        let hash = chain_hash_with_version(
            &prev_hash,
            seq,
            kind,
            subject,
            summary,
            &created_at,
            &agent_version,
        );
        let entry = AuditEntry {
            seq,
            created_at,
            kind: kind.to_string(),
            subject: subject.to_string(),
            summary: summary.to_string(),
            prev_hash,
            hash: hash.clone(),
            agent_version,
        };
        *head = (seq + 1, hash);
        drop(head);

        let _ = self.db.lock().execute(
            "INSERT OR REPLACE INTO audit_log
                 (seq, created_at, kind, subject, summary, prev_hash, hash, agent_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rightkit_db::rusqlite::params![
                entry.seq as i64,
                entry.created_at,
                entry.kind,
                entry.subject,
                entry.summary,
                entry.prev_hash,
                entry.hash,
                &entry.agent_version,
            ],
        );
        entry
    }

    /// Read all persisted entries, oldest first.
    pub fn entries(&self) -> Vec<AuditEntry> {
        let conn = self.db.lock();
        let Ok(mut stmt) = conn.prepare(
            "SELECT seq, created_at, kind, subject, summary, prev_hash, hash, agent_version
             FROM audit_log ORDER BY seq",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok(AuditEntry {
                seq: r.get::<_, i64>(0)? as u64,
                created_at: r.get(1)?,
                kind: r.get(2)?,
                subject: r.get(3)?,
                summary: r.get(4)?,
                prev_hash: r.get(5)?,
                hash: r.get(6)?,
                agent_version: r.get(7)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// Verify a chain of entries links correctly (tamper detection).
    pub fn verify(entries: &[AuditEntry]) -> bool {
        let mut prev = GENESIS.to_string();
        for (i, e) in entries.iter().enumerate() {
            if e.seq != i as u64 || e.prev_hash != prev {
                return false;
            }
            let expected = chain_hash_with_version(
                &e.prev_hash,
                e.seq,
                &e.kind,
                &e.subject,
                &e.summary,
                &e.created_at,
                &e.agent_version,
            );
            if expected != e.hash {
                return false;
            }
            prev = e.hash.clone();
        }
        true
    }
}

fn chain_hash_with_version(
    prev: &str,
    seq: u64,
    kind: &str,
    subject: &str,
    summary: &str,
    created_at: &str,
    agent_version: &Option<String>,
) -> String {
    let version_str = agent_version.as_deref().unwrap_or("");
    let material = format!("{prev}|{seq}|{kind}|{subject}|{summary}|{created_at}|{version_str}");
    let mut h: u64 = 1469598103934665603;
    for b in material.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    let mut h2 = h ^ 0x9E3779B97F4A7C15;
    h2 = h2.wrapping_mul(0xBF58476D1CE4E5B9);
    format!("{h:016x}{h2:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_chains() {
        let log = AuditLog::new();
        let a = log.append("model_promotion", "coder", "old→new");
        let b = log.append("goal_completed", "s1", "done");
        assert_eq!(a.seq, 0);
        assert_eq!(a.prev_hash, GENESIS);
        assert_eq!(b.seq, 1);
        assert_eq!(b.prev_hash, a.hash, "entry chains onto previous");
        assert!(AuditLog::verify(&[a, b]));
    }

    #[test]
    fn tampering_breaks_verification() {
        let log = AuditLog::new();
        let a = log.append("k", "s", "original");
        let mut b = log.append("k", "s", "second");
        // Tamper with the first entry's summary after the fact.
        let mut tampered_a = a.clone();
        tampered_a.summary = "rewritten".to_string();
        assert!(!AuditLog::verify(&[tampered_a, b.clone()]));
        // Tamper with the chain link.
        b.prev_hash = "forged".to_string();
        assert!(!AuditLog::verify(&[a, b]));
    }

    #[test]
    fn persists_and_resumes_chain() {
        let path = std::env::temp_dir().join(format!("cr-audit-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let last_hash = {
            let db = EvolutionDb::open(&path).unwrap();
            let log = AuditLog::open(db);
            log.append("k", "s", "one");
            log.append("k", "s", "two").hash
        };
        // A fresh instance resumes the chain from the persisted head.
        let db = EvolutionDb::open(&path).unwrap();
        let log2 = AuditLog::open(db);
        let third = log2.append("k", "s", "three");
        assert_eq!(third.seq, 2);
        assert_eq!(third.prev_hash, last_hash);

        // The whole on-disk chain verifies.
        let entries = log2.entries();
        assert_eq!(entries.len(), 3);
        assert!(AuditLog::verify(&entries));
        let _ = std::fs::remove_file(&path);
    }
}
