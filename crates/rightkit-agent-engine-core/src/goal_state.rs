//! Shared, budget-bounded goal state (F2).
//!
//! Lives behind an `Arc<Mutex<GoalState>>` shared between the engine (which
//! READS it at turn-end to decide whether to continue toward the goal) and the
//! daemon (which WRITES `tokens_used`/`cost_usd` from the `SessionUsageRollup`,
//! i.e. parent + ALL sub-agents — never the parent engine's usage alone).
//!
//! The continuation loop NEVER sets `achieved`; only the jury, via
//! `arbitrate()` at a conductor drive's end, can. This module only decides
//! whether to KEEP GOING or STOP, and why.

use rightkit_agent_engine_protocol::event::{GoalSnapshotV1, GoalStepV1};
use rightkit_agent_engine_protocol::{TaskNode, TaskState};

#[derive(Debug, Clone)]
pub struct GoalState {
    pub id: String,
    pub text: String,
    /// active | achieved | blocked | unmet | paused | budget-limited | usage-limited
    pub status: String,
    pub budget_usd: f64,
    pub budget_tokens: i64,
    // [council P0] Written ONLY by the daemon from SessionUsageRollup. The engine
    // reads these in `may_continue()` and never writes them, so the cap sees the
    // dominant sub-agent spend, not just the parent engine's tokens.
    pub tokens_used: i64,
    pub cost_usd: f64,
    pub continuation_count: i64,
    pub no_progress_count: i64,
    /// Graceful Degradation Score written by the conductor's jury
    /// arbitration. Read by `finalize_goal` to build the `CompletionInput`
    /// instead of hardcoding 1.0. Defaults to 0.0; older goals that predate
    /// this field will simply report 0.0 (rejected), which the jury treats
    /// as not-accepted — the conservative behavior.
    pub gds_score: f64,
    /// Whether a goal_verification artifact exists for this goal. Mirrors
    /// `completion_evidence.verification_present` on the live session but
    /// cached on the goal itself so `finalize_goal` reads both off one
    /// struct when arbitrating. Default `false`.
    pub verification_present: bool,
    /// Whether the auto-jury approved the goal_verification. Default `false`.
    pub verification_approved: bool,
    pub started_at: String,
    pub closed_at: Option<String>,
    pub dismissed_at: Option<String>,
    pub decomposition_state: String,
    pub plan_revision: i64,
    pub now: Option<String>,
    pub next: Option<String>,
    pub steps: Vec<GoalStepV1>,
    pub verification_state: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContinueDecision {
    /// Keep going: inject a continue-toward-goal nudge and re-enter the loop.
    Continue,
    /// Stop this turn but leave the goal open (e.g. paused / user input pending).
    Stop(String),
    /// Close the goal with a terminal status (budget-limited / blocked).
    Close(String),
}

impl GoalState {
    pub const MAX_NO_PROGRESS: i64 = 3; // == RunWatchdog stall threshold
    pub const MAX_CONTINUATIONS: i64 = 25; // backstop, mirrors Codex max_turns

    /// Decide whether the turn-end loop may continue toward the goal. Reads the
    /// daemon-fed rollup totals (`tokens_used`/`cost_usd`), NOT engine usage.
    pub fn may_continue(&self) -> ContinueDecision {
        if self.status == "paused" {
            return ContinueDecision::Stop("paused".into());
        }
        if self.budget_tokens > 0 && self.tokens_used >= self.budget_tokens {
            return ContinueDecision::Close("budget-limited".into());
        }
        if self.budget_usd > 0.0 && self.cost_usd >= self.budget_usd {
            return ContinueDecision::Close("budget-limited".into());
        }
        if self.no_progress_count >= Self::MAX_NO_PROGRESS {
            return ContinueDecision::Close("blocked".into());
        }
        if self.continuation_count >= Self::MAX_CONTINUATIONS {
            return ContinueDecision::Close("blocked".into());
        }
        ContinueDecision::Continue
    }

    /// Refund speculative rollup usage (for example, a cancelled child
    /// attempt). Values saturate at zero and a refund never changes lifecycle
    /// status; terminal goals must remain terminal even if counters are
    /// corrected later. Returns `(tokens_refunded, usd_refunded)`.
    pub fn refund_usage(&mut self, tokens: i64, cost_usd: f64) -> (i64, f64) {
        let tokens_refunded = tokens.max(0).min(self.tokens_used.max(0));
        self.tokens_used = (self.tokens_used - tokens_refunded).max(0);
        let requested_cost = if cost_usd.is_finite() {
            cost_usd.max(0.0)
        } else {
            0.0
        };
        let usd_refunded = requested_cost.min(self.cost_usd.max(0.0));
        self.cost_usd = (self.cost_usd - usd_refunded).max(0.0);
        (tokens_refunded, usd_refunded)
    }

    pub fn snapshot(&self) -> GoalSnapshotV1 {
        let usable = self.decomposition_state == "usable"
            && !self.steps.is_empty()
            && self.steps.iter().all(|step| step.weight > 0)
            && self
                .steps
                .iter()
                .map(|step| &step.id)
                .collect::<std::collections::HashSet<_>>()
                .len()
                == self.steps.len();
        let completion_percent = usable.then(|| {
            let total: i64 = self.steps.iter().map(|step| step.weight).sum();
            let verified: i64 = self
                .steps
                .iter()
                .filter(|step| step.verification_state == "verified")
                .map(|step| step.weight)
                .sum();
            if self.status == "achieved" && self.verification_state == "verified" {
                100
            } else {
                (100 * verified / total).min(99)
            }
        });
        GoalSnapshotV1 {
            goal_id: self.id.clone(),
            objective: self.text.clone(),
            status: self.status.clone(),
            started_at: self.started_at.clone(),
            closed_at: self.closed_at.clone(),
            dismissed_at: self.dismissed_at.clone(),
            decomposition_state: self.decomposition_state.clone(),
            plan_revision: self.plan_revision,
            completion_percent,
            now: self.now.clone(),
            next: self.next.clone(),
            steps: self.steps.clone(),
            verification_state: self.verification_state.clone(),
            budget_tokens: self.budget_tokens,
            tokens_used: self.tokens_used,
            continuation_count: self.continuation_count,
            no_progress_count: self.no_progress_count,
        }
    }

    /// Project the conductor's authoritative task graph into the durable goal.
    pub fn apply_task_graph(&mut self, tasks: &[TaskNode]) -> bool {
        if tasks.is_empty() {
            return false;
        }
        let shape = tasks
            .iter()
            .map(|task| (task.id.clone(), task.title.clone(), 1_i64))
            .collect::<Vec<_>>();
        let old_shape = self
            .steps
            .iter()
            .map(|step| (step.id.clone(), step.label.clone(), step.weight))
            .collect::<Vec<_>>();
        if shape != old_shape {
            self.plan_revision += 1;
        }
        self.decomposition_state = "usable".into();
        self.steps = tasks
            .iter()
            .map(|task| GoalStepV1 {
                id: task.id.clone(),
                label: task.title.clone(),
                weight: 1,
                state: format!("{:?}", task.state).to_lowercase(),
                verification_state: match task.state {
                    TaskState::Done => "verified".into(),
                    TaskState::Failed => "failed".into(),
                    TaskState::Cancelled => "cancelled".into(),
                    _ => "unverified".into(),
                },
            })
            .collect();
        let done = tasks
            .iter()
            .filter(|task| task.state == TaskState::Done)
            .count();
        self.gds_score = done as f64 / tasks.len() as f64;
        let done_ids = tasks
            .iter()
            .filter(|task| task.state == TaskState::Done)
            .map(|task| task.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let current = tasks
            .iter()
            .position(|task| task.state == TaskState::InProgress)
            .or_else(|| {
                tasks.iter().position(|task| {
                    task.state == TaskState::Open
                        && task.deps.iter().all(|dep| done_ids.contains(dep.as_str()))
                })
            });
        self.now = current.map(|index| tasks[index].title.clone());
        self.next = current.and_then(|index| {
            tasks[index + 1..]
                .iter()
                .find(|task| {
                    !matches!(
                        task.state,
                        TaskState::Done | TaskState::Failed | TaskState::Cancelled
                    )
                })
                .map(|task| task.title.clone())
        });
        true
    }
}

/// Stable verifier output describing what remains unresolved. Stable criterion
/// and evidence IDs are authoritative; text is used only as a conservative
/// fallback when no IDs exist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnresolvedGoalGap {
    pub criterion_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub fallback_lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalGapFingerprintClass {
    StableIds,
    ConservativeText,
}

/// Bounded, non-reasoning metadata retained for an unresolved verifier gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalGapFingerprint {
    pub digest: String,
    pub class: GoalGapFingerprintClass,
    pub item_count: usize,
}

impl UnresolvedGoalGap {
    /// Parse the stable, line-oriented contract requested from the independent
    /// verifier. Older/unstructured verifier replies remain usable through a
    /// conservative normalized-text fallback rather than inventing IDs.
    pub fn from_verifier_output(output: &str) -> Self {
        const CRITERION_PREFIX: &str = "UNRESOLVED_CRITERION:";
        const EVIDENCE_PREFIX: &str = "MISSING_EVIDENCE:";

        let mut gap = Self::default();
        for raw_line in output.lines() {
            let line = raw_line.trim();
            if let Some(value) = strip_ascii_case_prefix(line, CRITERION_PREFIX) {
                if !value.trim().is_empty() {
                    gap.criterion_ids.push(value.trim().to_string());
                }
                continue;
            }
            if let Some(value) = strip_ascii_case_prefix(line, EVIDENCE_PREFIX) {
                if !value.trim().is_empty() {
                    gap.evidence_ids.push(value.trim().to_string());
                }
                continue;
            }
            if !line.is_empty()
                && !line.to_ascii_uppercase().starts_with("VERDICT:")
                && !line.to_ascii_uppercase().starts_with("CONFIDENCE:")
            {
                gap.fallback_lines.push(line.to_string());
            }
        }
        gap
    }

    pub fn fingerprint(&self) -> Option<GoalGapFingerprint> {
        let mut criteria = normalize_ids(&self.criterion_ids, false);
        let mut evidence = normalize_ids(&self.evidence_ids, true);
        let (class, items) = if !criteria.is_empty() || !evidence.is_empty() {
            let mut items = Vec::with_capacity(criteria.len() + evidence.len());
            items.extend(criteria.drain(..).map(|id| format!("criterion:{id}")));
            items.extend(evidence.drain(..).map(|id| format!("evidence:{id}")));
            (GoalGapFingerprintClass::StableIds, items)
        } else {
            let mut lines = self
                .fallback_lines
                .iter()
                .filter_map(|line| normalize_fallback_line(line))
                .collect::<Vec<_>>();
            lines.sort();
            lines.dedup();
            (GoalGapFingerprintClass::ConservativeText, lines)
        };
        if items.is_empty() {
            return None;
        }
        let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
        hasher.update(match class {
            GoalGapFingerprintClass::StableIds => b"stable-ids-v1\n".as_slice(),
            GoalGapFingerprintClass::ConservativeText => b"text-v1\n".as_slice(),
        });
        for item in &items {
            hasher.update(item.as_bytes());
            hasher.update(b"\n");
        }
        Some(GoalGapFingerprint {
            digest: format!("sha256:{}", hasher.finalize()),
            class,
            item_count: items.len(),
        })
    }
}

fn strip_ascii_case_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .and_then(|_| value.get(prefix.len()..))
}

fn normalize_ids(values: &[String], location_aware: bool) -> Vec<String> {
    let mut normalized = values
        .iter()
        .filter_map(|value| normalize_id(value, location_aware))
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn normalize_id(value: &str, location_aware: bool) -> Option<String> {
    let mut value = value
        .trim()
        .trim_matches(|character: char| matches!(character, '`' | '"' | '\'' | ',' | ';'))
        .replace('\\', "/")
        .to_lowercase();
    while value.starts_with("./") {
        value.drain(..2);
    }
    while value.contains("//") {
        value = value.replace("//", "/");
    }
    if location_aware {
        value = drop_location_column(value);
    }
    (!value.is_empty()).then_some(value)
}

fn drop_location_column(value: String) -> String {
    let Some((without_last, last)) = value.rsplit_once(':') else {
        return value;
    };
    if last.parse::<u32>().is_err() {
        return value;
    }
    let Some((path, prior)) = without_last.rsplit_once(':') else {
        return value;
    };
    let looks_like_path = path.contains('/')
        || path
            .rsplit('/')
            .next()
            .is_some_and(|name| name.contains('.'));
    if prior.parse::<u32>().is_ok() && looks_like_path {
        without_last.to_string()
    } else {
        value
    }
}

fn normalize_fallback_line(value: &str) -> Option<String> {
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalGapObservation {
    First { count: u32 },
    Repeated { count: u32 },
    Changed { count: u32 },
    Stalled { count: u32 },
    Reset,
    NoFingerprint,
}

/// Tracks only the digest of the most recent unresolved verifier gap.
#[derive(Debug, Clone, Default)]
pub struct GoalGapTracker {
    last: Option<GoalGapFingerprint>,
    same_gap_count: u32,
}

impl GoalGapTracker {
    pub const STALL_THRESHOLD: u32 = 3;

    pub fn same_gap_count(&self) -> u32 {
        self.same_gap_count
    }

    pub fn last_fingerprint(&self) -> Option<&GoalGapFingerprint> {
        self.last.as_ref()
    }

    pub fn reset(&mut self) {
        self.last = None;
        self.same_gap_count = 0;
    }

    pub fn observe(
        &mut self,
        gap: &UnresolvedGoalGap,
        verified_progress: bool,
    ) -> GoalGapObservation {
        if verified_progress {
            self.reset();
            return GoalGapObservation::Reset;
        }
        let Some(next) = gap.fingerprint() else {
            self.reset();
            return GoalGapObservation::NoFingerprint;
        };
        if self.last.as_ref() == Some(&next) {
            self.same_gap_count = self.same_gap_count.saturating_add(1);
            if self.same_gap_count >= Self::STALL_THRESHOLD {
                GoalGapObservation::Stalled {
                    count: self.same_gap_count,
                }
            } else {
                GoalGapObservation::Repeated {
                    count: self.same_gap_count,
                }
            }
        } else {
            let changed = self.last.is_some();
            self.last = Some(next);
            self.same_gap_count = 1;
            if changed {
                GoalGapObservation::Changed { count: 1 }
            } else {
                GoalGapObservation::First { count: 1 }
            }
        }
    }

    /// Apply a verifier observation to the shared goal state. This leaves the
    /// generic `no_progress_count` untouched; the fingerprint trip is an
    /// orthogonal terminal signal.
    pub fn observe_for_goal(
        &mut self,
        goal: &mut GoalState,
        gap: &UnresolvedGoalGap,
        verified_progress: bool,
    ) -> GoalGapObservation {
        let observation = self.observe(gap, verified_progress);
        if matches!(observation, GoalGapObservation::Stalled { .. }) && !verified_progress {
            goal.status = "paused".into();
        }
        observation
    }
}
