// Fail-closed compaction used by the live session engine.
//
// CR-18: deterministic reconstruction IS the live primary strategy, not a
// legacy compatibility path. `prepare_authoritative_with_trigger` -- the
// entry point `SessionEngine::compact_context` calls -- runs the non-model
// pruner first and folds the dropped window with the deterministic
// statistical summarizer whenever pruning alone brings the fold-away set
// under the escalation target, which is derived by the caller from the SAME
// logged-envelope meter that triggered compaction. The fallible model
// summarizer is pure escalation: it runs only when pruning could not relieve
// the pressure, or when a tagged open obligation needs real prose. The
// legacy infallible `Compactor::compact` remains available for offline
// callers and deterministic compatibility tests; the live session engine
// goes through this path instead. A model summary is authoritative only
// after it completes successfully, and the transcript is not mutated on any
// summary failure.

/// Successful authoritative compaction result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionResult {
    pub summary: String,
    pub dropped_count: usize,
}

#[derive(Debug)]
pub struct PreparedAuthoritativeCompaction {
    pub summary: String,
    pub dropped_count: usize,
    /// Mechanism 7 (monotonic retry): the model-visible surface generation
    /// observed at the very start of the `prepare_authoritative` call that
    /// produced this result. Callers that want to retry pass this back into
    /// `Compactor::retry_authoritative` so a retry is refused unless the
    /// transcript actually changed since this snapshot.
    pub surface_generation: SurfaceGeneration,
    replacement: Vec<TranscriptMessage>,
    /// Exact original messages removed by preparation; count is not a prefix.
    pub removed_messages: Vec<TranscriptMessage>,
    /// Exact `keep_recent` (complete user turns) this pass used to choose its
    /// retained tail: `self.keep_recent` normally, 0 under pressure.
    /// Persisted with the settlement event so restart replay reuses it.
    pub retained_turns: usize,
}

/// Mechanism 5 (tag-based retention) applied at the authoritative hot path:
/// derive the always-keep id set from message content rather than requiring
/// callers to pre-tag ids. Protects the task spec (first user turn in the
/// to-be-summarized prefix), the latest user intent (most recent user turn
/// in that prefix), and any message carrying an explicit open-obligation
/// marker -- mirroring the `TagAwarePruner` doc contract in
/// `compactor_cheap_prune.rs`.
fn collect_always_keep_ids(messages: &[TranscriptMessage]) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    let mut first_user: Option<&str> = None;
    let mut last_user: Option<&str> = None;
    for message in messages {
        match message {
            TranscriptMessage::User { id, .. } => {
                if first_user.is_none() {
                    first_user = Some(id.as_str());
                }
                last_user = Some(id.as_str());
            }
            TranscriptMessage::Tool {
                id,
                summary,
                pinned,
                ..
            } if *pinned || summary.to_ascii_uppercase().contains("OPEN OBLIGATION") => {
                ids.insert(id.clone());
            }
            _ => {}
        }
    }
    if let Some(id) = first_user {
        ids.insert(id.to_string());
    }
    if let Some(id) = last_user {
        ids.insert(id.to_string());
    }
    ids
}

/// CR-19: one structured, out-of-prose obligation record. Extracted from a
/// tagged `TranscriptMessage::Tool` before any pruning or summarization
/// touches the transcript, so it exists independently of whatever prose a
/// summarizer later produces -- the summary is checked against this
/// structured list, not the other way around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObligationRecord {
    pub id: String,
    /// The obligation text with the `OPEN OBLIGATION:` marker stripped, used
    /// both as a substring probe against a produced summary and as
    /// independently-injectable content a caller can splice into a
    /// projected wire request even when a summary drops it.
    pub content: String,
}

/// Mechanism 5 (tag-based retention) + CR-19 (structured obligations):
/// collect every explicitly tagged open-obligation message as STRUCTURED
/// STATE, separate from the prose the summarizer will produce. Mirrors
/// `collect_always_keep_ids`'s obligation detection (same marker, same
/// message class) but returns the content too, not just the id -- the id
/// alone cannot be checked against summary prose.
pub fn collect_open_obligations(messages: &[TranscriptMessage]) -> Vec<ObligationRecord> {
    let mut obligations = Vec::new();
    for message in messages {
        if let TranscriptMessage::Tool { id, summary, .. } = message {
            if let Some(marker_pos) = summary.to_ascii_uppercase().find("OPEN OBLIGATION") {
                let content = summary[marker_pos + "OPEN OBLIGATION".len()..]
                    .trim_start_matches(':')
                    .trim()
                    .to_string();
                obligations.push(ObligationRecord {
                    id: id.clone(),
                    content,
                });
            }
        }
    }
    obligations
}

/// CR-19: `true` if every structured obligation is still recoverable from
/// `summary` -- either its id or its content text appears verbatim.
/// A summarizer is prose, not a structured store, so an exact-substring
/// check is the honest bar: it catches the failure mode this mechanism
/// exists for (the obligation silently vanished from the summary) without
/// pretending to validate semantic equivalence the summarizer never
/// promised.
fn summary_retains_all_obligations(summary: &str, obligations: &[ObligationRecord]) -> bool {
    obligations
        .iter()
        .all(|obligation| summary.contains(&obligation.id) || summary.contains(&obligation.content))
}

/// Mechanism 8 (lenient parsing) applied at the authoritative hot path: a
/// summarizer (real or prefired) may return either plain prose or a JSON
/// object shaped like `LenientCompactionSummary`. When it is JSON, parse it
/// leniently so one malformed known field does not discard an otherwise
/// good summary; anything that is not a JSON object is treated as already
/// being the summary prose, unchanged.
fn resolve_summary_text(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) if value.is_object() => match parse_lenient_summary(&value) {
            Ok(parsed) => parsed.text,
            Err(_) => raw.to_string(),
        },
        _ => raw.to_string(),
    }
}

/// Canonicalize the replacement history after every authoritative rebuild.
///
/// Codex's compact-history path reinserts canonical initial context before the
/// final retained user message or summary.  Our transcript also has a fresh
/// durable-memory projection plus a continuation marker, so preserve that
/// donor ordering while keeping the stronger local rule that fresh memory is
/// visible before the compact boundary: pinned initial/reference context,
/// continuation, fresh memory, compact summary, then the retained live tail.
/// This is deliberately applied after the shared rebuild helper because a
/// replayed or migrated transcript can carry those pinned items in a legacy
/// order even when the helper correctly replaces stale memory.
fn canonicalize_authoritative_rebuild(
    rebuilt: Vec<TranscriptMessage>,
    session_id: &str,
) -> Vec<TranscriptMessage> {
    let memory_id = format!("{MEMORY_BLOCK_ID_PREFIX}:{session_id}");
    let mut initial_or_reference = Vec::new();
    let mut continuations = Vec::new();
    let mut fresh_memory = Vec::new();
    let mut summaries = Vec::new();
    let mut retained_tail = Vec::new();

    for message in rebuilt {
        let message = match message {
            TranscriptMessage::Tool {
                id,
                tool_call_id,
                summary,
                created_at,
                pinned,
                ..
            } if pinned || summary.to_ascii_uppercase().contains("OPEN OBLIGATION") => {
                TranscriptMessage::System {
                    id: format!("preserved-obligation:{id}"),
                    created_at,
                    content: format!("Preserved tool obligation ({tool_call_id}): {summary}"),
                    pinned: true,
                }
            }
            other => other,
        };
        match &message {
            TranscriptMessage::System { id, .. } if id == POST_COMPACTION_CONTINUATION_ID => {
                continuations.push(message)
            }
            TranscriptMessage::System { id, .. } if id == &memory_id => fresh_memory.push(message),
            TranscriptMessage::Compact { .. } => summaries.push(message),
            TranscriptMessage::System { id, .. }
                if is_pinned(&message) && !id.starts_with("preserved-obligation:") =>
            {
                initial_or_reference.push(message)
            }
            TranscriptMessage::User { .. } if is_pinned(&message) => {
                initial_or_reference.push(message)
            }
            _ => retained_tail.push(message),
        }
    }

    let mut ordered = Vec::with_capacity(
        initial_or_reference.len()
            + continuations.len()
            + fresh_memory.len()
            + summaries.len()
            + retained_tail.len(),
    );
    ordered.extend(initial_or_reference);
    ordered.extend(continuations);
    ordered.extend(fresh_memory);
    ordered.extend(summaries);
    ordered.extend(retained_tail);
    ordered
}

const MAX_RECENT_TOOL_SUMMARY_BYTES: usize = 6_000;
const MAX_TOOL_EVIDENCE_BYTES: usize = 12_000;
const MAX_TOOL_EVIDENCE_PER_RESULT_BYTES: usize = 1_200;

/// Keep the configured number of complete user turns, including all messages
/// after the Nth most recent user message. Non-conversational prefixes remain
/// outside this suffix and are handled by the ordinary pinned rules.
fn recent_turn_message_count(messages: &[TranscriptMessage], turns: usize) -> usize {
    if turns == 0 {
        return 0;
    }
    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter_map(|(i, message)| matches!(message, TranscriptMessage::User { .. }).then_some(i))
        .collect();
    let Some(start_turn) = user_indices.len().checked_sub(turns) else {
        return messages.len();
    };
    messages.len().saturating_sub(user_indices[start_turn])
}

pub(crate) fn partition_recent_turns(
    messages: Vec<TranscriptMessage>,
    turns: usize,
) -> (Vec<TranscriptMessage>, Vec<TranscriptMessage>) {
    let recent_messages = recent_turn_message_count(&messages, turns);
    partition_authoritative_messages(messages, recent_messages)
}

pub(crate) fn bounded_summarizer_input(
    messages: &[TranscriptMessage],
    originals: &[TranscriptMessage],
) -> Vec<TranscriptMessage> {
    let mut bounded = messages.to_vec();
    for message in &mut bounded {
        if let TranscriptMessage::Tool { summary, .. } = message {
            if summary.len() > MAX_TOOL_EVIDENCE_PER_RESULT_BYTES {
                *summary = truncate_utf8(summary, MAX_TOOL_EVIDENCE_PER_RESULT_BYTES);
            }
        }
    }
    let excerpts = bounded_tool_evidence(originals);
    if !excerpts.is_empty() {
        bounded.push(TranscriptMessage::System {
            id: "compaction-tool-evidence".into(),
            created_at: now_iso(),
            content: format!("Bounded evidence from original folded-away tool results; retain actionable findings, errors, verification results, and retrieval references:\n{excerpts}"),
            pinned: false,
        });
    }
    bounded
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    let end = text
        .char_indices()
        .take_while(|(index, ch)| *index + ch.len_utf8() <= max_bytes)
        .last()
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    text[..end].to_string()
}

fn bounded_tool_evidence(messages: &[TranscriptMessage]) -> String {
    let mut output = String::new();
    for message in messages {
        let TranscriptMessage::Tool {
            tool_call_id,
            summary,
            output: reference,
            is_error,
            ..
        } = message
        else {
            continue;
        };
        if output.len() >= MAX_TOOL_EVIDENCE_BYTES {
            break;
        }
        let excerpt = truncate_utf8(summary, MAX_TOOL_EVIDENCE_PER_RESULT_BYTES);
        let line = format!(
            "{}{}: {}{}\n",
            if *is_error { "ERROR " } else { "" },
            tool_call_id,
            excerpt,
            reference
                .as_ref()
                .filter(|r| r.retrievable)
                .map(|r| format!(" [retrieval ref: {}]", r.id))
                .unwrap_or_default()
        );
        let remaining = MAX_TOOL_EVIDENCE_BYTES.saturating_sub(output.len());
        output.push_str(&truncate_utf8(&line, remaining));
    }
    output
}

fn stub_oversized_recent_tool_results(
    mut messages: Vec<TranscriptMessage>,
    original: &[TranscriptMessage],
) -> (Vec<TranscriptMessage>, Vec<TranscriptMessage>) {
    let mut stubbed = Vec::new();
    for message in &mut messages {
        if let TranscriptMessage::Tool {
            id,
            summary,
            pinned,
            ..
        } = message
        {
            if !*pinned
                && !summary.to_ascii_uppercase().contains("OPEN OBLIGATION")
                && summary.len() > MAX_RECENT_TOOL_SUMMARY_BYTES
            {
                if let Some(full) = original
                    .iter()
                    .find(|candidate| message_identity(candidate).0 == id)
                {
                    stubbed.push(full.clone());
                }
                let excerpt = truncate_utf8(summary, MAX_RECENT_TOOL_SUMMARY_BYTES);
                *summary = format!(
                    "[recent tool output bounded; first {} bytes shown]\n{excerpt}",
                    MAX_RECENT_TOOL_SUMMARY_BYTES
                );
            }
        }
    }
    (messages, stubbed)
}

impl PreparedAuthoritativeCompaction {
    /// The exact messages that would replace the transcript on commit, so a
    /// caller can measure gain before mutating anything.
    pub fn replacement_messages(&self) -> &[TranscriptMessage] {
        &self.replacement
    }
}

impl Compactor {
    /// Compact with a fallible model summarizer and no lossy fallback.
    ///
    /// Callers decide whether the operation is due (automatic threshold) or
    /// forced (manual request). This operation itself is the shared mutation
    /// path for both cases.
    pub async fn compact_authoritative(
        &self,
        transcript: &mut Transcript,
        summarizer: Arc<dyn FallibleSummarizer>,
        session_id: &str,
        workspace_root: &std::path::Path,
    ) -> Result<Option<CompactionResult>, CompactError> {
        let prepared = self
            .prepare_authoritative(transcript, summarizer, session_id, workspace_root)
            .await?;
        if let Some(prepared) = prepared {
            let result = CompactionResult {
                summary: prepared.summary.clone(),
                dropped_count: prepared.dropped_count,
            };
            self.commit_authoritative(transcript, prepared);
            Ok(Some(result))
        } else {
            Ok(None)
        }
    }

    pub async fn prepare_authoritative(
        &self,
        transcript: &Transcript,
        summarizer: Arc<dyn FallibleSummarizer>,
        session_id: &str,
        workspace_root: &std::path::Path,
    ) -> Result<Option<PreparedAuthoritativeCompaction>, CompactError> {
        // Direct callers with no envelope-derived target in scope get the
        // historical trigger: escalate unless pruning eliminated the entire
        // fold-away set. The live engine passes its meter-derived target
        // through `prepare_authoritative_with_trigger`.
        self.prepare_authoritative_with_trigger(
            transcript,
            summarizer,
            session_id,
            workspace_root,
            0,
        )
        .await
    }

    /// CR-18 (mechanisms 3-1 + 3-3 + 3-4): authoritative prepare with the
    /// cheap-first escalation target supplied by the caller's meter.
    ///
    /// `escalation_trigger_tokens` is derived by the live caller from the
    /// SAME post-call logged envelope that triggered compaction (the
    /// session's pressure overage past the automatic boundary), so the
    /// number that decides escalation and the number that fired the
    /// trigger cannot drift apart. Deterministic, model-free reconstruction
    /// wins whenever pruning brings the fold-away set at or under this
    /// target; anything larger escalates to a real summarizer.
    pub async fn prepare_authoritative_with_trigger(
        &self,
        transcript: &Transcript,
        summarizer: Arc<dyn FallibleSummarizer>,
        session_id: &str,
        workspace_root: &std::path::Path,
        escalation_trigger_tokens: u32,
    ) -> Result<Option<PreparedAuthoritativeCompaction>, CompactError> {
        self.prepare_authoritative_keeping(
            transcript,
            summarizer,
            session_id,
            workspace_root,
            escalation_trigger_tokens,
            self.keep_recent,
        )
        .await
    }

    /// Pressure-driven variant: the session is over its window (or the
    /// provider rejected the context) yet the transcript is shorter than the
    /// recent-message guard, so a small history dominated by large tool
    /// output would otherwise be impossible to compact. Keeps only the pinned,
    /// tool-block-straddling and latest-user anchors.
    pub async fn prepare_authoritative_under_pressure(
        &self,
        transcript: &Transcript,
        summarizer: Arc<dyn FallibleSummarizer>,
        session_id: &str,
        workspace_root: &std::path::Path,
        escalation_trigger_tokens: u32,
    ) -> Result<Option<PreparedAuthoritativeCompaction>, CompactError> {
        self.prepare_authoritative_keeping(
            transcript,
            summarizer,
            session_id,
            workspace_root,
            escalation_trigger_tokens,
            0,
        )
        .await
    }

    async fn prepare_authoritative_keeping(
        &self,
        transcript: &Transcript,
        summarizer: Arc<dyn FallibleSummarizer>,
        session_id: &str,
        workspace_root: &std::path::Path,
        escalation_trigger_tokens: u32,
        keep_recent: usize,
    ) -> Result<Option<PreparedAuthoritativeCompaction>, CompactError> {
        // Mechanism 7 (monotonic retry): observe the surface before doing
        // anything else. Handed back to the caller in the prepared result so
        // a subsequent `retry_authoritative` call can refuse a retry that
        // did not actually change the transcript.
        let surface_generation = SurfaceGeneration::observe(transcript);

        let prior_summary = extract_prior_summary(transcript);
        // Mechanism 5 (tag-based retention): derive the always-keep id set
        // from the ORIGINAL, unmasked transcript -- the observation masker
        // below overwrites tool-message content (including any open-
        // obligation marker) with a placeholder, so tagging must happen
        // before masking even though ids survive it unchanged.
        let always_keep_ids = collect_always_keep_ids(transcript.messages());
        let keep_recent_messages = recent_turn_message_count(transcript.messages(), keep_recent);
        // Snapshot full tagged messages before masking. Structured ids/content
        // validate summary prose; this copy independently survives rebuild.
        let open_obligation_messages = collect_open_obligation_messages(transcript.messages());
        // ObservationMasker: replace stale tool outputs with placeholders before
        // summarization, preserving the last MAX_PRESERVED_SKILL_READ_BLOCKS
        // skill-read blocks verbatim. This mirrors the session-controls masking
        // so direct callers of `prepare_authoritative` also get the invariant.
        let messages = {
            let mut masked = Transcript::new();
            masked.replace(transcript.messages().to_vec());
            let masker = crate::compaction_tuning::ObservationMasker::new(keep_recent);
            masker.mask(&mut masked);
            masked.messages().to_vec()
        };
        let (summarized, kept) = partition_authoritative_messages(messages, keep_recent_messages);
        let kept = protect_open_obligation_messages(kept, &open_obligation_messages);
        let (kept, stubbed_originals) =
            stub_oversized_recent_tool_results(kept, transcript.messages());
        let has_stubbed_outputs = !stubbed_originals.is_empty();

        let mut dropped_count = summarized.len();
        let summarized_ids: std::collections::HashSet<String> = summarized
            .iter()
            .map(|message| message_identity(message).0.to_string())
            .collect();
        let mut removed_messages: Vec<TranscriptMessage> = transcript
            .messages()
            .iter()
            .filter(|message| summarized_ids.contains(message_identity(message).0))
            .cloned()
            .collect();
        for original in stubbed_originals {
            if !removed_messages
                .iter()
                .any(|message| message_identity(message).0 == message_identity(&original).0)
            {
                removed_messages.push(original);
                dropped_count += 1;
            }
        }
        if dropped_count == 0 {
            return Ok(None);
        }

        // Mechanism 5 (tag-based retention): never let cheap pruning elide
        // the task spec, the latest user intent, or an explicitly tagged
        // open obligation (id set computed above, pre-masking).
        let pruner = TagAwarePruner { always_keep_ids };

        // Mechanism 3 (cheap-first escalation): try a non-model prune first,
        // remeasuring through the same token meter, and only escalate to a
        // model call if pruning did not bring the fold-away set under the
        // caller's meter-derived escalation target. A target of 0 means "no
        // escalation" fires only when pruning eliminated the entire
        // to-be-summarized set outright (nothing measurable left to fold) --
        // any surviving content still needs a real summary, so it still
        // escalates.
        let (pruned, outcome) =
            self.cheap_first_escalate(&summarized, &pruner, escalation_trigger_tokens);

        // CR-19: use structured records captured from the ORIGINAL transcript
        // and select those whose ids are in the fold-away prefix. Masking may
        // replace their marker/content, so collecting from `pruned` here
        // would silently make the validation blind to exactly this loss.
        let obligations_at_risk = collect_open_obligations(transcript.messages())
            .into_iter()
            .filter(|obligation| summarized_ids.contains(&obligation.id))
            .collect::<Vec<_>>();

        // CR-19 guard on the deterministic path: the statistical summarizer
        // emits a count digest, never obligation prose, so a fold-away set
        // that still carries a tagged obligation can never take the
        // deterministic shortcut -- it must escalate to a real summarizer
        // whose output is obligation-checked below.
        let outcome = if matches!(outcome, EscalationOutcome::PrunedNoEscalation { .. })
            && (!obligations_at_risk.is_empty() || has_stubbed_outputs)
        {
            EscalationOutcome::EscalateToSummarizer { tokens_after: 0 }
        } else {
            outcome
        };

        let raw_summary = match outcome {
            EscalationOutcome::PrunedNoEscalation { .. } => {
                // Pruning alone was sufficient: produce the fold-away text via
                // the deterministic, model-free statistical summarizer.
                // `summarizer` (the real, possibly expensive, fallible
                // summarizer) is never invoked on this path.
                StatisticalSummarizer
                    .summarize(&pruned, prior_summary.as_deref())
                    .await
            }
            EscalationOutcome::EscalateToSummarizer { .. } => {
                // Mechanism 2 (prefire): before paying for a model call,
                // check whether a background-prepared summary is still
                // valid for the current prefix. The cache is consumed
                // (cleared) either way -- a stale entry must not linger to
                // be checked again next time.
                let cached = self.prefired.lock().unwrap().take();
                match cached {
                    Some(prefired) if self.prefire_still_valid(transcript, &prefired) => {
                        prefired.summary
                    }
                    _ => {
                        let summarizer_input = bounded_summarizer_input(&pruned, &removed_messages);
                        summarizer
                            .try_summarize(&summarizer_input, prior_summary.as_deref())
                            .await?
                    }
                }
            }
        };

        // Mechanism 8 (lenient parsing): a JSON-shaped summary payload
        // tolerates a malformed known field rather than being discarded
        // outright.
        let resolved_summary = resolve_summary_text(&raw_summary);

        // Mechanism 9 (core safety pass, always last): whatever strategy
        // produced the summary, run it through the same final
        // normalization/hard-truncation pass as every other compaction path
        // in this file before it can become part of the transcript.
        let summary = core_safety_pass(&resolved_summary);
        if summary.is_empty() {
            return Err(CompactError::Summarizer(
                "model returned an empty compaction summary".into(),
            ));
        }

        // CR-19: reject -- fail closed, same as an empty summary above --
        // a summary that lost a structured obligation. This runs AFTER the
        // core safety pass (mechanism 9 always runs last) but BEFORE the
        // summary is allowed to become part of the rebuilt transcript, so a
        // dropped obligation never reaches canonical history. The caller's
        // existing fail-closed contract (transcript untouched on `Err`)
        // already protects the obligation's ONLY remaining verbatim copy --
        // the untouched original transcript this attempt was called with.
        if !summary_retains_all_obligations(&summary, &obligations_at_risk) {
            let missing: Vec<&str> = obligations_at_risk
                .iter()
                .filter(|obligation| {
                    !summary.contains(&obligation.id) && !summary.contains(&obligation.content)
                })
                .map(|obligation| obligation.id.as_str())
                .collect();
            return Err(CompactError::Summarizer(format!(
                "summary dropped {} tagged obligation(s): {}",
                missing.len(),
                missing.join(", ")
            )));
        }

        let compact_message = TranscriptMessage::Compact {
            id: format!("compact-{}", now_iso()),
            created_at: now_iso(),
            summary: summary.clone(),
            dropped_count: dropped_count as i32,
            pinned: true,
        };
        let provider = self.memory_block_provider.as_deref();
        Ok(Some(PreparedAuthoritativeCompaction {
            summary: core_safety_pass(&summary),
            dropped_count,
            surface_generation,
            removed_messages,
            retained_turns: keep_recent,
            replacement: canonicalize_authoritative_rebuild(
                rebuild_authoritative_messages(
                    kept,
                    compact_message,
                    session_id,
                    workspace_root,
                    provider,
                ),
                session_id,
            ),
        }))
    }

    /// Mechanism 7 (monotonic retry), authoritative-path wrapper. Refuses to
    /// re-run `prepare_authoritative_with_trigger` unless the transcript's
    /// model-visible surface actually advanced since `previous_generation`
    /// -- a backend can report success without changing model-visible state,
    /// and without this check a caller that retries on every "no progress"
    /// response loops forever paying for summarizer calls that can never
    /// help.
    ///
    /// CR-18 production caller: `SessionEngine::compact_context`, which
    /// routes the attempt after a failure through here (via the per-session
    /// pending-retry ledger in `compactor_retry.rs`) with a strictly smaller
    /// `escalation_trigger_tokens` target, and only on a later turn whose
    /// surface actually advanced.
    pub async fn retry_authoritative(
        &self,
        previous_generation: SurfaceGeneration,
        transcript: &Transcript,
        summarizer: Arc<dyn FallibleSummarizer>,
        session_id: &str,
        workspace_root: &std::path::Path,
        escalation_trigger_tokens: u32,
    ) -> Result<Option<PreparedAuthoritativeCompaction>, CompactError> {
        let current_generation = SurfaceGeneration::observe(transcript);
        if !retry_is_monotonic(previous_generation, current_generation) {
            return Err(CompactError::Summarizer(
                "refusing retry: transcript surface did not advance since the prior attempt".into(),
            ));
        }
        self.prepare_authoritative_with_trigger(
            transcript,
            summarizer,
            session_id,
            workspace_root,
            escalation_trigger_tokens,
        )
        .await
    }

    pub fn commit_authoritative(
        &self,
        transcript: &mut Transcript,
        prepared: PreparedAuthoritativeCompaction,
    ) {
        transcript.replace(prepared.replacement);
    }

    /// Replay a persisted authoritative boundary without invoking a model.
    /// Used only to reconstruct the compact engine context after daemon restart;
    /// the separate human-facing transcript remains complete.
    #[allow(clippy::too_many_arguments)]
    pub fn replay_authoritative_boundary(
        &self,
        transcript: &mut Transcript,
        id: String,
        created_at: String,
        summary: String,
        dropped_count: i32,
        session_id: &str,
        workspace_root: &std::path::Path,
    ) {
        let obligations = collect_open_obligation_messages(transcript.messages());
        let original = transcript.messages().to_vec();
        let (_, kept) = partition_recent_turns(original.clone(), self.keep_recent);
        let kept = protect_open_obligation_messages(kept, &obligations);
        // Live compaction bounds oversized recent tool results; replay must
        // produce the identical retained tail.
        let (kept, _) = stub_oversized_recent_tool_results(kept, &original);
        let compact_message = TranscriptMessage::Compact {
            id,
            created_at,
            summary: core_safety_pass(&summary),
            dropped_count,
            pinned: true,
        };
        let provider = self.memory_block_provider.as_deref();
        transcript.replace(canonicalize_authoritative_rebuild(
            rebuild_authoritative_messages(
                kept,
                compact_message,
                session_id,
                workspace_root,
                provider,
            ),
            session_id,
        ));
    }
}

#[cfg(test)]
mod authoritative_tests {
    use super::*;

    struct FixedSummary(Result<&'static str, &'static str>);

    struct FixedMemory;

    impl MemoryBlockProvider for FixedMemory {
        fn memory_block(&self, _session: SessionRef<'_>) -> Option<String> {
            Some("fresh memory".into())
        }
    }

    #[async_trait]
    impl FallibleSummarizer for FixedSummary {
        async fn try_summarize(
            &self,
            _messages: &[TranscriptMessage],
            _prior_summary: Option<&str>,
        ) -> Result<String, CompactError> {
            self.0
                .map(str::to_string)
                .map_err(|error| CompactError::Summarizer(error.into()))
        }
    }

    fn long_transcript() -> Transcript {
        let mut transcript = Transcript::new();
        for index in 0..12 {
            if index % 2 == 0 {
                transcript.push_user(format!("request {index}"));
            } else {
                transcript.push_assistant(format!("answer {index}"), None, Vec::new());
            }
        }
        transcript
    }

    #[test]
    fn recent_retention_counts_complete_user_turns() {
        let messages = vec![
            TranscriptMessage::User {
                id: "u1".into(),
                created_at: "t".into(),
                content: "one".into(),
                trust: None,
                pinned: false,
                attachments: vec![],
            },
            TranscriptMessage::Assistant {
                id: "a1".into(),
                created_at: "t".into(),
                content: "call".into(),
                reasoning_content: None,
                tool_calls: vec![],
                pinned: false,
            },
            tool_message("r1", "result one"),
            TranscriptMessage::User {
                id: "u2".into(),
                created_at: "t".into(),
                content: "two".into(),
                trust: None,
                pinned: false,
                attachments: vec![],
            },
            TranscriptMessage::Assistant {
                id: "a2".into(),
                created_at: "t".into(),
                content: "answer".into(),
                reasoning_content: None,
                tool_calls: vec![],
                pinned: false,
            },
        ];
        assert_eq!(recent_turn_message_count(&messages, 1), 2);
        assert_eq!(recent_turn_message_count(&messages, 2), 5);
        assert_eq!(recent_turn_message_count(&messages, 8), messages.len());
    }

    #[test]
    fn bounded_tool_evidence_keeps_original_errors_and_retrieval_refs() {
        let original = TranscriptMessage::Tool {
            id: "r".into(),
            created_at: "t".into(),
            tool_call_id: "call-r".into(),
            summary: "verification failed: mismatch".into(),
            output: Some(rightkit_agent_engine_protocol::refs::OutputRef {
                id: "out-ref".into(),
                kind: "tool_output".into(),
                size_bytes: 90_000,
                content_hash: None,
                preview: None,
                media_type: "text/plain".into(),
                truncated: true,
                retrievable: true,
            }),
            trust: None,
            is_error: true,
            pinned: false,
            process_id: None,
        };
        let evidence = bounded_tool_evidence(std::slice::from_ref(&original));
        assert!(evidence.contains("ERROR call-r"));
        assert!(evidence.contains("verification failed"));
        assert!(evidence.contains("out-ref"));
        assert!(evidence.len() <= MAX_TOOL_EVIDENCE_BYTES);
    }

    #[test]
    fn replay_bounds_oversized_recent_tool_results_like_live() {
        let mut transcript = Transcript::new();
        transcript.replace(vec![
            TranscriptMessage::User {
                id: "u1".into(),
                created_at: "t".into(),
                content: "go".into(),
                trust: None,
                pinned: false,
                attachments: vec![],
            },
            tool_message("r1", &"z".repeat(MAX_RECENT_TOOL_SUMMARY_BYTES + 2_000)),
        ]);
        Compactor::new(0, 1).replay_authoritative_boundary(
            &mut transcript,
            "compact-1".into(),
            "t".into(),
            "summary".into(),
            0,
            "s",
            std::path::Path::new("."),
        );
        let bounded = transcript
            .messages()
            .iter()
            .find_map(|m| match m {
                TranscriptMessage::Tool { id, summary, .. } if id == "r1" => Some(summary.clone()),
                _ => None,
            })
            .expect("recent tool result retained");
        assert!(
            bounded.starts_with("[recent tool output bounded;"),
            "{bounded}"
        );
        assert!(bounded.len() < MAX_RECENT_TOOL_SUMMARY_BYTES + 200);
    }

    #[test]
    fn pinned_obligation_result_becomes_non_tool_context_without_reordering_calls() {
        let rebuilt = vec![
            TranscriptMessage::Assistant {
                id: "a".into(),
                created_at: "t".into(),
                content: String::new(),
                reasoning_content: None,
                tool_calls: vec![],
                pinned: true,
            },
            tool_message("r", "OPEN OBLIGATION: preserve this result"),
        ];
        let ordered = canonicalize_authoritative_rebuild(rebuilt, "s");
        assert!(
            matches!(ordered.first(), Some(TranscriptMessage::Assistant { id, .. }) if id == "a")
        );
        assert!(
            matches!(ordered.get(1), Some(TranscriptMessage::System { content, .. }) if content.contains("preserve this result"))
        );
    }

    #[tokio::test]
    async fn manual_path_compacts_below_any_automatic_threshold() {
        let mut transcript = long_transcript();
        let result = Compactor::new(usize::MAX, 4)
            .compact_authoritative(
                &mut transcript,
                Arc::new(FixedSummary(Ok("model summary"))),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(result.summary, "model summary");
        assert!(result.dropped_count > 0);
        assert!(transcript.messages().iter().any(|message| matches!(
            message,
            TranscriptMessage::Compact { summary, .. } if summary == "model summary"
        )));
    }

    #[tokio::test]
    async fn summary_failure_preserves_the_only_full_transcript() {
        let mut transcript = long_transcript();
        let before = serde_json::to_string(transcript.messages()).unwrap();
        let error = Compactor::new(0, 4)
            .compact_authoritative(
                &mut transcript,
                Arc::new(FixedSummary(Err("stalled stream"))),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap_err();

        assert_eq!(error, CompactError::Summarizer("stalled stream".into()));
        assert_eq!(
            serde_json::to_string(transcript.messages()).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn empty_summary_fails_closed_without_statistical_fallback() {
        let mut transcript = long_transcript();
        let before = serde_json::to_string(transcript.messages()).unwrap();
        let error = Compactor::new(0, 4)
            .compact_authoritative(
                &mut transcript,
                Arc::new(FixedSummary(Ok("  \n"))),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("empty compaction summary"));
        assert_eq!(
            serde_json::to_string(transcript.messages()).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn successive_authoritative_compactions_retain_one_merged_marker() {
        let mut transcript = long_transcript();
        let compactor = Compactor::new(0, 4);
        compactor
            .compact_authoritative(
                &mut transcript,
                Arc::new(FixedSummary(Ok("first summary"))),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap()
            .unwrap();
        for index in 0..8 {
            transcript.push_user(format!("later request {index}"));
        }

        compactor
            .compact_authoritative(
                &mut transcript,
                Arc::new(FixedSummary(Ok("merged summary"))),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap()
            .unwrap();

        let summaries = transcript
            .messages()
            .iter()
            .filter_map(|message| match message {
                TranscriptMessage::Compact { summary, .. } => Some(summary.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(summaries, vec!["merged summary"]);
    }

    #[tokio::test]
    async fn authoritative_path_applies_core_safety_pass_to_the_summary() {
        let mut transcript = long_transcript();
        let result = Compactor::new(usize::MAX, 4)
            .compact_authoritative(
                &mut transcript,
                Arc::new(FixedSummary(Ok("  padded summary  "))),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap()
            .unwrap();
        // core_safety_pass (mechanism 9, see compactor_output_budget.rs) trims;
        // a well-formed short summary is otherwise unchanged.
        assert_eq!(result.summary, "padded summary");
    }

    // ---- Wiring tests: these exercise `prepare_authoritative` itself, not
    // the mechanism functions in isolation. A fake summarizer counts calls
    // and/or records the messages it received so each test fails if the
    // corresponding wiring is ever removed.

    struct CountingSummary {
        text: &'static str,
        calls: std::sync::Mutex<u32>,
        received: std::sync::Mutex<Vec<Vec<TranscriptMessage>>>,
    }

    impl CountingSummary {
        fn new(text: &'static str) -> Self {
            Self {
                text,
                calls: std::sync::Mutex::new(0),
                received: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn call_count(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl FallibleSummarizer for CountingSummary {
        async fn try_summarize(
            &self,
            messages: &[TranscriptMessage],
            _prior_summary: Option<&str>,
        ) -> Result<String, CompactError> {
            *self.calls.lock().unwrap() += 1;
            self.received.lock().unwrap().push(messages.to_vec());
            Ok(self.text.to_string())
        }
    }

    fn tool_message(id: &str, summary: &str) -> TranscriptMessage {
        TranscriptMessage::Tool {
            id: id.to_string(),
            created_at: "t".into(),
            tool_call_id: format!("call-{id}"),
            summary: summary.to_string(),
            output: None,
            trust: None,
            is_error: false,
            pinned: false,
            process_id: None,
        }
    }

    #[tokio::test]
    async fn summarizer_receives_bounded_original_folded_tool_evidence() {
        let mut transcript = Transcript::new();
        transcript.push_user("original request");
        transcript.push_tool_result(
            "tool-call",
            "OPEN OBLIGATION: verification error: expected 42, got 41",
            true,
        );
        transcript.push_user("latest request");
        let summarizer = Arc::new(CountingSummary::new(
            "OPEN OBLIGATION: verification error: expected 42, got 41",
        ));
        Compactor::new(0, 1)
            .prepare_authoritative(
                &transcript,
                summarizer.clone(),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap();
        let received = summarizer.received.lock().unwrap();
        assert!(received.iter().flatten().any(|message| matches!(message,
            TranscriptMessage::System { content, .. }
                if content.contains("Bounded evidence from original folded-away tool results")
                    && content.contains("verification error")
        )));
    }

    #[tokio::test]
    async fn a_valid_prefired_summary_is_used_with_zero_summarizer_calls() {
        let transcript = long_transcript();
        let compactor = Compactor::new(usize::MAX, 4);
        let fingerprint = prefire_fingerprint(&transcript, compactor.keep_recent);
        let (summarized, _kept) =
            partition_recent_turns(transcript.messages().to_vec(), compactor.keep_recent);
        compactor.store_prefired(PrefiredSummary {
            fingerprint,
            summary: "prefired summary".into(),
            dropped_count: summarized.len(),
        });

        let summarizer = Arc::new(CountingSummary::new("should never be used"));
        let prepared = compactor
            .prepare_authoritative(
                &transcript,
                summarizer.clone(),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap()
            .expect("prefix long enough to compact");

        assert_eq!(prepared.summary, "prefired summary");
        assert_eq!(
            summarizer.call_count(),
            0,
            "a valid prefire must resolve without any model call"
        );
    }

    #[tokio::test]
    async fn a_mutated_prefix_causes_a_refire() {
        let transcript = long_transcript();
        let compactor = Compactor::new(usize::MAX, 4);
        let fingerprint = prefire_fingerprint(&transcript, compactor.keep_recent);
        let (summarized, _kept) =
            partition_recent_turns(transcript.messages().to_vec(), compactor.keep_recent);
        compactor.store_prefired(PrefiredSummary {
            fingerprint,
            summary: "stale prefire".into(),
            dropped_count: summarized.len(),
        });

        // Same shape, different content: the fingerprint no longer matches.
        let mut mutated = Transcript::new();
        mutated.push_user("an edited early request replacing all prior history");
        for index in 1..12 {
            if index % 2 == 0 {
                mutated.push_user(format!("request {index}"));
            } else {
                mutated.push_assistant(format!("answer {index}"), None, Vec::new());
            }
        }

        let summarizer = Arc::new(CountingSummary::new("fresh summary"));
        let prepared = compactor
            .prepare_authoritative(
                &mutated,
                summarizer.clone(),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap()
            .expect("prefix long enough to compact");

        assert_eq!(prepared.summary, "fresh summary");
        assert_eq!(
            summarizer.call_count(),
            1,
            "a mutated prefix must refire through the real summarizer"
        );
    }

    #[tokio::test]
    async fn pruning_alone_when_sufficient_makes_zero_summarizer_calls() {
        // A transcript made entirely of untagged tool chatter: the cheap
        // pruner eliminates all of it, leaving nothing that needs a real
        // summary.
        let mut transcript = Transcript::new();
        for index in 0..8 {
            transcript.replace({
                let mut messages = transcript.messages().to_vec();
                messages.push(tool_message(
                    &format!("chatter-{index}"),
                    "ls output: a.txt",
                ));
                messages
            });
        }
        for index in 0..4 {
            transcript.push_user(format!("later {index}"));
        }

        let compactor = Compactor::new(usize::MAX, 4);
        let summarizer = Arc::new(CountingSummary::new("should never be used"));
        let prepared = compactor
            .prepare_authoritative(
                &transcript,
                summarizer.clone(),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap();

        assert_eq!(
            summarizer.call_count(),
            0,
            "pruning alone must resolve without any model call"
        );
        assert!(
            prepared.is_some(),
            "pruning must still produce a compaction result"
        );
    }

    #[tokio::test]
    async fn tagged_obligations_survive_a_prune_that_drops_other_content() {
        let mut transcript = Transcript::new();
        transcript.push_user("task spec: finish the migration");
        transcript.replace({
            let mut messages = transcript.messages().to_vec();
            messages.push(tool_message(
                "obligation-tool-1",
                "OPEN OBLIGATION: must finish the migration",
            ));
            messages.push(tool_message("chatter-tool-2", "ls output: a.txt b.txt"));
            messages
        });
        for index in 0..4 {
            transcript.push_user(format!("later {index}"));
        }

        let compactor = Compactor::new(usize::MAX, 4);
        // CR-19: the summarizer's output must retain the obligation's
        // content or id, or `prepare_authoritative` now rejects it (see
        // `summary_retains_all_obligations`) -- this fixed text stands in
        // for a real summarizer that DID retain it.
        let summarizer = Arc::new(CountingSummary::new(
            "real summary; must finish the migration",
        ));
        compactor
            .prepare_authoritative(
                &transcript,
                summarizer.clone(),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap();

        let received = summarizer.received.lock().unwrap();
        let call = received.last().expect("summarizer must have been called");
        assert!(
            call.iter().any(|message| matches!(
                message,
                TranscriptMessage::System { content, .. }
                    if content.contains("OPEN OBLIGATION: must finish the migration")
            )),
            "bounded original evidence must carry tagged obligation into summary input"
        );
        assert!(
            !call
                .iter()
                .any(|m| matches!(m, TranscriptMessage::Tool { id, .. } if id == "chatter-tool-2")),
            "untagged chatter is expected to be pruned away before the summarizer"
        );
    }

    #[tokio::test]
    async fn a_summary_that_drops_a_tagged_obligation_is_rejected() {
        let mut transcript = Transcript::new();
        transcript.push_user("task spec: finish the migration");
        transcript.replace({
            let mut messages = transcript.messages().to_vec();
            messages.push(tool_message(
                "obligation-tool-1",
                "OPEN OBLIGATION: must finish the migration",
            ));
            messages
        });
        for index in 0..4 {
            transcript.push_user(format!("later {index}"));
        }

        let compactor = Compactor::new(usize::MAX, 4);
        // A summarizer whose output mentions neither the obligation's id
        // nor its content -- exactly the silent-loss failure CR-19 closes.
        let summarizer = Arc::new(CountingSummary::new("an unrelated summary"));
        let before = serde_json::to_string(transcript.messages()).unwrap();

        let error = compactor
            .prepare_authoritative(
                &transcript,
                summarizer.clone(),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("dropped") && error.to_string().contains("obligation"),
            "expected an obligation-loss rejection, got: {error}"
        );
        assert_eq!(
            serde_json::to_string(transcript.messages()).unwrap(),
            before,
            "a rejected summary must leave the transcript untouched (fail-closed)"
        );
    }

    #[tokio::test]
    async fn a_summary_that_retains_the_obligation_content_is_accepted() {
        let mut transcript = Transcript::new();
        transcript.push_user("task spec: finish the migration");
        transcript.replace({
            let mut messages = transcript.messages().to_vec();
            messages.push(tool_message(
                "obligation-tool-1",
                "OPEN OBLIGATION: must finish the migration",
            ));
            messages
        });
        for index in 0..4 {
            transcript.push_user(format!("later {index}"));
        }

        let compactor = Compactor::new(usize::MAX, 4);
        let summarizer = Arc::new(CountingSummary::new(
            "summary that still mentions: must finish the migration",
        ));

        let prepared = compactor
            .prepare_authoritative(
                &transcript,
                summarizer,
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap()
            .expect("prefix long enough to compact");

        assert!(prepared.summary.contains("must finish the migration"));
    }

    #[tokio::test]
    async fn collect_open_obligations_extracts_structured_id_and_content() {
        let messages = vec![
            tool_message("obligation-1", "OPEN OBLIGATION: ship the release notes"),
            tool_message("chatter-1", "ls output: a.txt"),
        ];
        let obligations = collect_open_obligations(&messages);
        assert_eq!(obligations.len(), 1);
        assert_eq!(obligations[0].id, "obligation-1");
        assert_eq!(obligations[0].content, "ship the release notes");
    }

    #[tokio::test]
    async fn authoritative_rebuild_retains_obligation_message_after_masking() {
        let mut transcript = Transcript::new();
        transcript.push_user("task spec: ship the release");
        transcript.push_tool_result("call-1", "OPEN OBLIGATION: ship release notes", false);
        for index in 0..5 {
            transcript.push_user(format!("later request {index}"));
        }

        let compactor = Compactor::new(usize::MAX, 2);
        let prepared = compactor
            .prepare_authoritative(
                &transcript,
                Arc::new(FixedSummary(Ok("summary: ship release notes"))),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap()
            .expect("prefix long enough to compact");
        compactor.commit_authoritative(&mut transcript, prepared);

        assert!(transcript.messages().iter().any(|message| matches!(
            message,
            TranscriptMessage::System { id, content, pinned: true, .. }
                if id.starts_with("preserved-obligation:")
                    && content.contains("OPEN OBLIGATION: ship release notes")
        )));
    }

    #[test]
    fn replay_boundary_safety_pass_runs_before_write() {
        let mut transcript = long_transcript();
        Compactor::new(usize::MAX, 4).replay_authoritative_boundary(
            &mut transcript,
            "compact-replay".into(),
            "t".into(),
            "  replay summary  ".into(),
            2,
            "sess",
            std::path::Path::new("/tmp"),
        );

        assert!(transcript.messages().iter().any(|message| matches!(
            message,
            TranscriptMessage::Compact { summary, .. } if summary == "replay summary"
        )));
    }

    #[test]
    fn replay_rebuild_orders_initial_context_memory_summary_and_tail() {
        let mut transcript = Transcript::new();
        transcript.replace(vec![
            post_compaction_continuation_message(),
            TranscriptMessage::System {
                id: "reference-context".into(),
                created_at: "t".into(),
                content: "standing instruction".into(),
                pinned: true,
            },
            memory_block_message("sess", "stale memory"),
            TranscriptMessage::user("latest-user", "t", "continue work", None, false),
        ]);

        Compactor::new(usize::MAX, 1)
            .with_memory_block_provider(Arc::new(FixedMemory))
            .replay_authoritative_boundary(
                &mut transcript,
                "compact-replay".into(),
                "t".into(),
                "summary".into(),
                1,
                "sess",
                std::path::Path::new("/tmp"),
            );

        let ids = transcript
            .messages()
            .iter()
            .map(|message| match message {
                TranscriptMessage::System { id, .. }
                | TranscriptMessage::User { id, .. }
                | TranscriptMessage::Assistant { id, .. }
                | TranscriptMessage::Tool { id, .. }
                | TranscriptMessage::Compact { id, .. }
                | TranscriptMessage::Fold { id, .. } => id.as_str(),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            ids,
            vec![
                "reference-context",
                POST_COMPACTION_CONTINUATION_ID,
                "memory-block:sess",
                "compact-replay",
                "latest-user",
            ]
        );
        assert!(transcript.messages().iter().any(|message| matches!(
            message,
            TranscriptMessage::System { id, content, .. }
                if id == "memory-block:sess" && content == "fresh memory"
        )));
    }

    #[tokio::test]
    async fn a_malformed_field_in_a_json_summary_does_not_discard_it() {
        let transcript = long_transcript();
        let compactor = Compactor::new(usize::MAX, 4);
        let raw = serde_json::json!({
            "text": 42,
            "obligations": "finish the migration",
            "confidence": "high",
        })
        .to_string();
        let summarizer = Arc::new(FixedSummary(Ok(Box::leak(raw.into_boxed_str()))));

        let prepared = compactor
            .prepare_authoritative(
                &transcript,
                summarizer,
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap()
            .expect("prefix long enough to compact");

        assert_eq!(
            prepared.summary, "42",
            "a malformed known field must coerce, not discard, the summary"
        );
    }

    #[tokio::test]
    async fn retry_is_refused_when_the_surface_generation_did_not_advance() {
        let transcript = long_transcript();
        let compactor = Compactor::new(usize::MAX, 4);
        let summarizer = Arc::new(CountingSummary::new("first pass"));

        let prepared = compactor
            .prepare_authoritative(
                &transcript,
                summarizer.clone(),
                "sess",
                std::path::Path::new("/tmp"),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summarizer.call_count(), 1);

        // The transcript did not change since the first attempt: a retry
        // must be refused without touching the summarizer again.
        let retried = compactor
            .retry_authoritative(
                prepared.surface_generation,
                &transcript,
                summarizer.clone(),
                "sess",
                std::path::Path::new("/tmp"),
                0,
            )
            .await;

        assert!(
            retried.is_err(),
            "a retry with no surface-generation advance must be refused"
        );
        assert_eq!(
            summarizer.call_count(),
            1,
            "a refused retry must not call the summarizer again"
        );
    }
}
