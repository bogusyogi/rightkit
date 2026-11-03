// Circuit-breaker for the auto-compactor.
//
// Tracks consecutive compaction failures and trips the breaker after
// [`DEFAULT_MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES`] consecutive misses so the
// engine-loop can stop hammering a failing summarizer and surface a status.
// Mirrors the env var `RIGHTKIT_MAX_AUTOCOMPACT_FAILURES` and the canonical
// name `MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES` referenced in the FEATURE
// MATRIX.
//
// Sibling include! module.

use serde::Serialize;
use std::cmp::Ordering;
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

/// Typed failure taxonomy for durable compaction.  Only a summarizer that
/// made no progress is evidence that the summarizer itself is unhealthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionFailureClassV1 {
    SummarizerNoProgress,
    AccountingUnavailable,
    FederationUnavailable,
    StaleCapacity,
    BindingUnavailable,
    H9Unavailable,
    PolicyUnavailable,
    ScopeUnavailable,
    StaleSnapshot,
    Cancelled,
    StorageUnavailable,
    /// The prepared replacement would not shrink the model-facing context.
    /// Typed so callers stop retrying instead of looping on a no-op fold.
    NoGain,
}

impl CompactionFailureClassV1 {
    pub fn affects_breaker(self) -> bool {
        matches!(self, Self::SummarizerNoProgress)
    }
}

/// Default upper bound on consecutive compaction failures before the
/// circuit-breaker opens. Mirrored in [`MAX_AUTOCOMPACT_FAILURES_ENV`].
pub const DEFAULT_MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES: u32 = 3;

/// Env var read by [`CompactorBreaker::new_from_env_or_default`] to allow the
/// operator to tighten the breaker cap. Unparseable / unset values fall back
/// to [`DEFAULT_MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES`].
pub const MAX_AUTOCOMPACT_FAILURES_ENV: &str = "RIGHTKIT_MAX_AUTOCOMPACT_FAILURES";

/// Error type for failed compaction passes. Distinct from `Summarizer`-level
/// errors so callers can tell breaker trips from per-message failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactError {
    /// The summarizer refused to produce a summary (LLM error, IO failure,
    /// etc.). The breaker increments on each occurrence.
    Summarizer(String),
    /// Mechanism 6 (breaker): the user aborted the in-flight compaction call
    /// (e.g. cancelled the turn, closed the session). This is not a
    /// summarizer failure -- [`CompactorBreaker::record_outcome`] never
    /// counts it toward the consecutive-failure cap.
    Cancelled(String),
}

impl std::fmt::Display for CompactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactError::Summarizer(msg) => write!(f, "summarizer failure: {}", msg),
            CompactError::Cancelled(msg) => write!(f, "compaction cancelled: {}", msg),
        }
    }
}

/// Mechanism 6 (breaker): `true` if `error` should count toward the
/// consecutive-failure cap. A user-initiated cancel is not a real failure of
/// the summarizer and must not push the breaker toward tripping.
///
/// NOTE: this deliberately does NOT mirror claude-code's reference behaviour
/// (`autoCompact.ts:335-349`), which increments the counter unconditionally
/// on every attempt including a cancel, and only uses the user-abort
/// distinction to suppress log noise -- the counter itself still climbs.
/// Here a cancel is excluded from the count entirely. This is an intentional
/// divergence, not a bug: a cancel is not evidence the summarizer is
/// failing, and folding it into the trip cap would let a user who aborts
/// compaction a few times in a row spuriously trip the breaker for
/// unrelated later real failures.
pub fn counts_toward_breaker(error: &CompactError) -> bool {
    classify_compaction_error(error).affects_breaker()
}

pub fn classify_compaction_error(error: &CompactError) -> CompactionFailureClassV1 {
    match error {
        CompactError::Summarizer(_) => CompactionFailureClassV1::SummarizerNoProgress,
        CompactError::Cancelled(_) => CompactionFailureClassV1::Cancelled,
    }
}

impl std::error::Error for CompactError {}

/// Snapshot of breaker state, suitable for surfacing to the operator / UI.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CompactorBreakerStatus {
    /// Number of consecutive compaction failures observed since the last
    /// success (or since the breaker was constructed if no success yet).
    pub consecutive_failures: u32,
    /// Cap at which the breaker opens. Mirrors
    /// `cmp.breaker_cap` / `MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES`.
    pub cap: u32,
    /// `true` when `consecutive_failures >= cap`. When open, `compact()`
    /// short-circuits and returns `0` (preserving the historical convention
    /// that zero means "no work").
    pub is_open: bool,
    /// Failure count at the moment the breaker first tripped, retained so
    /// the operator can see the threshold that pinned it open.
    pub tripped_at: Option<u32>,
}

/// Atomic, fail-fast circuit-breaker for `Compactor::compact()`. Mirrors the
/// canonical constant `MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES` referenced in the
/// feature matrix.
#[derive(Debug)]
pub struct CompactorBreaker {
    consecutive_failures: AtomicU32,
    cap: u32,
}

impl CompactorBreaker {
    /// Construct a breaker with the default cap of
    /// [`DEFAULT_MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES`].
    pub fn new() -> Self {
        Self::with_cap(DEFAULT_MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES)
    }

    /// Construct a breaker with an explicit cap.
    pub fn with_cap(cap: u32) -> Self {
        Self {
            consecutive_failures: AtomicU32::new(0),
            cap: cap.max(1),
        }
    }

    /// Construct from `RIGHTKIT_MAX_AUTOCOMPACT_FAILURES` env var, falling
    /// back to the default cap when unset / unparseable.
    pub fn new_from_env_or_default() -> Self {
        let parsed = std::env::var(MAX_AUTOCOMPACT_FAILURES_ENV)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .filter(|v| *v > 0);
        match parsed {
            Some(cap) => Self::with_cap(cap),
            None => Self::new(),
        }
    }

    /// Records a successful compaction: resets the failure counter to zero.
    pub fn record_success(&self) {
        self.consecutive_failures.store(0, AtomicOrdering::SeqCst);
    }

    /// Records a failed compaction: increments the failure counter. Returns
    /// `true` if this call was the one that pushed the breaker into the open
    /// state. Subsequent failures while already open also return `false`.
    pub fn record_failure(&self) -> bool {
        let prev = self
            .consecutive_failures
            .fetch_add(1, AtomicOrdering::SeqCst);
        let next = prev.saturating_add(1);
        next.cmp(&self.cap) == Ordering::Equal
    }

    /// `true` if `consecutive_failures >= cap`.
    pub fn is_open(&self) -> bool {
        self.consecutive_failures.load(AtomicOrdering::SeqCst) >= self.cap
    }

    /// Cap that the breaker trips at.
    pub fn cap(&self) -> u32 {
        self.cap
    }

    /// Snapshot the breaker state. Useful for surfacing to the operator UI
    /// (CLI `hooks status`, daemon `/hooks/status`, web dashboard, etc.).
    pub fn status(&self) -> CompactorBreakerStatus {
        let consecutive_failures = self.consecutive_failures.load(AtomicOrdering::SeqCst);
        let is_open = consecutive_failures >= self.cap;
        let tripped_at = if is_open {
            Some(consecutive_failures)
        } else {
            None
        };
        CompactorBreakerStatus {
            consecutive_failures,
            cap: self.cap,
            is_open,
            tripped_at,
        }
    }

    /// Force-resets the breaker. Used by `Compactor::reset_breaker()` and by
    /// tests that want to clear state between assertions.
    pub fn reset(&self) {
        self.consecutive_failures.store(0, AtomicOrdering::SeqCst);
    }

    /// Mechanism 6 (breaker): record the outcome of one compaction attempt,
    /// distinguishing a user-abort from a real failure so cancels never count
    /// toward the trip cap. Returns `true` iff this call is the one that
    /// newly tripped the breaker (mirrors `record_failure`'s contract); a
    /// cancelled attempt always returns `false` and leaves the counter
    /// unchanged.
    pub fn record_outcome(&self, error: &CompactError) -> bool {
        if !counts_toward_breaker(error) {
            return false;
        }
        self.record_failure()
    }

    /// Record a typed outcome. External availability, policy, capacity,
    /// snapshot, cancellation, and storage failures never open this breaker.
    pub fn record_failure_class(&self, class: CompactionFailureClassV1) -> bool {
        class.affects_breaker() && self.record_failure()
    }
}

impl Default for CompactorBreaker {
    fn default() -> Self {
        Self::new()
    }
}

/// A summarizer whose `try_summarize` may fail. The `Compactor` consults the
/// breaker whenever a `dyn FallibleSummarizer` is configured. Existing
/// infallible `Summarizer` impls are upgraded to `FallibleSummarizer` via
/// [`InfallibleAdapter`].
#[async_trait]
pub trait FallibleSummarizer: Send + Sync {
    async fn try_summarize(
        &self,
        messages: &[TranscriptMessage],
        prior_summary: Option<&str>,
    ) -> Result<String, CompactError>;

    /// Return and clear usage from the most recent summarization attempt.
    /// Implementations that do not call a metered model may keep the default.
    fn take_usage(&self) -> Option<CompactionUsage> {
        None
    }

    /// Update the active model's compaction-only provider output ceiling.
    /// Non-provider/custom summarizers may ignore it.
    fn set_max_output_tokens(&self, _tokens: Option<u32>) {}
}

/// Provider-reported usage for one authoritative compaction model call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
}

/// Adapter that lifts an infallible [`Summarizer`] into a [`FallibleSummarizer`]
/// by wrapping `Ok(s)` around its `summarize()` result. Preserves the existing
/// semantics (e.g. `LlmSummarizer` still does its own internal fallback to
/// the statistical summary on LLM error) -- the breaker simply never trips
/// for `InfallibleAdapter` because the wrapped path never returns `Err`.
pub struct InfallibleAdapter {
    inner: std::sync::Arc<dyn Summarizer>,
}

impl InfallibleAdapter {
    pub fn new(inner: std::sync::Arc<dyn Summarizer>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl FallibleSummarizer for InfallibleAdapter {
    async fn try_summarize(
        &self,
        messages: &[TranscriptMessage],
        prior_summary: Option<&str>,
    ) -> Result<String, CompactError> {
        Ok(self.inner.summarize(messages, prior_summary).await)
    }
}

/// Reason a compaction pass returned `0` -- used for tests and to enrich the
/// operator-facing status payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactNoOpReason {
    BelowTrigger,
    NoMessagesToDrop,
    BreakerOpen,
    SummarizerFailed(String),
}
