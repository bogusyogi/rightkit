// Mechanism 2 (book §P2-3, 3-2): prefire with fingerprint validation.
//
// Sibling include! module of compactor.rs -- shares its module scope, no
// mod/use headers of its own. `fingerprint_prefix` below was cross-checked
// against the reference at
// `repos/cli/grok-build/crates/codegen/xai-grok-shell/src/session/compaction.rs`
// (reachable via network share) after an initial length-based-hash defect
// was caught by adversarial review; it now matches the reference's
// tag+content hashing shape.

/// Cheap, non-cryptographic fingerprint over a message slice. Mirrors
/// grok-build's `fingerprint_prefix`
/// (`session/compaction.rs`): hash the count, then per message the role/tag
/// AND the actual text content -- never a length or any other proxy for
/// content, because two different strings can share a byte length and a
/// length-based hash would silently treat a same-length in-place edit as
/// "unchanged". Detects "the prefix this cache was built from is still the
/// prefix we have now" -- covering rewinds, edits (same-length or not), and
/// any other mutation between a prefire pass and the real trigger. Message
/// id is intentionally NOT hashed here (ids can be stable across a content
/// edit); `message_identity` below is for id-based lookups, not fingerprinting.
pub fn fingerprint_prefix(messages: &[TranscriptMessage]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    messages.len().hash(&mut hasher);
    for message in messages {
        let (tag, text) = message_tag_and_text(message);
        tag.hash(&mut hasher);
        text.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Role/tag discriminant plus the actual text content used for fingerprinting.
/// The tag distinguishes e.g. a User and a Tool message that happen to carry
/// identical text; the text itself (not its length) is what changes the
/// fingerprint on an in-place edit.
fn message_tag_and_text(message: &TranscriptMessage) -> (u8, &str) {
    match message {
        TranscriptMessage::System { content, .. } => (0, content.as_str()),
        TranscriptMessage::User { content, .. } => (1, content.as_str()),
        TranscriptMessage::Assistant { content, .. } => (2, content.as_str()),
        TranscriptMessage::Tool { summary, .. } => (3, summary.as_str()),
        TranscriptMessage::Compact { summary, .. } => (4, summary.as_str()),
        TranscriptMessage::Fold { summary, .. } => (5, summary.as_str()),
    }
}

/// Shared by mechanism 5 (tag-based retention, see `compactor_cheap_prune.rs`)
/// for id-keyed always-keep lookups. NOT used by `fingerprint_prefix` --
/// fingerprinting hashes tag+content via `message_tag_and_text` above, never
/// a length. Defined once here since siblings share one module scope and
/// must not duplicate item names.
pub(crate) fn message_identity(message: &TranscriptMessage) -> (&str, usize) {
    match message {
        TranscriptMessage::System { id, content, .. } => (id.as_str(), content.len()),
        TranscriptMessage::User { id, content, .. } => (id.as_str(), content.len()),
        TranscriptMessage::Assistant { id, content, .. } => (id.as_str(), content.len()),
        TranscriptMessage::Tool { id, summary, .. } => (id.as_str(), summary.len()),
        TranscriptMessage::Compact { id, summary, .. } => (id.as_str(), summary.len()),
        TranscriptMessage::Fold { id, summary, .. } => (id.as_str(), summary.len()),
    }
}

/// Fingerprint over exactly the prefix a prefire pass at this keep_recent
/// would summarize -- the same partition prepare_authoritative computes.
pub fn prefire_fingerprint(transcript: &Transcript, keep_recent: usize) -> String {
    let (summarized, _kept) = partition_recent_turns(transcript.messages().to_vec(), keep_recent);
    fingerprint_prefix(&summarized)
}

/// A background-prepared summary cached against the prefix it was built
/// from. Produced by Compactor::prefire_prepare, consumed by
/// Compactor::prefire_still_valid + Compactor::commit_prefired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefiredSummary {
    pub fingerprint: String,
    pub summary: String,
    pub dropped_count: usize,
}

impl Compactor {
    /// Mechanism 2 (prefire, background half). Run a summarization pass
    /// ahead of the real trigger -- callers invoke this off the hot path at
    /// threshold - prefire_lead_percent -- and cache both its fingerprint
    /// and result for later validation.
    pub async fn prefire_prepare(
        &self,
        transcript: &Transcript,
        summarizer: Arc<dyn FallibleSummarizer>,
    ) -> Result<Option<PrefiredSummary>, CompactError> {
        let fingerprint = prefire_fingerprint(transcript, self.keep_recent);
        let (summarized, _kept) =
            partition_recent_turns(transcript.messages().to_vec(), self.keep_recent);
        if summarized.is_empty() {
            return Ok(None);
        }
        let prior_summary = extract_prior_summary(transcript);
        let summarizer_input = bounded_summarizer_input(&summarized, &summarized);
        let summary = core_safety_pass(
            &summarizer
                .try_summarize(&summarizer_input, prior_summary.as_deref())
                .await?,
        );
        if summary.is_empty() {
            return Err(CompactError::Summarizer(
                "model returned an empty prefire summary".into(),
            ));
        }
        Ok(Some(PrefiredSummary {
            fingerprint,
            summary,
            dropped_count: summarized.len(),
        }))
    }

    /// Mechanism 2 (prefire, validation half). At the real trigger, check
    /// whether prefired was built from the prefix the transcript still
    /// has. true means the cache is reusable with NO blocking model call
    /// on the hot path; false means the prefix mutated (a rewind or edit)
    /// and the caller must refire via compact_authoritative.
    pub fn prefire_still_valid(&self, transcript: &Transcript, prefired: &PrefiredSummary) -> bool {
        prefire_fingerprint(transcript, self.keep_recent) == prefired.fingerprint
    }

    /// Commit an already-validated prefired summary. NO summarizer call is
    /// made here -- this is the whole point of mechanism 2. Callers MUST
    /// have checked prefire_still_valid first; committing an unchecked
    /// stale prefire would apply a summary to the wrong prefix.
    pub fn commit_prefired(
        &self,
        transcript: &mut Transcript,
        prefired: PrefiredSummary,
        session_id: &str,
        workspace_root: &std::path::Path,
    ) -> CompactionResult {
        let (_summarized, kept) =
            partition_recent_turns(transcript.messages().to_vec(), self.keep_recent);
        let compact_message = TranscriptMessage::Compact {
            id: format!("compact-{}", now_iso()),
            created_at: now_iso(),
            summary: prefired.summary.clone(),
            dropped_count: prefired.dropped_count as i32,
            pinned: true,
        };
        let provider = self.memory_block_provider.as_deref();
        let replacement = rebuild_authoritative_messages(
            kept,
            compact_message,
            session_id,
            workspace_root,
            provider,
        );
        transcript.replace(replacement);
        CompactionResult {
            summary: prefired.summary,
            dropped_count: prefired.dropped_count,
        }
    }
}
