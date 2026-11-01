//! Shared "cheap model summarizes/classifies session data" helper.
//!
//! Three later atoms — ENG-82 (goal-progress audit), ENG-83 (session recap),
//! and ENG-108 (diff-hunk narration) — each need a small, cheap model to
//! summarize or classify some session data. This module is the one shared
//! implementation they all call, so there is one code path instead of three.
//!
//! Design:
//! - [`SmallModelTask`] enumerates the supported task shapes with typed
//!   output per task — callers never get raw text back to re-parse.
//! - [`SmallModelInput`] bounds input by a hard byte cap, truncating from the
//!   middle (keeping both a head and a tail) so context on either end
//!   survives, with an explicit marker showing where content was cut.
//! - [`SmallModelClient`] is the transport boundary callers depend on.
//!   [`RegistryBackedSmallModelClient`] resolves eligible routes from
//!   `rightkit_llm::agent_providers::registry::ModelRegistry`, while daemon hosts can
//!   supply a checked provider-route implementation. Tests inject a stub
//!   implementing the same trait.
//! - Every call carries a [`SmallModelBudget`] ceiling; the client refuses to
//!   run (returns `SmallModelError::BudgetExceeded`) once the ceiling would
//!   be crossed, tracked via [`SmallModelBudget::remaining_calls`] /
//!   `remaining_tokens` bookkeeping the caller updates from actual usage.
//!
//! `orchestration/src/lib.rs` is not owned by this lane; the required
//! addition there is:
//!
//! ```ignore
//! pub mod small_model_utility;
//! pub use small_model_utility::{
//!     RegistryBackedSmallModelClient, SmallModelBudget, SmallModelClient, SmallModelError,
//!     SmallModelInput, SmallModelOutput, SmallModelTask,
//! };
//! ```
//!
//! No new Cargo dependency is needed: this module uses only
//! `host_providers` (already a dependency of this crate, see
//! `orchestration/Cargo.toml`), `async-trait` (already present), and `std`.

use std::collections::BTreeMap;

use async_trait::async_trait;
use rightkit_llm::agent_providers::registry::{ModelRegistry, ModelRoute};

/// Marker inserted at the cut point when [`SmallModelInput::bounded`] truncates
/// from the middle of oversized content.
pub const TRUNCATION_MARKER: &str = "\n…[truncated]…\n";

/// The task shapes the shared small-model helper supports. Each variant
/// determines both the prompt framing and the typed output shape returned to
/// the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmallModelTask {
    /// ENG-83: summarize a session into a short recap.
    Recap,
    /// ENG-82: audit progress against a stated goal.
    ProgressAudit,
    /// ENG-108: narrate a diff hunk in plain language.
    DiffNarration,
    /// ENG-83: offer one concise next user instruction after a recap.
    SessionFollowUp,
    /// Generic label classification against a caller-supplied label set.
    Classify { labels: Vec<String> },
}

/// Typed output per task. Callers match on the variant that corresponds to
/// the `SmallModelTask` they issued; the client never hands back raw text for
/// a caller to re-parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmallModelOutput {
    Recap {
        summary: String,
    },
    ProgressAudit {
        on_track: bool,
        summary: String,
        blockers: Vec<String>,
    },
    DiffNarration {
        narration: String,
    },
    SessionFollowUp {
        suggestion: String,
    },
    Classify {
        label: String,
        confidence_bucket: ConfidenceBucket,
    },
}

/// Coarse confidence bucket for classification output. Kept coarse
/// deliberately: a small, cheap model's calibrated float confidence is not
/// trustworthy enough to expose as a precise number to callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfidenceBucket {
    Low,
    Medium,
    High,
}

/// Bounded input handed to the small model. Construct via [`SmallModelInput::bounded`]
/// so the truncation policy is always applied — there is no public constructor
/// that skips it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmallModelInput {
    /// Content after truncation, if any, has already been applied.
    pub content: String,
    /// True if `content` was shorter than the cap and passed through unchanged.
    pub truncated: bool,
    /// Original byte length before truncation.
    pub original_len: usize,
}

impl SmallModelInput {
    /// Build a bounded input from raw content and a hard byte cap.
    ///
    /// Truncation policy: if `raw` fits within `max_bytes`, it passes through
    /// unchanged. Otherwise the content is cut from the middle: the input is
    /// split into a head half and a tail half (each sized so head + marker +
    /// tail stays within `max_bytes`), joined by [`TRUNCATION_MARKER`], so
    /// context from both the start and the end of the content survives.
    ///
    /// Splits are performed on UTF-8 char boundaries (never inside a
    /// multi-byte codepoint) by walking outward from the byte midpoint.
    pub fn bounded(raw: &str, max_bytes: usize) -> Self {
        let original_len = raw.len();
        if original_len <= max_bytes {
            return Self {
                content: raw.to_string(),
                truncated: false,
                original_len,
            };
        }

        let marker_len = TRUNCATION_MARKER.len();
        // If the cap can't even fit the marker, degrade to a head-only slice.
        if max_bytes <= marker_len {
            let cut = floor_char_boundary(raw, max_bytes);
            return Self {
                content: raw[..cut].to_string(),
                truncated: true,
                original_len,
            };
        }

        let budget_for_content = max_bytes - marker_len;
        let head_len = floor_char_boundary(raw, budget_for_content / 2);
        let tail_budget = budget_for_content - head_len;
        let tail_start = ceil_char_boundary(raw, original_len.saturating_sub(tail_budget));

        let mut content = String::with_capacity(max_bytes);
        content.push_str(&raw[..head_len]);
        content.push_str(TRUNCATION_MARKER);
        content.push_str(&raw[tail_start..]);

        Self {
            content,
            truncated: true,
            original_len,
        }
    }
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn ceil_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// Errors a [`SmallModelClient`] call can return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmallModelError {
    /// The call would exceed the caller-supplied [`SmallModelBudget`].
    BudgetExceeded,
    /// No route is available in the registry to serve the request.
    NoRouteAvailable,
    /// The underlying provider call failed; message is human-readable only.
    ProviderError(String),
}

impl std::fmt::Display for SmallModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmallModelError::BudgetExceeded => write!(f, "small-model call would exceed budget"),
            SmallModelError::NoRouteAvailable => {
                write!(f, "no small-model route available in the registry")
            }
            SmallModelError::ProviderError(msg) => write!(f, "small-model provider error: {msg}"),
        }
    }
}

impl std::error::Error for SmallModelError {}

/// A ceiling on how much a caller is willing to spend on small-model calls
/// within some unit of work (e.g. one goal-progress audit run). Every call
/// through [`SmallModelClient`] is checked against this ceiling before it
/// runs, and refuses to run past it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmallModelBudget {
    /// Maximum number of small-model calls remaining.
    pub remaining_calls: u32,
    /// Maximum number of output tokens remaining across those calls.
    pub remaining_tokens: u32,
}

impl SmallModelBudget {
    pub fn new(remaining_calls: u32, remaining_tokens: u32) -> Self {
        Self {
            remaining_calls,
            remaining_tokens,
        }
    }

    /// True if at least one more call fits under this budget.
    pub fn can_afford(&self, estimated_tokens: u32) -> bool {
        self.remaining_calls > 0 && self.remaining_tokens >= estimated_tokens
    }

    /// Deduct the cost of one call. Saturates rather than panicking so a
    /// caller that under-estimated usage cannot underflow the budget.
    pub fn spend(&mut self, tokens_used: u32) {
        self.remaining_calls = self.remaining_calls.saturating_sub(1);
        self.remaining_tokens = self.remaining_tokens.saturating_sub(tokens_used);
    }
}

/// The trait ENG-82, ENG-83, and ENG-108 call through. Production hosts
/// provide a bounded transport; tests inject a stub with no network or process
/// dependency.
#[async_trait]
pub trait SmallModelClient: Send + Sync {
    /// Run one bounded small-model call. Implementations MUST check `budget`
    /// before doing any work and return `SmallModelError::BudgetExceeded`
    /// rather than running over it.
    async fn run(
        &self,
        task: SmallModelTask,
        input: &SmallModelInput,
        budget: &SmallModelBudget,
    ) -> Result<SmallModelOutput, SmallModelError>;
}

/// Default hard byte cap applied when a caller does not supply one via
/// [`SmallModelInput::bounded`] directly. Callers building `SmallModelInput`
/// themselves are free to choose a tighter cap; this constant only documents
/// the crate-wide default used by `RegistryBackedSmallModelClient` helpers.
pub const DEFAULT_INPUT_BYTE_CAP: usize = 16_000;

/// Default per-call token estimate used for budget pre-checks when a caller
/// has no better estimate. Conservative on purpose — small models are cheap,
/// but the point of the budget is to refuse before spending, not to be exact.
pub const DEFAULT_CALL_TOKEN_ESTIMATE: u32 = 512;

/// Production [`SmallModelClient`] that resolves the cheapest available route
/// from a [`ModelRegistry`] rather than hardcoding a model name or provider.
/// "Cheapest" here means lowest `ModelRoute::priority` among routes tagged
/// for small-model work in `tags`; callers configure that tag set when they
/// build the registry (e.g. via `providers/assets/model-metadata.toml`).
///
/// This struct only carries route-selection policy; it does not itself issue
/// the HTTP completion. Wiring it to an actual `OpenAiCompatClient` call is
/// left to the crate that owns the completion-issuing seam
/// (`host_providers::client::OpenAiCompatClient`), since this lane does
/// not own `providers/src/client.rs`. The struct still fully implements
/// [`SmallModelClient`] by resolving a route and delegating the actual call
/// through the `issue` closure supplied at construction, so callers can wire
/// a real transport without this module depending on transport internals.
pub struct RegistryBackedSmallModelClient<F>
where
    F: Fn(&ModelRoute, SmallModelTask, &SmallModelInput) -> Result<String, SmallModelError>
        + Send
        + Sync,
{
    registry: ModelRegistry,
    /// Route names eligible for small-model work, cheapest-first is derived
    /// from `ModelRoute::priority`, not from the order of this set.
    eligible_routes: Vec<String>,
    /// Issues the actual completion for a resolved route and returns raw
    /// text. Kept generic so this module never depends on the HTTP client.
    issue: F,
}

impl<F> RegistryBackedSmallModelClient<F>
where
    F: Fn(&ModelRoute, SmallModelTask, &SmallModelInput) -> Result<String, SmallModelError>
        + Send
        + Sync,
{
    pub fn new(registry: ModelRegistry, eligible_routes: Vec<String>, issue: F) -> Self {
        Self {
            registry,
            eligible_routes,
            issue,
        }
    }

    /// Resolve the cheapest (lowest-priority-value) route among
    /// `eligible_routes` that is actually registered.
    fn resolve_route(&self) -> Option<&ModelRoute> {
        self.registry
            .routes_by_priority()
            .into_iter()
            .find(|route| self.eligible_routes.iter().any(|name| name == &route.name))
    }
}

#[async_trait]
impl<F> SmallModelClient for RegistryBackedSmallModelClient<F>
where
    F: Fn(&ModelRoute, SmallModelTask, &SmallModelInput) -> Result<String, SmallModelError>
        + Send
        + Sync,
{
    async fn run(
        &self,
        task: SmallModelTask,
        input: &SmallModelInput,
        budget: &SmallModelBudget,
    ) -> Result<SmallModelOutput, SmallModelError> {
        if !budget.can_afford(DEFAULT_CALL_TOKEN_ESTIMATE) {
            return Err(SmallModelError::BudgetExceeded);
        }

        let route = self
            .resolve_route()
            .ok_or(SmallModelError::NoRouteAvailable)?;

        let raw = (self.issue)(route, task.clone(), input)?;
        Ok(parse_small_model_output(task, raw))
    }
}

/// Convert raw model text into the typed output for the requested task.
/// Kept as a free function so registry-backed and daemon transports share
/// identical parsing behavior.
pub fn parse_small_model_output(task: SmallModelTask, raw: String) -> SmallModelOutput {
    match task {
        SmallModelTask::Recap => SmallModelOutput::Recap {
            summary: raw.trim().to_string(),
        },
        SmallModelTask::ProgressAudit => {
            // Minimal structural parse: a leading "BLOCKED" or "ON_TRACK"
            // line, then free-text summary, then optional "- " blocker lines.
            let mut lines = raw.lines();
            let status_line = lines.next().unwrap_or_default().trim();
            let on_track = !status_line.eq_ignore_ascii_case("BLOCKED");
            let mut summary_lines = Vec::new();
            let mut blockers = Vec::new();
            for line in lines {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("- ") {
                    blockers.push(rest.to_string());
                } else if !trimmed.is_empty() {
                    summary_lines.push(trimmed.to_string());
                }
            }
            SmallModelOutput::ProgressAudit {
                on_track,
                summary: summary_lines.join(" "),
                blockers,
            }
        }
        SmallModelTask::DiffNarration => SmallModelOutput::DiffNarration {
            narration: raw.trim().to_string(),
        },
        SmallModelTask::SessionFollowUp => SmallModelOutput::SessionFollowUp {
            suggestion: raw.trim().to_string(),
        },
        SmallModelTask::Classify { labels } => {
            let trimmed = raw.trim();
            // First line matching a known label (case-insensitive) wins;
            // otherwise fall back to the first label with low confidence
            // rather than fabricating a match.
            let matched = labels
                .iter()
                .find(|label| trimmed.eq_ignore_ascii_case(label.as_str()))
                .cloned();
            match matched {
                Some(label) => SmallModelOutput::Classify {
                    label,
                    confidence_bucket: ConfidenceBucket::High,
                },
                None => SmallModelOutput::Classify {
                    label: labels.first().cloned().unwrap_or_default(),
                    confidence_bucket: ConfidenceBucket::Low,
                },
            }
        }
    }
}

/// Tag → route-name map used by callers that want to declare eligible small-
/// model routes by capability tag instead of by hardcoded name. Not
/// consumed by this module directly (the registry has no tag concept yet);
/// provided as the seam ENG-82/83/108 or a later lane can extend when
/// `ModelRoute` grows tags.
pub type SmallModelRouteTags = BTreeMap<String, Vec<String>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn route(name: &str, priority: u8) -> ModelRoute {
        ModelRoute::new(
            name,
            "anthropic",
            "https://api.anthropic.com/v1",
            name,
            priority,
        )
    }

    // ── SmallModelInput::bounded ───────────────────────────────────────

    #[test]
    fn bounded_passes_through_short_input_unchanged() {
        let input = SmallModelInput::bounded("hello world", 100);
        assert_eq!(input.content, "hello world");
        assert!(!input.truncated);
        assert_eq!(input.original_len, 11);
    }

    #[test]
    fn bounded_truncates_from_the_middle_keeping_both_ends() {
        let raw = "A".repeat(50) + "MIDDLE" + &"B".repeat(50);
        let input = SmallModelInput::bounded(&raw, 40);
        assert!(input.truncated);
        assert!(input.content.starts_with('A'));
        assert!(input.content.ends_with('B'));
        assert!(input.content.contains(TRUNCATION_MARKER));
        assert!(input.content.len() <= 40 + TRUNCATION_MARKER.len());
        assert_eq!(input.original_len, raw.len());
    }

    #[test]
    fn bounded_never_splits_a_multibyte_char() {
        // "é" is 2 bytes in UTF-8; place many around the midpoint so any
        // naive byte-index cut is likely to land inside one.
        let raw = "é".repeat(200);
        let input = SmallModelInput::bounded(&raw, 51);
        // Must still be valid UTF-8 (would panic on slicing otherwise).
        assert!(input.content.is_char_boundary(input.content.len()));
        let _ = input.content.chars().count();
    }

    #[test]
    fn bounded_degrades_to_head_only_when_cap_below_marker() {
        let raw = "x".repeat(100);
        let input = SmallModelInput::bounded(&raw, 5);
        assert!(input.truncated);
        assert!(input.content.len() <= 5);
        assert!(!input.content.contains(TRUNCATION_MARKER));
    }

    // ── SmallModelBudget ────────────────────────────────────────────────

    #[test]
    fn budget_can_afford_checks_both_calls_and_tokens() {
        let budget = SmallModelBudget::new(1, 100);
        assert!(budget.can_afford(50));
        assert!(!budget.can_afford(200));

        let exhausted = SmallModelBudget::new(0, 1000);
        assert!(!exhausted.can_afford(1));
    }

    #[test]
    fn budget_spend_saturates_instead_of_underflowing() {
        let mut budget = SmallModelBudget::new(1, 10);
        budget.spend(999);
        assert_eq!(budget.remaining_calls, 0);
        assert_eq!(budget.remaining_tokens, 0);
    }

    // ── stub client for downstream-lane style tests ────────────────────

    struct StubSmallModelClient {
        response: SmallModelOutput,
    }

    #[async_trait]
    impl SmallModelClient for StubSmallModelClient {
        async fn run(
            &self,
            _task: SmallModelTask,
            _input: &SmallModelInput,
            budget: &SmallModelBudget,
        ) -> Result<SmallModelOutput, SmallModelError> {
            if !budget.can_afford(DEFAULT_CALL_TOKEN_ESTIMATE) {
                return Err(SmallModelError::BudgetExceeded);
            }
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn stub_client_refuses_past_budget() {
        let stub = StubSmallModelClient {
            response: SmallModelOutput::Recap {
                summary: "ok".to_string(),
            },
        };
        let input = SmallModelInput::bounded("session data", 1000);
        let exhausted_budget = SmallModelBudget::new(0, 0);

        let result = stub
            .run(SmallModelTask::Recap, &input, &exhausted_budget)
            .await;
        assert_eq!(result, Err(SmallModelError::BudgetExceeded));
    }

    #[tokio::test]
    async fn stub_client_returns_typed_output_under_budget() {
        let stub = StubSmallModelClient {
            response: SmallModelOutput::DiffNarration {
                narration: "adds retry logic".to_string(),
            },
        };
        let input = SmallModelInput::bounded("diff hunk", 1000);
        let budget = SmallModelBudget::new(5, 5000);

        let result = stub
            .run(SmallModelTask::DiffNarration, &input, &budget)
            .await
            .expect("should succeed under budget");
        match result {
            SmallModelOutput::DiffNarration { narration } => {
                assert_eq!(narration, "adds retry logic");
            }
            other => panic!("unexpected output variant: {other:?}"),
        }
    }

    // ── RegistryBackedSmallModelClient route resolution ────────────────

    #[tokio::test]
    async fn registry_backed_client_resolves_cheapest_eligible_route() {
        let mut registry = ModelRegistry::new();
        registry.register(route("expensive-small", 10));
        registry.register(route("cheap-small", 1));
        registry.register(route("not-eligible", 0));

        let client = RegistryBackedSmallModelClient::new(
            registry,
            vec!["cheap-small".to_string(), "expensive-small".to_string()],
            |resolved_route, _task, _input| Ok(format!("used:{}", resolved_route.name)),
        );

        let input = SmallModelInput::bounded("data", 1000);
        let budget = SmallModelBudget::new(5, 5000);
        let result = client
            .run(SmallModelTask::Recap, &input, &budget)
            .await
            .expect("should resolve a route");

        match result {
            SmallModelOutput::Recap { summary } => assert_eq!(summary, "used:cheap-small"),
            other => panic!("unexpected output: {other:?}"),
        }
    }

    #[tokio::test]
    async fn registry_backed_client_errors_when_no_route_eligible() {
        let mut registry = ModelRegistry::new();
        registry.register(route("only-route", 1));

        let client = RegistryBackedSmallModelClient::new(
            registry,
            vec!["not-registered".to_string()],
            |_route, _task, _input| Ok("unused".to_string()),
        );

        let input = SmallModelInput::bounded("data", 1000);
        let budget = SmallModelBudget::new(5, 5000);
        let result = client.run(SmallModelTask::Recap, &input, &budget).await;
        assert_eq!(result, Err(SmallModelError::NoRouteAvailable));
    }

    #[tokio::test]
    async fn registry_backed_client_refuses_past_budget_before_resolving_route() {
        let mut registry = ModelRegistry::new();
        registry.register(route("cheap-small", 1));

        let client = RegistryBackedSmallModelClient::new(
            registry,
            vec!["cheap-small".to_string()],
            |_route, _task, _input| panic!("issue must not be called when budget is exhausted"),
        );

        let input = SmallModelInput::bounded("data", 1000);
        let exhausted = SmallModelBudget::new(0, 0);
        let result = client.run(SmallModelTask::Recap, &input, &exhausted).await;
        assert_eq!(result, Err(SmallModelError::BudgetExceeded));
    }

    #[test]
    fn parse_output_progress_audit_splits_status_summary_and_blockers() {
        let raw =
            "BLOCKED\nWaiting on review\nStill needs sign-off\n- missing test coverage\n- flaky CI"
                .to_string();
        let out = parse_small_model_output(SmallModelTask::ProgressAudit, raw);
        match out {
            SmallModelOutput::ProgressAudit {
                on_track,
                summary,
                blockers,
            } => {
                assert!(!on_track);
                assert_eq!(summary, "Waiting on review Still needs sign-off");
                assert_eq!(
                    blockers,
                    vec!["missing test coverage".to_string(), "flaky CI".to_string()]
                );
            }
            other => panic!("unexpected output: {other:?}"),
        }
    }

    #[test]
    fn parse_output_classify_matches_known_label_case_insensitively() {
        let out = parse_small_model_output(
            SmallModelTask::Classify {
                labels: vec!["bug".to_string(), "feature".to_string()],
            },
            "  Bug  ".to_string(),
        );
        match out {
            SmallModelOutput::Classify {
                label,
                confidence_bucket,
            } => {
                assert_eq!(label, "bug");
                assert_eq!(confidence_bucket, ConfidenceBucket::High);
            }
            other => panic!("unexpected output: {other:?}"),
        }
    }

    #[test]
    fn parse_output_classify_falls_back_low_confidence_on_no_match() {
        let out = parse_small_model_output(
            SmallModelTask::Classify {
                labels: vec!["bug".to_string(), "feature".to_string()],
            },
            "not a label at all".to_string(),
        );
        match out {
            SmallModelOutput::Classify {
                label,
                confidence_bucket,
            } => {
                assert_eq!(label, "bug");
                assert_eq!(confidence_bucket, ConfidenceBucket::Low);
            }
            other => panic!("unexpected output: {other:?}"),
        }
    }
}
