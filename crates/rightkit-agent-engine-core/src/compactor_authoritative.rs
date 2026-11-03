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
