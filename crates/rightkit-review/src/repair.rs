//! The repair loop. A repairer proposes a new candidate after a block, up to a
//! hard cap. The loop never calls the repairer more than `cap` times, and it
//! always returns the last inspectable candidate.

use serde::{Deserialize, Serialize};

use crate::extension::RuleRegistry;
use crate::judge::{run_judge, Judge};
use crate::record::{Attempt, VerdictRecord, VERDICT_RECORD_SCHEMA, VERDICT_RECORD_VERSION};
use crate::rules::RuleSet;
use crate::subject::Subject;
use crate::verdict::{disposition, Disposition, Finding};
use crate::ReviewError;

/// The largest repair count any caller may request.
pub const HARD_MAX_REPAIR_ATTEMPTS: u32 = 5;

/// A repair count checked against the hard limit at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairCap(u32);

impl RepairCap {
    pub fn new(max_repairs: u32) -> Result<Self, ReviewError> {
        if max_repairs > HARD_MAX_REPAIR_ATTEMPTS {
            return Err(ReviewError::RepairCapOutOfRange {
                requested: max_repairs,
                max: HARD_MAX_REPAIR_ATTEMPTS,
            });
        }
        Ok(RepairCap(max_repairs))
    }

    pub fn max_repairs(self) -> u32 {
        self.0
    }
}

/// Proposes a repaired candidate from the failing one. `attempt` is one-based.
/// Returns `None` when the repairer cannot make progress.
pub trait Repairer {
    fn repair(
        &mut self,
        candidate: &Subject,
        findings: &[Finding],
        attempt: u32,
    ) -> Option<Subject>;
}

impl<F> Repairer for F
where
    F: FnMut(&Subject, &[Finding], u32) -> Option<Subject>,
{
    fn repair(
        &mut self,
        candidate: &Subject,
        findings: &[Finding],
        attempt: u32,
    ) -> Option<Subject> {
        self(candidate, findings, attempt)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The candidate passed the deterministic rules.
    Passed,
    /// A finding routes the candidate to a human.
    Held,
    /// The candidate still blocks after the cap, so the item stops here.
    RepairCapReached,
    /// The repairer returned no candidate before the cap.
    RepairUnavailable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoopOutcome {
    pub stop: StopReason,
    /// The last candidate evaluated, kept for inspection on a block.
    pub final_candidate: Subject,
    pub record: VerdictRecord,
}

/// Evaluates, repairs while blocked, and stops on pass, hold, cap, or refusal.
/// The judge runs once on the final candidate and is recorded as advisory.
/// Extension kinds find no registry here. Use [`run_repair_loop_with`] for them.
pub fn run_repair_loop(
    rules: &RuleSet,
    initial: Subject,
    cap: RepairCap,
    repairer: &mut dyn Repairer,
    judge: &dyn Judge,
) -> Result<LoopOutcome, ReviewError> {
    run_repair_loop_with(rules, initial, cap, repairer, judge, &RuleRegistry::new())
}

/// As [`run_repair_loop`], resolving extension rule kinds through `registry`.
pub fn run_repair_loop_with(
    rules: &RuleSet,
    initial: Subject,
    cap: RepairCap,
    repairer: &mut dyn Repairer,
    judge: &dyn Judge,
    registry: &RuleRegistry,
) -> Result<LoopOutcome, ReviewError> {
    let mut candidate = initial;
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut repairs_used: u32 = 0;
    let stop = loop {
        let findings = rules.evaluate_with(&candidate, registry)?;
        let current = disposition(&findings);
        attempts.push(Attempt {
            attempt: repairs_used,
            findings: findings.clone(),
            disposition: current,
        });
        match current {
            Disposition::Pass => break StopReason::Passed,
            Disposition::Hold => break StopReason::Held,
            Disposition::Block => {}
        }
        if repairs_used >= cap.max_repairs() {
            break StopReason::RepairCapReached;
        }
        match repairer.repair(&candidate, &findings, repairs_used + 1) {
            Some(next) => {
                repairs_used += 1;
                candidate = next;
            }
            None => break StopReason::RepairUnavailable,
        }
    };
    let advisory = vec![run_judge(judge, &candidate)];
    let final_disposition = attempts
        .last()
        .map(|attempt| attempt.disposition)
        .unwrap_or(Disposition::Block);
    let record = VerdictRecord {
        schema: VERDICT_RECORD_SCHEMA.to_string(),
        version: VERDICT_RECORD_VERSION.to_string(),
        rule_set_id: rules.id.clone(),
        rule_set_version: rules.version,
        artifact_id: candidate.artifact.artifact_id.clone(),
        attempts,
        disposition: final_disposition,
        stop,
        advisory,
    };
    Ok(LoopOutcome {
        stop,
        final_candidate: candidate,
        record,
    })
}
