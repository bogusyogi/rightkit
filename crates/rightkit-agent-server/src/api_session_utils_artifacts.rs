use super::*;

/// Build + publish an `AgentRosterUpdated` snapshot from the parent's roster (F1).
/// Also feeds the F2 budget meter: the active goal's `tokens_used` is the
/// sub-agent rollup (parent + ALL children), so a runaway sub-agent fleet trips
/// the goal's token cap — the [council P0] correctness fix.
/// The main agent's rail entry, read from the session row's own sources so the
/// rail and the row (`session_row`) cannot disagree: model and provider under
/// one guard, `costUsd` from the same `cost_usd` slot (null = unknown).
pub(crate) fn main_agent_entry(
    parent: &LiveSession,
) -> rightkit_agent_engine_protocol::AgentRosterMain {
    let (model, provider) = {
        let model = lock_recover(&parent.model, "session.model");
        (
            model.clone(),
            lock_recover(&parent.provider_id, "session.provider_id").clone(),
        )
    };
    rightkit_agent_engine_protocol::AgentRosterMain {
        sub_session_id: parent.id.clone(),
        role: "main".into(),
        label: "Main".into(),
        model,
        provider: provider.filter(|p| !p.is_empty()),
        cost_usd: *lock_recover(&parent.cost_usd, "session.cost_usd"),
        status: if parent.live_turn_handle.is_active() {
            "running"
        } else {
            "idle"
        }
        .into(),
    }
}

/// The roster as the rail reads it: children, the main agent, and the total.
pub(crate) fn agent_roster_view(parent: &LiveSession) -> serde_json::Value {
    let rows = lock_recover(&parent.agent_roster, "session.agent_roster").rows();
    let main = main_agent_entry(parent);
    let (total, known) = rightkit_agent_engine_protocol::roster_total_cost(main.cost_usd, &rows);
    serde_json::json!({
        "main": main,
        "agents": rows,
        "totalCostUsd": total,
        "costKnown": known,
    })
}

pub(crate) fn publish_agent_roster(parent: &LiveSession) {
    let (agents, rollup) = {
        let mut roster = lock_recover(&parent.agent_roster, "session.agent_roster");
        // Skip broadcasts that carry only counter movement; spawns, completions,
        // failures and new tool calls always go out immediately.
        if !roster.should_publish() {
            return;
        }
        (roster.rows(), roster.rollup(&parent.id, &now_iso()))
    };
    if let Some(goal) = lock_recover(&parent.goal_state, "session.goal_state").as_mut() {
        goal.tokens_used = (rollup.tokens_in + rollup.tokens_out) as i64;
    }
    let main = main_agent_entry(parent);
    let (total, known) = rightkit_agent_engine_protocol::roster_total_cost(main.cost_usd, &agents);
    let seq = parent.conduit.next_seq();
    parent.conduit.publish(ProtocolEvent::AgentRosterUpdated {
        session_id: parent.id.clone(),
        seq,
        created_at: now_iso(),
        agents,
        main: Some(main),
        total_cost_usd: Some(total),
        cost_known: known,
    });
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub(crate) fn spawn_event_persister(
    session_id: String,
    conduit: Arc<Conduit>,
    store: SharedStorage,
    deleted_session_ids: Arc<Mutex<HashSet<String>>>,
    persistence_state: rightkit_agent_engine_storage::EventPersistenceState,
    initial_events: Vec<(i64, String, String)>,
) {
    // Explicit persistence ingress is installed synchronously before the
    // session becomes reachable. Durable facts never depend on subscribing to
    // their own live broadcast; projections are enqueued before live delivery.
    let first_seq = conduit.current_high_water().saturating_add(1);
    let mut admissions = conduit.install_persistence_gate();
    let conduit_for_commit = conduit.clone();
    tokio::spawn(async move {
        // Events that precede everything the session publishes (a forked child's seed): stored
        // first, in order, waiting for the session row they belong to.
        for (seq, kind, payload) in initial_events {
            let mut stored = false;
            for attempt in 0..60 {
                if attempt > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                let store = store.clone();
                let sid = session_id.clone();
                let (kind, payload) = (kind.clone(), payload.clone());
                let result = tokio::task::spawn_blocking(move || {
                    let mut storage = store.blocking_lock();
                    crate::api::append_event_via_storage_authority(
                        &mut storage,
                        &sid,
                        seq,
                        &kind,
                        &payload,
                    )
                })
                .await;
                if matches!(result, Ok(Ok(()))) {
                    stored = true;
                    break;
                }
            }
            if !stored {
                conduit_for_commit
                    .latch_persistence_failure(format!("seed event {seq} could not be stored"));
                break;
            }
        }
        // Producers reserve sequence numbers before constructing/publishing
        // events. A concurrent producer can therefore enqueue N+1 before N.
        // Keep that event until its predecessor commits instead of exhausting
        // retries & permanently losing the durable fact at the queue's head.
        let mut next_seq = first_seq;
        let mut waiting = std::collections::BTreeMap::new();
        loop {
            let ev = if let Some(event) = waiting.remove(&next_seq) {
                event
            } else {
                let Some(event) = admissions.recv().await else {
                    break;
                };
                let seq = event.meta().seq;
                if seq > next_seq {
                    // Compaction commits its companion rows outside this queue.
                    // Advance only over a contiguous range actually in SQLite;
                    // an uncommitted predecessor still takes the reorder path.
                    let gap = seq.saturating_sub(next_seq);
                    if gap <= rightkit_agent_engine_core::conduit::MAX_HISTORY_EVENTS as i64 {
                        let committed = store.lock().await.events_in_range(
                            &session_id,
                            next_seq,
                            seq - 1,
                            gap as usize,
                        );
                        if let Ok(rows) = committed {
                            if rows.len() == gap as usize
                                && rows.iter().enumerate().all(|(index, row)| {
                                    row.seq == next_seq.saturating_add(index as i64)
                                })
                            {
                                next_seq = seq;
                            }
                        }
                    }
                }
                if seq > next_seq {
                    if waiting.len() >= rightkit_agent_engine_core::conduit::MAX_HISTORY_EVENTS {
                        conduit_for_commit.latch_persistence_failure(format!(
                            "event admission reorder bound reached while waiting for sequence {next_seq}"
                        ));
                        let previous = persistence_state.snapshot();
                        persistence_state.update(rightkit_agent_engine_storage::EventPersistenceSnapshot {
                            last_error: Some(format!("event admission reorder bound reached while waiting for sequence {next_seq}")),
                            ..previous
                        });
                        tracing::error!(session_id = %session_id, missing_seq = next_seq, "event persistence admission stopped at reorder bound");
                        break;
                    }
                    waiting.entry(seq).or_insert(event);
                    continue;
                }
                event
            };
            let durable = ev.is_durable_fact();
            let meta = ev.meta();
            let durable_envelope = if durable {
                match rightkit_agent_engine_protocol::event::EventEnvelopeV1::canonical(ev.clone())
                {
                    Ok(envelope) => Some(envelope),
                    Err(error) => {
                        conduit_for_commit.latch_persistence_failure(format!(
                            "invalid durable event envelope at seq {}: {error}",
                            meta.seq
                        ));
                        let previous = persistence_state.snapshot();
                        persistence_state.update(
                            rightkit_agent_engine_storage::EventPersistenceSnapshot {
                                last_published_seq: previous.last_published_seq,
                                last_persisted_seq: previous.last_persisted_seq,
                                last_error: Some(format!(
                                    "invalid durable event envelope: {error}"
                                )),
                            },
                        );
                        continue;
                    }
                }
            } else {
                None
            };
            let payload = serde_json::to_string(&ev).unwrap_or_default();
            let kind = serde_json::from_str::<serde_json::Value>(&payload)
                .ok()
                .and_then(|value| {
                    value
                        .get("$kind")
                        .and_then(|kind| kind.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_default();
            let sid = if meta.session_id.is_empty() {
                session_id.clone()
            } else {
                meta.session_id.clone()
            };

            let mut last_error = None;
            let mut persisted = false;
            let mut deleted = false;
            for attempt in 0..3 {
                if attempt > 0 {
                    let backoff_ms = 50u64 * (1u64 << attempt);
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                }
                let store = store.clone();
                let sid = sid.clone();
                let kind = kind.clone();
                let payload = payload.clone();
                let deleted_session_ids = deleted_session_ids.clone();
                let event = ev.clone();
                let meta = meta.clone();
                let result = tokio::task::spawn_blocking(move || -> Result<bool, String> {
                    let deleted_ids = lock_recover(&deleted_session_ids, "deleted_session_ids");
                    if deleted_ids.contains(&sid) {
                        return Ok(false);
                    }
                    let mut storage = store.blocking_lock();
                    crate::api::append_event_via_storage_authority(
                        &mut storage,
                        &sid,
                        meta.seq,
                        &kind,
                        &payload,
                    )?;
                    if let Err(error) = crate::api_tool_invocations::persist_tool_invocation_event(
                        &mut storage,
                        &event,
                    ) {
                        tracing::warn!(session_id = %sid, %error, "tool-invocation persist failed");
                    }
                    Ok(true)
                })
                .await;
                match result {
                    Ok(Ok(true)) => {
                        persisted = true;
                        break;
                    }
                    Ok(Ok(false)) => {
                        deleted = true;
                        break;
                    }
                    Ok(Err(error)) => last_error = Some(error),
                    Err(error) => last_error = Some(format!("persist task join failed: {error}")),
                }
            }

            if deleted {
                continue;
            }
            if persisted {
                if meta.seq == next_seq {
                    next_seq = next_seq.saturating_add(1);
                }
                let previous = persistence_state.snapshot();
                persistence_state.update(rightkit_agent_engine_storage::EventPersistenceSnapshot {
                    last_published_seq: if durable {
                        previous.last_published_seq
                    } else {
                        meta.seq.max(previous.last_published_seq)
                    },
                    last_persisted_seq: meta.seq.max(previous.last_persisted_seq),
                    last_error: None,
                });
                if durable {
                    let envelope = durable_envelope
                        .as_ref()
                        .expect("durable event validated before persistence");
                    tracing::trace!(
                        event_id = %envelope.event_id,
                        schema_version = envelope.schema_version,
                        "publishing committed durable event"
                    );
                    conduit_for_commit.publish_committed(ev);
                    let previous = persistence_state.snapshot();
                    persistence_state.update(
                        rightkit_agent_engine_storage::EventPersistenceSnapshot {
                            last_published_seq: meta.seq.max(previous.last_published_seq),
                            last_persisted_seq: previous.last_persisted_seq,
                            last_error: None,
                        },
                    );
                }
                continue;
            }

            let error = last_error.unwrap_or_else(|| "unknown persistence failure".to_string());
            tracing::error!(
                session_id = %sid,
                seq = meta.seq,
                %error,
                durable,
                "event persistence failed after retries"
            );
            // Latch session health (live-only) so producers are refused and
            // the engine loop stops; nothing durable is written about it.
            conduit_for_commit.latch_persistence_failure(format!(
                "seq {} failed to persist after retries: {error}",
                meta.seq
            ));
            let previous = persistence_state.snapshot();
            persistence_state.update(rightkit_agent_engine_storage::EventPersistenceSnapshot {
                last_published_seq: if durable {
                    previous.last_published_seq
                } else {
                    meta.seq.max(previous.last_published_seq)
                },
                last_persisted_seq: previous.last_persisted_seq,
                last_error: Some(error),
            });
            // A failed durable fact is deliberately NOT emitted as RunFailed:
            // doing so would make a semantic failure visible without durable
            // truth and would recurse through the same failed persistence path.
        }
    });
}

/// Keep a coarse, bounded snapshot of the reply that is still streaming, so a
/// crash mid-reply does not lose the text shown so far. One row per session is
/// overwritten at most every 2 s (or per 4 KiB of new text); any settled fact
/// that follows the reply supersedes it. Token deltas stay live-only: this row
/// is read only on resume (`partial_reply_recovery_event`).
pub(crate) fn spawn_partial_reply_snapshotter(
    session_id: String,
    conduit: Arc<Conduit>,
    store: SharedStorage,
    deleted_session_ids: Arc<Mutex<HashSet<String>>>,
) {
    const FLUSH_EVERY: std::time::Duration = std::time::Duration::from_secs(2);
    const FLUSH_BYTES: usize = 4 * 1024;
    const MAX_BYTES: usize = 256 * 1024;
    let mut subscription = conduit.subscribe();
    drop(conduit);
    tokio::spawn(async move {
        let mut text = String::new();
        let mut anchor_seq = 0i64;
        let mut unsaved_bytes = 0usize;
        let mut written = false;
        let mut last_flush = std::time::Instant::now();
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            enum Step {
                Event(Box<ProtocolEvent>),
                Tick,
                Done,
            }
            let step = tokio::select! {
                received = subscription.recv() => match received {
                    rightkit_agent_engine_core::conduit::ConduitReceive::Event(event) => {
                        Step::Event(Box::new(event))
                    }
                    rightkit_agent_engine_core::conduit::ConduitReceive::Lagged { .. } => Step::Tick,
                    rightkit_agent_engine_core::conduit::ConduitReceive::Closed => Step::Done,
                },
                _ = tick.tick() => Step::Tick,
            };
            let mut settled = false;
            match step {
                Step::Done => break,
                Step::Tick => {}
                Step::Event(event) => match *event {
                    ProtocolEvent::AssistantDelta {
                        seq,
                        text: delta,
                        is_reasoning: false,
                        ..
                    } => {
                        if text.is_empty() {
                            anchor_seq = seq;
                            last_flush = std::time::Instant::now();
                        }
                        if text.len() < MAX_BYTES {
                            text.push_str(&delta);
                            unsaved_bytes += delta.len();
                        }
                    }
                    // The reply ended, moved on to tools, or the turn did: the
                    // snapshot is superseded by a settled fact or a fresh start.
                    ProtocolEvent::AssistantMessageCompleted { .. }
                    | ProtocolEvent::ToolCallStarted { .. }
                    | ProtocolEvent::ToolApprovalRequested { .. }
                    | ProtocolEvent::SessionTurnFinished { .. }
                    | ProtocolEvent::RunInterrupted { .. }
                    | ProtocolEvent::RunFailed { .. }
                    | ProtocolEvent::BudgetExceeded { .. }
                    | ProtocolEvent::ProviderRetrying { .. } => settled = true,
                    _ => {}
                },
            }
            if settled {
                text.clear();
                unsaved_bytes = 0;
                // While the daemon is stopping, a snapshot stays: shutdown turns it into an interrupted reply
                // once the persister has caught up (`append_partial_reply_at_shutdown`).
                if written
                    && !crate::api::SHUTDOWN_SETTLING.load(std::sync::atomic::Ordering::SeqCst)
                {
                    written = false;
                    let store = store.clone();
                    let sid = session_id.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let _ = store.blocking_lock().clear_partial_reply(&sid);
                    })
                    .await;
                }
                continue;
            }
            if unsaved_bytes > 0
                && (unsaved_bytes >= FLUSH_BYTES || last_flush.elapsed() >= FLUSH_EVERY)
            {
                unsaved_bytes = 0;
                last_flush = std::time::Instant::now();
                written = true;
                let store = store.clone();
                let sid = session_id.clone();
                let snapshot = text.clone();
                let deleted = deleted_session_ids.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if lock_recover(&deleted, "deleted_session_ids").contains(&sid) {
                        return;
                    }
                    let _ = store.blocking_lock().save_partial_reply(
                        &sid,
                        anchor_seq,
                        &snapshot,
                        &now_iso(),
                    );
                })
                .await;
            }
        }
    });
}

/// Typed outcome of the latch-cancel watcher for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LatchCancelOutcome {
    /// Persistence latched: the session interrupt was fired, cancelling every
    /// in-flight owned tool execution (they settle through the interrupt
    /// path, never as success). `reason` is the first latch reason.
    InFlightCancelled { reason: String },
    /// The conduit was dropped before any latch; nothing to cancel.
    SessionEnded,
}

/// Cancel in-flight tool executions when the session's persistence latches.
/// Fires the same interrupt channel the engine's tool dispatch already races
/// against, so a tool that was already running is killed rather than left to
/// finish unrecorded. Holds no strong reference to the conduit.
pub(crate) fn spawn_latch_cancel(
    conduit: &Arc<Conduit>,
    session_id: String,
    interrupt_tx: tokio::sync::watch::Sender<bool>,
) -> tokio::task::JoinHandle<LatchCancelOutcome> {
    let mut latch = conduit.subscribe_latch();
    let weak_conduit = Arc::downgrade(conduit);
    tokio::spawn(async move {
        let reason = match latch.wait_for(|reason| reason.is_some()).await {
            Ok(guard) => guard.clone().unwrap_or_default(),
            Err(_) => return LatchCancelOutcome::SessionEnded,
        };
        tracing::error!(%reason, "persistence latched: cancelling in-flight tool executions");
        interrupt_tx.send_replace(true);
        // Tell live clients. PersistenceFailed is a live projection: it takes
        // no durable seq and never enters the (failed) persistence queue.
        if let Some(conduit) = weak_conduit.upgrade() {
            conduit.publish(
                rightkit_agent_engine_protocol::ProtocolEvent::PersistenceFailed {
                    session_id,
                    seq: 0,
                    created_at: now_iso(),
                    reason: reason.clone(),
                },
            );
        }
        LatchCancelOutcome::InFlightCancelled { reason }
    })
}

/// Wait until the durable consumer has acknowledged `target_seq`. A request
/// that accepted a user turn must not report success while that turn exists
/// only in memory.
pub(crate) async fn await_event_persistence(
    state: rightkit_agent_engine_storage::EventPersistenceState,
    target_seq: i64,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        // The first event of a new session also pays for opening/migrating
        // the store; under antivirus or disk load that alone can exceed a
        // couple of seconds, which must not reject a valid user turn.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut snapshot = state.snapshot();
        loop {
            if snapshot.last_persisted_seq < target_seq {
                if let Some(error) = snapshot.last_error.clone() {
                    return Err(error);
                }
            }
            if snapshot.last_published_seq >= target_seq {
                if snapshot.last_persisted_seq >= target_seq {
                    return Ok(());
                }
                return Err(snapshot
                    .last_error
                    .unwrap_or_else(|| "event persistence did not acknowledge the turn".into()));
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(format!(
                    "timed out waiting for durable event sequence {target_seq}"
                ));
            }
            snapshot = state.wait_for_change(snapshot, deadline - now);
        }
    })
    .await
    .map_err(|error| format!("event persistence acknowledgement task failed: {error}"))?
}

pub(crate) fn record_subagent_transcript_artifact(
    state: &DaemonState,
    fork: &SubagentFork,
    success: bool,
) {
    let Some(parent) = state.get_session(&fork.parent_session_id) else {
        return;
    };
    let Some(child) = state.get_session(&fork.sub_session_id) else {
        return;
    };
    let transcript = lock_recover(&child.transcript, "session.transcript").clone();
    let transcript_json = serde_json::to_string(&transcript).unwrap_or_default();
    let artifact = rightkit_agent_engine_protocol::ArtifactRow {
        id: format!("subagent-transcript-{}", fork.sub_session_id),
        path: format!("session://{}/transcript", fork.sub_session_id),
        kind: "subagent_transcript".to_string(),
        content_hash: format!(
            "sha256:{}",
            rightkit_fs::digest::sha256_bytes(transcript_json.as_bytes())
        ),
        shippable: false,
        producer: fork.role.clone(),
        provenance: fork.sub_session_id.clone(),
        trust_label: "unreviewed".to_string(),
        verdict: None,
        approvals: Vec::new(),
        meta: fork.artifact_meta(success, transcript.len()).to_string(),
        created_at: now_iso(),
    };
    push_session_artifact(&parent, artifact);
}
