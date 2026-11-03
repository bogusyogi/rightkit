//! Itemized per-task unbacked-work gate (FM-188).
//!
//! The previous gate counted a single aggregate `typed_mutations` counter for
//! the whole run. That allowed a single mutated file to back an entire
//! victory claim even when other promised tasks had no evidence. This module
//! makes the gate itemized: every stable task/job identity must present its
//! own typed-mutation evidence, and the gate refuses **only** the tasks that
//! lack it.
//!
//! Stable identity is `TaskNode.id` (and optionally a job id when the
//! orchestrator maps a task to a job). Evidence is the typed-mutation count
//! produced for that identity. The gate tracks attempts per `(run_id, task_id)`
//! so one task hitting the cap does not poison another.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Evidence for a single stable task/job identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEvidence {
    /// Stable identifier — `TaskNode.id` or `job_id` when available.
    pub task_id: String,
    /// Typed mutation count attributed to this task.
    pub typed_mutations: u32,
    /// Whether this task's claim text suggests done/victory.
    pub claim_suggests_done: bool,
    /// Optional stable job identity (e.g. conductor job id). When present the
    /// gate keys on `job_id` for cross-run stability; `task_id` is still the
    /// primary key.
    #[serde(default)]
    pub job_id: Option<String>,
}

impl TaskEvidence {
    pub fn new(
        task_id: impl Into<String>,
        typed_mutations: u32,
        claim_suggests_done: bool,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            typed_mutations,
            claim_suggests_done,
            job_id: None,
        }
    }

    pub fn with_job(mut self, job_id: impl Into<String>) -> Self {
        self.job_id = Some(job_id.into());
        self
    }

    /// Stable key the gate uses for attempt tracking. Prefers `job_id` when
    /// present, otherwise `task_id`. Both are stable across retries.
    pub fn stable_key(&self) -> &str {
        self.job_id.as_deref().unwrap_or(&self.task_id)
    }

    pub fn is_backed(&self) -> bool {
        !self.claim_suggests_done || self.typed_mutations > 0
    }
}

/// Per-task verdict — one entry per stable identity that was claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerTaskVerdict {
    Backed { task_id: String },
    Unbacked { task_id: String, attempts: u32 },
    Refused { task_id: String, reason: String },
}

/// Itemized gate. Tracks `max_unbacked_reenter` attempts per `(run_id, task_id)`.
#[derive(Debug, Clone)]
pub struct UnbackedWorkGate {
    pub max_unbacked_reenter: u32,
    pub fail_closed_on_unbacked: bool,
    per_task_attempts: HashMap<String, HashMap<String, u32>>,
}

impl Default for UnbackedWorkGate {
    fn default() -> Self {
        Self::new(3, true)
    }
}

impl UnbackedWorkGate {
    pub fn new(max_unbacked_reenter: u32, fail_closed_on_unbacked: bool) -> Self {
        Self {
            max_unbacked_reenter,
            fail_closed_on_unbacked,
            per_task_attempts: HashMap::new(),
        }
    }

    pub fn reset_run(&mut self, run_id: &str) {
        self.per_task_attempts.remove(run_id);
    }

    pub fn reset_task(&mut self, run_id: &str, task_id: &str) {
        if let Some(inner) = self.per_task_attempts.get_mut(run_id) {
            inner.remove(task_id);
        }
    }

    /// Check a batch of per-task evidence for `run_id`. Returns one verdict
    /// per claimed task. Unclaimed tasks (claim_suggests_done == false) are
    /// omitted as Backed. Tasks with `typed_mutations > 0` reset their
    /// attempt counter.
    pub fn check(&mut self, run_id: &str, evidences: &[TaskEvidence]) -> Vec<PerTaskVerdict> {
        let entry = self
            .per_task_attempts
            .entry(run_id.to_string())
            .or_default();
        let mut out = Vec::new();
        for ev in evidences {
            if !ev.claim_suggests_done {
                continue;
            }
            let key = ev.stable_key().to_string();
            if ev.typed_mutations > 0 {
                entry.remove(&key);
                out.push(PerTaskVerdict::Backed {
                    task_id: ev.task_id.clone(),
                });
                continue;
            }
            let attempts = entry.entry(key.clone()).or_insert(0);
            if *attempts < self.max_unbacked_reenter {
                *attempts += 1;
                out.push(PerTaskVerdict::Unbacked {
                    task_id: ev.task_id.clone(),
                    attempts: *attempts,
                });
            } else if self.fail_closed_on_unbacked {
                out.push(PerTaskVerdict::Refused {
                    task_id: ev.task_id.clone(),
                    reason: format!(
                        "task {} in run {} reached the unbacked-work cap ({} attempts) without producing typed mutations",
                        ev.task_id, run_id, self.max_unbacked_reenter
                    ),
                });
            } else {
                out.push(PerTaskVerdict::Unbacked {
                    task_id: ev.task_id.clone(),
                    attempts: *attempts,
                });
            }
        }
        // Backed tasks that were claimed and had mutations are already pushed.
        // For the "all backed" case the caller can treat empty or all-Backed as success.
        out
    }

    /// Convenience: true iff every claimed task is backed. Empty input is backed.
    pub fn is_fully_backed(verdicts: &[PerTaskVerdict]) -> bool {
        verdicts
            .iter()
            .all(|v| matches!(v, PerTaskVerdict::Backed { .. }))
    }

    pub fn unbacked_tasks(verdicts: &[PerTaskVerdict]) -> Vec<String> {
        verdicts
            .iter()
            .filter_map(|v| match v {
                PerTaskVerdict::Unbacked { task_id, .. } => Some(task_id.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn refused_tasks(verdicts: &[PerTaskVerdict]) -> Vec<String> {
        verdicts
            .iter()
            .filter_map(|v| match v {
                PerTaskVerdict::Refused { task_id, .. } => Some(task_id.clone()),
                _ => None,
            })
            .collect()
    }
}
