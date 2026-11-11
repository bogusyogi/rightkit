// The self-evaluating harness — turns finished conductor runs into durable
// learning signal.
//
// After each run the harness does two things:
// 1. **Observation/evaluation**: tally per-role performance (runs, successes)
//    so the system has a running picture of which agents succeed at which work
//    — the foundation the evaluation/promotion layers build on.
// 2. **Learning**: extract candidate proposals from the run
//    ([`PostSessionLearningExtractor`]), score them ([`EvaluationEngine`]),
//    promote those over threshold, and store them — surfaced via
//    `GET /runtime/learning` (previously a hardcoded empty array).
//
// State is persisted to the `learning_proposals` and `role_stats` tables of
// the evolution database (write-through after each run) so self-improvement
// accumulates across daemon restarts. The pipeline is exercised by unit tests.

use crate::api::lock_recover;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rightkit_agent_engine_core::{
    set_global_self_review_fork, RestrictedSelfReviewFork, SelfReviewInput,
    SelfReviewProposalDraft, SelfReviewProposalKind,
};

use rightkit_agent_learning::{
    DailyAnalyzer, DailyReport, EvaluationEngine, LearningProposalStore,
    PostSessionLearningExtractor, Proposal, ProposalKind, ProposalStatus, ReviewCandidate,
    SessionSummary,
};
use serde::{Deserialize, Serialize};

use crate::evo_db::EvolutionDb;

/// Cap on how many learned lessons are injected into an agent's context, to
/// keep the prompt bounded as the store grows.
const MAX_INJECTED_LESSONS: usize = 12;

/// Running performance tally for one agent role.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RoleStat {
    pub runs: u32,
    pub successes: u32,
}

impl RoleStat {
    pub fn success_rate(&self) -> f64 {
        if self.runs == 0 {
            0.0
        } else {
            self.successes as f64 / self.runs as f64
        }
    }
}

/// Shared, cloneable handle to the harness state.
#[derive(Clone)]
pub struct LearningHarness {
    proposals: Arc<Mutex<LearningProposalStore>>,
    role_stats: Arc<Mutex<HashMap<String, RoleStat>>>,
    /// The evolution database; state is written through after each run so
    /// learning accumulates across daemon restarts.
    db: EvolutionDb,
}

impl Default for LearningHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl LearningHarness {
    /// Backed by an ephemeral in-memory database (tests / no home dir).
    pub fn new() -> Self {
        Self::open(EvolutionDb::open_in_memory())
    }

    /// Load persisted state from `db` and write through to it.
    pub fn open(db: EvolutionDb) -> Self {
        let mut store = LearningProposalStore::new();
        let mut roles = HashMap::new();
        {
            let conn = db.lock();
            let proposal_jsons: Vec<String> = conn
                .prepare("SELECT json FROM learning_proposals")
                .and_then(|mut stmt| {
                    stmt.query_map([], |r| r.get::<_, String>(0))
                        .map(|rows| rows.flatten().collect())
                })
                .unwrap_or_default();
            for json in proposal_jsons {
                if let Ok(p) = serde_json::from_str::<Proposal>(&json) {
                    store.add(p);
                }
            }
            let stat_rows: Vec<(String, i64, i64)> = conn
                .prepare("SELECT role, runs, successes FROM role_stats")
                .and_then(|mut stmt| {
                    stmt.query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    })
                    .map(|rows| rows.flatten().collect())
                })
                .unwrap_or_default();
            for (role, runs, successes) in stat_rows {
                roles.insert(
                    role,
                    RoleStat {
                        runs: runs as u32,
                        successes: successes as u32,
                    },
                );
            }
        }
        let harness = Self {
            proposals: Arc::new(Mutex::new(store)),
            role_stats: Arc::new(Mutex::new(roles)),
            db,
        };
        set_global_self_review_fork(Some(Arc::new(harness.clone())));
        harness
    }

    /// Write current state through to the database (best-effort, transactional).
    fn persist(&self) {
        let proposals: Vec<Proposal> = self
            .proposals
            .lock()
            .unwrap()
            .all()
            .into_iter()
            .cloned()
            .collect();
        let roles = lock_recover(&self.role_stats, "self.role_stats").clone();
        let mut conn = self.db.lock();
        let Ok(tx) = rightkit_db::write_txn(&mut conn) else {
            return;
        };
        let _ = tx.execute("DELETE FROM learning_proposals", []);
        for p in &proposals {
            if let Ok(json) = serde_json::to_string(p) {
                let _ = tx.execute(
                    "INSERT OR REPLACE INTO learning_proposals(id, json) VALUES(?1, ?2)",
                    rightkit_db::rusqlite::params![p.id, json],
                );
            }
        }
        let _ = tx.execute("DELETE FROM role_stats", []);
        for (role, s) in &roles {
            let _ = tx.execute(
                "INSERT OR REPLACE INTO role_stats(role, runs, successes) VALUES(?1, ?2, ?3)",
                rightkit_db::rusqlite::params![role, s.runs as i64, s.successes as i64],
            );
        }
        let _ = tx.commit();
    }

    /// Record a finished run: update per-role stats and extract/score/store
    /// learning proposals. `role_results` is `(role, succeeded)` per worker.
    pub fn record_run(&self, summary: &SessionSummary, role_results: &[(String, bool)]) {
        {
            let mut stats = lock_recover(&self.role_stats, "self.role_stats");
            for (role, ok) in role_results {
                let e = stats.entry(role.clone()).or_default();
                e.runs += 1;
                if *ok {
                    e.successes += 1;
                }
            }
        }

        let extractor = PostSessionLearningExtractor::new();
        let engine = EvaluationEngine::default();
        let mut store = lock_recover(&self.proposals, "self.proposals");
        for (i, mut p) in extractor.extract(summary).into_iter().enumerate() {
            // Re-id so proposals from different sessions never collide.
            p.id = format!("{}-{}-{i}", summary.session_id, p.id);
            // B3: no held-out set plumbed yet — base-only scoring avoids self-evidence leakage
            engine.evaluate(&mut p, None);
            let promote = engine.should_promote(&p);
            let id = p.id.clone();
            store.add(p);
            if promote {
                let _ = store.promote(&id);
            }
        }
        drop(store);
        self.persist();
    }

    /// The promoted lessons worth injecting into future agents' context: the
    /// harness rules ("avoid X") and durable memories the system has learned and
    /// accepted. Capped and most-recent-first. This is what closes the loop —
    /// past mistakes and decisions shape future runs.
    pub fn lessons(&self) -> Vec<String> {
        let store = lock_recover(&self.proposals, "self.proposals");
        let mut promoted: Vec<String> = store
            .by_status(ProposalStatus::Promoted)
            .iter()
            .filter(|p| matches!(p.kind, ProposalKind::HarnessRule | ProposalKind::Memory))
            .map(|p| p.summary.clone())
            .collect();
        if promoted.len() > MAX_INJECTED_LESSONS {
            promoted = promoted.split_off(promoted.len() - MAX_INJECTED_LESSONS);
        }
        promoted
    }

    /// All stored proposals as JSON (for `GET /runtime/learning`).
    pub fn proposals_json(&self) -> Vec<serde_json::Value> {
        self.proposals
            .lock()
            .unwrap()
            .all()
            .iter()
            .map(|p| serde_json::to_value(p).unwrap_or(serde_json::Value::Null))
            .collect()
    }

    /// Add a proposal if it does not already exist, returning the stored row.
    pub fn add_proposal_if_absent(&self, proposal: Proposal) -> Proposal {
        let mut store = lock_recover(&self.proposals, "self.proposals");
        if let Some(existing) = store.get(&proposal.id) {
            return existing.clone();
        }
        store.add(proposal.clone());
        drop(store);
        self.persist();
        proposal
    }

    /// Store a restricted self-review proposal as quarantine-only learning
    /// input. It starts as `Candidate`, never live/promoted.
    pub fn add_self_review_proposal(&self, draft: SelfReviewProposalDraft) -> Proposal {
        self.add_proposal_if_absent(self_review_draft_to_proposal(draft))
    }

    pub fn proposal(&self, id: &str) -> Option<Proposal> {
        lock_recover(&self.proposals, "self.proposals")
            .get(id)
            .cloned()
    }

    /// Approve a proposal (promote it). Returns true if found.
    pub fn approve_proposal(&self, id: &str) -> bool {
        let ok = lock_recover(&self.proposals, "self.proposals")
            .promote(id)
            .is_ok();
        if ok {
            self.persist();
        }
        ok
    }

    /// Reject a proposal. Returns true if found.
    pub fn reject_proposal(&self, id: &str) -> bool {
        let ok = lock_recover(&self.proposals, "self.proposals")
            .reject(id, None)
            .is_ok();
        if ok {
            self.persist();
        }
        ok
    }

    /// Per-role performance metrics as JSON (for `GET /runtime/metrics`).
    pub fn metrics_json(&self) -> Vec<serde_json::Value> {
        let stats = lock_recover(&self.role_stats, "self.role_stats");
        let mut rows: Vec<serde_json::Value> = stats
            .iter()
            .map(|(role, s)| {
                serde_json::json!({
                    "role": role,
                    "runs": s.runs,
                    "successes": s.successes,
                    "successRate": s.success_rate(),
                })
            })
            .collect();
        // Stable order: worst success rate first (most in need of attention).
        rows.sort_by(|a, b| {
            let ar = a["successRate"].as_f64().unwrap_or(0.0);
            let br = b["successRate"].as_f64().unwrap_or(0.0);
            ar.partial_cmp(&br).unwrap_or(std::cmp::Ordering::Equal)
        });
        rows
    }
}

impl RestrictedSelfReviewFork for LearningHarness {
    fn review_and_propose(&self, input: SelfReviewInput) {
        if !input.is_governed() {
            return;
        }
        for draft in extract_self_review_drafts(&input) {
            self.add_self_review_proposal(draft);
        }
    }
}

fn extract_self_review_drafts(input: &SelfReviewInput) -> Vec<SelfReviewProposalDraft> {
    let mut drafts = Vec::new();
    let assistant = input.assistant_text.trim();
    if assistant.is_empty() {
        return drafts;
    }

    for line in assistant.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        let marked = lower
            .strip_prefix("remember:")
            .or_else(|| lower.strip_prefix("learned:"))
            .or_else(|| lower.strip_prefix("correction:"));
        if marked.is_none() {
            continue;
        }
        let summary = trimmed
            .split_once(':')
            .map(|(_, rest)| rest.trim())
            .unwrap_or(trimmed);
        if summary.len() < 8 {
            continue;
        }
        let key = input
            .stable_system_cache_key
            .as_deref()
            .unwrap_or("no-cache-key");
        drafts.push(SelfReviewProposalDraft {
            id: stable_review_proposal_id(&input.session_id, summary),
            kind: SelfReviewProposalKind::Memory,
            summary: summary.to_string(),
            evidence: format!(
                "restricted self-review fork; model={}; tools={}; cache_key={key}; stable_system_bytes={}; transcript_messages={}",
                input.model,
                input.allowed_tools.join(","),
                input.stable_system_prompt_bytes.len(),
                input.transcript.len()
            ),
        });
    }

    drafts
}

fn self_review_draft_to_proposal(draft: SelfReviewProposalDraft) -> Proposal {
    Proposal {
        id: draft.id,
        kind: match draft.kind {
            SelfReviewProposalKind::Memory => ProposalKind::Memory,
            SelfReviewProposalKind::HarnessRule => ProposalKind::HarnessRule,
            SelfReviewProposalKind::Workflow => ProposalKind::Workflow,
            SelfReviewProposalKind::SkillTweak => ProposalKind::SkillTweak,
            SelfReviewProposalKind::Skill => ProposalKind::Skill,
        },
        summary: draft.summary,
        evidence: draft.evidence,
        score: 0.0,
        status: ProposalStatus::Candidate,
        created_at: crate::api::now_iso(),
        age_sessions: 0,
        rejection_reason: None,
    }
}

fn stable_review_proposal_id(session_id: &str, summary: &str) -> String {
    rightkit_agent_engine_core::self_review::stable_self_review_proposal_id(session_id, summary)
}
