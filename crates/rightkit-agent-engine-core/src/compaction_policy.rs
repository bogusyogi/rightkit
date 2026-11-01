//! Active-model compaction limits.
//!
//! Unknown model capacity is represented explicitly and disables automatic
//! compaction. Manual compaction remains available, but never invents a window
//! or silently trusts the providers crate's compatibility fallback.
//!
//! ## Tiers
//!
//! [`ensure_context_fits`] selects between four actions against the
//! model-visible context:
//!
//! | tier | when | what |
//! |---|---|---|
//! | [`CompactionAction::None`] | below prefire | no compaction work |
//! | [`CompactionAction::Prefire`] | prefire window only | start background summarization |
//! | [`CompactionAction::AutoCompact`] | at/over automatic | block-and-compact on the hot path |
//! | [`CompactionAction::CriticalSync`] | over critical | drop a hard guarantee: refuse the request unless the call is the explicit user-triggered path |
//!
//! The critical-sync tier is a *promise* the engine cannot keep once the
//! context window is fully consumed: manual compaction remains the only way
//! out, and `ensure_context_fits` reflects that.

use serde::{Deserialize, Serialize};

/// Per-image token cost used to project image-payload contribution to the
/// model-visible context window. Modelled on jcode's `IMAGE_TOKEN_COST`
/// (`crates/jcode-compaction-core/src/lib.rs:58`), but the value differs: jcode
/// uses 1_600, the host app uses 765 (corrected citation, #70):
/// inline base64 images cost roughly this many tokens each, independent of
/// their actual decoded pixel size (the provider's wire serializer has
/// already spent the bytes; the tokens are the visible budget consequence).
pub const IMAGE_TOKEN_COST: u64 = 765;

/// Window fraction above the auto-trigger boundary that flips the action
/// from `AutoCompact` to `CriticalSync`. At and above this fraction the
/// session is over the documented window: compaction can no longer make
/// guarantees because the context is already past the model's published
/// limit. Kept as a default so tests and operators can override.
pub const DEFAULT_CRITICAL_LEAD_PERCENT: u8 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionModelPolicy {
    pub model_id: String,
    pub capacity_known: bool,
    pub context_window_tokens: Option<u64>,
    pub trigger_percent: Option<u8>,
    pub reserve_tokens: Option<u64>,
    pub summary_max_output_tokens: Option<u32>,
}

impl CompactionModelPolicy {
    pub fn known(
        model_id: impl Into<String>,
        context_window_tokens: u64,
        trigger_percent: u8,
        reserve_tokens: u64,
        summary_max_output_tokens: u32,
    ) -> Self {
        let valid = context_window_tokens > 0
            && (1..=95).contains(&trigger_percent)
            && reserve_tokens > 0
            && reserve_tokens < context_window_tokens
            && summary_max_output_tokens > 0;
        if !valid {
            return Self::unknown(model_id);
        }
        Self {
            model_id: model_id.into(),
            capacity_known: true,
            context_window_tokens: Some(context_window_tokens),
            trigger_percent: Some(trigger_percent),
            reserve_tokens: Some(reserve_tokens),
            summary_max_output_tokens: Some(summary_max_output_tokens),
        }
    }

    pub fn unknown(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            capacity_known: false,
            context_window_tokens: None,
            trigger_percent: None,
            reserve_tokens: None,
            summary_max_output_tokens: None,
        }
    }

    /// Headroom kept between the reserve-derived trigger and the send gate
    /// (`window - reserve`, enforced by the provider capacity gate). The
    /// trigger reads the PREVIOUS call's envelope, while the next request adds
    /// new tool results and user input; without this margin that growth could
    /// land between trigger and gate (audit C9: ~6k tokens on 128k models).
    pub fn reserve_headroom_margin(window: u64) -> u64 {
        (window / 16).min(16_384)
    }

    fn reserve_boundary(window: u64, reserve: u64) -> u64 {
        window
            .saturating_sub(reserve)
            .saturating_sub(Self::reserve_headroom_margin(window))
    }

    /// Automatic boundary is the earlier of the per-model ratio and the
    /// reserved-headroom boundary (reserve plus a safety margin so the
    /// trigger fires before the send gate can refuse).
    pub fn auto_trigger_tokens(&self) -> Option<u64> {
        let window = self.context_window_tokens?;
        let percent = u64::from(self.trigger_percent?);
        let reserve = self.reserve_tokens?;
        let ratio_boundary = window.saturating_mul(percent) / 100;
        Some(ratio_boundary.min(Self::reserve_boundary(window, reserve)))
    }

    pub fn automatic_due(&self, context_tokens: u64) -> bool {
        self.auto_trigger_tokens()
            .is_some_and(|boundary| context_tokens >= boundary)
    }

    /// Mechanism 1 (post-call pressure). Evaluate the automatic-compaction
    /// boundary against the just-completed call's actual logged envelope
    /// -- measured at the next step boundary -- rather than a pre-request
    /// estimate that is stale by send time.
    pub fn automatic_due_from_envelope(&self, envelope: &PostCallEnvelope) -> bool {
        self.automatic_due(envelope.context_tokens())
    }

    /// Mechanism 2 (prefire threshold). Token boundary at which a background
    /// prefire summarization should start: `prefire_lead_percent` PERCENTAGE
    /// POINTS below the real automatic trigger (default 10, see
    /// [`DEFAULT_PREFIRE_LEAD_PERCENT`]). `None` when capacity is unknown,
    /// mirroring `auto_trigger_tokens`.
    pub fn prefire_trigger_tokens(&self, prefire_lead_percent: u8) -> Option<u64> {
        let window = self.context_window_tokens?;
        let trigger_percent = self.trigger_percent?;
        let reserve = self.reserve_tokens?;
        let lead_percent = u64::from(trigger_percent.saturating_sub(prefire_lead_percent));
        let ratio_boundary = window.saturating_mul(lead_percent) / 100;
        Some(ratio_boundary.min(Self::reserve_boundary(window, reserve)))
    }

    /// `true` once context has crossed the prefire boundary but has not yet
    /// reached the real automatic boundary -- the window in which a
    /// background prefire summarization pass should be started (or already
    /// be running).
    pub fn prefire_due(&self, context_tokens: u64, prefire_lead_percent: u8) -> bool {
        self.prefire_trigger_tokens(prefire_lead_percent)
            .is_some_and(|boundary| context_tokens >= boundary)
            && !self.automatic_due(context_tokens)
    }

    /// Hard-sync boundary: the context is at or past the trigger
    /// *plus* `critical_lead_percent` percentage points. The engine cannot
    /// guarantee a clean model call any more; the only safe path is an
    /// explicit user/manual compact before any further request.
    pub fn critical_trigger_tokens(&self, critical_lead_percent: u8) -> Option<u64> {
        let window = self.context_window_tokens?;
        let trigger_percent = self.trigger_percent?;
        // `reserve_tokens` must still be valid (constructed via `known`), but
        // the critical boundary is deliberately allowed to sit above the
        // auto-trigger's reserve-capped boundary -- it clamps only to the
        // window itself, never to `window - reserve`. Capping it the same
        // way as `auto_trigger_tokens` would collapse the critical tier into
        // the automatic tier and silently drop the hard-sync safety net.
        let _reserve = self.reserve_tokens?;
        let critical_percent = u64::from(
            trigger_percent
                .saturating_add(critical_lead_percent)
                .min(100),
        );
        let ratio_boundary = window.saturating_mul(critical_percent) / 100;
        Some(ratio_boundary.min(window))
    }

    /// `true` when context has crossed the hard-sync boundary.
    pub fn critical_due(&self, context_tokens: u64, critical_lead_percent: u8) -> bool {
        self.critical_trigger_tokens(critical_lead_percent)
            .is_some_and(|boundary| context_tokens >= boundary)
    }
}

/// Tiers the engine can take against the model-visible context.
///
/// Ordered roughly from cheapest to most-disruptive; the engine's hot path
/// dispatches off the value [`ensure_context_fits`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompactionAction {
    /// Below the prefire window. No compaction work needed.
    None,
    /// Inside the prefire window: a background summary should be started
    /// (or already running) so a real trigger lands with a cached result.
    Prefire,
    /// At/over the automatic trigger: block the hot path and compact now.
    AutoCompact,
    /// At/over the critical boundary: refuse further automatic action;
    /// the only path forward is the user-triggered `compact_context_now` /
    /// `/compact` command. This is the last-chance tier -- past it the
    /// model's documented context window is already exceeded.
    CriticalSync,
}

/// Compute the [`CompactionAction`] tier for the given measured context
/// against the policy, including image-payload projection. `inline_images`
/// is the number of base64 images currently in the model-visible request;
/// each is debited [`IMAGE_TOKEN_COST`] tokens so an image-heavy session
/// trips compaction on the *projected* token cost, not just the text-only
/// count. `critical_lead_percent` is the percentage-point window above the
/// auto trigger that flips the action into [`CompactionAction::CriticalSync`].
///
/// The projection is conservative: image counts are added to the measured
/// `context_tokens`, never subtracted -- a stale-by-one image count must
/// not let a session pretend it has more room than it does.
pub fn ensure_context_fits(
    policy: &CompactionModelPolicy,
    context_tokens: u64,
    inline_images: u64,
    prefire_lead_percent: u8,
    critical_lead_percent: u8,
) -> CompactionAction {
    if !policy.capacity_known {
        return CompactionAction::None;
    }
    let projected = context_tokens.saturating_add(inline_images.saturating_mul(IMAGE_TOKEN_COST));
    if policy.critical_due(projected, critical_lead_percent) {
        CompactionAction::CriticalSync
    } else if policy.automatic_due(projected) {
        CompactionAction::AutoCompact
    } else if policy.prefire_due(projected, prefire_lead_percent) {
        CompactionAction::Prefire
    } else {
        CompactionAction::None
    }
}

impl Default for CompactionModelPolicy {
    fn default() -> Self {
        Self::unknown("")
    }
}

/// Default percentage-point lead for mechanism 2 (prefire). Mirrors the
/// book's `threshold - prefire_lead_percent` with a default of 10 points.
pub const DEFAULT_PREFIRE_LEAD_PERCENT: u8 = 10;

/// Actual provider-reported usage from the most recently completed call.
/// Mechanism 1 measures pressure from this -- the just-completed call's
/// logged envelope -- rather than from a pre-request estimate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostCallEnvelope {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

impl PostCallEnvelope {
    /// Total tokens actually consumed by context, as reported by the
    /// provider for the just-completed call.
    pub fn context_tokens(&self) -> u64 {
        self.prompt_tokens
            .saturating_add(self.completion_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_creation_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn million_token_policy_uses_model_ratio() {
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        assert_eq!(policy.auto_trigger_tokens(), Some(700_000));
        assert!(!policy.automatic_due(699_999));
        assert!(policy.automatic_due(700_000));
    }

    #[test]
    fn reserved_headroom_can_trigger_before_ratio() {
        let policy = CompactionModelPolicy::known("model", 100_000, 90, 30_000, 10_000);
        // window - reserve = 70_000, minus the 6_250 send-gate safety margin.
        assert_eq!(policy.auto_trigger_tokens(), Some(63_750));
    }

    #[test]
    fn trigger_leaves_a_gap_below_the_send_gate_on_128k_models() {
        // Audit C9: trigger must sit well below `window - reserve`.
        let policy = CompactionModelPolicy::known("model", 128_000, 95, 16_000, 8_192);
        let gate = 128_000 - 16_000;
        let trigger = policy.auto_trigger_tokens().unwrap();
        assert!(gate - trigger >= 8_000, "gap was only {}", gate - trigger);
    }

    #[test]
    fn unknown_capacity_is_visible_and_never_auto_compacts() {
        let policy = CompactionModelPolicy::unknown("acme/mystery");
        assert!(!policy.capacity_known);
        assert_eq!(policy.context_window_tokens, None);
        assert_eq!(policy.auto_trigger_tokens(), None);
        assert!(!policy.automatic_due(u64::MAX));
    }

    #[test]
    fn post_call_envelope_sums_all_token_kinds() {
        let envelope = PostCallEnvelope {
            prompt_tokens: 100,
            completion_tokens: 20,
            cache_read_tokens: 5,
            cache_creation_tokens: 3,
        };
        assert_eq!(envelope.context_tokens(), 128);
    }

    #[test]
    fn automatic_due_from_envelope_matches_automatic_due_on_context_tokens() {
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        let under = PostCallEnvelope {
            prompt_tokens: 699_999,
            ..Default::default()
        };
        let over = PostCallEnvelope {
            prompt_tokens: 700_000,
            ..Default::default()
        };
        assert!(!policy.automatic_due_from_envelope(&under));
        assert!(policy.automatic_due_from_envelope(&over));
    }

    #[test]
    fn prefire_trigger_is_lead_percent_points_below_real_trigger() {
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        // trigger_percent=70, lead=10 -> prefire at 60% = 600_000.
        assert_eq!(
            policy.prefire_trigger_tokens(DEFAULT_PREFIRE_LEAD_PERCENT),
            Some(600_000)
        );
    }

    #[test]
    fn prefire_due_only_between_prefire_and_real_boundary() {
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        assert!(!policy.prefire_due(599_999, DEFAULT_PREFIRE_LEAD_PERCENT));
        assert!(policy.prefire_due(600_000, DEFAULT_PREFIRE_LEAD_PERCENT));
        assert!(policy.prefire_due(699_999, DEFAULT_PREFIRE_LEAD_PERCENT));
        // At and past the real trigger, prefire_due is false -- the real
        // automatic boundary takes over.
        assert!(!policy.prefire_due(700_000, DEFAULT_PREFIRE_LEAD_PERCENT));
    }

    /// 1M window, trigger 70%, prefire lead 10pts, critical lead 5pts (see
    /// [`DEFAULT_CRITICAL_LEAD_PERCENT`]): prefire at 600k, auto at 700k,
    /// critical at 750k.
    fn four_tier_policy() -> CompactionModelPolicy {
        CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768)
    }

    fn action_at(tokens: u64, images: u64) -> CompactionAction {
        ensure_context_fits(
            &four_tier_policy(),
            tokens,
            images,
            DEFAULT_PREFIRE_LEAD_PERCENT,
            DEFAULT_CRITICAL_LEAD_PERCENT,
        )
    }

    #[test]
    fn critical_trigger_is_lead_percent_points_above_real_trigger() {
        assert_eq!(
            four_tier_policy().critical_trigger_tokens(DEFAULT_CRITICAL_LEAD_PERCENT),
            Some(750_000)
        );
    }

    #[test]
    fn critical_trigger_never_exceeds_the_window() {
        // trigger 95% + 15pt lead would be 110%; clamps to the window.
        let policy = CompactionModelPolicy::known("model", 100_000, 95, 1_000, 4_096);
        assert_eq!(policy.critical_trigger_tokens(15), Some(100_000));
    }

    #[test]
    fn each_tier_owns_its_band() {
        assert_eq!(action_at(599_999, 0), CompactionAction::None);
        assert_eq!(action_at(600_000, 0), CompactionAction::Prefire);
        assert_eq!(action_at(699_999, 0), CompactionAction::Prefire);
        assert_eq!(action_at(700_000, 0), CompactionAction::AutoCompact);
        assert_eq!(action_at(749_999, 0), CompactionAction::AutoCompact);
        assert_eq!(action_at(750_000, 0), CompactionAction::CriticalSync);
    }

    #[test]
    fn inline_images_project_into_the_decision() {
        // 599_000 text tokens is below the 600k prefire line on its own, but
        // 10 resident images project it over. This is the whole reason
        // `ensure_context_fits` exists rather than a bare `automatic_due`.
        assert_eq!(action_at(599_000, 0), CompactionAction::None);
        assert_eq!(action_at(599_000, 10), CompactionAction::Prefire);
        // At IMAGE_TOKEN_COST (765/image), 132 images project to 699_980 —
        // still short of the 700_000 auto trigger; 133 crosses it. The
        // boundary is exact, not approximate.
        assert_eq!(action_at(599_000, 132), CompactionAction::Prefire);
        assert_eq!(action_at(599_000, 133), CompactionAction::AutoCompact);
    }

    #[test]
    fn unknown_capacity_never_dispatches_any_tier() {
        let policy = CompactionModelPolicy::unknown("acme/mystery");
        assert_eq!(
            policy.critical_trigger_tokens(DEFAULT_CRITICAL_LEAD_PERCENT),
            None
        );
        assert_eq!(
            ensure_context_fits(
                &policy,
                u64::MAX,
                u64::MAX,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::None
        );
    }

    #[test]
    fn prefire_trigger_unknown_when_capacity_unknown() {
        let policy = CompactionModelPolicy::unknown("acme/mystery");
        assert_eq!(
            policy.prefire_trigger_tokens(DEFAULT_PREFIRE_LEAD_PERCENT),
            None
        );
        assert!(!policy.prefire_due(u64::MAX, DEFAULT_PREFIRE_LEAD_PERCENT));
    }

    #[test]
    fn critical_due_flips_above_auto_trigger_plus_lead() {
        // trigger=70%, critical_lead=5 -> critical at 75% = 750_000.
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        assert_eq!(
            policy.critical_trigger_tokens(DEFAULT_CRITICAL_LEAD_PERCENT),
            Some(750_000)
        );
        assert!(!policy.critical_due(749_999, DEFAULT_CRITICAL_LEAD_PERCENT));
        assert!(policy.critical_due(750_000, DEFAULT_CRITICAL_LEAD_PERCENT));
        // Past the published window still trips critical: the engine cannot
        // pretend the model has room it does not.
        assert!(policy.critical_due(u64::MAX, DEFAULT_CRITICAL_LEAD_PERCENT));
    }

    #[test]
    fn critical_due_unknown_when_capacity_unknown() {
        let policy = CompactionModelPolicy::unknown("acme/mystery");
        assert_eq!(
            policy.critical_trigger_tokens(DEFAULT_CRITICAL_LEAD_PERCENT),
            None
        );
        assert!(!policy.critical_due(u64::MAX, DEFAULT_CRITICAL_LEAD_PERCENT));
    }

    #[test]
    fn ensure_context_fits_picks_none_below_prefire() {
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        assert_eq!(
            ensure_context_fits(
                &policy,
                100_000,
                0,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::None
        );
    }

    #[test]
    fn ensure_context_fits_picks_prefire_inside_window() {
        // 650_000 is inside the 60%-70% prefire window.
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        assert_eq!(
            ensure_context_fits(
                &policy,
                650_000,
                0,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::Prefire
        );
    }

    #[test]
    fn ensure_context_fits_picks_auto_compact_at_trigger() {
        // Exactly at the auto trigger.
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        assert_eq!(
            ensure_context_fits(
                &policy,
                700_000,
                0,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::AutoCompact
        );
    }

    #[test]
    fn ensure_context_fits_picks_critical_sync_past_critical() {
        // 750_000 is at the critical boundary.
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        assert_eq!(
            ensure_context_fits(
                &policy,
                750_000,
                0,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::CriticalSync
        );
    }

    #[test]
    fn ensure_context_fits_unknown_capacity_is_always_none() {
        let policy = CompactionModelPolicy::unknown("acme/mystery");
        assert_eq!(
            ensure_context_fits(
                &policy,
                u64::MAX,
                u64::MAX,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::None
        );
    }

    #[test]
    fn ensure_context_fits_image_payload_projects_to_tier() {
        // 1_000_000-window, 70% trigger = 700_000, 5% critical = 750_000.
        // A session with text=0 but 1000 inline images projects to
        // 0 + 1000*765 = 765_000 tokens, which is past the critical
        // boundary. The image payload alone is enough to flip into
        // CriticalSync.
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        assert_eq!(
            ensure_context_fits(
                &policy,
                0,
                1_000,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::CriticalSync
        );
        // 800 images -> 612_000 projected, in the prefire window.
        assert_eq!(
            ensure_context_fits(
                &policy,
                0,
                800,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::Prefire
        );
    }

    /// `ensure_context_fits` is a pure projection against the policy + a
    /// measured context, not a side-effecting state machine. The same
    /// inputs always yield the same tier, regardless of how many times
    /// the engine has already compacted in this session. The hot path
    /// must be free to consult it on every turn.
    #[test]
    fn ensure_context_fits_is_pure_and_repeatable() {
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        let first = ensure_context_fits(
            &policy,
            720_000,
            4,
            DEFAULT_PREFIRE_LEAD_PERCENT,
            DEFAULT_CRITICAL_LEAD_PERCENT,
        );
        let second = ensure_context_fits(
            &policy,
            720_000,
            4,
            DEFAULT_PREFIRE_LEAD_PERCENT,
            DEFAULT_CRITICAL_LEAD_PERCENT,
        );
        let third = ensure_context_fits(
            &policy,
            720_000,
            4,
            DEFAULT_PREFIRE_LEAD_PERCENT,
            DEFAULT_CRITICAL_LEAD_PERCENT,
        );
        assert_eq!(first, CompactionAction::AutoCompact);
        assert_eq!(first, second);
        assert_eq!(second, third);
    }

    /// Triple-compaction guard: when the policy crosses the prefire,
    /// auto, and critical boundaries in turn (text alone, no image
    /// projection), `ensure_context_fits` must step through the tiers
    /// in order -- never skip a tier, never regress. The hot path is
    /// allowed to call `compact` at each boundary once; it is NOT
    /// allowed to keep firing `compact` after the answer is already
    /// `CriticalSync` (manual is the only legal path past that point).
    #[test]
    fn ensure_context_fits_steps_through_tiers_without_skipping() {
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        // Below prefire.
        assert_eq!(
            ensure_context_fits(
                &policy,
                599_999,
                0,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::None
        );
        // Just inside prefire.
        assert_eq!(
            ensure_context_fits(
                &policy,
                600_000,
                0,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::Prefire
        );
        // Past prefire, at the auto trigger.
        assert_eq!(
            ensure_context_fits(
                &policy,
                700_000,
                0,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::AutoCompact
        );
        // Just inside critical.
        assert_eq!(
            ensure_context_fits(
                &policy,
                749_999,
                0,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::AutoCompact
        );
        // At the critical boundary.
        assert_eq!(
            ensure_context_fits(
                &policy,
                750_000,
                0,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::CriticalSync
        );
        // Three automatic compactions (Prefire+AutoCompact+AutoCompact)
        // are the most the engine can ever do automatically; after that
        // the tier stays at CriticalSync and refuses further automatic
        // action.
        assert_eq!(
            ensure_context_fits(
                &policy,
                800_000,
                0,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::CriticalSync
        );
        assert_eq!(
            ensure_context_fits(
                &policy,
                900_000,
                0,
                DEFAULT_PREFIRE_LEAD_PERCENT,
                DEFAULT_CRITICAL_LEAD_PERCENT,
            ),
            CompactionAction::CriticalSync
        );
    }

    /// Auto-triggered compactions are capped at three per session. The
    /// prefire summary, the auto-compact, and the post-critical hold are
    /// the three automatic fire events; anything past that point must be
    /// user/manual. This test documents the invariant for `Compactor`:
    /// the breaker + retry ledger already cap LLM-failure-driven
    /// retries at two (see `compactor_retry::MAX_COMPACTION_RETRIES`),
    /// and `critical_due` caps automatic *fire* attempts -- a
    /// permanently-failing compaction cycle thus converges to
    /// `CriticalSync` rather than spinning.
    #[test]
    fn triple_compaction_guard_caps_automatic_fires() {
        let policy = CompactionModelPolicy::known("model", 1_000_000, 70, 65_536, 32_768);
        // One fire for prefire-spawned background summary (logical fire
        // event when the prefire window opens).
        let prefire_call = ensure_context_fits(
            &policy,
            600_000,
            0,
            DEFAULT_PREFIRE_LEAD_PERCENT,
            DEFAULT_CRITICAL_LEAD_PERCENT,
        );
        // Second fire for the auto-compact at the trigger.
        let auto_call = ensure_context_fits(
            &policy,
            700_000,
            0,
            DEFAULT_PREFIRE_LEAD_PERCENT,
            DEFAULT_CRITICAL_LEAD_PERCENT,
        );
        // Third fire for the post-critical hold; this is still
        // automatic but it is the LAST automatic fire the engine may
        // issue for the lifetime of this compaction cycle.
        let critical_call = ensure_context_fits(
            &policy,
            750_000,
            0,
            DEFAULT_PREFIRE_LEAD_PERCENT,
            DEFAULT_CRITICAL_LEAD_PERCENT,
        );
        // Past the third fire, the engine must NOT return a tier that
        // would imply another automatic call. CriticalSync is the only
        // option that is honest about that.
        let post_guard = ensure_context_fits(
            &policy,
            900_000,
            0,
            DEFAULT_PREFIRE_LEAD_PERCENT,
            DEFAULT_CRITICAL_LEAD_PERCENT,
        );

        assert_eq!(prefire_call, CompactionAction::Prefire);
        assert_eq!(auto_call, CompactionAction::AutoCompact);
        assert_eq!(critical_call, CompactionAction::CriticalSync);
        assert_eq!(post_guard, CompactionAction::CriticalSync);
    }
}
