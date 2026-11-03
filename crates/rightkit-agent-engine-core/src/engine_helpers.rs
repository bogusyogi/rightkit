// Shared helpers used by the session engine — extracted from
// `engine.rs` to keep it under the 400-line convention. These are
// pure functions / thin wrappers with no engine state; pulling them
// out makes the engine struct itself more focused.

use super::*;
use crate::cache::StableCacheKey;

/// Maximum UTF-8 byte size of the stable system kernel assigned cache identity.
///
/// The engine's deterministic prompt-budget estimator is `ceil(chars / 4)`.
/// A 12,000-byte ceiling is conservative for UTF-8 (`chars <= bytes`) and
/// therefore bounds the kernel at no more than 3,000 estimated tokens.  The
/// complete prefix is still sent when it exceeds this ceiling, without stable
/// cache identity. It is never silently truncated, so cache identity cannot
/// describe bytes the provider did not receive.
pub(crate) const STABLE_SYSTEM_KERNEL_MAX_BYTES: usize = 12_000;

/// Result of inspecting the cache-stable system prefix.
///
/// `NoPinnedPrefix` and `Oversized` are deliberately separate: callers that
/// assemble the provider request can preserve the transcript while emitting a
/// diagnostic or disabling cache identity only for the over-budget case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StableSystemPrefixOutcome {
    NoPinnedPrefix,
    WithinBudget(StableCacheKey),
    Oversized { byte_len: usize },
}

/// Current wall-clock time as an ISO-8601 string. Thin wrapper around
/// `rightkit_agent_engine_protocol::now_iso` so call sites can stay scoped to the
/// engine crate.
pub(crate) fn now_iso() -> String {
    rightkit_agent_engine_protocol::now_iso()
}

/// Monotonic-ish nanosecond timestamp. Best-effort: returns `0` if
/// the system clock is before the UNIX epoch.
pub(crate) fn now_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Inspect the stable prefix while preserving the distinction between an
/// absent prefix and one rejected for exceeding the deterministic budget.
///
/// Hermes three-tier split: only the *stable* tier (concatenated, pinned
/// System messages) and the explicitly supplied day bucket contribute to the
/// cache hash. Volatile content (per-turn memories, second-granular
/// timestamps) lives in the volatile tier and is intentionally excluded, so
/// clock drift cannot break cache stability. Used by the agent loop to observe
/// the cache tracker and by the self-review fork to derive its cache key.
/// Day-aware and clock-free: callers MUST pass the `engine.day_key` explicitly
/// so this function never reads the wall clock.
pub(crate) fn inspect_stable_system_prefix(
    messages: &[TranscriptMessage],
    day: &str,
) -> StableSystemPrefixOutcome {
    // P2-6: contiguous real wire prefix — only the leading contiguous block
    // of pinned System messages contributes. The first non-System (history)
    // terminates the prefix; any subsequent System (volatile overlay such as
    // goal reminder) is after history and never enters the hash.
    let mut prefix = String::new();
    for message in messages {
        match message {
            TranscriptMessage::System {
                content, pinned, ..
            } if *pinned => {
                prefix.push_str(content);
                prefix.push('\n');
            }
            TranscriptMessage::System { .. } => {}
            _ => break,
        }
    }
    if prefix.is_empty() {
        StableSystemPrefixOutcome::NoPinnedPrefix
    } else if prefix.len() > STABLE_SYSTEM_KERNEL_MAX_BYTES {
        // Reject the complete stable prefix rather than hashing a truncated
        // approximation. The byte length is retained so callers can expose
        // the budget violation without inspecting or logging prompt content.
        StableSystemPrefixOutcome::Oversized {
            byte_len: prefix.len(),
        }
    } else {
        StableSystemPrefixOutcome::WithinBudget(StableCacheKey::new(prefix, day))
    }
}

/// Build the stable-tier cache key, omitting it when no pinned prefix exists
/// or when the complete prefix exceeds the kernel budget. Use
/// [`inspect_stable_system_prefix`] when those cases need to remain distinct.
pub(crate) fn pinned_system_prefix_as_stable(
    messages: &[TranscriptMessage],
    day: &str,
) -> Option<StableCacheKey> {
    match inspect_stable_system_prefix(messages, day) {
        StableSystemPrefixOutcome::WithinBudget(key) => Some(key),
        StableSystemPrefixOutcome::NoPinnedPrefix | StableSystemPrefixOutcome::Oversized { .. } => {
            None
        }
    }
}
