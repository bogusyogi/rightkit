//! Fail-closed persistence contract for exact pre-compaction transcripts.

/// Canonical wire authorization shared with Membrane.  the host app carries
/// this protocol value through archive input; it does not define a second
/// federation decision schema.
pub use rightkit_agent_engine_protocol::event::CompactionFederationDecisionV1;
use rightkit_agent_engine_protocol::TranscriptMessage;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Typed failure returned by archive sinks so callers can update compaction
/// retry state without classifying backend messages by string matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionArchiveError {
    pub class: crate::compactor::CompactionFailureClassV1,
    pub message: String,
}

impl CompactionArchiveError {
    pub fn new(
        class: crate::compactor::CompactionFailureClassV1,
        message: impl Into<String>,
    ) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CompactionArchiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CompactionArchiveError {}

impl From<String> for CompactionArchiveError {
    fn from(message: String) -> Self {
        Self::new(
            crate::compactor::CompactionFailureClassV1::StorageUnavailable,
            message,
        )
    }
}

impl From<&str> for CompactionArchiveError {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

/// CR-16: event-ledger kind under which a persisted
/// [`CompactionProjectionRecord`] is stored when a sink writes it durably:
/// an ordinary append-only row (`kind = "compactionProjection"`, payload =
/// the record's JSON) appended strictly AFTER its session's compaction
/// marker, so persisting or scanning projections never touches the
/// pre-compaction range a projection protects. The retention pass in the
/// `host app` bin consumes rows of this kind in preference to deriving
/// projections operationally.
pub const PROJECTION_EVENT_KIND: &str = "compactionProjection";

/// FM-178: event-ledger kind under which a persisted [`CompactionRolloutLineage`]
/// is stored by the live compaction path, so full visible history can be
/// reconstructed independently of the compacted context (ported codex
/// `RolloutLineage` crash-safe fork pointers).
pub const LINEAGE_EVENT_KIND: &str = "compactionLineage";

pub fn transcript_digest(messages: &[TranscriptMessage]) -> String {
    let bytes = serialize_archive_messages(messages).unwrap_or_default();
    rightkit_fs::digest::sha256_bytes(&bytes)
}

#[derive(Debug, Clone)]
pub struct CompactionArchiveInput {
    pub session_id: String,
    pub created_at: String,
    pub messages: Vec<TranscriptMessage>,
    /// CR-16: the immutable projection record built by the live compaction
    /// path over exactly `messages` (and the canonical seq range they
    /// correspond to), handed to the sink in the SAME fail-closed call that
    /// archives the transcript so a sink that supports persistence stores
    /// the projection atomically with the archive it describes. `None`
    /// means the caller could not build one; sinks must treat that as
    /// "no persisted projection", never as permission to skip the archive
    /// itself.
    pub projection: Option<CompactionProjectionRecord>,
    /// FM-178 / CRA-11 (Z24 residual fix): the live session lineage to
    /// persist in the SAME fail-closed transaction that writes the archive
    /// row, epoch advance, and `SessionCompacted` event. Carrying lineage
    /// into this one call (rather than a separate best-effort
    /// `persist_lineage` call after `archive()` returns) closes the crash
    /// window where a durable archive could land with no lineage: a hidden
    /// partial commit. `None` means the caller has no lineage segment to
    /// record this pass; sinks must treat that as "nothing to persist",
    /// never as permission to skip the archive itself.
    pub lineage: Option<CompactionRolloutLineage>,
    /// Failure-relevant decision snapshot captured before any external work.
    pub snapshot: Option<crate::compactor::CompactionSnapshotV1>,
    /// Federation decision is consumed exactly once by the durable sink.
    pub federation_decision: Option<CompactionFederationDecisionV1>,
    /// Monotonic instant captured before federation request.
    pub federation_request_started_at: Option<std::time::Instant>,
}

impl CompactionArchiveInput {
    /// Check the cross-object invariants a sink must enforce before writing an
    /// archive. In particular, a projection for another session must never be
    /// persisted beside this transcript.
    pub fn validate(&self) -> Result<(), String> {
        if self.session_id.trim().is_empty() || self.created_at.trim().is_empty() {
            return Err("compaction archive session id and timestamp must be non-empty".into());
        }
        if let Some(projection) = self.projection.as_ref() {
            projection.validate()?;
            if projection.session_id != self.session_id {
                return Err("compaction projection session does not match archive".into());
            }
        }
        if let Some(snapshot) = self.snapshot.as_ref() {
            if snapshot.transcript_digest.trim().is_empty() {
                return Err("compaction snapshot transcript digest is required".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionArchiveReceipt {
    pub archive_id: String,
    pub sha256: String,
    pub byte_length: usize,
    pub message_count: usize,
    /// Durable epoch returned by the same transaction that committed archive.
    pub compaction_epoch: u64,
    /// Highest event sequence committed atomically by this archive sink.
    /// Zero for sinks without a durable event ledger.
    pub durable_high_water: i64,
}

impl CompactionArchiveReceipt {
    /// Build the receipt for the exact JSON bytes a durable sink is expected
    /// to persist. Keeping this calculation here gives in-process sinks and
    /// recovery callers one byte-addressed contract instead of separate
    /// message-count or re-serialization checks.
    pub fn for_messages(
        archive_id: impl Into<String>,
        messages: &[TranscriptMessage],
    ) -> Result<Self, String> {
        let bytes = serialize_archive_messages(messages)?;
        Ok(Self {
            archive_id: archive_id.into(),
            sha256: rightkit_fs::digest::sha256_bytes(&bytes),
            byte_length: bytes.len(),
            message_count: messages.len(),
            // In-memory receipts have no transactional epoch authority.
            compaction_epoch: 0,
            durable_high_water: 0,
        })
    }

    /// Confirm that a recovered transcript is byte-for-byte the payload named
    /// by this receipt. A receipt must never be treated as proof of recovery
    /// until this check succeeds.
    pub fn verifies_messages(&self, messages: &[TranscriptMessage]) -> bool {
        let Ok(bytes) = serialize_archive_messages(messages) else {
            return false;
        };
        self.verifies_bytes(&bytes, messages.len())
    }

    /// Confirm receipt metadata against the exact bytes read from durable
    /// storage. This is the recovery primitive when re-serializing decoded
    /// messages would lose byte-level fidelity.
    pub fn verifies_bytes(&self, bytes: &[u8], message_count: usize) -> bool {
        self.byte_length == bytes.len()
            && self.message_count == message_count
            && self.sha256 == rightkit_fs::digest::sha256_bytes(bytes)
    }
}

/// Canonical archive encoding shared by receipt producers and recovery
/// callers. The bytes, rather than an in-memory message value, are the durable
/// object addressed by `CompactionArchiveReceipt`.
pub fn serialize_archive_messages(messages: &[TranscriptMessage]) -> Result<Vec<u8>, String> {
    serde_json::to_vec(messages)
        .map_err(|error| format!("failed to encode transcript archive: {error}"))
}

#[async_trait::async_trait]
pub trait CompactionArchiveSink: Send + Sync {
    /// FM-178 / CRA-11 (Z24 residual fix): archive the pre-compaction
    /// transcript AND, when `input.lineage` is `Some`, persist that lineage
    /// durably inside the same fail-closed transaction as the archive row,
    /// projection event, and epoch advance. There is no separate
    /// `persist_lineage` call any more -- a sink that supported it as an
    /// independent best-effort write (discarding its own errors) is exactly
    /// the hidden-partial-commit defect this contract closes: a crash
    /// between two separate commits could leave a durable archive with no
    /// lineage. One call, one commit, all-or-nothing.
    async fn archive(
        &self,
        input: CompactionArchiveInput,
    ) -> Result<CompactionArchiveReceipt, CompactionArchiveError>;
}

/// Membrane admission provider for live compaction. the host app consumes the
/// canonical decision; it never derives one locally.
#[async_trait::async_trait]
pub trait CompactionDecisionProvider: Send + Sync {
    async fn issue(
        &self,
        session_id: &str,
        snapshot: &crate::compactor::CompactionSnapshotV1,
    ) -> Result<(CompactionFederationDecisionV1, Instant), String>;
}

/// Mechanism 10 (never mutate canonical history). The wire request sent to
/// the provider is a READ-TIME PROJECTION over canonical history:
/// implementations borrow `canonical` and return an owned, possibly-elided
/// copy for the outbound request only. They MUST NOT mutate what they are
/// given. The stored transcript -- and, on compaction, the
/// [`CompactionArchiveSink`] above -- remains the permanent, unedited audit
/// trail regardless of what any projection strips for the wire.
pub trait WireProjection: Send + Sync {
    /// Project `canonical` into the payload that goes out over the wire.
    /// `canonical` is borrowed immutably: this is a read, never a write.
    fn project(&self, canonical: &[TranscriptMessage]) -> Vec<TranscriptMessage>;
}

/// CR-16: an immutable record of one compaction pass, referencing the exact
/// source seq range it summarized plus a content hash over that range --
/// never the messages themselves, and never a mutation of canonical
/// history. The append-only event ledger (`rightkit-agent-engine-storage`'s `events`
/// table) remains the sole canonical store; this record is a POINTER into
/// it with an integrity check, produced once at compaction time and stored
/// alongside (or as) the `SessionCompacted` event payload.
///
/// Session replay, audit, export, and recovery use [`Self::verify`] against
/// a freshly re-fetched canonical range to confirm the projection still
/// matches what the ledger actually holds -- catching any bypass that
/// mutated or deleted canonical rows the projection depends on (which is
/// exactly the failure mode CR-16 exists to close: a hidden retention job
/// silently invalidating a projection's evidentiary basis).
///
/// The live compaction path (`SessionEngine::compact_context`) builds this
/// record at archive time over the exact pre-compaction transcript it is
/// about to persist and hands it to the sink via
/// [`CompactionArchiveInput::projection`], so the projection is captured at
/// the true moment of compaction rather than reconstructed after the fact.
/// `host app` bin's `main_runtime_server::run_event_retention_pass` (the
/// hourly retention job) is the real non-test consumer: it prefers a
/// persisted record (event kind [`PROJECTION_EVENT_KIND`]) over an
/// operationally derived one, and only ever deletes a pre-compaction range
/// on a later tick once a fresh fetch verifies unchanged against whichever
/// record covers it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompactionProjectionRecord {
    /// Session this projection belongs to.
    pub session_id: String,
    /// First canonical event seq (inclusive) folded into this projection.
    pub source_seq_start: u32,
    /// Last canonical event seq (inclusive) folded into this projection.
    pub source_seq_end: u32,
    /// Hex-encoded content hash over the canonical range
    /// `[source_seq_start, source_seq_end]`, computed the same way on both
    /// sides of [`Self::verify`] via [`hash_canonical_range`].
    pub content_hash: String,
    /// The projection summary text itself -- what the model-visible surface
    /// sees in place of the summarized range. NOT canonical; only the range
    /// above and its hash are load-bearing for verification.
    pub summary: String,
}

impl CompactionProjectionRecord {
    /// Build a projection record over `canonical_range`, whose events span
    /// exactly `[source_seq_start, source_seq_end]` (inclusive) -- callers
    /// are responsible for fetching that exact range from canonical
    /// storage; this constructor only hashes what it is given.
    pub fn build(
        session_id: impl Into<String>,
        source_seq_start: u32,
        source_seq_end: u32,
        canonical_range: &[TranscriptMessage],
        summary: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            source_seq_start,
            source_seq_end,
            content_hash: hash_canonical_range(canonical_range),
            summary: summary.into(),
        }
    }

    /// Validate identity and source-range invariants before a record is
    /// persisted or used to authorize recovery. `build` remains infallible for
    /// compatibility with existing compaction callers; sinks should reject a
    /// record that does not pass this check.
    pub fn validate(&self) -> Result<(), String> {
        if self.session_id.trim().is_empty() {
            return Err("projection record session id must be non-empty".into());
        }
        if self.source_seq_start > self.source_seq_end {
            return Err("projection record source range is inverted".into());
        }
        if self.content_hash.len() != 64
            || !self
                .content_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("projection record content hash must be a SHA-256 hex digest".into());
        }
        Ok(())
    }

    /// Verify this record against a freshly fetched canonical range. `true`
    /// means the range this projection was built from is byte-for-byte the
    /// range canonical storage still holds for
    /// `[source_seq_start, source_seq_end]` -- the projection can be trusted
    /// to stand in for that range. `false` means canonical history for that
    /// range has changed (or the wrong range was fetched), and the
    /// projection MUST NOT be treated as authoritative until reconciled.
    pub fn verify(&self, canonical_range: &[TranscriptMessage]) -> bool {
        self.validate().is_ok() && hash_canonical_range(canonical_range) == self.content_hash
    }

    /// Decode a record from its durable JSON form (the payload stored under
    /// [`PROJECTION_EVENT_KIND`]). Records are produced with the derives on
    /// this struct (`serde_json::to_string`); this constructor is the typed
    /// read side so consumers never hand-parse the payload.
    pub fn decode_json(payload: &str) -> Result<Self, String> {
        let record: Self = serde_json::from_str(payload)
            .map_err(|error| format!("invalid projection record: {error}"))?;
        record.validate()?;
        Ok(record)
    }
}

// ===== FM-178: crash-safe compaction lineage (ported from codex `RolloutLineage`) =====
//
// Ported EXACTLY from
// `codex-rs/thread-store/src/local/rollout_lineage.rs`:
//   - `RolloutLineageSegment` -> `CompactionLineageSegment`
//   - `RolloutLineage`        -> `CompactionRolloutLineage`
//   - `truncate_at` / `segment_index_for_ordinal` / `validate_cutoff_bounds`
//     / `malformed_lineage` lifted verbatim, adapting only the donor's
//     thread-store types (absent from the host app) to the host app's own
//     `CompactionProjectionRecord` + `TranscriptMessage`:
//       * `rollout_id: ThreadId`     -> `session_id: String`
//       * `rollout_path: PathBuf`    -> `canonical_range: Vec<TranscriptMessage>`
//         (codex's immutable rollout file is replaced by the immutable
//         projection content; cutoff bounds are precomputed into
//         `byte_length` instead of read from the file at truncate time, so
//         `truncate_at` is sync rather than async — no filesystem IO in
//         the host app's model).
//       * `HistoryPosition`          -> `CompactionHistoryPosition`
//     The algorithm — ordered immutable segments, a crash-safe `truncate_at`
//     that cuts the segment list at a fork position and validates the cutoff
//     bounds before committing the cut — is preserved byte-for-byte.
//
// the host app's existing hash-canonical-range verification
// (`CompactionProjectionRecord::build`/`verify`) is left untouched; this
// lineage is an ADDITIONAL, orthogonal crash-safety layer over the same
// projection records.

/// One immutable canonical range contributing to a paginated session's
/// compacted history.
///
/// Mirrors codex's `RolloutLineageSegment`. `canonical_range` is the immutable
/// content a segment contributes (codex stored an immutable rollout file
/// path); `byte_length` is its serialized length, the analog of the rollout
/// file length that `validate_cutoff_bounds` checks the cutoff against.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactionLineageSegment {
    pub session_id: String,
    pub canonical_range: Vec<TranscriptMessage>,
    pub start_ordinal: u64,
    pub end: Option<CompactionHistoryPosition>,
    /// Serialized length of `canonical_range`, used by
    /// `validate_cutoff_bounds` to reject cutoffs past the source range
    /// (codex validates against rollout file length; we validate against the
    /// canonical range's serialized length).
    pub byte_length: u64,
}

/// A fork position within a paginated history lineage.
///
/// Mirrors codex's `HistoryPosition`: `end_ordinal_exclusive` is the exclusive
/// ordinal the lineage is cut at, `end_byte_offset` the exclusive byte offset
/// (within the forked segment's `canonical_range`) the cut lands on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionHistoryPosition {
    pub session_id: String,
    pub end_ordinal_exclusive: u64,
    pub end_byte_offset: u64,
}

/// Ordered immutable canonical ranges contributing to one forked compacted
/// history.
///
/// This is the only the host app abstraction that follows compaction fork
/// pointers. Readers consume its bounded segments without resolving or
/// mutating fork pointers themselves.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactionRolloutLineage {
    pub segments: Vec<CompactionLineageSegment>,
}

impl Default for CompactionRolloutLineage {
    fn default() -> Self {
        Self {
            segments: Vec::new(),
        }
    }
}

impl CompactionRolloutLineage {
    pub fn segments(&self) -> &[CompactionLineageSegment] {
        self.segments.as_slice()
    }

    /// Decode a lineage from its durable JSON form (the payload stored under
    /// [`LINEAGE_EVENT_KIND`] by `CompactionArchiveSink::persist_lineage`).
    ///
    /// S:E3: this is the archive read-back path a reopen after a crash uses to
    /// reconstruct the lineage before calling [`Self::materialize`]. A
    /// crash-torn write (the process died mid-write, leaving a truncated tail)
    /// must be tolerated as an ordinary decode error — never a panic — so a
    /// caller reopening after a crash can fall back to deriving the lineage
    /// operationally instead of trusting a partially-written record.
    pub fn decode_json(payload: &str) -> Result<Self, String> {
        serde_json::from_str(payload)
            .map_err(|error| format!("invalid compaction lineage (possibly crash-torn): {error}"))
    }

    pub fn segment_index_for_ordinal(&self, ordinal: u64) -> Option<usize> {
        self.segments.iter().position(|segment| {
            ordinal >= segment.start_ordinal()
                && segment
                    .end_ordinal()
                    .is_none_or(|end_ordinal| ordinal < end_ordinal)
        })
    }

    /// Crash-safe truncation at a fork position.
    ///
    /// Lifted from codex's `RolloutLineage::truncate_at`. The donor reads the
    /// forked segment's rollout file length inside `validate_cutoff_bounds`;
    /// here `byte_length` is precomputed on the segment, so no IO occurs and
    /// the method is sync. The cut is only committed after the bounds check
    /// succeeds, so a failed truncate leaves the caller's lineage intact.
    pub fn truncate_at(mut self, end: CompactionHistoryPosition) -> Result<Self, String> {
        let segment_index = self
            .segments
            .iter()
            .position(|segment| segment.session_id == end.session_id)
            .ok_or_else(|| "fork position is outside the source lineage".to_string())?;
        let segment = self
            .segments
            .get(segment_index)
            .ok_or_else(|| "rollout lineage has no segments".to_string())?;
        validate_cutoff_bounds(
            &segment.session_id,
            segment.start_ordinal,
            segment.canonical_range.len(),
            segment.byte_length,
            &end,
        )?;
        self.segments.truncate(segment_index + 1);
        let segment = self
            .segments
            .last_mut()
            .ok_or_else(|| "rollout lineage has no segments".to_string())?;
        segment.end = Some(end);
        Ok(self)
    }

    /// Materialize the lineage into the ordered canonical transcript it
    /// covers.
    ///
    /// Mirrors codex's reference materialization: each immutable segment's
    /// content is concatenated in order. A segment carrying a `end` cutoff
    /// contributes only the messages up to that cutoff, honoring the
    /// crash-safe truncation a prior `truncate_at` applied.
    pub fn materialize(&self) -> Vec<TranscriptMessage> {
        let mut out = Vec::new();
        for segment in &self.segments {
            match &segment.end {
                Some(end) if end.end_ordinal_exclusive > segment.start_ordinal => {
                    let count = end
                        .end_ordinal_exclusive
                        .saturating_sub(segment.start_ordinal)
                        .min(segment.canonical_range.len() as u64)
                        as usize;
                    out.extend(segment.canonical_range.iter().take(count).cloned());
                }
                Some(_) => {
                    // Cutoff at or before the segment's start: contributes nothing.
                }
                None => out.extend(segment.canonical_range.iter().cloned()),
            }
        }
        out
    }
}

impl CompactionLineageSegment {
    pub fn session_id(&self) -> &str {
        self.session_id.as_str()
    }

    pub fn start_ordinal(&self) -> u64 {
        self.start_ordinal
    }

    pub fn end_ordinal(&self) -> Option<u64> {
        self.end.as_ref().map(|end| end.end_ordinal_exclusive)
    }
}

fn validate_cutoff_bounds(
    session_id: &str,
    start_ordinal: u64,
    message_count: usize,
    byte_length: u64,
    end: &CompactionHistoryPosition,
) -> Result<(), String> {
    if end.end_ordinal_exclusive == 0 {
        return Err(malformed_lineage(
            session_id,
            "cutoff cannot include source session metadata",
        ));
    }
    if end.end_ordinal_exclusive < start_ordinal {
        return Err(malformed_lineage(
            session_id,
            "cutoff ordinal is before the source segment",
        ));
    }
    let end_ordinal = start_ordinal.saturating_add(message_count as u64);
    if end.end_ordinal_exclusive > end_ordinal {
        return Err(malformed_lineage(
            session_id,
            "cutoff ordinal is past the source rollout",
        ));
    }
    if end.end_byte_offset > byte_length {
        return Err(malformed_lineage(
            session_id,
            "cutoff byte offset is past the source rollout",
        ));
    }
    Ok(())
}

fn malformed_lineage(session_id: &str, detail: &str) -> String {
    format!("invalid paginated history lineage for {session_id}: {detail}")
}

/// Deterministic SHA-256 content hash over an ordered message range. Shared
/// by [`CompactionProjectionRecord::build`] and [`CompactionProjectionRecord::verify`]
/// so both sides of a verification compute the hash identically. The hash is
/// over the canonical JSON encoding, making an archive/projection mismatch
/// observable even when message metadata changes without changing its count.
pub fn hash_canonical_range(range: &[TranscriptMessage]) -> String {
    // TranscriptMessage contains only serializable, bounded wire fields. Keep
    // a deterministic fallback for the infallible-at-runtime serializer API;
    // callers that need an error-bearing encoding use
    // `serialize_archive_messages` directly.
    serde_json::to_vec(range)
        .map(|bytes| rightkit_fs::digest::sha256_bytes(&bytes))
        .unwrap_or_else(|_| rightkit_fs::digest::sha256_bytes(&[]))
}

/// Identity projection: the wire payload equals canonical history verbatim.
/// The default when no elision strategy (masking, tag-based retention,
/// token-budget windowing) is configured for the wire path.
pub struct IdentityProjection;

impl WireProjection for IdentityProjection {
    fn project(&self, canonical: &[TranscriptMessage]) -> Vec<TranscriptMessage> {
        canonical.to_vec()
    }
}

/// Rolling observation decay (P2-2 / v4.2 §10A T5).
///
/// Complements P1-6 masking: masking handles *large* results, this handles
/// *old* ones. A read-time projection that clears stale large tool outputs
/// from the model request independently of full-conversation compaction, so
/// the conversation's turn structure survives while bulk output drops out.
///
/// Modelled on claude-code's `microCompact.ts:39-47`: clear stale per-result
/// outputs (`read`/`bash`/`grep`/`glob`/`websearch`/`webfetch`/`edit`/`write`)
/// without touching the surrounding conversation. Unlike the compaction-time
/// [`crate::compaction_tuning::ObservationMasker`] (which *mutates* the
/// transcript), this is a [`WireProjection`]: it borrows canonical history and
/// returns an owned, elided copy for the outbound request only — canonical
/// rows and turn structure are never rewritten (invariants AB, Y).
///
/// Rules enforced here (the same class of logic as `ObservationMasker`):
/// - Only `Tool` result messages older than the recent horizon are candidates.
/// - Pinned results are never decayed (they are load-bearing by construction).
/// - Exempt tool classes (`rightkit_agent_coding_tools::output_compaction::is_exempt_tool`
///   — `ask_user`, `exit_plan_mode`, `skill_tool`, `plugin_tool`,
///   `todo_write`) are never decayed, whatever their age.
/// - Open obligations (summaries carrying the literal `OPEN OBLIGATION`,
///   the tag convention in `compactor_authoritative.rs`) are never decayed.
/// - Small results stay resident: only outputs above `min_decay_bytes` are
///   candidates, so cheap-but-useful old output is not removed gratuitously.
/// - A decayed result keeps its durable [`rightkit_agent_engine_protocol::refs::OutputRef`]
///   (`output`) so it stays recoverable through the normal file-read path;
///   only the in-context `summary` is replaced, and the replacement states
///   the original size (invariant I).
#[derive(Debug, Clone)]
pub struct DecayingObservationProjection {
    /// Number of trailing messages treated as recent and exempt from decay.
    /// Mirrors `ObservationMasker::keep_recent` so both layers use the same
    /// notion of "recent".
    pub keep_recent: usize,
    /// Byte threshold: a stale tool summary shorter than this stays resident.
    pub min_decay_bytes: usize,
}

/// Upper bound for the decay threshold (audit C5). Capped results are <= ~4000
/// bytes; the threshold must sit below that or read-time decay never fires.
pub const MAX_MIN_DECAY_BYTES: usize = 2_000;

/// Newest results kept verbatim per exempt tool (e.g. `todo_write`); older
/// repeats become decay candidates when they carry a retrievable reference.
pub const EXEMPT_RESULTS_KEPT_PER_TOOL: usize = 4;

/// Exempt-class check that also covers the real skill tool name. The tools
/// crate lists `skill_tool` but the registered tool is `invoke_skill` (audit
/// C5), so both are treated as exempt here.
pub fn is_exempt_tool_name(name: &str) -> bool {
    name == "invoke_skill"
        || rightkit_agent_coding_tools::output_compaction::is_exempt_tool(name)
}

impl DecayingObservationProjection {
    pub fn new(keep_recent: usize, min_decay_bytes: usize) -> Self {
        Self {
            keep_recent,
            // Audit C5: capped tool results are at most ~4000 bytes, so a
            // threshold above that makes decay unreachable. Clamp so a caller
            // passing a legacy 30_000 still gets a live threshold.
            min_decay_bytes: min_decay_bytes.min(MAX_MIN_DECAY_BYTES),
        }
    }

    /// Decay stub for a stale large tool output. States the original size so
    /// the model knows exactly how much it is not seeing (invariant I) and
    /// names the recovery path.
    fn decay_stub(original_bytes: usize) -> String {
        format!("[tool output decayed — {original_bytes} bytes; re-read the file to recover]")
    }

    fn is_exempt_tool(name: &str) -> bool {
        is_exempt_tool_name(name)
    }

    fn is_obligation(summary: &str) -> bool {
        summary.to_ascii_uppercase().contains("OPEN OBLIGATION")
    }
}

impl WireProjection for DecayingObservationProjection {
    fn project(&self, canonical: &[TranscriptMessage]) -> Vec<TranscriptMessage> {
        // Freeze the projection between explicit compact boundaries. Only
        // history preceding the latest Compact marker is eligible; appending
        // a turn cannot make another already-sent result change on the wire.
        let boundary = canonical
            .iter()
            .rposition(|message| matches!(message, TranscriptMessage::Compact { .. }));
        let Some(boundary) = boundary else {
            return canonical.to_vec();
        };
        let keep_from = boundary.saturating_sub(self.keep_recent);

        // Resolve each `tool_call_id` to its tool name by scanning the
        // Assistant messages that carried the calls, so the exempt-class
        // check runs against the real tool that produced the result (the
        // exact pairing `engine_tool_dispatch.rs` uses to externalize).
        let mut tool_names: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::new();
        for msg in canonical {
            if let TranscriptMessage::Assistant { tool_calls, .. } = msg {
                for call in tool_calls {
                    tool_names.insert(call.id.as_str(), call.name.as_str());
                }
            }
        }

        // Rank exempt results per tool name from the newest (0) backwards so
        // repeated ones (todo_write) cannot accumulate without bound.
        let mut exempt_rank: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();
        {
            let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
            for (i, msg) in canonical.iter().enumerate().rev() {
                if let TranscriptMessage::Tool { tool_call_id, .. } = msg {
                    let name = tool_names.get(tool_call_id.as_str()).copied().unwrap_or("");
                    if Self::is_exempt_tool(name) {
                        let n = seen.entry(name).or_insert(0);
                        exempt_rank.insert(i, *n);
                        *n += 1;
                    }
                }
            }
        }

        canonical
            .iter()
            .enumerate()
            .map(|(i, msg)| {
                let TranscriptMessage::Tool {
                    id,
                    created_at,
                    tool_call_id,
                    summary,
                    output,
                    trust,
                    is_error,
                    pinned,
                    process_id,
                } = msg
                else {
                    return msg.clone();
                };
                // Recent horizon, pinned results, and small results always stay.
                if i >= keep_from || i >= boundary || *pinned || summary.len() < self.min_decay_bytes {
                    return msg.clone();
                }
                // Exempt classes keep their newest results verbatim; only
                // older repeats past the per-tool cap fall through to decay.
                // Open obligations are never decayed.
                let exempt_within_cap = exempt_rank
                    .get(&i)
                    .is_some_and(|rank| *rank < EXEMPT_RESULTS_KEPT_PER_TOOL);
                if exempt_within_cap || Self::is_obligation(summary) {
                    return msg.clone();
                }
                // Only decay when a durable, retrievable output reference exists: the decay
                // stub tells the model to re-read the file, so promising that
                // with no reference — or a reference that cannot be resolved
                // through the authenticated retrieval surface — would be a lie
                // (invariant H — every large result keeps a recoverable full
                // reference). An old, large result without a retrievable ref
                // stays verbatim instead.
                let Some(output_ref) = output.as_ref().filter(|r| r.retrievable) else {
                    // A `todo_write` result past the per-tool cap is wholly
                    // superseded by newer snapshots, so it can be elided even
                    // without a retrievable reference (inline verbatim exempt
                    // results never get one).
                    let name = tool_names.get(tool_call_id.as_str()).copied().unwrap_or("");
                    if name == "todo_write" && exempt_rank.contains_key(&i) {
                        return TranscriptMessage::Tool {
                            id: id.clone(),
                            created_at: created_at.clone(),
                            tool_call_id: tool_call_id.clone(),
                            summary: format!(
                                "[older todo_write result elided — {} bytes; superseded by newer todo_write results]",
                                summary.len()
                            ),
                            output: output.clone(),
                            trust: trust.clone(),
                            is_error: *is_error,
                            pinned: *pinned,
                            process_id: process_id.clone(),
                        };
                    }
                    return msg.clone();
                };
                // Decay: replace only the in-context summary; keep the durable
                // output reference so the model can recover the full result.
                TranscriptMessage::Tool {
                    id: id.clone(),
                    created_at: created_at.clone(),
                    tool_call_id: tool_call_id.clone(),
                    summary: Self::decay_stub(summary.len()),
                    output: Some(output_ref.clone()),
                    trust: trust.clone(),
                    is_error: *is_error,
                    pinned: *pinned,
                    process_id: process_id.clone(),
                }
            })
            .collect()
    }
}
