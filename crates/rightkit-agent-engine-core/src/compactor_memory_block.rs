// Post-compaction memory-block re-injection seam.
//
// The compactor itself is memory-agnostic — it doesn't know what's in scope,
// which session is being compacted, or where to look. The bin-side
// `session_memory_block` builder already knows all of that, so the
// `MemoryBlockProvider` trait below is the seam the compactor invokes at
// rebuild time.
//
// Two contracts matter:
//
// 1. The provider is invoked at compaction time. Its output is **never**
//    cached on the compactor or the transcript — every compaction calls it
//    fresh so a re-compaction observes the live durable-memory set rather
//    than the block compiled at session start.
//
// 2. The injected block carries a fixed sentinel id (`memory-block:<session>`)
//    that the rebuild path uses to replace, not append. Re-compaction
//    REPLACES any prior sentinel block (mirror of the `has_continuation`
//    guard on `post-compaction-continuation`); it never accumulates.
//
// Sibling include! module: keeps `compactor.rs` under the 400-line cap.

/// Lightweight handle the compactor passes to a `MemoryBlockProvider` so
/// the provider can look up the right scope without holding a borrow on
/// the compactor internals.
#[derive(Copy, Clone)]
pub struct SessionRef<'a> {
    pub session_id: &'a str,
    pub workspace_root: &'a std::path::Path,
    pub task_id: Option<&'a str>,
    pub request_id: Option<&'a str>,
    pub generation: Option<&'a str>,
    pub capacity: CompactionCapacity,
}

#[derive(Copy, Clone, Default)]
pub struct CompactionCapacity {
    pub context_window_tokens: Option<u64>,
    pub reserved_tokens: Option<u64>,
    pub observed_context_tokens: Option<u64>,
    pub estimator_basis: Option<&'static str>,
}

/// Provider invoked at compaction time. Returns the bounded memory block to
/// inject immediately after the post-compaction continuation system message.
/// The engine constructs an instance of this trait from
/// `crate::compactor::Compactor` via `Compactor::with_memory_block_provider`,
/// and the bin implements it with the existing turn-0
/// `session_memory_block` builder so compaction and turn-0 share one source.
///
/// Contract: returning `None` (or an empty/whitespace-only block) means
/// "no memory to inject" — compaction proceeds unchanged. Implementations
/// MUST NOT panic; the engine session loop cannot recover from a panic
/// fired mid-compaction.
pub trait MemoryBlockProvider: Send + Sync {
    fn memory_block(&self, session: SessionRef<'_>) -> Option<String>;
}

/// Stable sentinel id prefix for the injected memory block. The full id
/// embeds `session_id` so concurrent or previously-compacted sessions never
/// collide on re-injection.
pub(crate) const MEMORY_BLOCK_ID_PREFIX: &str = "memory-block";

/// Build the sentinel-pinned system message that injects the provider's
/// memory block at the head of the rebuilt transcript (after the
/// continuation marker, before the compact summary).
pub(crate) fn memory_block_message(session_id: &str, content: &str) -> TranscriptMessage {
    TranscriptMessage::System {
        id: format!("{MEMORY_BLOCK_ID_PREFIX}:{session_id}"),
        created_at: now_iso(),
        content: content.to_string(),
        pinned: true,
    }
}

/// Predicate mirroring `has_continuation` — true if the rebuild set already
/// carries a memory-block sentinel for this session, so the rebuild path
/// knows to REPLACE the block rather than append a duplicate.
pub(crate) fn has_memory_block(messages: &[TranscriptMessage], session_id: &str) -> bool {
    let id = format!("{MEMORY_BLOCK_ID_PREFIX}:{session_id}");
    messages.iter().any(|message| {
        matches!(
            message,
            TranscriptMessage::System { id: existing, .. } if existing == &id
        )
    })
}

/// Strip any existing memory-block sentinel for this session from a rebuild
/// set. Used after the rebuild predicate detects a prior injection; the
/// rebuild path then re-inserts the fresh block with the same id.
pub(crate) fn strip_prior_memory_block(
    messages: Vec<TranscriptMessage>,
    session_id: &str,
) -> Vec<TranscriptMessage> {
    let id = format!("{MEMORY_BLOCK_ID_PREFIX}:{session_id}");
    messages
        .into_iter()
        .filter(|message| {
            !matches!(
                message,
                TranscriptMessage::System { id: existing, .. } if existing == &id
            )
        })
        .collect()
}
