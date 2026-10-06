//! [DROP-IN CORE]
//! Explicit model/search policy. The point is to stop platform-specific magic
//! constants from becoming implicit semantics during extraction.

/// Tuned macOS mitigation introduced by local commit c0239763.
/// IMPORTANT: the 224-clip canonical corpus used to choose this value is now a
/// tuning/dev corpus. Do not claim 6.66% as blind evaluation.
pub const MACOS_POST_C0239763_EMIT_REWARD: f32 = 0.15;

/// Emission reward for the ONNX (Windows) beam. At 0.15 (the macOS value),
/// Windows field recordings still paid for a spurious leading filler
/// ("Okay. Testing one, two, three." on a clip that is "Testing one, two,
/// three."); 0.10 did not, while sitting inside the macOS corpus's flat
/// optimum (6.66% across 0.10-0.20). Shared mechanism, platform-specific
/// value — the same rule the duration cap follows. macOS keeps its
/// validated 0.15 untouched.
pub const WINDOWS_EMIT_REWARD: f32 = 0.10;

/// Current vendored Windows model code clamps the duration *logit index* with
/// `.min(self.config.duration_cap)`. Preserve index-cap semantics during extraction.
pub const WINDOWS_VENDOR_DURATION_CAP_INDEX: usize = 2;

/// Model-defined semantics that must be supplied by each runtime/model adapter.
/// No cross-platform defaults are provided deliberately: blank ID and duration
/// table are correctness-critical and must be copied from the actual shipped model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSemantics {
    pub blank_id: usize,
    /// Maps a duration-logit index to encoder-time advance. Example only: a model
    /// could use `[0, 1, 2, 3, 4]`; adapters must supply the real shipped table.
    pub duration_table: Vec<usize>,
}

impl ModelSemantics {
    pub fn new(blank_id: usize, duration_table: Vec<usize>) -> Self {
        assert!(
            !duration_table.is_empty(),
            "duration table must not be empty"
        );
        Self {
            blank_id,
            duration_table,
        }
    }

    /// Apply an optional cap to the *logit index*, matching current Windows
    /// `.min(duration_cap)` semantics, then map through the model duration table.
    pub fn duration_for_index(
        &self,
        raw_duration_index: usize,
        cap_index: Option<usize>,
    ) -> Option<(usize, usize)> {
        let index = match cap_index {
            Some(cap) => raw_duration_index.min(cap),
            None => raw_duration_index,
        };
        self.duration_table
            .get(index)
            .copied()
            .map(|frames| (index, frames))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruningMode {
    /// Preserve the current platform loop's cross-time/global behavior.
    /// Use this during extraction. Do not "improve" pruning while moving code.
    LegacyGlobal,

    /// Architectural follow-up: compare/prune hypotheses at equal encoder time.
    /// This is intentionally opt-in because it changes search semantics.
    TimeSynchronous,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodePolicy {
    pub beam_width: usize,
    pub emit_reward: f32,
    /// Optional cap on the duration-output index, matching the existing Windows
    /// `.min(duration_cap)` behavior. `None` means no additional cap.
    pub duration_cap_index: Option<usize>,
    pub pruning_mode: PruningMode,
    /// Safety guard for malformed model outputs / zero-duration loops.
    pub max_search_steps: usize,
}

impl DecodePolicy {
    pub fn extraction_compatible(beam_width: usize) -> Self {
        Self {
            beam_width: beam_width.max(1),
            emit_reward: 0.0,
            duration_cap_index: None,
            pruning_mode: PruningMode::LegacyGlobal,
            max_search_steps: 100_000,
        }
    }

    /// Known post-c0239763 macOS policy. Keep the remaining loop semantics copied
    /// from the actual local branch; this constructor does not guess duration or
    /// candidate-selection behavior that is not represented here.
    pub fn macos_post_c0239763() -> Self {
        Self {
            beam_width: 2,
            emit_reward: MACOS_POST_C0239763_EMIT_REWARD,
            duration_cap_index: None,
            pruning_mode: PruningMode::LegacyGlobal,
            max_search_steps: 100_000,
        }
    }

    /// Apply the current Windows vendored duration-index clamp without asserting
    /// that every other Windows search setting is identical to macOS.
    pub fn with_windows_vendor_duration_cap(mut self) -> Self {
        self.duration_cap_index = Some(WINDOWS_VENDOR_DURATION_CAP_INDEX);
        self
    }
}

