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
