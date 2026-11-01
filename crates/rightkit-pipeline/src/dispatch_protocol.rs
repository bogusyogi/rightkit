//! Payload shapes for dispatching an outgoing content brief to an editor engine as an external process
//! (`dispatch.run`, `dispatch.list`). The engine runs the other tool; it never renders or edits media itself.
//! The brief and the delivery envelope are the contracts-io module's (`brief_out.*`, `delivery.accept`); these
//! shapes only carry the attempt that connects them.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use rightkit_social_publish::protocol::Scope;

use crate::dispatch_host::DeliveryRecord;

/// `dispatch.run` input. `request_id` names an outgoing brief stored by `brief_out.put`. `engine` is
/// `cutright` or `genright`. `now` stamps the attempt (the wall clock is used only to bound the child).
/// `timeout_secs` is the hard wall-clock limit for the child (default 600, at most 3600). `project_dir` is the
/// absolute path of the CutRight project the brief is run against (CutRight's `--project`); it is required for
/// `cutright`. `retry_interrupted` must be true to run again after an attempt ended without a recorded result.
#[derive(Debug, Clone, Deserialize)]
pub struct DispatchRunInput {
    pub scope: Scope,
    pub request_id: String,
    pub engine: String,
    pub now: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub project_dir: Option<String>,
    #[serde(default)]
    pub retry_interrupted: Option<bool>,
    /// Test hook, honoured only when the environment variable `POSTRIGHT_DISPATCH_TEST_HOOKS` is exactly `1` and
    /// the engine runs under QA isolation; otherwise `true` is `bad_input`. The engine is spawned and its identity
    /// recorded, then the call returns at once with outcome `abandoned_for_test`, leaving the engine alive and the
    /// attempt `running` with a dead owner, so a test can prove the leftover sweep.
    #[cfg(feature = "test-mode")]
    #[serde(default)]
    pub test_abandon_after_spawn: Option<bool>,
}

/// One dispatch attempt, as stored. `outcome` is `running` while the child lives, then one of `delivered`,
/// `partial`, `engine_failed`, `timed_out`, `delivery_invalid`, `engine_missing`, or `interrupted` (a later call
/// found the attempt still `running` with a dead owner). `exit_code` is null when the child was killed or never ran.
/// `detail` carries the message, the stderr tail, the engine's own error and the measured run time, and for a
/// `delivered` or `partial` result `engine_status`, `deliverables` (the keys that produced an envelope) and
/// `skipped` (the engine's list, verbatim).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchAttempt {
    pub attempt_id: i64,
    pub request_id: String,
    pub engine: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub exit_code: Option<i64>,
    pub outcome: String,
    pub detail: Value,
    /// The engine process as recorded right after the spawn (null when nothing was spawned). `engine_start` and
    /// `engine_exe` are the identity the sweep compares; `engine_identity_source` names the platform source
    /// (`linux-proc`, `macos-ps`, `windows-handle`) or is `none` when no identity could be read.
    pub engine_pid: Option<i64>,
    pub engine_pgid: Option<i64>,
    pub engine_start: Option<String>,
    pub engine_exe: Option<String>,
    pub engine_identity_source: Option<String>,
    /// What the leftover sweep did to this attempt's engine: `swept_killed`, `swept_already_gone`,
    /// `sweep_unverified`, `sweep_no_engine` or `sweep_kill_failed`; null when no sweep touched it.
    pub sweep_action: Option<String>,
    pub sweep_at: Option<String>,
    pub sweep_detail: Option<String>,
    /// True for an attempt made by the `test_abandon_after_spawn` hook (test-mode builds only; a release build
    /// neither writes nor reads the `abandoned_for_test` column, which stays in the schema for existing databases).
    #[cfg(feature = "test-mode")]
    pub abandoned_for_test: bool,
}

/// `dispatch.run` output. `outcome` is `delivered` (engine status `done`), `partial` (engine status `partial`: a
/// normal outcome, not an error), `awaiting_approval` (the producer wants a person's approval in its own app first:
/// a waiting state, never retried automatically), `engine_failed`, `timed_out`, `delivery_invalid`, `engine_missing` or (test hook
/// only) `abandoned_for_test`. `spawned` is false when no child ran (a repeat of a delivered request, or a missing
/// engine). `brief_state` is the outgoing brief's state after the call (`issued`, `partial` or `delivered`).
/// `deliveries` holds the envelopes this call accepted (a repeat of a delivered request: every stored envelope of the
/// brief), each with its `deliverable` key; `delivery` is the first of them, or null. `skipped` is the engine's own
/// `skipped` list, verbatim (a paid-route refusal shows here as `spend_authorisation_required`).
/// `stderr_tail` is the capped tail of the child's stderr, set when a child ran. `engine_error` is the
/// `error` object the engine printed on stdout, when it printed one. `interrupted` lists attempts this call
/// found dead and marked `interrupted`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchRunOutput {
    pub scope: Scope,
    pub request_id: String,
    pub engine: String,
    pub outcome: String,
    pub spawned: bool,
    pub brief_state: String,
    pub attempt: Option<DispatchAttempt>,
    pub delivery: Option<DeliveryRecord>,
    pub deliveries: Vec<DeliveryRecord>,
    pub skipped: Vec<Value>,
    pub message: Option<String>,
    pub stderr_tail: Option<String>,
    pub engine_error: Option<Value>,
    pub interrupted: Vec<i64>,
}

/// `dispatch.list` input: the attempts of one scope, or of one request when `request_id` is set.
#[derive(Debug, Clone, Deserialize)]
pub struct DispatchListInput {
    pub scope: Scope,
    #[serde(default)]
    pub request_id: Option<String>,
}

/// `dispatch.list` output, ordered by attempt id (oldest first).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchListOutput {
    pub scope: Scope,
    pub attempts: Vec<DispatchAttempt>,
}

/// `dispatch.sweep` input: settle the dead `running` attempts of one scope. `now` stamps the sweep.
#[derive(Debug, Clone, Deserialize)]
pub struct DispatchSweepInput {
    pub scope: Scope,
    pub now: String,
}

/// One attempt a sweep settled: what it did to the recorded engine (`action`, with `detail`) and the outcome the
/// attempt now has (`interrupted`, or `delivered` when its request was already delivered).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchSwept {
    pub attempt_id: i64,
    pub request_id: String,
    pub action: String,
    pub detail: String,
    pub outcome: String,
}

/// `dispatch.sweep` output: the attempts settled, oldest first. Empty when nothing was dead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchSweepOutput {
    pub scope: Scope,
    pub swept: Vec<DispatchSwept>,
}
