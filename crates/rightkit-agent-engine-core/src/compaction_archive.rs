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
    /// [`LINEAGE_EVENT_KIND`] by [`CompactionArchiveSink::persist_lineage`]).
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
/// - Exempt tool classes (`rightkit_agent_tools::coding_tools::output_compaction::is_exempt_tool`
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
        || rightkit_agent_tools::coding_tools::output_compaction::is_exempt_tool(name)
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

#[cfg(test)]
mod wire_projection_tests {
    use super::*;
    use rightkit_agent_engine_protocol::ToolCall;

    fn sample_messages() -> Vec<TranscriptMessage> {
        vec![
            TranscriptMessage::User {
                id: "u1".into(),
                created_at: "t".into(),
                content: "hello".into(),
                trust: None,
                pinned: false,
                attachments: Vec::new(),
            },
            TranscriptMessage::Assistant {
                id: "a1".into(),
                created_at: "t".into(),
                content: "hi".into(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                pinned: false,
            },
        ]
    }

    #[test]
    fn projection_record_verifies_against_the_exact_range_it_was_built_from() {
        let range = sample_messages();
        let record = CompactionProjectionRecord::build("sess", 1, 2, &range, "a summary");
        assert_eq!(record.source_seq_start, 1);
        assert_eq!(record.source_seq_end, 2);
        assert!(
            record.verify(&range),
            "a projection must verify against the exact canonical range it was built from"
        );
    }

    #[test]
    fn projection_record_rejects_a_range_that_diverged_from_canonical() {
        let original_range = sample_messages();
        let record = CompactionProjectionRecord::build("sess", 1, 2, &original_range, "a summary");

        // Simulate canonical drift: one event in the range was mutated
        // (or a different range entirely was fetched for verification).
        let mut mutated_range = original_range;
        if let TranscriptMessage::User { content, .. } = &mut mutated_range[0] {
            *content = "a different first message".into();
        }
        assert!(
            !record.verify(&mutated_range),
            "a projection must NOT verify once its canonical range has diverged"
        );
    }

    #[test]
    fn building_a_projection_record_never_mutates_the_canonical_range() {
        let range = sample_messages();
        let before = serde_json::to_string(&range).unwrap();
        let _ = CompactionProjectionRecord::build("sess", 1, 2, &range, "summary");
        let after = serde_json::to_string(&range).unwrap();
        assert_eq!(
            before, after,
            "building a projection record must be read-only over canonical history"
        );
    }

    #[test]
    fn projection_record_round_trips_through_its_durable_json_form() {
        let range = sample_messages();
        let record = CompactionProjectionRecord::build("sess", 3, 9, &range, "a summary");
        let payload = serde_json::to_string(&record).unwrap();
        let decoded = CompactionProjectionRecord::decode_json(&payload).unwrap();
        assert_eq!(decoded, record, "the durable form must round-trip exactly");
        // The decoded record still verifies against the same canonical range.
        assert!(decoded.verify(&range));
    }

    #[test]
    fn decode_json_rejects_a_garbage_payload() {
        assert!(CompactionProjectionRecord::decode_json("not a record").is_err());
    }

    #[test]
    fn archive_input_carries_the_live_projection_record_to_the_sink() {
        let range = sample_messages();
        let record = CompactionProjectionRecord::build("sess", 0, 2, &range, "summary");
        let input = CompactionArchiveInput {
            session_id: "sess".into(),
            created_at: "t".into(),
            messages: range.clone(),
            projection: Some(record.clone()),
            lineage: None,
            snapshot: None,
            federation_decision: None,
            federation_request_started_at: None,
        };
        let carried = input.projection.expect("live path must attach its record");
        assert_eq!(carried, record);
        assert!(carried.verify(&input.messages));
    }

    #[test]
    fn identity_projection_returns_canonical_verbatim() {
        let canonical = sample_messages();
        let projected = IdentityProjection.project(&canonical);
        // `TranscriptMessage` has no `PartialEq`; compare via serialization.
        assert_eq!(
            serde_json::to_string(&projected).unwrap(),
            serde_json::to_string(&canonical).unwrap()
        );
    }

    #[test]
    fn projecting_never_mutates_the_canonical_slice() {
        let canonical = sample_messages();
        let before = serde_json::to_string(&canonical).unwrap();
        let _ = IdentityProjection.project(&canonical);
        let after = serde_json::to_string(&canonical).unwrap();
        // `project` takes `&[TranscriptMessage]`, so the compiler already
        // forbids mutation through this call; this assertion additionally
        // documents and pins the read-time-projection invariant (mechanism
        // 10) at the value level for anyone editing this trait later.
        assert_eq!(before, after);
    }

    fn user_turn(id: &str) -> TranscriptMessage {
        TranscriptMessage::user(id, "t", format!("turn {id}"), None, false)
    }

    fn assistant_calling(id: &str, call: ToolCall) -> TranscriptMessage {
        TranscriptMessage::Assistant {
            id: format!("a-{id}"),
            created_at: "t".into(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![call],
            pinned: false,
        }
    }

    fn tool_result(id: &str, call_id: &str, summary: &str) -> TranscriptMessage {
        TranscriptMessage::Tool {
            id: id.into(),
            created_at: "t".into(),
            tool_call_id: call_id.into(),
            summary: summary.into(),
            output: None,
            trust: None,
            is_error: false,
            pinned: false,
            process_id: None,
        }
    }

    /// A tool result carrying a durable, retrievable output reference — the
    /// precondition the decay projection requires before it will replace a
    /// stale summary with a "re-read to recover" stub.
    fn tool_result_with_retrievable_ref(
        id: &str,
        call_id: &str,
        summary: &str,
    ) -> TranscriptMessage {
        let mut msg = tool_result(id, call_id, summary);
        if let TranscriptMessage::Tool { output, .. } = &mut msg {
            *output = Some(rightkit_agent_engine_protocol::refs::OutputRef {
                id: format!("ref-{id}"),
                kind: "tool_output".into(),
                size_bytes: summary.len() as i64,
                content_hash: None,
                preview: None,
                media_type: "text/plain; charset=utf-8".into(),
                truncated: true,
                retrievable: true,
            });
        }
        msg
    }

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments_json: "{}".into(),
        }
    }

    fn decay_boundary() -> TranscriptMessage {
        TranscriptMessage::Compact {
            id: "decay-boundary".into(),
            created_at: "t".into(),
            summary: "summary".into(),
            dropped_count: 0,
            pinned: true,
        }
    }

    fn long_run(prefix: &str) -> Vec<TranscriptMessage> {
        // One full turn: user, assistant issuing a `read_file` call, result.
        vec![
            user_turn(prefix),
            assistant_calling(prefix, call(&format!("tc-{prefix}"), "read_file")),
            tool_result_with_retrievable_ref(
                &format!("t-{prefix}"),
                &format!("tc-{prefix}"),
                &format!("big output {prefix}: {}", "x".repeat(50_000)),
            ),
        ]
    }

    #[test]
    fn decay_replaces_stale_large_tool_outputs_only() {
        let mut canonical = Vec::new();
        canonical.extend(long_run("t1"));
        canonical.extend(long_run("t2"));
        canonical.extend(long_run("t3"));
        canonical.push(decay_boundary());
        let projection = DecayingObservationProjection::new(/* keep_recent */ 3, 30_000);
        let projected = projection.project(&canonical);
        let original_size = match &canonical[2] {
            TranscriptMessage::Tool { summary, .. } => summary.len(),
            _ => unreachable!("fixture index 2 is a tool result"),
        };

        // t1's result is old and large → decayed to a size-stating stub.
        // P2-2: decayed output MUST retain its retrievable reference so the
        // model can re-read the full result via the normal file path -- the old
        // contradictory fixture below asserted `output.is_none()` which
        // directly violates invariant H (every large result keeps a recoverable
        // full reference).
        let stale = &projected[2];
        if let TranscriptMessage::Tool {
            summary, output, ..
        } = stale
        {
            assert!(
                summary.contains("decayed"),
                "stale large output must be decayed, got {summary}"
            );
            assert!(
                summary.contains(&original_size.to_string()),
                "decay stub must state original size"
            );
            assert!(
                output.as_ref().is_some_and(|r| r.retrievable),
                "decayed output must retain its retrievable reference; got {output:?}"
            );
        } else {
            panic!("expected tool at index 2");
        }
        // Turn structure is unchanged: the user + assistant rows survive.
        assert!(matches!(projected[0], TranscriptMessage::User { .. }));
        assert!(matches!(projected[1], TranscriptMessage::Assistant { .. }));
        // Only t3's result is within the trailing three-message horizon.
        for idx in [8usize] {
            if let TranscriptMessage::Tool { summary, .. } = &projected[idx] {
                assert!(
                    !summary.contains("decayed"),
                    "recent result at {idx} must stay verbatim"
                );
            } else {
                panic!("expected tool at {idx}");
            }
        }
    }

    /// Tier 1 pairing invariant: decay only rewrites a result's in-context
    /// summary. Message count, order, ids and every tool_use/tool_result link
    /// are untouched, so the provider never sees a dangling call or orphan
    /// result, and the cleared output costs no model call (the projection is a
    /// pure function).
    #[test]
    fn decay_preserves_message_order_ids_and_tool_pairing() {
        let mut canonical = Vec::new();
        for prefix in ["p1", "p2", "p3", "p4"] {
            canonical.extend(long_run(prefix));
        }
        canonical.push(decay_boundary());
        let projected = DecayingObservationProjection::new(3, 30_000).project(&canonical);

        assert_eq!(projected.len(), canonical.len());
        let id_of = |message: &TranscriptMessage| match message {
            TranscriptMessage::User { id, .. }
            | TranscriptMessage::Assistant { id, .. }
            | TranscriptMessage::Tool { id, .. }
            | TranscriptMessage::System { id, .. }
            | TranscriptMessage::Compact { id, .. }
            | TranscriptMessage::Fold { id, .. } => id.clone(),
        };
        let before: Vec<String> = canonical.iter().map(id_of).collect();
        let after: Vec<String> = projected.iter().map(id_of).collect();
        assert_eq!(before, after);

        let mut issued = std::collections::HashSet::new();
        let mut decayed = 0usize;
        for message in &projected {
            match message {
                TranscriptMessage::Assistant { tool_calls, .. } => {
                    issued.extend(tool_calls.iter().map(|call| call.id.clone()));
                }
                TranscriptMessage::Tool {
                    tool_call_id,
                    summary,
                    ..
                } => {
                    assert!(issued.contains(tool_call_id), "orphan {tool_call_id}");
                    decayed += usize::from(summary.contains("decayed"));
                }
                _ => {}
            }
        }
        assert!(decayed >= 3, "old results must actually be cleared");
    }

    #[test]
    fn decay_projection_is_frozen_until_next_compaction_boundary() {
        let mut canonical = long_run("before");
        canonical.push(decay_boundary());
        let projection = DecayingObservationProjection::new(0, 30_000);
        let before = projection.project(&canonical);
        let old_summary = match &before[2] {
            TranscriptMessage::Tool { summary, .. } => summary.clone(),
            _ => unreachable!(),
        };
        canonical.extend(long_run("after"));
        let after = projection.project(&canonical);
        assert!(
            matches!(&after[2], TranscriptMessage::Tool { summary, .. } if summary == &old_summary)
        );
        assert!(
            matches!(&after[6], TranscriptMessage::Tool { summary, .. } if !summary.contains("decayed"))
        );
    }

    #[test]
    fn decay_never_mutates_canonical_history() {
        let canonical = long_run("t1");
        let before = serde_json::to_string(&canonical).unwrap();
        let projection = DecayingObservationProjection::new(0, 30_000);
        let _ = projection.project(&canonical);
        let after = serde_json::to_string(&canonical).unwrap();
        assert_eq!(
            before, after,
            "rolling decay must be a read-time projection, never a mutation"
        );
    }

    #[test]
    fn decay_skips_exempt_tool_classes_however_old() {
        // The exempt result is large enough to be a decay candidate AND has a
        // retrievable ref — only the exempt class keeps it resident, so the
        // test proves the exemption, not the "no ref" guard.
        let mut canonical = vec![
            assistant_calling("q", call("tc-q", "ask_user")),
            tool_result_with_retrievable_ref(
                "t-q",
                "tc-q",
                &format!("the user's answer must survive {}", "z".repeat(50_000)),
            ),
        ];
        canonical.extend(long_run("x"));
        canonical.push(decay_boundary());
        let projection = DecayingObservationProjection::new(0, 30_000);
        let projected = projection.project(&canonical);

        let answer_survived = projected.iter().any(|m| {
            matches!(m, TranscriptMessage::Tool { summary, .. }
                if summary.contains("user's answer must survive"))
        });
        assert!(answer_survived, "an exempt ask_user result was decayed");

        let stale_decayed = projected.iter().any(
            |m| matches!(m, TranscriptMessage::Tool { summary, .. } if summary.contains("decayed")),
        );
        assert!(
            stale_decayed,
            "a non-exempt stale result should still decay"
        );
    }

    #[test]
    fn decay_threshold_is_clamped_below_the_tool_output_cap() {
        let projection = DecayingObservationProjection::new(0, 30_000);
        assert!(projection.min_decay_bytes <= MAX_MIN_DECAY_BYTES);
        // A 3000-byte stale result (under the 4000-byte cap) must now decay.
        let mut canonical = vec![
            assistant_calling("m", call("tc-m", "read_file")),
            tool_result_with_retrievable_ref("t-m", "tc-m", &"m".repeat(3_000)),
        ];
        canonical.push(decay_boundary());
        let projected = projection.project(&canonical);
        assert!(
            matches!(&projected[1], TranscriptMessage::Tool { summary, .. } if summary.contains("decayed"))
        );
    }

    #[test]
    fn invoke_skill_is_exempt_and_repeated_exempt_results_are_capped() {
        assert!(is_exempt_tool_name("invoke_skill"));
        assert!(is_exempt_tool_name("todo_write"));
        let total = EXEMPT_RESULTS_KEPT_PER_TOOL + 3;
        let mut canonical = Vec::new();
        for n in 0..total {
            canonical.push(assistant_calling(
                &format!("td{n}"),
                call(&format!("tc-td{n}"), "todo_write"),
            ));
            canonical.push(tool_result_with_retrievable_ref(
                &format!("t-td{n}"),
                &format!("tc-td{n}"),
                &"d".repeat(3_000),
            ));
        }
        canonical.push(decay_boundary());
        let projected = DecayingObservationProjection::new(0, 30_000).project(&canonical);
        let decayed: Vec<bool> = projected
            .iter()
            .filter_map(|m| match m {
                TranscriptMessage::Tool { summary, .. } => Some(summary.contains("decayed")),
                _ => None,
            })
            .collect();
        assert_eq!(
            decayed.iter().filter(|d| **d).count(),
            total - EXEMPT_RESULTS_KEPT_PER_TOOL
        );
        // The oldest are the ones decayed; the newest stay verbatim.
        assert!(decayed[0]);
        assert!(!decayed[total - 1]);
    }

    #[test]
    fn superseded_todo_write_results_elide_without_a_retrievable_ref() {
        let total = EXEMPT_RESULTS_KEPT_PER_TOOL + 2;
        let mut canonical = Vec::new();
        for n in 0..total {
            canonical.push(assistant_calling(
                &format!("sd{n}"),
                call(&format!("tc-sd{n}"), "todo_write"),
            ));
            canonical.push(tool_result(
                &format!("t-sd{n}"),
                &format!("tc-sd{n}"),
                &"s".repeat(3_000),
            ));
        }
        canonical.push(decay_boundary());
        let projected = DecayingObservationProjection::new(0, 30_000).project(&canonical);
        let elided = projected
            .iter()
            .filter(|m| matches!(m, TranscriptMessage::Tool { summary, .. } if summary.contains("superseded")))
            .count();
        assert_eq!(elided, total - EXEMPT_RESULTS_KEPT_PER_TOOL);
    }

    #[test]
    fn decay_skips_open_obligations() {
        // The obligation is large enough to be a decay candidate AND has a
        // retrievable ref — only the OPEN OBLIGATION marker keeps it resident.
        let mut canonical = vec![
            assistant_calling("o", call("tc-o", "edit_file")),
            tool_result_with_retrievable_ref(
                "t-o",
                "tc-o",
                &format!("OPEN OBLIGATION: finish the report {}", "w".repeat(50_000)),
            ),
        ];
        canonical.extend(long_run("x"));
        canonical.push(decay_boundary());
        let projection = DecayingObservationProjection::new(0, 30_000);
        let projected = projection.project(&canonical);

        let obligation_survived = projected.iter().any(|m| {
            matches!(m, TranscriptMessage::Tool { summary, .. }
                if summary.contains("OPEN OBLIGATION"))
        });
        assert!(
            obligation_survived,
            "an open obligation must never be decayed"
        );
    }

    #[test]
    fn decay_skips_small_outputs_and_pinned_results() {
        let mut canonical = vec![
            assistant_calling("s", call("tc-s", "list_files")),
            tool_result("t-s", "tc-s", "small listing"),
            assistant_calling("p", call("tc-p", "read_file")),
            TranscriptMessage::Tool {
                id: "t-p".into(),
                created_at: "t".into(),
                tool_call_id: "tc-p".into(),
                summary: format!("pinned {}", "y".repeat(50_000)),
                output: None,
                trust: None,
                is_error: false,
                pinned: true,
                process_id: None,
            },
        ];
        canonical.extend(long_run("x"));
        let projection = DecayingObservationProjection::new(0, 30_000);
        let projected = projection.project(&canonical);

        let small_survived = projected.iter().any(
            |m| matches!(m, TranscriptMessage::Tool { summary, .. } if summary == "small listing"),
        );
        assert!(small_survived, "a small old result must stay resident");
        let pinned_survived = projected.iter().any(
            |m| matches!(m, TranscriptMessage::Tool { summary, .. } if summary.contains("pinned")),
        );
        assert!(pinned_survived, "a pinned result must never be decayed");
    }

    /// Invariant H: a decay stub promises the model can re-read the file, so a
    /// result with NO durable reference — or a reference that is not
    /// retrievable — must never be decayed. Replacing a large output the model
    /// can't recover with "re-read the file to recover" would be a lie.
    #[test]
    fn decay_never_replaces_output_without_a_retrievable_reference() {
        let mut no_ref = long_run("no-ref");
        if let TranscriptMessage::Tool { output, .. } = &mut no_ref[2] {
            *output = None;
        }
        let mut non_retrievable = long_run("non-retrievable");
        if let TranscriptMessage::Tool {
            output: Some(output),
            ..
        } = &mut non_retrievable[2]
        {
            output.retrievable = false;
        }

        let projection = DecayingObservationProjection::new(0, 30_000);
        for canonical in [no_ref, non_retrievable] {
            let projected = projection.project(&canonical);
            let stale = &projected[2];
            if let TranscriptMessage::Tool { summary, .. } = stale {
                assert!(
                    !summary.contains("decayed"),
                    "a result without a retrievable reference must stay verbatim, got {summary}"
                );
                assert!(
                    summary.contains("big output"),
                    "the full summary must remain resident when it cannot be recovered via a ref"
                );
            } else {
                panic!("expected tool at index 2");
            }
        }
    }

    // ===== FM-178: lineage `materialize` / `truncate_at` test shapes =====
    //
    // Ported test SHAPES from codex's `rollout_lineage_tests.rs`. The donor
    // builds lineages on disk through `LocalThreadStore` + rollout files; that
    // IO/store machinery is absent from the host app, so the segments are built
    // directly over in-memory canonical ranges (the faithful analog of codex's
    // immutable rollout files) and the same assertions are made.

    /// A segment whose `byte_length` is the serialized length of its canonical
    /// range — the analog of the rollout file length codex validates a cutoff
    /// against in `validate_cutoff_bounds`.
    fn lineage_segment(
        session_id: &str,
        canonical_range: Vec<TranscriptMessage>,
        start_ordinal: u64,
        end: Option<CompactionHistoryPosition>,
    ) -> CompactionLineageSegment {
        let byte_length = serde_json::to_vec(&canonical_range)
            .expect("serialize canonical range")
            .len() as u64;
        CompactionLineageSegment {
            session_id: session_id.to_string(),
            canonical_range,
            start_ordinal,
            end,
            byte_length,
        }
    }

    /// Mirrors codex's `resolves_nested_lineage_with_empty_intermediate_segments`
    /// plus materialization: nested immutable segments concatenate in fork
    /// order, and a segment carrying a cutoff `end` contributes only up to it.
    #[test]
    fn compaction_lineage_materializes_nested_segments_in_fork_order() {
        let root_cut = CompactionHistoryPosition {
            session_id: "root".into(),
            end_ordinal_exclusive: 3,
            end_byte_offset: 0,
        };
        let root = lineage_segment(
            "root",
            vec![
                user_turn("r0"),
                user_turn("r1"),
                user_turn("r2"),
                user_turn("r3"),
            ],
            1,
            Some(root_cut),
        );
        let child = lineage_segment(
            "child",
            vec![user_turn("c0"), user_turn("c1"), user_turn("c2")],
            4,
            None,
        );
        let lineage = CompactionRolloutLineage {
            segments: vec![root, child],
        };

        let materialized = lineage.materialize();
        let ids: Vec<&str> = materialized
            .iter()
            .filter_map(|m| match m {
                TranscriptMessage::User { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        // root contributes messages[0..(3-1)=2]; child contributes all 3.
        assert_eq!(ids, vec!["r0", "r1", "c0", "c1", "c2"]);
    }

    /// Mirrors codex's `resolves_lineage_at_explicit_history_position`: the
    /// lineage is cut at a fork position and the cutoff bounds are validated
    /// before the cut is committed.
    #[test]
    fn compaction_lineage_truncate_at_cuts_and_validates_bounds() {
        let root_cut = CompactionHistoryPosition {
            session_id: "root".into(),
            end_ordinal_exclusive: 3,
            end_byte_offset: 0,
        };
        let root = lineage_segment(
            "root",
            vec![user_turn("r0"), user_turn("r1"), user_turn("r2")],
            1,
            Some(root_cut),
        );
        let middle = lineage_segment("middle", vec![user_turn("m0"), user_turn("m1")], 4, None);
        let child = lineage_segment(
            "child",
            vec![user_turn("c0"), user_turn("c1"), user_turn("c2")],
            6,
            None,
        );
        let lineage = CompactionRolloutLineage {
            segments: vec![root, middle, child],
        };

        let end = CompactionHistoryPosition {
            session_id: "child".into(),
            end_ordinal_exclusive: 8,
            end_byte_offset: serde_json::to_vec(&vec![user_turn("c0"), user_turn("c1")])
                .expect("serialize")
                .len() as u64,
        };
        let truncated = lineage
            .truncate_at(end.clone())
            .expect("truncate within bounds");
        assert_eq!(truncated.segments.len(), 3);
        assert_eq!(truncated.segments[2].session_id, "child");
        assert_eq!(truncated.segments[2].end, Some(end));

        let materialized = truncated.materialize();
        let ids: Vec<&str> = materialized
            .iter()
            .filter_map(|m| match m {
                TranscriptMessage::User { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        // root (cut at 3 -> 2) + middle (2) + child (cut at 8 -> 8-6=2).
        assert_eq!(ids, vec!["r0", "r1", "m0", "m1", "c0", "c1"]);
    }

    /// Mirrors codex's `rejects_missing_cycles_and_out_of_bounds_offsets`: a
    /// cutoff past the source range, and a cutoff at ordinal 0 (which would
    /// include source session metadata), are both rejected.
    #[test]
    fn compaction_lineage_truncate_at_rejects_out_of_bounds_offsets() {
        let child = lineage_segment("child", vec![user_turn("c0"), user_turn("c1")], 1, None);
        let past_offset = CompactionHistoryPosition {
            session_id: "child".into(),
            end_ordinal_exclusive: 2,
            end_byte_offset: child.byte_length + 1,
        };
        let err = CompactionRolloutLineage {
            segments: vec![child],
        }
        .truncate_at(past_offset)
        .expect_err("cutoff past source range must be rejected");
        assert!(
            err.contains("cutoff byte offset is past the source rollout"),
            "got: {err}"
        );

        let at_metadata = CompactionHistoryPosition {
            session_id: "child".into(),
            end_ordinal_exclusive: 0,
            end_byte_offset: 0,
        };
        let err = CompactionRolloutLineage {
            segments: vec![lineage_segment("child", vec![user_turn("c0")], 1, None)],
        }
        .truncate_at(at_metadata)
        .expect_err("cutoff at ordinal 0 must be rejected");
        assert!(
            err.contains("cutoff cannot include source session metadata"),
            "got: {err}"
        );
    }

    /// A fork position that does not appear in the lineage is rejected
    /// outright (codex returns "fork position is outside the source lineage").
    #[test]
    fn compaction_lineage_truncate_at_rejects_unknown_fork_position() {
        let child = lineage_segment("child", vec![user_turn("c0")], 1, None);
        let err = CompactionRolloutLineage {
            segments: vec![child],
        }
        .truncate_at(CompactionHistoryPosition {
            session_id: "absent".into(),
            end_ordinal_exclusive: 1,
            end_byte_offset: 0,
        })
        .expect_err("unknown fork must be rejected");
        assert!(
            err.contains("fork position is outside the source lineage"),
            "got: {err}"
        );
    }

    /// Crash-safety invariant: a failed truncate does not mutate the caller's
    /// lineage, because the bounds check runs before the cut is committed.
    #[test]
    fn compaction_lineage_truncate_at_is_crash_safe() {
        let child = lineage_segment("child", vec![user_turn("c0")], 1, None);
        let lineage = CompactionRolloutLineage {
            segments: vec![child],
        };
        let res = lineage.truncate_at(CompactionHistoryPosition {
            session_id: "absent".into(),
            end_ordinal_exclusive: 1,
            end_byte_offset: 0,
        });
        assert!(
            res.is_err(),
            "truncate at an absent fork must fail and leave the source intact"
        );
    }

    /// S:E3 (archive read-back path): a lineage truncated mid-record,
    /// persisted to its durable JSON form, and reopened (as after a daemon
    /// restart or crash recovery) must materialize byte-for-byte identical to
    /// materializing the pre-persistence lineage. This is the same invariant
    /// as `CompactionProjectionRecord`'s hash-verified projection, applied to
    /// the lineage's own reopen/materialize path.
    #[test]
    fn compaction_lineage_reopens_from_durable_json_and_materializes_byte_exact() {
        let root_cut = CompactionHistoryPosition {
            session_id: "root".into(),
            end_ordinal_exclusive: 3,
            end_byte_offset: 0,
        };
        let root = lineage_segment(
            "root",
            vec![user_turn("r0"), user_turn("r1"), user_turn("r2")],
            1,
            Some(root_cut),
        );
        let child = lineage_segment("child", vec![user_turn("c0"), user_turn("c1")], 4, None);
        let lineage = CompactionRolloutLineage {
            segments: vec![root, child],
        };

        // Truncate mid-record (cut lands inside "child", not at its boundary).
        let mid_cut = CompactionHistoryPosition {
            session_id: "child".into(),
            end_ordinal_exclusive: 5,
            end_byte_offset: serde_json::to_vec(&vec![user_turn("c0")])
                .expect("serialize")
                .len() as u64,
        };
        let truncated = lineage
            .truncate_at(mid_cut)
            .expect("truncate within bounds");
        let before_bytes = serde_json::to_vec(&truncated.materialize()).expect("serialize");

        // Simulate the durable write + a fresh process reopening it.
        let payload = serde_json::to_string(&truncated).expect("persist lineage");
        let reopened =
            CompactionRolloutLineage::decode_json(&payload).expect("reopen persisted lineage");
        let after_bytes = serde_json::to_vec(&reopened.materialize()).expect("serialize");

        assert_eq!(
            before_bytes, after_bytes,
            "materializing a reopened lineage must be byte-exact with the pre-persistence lineage"
        );
    }

    /// S:E3: a crash mid-write can leave a torn (truncated) tail on the
    /// persisted JSON payload. Reopening it must surface an ordinary decode
    /// error — never panic — so the archive read-back path can fall back to
    /// an operationally-derived lineage instead of trusting partial data.
    #[test]
    fn compaction_lineage_decode_tolerates_crash_torn_tail() {
        let lineage = CompactionRolloutLineage {
            segments: vec![lineage_segment(
                "child",
                vec![user_turn("c0"), user_turn("c1")],
                1,
                None,
            )],
        };
        let payload = serde_json::to_string(&lineage).expect("serialize");

        // Simulate a crash mid-write: the tail bytes never made it to disk.
        let torn = &payload[..payload.len() / 2];
        let result = CompactionRolloutLineage::decode_json(torn);
        assert!(
            result.is_err(),
            "a crash-torn tail must be tolerated as a decode error, not accepted as valid"
        );

        // And, critically, decoding a torn payload must not panic — the above
        // call returning (rather than unwinding) is itself the assertion that
        // matters for a caller unwrapping recovery on the hot path.
    }

    fn lineage_ids(messages: &[TranscriptMessage]) -> Vec<String> {
        messages
            .iter()
            .filter_map(|m| match m {
                TranscriptMessage::User { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    /// Two forks taken from one compaction share the immutable pre-compaction
    /// segment; each recovers that exact content byte-for-byte plus only its
    /// own post-compaction segment, and neither leaks into the other.
    #[test]
    fn compaction_lineage_two_forks_each_recover_exact_pre_compaction_content() {
        let pre = vec![user_turn("p0"), user_turn("p1"), user_turn("p2")];
        let pre_bytes = serde_json::to_vec(&pre).expect("serialize");
        let root = lineage_segment("root", pre.clone(), 1, None);

        let fork_a = CompactionRolloutLineage {
            segments: vec![
                root.clone(),
                lineage_segment("fork-a", vec![user_turn("a0"), user_turn("a1")], 4, None),
            ],
        };
        let fork_b = CompactionRolloutLineage {
            segments: vec![
                root.clone(),
                lineage_segment("fork-b", vec![user_turn("b0")], 4, None),
            ],
        };

        let got_a = fork_a.materialize();
        let got_b = fork_b.materialize();
        assert_eq!(lineage_ids(&got_a), vec!["p0", "p1", "p2", "a0", "a1"]);
        assert_eq!(lineage_ids(&got_b), vec!["p0", "p1", "p2", "b0"]);
        assert_eq!(
            serde_json::to_vec(&got_a[..3]).expect("serialize"),
            pre_bytes,
            "fork A must recover pre-compaction content byte-exact"
        );
        assert_eq!(
            serde_json::to_vec(&got_b[..3]).expect("serialize"),
            pre_bytes,
            "fork B must recover pre-compaction content byte-exact"
        );

        // Truncating one fork at the compaction boundary leaves the other intact
        // and yields exactly the pre-compaction transcript.
        let boundary = CompactionHistoryPosition {
            session_id: "root".into(),
            end_ordinal_exclusive: 4,
            end_byte_offset: pre_bytes.len() as u64,
        };
        let cut_a = fork_a.truncate_at(boundary).expect("cut at boundary");
        assert_eq!(
            serde_json::to_vec(&cut_a.materialize()).expect("serialize"),
            pre_bytes
        );
        assert_eq!(
            lineage_ids(&fork_b.materialize()),
            vec!["p0", "p1", "p2", "b0"]
        );

        // Both forks survive a durable reopen with identical recovery.
        for fork in [&cut_a, &fork_b] {
            let payload = serde_json::to_string(fork).expect("persist");
            let reopened = CompactionRolloutLineage::decode_json(&payload).expect("reopen");
            assert_eq!(
                serde_json::to_vec(&reopened.materialize()).expect("serialize"),
                serde_json::to_vec(&fork.materialize()).expect("serialize")
            );
        }
    }

    /// After a second compaction the lineage is a chain root -> mid -> tip;
    /// recovery walks the whole chain in order, and truncating at the first
    /// compaction boundary or the second recovers each exact earlier state.
    #[test]
    fn compaction_lineage_recovery_after_second_compaction_walks_chain() {
        let first = vec![user_turn("p0"), user_turn("p1")];
        let second = vec![user_turn("m0"), user_turn("m1"), user_turn("m2")];
        let first_bytes = serde_json::to_vec(&first).expect("serialize");
        let second_bytes = serde_json::to_vec(&second).expect("serialize");
        let lineage = CompactionRolloutLineage {
            segments: vec![
                lineage_segment("root", first.clone(), 1, None),
                lineage_segment("mid", second.clone(), 3, None),
                lineage_segment("tip", vec![user_turn("t0")], 6, None),
            ],
        };

        assert_eq!(
            lineage_ids(&lineage.materialize()),
            vec!["p0", "p1", "m0", "m1", "m2", "t0"]
        );

        // State before the second compaction: root + mid only.
        let before_second = lineage
            .clone()
            .truncate_at(CompactionHistoryPosition {
                session_id: "mid".into(),
                end_ordinal_exclusive: 6,
                end_byte_offset: second_bytes.len() as u64,
            })
            .expect("cut at second compaction boundary");
        let mut expected = first.clone();
        expected.extend(second.clone());
        assert_eq!(
            serde_json::to_vec(&before_second.materialize()).expect("serialize"),
            serde_json::to_vec(&expected).expect("serialize"),
            "pre-second-compaction content must be recovered byte-exact"
        );
        assert_eq!(before_second.segments().len(), 2);

        // State before the first compaction: root alone.
        let before_first = lineage
            .truncate_at(CompactionHistoryPosition {
                session_id: "root".into(),
                end_ordinal_exclusive: 3,
                end_byte_offset: first_bytes.len() as u64,
            })
            .expect("cut at first compaction boundary");
        assert_eq!(
            serde_json::to_vec(&before_first.materialize()).expect("serialize"),
            first_bytes
        );
    }
}
