use super::*;

#[cfg(test)]
static EVT003_PROJECTION_GATES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Notify>>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn evt003_projection_gates(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Notify>>> {
    EVT003_PROJECTION_GATES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
pub(crate) fn evt003_pause_projection(session_id: &str) -> Arc<tokio::sync::Notify> {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut gates = evt003_projection_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gates.insert(session_id.to_string(), gate.clone());
    gate
}

#[cfg(test)]
pub(crate) fn evt003_take_projection_gate(session_id: &str) -> Option<Arc<tokio::sync::Notify>> {
    evt003_projection_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session_id)
}

/// Build + publish an `AgentRosterUpdated` snapshot from the parent's roster (F1).
/// Also feeds the F2 budget meter: the active goal's `tokens_used` is the
/// sub-agent rollup (parent + ALL children), so a runaway sub-agent fleet trips
/// the goal's token cap — the [council P0] correctness fix.
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
    let seq = parent.conduit.next_seq();
    parent.conduit.publish(ProtocolEvent::AgentRosterUpdated {
        session_id: parent.id.clone(),
        seq,
        created_at: now_iso(),
        agents,
    });
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Install the session's explicit persistence ingress and append every event
/// to SQLite, so the event log survives a daemon restart. Wired for **every**
/// session (top-level chat and sub-agents) — the durable record of the whole
/// agent tree. Durable facts are published only after this worker commits them.
///
/// F1 wiring: when the published event carries a non-zero `seq` (i.e. it was
/// produced by the F1 unified-seq contract), the persister calls the typed
/// `append_event_at_seq` and surfaces a typed `PersistenceGapError` instead
/// of silently dropping the row. Legacy events with `seq == 0` still take
/// the producer-assigned legacy path so the persister does not regress any
/// pre-F1 publisher; the F1 contract will replace those over time.
#[cfg(test)]
static EVT003_STOP_AFTER_PERSIST_SESSION: std::sync::Mutex<Option<String>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn evt003_take_stop_after_persist(session_id: &str) -> bool {
    let mut guard = EVT003_STOP_AFTER_PERSIST_SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.as_deref() == Some(session_id) {
        *guard = None;
        true
    } else {
        false
    }
}

pub(crate) fn spawn_event_persister(
    session_id: String,
    conduit: Arc<Conduit>,
    store: SharedStorage,
    deleted_session_ids: Arc<Mutex<HashSet<String>>>,
    persistence_state: rightkit_agent_engine_storage::EventPersistenceState,
) {
    // Explicit persistence ingress is installed synchronously before the
    // session becomes reachable. Durable facts never depend on subscribing to
    // their own live broadcast; projections are enqueued before live delivery.
    let first_seq = conduit.current_high_water().saturating_add(1);
    let mut admissions = conduit.install_persistence_gate();
    let conduit_for_commit = conduit.clone();
    tokio::spawn(async move {
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
                match rightkit_agent_engine_protocol::event::EventEnvelopeV1::canonical(ev.clone()) {
                    Ok(envelope) => Some(envelope),
                    Err(error) => {
                        conduit_for_commit.latch_persistence_failure(format!(
                            "invalid durable event envelope at seq {}: {error}",
                            meta.seq
                        ));
                        let previous = persistence_state.snapshot();
                        persistence_state.update(rightkit_agent_engine_storage::EventPersistenceSnapshot {
                            last_published_seq: previous.last_published_seq,
                            last_persisted_seq: previous.last_persisted_seq,
                            last_error: Some(format!("invalid durable event envelope: {error}")),
                        });
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
                #[cfg(test)]
                if durable && evt003_take_stop_after_persist(&sid) {
                    return;
                }
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
                    persistence_state.update(rightkit_agent_engine_storage::EventPersistenceSnapshot {
                        last_published_seq: meta.seq.max(previous.last_published_seq),
                        last_persisted_seq: previous.last_persisted_seq,
                        last_error: None,
                    });
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

/// Typed outcome of the latch-cancel watcher for one session.
#[allow(dead_code)]
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
            conduit.publish(rightkit_agent_engine_protocol::ProtocolEvent::PersistenceFailed {
                session_id,
                seq: 0,
                created_at: now_iso(),
                reason: reason.clone(),
            });
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

#[cfg(test)]
mod evt003_tests {
    use super::*;

    fn durable_event(seq: i64) -> ProtocolEvent {
        ProtocolEvent::UserMessageSubmitted {
            session_id: "evt003".into(),
            seq,
            created_at: now_iso(),
            text: "persist me".into(),
            nonce: None,
            trust: None,
            attachments: Vec::new(),
        }
    }

    fn projection_event(seq: i64) -> ProtocolEvent {
        ProtocolEvent::AssistantDelta {
            session_id: "evt003".into(),
            seq,
            created_at: now_iso(),
            text: "delta".into(),
            is_reasoning: false,
        }
    }

    fn start_persister() -> (
        SharedStorage,
        Arc<Conduit>,
        rightkit_agent_engine_storage::EventPersistenceState,
    ) {
        let store = SharedStorage::new(rightkit_agent_engine_storage::Storage::open_in_memory().unwrap());
        let conduit = Arc::new(Conduit::new(16));
        let state = rightkit_agent_engine_storage::EventPersistenceState::new();
        spawn_event_persister(
            "evt003".into(),
            conduit.clone(),
            store.clone(),
            Arc::new(Mutex::new(HashSet::new())),
            state.clone(),
        );
        (store, conduit, state)
    }

    /// Publish durable events 2..=514 with seq 1 never arriving, so the
    /// persister's reorder buffer overflows and it must latch.
    fn overflow_reorder_bound(conduit: &Conduit) {
        for seq in 2..=(rightkit_agent_engine_core::conduit::MAX_HISTORY_EVENTS as i64 + 2) {
            conduit.publish(durable_event(seq));
        }
    }

    async fn wait_for_latch(conduit: &Conduit) -> String {
        for _ in 0..200 {
            if let Some(reason) = conduit.persistence_failure() {
                return reason;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("persister never latched");
    }

    #[tokio::test]
    async fn persister_latches_on_reorder_bound() {
        let (_store, conduit, state) = start_persister();
        overflow_reorder_bound(&conduit);
        let reason = wait_for_latch(&conduit).await;
        assert!(
            reason.contains("reorder bound"),
            "unexpected reason: {reason}"
        );
        assert!(state
            .snapshot()
            .last_error
            .is_some_and(|error| error.contains("reorder bound")));
        let refused = conduit.try_publish(durable_event(1)).expect_err("latched");
        assert!(refused.reason.contains("reorder bound"));
    }

    #[tokio::test]
    async fn persister_latches_on_invalid_envelope() {
        let (store, conduit, state) = start_persister();
        // Blank session id cannot form a durable envelope.
        conduit.publish(ProtocolEvent::UserMessageSubmitted {
            session_id: String::new(),
            seq: 1,
            created_at: now_iso(),
            text: "bad".into(),
            nonce: None,
            trust: None,
            attachments: Vec::new(),
        });
        let reason = wait_for_latch(&conduit).await;
        assert!(
            reason.contains("invalid durable event envelope"),
            "unexpected reason: {reason}"
        );
        assert!(state
            .snapshot()
            .last_error
            .is_some_and(|error| error.contains("invalid durable event envelope")));
        assert!(store
            .lock()
            .await
            .events_after("evt003", 0)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn persister_latch_cancels_in_flight_tool_with_typed_outcome() {
        let (_store, conduit, _state) = start_persister();
        let (interrupt_tx, mut interrupt_rx) = tokio::sync::watch::channel(false);
        let mut live = conduit.subscribe();
        let watcher = spawn_latch_cancel(&conduit, "evt003".into(), interrupt_tx);

        // A tool already running: it only ends when interrupted.
        let tool = tokio::spawn(async move {
            tokio::select! {
                _ = interrupt_rx.wait_for(|interrupted| *interrupted) => "cancelled",
                () = tokio::time::sleep(std::time::Duration::from_secs(30)) => "completed",
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !tool.is_finished(),
            "tool must be in flight before the latch"
        );

        overflow_reorder_bound(&conduit);
        let reason = wait_for_latch(&conduit).await;

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), watcher)
            .await
            .expect("watcher should fire on latch")
            .unwrap();
        assert_eq!(outcome, LatchCancelOutcome::InFlightCancelled { reason });
        let tool_result = tokio::time::timeout(std::time::Duration::from_secs(2), tool)
            .await
            .expect("in-flight tool should be cancelled")
            .unwrap();
        assert_eq!(tool_result, "cancelled");
        let live_event = tokio::time::timeout(std::time::Duration::from_secs(2), live.recv())
            .await
            .expect("latch should emit a live event");
        let rightkit_agent_engine_core::conduit::ConduitReceive::Event(event) = live_event else {
            panic!("expected live persistenceFailed event");
        };
        match event {
            rightkit_agent_engine_protocol::ProtocolEvent::PersistenceFailed {
                reason: live_reason,
                ..
            } => {
                assert!(!live_reason.is_empty());
            }
            other => panic!("expected PersistenceFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn latch_watcher_ends_quietly_when_session_drops_without_latch() {
        let conduit = Arc::new(Conduit::new(4));
        let (interrupt_tx, interrupt_rx) = tokio::sync::watch::channel(false);
        let watcher = spawn_latch_cancel(&conduit, "s".into(), interrupt_tx);
        drop(conduit);
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), watcher)
            .await
            .expect("watcher should end")
            .unwrap();
        assert_eq!(outcome, LatchCancelOutcome::SessionEnded);
        assert!(!*interrupt_rx.borrow());
    }

    #[tokio::test]
    async fn durable_fact_is_in_sqlite_before_live_visibility() {
        let (store, conduit, state) = start_persister();
        let mut live = conduit.subscribe();
        conduit.publish(durable_event(1));

        let received = tokio::time::timeout(std::time::Duration::from_secs(1), live.recv())
            .await
            .expect("durable fact should become visible after commit");
        let rightkit_agent_engine_core::conduit::ConduitReceive::Event(event) = received else {
            panic!("expected durable event");
        };
        assert_eq!(event.meta().seq, 1);

        let rows = store.lock().await.events_after("evt003", 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, 1);
        let snapshot = state.snapshot();
        assert!(snapshot.last_persisted_seq >= 1);
        assert!(snapshot.last_published_seq >= 1);
    }

    #[tokio::test]
    async fn concurrent_publication_preserves_cancel_facts_in_reserved_sequence_order() {
        let (store, conduit, state) = start_persister();
        let mut live = conduit.subscribe();
        // Force the real producer race: a watcher publishes its later
        // reservation before the engine publishes the preceding durable fact.
        let first = conduit.next_seq();
        let second = conduit.next_seq();
        conduit.publish(durable_event(second));
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(store
            .lock()
            .await
            .events_after("evt003", 0)
            .unwrap()
            .is_empty());
        assert!(state.snapshot().last_error.is_none());
        conduit.publish(durable_event(first));
        await_event_persistence(state.clone(), second)
            .await
            .unwrap();
        let rows = store.lock().await.events_after("evt003", 0).unwrap();
        assert_eq!(
            rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
            vec![first, second]
        );
        for expected in [first, second] {
            let received = tokio::time::timeout(std::time::Duration::from_secs(1), live.recv())
                .await
                .unwrap();
            let rightkit_agent_engine_core::conduit::ConduitReceive::Event(event) = received else {
                panic!("expected committed durable fact")
            };
            assert_eq!(event.meta().seq, expected);
        }
    }

    #[tokio::test]
    async fn failed_durable_append_never_becomes_live() {
        let (store, conduit, state) = start_persister();
        let mut live = conduit.subscribe();
        store
            .lock()
            .await
            .conn()
            .execute("DROP TABLE events", [])
            .expect("drop events table for fault injection");

        conduit.publish(durable_event(1));
        let persisted = await_event_persistence(state.clone(), 1)
            .await
            .expect_err("durable append must fail");
        assert!(!persisted.is_empty());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), live.recv())
                .await
                .is_err()
        );
        let snapshot = state.snapshot();
        assert_eq!(snapshot.last_persisted_seq, 0);
        assert_eq!(snapshot.last_published_seq, 0);
        assert!(snapshot.last_error.is_some());
    }

    #[tokio::test]
    async fn committed_row_survives_stop_between_commit_and_broadcast() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("evt003.sqlite");
        let store = SharedStorage::new(rightkit_agent_engine_storage::Storage::open(&db).unwrap());
        let conduit = Arc::new(Conduit::new(16));
        let state = rightkit_agent_engine_storage::EventPersistenceState::new();
        *EVT003_STOP_AFTER_PERSIST_SESSION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some("evt003-crash".into());
        spawn_event_persister(
            "evt003-crash".into(),
            conduit.clone(),
            store.clone(),
            Arc::new(Mutex::new(HashSet::new())),
            state.clone(),
        );
        let mut live = conduit.subscribe();
        conduit.publish(ProtocolEvent::UserMessageSubmitted {
            session_id: "evt003-crash".into(),
            seq: 1,
            created_at: now_iso(),
            text: "commit then stop".into(),
            nonce: None,
            trust: None,
            attachments: Vec::new(),
        });

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if state.snapshot().last_persisted_seq >= 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("durable append should commit before injected stop");
        assert_eq!(state.snapshot().last_published_seq, 0);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), live.recv())
                .await
                .is_err()
        );

        drop(store);
        let reopened = rightkit_agent_engine_storage::Storage::open(&db).unwrap();
        let rows = reopened.events_after("evt003-crash", 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, 1);
    }

    #[tokio::test]
    async fn live_projection_stays_immediate_while_shared_seq_persists_in_order() {
        let (store, conduit, state) = start_persister();
        let mut live = conduit.subscribe();
        conduit.publish(projection_event(1));
        let projected = tokio::time::timeout(std::time::Duration::from_millis(100), live.recv())
            .await
            .expect("projection must remain live");
        assert!(matches!(
            projected,
            rightkit_agent_engine_core::conduit::ConduitReceive::Event(_)
        ));

        // Projection events remain live-only and do not consume durable
        // sequence numbers. First durable fact therefore occupies sequence 1.
        conduit.publish(durable_event(1));
        await_event_persistence(state, 1)
            .await
            .expect("durable fact should persist and publish");
        let committed = live
            .try_recv()
            .expect("committed durable fact should be live");
        let event = committed;
        assert_eq!(event.meta().seq, 1);

        let rows = store.lock().await.events_after("evt003", 0).unwrap();
        assert_eq!(rows.iter().map(|row| row.seq).collect::<Vec<_>>(), vec![1]);
    }
}
