//! Resident-owned scheduling mechanics for bounded background review.
//!
//! This module owns eligibility, budgets, cancellation, content-free H5
//! observations, and the fail-closed semantic learner boundary. Learners may
//! return opaque proposal references only; Cortex writes remain outside it.

use rightkit_context_protocol::background_review::{
    BackgroundReviewCursorV1, BackgroundReviewEventRangeV1,
    BackgroundReviewForegroundMemoryStateV1, BackgroundReviewInputSelectionCandidateV1,
    BackgroundReviewInputSelectionSkipReasonV1, BackgroundReviewInputSelectionSkippedV1,
    BackgroundReviewInputSelectionV1, BackgroundReviewProvenanceRefV1,
    BackgroundReviewSessionEventV1, BackgroundSemanticReviewRequestV1,
    BackgroundSemanticReviewResultV1, BackgroundSemanticReviewStatusV1,
};
use rightkit_context_protocol::{
    canonical_json_of, digest_str, BackgroundReviewActivitySignalV1, BackgroundReviewConfigError,
    BackgroundReviewConfigV1, BackgroundReviewExecutionStatusV1, BackgroundReviewExecutionV1,
    BackgroundReviewJobKindV1, BackgroundReviewJobV1, BackgroundReviewObservationReceiptV1,
    BackgroundReviewObservationV1, BackgroundReviewProposalRefV1, BackgroundReviewReasonV1,
    BackgroundReviewStatusV1,
};
use rightkit_memory_core::review::{
    bound_memory_candidate_extraction_window_with_state, select_review_input,
    validate_memory_candidates_for_window, validate_semantic_curation_proposals,
    DeterministicReviewLimitsV1, ForegroundMemoryEmissionV1, ForegroundMemoryStateV1,
    MemoryCandidateExtractionDecisionV1, MemoryCandidateExtractionLimitsV1, MemoryCandidateV1,
    ReviewInputSelectionLimitsV1, SemanticCurationProposalV1, DETERMINISTIC_REVIEW_ANALYZER_ID,
    DETERMINISTIC_REVIEW_ANALYZER_VERSION, REVIEW_INPUT_NOVELTY_FLOOR,
};
use rightkit_memory_core::{EventCursor, SessionEvent};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

/// Environment variable overriding workspace-relative review configuration.
pub const CONFIG_PATH_ENV: &str = "RIGHTKIT_CONTEXT_BACKGROUND_REVIEW_CONFIG";
/// Default fail-closed configuration location beneath workspace root.
pub const DEFAULT_CONFIG_RELATIVE_PATH: &str = ".membrane/background-review.json";
/// One initial attempt plus one retry is permitted for each job id.
pub const MAX_ATTEMPTS: u8 = 2;

/// Result of asking scheduler to admit one job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundReviewDecision {
    Started { attempt: u8 },
    Deferred { reason: BackgroundReviewReasonV1 },
}

/// Terminal result supplied when producer finishes one admitted job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundReviewCompletion {
    Completed,
    Failed,
    FailedWithReason(BackgroundReviewReasonV1),
}

/// Result of converting one host activity signal into one scheduler decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundReviewProduction {
    Started {
        job: BackgroundReviewJobV1,
        attempt: u8,
    },
    Deferred {
        reason: BackgroundReviewReasonV1,
    },
}

/// Learner output is intentionally limited to opaque proposal references. A
/// semantic learner cannot write Cortex through this interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundReviewLearnerResult {
    Proposals(Vec<BackgroundReviewProposalRefV1>),
    Blocked(BackgroundReviewReasonV1),
    Failed(BackgroundReviewReasonV1),
}

/// Semantic learner boundary. Implementations own model/provider access; this
/// runtime never synthesizes learner output.
pub trait BackgroundReviewLearner: Send + Sync {
    fn execute(&self, job: &BackgroundReviewJobV1) -> BackgroundReviewLearnerResult;
}

/// Proposal handoff boundary. Implementations may forward opaque references to
/// a governed proposal/admission path, but cannot receive a Cortex store here.
pub trait BackgroundReviewProposalSink: Send + Sync {
    fn submit(
        &self,
        job: &BackgroundReviewJobV1,
        proposals: &[BackgroundReviewProposalRefV1],
    ) -> Result<(), BackgroundReviewReasonV1>;
}

/// Default learner while no production semantic provider is wired.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSemanticReviewLearner;

impl BackgroundReviewLearner for NoSemanticReviewLearner {
    fn execute(&self, _job: &BackgroundReviewJobV1) -> BackgroundReviewLearnerResult {
        BackgroundReviewLearnerResult::Blocked(BackgroundReviewReasonV1::SemanticProviderNotWired)
    }
}

/// Bounded durable sink for content-free H5 receipts. The path is supplied by
/// the caller so storage ownership remains outside the learner.
pub trait BackgroundReviewObservationSink: Send + Sync {
    fn append(
        &self,
        receipts: &[BackgroundReviewObservationReceiptV1],
    ) -> Result<(), BackgroundReviewSinkError>;
}

#[derive(Debug, thiserror::Error)]
pub enum BackgroundReviewSinkError {
    #[error("background review observation is invalid: {0}")]
    Invalid(String),
    #[error("background review observation JSON could not be encoded: {0}")]
    Encode(String),
    #[error("background review observation sink received too many records")]
    BatchLimit,
    #[error("background review observation exceeds the record byte limit")]
    RecordLimit,
    #[error("background review observation sink is full")]
    FileLimit,
    #[error("background review observation sink IO failed: {0}")]
    Io(String),
}

/// JSONL sink used by the resident when a durable H5 sidecar is configured.
#[derive(Debug, Clone)]
pub struct JsonlBackgroundReviewObservationSink {
    path: PathBuf,
}

/// Environment variable overriding the H5 observation sidecar path.
pub const OBSERVATIONS_PATH_ENV: &str = "RIGHTKIT_CONTEXT_BACKGROUND_REVIEW_OBSERVATIONS";
/// Default H5 observation sidecar beneath workspace root.
pub const DEFAULT_OBSERVATIONS_RELATIVE_PATH: &str =
    ".membrane/background-review-observations.jsonl";
/// Maximum number of receipts written by one append call.
pub const MAX_OBSERVATION_BATCH: usize = 256;
/// Maximum serialized receipt size including no trailing newline.
pub const MAX_OBSERVATION_RECORD_BYTES: usize = 16 * 1024;
/// Maximum durable H5 sidecar size.
pub const MAX_OBSERVATION_FILE_BYTES: u64 = 1024 * 1024;

impl JsonlBackgroundReviewObservationSink {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_workspace_root(workspace_root: impl AsRef<Path>) -> Self {
        let path = std::env::var_os(OBSERVATIONS_PATH_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                workspace_root
                    .as_ref()
                    .join(DEFAULT_OBSERVATIONS_RELATIVE_PATH)
            });
        Self::new(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl BackgroundReviewObservationSink for JsonlBackgroundReviewObservationSink {
    fn append(
        &self,
        receipts: &[BackgroundReviewObservationReceiptV1],
    ) -> Result<(), BackgroundReviewSinkError> {
        if receipts.is_empty() {
            return Ok(());
        }
        if receipts.len() > MAX_OBSERVATION_BATCH {
            return Err(BackgroundReviewSinkError::BatchLimit);
        }
        let mut payload = Vec::new();
        for receipt in receipts {
            receipt
                .validate()
                .map_err(|error| BackgroundReviewSinkError::Invalid(error.to_string()))?;
            let line = serde_json::to_vec(receipt)
                .map_err(|error| BackgroundReviewSinkError::Encode(error.to_string()))?;
            if line.len() > MAX_OBSERVATION_RECORD_BYTES {
                return Err(BackgroundReviewSinkError::RecordLimit);
            }
            payload.extend_from_slice(&line);
            payload.push(b'\n');
        }
        let existing = fs::metadata(&self.path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        // The cap bounds disk, but refusing the write forever is not what
        // bounds it — rotation does. A full sidecar used to kill the
        // observation rail permanently and make the resident log
        // `sink_unavailable` on every tick, with no recovery short of an
        // operator deleting the file. Keep one previous generation and
        // continue: at most two files, and the rail stays alive.
        if existing.saturating_add(payload.len() as u64) > MAX_OBSERVATION_FILE_BYTES {
            if payload.len() as u64 > MAX_OBSERVATION_FILE_BYTES {
                return Err(BackgroundReviewSinkError::FileLimit);
            }
            let rotated = self.path.with_extension("jsonl.1");
            fs::rename(&self.path, &rotated)
                .map_err(|error| BackgroundReviewSinkError::Io(error.to_string()))?;
        }
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .map_err(|error| BackgroundReviewSinkError::Io(error.to_string()))?;
        }
        rightkit_fs::append_record(&self.path, &payload, rightkit_fs::AppendOptions::default())
            .map(|_| ())
            .map_err(|error| BackgroundReviewSinkError::Io(error.to_string()))
    }
}

/// Convert one admitted job into a typed learner execution, then close the
/// scheduler attempt without exposing semantic content to Cortex.
pub fn execute_background_review(
    scheduler: &BackgroundReviewScheduler,
    job: &BackgroundReviewJobV1,
    learner: &dyn BackgroundReviewLearner,
    proposal_sink: Option<&dyn BackgroundReviewProposalSink>,
    observed_at_unix_ms: u64,
) -> BackgroundReviewExecutionV1 {
    if !scheduler.is_active_job(&job.job_id) {
        return BackgroundReviewExecutionV1 {
            schema_version: BackgroundReviewExecutionV1::SCHEMA_VERSION,
            job_id: job.job_id.clone(),
            kind: job.kind,
            status: BackgroundReviewExecutionStatusV1::Blocked,
            proposals: Vec::new(),
            reason: Some(BackgroundReviewReasonV1::InvalidJob),
        };
    }

    let (status, proposals, reason, completion) = match learner.execute(job) {
        BackgroundReviewLearnerResult::Proposals(proposals) => {
            if proposals
                .iter()
                .any(|proposal| proposal.validate().is_err())
            {
                (
                    BackgroundReviewExecutionStatusV1::Failed,
                    Vec::new(),
                    Some(BackgroundReviewReasonV1::InvalidProposal),
                    BackgroundReviewCompletion::FailedWithReason(
                        BackgroundReviewReasonV1::InvalidProposal,
                    ),
                )
            } else if proposals.is_empty() {
                (
                    BackgroundReviewExecutionStatusV1::Proposals,
                    proposals,
                    None,
                    BackgroundReviewCompletion::Completed,
                )
            } else {
                match proposal_sink {
                    Some(sink) => match sink.submit(job, &proposals) {
                        Ok(()) => (
                            BackgroundReviewExecutionStatusV1::Proposals,
                            proposals,
                            None,
                            BackgroundReviewCompletion::Completed,
                        ),
                        Err(reason) => (
                            BackgroundReviewExecutionStatusV1::Failed,
                            Vec::new(),
                            Some(reason),
                            BackgroundReviewCompletion::FailedWithReason(reason),
                        ),
                    },
                    None => (
                        BackgroundReviewExecutionStatusV1::Failed,
                        Vec::new(),
                        Some(BackgroundReviewReasonV1::ProposalSinkUnavailable),
                        BackgroundReviewCompletion::FailedWithReason(
                            BackgroundReviewReasonV1::ProposalSinkUnavailable,
                        ),
                    ),
                }
            }
        }
        BackgroundReviewLearnerResult::Blocked(reason) => (
            BackgroundReviewExecutionStatusV1::Blocked,
            Vec::new(),
            Some(reason),
            BackgroundReviewCompletion::FailedWithReason(reason),
        ),
        BackgroundReviewLearnerResult::Failed(reason) => (
            BackgroundReviewExecutionStatusV1::Failed,
            Vec::new(),
            Some(reason),
            BackgroundReviewCompletion::FailedWithReason(reason),
        ),
    };
    let _ = scheduler.finish_with_completion(&job.job_id, completion, observed_at_unix_ms);
    BackgroundReviewExecutionV1 {
        schema_version: BackgroundReviewExecutionV1::SCHEMA_VERSION,
        job_id: job.job_id.clone(),
        kind: job.kind,
        status,
        proposals,
        reason,
    }
}

/// Activity-to-job adapter. It accepts only host-provided activity and token
/// measurements, and reports missing measurements as typed deferrals.
pub struct BackgroundReviewProducer<'a> {
    scheduler: &'a BackgroundReviewScheduler,
}

impl<'a> BackgroundReviewProducer<'a> {
    pub fn new(scheduler: &'a BackgroundReviewScheduler) -> Self {
        Self { scheduler }
    }

    pub fn admit(
        &self,
        signal: &BackgroundReviewActivitySignalV1,
        kind: BackgroundReviewJobKindV1,
        observed_at_unix_ms: u64,
    ) -> BackgroundReviewProduction {
        if signal.validate().is_err() {
            self.scheduler
                .observe_deferred(BackgroundReviewReasonV1::InvalidJob, observed_at_unix_ms);
            return BackgroundReviewProduction::Deferred {
                reason: BackgroundReviewReasonV1::InvalidJob,
            };
        }
        self.scheduler
            .set_foreground_active(signal.foreground_active, observed_at_unix_ms);
        self.scheduler.record_activity(signal.activity_units);
        let Some(input_tokens) = signal.input_tokens else {
            self.scheduler.observe_deferred(
                BackgroundReviewReasonV1::ModelInputUnavailable,
                observed_at_unix_ms,
            );
            return BackgroundReviewProduction::Deferred {
                reason: BackgroundReviewReasonV1::ModelInputUnavailable,
            };
        };
        let job = BackgroundReviewJobV1 {
            schema_version: BackgroundReviewJobV1::SCHEMA_VERSION,
            job_id: deterministic_job_id(signal, kind),
            kind,
            turn_id: signal.turn_id.clone(),
            input_tokens,
            requested_at_unix_ms: signal.observed_at_unix_ms,
        };
        match self.scheduler.start(job.clone(), observed_at_unix_ms) {
            BackgroundReviewDecision::Started { attempt } => {
                BackgroundReviewProduction::Started { job, attempt }
            }
            BackgroundReviewDecision::Deferred { reason } => {
                BackgroundReviewProduction::Deferred { reason }
            }
        }
    }
}

fn deterministic_job_id(
    signal: &BackgroundReviewActivitySignalV1,
    kind: BackgroundReviewJobKindV1,
) -> String {
    let basis = format!(
        "{}|{}|{}|{}",
        kind.as_str(),
        signal.session_id,
        signal.turn_id,
        signal.observed_at_unix_ms
    );
    format!(
        "background-job-{}",
        digest_str(&basis).trim_start_matches("sha256:")
    )
}

/// Environment variable that once named an external provider endpoint. The
/// resident only runs in-process providers now; adapt status still reads it to
/// report that no external provider is configured.
pub const BACKGROUND_SEMANTIC_PROVIDER_ENDPOINT_ENV: &str =
    "RIGHTKIT_CONTEXT_BACKGROUND_SEMANTIC_PROVIDER_ENDPOINT";
pub const BACKGROUND_SEMANTIC_CAPABILITY: &str = "proposal_only";

/// Input assembled by resident-owned source adapters. Events must already be
/// after cursor; request validation enforces this before provider execution.
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundSemanticReviewInputV1 {
    pub task_id: Option<String>,
    pub cursor: EventCursor,
    pub events: Vec<SessionEvent>,
    /// Events before the cursor form the deterministic nearest-neighbour
    /// baseline; they are never candidates for the current run.
    pub reviewed_baseline: Vec<SessionEvent>,
    pub foreground_memory_state: ForegroundMemoryStateV1,
}

impl BackgroundSemanticReviewInputV1 {
    pub fn session_id(&self) -> &str {
        &self.cursor.session_id
    }
}

/// Proposal-only boundary. Implementations may forward validated values to a
/// governed Cortex admission/review path, but cannot receive a durable store
/// through this interface.
pub trait BackgroundReviewProposalAdmission: Send + Sync {
    fn admit_curation(
        &self,
        _job: &BackgroundReviewJobV1,
        _proposals: &[SemanticCurationProposalV1],
    ) -> Result<(), BackgroundReviewReasonV1> {
        Err(BackgroundReviewReasonV1::ProposalSinkUnavailable)
    }

    fn admit_memory_candidates(
        &self,
        _job: &BackgroundReviewJobV1,
        _candidates: &[MemoryCandidateV1],
    ) -> Result<(), BackgroundReviewReasonV1> {
        Err(BackgroundReviewReasonV1::ProposalSinkUnavailable)
    }

    fn submit_curation(
        &self,
        job: &BackgroundReviewJobV1,
        proposals: &[SemanticCurationProposalV1],
    ) -> Result<(), BackgroundReviewReasonV1> {
        self.admit_curation(job, proposals)
    }

    fn submit_memory_candidates(
        &self,
        job: &BackgroundReviewJobV1,
        candidates: &[MemoryCandidateV1],
    ) -> Result<(), BackgroundReviewReasonV1> {
        self.admit_memory_candidates(job, candidates)
    }

    /// ADP-035: Adapt learner proposals are a distinct kind — not
    /// semantic-curation records and must not be relabeled as such.
    fn admit_adapt_proposals(
        &self,
        _job: &BackgroundReviewJobV1,
        _proposals: &[rightkit_adapt::learner::AdaptLearnerProposalV1],
    ) -> Result<(), BackgroundReviewReasonV1> {
        Err(BackgroundReviewReasonV1::ProposalSinkUnavailable)
    }

    fn submit_adapt_proposals(
        &self,
        job: &BackgroundReviewJobV1,
        proposals: &[rightkit_adapt::learner::AdaptLearnerProposalV1],
    ) -> Result<(), BackgroundReviewReasonV1> {
        self.admit_adapt_proposals(job, proposals)
    }
}

pub use BackgroundReviewProposalAdmission as BackgroundSemanticProposalSink;

/// Proposal queue used when a deployment supplies no in-process Cortex
/// admission object. It records proposal payloads for a governed consumer; it
/// never writes a durable memory record.
#[derive(Debug, Clone)]
pub struct JsonlBackgroundReviewProposalAdmission {
    path: PathBuf,
}

pub const BACKGROUND_REVIEW_PROPOSALS_PATH_ENV: &str =
    "RIGHTKIT_CONTEXT_BACKGROUND_REVIEW_PROPOSALS";
pub const DEFAULT_PROPOSALS_RELATIVE_PATH: &str = ".membrane/background-review-proposals.jsonl";
pub const MAX_PROPOSAL_RECORD_BYTES: usize = 128 * 1024;
pub const MAX_PROPOSAL_FILE_BYTES: u64 = 8 * 1024 * 1024;

impl JsonlBackgroundReviewProposalAdmission {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_workspace_root(workspace_root: impl AsRef<Path>) -> Self {
        let path = std::env::var_os(BACKGROUND_REVIEW_PROPOSALS_PATH_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                workspace_root
                    .as_ref()
                    .join(DEFAULT_PROPOSALS_RELATIVE_PATH)
            });
        Self::new(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn append<T: serde::Serialize>(
        &self,
        job: &BackgroundReviewJobV1,
        kind: &'static str,
        values: &[T],
    ) -> Result<(), BackgroundReviewReasonV1> {
        if values.is_empty() {
            return Ok(());
        }
        let mut bytes = Vec::new();
        for value in values {
            let record = serde_json::json!({
                "schemaVersion": BackgroundReviewExecutionV1::SCHEMA_VERSION,
                "jobId": job.job_id.as_str(),
                "kind": kind,
                "proposal": value,
            });
            let line = serde_json::to_vec(&record)
                .map_err(|_| BackgroundReviewReasonV1::ProposalSinkUnavailable)?;
            if line.len() > MAX_PROPOSAL_RECORD_BYTES {
                return Err(BackgroundReviewReasonV1::ProposalSinkUnavailable);
            }
            bytes.extend_from_slice(&line);
            bytes.push(b'\n');
        }
        let existing = fs::metadata(&self.path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if existing.saturating_add(bytes.len() as u64) > MAX_PROPOSAL_FILE_BYTES {
            return Err(BackgroundReviewReasonV1::ProposalSinkUnavailable);
        }
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .map_err(|_| BackgroundReviewReasonV1::ProposalSinkUnavailable)?;
        }
        rightkit_fs::append_record(&self.path, &bytes, rightkit_fs::AppendOptions::default())
            .map(|_| ())
            .map_err(|_| BackgroundReviewReasonV1::ProposalSinkUnavailable)
    }
}

impl BackgroundReviewProposalAdmission for JsonlBackgroundReviewProposalAdmission {
    fn admit_curation(
        &self,
        job: &BackgroundReviewJobV1,
        proposals: &[SemanticCurationProposalV1],
    ) -> Result<(), BackgroundReviewReasonV1> {
        self.append(job, "semantic_curation", proposals)
    }

    fn admit_memory_candidates(
        &self,
        job: &BackgroundReviewJobV1,
        candidates: &[MemoryCandidateV1],
    ) -> Result<(), BackgroundReviewReasonV1> {
        self.append(job, "memory_candidate", candidates)
    }

    fn admit_adapt_proposals(
        &self,
        job: &BackgroundReviewJobV1,
        proposals: &[rightkit_adapt::learner::AdaptLearnerProposalV1],
    ) -> Result<(), BackgroundReviewReasonV1> {
        self.append(job, "adapt_proposal", proposals)
    }
}

/// In-memory monotonic cursor used by one resident lifetime. A caller may wrap
/// it with a durable cursor source, but cursor advancement remains gated by
/// successful proposal admission in [`execute_background_semantic_review`].
#[derive(Debug, Default)]
pub struct BackgroundReviewCursorStore {
    cursors: Mutex<HashMap<String, EventCursor>>,
}

impl BackgroundReviewCursorStore {
    pub fn get(&self, session_id: &str) -> Result<EventCursor, BackgroundReviewReasonV1> {
        if session_id.trim().is_empty() {
            return Err(BackgroundReviewReasonV1::CursorInputUnavailable);
        }
        let cursors = self
            .cursors
            .lock()
            .map_err(|_| BackgroundReviewReasonV1::CursorInputUnavailable)?;
        Ok(cursors
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| EventCursor {
                session_id: session_id.to_string(),
                last_seq: 0,
            }))
    }

    pub fn set_initial(&self, cursor: EventCursor) -> Result<(), BackgroundReviewReasonV1> {
        if cursor.session_id.trim().is_empty() {
            return Err(BackgroundReviewReasonV1::CursorInputUnavailable);
        }
        let mut cursors = self
            .cursors
            .lock()
            .map_err(|_| BackgroundReviewReasonV1::CursorInputUnavailable)?;
        cursors.entry(cursor.session_id.clone()).or_insert(cursor);
        Ok(())
    }

    pub fn advance(
        &self,
        cursor: &BackgroundReviewCursorV1,
    ) -> Result<(), BackgroundReviewReasonV1> {
        cursor
            .validate()
            .map_err(|_| BackgroundReviewReasonV1::CursorInputUnavailable)?;
        let mut cursors = self
            .cursors
            .lock()
            .map_err(|_| BackgroundReviewReasonV1::CursorInputUnavailable)?;
        let current = cursors
            .entry(cursor.session_id.clone())
            .or_insert_with(|| EventCursor {
                session_id: cursor.session_id.clone(),
                last_seq: 0,
            });
        if cursor.last_seq < current.last_seq {
            return Err(BackgroundReviewReasonV1::CursorInputUnavailable);
        }
        current.last_seq = cursor.last_seq;
        Ok(())
    }
}

/// Typed provider transport failures. No failure is converted into an empty
/// semantic result.
#[derive(Debug, thiserror::Error)]
pub enum BackgroundSemanticReviewProviderError {
    #[error("background semantic provider configuration is unavailable: {0}")]
    Configuration(String),
    #[error("background semantic provider is unavailable: {0}")]
    Unavailable(String),
    #[error("background semantic provider request timed out")]
    Timeout,
    #[error("background semantic provider authentication failed")]
    Authentication,
    #[error("background semantic provider protocol failed: {0}")]
    Protocol(String),
    #[error("background semantic provider returned invalid result: {0}")]
    InvalidResult(String),
    #[error("background semantic provider request encoding failed: {0}")]
    Encode(String),
    #[error("background semantic provider IO failed: {0}")]
    Io(String),
}

/// One provider seam serves Adapt review, Cortex semantic Dream, and Cortex
/// memory extraction.
pub trait BackgroundSemanticReviewProvider: Send + Sync {
    fn execute(
        &self,
        request: &BackgroundSemanticReviewRequestV1,
    ) -> Result<BackgroundSemanticReviewResultV1, BackgroundSemanticReviewProviderError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoBackgroundSemanticReviewProvider;

impl BackgroundSemanticReviewProvider for NoBackgroundSemanticReviewProvider {
    fn execute(
        &self,
        _request: &BackgroundSemanticReviewRequestV1,
    ) -> Result<BackgroundSemanticReviewResultV1, BackgroundSemanticReviewProviderError> {
        Err(BackgroundSemanticReviewProviderError::Unavailable(
            "semantic_provider_not_wired".into(),
        ))
    }
}

/// First-party, in-process, deterministic semantic-review provider.
///
/// It calls no model and opens no socket.  It runs Cortex's own deterministic
/// analyzer ([`rightkit_memory_core::review::analyze_events_for_curation`] and
/// [`rightkit_memory_core::review::extract_bounded_memory_candidate`]) over the
/// request's own event window and returns proposal-only candidates.  Nothing
/// here admits, writes, or advances durable truth: the governed proposal queue
/// in [`execute_background_semantic_review`] remains the only path onward.
///
/// It intentionally refuses [`BackgroundReviewJobKindV1::AdaptBehavioralReview`]:
/// a deterministic duplicate/assertion analyzer is not a behavioral learner and
/// must not pretend to be one.
#[derive(Default)]
pub struct DeterministicFirstPartySemanticReviewProvider {
    limits: DeterministicReviewLimitsV1,
    now_unix_ms: Option<u64>,
}

impl std::fmt::Debug for DeterministicFirstPartySemanticReviewProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeterministicFirstPartySemanticReviewProvider")
            .field("limits", &self.limits)
            .field("now_unix_ms", &self.now_unix_ms)
            .finish()
    }
}

impl DeterministicFirstPartySemanticReviewProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limits(mut self, limits: DeterministicReviewLimitsV1) -> Self {
        self.limits = limits;
        self
    }

    /// Pin the clock used for the deadline check and provenance timestamp.
    pub fn with_now_unix_ms(mut self, now_unix_ms: u64) -> Self {
        self.now_unix_ms = Some(now_unix_ms);
        self
    }

    /// Truthful analyzer identity.  This is not a model name.
    pub fn provider_label() -> String {
        format!("{DETERMINISTIC_REVIEW_ANALYZER_ID}@{DETERMINISTIC_REVIEW_ANALYZER_VERSION}")
    }

    fn now(&self) -> u64 {
        self.now_unix_ms.unwrap_or_else(unix_ms)
    }

    fn result(
        &self,
        request: &BackgroundSemanticReviewRequestV1,
        curation_proposals: Vec<serde_json::Value>,
        memory_candidates: Vec<serde_json::Value>,
        next_cursor: Option<BackgroundReviewCursorV1>,
        status: BackgroundSemanticReviewStatusV1,
    ) -> BackgroundSemanticReviewResultV1 {
        let observed_at_unix_ms = self.now();
        // The receipt binds this result to the exact request bytes it analyzed;
        // there is no upstream host receipt to point at because no external
        // producer was involved.
        let receipt_digest = digest_str(&canonical_json_of(request));
        let provenance_receipt = rightkit_context_protocol::HostObservationProvenanceV1::new(
            format!("{DETERMINISTIC_REVIEW_ANALYZER_ID}-{}", request.job_id),
            // The producer label alone loses job lineage; a caller correlating
            // this refusal against the scheduler attempt that produced it
            // needs the exact job id inside the provenance source, not just
            // the analyzer identity.
            format!("{}/{}", Self::provider_label(), request.job_id),
            observed_at_unix_ms,
            receipt_digest,
        );
        BackgroundSemanticReviewResultV1 {
            schema_version: BackgroundSemanticReviewResultV1::SCHEMA_VERSION,
            job_id: request.job_id.clone(),
            job_kind: request.job_kind,
            session_id: request.session_id.clone(),
            task_id: request.task_id.clone(),
            turn_id: request.turn_id.clone(),
            curation_proposals,
            memory_candidates,
            next_cursor,
            // No model ran, and usage is not measured: claiming either would be
            // a fabrication.
            model: None,
            provider: Some(Self::provider_label()),
            usage: None,
            provenance_receipt,
            status,
        }
    }

    fn refuse(
        &self,
        request: &BackgroundSemanticReviewRequestV1,
        reason: BackgroundReviewReasonV1,
    ) -> BackgroundSemanticReviewResultV1 {
        self.result(
            request,
            Vec::new(),
            Vec::new(),
            None,
            BackgroundSemanticReviewStatusV1::Blocked { reason },
        )
    }
}

/// Flatten one wire event into the analyzer's protocol-neutral view.
fn analyzer_event(
    event: &BackgroundReviewSessionEventV1,
) -> rightkit_memory_core::review::DeterministicReviewEventV1 {
    rightkit_memory_core::review::DeterministicReviewEventV1 {
        event_id: event.event_id.clone(),
        seq: event.seq,
        scope_id: event.scope_id.clone(),
        content_hash: event.content_hash.clone(),
        text: rightkit_memory_core::review::review_payload_text(&event.payload),
    }
}

/// Inverse of [`protocol_event`], so the provider can reuse Cortex's own
/// foreground gate rather than restating it.
fn core_event(event: &BackgroundReviewSessionEventV1) -> SessionEvent {
    SessionEvent {
        schema_version: event.schema_version,
        session_id: event.session_id.clone(),
        seq: event.seq,
        event_id: event.event_id.clone(),
        event_type: event.event_type.clone(),
        payload: event.payload.clone(),
        scope_id: event.scope_id.clone(),
        authority: event.authority.clone(),
        influence_class: event.influence_class.clone(),
        lifecycle: event.lifecycle.clone(),
        retention: event.retention.clone(),
        provenance: event
            .provenance
            .iter()
            .map(|item| rightkit_memory_core::ProvenanceRef {
                source: item.source.clone(),
                source_event_ids: item.source_event_ids.clone(),
                producer: item.producer.clone(),
            })
            .collect(),
        occurred_at_ms: event.occurred_at_ms,
        recorded_at_ms: event.recorded_at_ms,
        content_hash: event.content_hash.clone(),
    }
}

/// Translate the wire foreground signal into Cortex's three-state signal.  The
/// emission id is synthesized from the range because the wire contract carries
/// only the range; identity of the foreground writer is not invented.
fn core_foreground_state(
    state: &BackgroundReviewForegroundMemoryStateV1,
    session_id: &str,
) -> ForegroundMemoryStateV1 {
    match state {
        BackgroundReviewForegroundMemoryStateV1::Unavailable => {
            ForegroundMemoryStateV1::Unavailable
        }
        BackgroundReviewForegroundMemoryStateV1::AvailableNoEmission => {
            ForegroundMemoryStateV1::AvailableNoEmission
        }
        BackgroundReviewForegroundMemoryStateV1::AvailableEmission { range } => {
            ForegroundMemoryStateV1::AvailableEmission(ForegroundMemoryEmissionV1 {
                emission_id: format!(
                    "foreground-emission-{session_id}-{}-{}",
                    range.start_seq, range.end_seq
                ),
                session_id: session_id.to_string(),
                start_seq: range.start_seq,
                end_seq: range.end_seq,
            })
        }
    }
}

impl BackgroundSemanticReviewProvider for DeterministicFirstPartySemanticReviewProvider {
    fn execute(
        &self,
        request: &BackgroundSemanticReviewRequestV1,
    ) -> Result<BackgroundSemanticReviewResultV1, BackgroundSemanticReviewProviderError> {
        request
            .validate()
            .map_err(|error| BackgroundSemanticReviewProviderError::Protocol(error.to_string()))?;
        if !request
            .restricted_capabilities
            .iter()
            .any(|capability| capability == BACKGROUND_SEMANTIC_CAPABILITY)
        {
            return Ok(self.refuse(request, BackgroundReviewReasonV1::InvalidJob));
        }
        if self.now() >= request.deadline_unix_ms {
            return Ok(self.refuse(request, BackgroundReviewReasonV1::TimeGate));
        }
        if request.per_turn_budget_remaining == 0 {
            return Ok(self.refuse(request, BackgroundReviewReasonV1::PerTurnBudgetExceeded));
        }
        if request.aggregate_budget_remaining == 0 {
            return Ok(self.refuse(request, BackgroundReviewReasonV1::AggregateBudgetExceeded));
        }
        let budget = request
            .per_turn_budget_remaining
            .min(request.aggregate_budget_remaining)
            .min(usize::MAX as u64) as usize;

        match request.job_kind {
            // Not a behavioral learner. Refusing is truthful; inventing Adapt
            // categories from lexical rules would not be.
            BackgroundReviewJobKindV1::AdaptBehavioralReview => {
                Ok(self.refuse(request, BackgroundReviewReasonV1::SemanticProviderNotWired))
            }
            // Maintenance jobs never reach a semantic provider; the resident
            // executes them read-only without model input.
            BackgroundReviewJobKindV1::CorpusHealthMaintenance => {
                Ok(self.refuse(request, BackgroundReviewReasonV1::SemanticProviderNotWired))
            }
            BackgroundReviewJobKindV1::CortexSemanticDream => {
                // Budget bounds the window analyzed. Truncation is safe because
                // the cursor only advances over the prefix actually analyzed.
                let mut spent = 0usize;
                let mut analyzed = Vec::new();
                for event in &request.events {
                    let cost = rightkit_memory_core::estimate_tokens(&event.payload.to_string());
                    if spent.saturating_add(cost) > budget {
                        break;
                    }
                    spent = spent.saturating_add(cost);
                    analyzed.push(event);
                }
                if analyzed.is_empty() {
                    return Ok(
                        self.refuse(request, BackgroundReviewReasonV1::PerTurnBudgetExceeded)
                    );
                }
                let events = analyzed
                    .iter()
                    .map(|event| analyzer_event(event))
                    .collect::<Vec<_>>();
                let findings =
                    rightkit_memory_core::review::analyze_events_for_curation(&events, self.limits);
                let mut proposals = Vec::new();
                for proposal in &findings.proposals {
                    let value = serde_json::to_value(proposal).map_err(|error| {
                        BackgroundSemanticReviewProviderError::Encode(error.to_string())
                    })?;
                    proposals.push(value);
                }
                let next_cursor = if proposals.is_empty() {
                    None
                } else {
                    analyzed.last().map(|event| BackgroundReviewCursorV1 {
                        session_id: request.session_id.clone(),
                        last_seq: event.seq,
                    })
                };
                Ok(self.result(
                    request,
                    proposals,
                    Vec::new(),
                    next_cursor,
                    BackgroundSemanticReviewStatusV1::Proposals,
                ))
            }
            BackgroundReviewJobKindV1::CortexMemoryCandidateExtraction => {
                let cursor = EventCursor {
                    session_id: request.cursor.session_id.clone(),
                    last_seq: request.cursor.last_seq,
                };
                let events = request.events.iter().map(core_event).collect::<Vec<_>>();
                let foreground_state =
                    core_foreground_state(&request.foreground_memory_state, &request.session_id);
                let limits = MemoryCandidateExtractionLimitsV1 {
                    max_events:
                        rightkit_context_protocol::background_review::BACKGROUND_SEMANTIC_REVIEW_MAX_EVENTS,
                    max_input_tokens: budget.max(1),
                    max_duration_ms: request.deadline_unix_ms.saturating_sub(self.now()).max(1),
                    max_model_requests: 1,
                };
                // CTX-024's gate, evaluated by Cortex's own function: extraction
                // proceeds only when authoritative foreground memory does not
                // already cover this cursor range.
                let decision = bound_memory_candidate_extraction_window_with_state(
                    &cursor,
                    &events,
                    &foreground_state,
                    limits,
                    true,
                );
                let window = match decision {
                    Ok(MemoryCandidateExtractionDecisionV1::WindowBound { window }) => window,
                    Ok(MemoryCandidateExtractionDecisionV1::Skipped { .. }) => {
                        // Covered by foreground memory, or no new events: a
                        // completed no-op, never a fabricated candidate.
                        return Ok(self.result(
                            request,
                            Vec::new(),
                            Vec::new(),
                            None,
                            BackgroundSemanticReviewStatusV1::Proposals,
                        ));
                    }
                    Ok(MemoryCandidateExtractionDecisionV1::Blocked { reason }) => {
                        let reason = match reason {
                            rightkit_memory_core::review::MemoryCandidateExtractionBlockerV1::ForegroundMemoryEmissionSignalUnavailable => {
                                BackgroundReviewReasonV1::ForegroundMemoryEmissionSignalUnavailable
                            }
                            rightkit_memory_core::review::MemoryCandidateExtractionBlockerV1::ModelInputUnavailable => {
                                BackgroundReviewReasonV1::ModelInputUnavailable
                            }
                            rightkit_memory_core::review::MemoryCandidateExtractionBlockerV1::CursorInputUnavailable => {
                                BackgroundReviewReasonV1::CursorInputUnavailable
                            }
                        };
                        return Ok(self.refuse(request, reason));
                    }
                    // The only errors here are input-budget overruns and cursor
                    // mismatches; both are refusals, never a partial candidate.
                    Err(_) => {
                        return Ok(
                            self.refuse(request, BackgroundReviewReasonV1::PerTurnBudgetExceeded)
                        );
                    }
                };
                let analyzer_events = request
                    .events
                    .iter()
                    .map(analyzer_event)
                    .collect::<Vec<_>>();
                let Some(candidate) =
                    rightkit_memory_core::review::extract_bounded_memory_candidate(
                        &window,
                        &analyzer_events,
                    )
                else {
                    return Ok(self.result(
                        request,
                        Vec::new(),
                        Vec::new(),
                        None,
                        BackgroundSemanticReviewStatusV1::Proposals,
                    ));
                };
                let value = serde_json::to_value(&candidate).map_err(|error| {
                    BackgroundSemanticReviewProviderError::Encode(error.to_string())
                })?;
                let next_cursor = request.events.last().map(|event| BackgroundReviewCursorV1 {
                    session_id: request.session_id.clone(),
                    last_seq: event.seq,
                });
                Ok(self.result(
                    request,
                    Vec::new(),
                    vec![value],
                    next_cursor,
                    BackgroundSemanticReviewStatusV1::Proposals,
                ))
            }
        }
    }
}

/// Build authenticated provider request from Cortex-owned bounded input.
pub fn build_background_semantic_review_request(
    job: &BackgroundReviewJobV1,
    input: &BackgroundSemanticReviewInputV1,
    per_turn_budget_remaining: u64,
    aggregate_budget_remaining: u64,
    deadline_unix_ms: u64,
) -> Result<BackgroundSemanticReviewRequestV1, BackgroundSemanticReviewProviderError> {
    let (selected_events, review_input_selection) =
        if job.kind == BackgroundReviewJobKindV1::AdaptBehavioralReview {
            let max_input_tokens = per_turn_budget_remaining
                .min(aggregate_budget_remaining)
                .min(usize::MAX as u64) as usize;
            let selection = select_review_input(
                &input.cursor,
                &input.events,
                &input.reviewed_baseline,
                ReviewInputSelectionLimitsV1 {
                    max_input_tokens,
                    novelty_floor: REVIEW_INPUT_NOVELTY_FLOOR,
                },
            )
            .map_err(|error| BackgroundSemanticReviewProviderError::Protocol(error.to_string()))?;
            let receipt = protocol_selection(&selection.receipt);
            (selection.events, Some(receipt))
        } else {
            (input.events.clone(), None)
        };
    let events = selected_events
        .iter()
        .map(protocol_event)
        .collect::<Vec<_>>();
    let foreground_memory_state = match &input.foreground_memory_state {
        ForegroundMemoryStateV1::Unavailable => {
            BackgroundReviewForegroundMemoryStateV1::Unavailable
        }
        ForegroundMemoryStateV1::AvailableNoEmission => {
            BackgroundReviewForegroundMemoryStateV1::AvailableNoEmission
        }
        ForegroundMemoryStateV1::AvailableEmission(emission) => {
            BackgroundReviewForegroundMemoryStateV1::AvailableEmission {
                range: BackgroundReviewEventRangeV1 {
                    start_seq: emission.start_seq,
                    end_seq: emission.end_seq,
                },
            }
        }
    };
    let request = BackgroundSemanticReviewRequestV1 {
        schema_version: BackgroundSemanticReviewRequestV1::SCHEMA_VERSION,
        job_id: job.job_id.clone(),
        job_kind: job.kind,
        session_id: input.cursor.session_id.clone(),
        task_id: input.task_id.clone(),
        turn_id: job.turn_id.clone(),
        cursor: BackgroundReviewCursorV1 {
            session_id: input.cursor.session_id.clone(),
            last_seq: input.cursor.last_seq,
        },
        events,
        review_input_selection,
        foreground_memory_state,
        per_turn_budget_remaining,
        aggregate_budget_remaining,
        deadline_unix_ms,
        restricted_capabilities: vec![
            BACKGROUND_SEMANTIC_CAPABILITY.to_string(),
            "no_durable_writes".to_string(),
        ],
    };
    request
        .validate()
        .map_err(|error| BackgroundSemanticReviewProviderError::Protocol(error.to_string()))?;
    Ok(request)
}

fn protocol_selection(
    selection: &rightkit_memory_core::review::ReviewInputSelectionV1,
) -> BackgroundReviewInputSelectionV1 {
    BackgroundReviewInputSelectionV1 {
        schema_version: BackgroundReviewInputSelectionV1::SCHEMA_VERSION,
        candidates_considered: selection
            .candidates_considered
            .iter()
            .map(|candidate| BackgroundReviewInputSelectionCandidateV1 {
                event_id: candidate.event_id.clone(),
                seq: candidate.seq,
                novelty_score: candidate.novelty_score,
                estimated_input_tokens: candidate.estimated_input_tokens,
            })
            .collect(),
        selected: selection.selected.clone(),
        skipped: selection
            .skipped
            .iter()
            .map(|entry| BackgroundReviewInputSelectionSkippedV1 {
                event_id: entry.event_id.clone(),
                reason: match entry.reason {
                    rightkit_memory_core::review::ReviewInputSelectionSkipReasonV1::BudgetExhausted => {
                        BackgroundReviewInputSelectionSkipReasonV1::BudgetExhausted
                    }
                    rightkit_memory_core::review::ReviewInputSelectionSkipReasonV1::BelowNoveltyFloor => {
                        BackgroundReviewInputSelectionSkipReasonV1::BelowNoveltyFloor
                    }
                },
            })
            .collect(),
        quiet_period: selection.quiet_period,
    }
}

fn protocol_event(event: &SessionEvent) -> BackgroundReviewSessionEventV1 {
    BackgroundReviewSessionEventV1 {
        schema_version: event.schema_version,
        session_id: event.session_id.clone(),
        seq: event.seq,
        event_id: event.event_id.clone(),
        event_type: event.event_type.clone(),
        payload: event.payload.clone(),
        scope_id: event.scope_id.clone(),
        authority: event.authority.clone(),
        influence_class: event.influence_class.clone(),
        lifecycle: event.lifecycle.clone(),
        retention: event.retention.clone(),
        provenance: event
            .provenance
            .iter()
            .map(|item| BackgroundReviewProvenanceRefV1 {
                source: item.source.clone(),
                source_event_ids: item.source_event_ids.clone(),
                producer: item.producer.clone(),
            })
            .collect(),
        occurred_at_ms: event.occurred_at_ms,
        recorded_at_ms: event.recorded_at_ms,
        content_hash: event.content_hash.clone(),
    }
}

/// Run one scheduler-admitted semantic attempt. Provider output is parsed and
/// validated before admission; cursor advances only after sink success.
#[allow(clippy::too_many_arguments)]
pub fn execute_background_semantic_review(
    scheduler: &BackgroundReviewScheduler,
    job: &BackgroundReviewJobV1,
    input: &BackgroundSemanticReviewInputV1,
    provider: &dyn BackgroundSemanticReviewProvider,
    proposal_sink: Option<&dyn BackgroundReviewProposalAdmission>,
    cursor_store: &BackgroundReviewCursorStore,
    deadline_unix_ms: u64,
    observed_at_unix_ms: u64,
) -> BackgroundReviewExecutionV1 {
    let blocked = |status: BackgroundReviewExecutionStatusV1,
                   reason: BackgroundReviewReasonV1|
     -> BackgroundReviewExecutionV1 {
        let _ = scheduler.finish_with_completion(
            &job.job_id,
            BackgroundReviewCompletion::FailedWithReason(reason),
            observed_at_unix_ms,
        );
        BackgroundReviewExecutionV1 {
            schema_version: BackgroundReviewExecutionV1::SCHEMA_VERSION,
            job_id: job.job_id.clone(),
            kind: job.kind,
            status,
            proposals: Vec::new(),
            reason: Some(reason),
        }
    };
    if !scheduler.is_active_job(&job.job_id) {
        return BackgroundReviewExecutionV1 {
            schema_version: BackgroundReviewExecutionV1::SCHEMA_VERSION,
            job_id: job.job_id.clone(),
            kind: job.kind,
            status: BackgroundReviewExecutionStatusV1::Blocked,
            proposals: Vec::new(),
            reason: Some(BackgroundReviewReasonV1::InvalidJob),
        };
    }
    let Some((per_turn_budget_remaining, aggregate_budget_remaining)) =
        scheduler.budget_remaining(&job.turn_id)
    else {
        return blocked(
            BackgroundReviewExecutionStatusV1::Blocked,
            BackgroundReviewReasonV1::ModelInputUnavailable,
        );
    };
    let request = match build_background_semantic_review_request(
        job,
        input,
        per_turn_budget_remaining,
        aggregate_budget_remaining,
        deadline_unix_ms,
    ) {
        Ok(request) => request,
        Err(_) => {
            return blocked(
                BackgroundReviewExecutionStatusV1::Failed,
                BackgroundReviewReasonV1::CursorInputUnavailable,
            );
        }
    };

    let extraction_window = if job.kind
        == BackgroundReviewJobKindV1::CortexMemoryCandidateExtraction
    {
        let max_input_tokens = per_turn_budget_remaining
            .min(aggregate_budget_remaining)
            .min(usize::MAX as u64) as usize;
        let limits = MemoryCandidateExtractionLimitsV1 {
            max_events:
                rightkit_context_protocol::background_review::BACKGROUND_SEMANTIC_REVIEW_MAX_EVENTS,
            max_input_tokens: max_input_tokens.max(1),
            max_duration_ms: deadline_unix_ms.saturating_sub(observed_at_unix_ms).max(1),
            max_model_requests: 1,
        };
        match bound_memory_candidate_extraction_window_with_state(
            &input.cursor,
            &input.events,
            &input.foreground_memory_state,
            limits,
            true,
        ) {
            Ok(MemoryCandidateExtractionDecisionV1::WindowBound { window }) => Some(window),
            Ok(MemoryCandidateExtractionDecisionV1::Skipped { .. }) => {
                let _ = scheduler.finish_with_completion(
                    &job.job_id,
                    BackgroundReviewCompletion::Completed,
                    observed_at_unix_ms,
                );
                return BackgroundReviewExecutionV1 {
                    schema_version: BackgroundReviewExecutionV1::SCHEMA_VERSION,
                    job_id: job.job_id.clone(),
                    kind: job.kind,
                    status: BackgroundReviewExecutionStatusV1::Proposals,
                    proposals: Vec::new(),
                    reason: None,
                };
            }
            Ok(MemoryCandidateExtractionDecisionV1::Blocked { reason }) => {
                let reason = match reason {
                    rightkit_memory_core::review::MemoryCandidateExtractionBlockerV1::ModelInputUnavailable => {
                        BackgroundReviewReasonV1::ModelInputUnavailable
                    }
                    rightkit_memory_core::review::MemoryCandidateExtractionBlockerV1::CursorInputUnavailable => {
                        BackgroundReviewReasonV1::CursorInputUnavailable
                    }
                    rightkit_memory_core::review::MemoryCandidateExtractionBlockerV1::ForegroundMemoryEmissionSignalUnavailable => {
                        BackgroundReviewReasonV1::ForegroundMemoryEmissionSignalUnavailable
                    }
                };
                return blocked(BackgroundReviewExecutionStatusV1::Blocked, reason);
            }
            Err(_) => {
                return blocked(
                    BackgroundReviewExecutionStatusV1::Failed,
                    BackgroundReviewReasonV1::InvalidProposal,
                );
            }
        }
    } else {
        None
    };

    // A mechanical quiet period or exhausted selection budget is a completed
    // no-op, not permission to ask a semantic provider to invent input.
    if job.kind == BackgroundReviewJobKindV1::AdaptBehavioralReview
        && request
            .review_input_selection
            .as_ref()
            .is_some_and(|selection| selection.selected.is_empty())
    {
        let _ = scheduler.finish_with_completion(
            &job.job_id,
            BackgroundReviewCompletion::Completed,
            observed_at_unix_ms,
        );
        return BackgroundReviewExecutionV1 {
            schema_version: BackgroundReviewExecutionV1::SCHEMA_VERSION,
            job_id: job.job_id.clone(),
            kind: job.kind,
            status: BackgroundReviewExecutionStatusV1::Proposals,
            proposals: Vec::new(),
            reason: None,
        };
    }

    let result = match provider.execute(&request) {
        Ok(result) => result,
        Err(error) => {
            let reason = match error {
                BackgroundSemanticReviewProviderError::Configuration(_)
                | BackgroundSemanticReviewProviderError::Unavailable(_) => {
                    BackgroundReviewReasonV1::SemanticProviderNotWired
                }
                BackgroundSemanticReviewProviderError::Timeout
                | BackgroundSemanticReviewProviderError::Authentication
                | BackgroundSemanticReviewProviderError::Protocol(_)
                | BackgroundSemanticReviewProviderError::InvalidResult(_)
                | BackgroundSemanticReviewProviderError::Encode(_)
                | BackgroundSemanticReviewProviderError::Io(_) => BackgroundReviewReasonV1::Failed,
            };
            return blocked(BackgroundReviewExecutionStatusV1::Failed, reason);
        }
    };
    if result.validate_against(&request).is_err() {
        return blocked(
            BackgroundReviewExecutionStatusV1::Failed,
            BackgroundReviewReasonV1::InvalidProposal,
        );
    }
    match result.status {
        BackgroundSemanticReviewStatusV1::Blocked { reason } => {
            let reason = execution_reason(reason);
            return blocked(BackgroundReviewExecutionStatusV1::Blocked, reason);
        }
        BackgroundSemanticReviewStatusV1::Failed { reason } => {
            let reason = execution_reason(reason);
            return blocked(BackgroundReviewExecutionStatusV1::Failed, reason);
        }
        BackgroundSemanticReviewStatusV1::Proposals => {}
    }
    let mut refs = Vec::new();
    if job.kind == BackgroundReviewJobKindV1::CortexMemoryCandidateExtraction {
        if !result.curation_proposals.is_empty() {
            return blocked(
                BackgroundReviewExecutionStatusV1::Failed,
                BackgroundReviewReasonV1::InvalidProposal,
            );
        }
        let candidates = result
            .memory_candidates
            .iter()
            .map(|value| serde_json::from_value::<MemoryCandidateV1>(value.clone()))
            .collect::<Result<Vec<_>, _>>();
        let Ok(candidates) = candidates else {
            return blocked(
                BackgroundReviewExecutionStatusV1::Failed,
                BackgroundReviewReasonV1::InvalidProposal,
            );
        };
        let Some(window) = extraction_window.as_ref() else {
            return blocked(
                BackgroundReviewExecutionStatusV1::Failed,
                BackgroundReviewReasonV1::InvalidProposal,
            );
        };
        if validate_memory_candidates_for_window(window, &candidates).is_err() {
            return blocked(
                BackgroundReviewExecutionStatusV1::Failed,
                BackgroundReviewReasonV1::InvalidProposal,
            );
        }
        if !candidates.is_empty() {
            let Some(sink) = proposal_sink else {
                return blocked(
                    BackgroundReviewExecutionStatusV1::Failed,
                    BackgroundReviewReasonV1::ProposalSinkUnavailable,
                );
            };
            if let Err(reason) = sink.submit_memory_candidates(job, &candidates) {
                return blocked(BackgroundReviewExecutionStatusV1::Failed, reason);
            }
            refs.extend(
                candidates
                    .iter()
                    .map(|candidate| proposal_ref(&candidate.candidate_id, candidate)),
            );
        }
    } else if job.kind == BackgroundReviewJobKindV1::AdaptBehavioralReview {
        // ADP-035: Adapt learner proposals are their own kind — they are not
        // semantic-curation records and must not be relabeled as such.
        if !result.memory_candidates.is_empty() {
            return blocked(
                BackgroundReviewExecutionStatusV1::Failed,
                BackgroundReviewReasonV1::InvalidProposal,
            );
        }
        let proposals = result
            .curation_proposals
            .iter()
            .map(|value| {
                serde_json::from_value::<rightkit_adapt::learner::AdaptLearnerProposalV1>(
                    value.clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>();
        let Ok(proposals) = proposals else {
            return blocked(
                BackgroundReviewExecutionStatusV1::Failed,
                BackgroundReviewReasonV1::InvalidProposal,
            );
        };
        if proposals
            .iter()
            .any(|proposal| proposal.validate().is_err())
        {
            return blocked(
                BackgroundReviewExecutionStatusV1::Failed,
                BackgroundReviewReasonV1::InvalidProposal,
            );
        }
        if !proposals.is_empty() {
            let Some(sink) = proposal_sink else {
                return blocked(
                    BackgroundReviewExecutionStatusV1::Failed,
                    BackgroundReviewReasonV1::ProposalSinkUnavailable,
                );
            };
            if let Err(reason) = sink.submit_adapt_proposals(job, &proposals) {
                return blocked(BackgroundReviewExecutionStatusV1::Failed, reason);
            }
            refs.extend(
                proposals
                    .iter()
                    .map(|proposal| proposal_ref(&proposal.proposal_id, proposal)),
            );
        }
    } else {
        if !result.memory_candidates.is_empty() {
            return blocked(
                BackgroundReviewExecutionStatusV1::Failed,
                BackgroundReviewReasonV1::InvalidProposal,
            );
        }
        let proposals = result
            .curation_proposals
            .iter()
            .map(|value| serde_json::from_value::<SemanticCurationProposalV1>(value.clone()))
            .collect::<Result<Vec<_>, _>>();
        let Ok(proposals) = proposals else {
            return blocked(
                BackgroundReviewExecutionStatusV1::Failed,
                BackgroundReviewReasonV1::InvalidProposal,
            );
        };
        if validate_semantic_curation_proposals(&proposals).is_err() {
            return blocked(
                BackgroundReviewExecutionStatusV1::Failed,
                BackgroundReviewReasonV1::InvalidProposal,
            );
        }
        if !proposals.is_empty() {
            let Some(sink) = proposal_sink else {
                return blocked(
                    BackgroundReviewExecutionStatusV1::Failed,
                    BackgroundReviewReasonV1::ProposalSinkUnavailable,
                );
            };
            if let Err(reason) = sink.submit_curation(job, &proposals) {
                return blocked(BackgroundReviewExecutionStatusV1::Failed, reason);
            }
            refs.extend(
                proposals
                    .iter()
                    .map(|proposal| proposal_ref(&proposal.proposal_id, proposal)),
            );
        }
    }
    if refs.len() > 64 {
        return blocked(
            BackgroundReviewExecutionStatusV1::Failed,
            BackgroundReviewReasonV1::InvalidProposal,
        );
    }
    if let Some(next_cursor) = result.next_cursor.as_ref() {
        // Selection may leave a higher-scoring event after an eligible gap.
        // Never let a provider cursor jump over that skipped event: only the
        // selected contiguous prefix is consumable in this run.
        let next_cursor = safe_selected_cursor(&request, next_cursor);
        if refs.is_empty() || cursor_store.advance(&next_cursor).is_err() {
            return blocked(
                BackgroundReviewExecutionStatusV1::Failed,
                BackgroundReviewReasonV1::CursorInputUnavailable,
            );
        }
    }
    let _ = scheduler.finish_with_completion(
        &job.job_id,
        BackgroundReviewCompletion::Completed,
        observed_at_unix_ms,
    );
    BackgroundReviewExecutionV1 {
        schema_version: BackgroundReviewExecutionV1::SCHEMA_VERSION,
        job_id: job.job_id.clone(),
        kind: job.kind,
        status: BackgroundReviewExecutionStatusV1::Proposals,
        proposals: refs,
        reason: None,
    }
}

fn safe_selected_cursor(
    request: &BackgroundSemanticReviewRequestV1,
    proposed: &BackgroundReviewCursorV1,
) -> BackgroundReviewCursorV1 {
    let Some(selection) = request.review_input_selection.as_ref() else {
        return proposed.clone();
    };
    let selected = selection
        .selected
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut contiguous = request.cursor.last_seq;
    let mut candidates = selection.candidates_considered.iter().collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| candidate.seq);
    for candidate in candidates {
        if candidate.seq != contiguous.saturating_add(1) {
            if candidate.seq > contiguous {
                break;
            }
            continue;
        }
        if !selected.contains(candidate.event_id.as_str()) {
            break;
        }
        contiguous = candidate.seq;
    }
    BackgroundReviewCursorV1 {
        session_id: proposed.session_id.clone(),
        last_seq: proposed.last_seq.min(contiguous),
    }
}

fn proposal_ref<T: serde::Serialize>(
    proposal_id: &str,
    proposal: &T,
) -> BackgroundReviewProposalRefV1 {
    BackgroundReviewProposalRefV1 {
        proposal_id: proposal_id.to_string(),
        proposal_digest: digest_str(&canonical_json_of(proposal)),
    }
}

fn execution_reason(reason: BackgroundReviewReasonV1) -> BackgroundReviewReasonV1 {
    match reason {
        BackgroundReviewReasonV1::ModelInputUnavailable
        | BackgroundReviewReasonV1::SemanticProviderNotWired
        | BackgroundReviewReasonV1::ProposalSinkUnavailable
        | BackgroundReviewReasonV1::ForegroundMemoryEmissionSignalUnavailable
        | BackgroundReviewReasonV1::CursorInputUnavailable
        | BackgroundReviewReasonV1::InvalidProposal
        | BackgroundReviewReasonV1::InvalidJob
        | BackgroundReviewReasonV1::Failed => reason,
        _ => BackgroundReviewReasonV1::Failed,
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigState {
    Ready,
    Unavailable,
    Invalid,
}

#[derive(Debug)]
struct ActiveJob {
    job: BackgroundReviewJobV1,
    attempt: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JobIdentity {
    kind: BackgroundReviewJobKindV1,
    turn_id: String,
    input_tokens: u64,
}

impl JobIdentity {
    fn from_job(job: &BackgroundReviewJobV1) -> Self {
        Self {
            kind: job.kind,
            turn_id: job.turn_id.clone(),
            input_tokens: job.input_tokens,
        }
    }
}

#[derive(Debug, Default)]
struct SchedulerState {
    hub_active: bool,
    foreground_active: bool,
    last_started_at_unix_ms: Option<u64>,
    activity_units: u64,
    active: HashMap<BackgroundReviewJobKindV1, ActiveJob>,
    attempts: HashMap<String, u8>,
    job_identities: HashMap<String, JobIdentity>,
    completed_jobs: HashSet<String>,
    turn_input_tokens: HashMap<String, u64>,
    aggregate_input_tokens: u64,
    observations: VecDeque<BackgroundReviewObservationV1>,
    /// Identity `(reason, job_id, kind)` of the most recently emitted deferral.
    /// The resident re-evaluates persistent host states every 250 ms (hub
    /// inactive, foreground preemption, missing or stale input snapshot,
    /// exhausted budgets); recording every identical evaluation would rotate
    /// the bounded sidecar into uselessness and drown real transitions.
    /// Deferrals therefore emit on state change only; any non-deferred
    /// observation clears the marker so a later identical deferral records
    /// again.
    last_deferral: Option<(
        BackgroundReviewReasonV1,
        Option<String>,
        Option<BackgroundReviewJobKindV1>,
    )>,
}

/// Thread-safe scheduler state owned by the resident.
pub struct BackgroundReviewScheduler {
    config: Option<BackgroundReviewConfigV1>,
    config_state: ConfigState,
    state: Mutex<SchedulerState>,
}

impl std::fmt::Debug for BackgroundReviewScheduler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackgroundReviewScheduler")
            .field("config", &self.config)
            .field("config_state", &self.config_state)
            .finish_non_exhaustive()
    }
}

impl BackgroundReviewScheduler {
    /// Construct scheduler from already validated configuration.
    pub fn new(config: BackgroundReviewConfigV1) -> Result<Self, BackgroundReviewConfigError> {
        config
            .validate()
            .map_err(BackgroundReviewConfigError::Invalid)?;
        Ok(Self {
            config: Some(config),
            config_state: ConfigState::Ready,
            state: Mutex::new(SchedulerState::default()),
        })
    }

    /// Construct fail-closed scheduler that records startup reason.
    pub fn disabled(reason: BackgroundReviewReasonV1, observed_at_unix_ms: u64) -> Self {
        let config_state = match reason {
            BackgroundReviewReasonV1::ConfigUnavailable => ConfigState::Unavailable,
            BackgroundReviewReasonV1::ConfigInvalid => ConfigState::Invalid,
            _ => ConfigState::Invalid,
        };
        let scheduler = Self {
            config: None,
            config_state,
            state: Mutex::new(SchedulerState::default()),
        };
        scheduler.record_observation(
            None,
            None,
            BackgroundReviewStatusV1::Deferred,
            reason,
            observed_at_unix_ms,
            0,
            0,
        );
        scheduler
    }

    /// Read configuration from path; unreadable or invalid input never runs a job.
    pub fn from_config_path(path: impl AsRef<Path>, observed_at_unix_ms: u64) -> Self {
        let path = path.as_ref();
        match fs::read_to_string(path) {
            Ok(contents) => match BackgroundReviewConfigV1::from_json(&contents) {
                Ok(config) => Self::new(config).unwrap_or_else(|_| {
                    Self::disabled(BackgroundReviewReasonV1::ConfigInvalid, observed_at_unix_ms)
                }),
                Err(BackgroundReviewConfigError::Invalid(_))
                | Err(BackgroundReviewConfigError::Json(_))
                | Err(BackgroundReviewConfigError::Io(_)) => {
                    Self::disabled(BackgroundReviewReasonV1::ConfigInvalid, observed_at_unix_ms)
                }
            },
            Err(_) => Self::disabled(
                BackgroundReviewReasonV1::ConfigUnavailable,
                observed_at_unix_ms,
            ),
        }
    }

    /// Resolve workspace configuration path, honoring explicit environment override.
    pub fn config_path_for_workspace(workspace_root: impl AsRef<Path>) -> PathBuf {
        std::env::var_os(CONFIG_PATH_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace_root.as_ref().join(DEFAULT_CONFIG_RELATIVE_PATH))
    }

    /// Read fail-closed configuration beneath workspace root.
    pub fn from_workspace_root(workspace_root: impl AsRef<Path>, observed_at_unix_ms: u64) -> Self {
        let path = Self::config_path_for_workspace(workspace_root);
        Self::from_config_path(path, observed_at_unix_ms)
    }

    /// Return loaded configuration, if configuration was readable and valid.
    pub fn config(&self) -> Option<BackgroundReviewConfigV1> {
        self.config.clone()
    }

    /// Mark whether background work is authorized. Deactivation cancels all admitted work
    /// synchronously.
    pub fn set_hub_active(&self, active: bool, observed_at_unix_ms: u64) {
        let mut state = self.state.lock().expect("background review state poisoned");
        let was_active = state.hub_active;
        state.hub_active = active;
        if active || !was_active {
            return;
        }
        self.cancel_all_locked(
            &mut state,
            BackgroundReviewReasonV1::HubInactive,
            observed_at_unix_ms,
        );
    }

    /// Mark foreground work. Starting foreground work pre-empts admitted review jobs.
    pub fn set_foreground_active(&self, active: bool, observed_at_unix_ms: u64) {
        let mut state = self.state.lock().expect("background review state poisoned");
        state.foreground_active = active;
        if active {
            self.cancel_all_locked(
                &mut state,
                BackgroundReviewReasonV1::ForegroundPreempted,
                observed_at_unix_ms,
            );
        }
    }

    /// Add activity units used by activity gate. Counter saturates on overflow.
    pub fn record_activity(&self, activity_units: u64) {
        let mut state = self.state.lock().expect("background review state poisoned");
        state.activity_units = state.activity_units.saturating_add(activity_units);
    }

    /// Report why no review was admitted at an idle checkpoint.
    pub fn observe_idle(&self, observed_at_unix_ms: u64) {
        let mut state = self.state.lock().expect("background review state poisoned");
        let reason = match self.config_state {
            ConfigState::Unavailable => BackgroundReviewReasonV1::ConfigUnavailable,
            ConfigState::Invalid => BackgroundReviewReasonV1::ConfigInvalid,
            ConfigState::Ready if !self.config.as_ref().is_some_and(|config| config.enabled) => {
                BackgroundReviewReasonV1::Disabled
            }
            ConfigState::Ready if !state.hub_active => BackgroundReviewReasonV1::HubInactive,
            ConfigState::Ready if state.foreground_active => {
                BackgroundReviewReasonV1::ForegroundPreempted
            }
            ConfigState::Ready if !state.active.is_empty() => {
                BackgroundReviewReasonV1::AlreadyRunning
            }
            ConfigState::Ready
                if state.last_started_at_unix_ms.is_some_and(|started| {
                    observed_at_unix_ms.saturating_sub(started)
                        < self
                            .config
                            .as_ref()
                            .expect("ready configuration exists")
                            .min_elapsed_ms
                }) =>
            {
                BackgroundReviewReasonV1::TimeGate
            }
            ConfigState::Ready
                if state.activity_units
                    < self
                        .config
                        .as_ref()
                        .expect("ready configuration exists")
                        .activity_threshold =>
            {
                BackgroundReviewReasonV1::ActivityGate
            }
            ConfigState::Ready => BackgroundReviewReasonV1::NoEligibleWork,
        };
        self.observe_locked(
            &mut state,
            None,
            None,
            BackgroundReviewStatusV1::Deferred,
            reason,
            observed_at_unix_ms,
            0,
            0,
            None,
        );
    }

    /// Ask scheduler to admit one resident-owned job.
    pub fn start(
        &self,
        job: BackgroundReviewJobV1,
        observed_at_unix_ms: u64,
    ) -> BackgroundReviewDecision {
        let mut state = self.state.lock().expect("background review state poisoned");
        if job.validate().is_err() {
            return self.defer_locked(
                &mut state,
                &job,
                BackgroundReviewReasonV1::InvalidJob,
                observed_at_unix_ms,
            );
        }
        let Some(config) = self.config.as_ref() else {
            let reason = match self.config_state {
                ConfigState::Unavailable => BackgroundReviewReasonV1::ConfigUnavailable,
                ConfigState::Invalid | ConfigState::Ready => {
                    BackgroundReviewReasonV1::ConfigInvalid
                }
            };
            return self.defer_locked(&mut state, &job, reason, observed_at_unix_ms);
        };
        if !config.enabled {
            return self.defer_locked(
                &mut state,
                &job,
                BackgroundReviewReasonV1::Disabled,
                observed_at_unix_ms,
            );
        }
        if !state.hub_active {
            return self.defer_locked(
                &mut state,
                &job,
                BackgroundReviewReasonV1::HubInactive,
                observed_at_unix_ms,
            );
        }
        if state.foreground_active {
            return self.defer_locked(
                &mut state,
                &job,
                BackgroundReviewReasonV1::ForegroundPreempted,
                observed_at_unix_ms,
            );
        }
        let identity = JobIdentity::from_job(&job);
        if state
            .job_identities
            .get(&job.job_id)
            .is_some_and(|existing| existing != &identity)
        {
            return self.defer_locked(
                &mut state,
                &job,
                BackgroundReviewReasonV1::InvalidJob,
                observed_at_unix_ms,
            );
        }
        // A completed job id is terminal. Replaying it cannot be treated as
        // retry because only failed/cancelled attempts may consume retry slot.
        if state.completed_jobs.contains(&job.job_id) {
            return self.defer_locked(
                &mut state,
                &job,
                BackgroundReviewReasonV1::RetryLimit,
                observed_at_unix_ms,
            );
        }
        if state.active.contains_key(&job.kind) {
            return self.defer_locked(
                &mut state,
                &job,
                BackgroundReviewReasonV1::AlreadyRunning,
                observed_at_unix_ms,
            );
        }
        if state.last_started_at_unix_ms.is_some_and(|started| {
            observed_at_unix_ms.saturating_sub(started) < config.min_elapsed_ms
        }) {
            return self.defer_locked(
                &mut state,
                &job,
                BackgroundReviewReasonV1::TimeGate,
                observed_at_unix_ms,
            );
        }
        if state.activity_units < config.activity_threshold {
            return self.defer_locked(
                &mut state,
                &job,
                BackgroundReviewReasonV1::ActivityGate,
                observed_at_unix_ms,
            );
        }
        let prior_attempts = state.attempts.get(&job.job_id).copied().unwrap_or_default();
        if prior_attempts >= MAX_ATTEMPTS {
            return self.defer_locked(
                &mut state,
                &job,
                BackgroundReviewReasonV1::RetryLimit,
                observed_at_unix_ms,
            );
        }
        if state
            .turn_input_tokens
            .get(&job.turn_id)
            .copied()
            .unwrap_or_default()
            .saturating_add(job.input_tokens)
            > config.per_turn_input_budget
        {
            return self.defer_locked(
                &mut state,
                &job,
                BackgroundReviewReasonV1::PerTurnBudgetExceeded,
                observed_at_unix_ms,
            );
        }
        if state
            .aggregate_input_tokens
            .saturating_add(job.input_tokens)
            > config.aggregate_input_budget
        {
            return self.defer_locked(
                &mut state,
                &job,
                BackgroundReviewReasonV1::AggregateBudgetExceeded,
                observed_at_unix_ms,
            );
        }
        let attempt = prior_attempts + 1;
        state.job_identities.insert(job.job_id.clone(), identity);
        state.attempts.insert(job.job_id.clone(), attempt);
        state
            .turn_input_tokens
            .entry(job.turn_id.clone())
            .and_modify(|tokens| *tokens = tokens.saturating_add(job.input_tokens))
            .or_insert(job.input_tokens);
        state.aggregate_input_tokens = state
            .aggregate_input_tokens
            .saturating_add(job.input_tokens);
        state.last_started_at_unix_ms = Some(observed_at_unix_ms);
        state.activity_units = 0;
        let kind = job.kind;
        state.active.insert(
            kind,
            ActiveJob {
                job: job.clone(),
                attempt,
            },
        );
        self.observe_locked(
            &mut state,
            Some(job.job_id.clone()),
            Some(kind),
            BackgroundReviewStatusV1::Started,
            BackgroundReviewReasonV1::Started,
            observed_at_unix_ms,
            attempt,
            job.input_tokens,
            Some(&job.turn_id),
        );
        BackgroundReviewDecision::Started { attempt }
    }

    /// Finish admitted job. Returns false when no matching active job exists.
    pub fn finish(
        &self,
        job_id: &str,
        completion: BackgroundReviewCompletion,
        observed_at_unix_ms: u64,
    ) -> bool {
        self.finish_with_completion(job_id, completion, observed_at_unix_ms)
    }

    /// Finish admitted job while preserving a typed learner/provider reason.
    pub fn finish_with_completion(
        &self,
        job_id: &str,
        completion: BackgroundReviewCompletion,
        observed_at_unix_ms: u64,
    ) -> bool {
        let mut state = self.state.lock().expect("background review state poisoned");
        let Some(kind) = state
            .active
            .iter()
            .find_map(|(kind, active)| (active.job.job_id == job_id).then_some(*kind))
        else {
            return false;
        };
        let active = state
            .active
            .remove(&kind)
            .expect("active job found before removal");
        let turn_input_tokens = state
            .turn_input_tokens
            .get(&active.job.turn_id)
            .copied()
            .unwrap_or_default();
        let (status, reason) = match completion {
            BackgroundReviewCompletion::Completed => (
                BackgroundReviewStatusV1::Completed,
                BackgroundReviewReasonV1::Completed,
            ),
            BackgroundReviewCompletion::Failed => (
                BackgroundReviewStatusV1::Failed,
                BackgroundReviewReasonV1::Failed,
            ),
            BackgroundReviewCompletion::FailedWithReason(reason) => {
                (BackgroundReviewStatusV1::Failed, reason)
            }
        };
        if matches!(completion, BackgroundReviewCompletion::Completed) {
            state.completed_jobs.insert(active.job.job_id.clone());
        }
        self.observe_locked_with_turn_tokens(
            &mut state,
            Some(active.job.job_id),
            Some(kind),
            status,
            reason,
            observed_at_unix_ms,
            active.attempt,
            active.job.input_tokens,
            turn_input_tokens,
        );
        true
    }

    /// Report a typed deferral when no valid job can be constructed from the
    /// host signal. No token budget is reserved.
    pub fn observe_deferred(&self, reason: BackgroundReviewReasonV1, observed_at_unix_ms: u64) {
        let mut state = self.state.lock().expect("background review state poisoned");
        self.observe_locked(
            &mut state,
            None,
            None,
            BackgroundReviewStatusV1::Deferred,
            reason,
            observed_at_unix_ms,
            0,
            0,
            None,
        );
    }

    /// Return true only for a job admitted by this scheduler. Learner
    /// execution must never bypass this check.
    pub fn is_active_job(&self, job_id: &str) -> bool {
        self.state
            .lock()
            .expect("background review state poisoned")
            .active
            .values()
            .any(|active| active.job.job_id == job_id)
    }

    /// Persist queued observations through a caller-owned durable sink. Queue
    /// entries remain intact when sink validation or IO fails.
    pub fn persist_observations<S: BackgroundReviewObservationSink + ?Sized>(
        &self,
        sink: &S,
    ) -> Result<usize, BackgroundReviewSinkError> {
        let mut state = self.state.lock().expect("background review state poisoned");
        if state.observations.is_empty() {
            return Ok(0);
        }
        let receipts = state
            .observations
            .iter()
            .cloned()
            .map(BackgroundReviewObservationReceiptV1::from_observation)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| BackgroundReviewSinkError::Invalid(error.to_string()))?;
        sink.append(&receipts)?;
        let count = receipts.len();
        state.observations.drain(..count);
        Ok(count)
    }

    /// Cancel admitted job and emit content-free cancellation observation.
    pub fn cancel(&self, job_id: &str, observed_at_unix_ms: u64) -> bool {
        let mut state = self.state.lock().expect("background review state poisoned");
        let Some(kind) = state
            .active
            .iter()
            .find_map(|(kind, active)| (active.job.job_id == job_id).then_some(*kind))
        else {
            return false;
        };
        let active = state
            .active
            .remove(&kind)
            .expect("active job found before removal");
        let turn_input_tokens = state
            .turn_input_tokens
            .get(&active.job.turn_id)
            .copied()
            .unwrap_or_default();
        self.observe_locked_with_turn_tokens(
            &mut state,
            Some(active.job.job_id),
            Some(kind),
            BackgroundReviewStatusV1::Cancelled,
            BackgroundReviewReasonV1::Cancelled,
            observed_at_unix_ms,
            active.attempt,
            active.job.input_tokens,
            turn_input_tokens,
        );
        true
    }

    /// Return number of currently admitted jobs.
    pub fn active_count(&self) -> usize {
        self.state
            .lock()
            .expect("background review state poisoned")
            .active
            .len()
    }

    /// Return aggregate input reservation consumed over scheduler lifetime.
    pub fn aggregate_input_tokens(&self) -> u64 {
        self.state
            .lock()
            .expect("background review state poisoned")
            .aggregate_input_tokens
    }

    /// Return unreserved per-turn and aggregate input budget. Missing config
    /// remains explicit instead of becoming an unbounded request.
    pub fn budget_remaining(&self, turn_id: &str) -> Option<(u64, u64)> {
        let config = self.config.as_ref()?;
        let state = self.state.lock().expect("background review state poisoned");
        let turn_used = state.turn_input_tokens.get(turn_id).copied().unwrap_or(0);
        Some((
            config.per_turn_input_budget.saturating_sub(turn_used),
            config
                .aggregate_input_budget
                .saturating_sub(state.aggregate_input_tokens),
        ))
    }

    /// Drain observations in emission order.
    pub fn drain_observations(&self) -> Vec<BackgroundReviewObservationV1> {
        let mut state = self.state.lock().expect("background review state poisoned");
        state.observations.drain(..).collect()
    }

    fn defer_locked(
        &self,
        state: &mut SchedulerState,
        job: &BackgroundReviewJobV1,
        reason: BackgroundReviewReasonV1,
        observed_at_unix_ms: u64,
    ) -> BackgroundReviewDecision {
        let attempt = state.attempts.get(&job.job_id).copied().unwrap_or_default();
        let job_id = (!job.job_id.trim().is_empty()).then(|| job.job_id.clone());
        let kind = job_id.as_ref().map(|_| job.kind);
        self.observe_locked(
            state,
            job_id,
            kind,
            BackgroundReviewStatusV1::Deferred,
            reason,
            observed_at_unix_ms,
            attempt,
            job.input_tokens,
            Some(&job.turn_id),
        );
        BackgroundReviewDecision::Deferred { reason }
    }

    fn cancel_all_locked(
        &self,
        state: &mut SchedulerState,
        reason: BackgroundReviewReasonV1,
        observed_at_unix_ms: u64,
    ) {
        let mut active_jobs: Vec<_> = state.active.drain().map(|(_, active)| active).collect();
        active_jobs.sort_by(|left, right| {
            left.job
                .job_id
                .cmp(&right.job.job_id)
                .then_with(|| left.job.kind.as_str().cmp(right.job.kind.as_str()))
        });
        for active in active_jobs {
            let turn_input_tokens = state
                .turn_input_tokens
                .get(&active.job.turn_id)
                .copied()
                .unwrap_or_default();
            self.observe_locked_with_turn_tokens(
                state,
                Some(active.job.job_id),
                Some(active.job.kind),
                BackgroundReviewStatusV1::Cancelled,
                reason,
                observed_at_unix_ms,
                active.attempt,
                active.job.input_tokens,
                turn_input_tokens,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_observation(
        &self,
        job_id: Option<String>,
        kind: Option<BackgroundReviewJobKindV1>,
        status: BackgroundReviewStatusV1,
        reason: BackgroundReviewReasonV1,
        observed_at_unix_ms: u64,
        attempt: u8,
        input_tokens: u64,
    ) {
        let mut state = self.state.lock().expect("background review state poisoned");
        self.observe_locked(
            &mut state,
            job_id,
            kind,
            status,
            reason,
            observed_at_unix_ms,
            attempt,
            input_tokens,
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_locked(
        &self,
        state: &mut SchedulerState,
        job_id: Option<String>,
        kind: Option<BackgroundReviewJobKindV1>,
        status: BackgroundReviewStatusV1,
        reason: BackgroundReviewReasonV1,
        observed_at_unix_ms: u64,
        attempt: u8,
        input_tokens: u64,
        turn_id: Option<&str>,
    ) {
        let turn_input_tokens = turn_id
            .and_then(|id| state.turn_input_tokens.get(id))
            .copied()
            .unwrap_or_default();
        self.observe_locked_with_turn_tokens(
            state,
            job_id,
            kind,
            status,
            reason,
            observed_at_unix_ms,
            attempt,
            input_tokens,
            turn_input_tokens,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_locked_with_turn_tokens(
        &self,
        state: &mut SchedulerState,
        job_id: Option<String>,
        kind: Option<BackgroundReviewJobKindV1>,
        status: BackgroundReviewStatusV1,
        reason: BackgroundReviewReasonV1,
        observed_at_unix_ms: u64,
        attempt: u8,
        input_tokens: u64,
        turn_input_tokens: u64,
    ) {
        if status == BackgroundReviewStatusV1::Deferred {
            let key = (reason, job_id.clone(), kind);
            if state.last_deferral.as_ref() == Some(&key) {
                return;
            }
            state.last_deferral = Some(key);
        } else {
            state.last_deferral = None;
        }
        state.observations.push_back(BackgroundReviewObservationV1 {
            schema_version: BackgroundReviewObservationV1::SCHEMA_VERSION,
            job_id,
            kind,
            status,
            reason,
            observed_at_unix_ms,
            attempt,
            input_tokens,
            turn_input_tokens,
            aggregate_input_tokens: state.aggregate_input_tokens,
            activity_units: state.activity_units,
            hub_active: state.hub_active,
            foreground_active: state.foreground_active,
        });
    }
}

pub use runner::BackgroundReviewRunner;

/// The resident's background-review scheduler: one thread that runs the review,
/// corpus-health and daily Cortex dream jobs for as long as the resident serves.
/// It is a plain interval loop; background authority comes from the lifecycle
/// control, which `serve` opens at startup and closes on drain.
mod runner {
    use super::*;
    use crate::adapt_service::AdaptFirstPartySemanticReviewProvider;
    use crate::background_review_input::{
        durable_reviewed_through_seq, input_path as background_review_input_path,
        BackgroundReviewInputSnapshotV1,
    };
    use crate::{MemDb, MemoryStore};
    use rightkit_memory_core::ProvenanceRef;
    use rightkit_memory_store::{AbsorbedStore, SessionEvent as StoreSessionEvent};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Handle to the running scheduler thread; dropping it stops the thread.
    pub struct BackgroundReviewRunner {
        stop: Arc<AtomicBool>,
        /// Closed by a drain (through the lifecycle) or by `Drop`, cutting the tick wait short.
        wake: crate::wake::Wake,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl BackgroundReviewRunner {
        pub fn start(
            root: PathBuf,
            control: crate::service::LifecycleControl,
        ) -> Result<Self, String> {
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let wake = control.new_wake();
            let thread_wake = wake.clone();
            let thread = std::thread::Builder::new()
                .name("membrane-background-review".into())
                .spawn(move || run_loop(&root, &control, &thread_stop, &thread_wake))
                .map_err(|error| format!("start background review: {error}"))?;
            Ok(Self {
                stop,
                wake,
                thread: Some(thread),
            })
        }
    }

    impl Drop for BackgroundReviewRunner {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.wake.close();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn run_loop(
        root: &PathBuf,
        control: &crate::service::LifecycleControl,
        stop: &AtomicBool,
        wake: &crate::wake::Wake,
    ) {
        let scheduler = BackgroundReviewScheduler::from_workspace_root(root, now_unix_ms());
        let sink = JsonlBackgroundReviewObservationSink::from_workspace_root(root);
        let mut executor = ReviewExecutor::new(root);
        let now = now_unix_ms();
        scheduler.set_hub_active(true, now);
        scheduler.observe_idle(now);
        executor.tick(root, &scheduler, control, now);
        executor.tick_daily_cortex_dream(root, &scheduler, control, now);
        emit_background_observations(&scheduler, &sink);
        let mut last_tick = now;
        while !stop.load(Ordering::Acquire) && !control.shutdown_requested() {
            // Block until the next tick is due; a drain, an authority change or `Drop`
            // wakes the wait at once, so stop stays prompt without 100 ms slices.
            let due_in = (BACKGROUND_REVIEW_TICK.as_millis() as u64)
                .saturating_sub(now_unix_ms().saturating_sub(last_tick));
            wake.wait(Duration::from_millis(due_in.max(1)));
            let now = now_unix_ms();
            if now.saturating_sub(last_tick) < BACKGROUND_REVIEW_TICK.as_millis() as u64 {
                continue;
            }
            if control.background_authority_open() {
                executor.tick(root, &scheduler, control, now);
                executor.tick_daily_cortex_dream(root, &scheduler, control, now);
            } else {
                scheduler.set_hub_active(false, now);
            }
            emit_background_observations(&scheduler, &sink);
            last_tick = now;
        }
        let now = now_unix_ms();
        scheduler.set_hub_active(false, now);
        scheduler.observe_idle(now);
        emit_background_observations(&scheduler, &sink);
    }

    const CORTEX_DB_RELATIVE_PATH: &str = "tools/.cache/memory/cortex-engine.db";
    const BACKGROUND_REVIEW_TICK: Duration = Duration::from_millis(250);
    const BACKGROUND_REVIEW_OBSERVATION_PREFIX: &str = "membrane-background-review ";

    /// MEM-061: deterministic read-only corpus-health cadence. Long enough that
    /// the maintenance kind can never starve review kinds through the shared
    /// activity/min-elapsed gates; a deferred tick is an observed scheduler
    /// decision, not a retry storm.
    const CORPUS_HEALTH_INTERVAL_MS: u64 = 900_000;

    struct ReviewExecutor {
        /// In-process deterministic first-party analyzer: a default deployment still
        /// produces governed proposals instead of silently doing nothing.
        provider: Box<dyn BackgroundSemanticReviewProvider>,
        cursor_store: BackgroundReviewCursorStore,
        proposal_sink: JsonlBackgroundReviewProposalAdmission,
        last_corpus_health_unix_ms: u64,
        last_dream_attempt_unix_ms: u64,
    }

    impl ReviewExecutor {
        fn new(root: &PathBuf) -> Self {
            Self {
                provider: Box::new(AdaptFirstPartySemanticReviewProvider::new()),
                cursor_store: BackgroundReviewCursorStore::default(),
                proposal_sink: JsonlBackgroundReviewProposalAdmission::from_workspace_root(root),
                last_corpus_health_unix_ms: 0,
                last_dream_attempt_unix_ms: 0,
            }
        }

        fn input_path(root: &Path) -> PathBuf {
            background_review_input_path(root)
        }

        fn tick(
            &mut self,
            root: &Path,
            scheduler: &BackgroundReviewScheduler,
            control: &crate::service::LifecycleControl,
            now: u64,
        ) {
            // The maintenance kind ticks on its own cadence regardless of whether
            // a review snapshot is pending; single-flight is per-kind so both can
            // be admitted under one scheduler.
            self.tick_corpus_health(root, scheduler, control, now);
            let provider = self.provider.as_ref();
            let input_path = Self::input_path(root);
            let snapshot = match fs::read_to_string(&input_path).ok().and_then(|value| {
                serde_json::from_str::<BackgroundReviewInputSnapshotV1>(&value).ok()
            }) {
                Some(snapshot) => snapshot,
                None => {
                    scheduler
                        .observe_deferred(BackgroundReviewReasonV1::CursorInputUnavailable, now);
                    return;
                }
            };
            if snapshot.schema_version != 1
                || snapshot.activity.validate().is_err()
                || snapshot.foreground_memory_state.validate().is_err()
            {
                scheduler.observe_deferred(BackgroundReviewReasonV1::InvalidJob, now);
                return;
            }
            // Corpus-health jobs are resident-scheduled on their own cadence; a
            // snapshot claiming that kind is not semantic-review input and must
            // never reach the semantic executor.
            if snapshot.job_kind
                == rightkit_context_protocol::background_review::BackgroundReviewJobKindV1::CorpusHealthMaintenance
            {
                scheduler.observe_deferred(BackgroundReviewReasonV1::InvalidJob, now);
                return;
            }
            // A request whose execution window already lapsed is withdrawn work,
            // not an admission: starting it would reserve token budget and burn a
            // retry attempt on a provider call that can only refuse as TimeGate.
            // Its stale activity signal is deliberately not credited either.
            if snapshot.deadline_unix_ms <= now {
                scheduler.observe_deferred(BackgroundReviewReasonV1::TimeGate, now);
                return;
            }
            let production = BackgroundReviewProducer::new(scheduler).admit(
                &snapshot.activity,
                snapshot.job_kind,
                now,
            );
            let super::BackgroundReviewProduction::Started { job, .. } = production else {
                return;
            };
            // Drain or parent-close on the control thread can revoke background
            // authority while this tick is mid-flight. Re-check before touching
            // cursor input or the provider so a revoked job ends as a HubInactive
            // cancellation rather than executing under a drained permit.
            if !control.background_authority_open() {
                scheduler.set_hub_active(false, now);
                return;
            }
            let input = match load_background_semantic_input(root, &self.cursor_store, &snapshot) {
                Ok(input) => input,
                Err(reason) => {
                    let _ = scheduler.finish_with_completion(
                        &job.job_id,
                        BackgroundReviewCompletion::FailedWithReason(reason),
                        now,
                    );
                    return;
                }
            };
            // The request may have lapsed while cursor input loaded; cancel the
            // admitted job instead of handing a dead request to the provider.
            if snapshot.deadline_unix_ms <= now {
                scheduler.cancel(&job.job_id, now);
                return;
            }
            let _execution = execute_background_semantic_review(
                scheduler,
                &job,
                &input,
                provider,
                Some(&self.proposal_sink),
                &self.cursor_store,
                snapshot.deadline_unix_ms,
                now,
            );
        }

        /// MEM-061/062: deterministic read-only corpus-health maintenance, the
        /// committed second live consumer proving the scheduler is generic over
        /// job kinds rather than review-specific. Admitted through the same
        /// single-flight/authority gates; its interval reservation prevents a
        /// deferred decision from becoming a retry storm, and the same drain,
        /// parent-close, and mid-tick revocation paths cancel it.
        fn tick_corpus_health(
            &mut self,
            root: &Path,
            scheduler: &BackgroundReviewScheduler,
            control: &crate::service::LifecycleControl,
            now: u64,
        ) {
            if now.saturating_sub(self.last_corpus_health_unix_ms) < CORPUS_HEALTH_INTERVAL_MS {
                return;
            }
            self.last_corpus_health_unix_ms = now;
            if !control.background_authority_open() {
                return;
            }
            // Interval bucket makes the job id deterministic and unique per run;
            // replaying an already-completed id is refused by the terminal-id
            // gate, so identity and dedup stay honest.
            let job = rightkit_context_protocol::background_review::BackgroundReviewJobV1 {
                schema_version:
                    rightkit_context_protocol::background_review::BackgroundReviewJobV1::SCHEMA_VERSION,
                job_id: format!("corpus-health-{}", now / CORPUS_HEALTH_INTERVAL_MS),
                kind: rightkit_context_protocol::background_review::BackgroundReviewJobKindV1::CorpusHealthMaintenance,
                turn_id: "corpus-health".to_owned(),
                input_tokens: 0,
                requested_at_unix_ms: now,
            };
            let super::BackgroundReviewDecision::Started { .. } = scheduler.start(job.clone(), now)
            else {
                return;
            };
            // Authority may drain between admission and execution; a revoked job
            // must end as a HubInactive cancellation, never run under a dead
            // permit.
            if !control.background_authority_open() {
                scheduler.set_hub_active(false, now);
                return;
            }
            let completion = match run_corpus_health_check(root) {
                Ok(()) => BackgroundReviewCompletion::Completed,
                Err(reason) => BackgroundReviewCompletion::FailedWithReason(reason),
            };
            let _ = scheduler.finish_with_completion(&job.job_id, completion, now);
        }

        /// Run Cortex's deterministic durable-memory consolidation once per UTC
        /// day through the same scheduler and lifecycle gates as other background
        /// work. A durable day marker prevents a resident restart from repeating a
        /// completed dream; failures remain retryable after a bounded delay.
        fn tick_daily_cortex_dream(
            &mut self,
            root: &Path,
            scheduler: &BackgroundReviewScheduler,
            control: &crate::service::LifecycleControl,
            now: u64,
        ) {
            if !control.background_authority_open()
                || now.saturating_sub(self.last_dream_attempt_unix_ms) < 60_000
            {
                return;
            }
            let today = utc_date(now);
            let marker = root.join(".membrane/cortex-dream-last-date");
            if fs::read_to_string(&marker)
                .ok()
                .is_some_and(|value| value.trim() == today)
            {
                return;
            }
            self.last_dream_attempt_unix_ms = now;
            let job = rightkit_context_protocol::background_review::BackgroundReviewJobV1 {
                schema_version:
                    rightkit_context_protocol::background_review::BackgroundReviewJobV1::SCHEMA_VERSION,
                job_id: format!("cortex-dream-{today}"),
                kind:
                    rightkit_context_protocol::background_review::BackgroundReviewJobKindV1::CortexSemanticDream,
                turn_id: format!("daily-{today}"),
                input_tokens: 0,
                requested_at_unix_ms: now,
            };
            let super::BackgroundReviewDecision::Started { .. } = scheduler.start(job.clone(), now)
            else {
                return;
            };
            if !control.background_authority_open() {
                scheduler.set_hub_active(false, now);
                return;
            }
            let database_path = crate::qa_env::cortex_db()
                .map(PathBuf::from)
                .filter(|path| path.is_file())
                .unwrap_or_else(|| root.join(CORTEX_DB_RELATIVE_PATH));
            let result = MemDb::open(&database_path)
                .map_err(|error| error.to_string())
                .map(MemoryStore::open)
                .and_then(|store| store.dream_now(&today));
            match result {
                Ok(status) => {
                    if let Some(parent) = marker.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let persisted = fs::OpenOptions::new()
                        .create(true)
                        .truncate(true)
                        .write(true)
                        .open(&marker)
                        .and_then(|mut file| {
                            file.write_all(today.as_bytes())?;
                            file.sync_data()
                        });
                    if let Err(error) = persisted {
                        stderr_line!("membrane cortex-dream date marker failed: {error}");
                        let _ = scheduler.finish_with_completion(
                            &job.job_id,
                            BackgroundReviewCompletion::Failed,
                            now,
                        );
                        return;
                    }
                    stderr_line!(
                        "membrane cortex-dream completed date={today} consolidated={}",
                        status.consolidated_count
                    );
                    let _ = scheduler.finish_with_completion(
                        &job.job_id,
                        BackgroundReviewCompletion::Completed,
                        now,
                    );
                }
                Err(error) => {
                    stderr_line!("membrane cortex-dream failed date={today}: {error}");
                    let _ = scheduler.finish_with_completion(
                        &job.job_id,
                        BackgroundReviewCompletion::Failed,
                        now,
                    );
                }
            }
        }
    }

    fn utc_date(unix_ms: u64) -> String {
        let days = (unix_ms / 86_400_000) as i64;
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let day_of_era = z - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let mut year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_prime = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
        let month = month_prime + if month_prime < 10 { 3 } else { -9 };
        if month <= 2 {
            year += 1;
        }
        format!("{year:04}-{month:02}-{day:02}")
    }

    /// Read-only corpus probe behind the CorpusHealthMaintenance kind: open the
    /// store without migrations or outbox effects, require rightkit-db's open-time
    /// `quick_check` to pass. Any miss is a typed failure on the job receipt, never a
    /// fabricated pass and never a write.
    fn run_corpus_health_check(root: &Path) -> Result<(), BackgroundReviewReasonV1> {
        let database_path = crate::qa_env::cortex_db()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .unwrap_or_else(|| root.join(CORTEX_DB_RELATIVE_PATH));
        if !database_path.is_file() {
            return Err(BackgroundReviewReasonV1::CursorInputUnavailable);
        }
        rightkit_db::DbOptions {
            integrity: rightkit_db::IntegrityCheck::Quick,
            ..rightkit_db::DbOptions::new(&database_path)
        }
        .open_read_only()
        .map(|_| ())
        .map_err(|_| BackgroundReviewReasonV1::Failed)
    }

    fn load_background_semantic_input(
        root: &Path,
        cursor_store: &BackgroundReviewCursorStore,
        snapshot: &BackgroundReviewInputSnapshotV1,
    ) -> Result<BackgroundSemanticReviewInputV1, BackgroundReviewReasonV1> {
        let session_id = &snapshot.activity.session_id;
        // The resident publishes the database it actually opened; the
        // workspace-relative default is only a fallback for a root-shaped install.
        let database_path = crate::qa_env::cortex_db()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .unwrap_or_else(|| root.join(CORTEX_DB_RELATIVE_PATH));
        if !database_path.is_file() {
            return Err(BackgroundReviewReasonV1::CursorInputUnavailable);
        }
        let db = MemDb::open(&database_path)
            .map_err(|_| BackgroundReviewReasonV1::CursorInputUnavailable)?;
        // Crash safety across resident restarts: the in-memory review cursor starts
        // empty, so seed it once from what Cortex durably knows it already
        // reviewed. `set_initial` never moves a cursor this process already owns.
        if let Some(reviewed_through) = durable_reviewed_through_seq(&db, session_id) {
            let _ = cursor_store.set_initial(rightkit_memory_core::EventCursor {
                session_id: session_id.clone(),
                last_seq: reviewed_through,
            });
        }
        let cursor = cursor_store.get(session_id)?;
        let store =
            AbsorbedStore::new(db).map_err(|_| BackgroundReviewReasonV1::CursorInputUnavailable)?;
        let high_water = store
            .cursor(session_id)
            .map_err(|_| BackgroundReviewReasonV1::CursorInputUnavailable)?;
        let end_seq = high_water
            .last_seq
            .min(cursor.last_seq.saturating_add(256))
            .saturating_add(1);
        let stored_events = if end_seq > cursor.last_seq.saturating_add(1) {
            store
                .events_range(session_id, cursor.last_seq.saturating_add(1), end_seq)
                .map_err(|_| BackgroundReviewReasonV1::CursorInputUnavailable)?
        } else {
            Vec::new()
        };
        // The baseline is read-only context from before the background cursor.
        // Bound it so selection remains deterministic and cannot turn a review
        // tick into an unbounded historical read.
        let baseline_start = cursor.last_seq.saturating_sub(256).saturating_add(1);
        let reviewed_baseline = if cursor.last_seq >= baseline_start {
            store
                .events_range(
                    session_id,
                    baseline_start,
                    cursor.last_seq.saturating_add(1),
                )
                .map_err(|_| BackgroundReviewReasonV1::CursorInputUnavailable)?
        } else {
            Vec::new()
        };
        let events = stored_events
            .into_iter()
            .map(store_event_to_core)
            .collect::<Vec<_>>();
        let reviewed_baseline = reviewed_baseline
            .into_iter()
            .map(store_event_to_core)
            .collect::<Vec<_>>();
        Ok(BackgroundSemanticReviewInputV1 {
            task_id: snapshot.task_id.clone(),
            cursor,
            events,
            reviewed_baseline,
            foreground_memory_state: match &snapshot.foreground_memory_state {
                BackgroundReviewForegroundMemoryStateV1::Unavailable => {
                    rightkit_memory_core::review::ForegroundMemoryStateV1::Unavailable
                }
                BackgroundReviewForegroundMemoryStateV1::AvailableNoEmission => {
                    rightkit_memory_core::review::ForegroundMemoryStateV1::AvailableNoEmission
                }
                BackgroundReviewForegroundMemoryStateV1::AvailableEmission { range } => {
                    rightkit_memory_core::review::ForegroundMemoryStateV1::AvailableEmission(
                        rightkit_memory_core::review::ForegroundMemoryEmissionV1 {
                            emission_id: format!(
                                "background-signal-{}-{}",
                                range.start_seq, range.end_seq
                            ),
                            session_id: session_id.clone(),
                            start_seq: range.start_seq,
                            end_seq: range.end_seq,
                        },
                    )
                }
            },
        })
    }

    fn store_event_to_core(event: StoreSessionEvent) -> SessionEvent {
        SessionEvent {
            schema_version: event.schema_version,
            session_id: event.session_id,
            seq: event.seq,
            event_id: event.event_id,
            event_type: event.event_type,
            payload: event.payload,
            scope_id: event.scope_id,
            authority: event.authority,
            influence_class: event.influence_class,
            lifecycle: event.lifecycle,
            retention: event.retention,
            provenance: event
                .provenance
                .into_iter()
                .map(|item| ProvenanceRef {
                    source: item.source,
                    source_event_ids: item.source_event_ids,
                    producer: item.producer,
                })
                .collect(),
            occurred_at_ms: event.occurred_at_ms,
            recorded_at_ms: event.recorded_at_ms,
            content_hash: event.content_hash,
        }
    }

    fn emit_background_observations(
        scheduler: &BackgroundReviewScheduler,
        sink: &JsonlBackgroundReviewObservationSink,
    ) {
        // A persistent sink failure repeats on every tick. Reporting it each time
        // buried the log in thousands of identical lines that said nothing about
        // the cause; report the reason, and only when it changes.
        static LAST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
        match scheduler.persist_observations(sink) {
            Ok(_) => {
                if let Ok(mut last) = LAST.lock() {
                    *last = None;
                }
            }
            Err(error) => {
                let reason = format!("{error}");
                let repeated = LAST
                    .lock()
                    .map(|mut last| {
                        let same = last.as_deref() == Some(reason.as_str());
                        if !same {
                            *last = Some(reason.clone());
                        }
                        same
                    })
                    .unwrap_or(false);
                if !repeated {
                    stderr_line!(
                        "{BACKGROUND_REVIEW_OBSERVATION_PREFIX}sink_unavailable {} ({})",
                        reason,
                        sink.path().display()
                    );
                }
            }
        }
    }

    fn now_unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}
