use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};

use rightkit_agent_engine_core::agents::is_website_task;
use rightkit_agent_engine_protocol::{ContentTrust, ProtocolEvent, TranscriptMessage};
use serde::Deserialize;

use super::rewind::strip_system_reminders;
use crate::api::{
    direct_message_engine_payload, dynamic_user_turn_reminder, lock_recover, now_iso, now_millis,
    require_license_feature, run_dispatched_session_entry, set_goal_status_field, touch_session,
    trust_envelope, DaemonState, EditGoal, PostMessage, SetGoal,
};
use crate::api_permissions::{enforce_trusted_source_authority, lower_source_trust};

// The mid-run steering queue (queue semantics only; see the module docs).
// Kept beside this file, not in the shared protocol or the LiveSession shape.
#[path = "steering.rs"]
mod steering;

const EVT005_QUALIFICATION_FAULT_ENV: &str = "RIGHTKIT_QUAL_EVT005_FAULT";
/// CQ-13: fault points only arm inside the qualification harness's isolated
/// daemon (`tests/support/daemon_process.rs` sets this for every spawned
/// child). An inherited point variable alone can no longer terminate an
/// ordinary build.
const QUALIFICATION_FAULTS_ARMED_ENV: &str = "RIGHTKIT_QUAL_FAULTS_ARMED";

fn evt005_qualification_fault(point: &str) {
    if std::env::var(QUALIFICATION_FAULTS_ARMED_ENV)
        .ok()
        .as_deref()
        != Some("1")
    {
        return;
    }
    if std::env::var(EVT005_QUALIFICATION_FAULT_ENV)
        .ok()
        .as_deref()
        == Some(point)
    {
        // Exit instead of panic so destructors cannot accidentally settle or
        // release the durable state whose crash window is under qualification.
        std::process::exit(86);
    }
}

pub(crate) async fn post_message(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<PostMessage>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if state.handoff_intake_is_fenced(&id) {
        let correlation_id = rightkit_agent_engine_protocol::uuid_v7();
        return Err(machine_error(
            StatusCode::CONFLICT,
            "handoff_controller_fenced",
            "session intake is fenced while root-controller handoff is in flight",
            &correlation_id,
            true,
            "none",
        ));
    }
    if state.get_session(&id).is_none() {
        // A dormant session (history in storage) is resumed with its context
        // seeded; a genuinely new id starts fresh.
        let workspace = body
            .workspace_root
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .unwrap_or("");
        if state
            .resume_session_with_ws(&id, body.workspace_root.as_deref())
            .await
            .is_none()
        {
            let role = if is_website_task(&body.text) {
                Some("designer")
            } else {
                None
            };
            state.create_session(&id, role, workspace, "New session", None);
        }
    }
    // Rebind an already-live session whose workspace_root was never set (e.g.
    // the persisted "Default session" auto-resumed before the first message).
    // Evict it so resume_session_with_ws rebuilds the engine against the real
    // workspace from this request.
    if let Some(existing) = state.get_session(&id) {
        let new_ws = body
            .workspace_root
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .unwrap_or("");
        if existing.workspace_root.is_empty() && !new_ws.is_empty() {
            if let Some(tx) = &existing.interrupt_tx {
                let _ = tx.send(true);
            }
            drop(existing);
            lock_recover(&state.sessions, "sessions").remove(&id);
            state
                .resume_session_with_ws(&id, body.workspace_root.as_deref())
                .await;
        }
    }
    let live = state.get_session(&id).ok_or_else(|| {
        machine_error(
            StatusCode::NOT_FOUND,
            "session_not_found",
            "session was not found",
            &rightkit_agent_engine_protocol::uuid_v7(),
            false,
            "none",
        )
    })?;

    if let Some(client_id) = body
        .client_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        crate::api_clients::supervisor_attach_session(client_id, &id);
    }

    let correlation_id = rightkit_agent_engine_protocol::uuid_v7();
    // A body nonce wins; otherwise the `Idempotency-Key` header is honoured so
    // every retryable ingress accepts the same key spellings.
    let idempotency =
        super::idempotency::resolve_nonce(body.nonce.as_deref(), &headers).map_err(|status| {
            machine_error(
                status,
                "idempotency_key_invalid",
                "idempotency key was not valid UTF-8 or exceeds 256 bytes",
                &correlation_id,
                false,
                "none",
            )
        })?;
    let request_digest = machine_message_digest(&body);
    let mut admitted_seq: Option<i64> = None;
    if let Some(key) = idempotency.as_deref() {
        let Some(storage) = &live.storage else {
            return Err(machine_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "idempotency_unavailable",
                "durable idempotency storage is unavailable",
                &correlation_id,
                true,
                "none",
            ));
        };
        let decision = storage
            .lock()
            .await
            .claim_idempotent_request_wire(
                &id,
                "post_message",
                key,
                &request_digest,
                &correlation_id,
                &now_iso(),
            )
            .map_err(|error| {
                machine_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "idempotency_storage_failed",
                    &error.to_string(),
                    &correlation_id,
                    true,
                    "none",
                )
            })?;
        match decision["decision"].as_str() {
            Some("fresh") => {}
            Some("replay") => {
                let original = decision["responseJson"]
                    .as_str()
                    .and_then(|value| serde_json::from_str(value).ok())
                    .ok_or_else(|| {
                        machine_error(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "idempotency_replay_corrupt",
                            "stored response could not be replayed",
                            decision["correlationId"]
                                .as_str()
                                .unwrap_or(&correlation_id),
                            false,
                            "unknown",
                        )
                    })?;
                return Ok(Json(original));
            }
            Some("conflict") => {
                return Err(machine_error(
                    StatusCode::CONFLICT,
                    "idempotency_key_conflict",
                    "idempotency key was already used with a different request digest",
                    decision["correlationId"]
                        .as_str()
                        .unwrap_or(&correlation_id),
                    false,
                    "none",
                ));
            }
            Some("pending")
                if decision["effectState"].as_str() == Some("admitted")
                    && decision["admissionSeq"].as_i64().is_some_and(|seq| seq > 0) =>
            {
                admitted_seq = decision["admissionSeq"].as_i64();
            }
            _ => {
                return Err(machine_error(
                    StatusCode::CONFLICT,
                    "idempotency_outcome_pending",
                    "the original request has no proven terminal outcome; reconcile before retrying",
                    decision["correlationId"]
                        .as_str()
                        .unwrap_or(&correlation_id),
                    true,
                    // Storage phases fence replay; they do not prove an effect outcome.
                    // Keep the durable phase intact and expose the public unknown state.
                    "unknown",
                ));
            }
        }
    }

    // Build the canonical content string for the engine. When the user attaches images,
    // encode the message as a JSON array of content parts (OpenAI multipart format) so
    // vision-capable models receive the images. Plain text messages stay as strings.
    let engine_content = match &body.attachments {
        Some(atts) if !atts.is_empty() => {
            let mut parts: Vec<serde_json::Value> = Vec::new();
            if !body.text.is_empty() {
                parts.push(serde_json::json!({ "type": "text", "text": &body.text }));
            }
            for att in atts {
                if att.attachment_type == "image" {
                    parts.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", att.mime_type, att.data)
                        }
                    }));
                }
            }
            serde_json::to_string(&parts).unwrap_or_else(|_| body.text.clone())
        }
        _ => body.text.clone(),
    };

    // EVT-005: the user turn enters the live transcript/provider path only
    // after its durable admission fact is committed. Identical retries may
    // adopt an already-admitted sequence, but never allocate a second event.
    let request_trust = lower_source_trust(&headers)
        .map_err(|status| {
            machine_error(
                status,
                "source_trust_invalid",
                "source trust headers were invalid",
                &correlation_id,
                false,
                "none",
            )
        })?
        .unwrap_or(ContentTrust::User);
    let request_source = match request_trust {
        ContentTrust::RemoteAgentUntrusted => "daemon.postMessage.remoteAgentUntrusted",
        ContentTrust::ToolUntrusted => "daemon.postMessage.toolUntrusted",
        _ => "daemon.postMessage",
    };
    let transcript_trust = trust_envelope(request_trust.clone(), request_source, &engine_content);
    let event_trust = trust_envelope(request_trust, request_source, &body.text);
    let user_created_at = now_iso();
    let user_seq = if let Some(seq) = admitted_seq {
        seq
    } else {
        let seq = live.conduit.next_seq();
        live.conduit.publish(ProtocolEvent::UserMessageSubmitted {
            session_id: id.clone(),
            seq,
            created_at: user_created_at.clone(),
            text: body.text.clone(),
            nonce: idempotency.clone(),
            trust: Some(event_trust),
            attachments: body.references.clone(),
        });
        if live.storage.is_some() {
            if let Err(error) =
                crate::api::await_event_persistence(live.persistence_state.clone(), seq).await
            {
                tracing::error!(session_id = %id, seq, %error, "user turn persistence failed");
                if let (Some(storage), Some(key)) = (&live.storage, idempotency.as_deref()) {
                    let _ = storage.lock().await.mark_idempotent_request_unknown(
                        &id,
                        "post_message",
                        key,
                        &request_digest,
                        &now_iso(),
                    );
                }
                return Err(machine_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "event_persistence_failed",
                    &error.to_string(),
                    &correlation_id,
                    true,
                    "unknown",
                ));
            }
        }
        seq
    };
    evt005_qualification_fault("after_admission_before_dispatch");
    let user_message_id = format!("user-{user_seq}");
    // The transcript guard must not be part of this future's state: holding a
    // std MutexGuard across the await below makes the handler non-Send and the
    // axum route fails to compile. A block scope guarantees it is released.
    {
        let mut transcript = lock_recover(&live.transcript, "session.transcript");
        if !transcript
            .iter()
            .any(|message| message.id() == user_message_id)
        {
            transcript.push(TranscriptMessage::user_with_attachments(
                user_message_id,
                user_created_at,
                engine_content.clone(),
                Some(transcript_trust.clone()),
                false,
                body.references.clone(),
            ));
        }
    }

    let memory_injection = if state.uses_direct_context() {
        None
    } else {
        crate::api::retrieve_session_memory(
            &state,
            &id,
            &live.workspace_root,
            &body.text,
            5,
            request_source,
        )
        .await
    };
    let reminder = dynamic_user_turn_reminder(
        memory_injection
            .as_ref()
            .map(|injection| injection.context.as_str()),
        &[],
    );
    let direct_engine_content =
        direct_message_engine_payload(&engine_content, &transcript_trust, &reminder);
    // PL-008 gap retention: once a handoff claim fences intake, preserve
    // arrivals under its epoch instead of dropping or duplicating them.
    if let Some(storage) = &live.storage {
        let storage = storage.clone();
        let gap_id = idempotency
            .clone()
            .unwrap_or_else(|| format!("gap-{}", now_iso()));
        let payload = serde_json::json!({"text": body.text.clone(), "attachments": body.attachments.is_some()}).to_string();
        let session_id = id.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let store = storage.blocking_lock();
            if let Ok(Some(handoff)) = store.handoff_state(&session_id) {
                if matches!(
                    handoff.phase.as_str(),
                    "quiescing" | "source_quiesced" | "resumed"
                ) {
                    let _ = store.retain_handoff_gap_message(
                        &rightkit_agent_engine_storage::HandoffGapMessage {
                            session_id,
                            controller_epoch: handoff.controller_epoch,
                            message_key: gap_id,
                            payload_json: payload,
                            delivered_at: None,
                            created_at: now_iso(),
                        },
                    );
                }
            }
        })
        .await;
    }
    if let Some(enabled) = body.sandbox_grant_prompts {
        live.sandbox_grant_prompts
            .store(enabled, std::sync::atomic::Ordering::Release);
    }
    let routing_enabled = require_license_feature(&state, "routing").is_ok();
    let classifier = state.dispatch_classifier();
    let dispatch_conduit = live.conduit.clone();
    let dispatch_session_id = id.clone();
    let direct_tx = live.msg_tx.clone();
    let direct_live = live.clone();
    let direct_conduit = live.conduit.clone();
    let direct_session_id = id.clone();
    let direct_storage = live.storage.clone();
    let direct_key = idempotency.clone();
    let direct_digest = request_digest.clone();
    let conduct_state = state.clone();
    let conduct_session = live.clone();
    let conduct_session_id = id.clone();
    let conduct_goal = body.text.clone();
    let conduct_storage = live.storage.clone();
    let conduct_key = idempotency.clone();
    let conduct_digest = request_digest.clone();
    let dispatch_result = run_dispatched_session_entry(
        routing_enabled,
        &body.text,
        classifier.as_deref(),
        move |decision| {
            let seq = dispatch_conduit.next_seq();
            dispatch_conduit.publish(ProtocolEvent::DispatchDecided {
                session_id: dispatch_session_id.clone(),
                seq,
                created_at: now_iso(),
                strategy: decision.strategy_label(),
                rationale: decision.rationale.clone(),
            });
        },
        move || async move {
            if let Some(tx) = direct_tx {
                if let (Some(storage), Some(key)) = (&direct_storage, direct_key.as_deref()) {
                    let fenced = storage.lock().await.mark_idempotent_request_dispatching(
                        &direct_session_id, "post_message", key, &direct_digest, &now_iso(),
                    ).map_err(|error| error.to_string())?;
                    if !fenced {
                        return Err("durable prompt admission is not dispatchable".to_string());
                    }
                    evt005_qualification_fault("after_dispatch_fence");
                }
                // An acknowledged stop applies to the preceding turn. Clear
                // its latched signal only when dispatching new work into an
                // idle engine; never clear cancellation of an active turn.
                if !direct_live.live_turn_handle.is_active() {
                    if let Some(interrupt) = &direct_live.interrupt_tx {
                        let _ = interrupt.send(false);
                    }
                }
                // Hold the route gate across the send so a route update cannot
                // commit between the busy check it performs and this turn
                // landing in the engine queue: the admitted prompt provably
                // runs on the route committed before it.
                let route_gate = direct_live.route_update_gate.lock().await;
                direct_live
                    .pending_admissions
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                if tx.try_send(direct_engine_content).is_err() {
                    direct_live
                        .pending_admissions
                        .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                    drop(route_gate);
                    if let (Some(storage), Some(key)) = (&direct_storage, direct_key.as_deref()) {
                        let _ = storage.lock().await.release_idempotent_request_dispatching(
                            &direct_session_id, "post_message", key, &direct_digest, &now_iso(),
                        );
                    }
                    return Err("session provider channel rejected admitted prompt before dispatch".to_string());
                }
                drop(route_gate);
            } else {
                let message = TranscriptMessage::Assistant {
                    id: format!("assistant-{}", now_millis()),
                    created_at: now_iso(),
                    content: crate::api::missing_provider_key_message(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    pinned: false, origin: None,
                };
                let seq = direct_conduit.next_seq();
                direct_conduit.publish(ProtocolEvent::AssistantMessageCompleted {
                    session_id: direct_session_id.clone(),
                    seq,
                    created_at: now_iso(),
                    message, phase: rightkit_agent_engine_protocol::AssistantMessagePhase::legacy_default(),
                });
            }
            Ok(())
        },
        move || async move {
            if let (Some(storage), Some(key)) = (&conduct_storage, conduct_key.as_deref()) {
                let fenced = storage.lock().await.mark_idempotent_request_dispatching(
                    &conduct_session_id, "post_message", key, &conduct_digest, &now_iso(),
                ).map_err(|error| error.to_string())?;
                if !fenced {
                    return Err("durable prompt admission is not dispatchable".to_string());
                }
                evt005_qualification_fault("after_dispatch_fence");
            }
            let settle_storage = conduct_storage.clone();
            let settle_key = conduct_key.clone();
            let settle_digest = conduct_digest.clone();
            let settle_session_id = conduct_session_id.clone();
            tokio::spawn(async move {
                if let Err(e) = conduct_state
                    .run_conductor(&conduct_session_id, &conduct_goal, false)
                    .await
                {
                    tracing::error!(session_id = %conduct_session_id, error = %e, "auto-dispatch conductor error");
                    // EVT-005/EVT-006: the fence was raised before this task
                    // started, and nothing here resolved it. Left as-is, the
                    // row stays `dispatching` forever, so every later retry of
                    // this idempotency key is refused as "not dispatchable" —
                    // a permanent lockout even when the prompt never reached a
                    // provider at all.
                    //
                    // It is settled as `unknown`, NOT released to `admitted`:
                    // the conductor may already have dispatched to a provider
                    // before failing, and releasing would authorise a
                    // duplicate non-idempotent send. `unknown` is the honest
                    // state, and the retry path surfaces it as pending/unknown
                    // for reconciliation rather than silently replaying.
                    if let (Some(storage), Some(key)) =
                        (&settle_storage, settle_key.as_deref())
                    {
                        if let Err(error) = storage
                            .lock()
                            .await
                            .mark_idempotent_request_unknown(
                                &settle_session_id,
                                "post_message",
                                key,
                                &settle_digest,
                                &now_iso(),
                            )
                        {
                            tracing::error!(
                                session_id = %settle_session_id,
                                error = %error,
                                "failed to settle interrupted auto-dispatch admission as unknown"
                            );
                        }
                    }
                    let seq = conduct_session.conduit.next_seq();
                    conduct_session.conduit.publish(ProtocolEvent::RunFailed {
                        session_id: conduct_session_id.clone(),
                        seq,
                        created_at: now_iso(),
                        error: e,
                        kind: "orchestration".into(),
                        hint: "Check that the conductor's model and provider key are configured."
                            .into(),
                    });
                }
            });
            Ok(())
        },
    )
    .await;
    if let Err(error) = dispatch_result {
        let seq = live.conduit.next_seq();
        live.conduit.publish(ProtocolEvent::RunFailed {
            session_id: id.clone(),
            seq,
            created_at: now_iso(),
            error: error.clone(),
            kind: "dispatch".into(),
            hint: "The admitted prompt did not cross the provider-dispatch boundary.".into(),
        });
        return Err(machine_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "dispatch_failed",
            &error,
            &correlation_id,
            true,
            "unknown",
        ));
    }

    let response = serde_json::json!({
        "schemaVersion": crate::host_identity::schema("machine.v1"),
        "sessionId": id,
        "requestId": idempotency,
        "correlationId": correlation_id,
        "effectState": "committed",
        "accepted": true,
        "seq": user_seq
    });
    if let (Some(storage), Some(key)) = (&live.storage, idempotency.as_deref()) {
        let response_json = serde_json::to_string(&response).unwrap_or_default();
        let committed = storage
            .lock()
            .await
            .complete_idempotent_request(
                &id,
                "post_message",
                key,
                &request_digest,
                &response_json,
                &now_iso(),
            )
            .unwrap_or(false);
        if !committed {
            return Err(machine_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "idempotency_commit_failed",
                "message committed but its replay receipt could not be persisted",
                &correlation_id,
                true,
                "unknown",
            ));
        }
    }
    Ok(Json(response))
}

fn machine_message_digest(body: &PostMessage) -> String {
    let attachments = body
        .attachments
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|attachment| {
            serde_json::json!({
                "type": attachment.attachment_type,
                "mimeType": attachment.mime_type,
                "data": attachment.data,
            })
        })
        .collect::<Vec<_>>();
    let canonical = serde_json::json!({
        "text": body.text,
        "attachments": attachments,
        "references": body.references,
        "workspaceRoot": body.workspace_root,
        "sandboxGrantPrompts": body.sandbox_grant_prompts,
    });
    format!(
        "sha256:{}",
        rightkit_fs::digest::sha256_bytes(canonical.to_string().as_bytes())
    )
}

fn machine_error(
    status: StatusCode,
    code: &str,
    detail: &str,
    correlation_id: &str,
    retryable: bool,
    effect_state: &str,
) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(serde_json::json!({
            "schemaVersion": crate::host_identity::schema("error.v1"),
            "code": code,
            "detail": detail,
            "correlationId": correlation_id,
            "retryable": retryable,
            "effectState": effect_state,
            "remediation": match code {
                "idempotency_outcome_pending" | "event_persistence_failed" =>
                    Some("reconcile by idempotency key before retrying"),
                _ => None,
            },
        })),
    )
}

// FM-124: routes edit / reorder / send-now (and list/enqueue/remove)
// controls for the mid-run steering queue onto `steering::SteeringQueue`
// (engine/crates/engine/src/steering.rs), which owns the actual queue
// semantics and rejection rules (e.g. an already-dispatched item can no
// longer be edited/reordered/removed).
pub(crate) async fn steer_session(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    // CR3 #57: an enqueue/edit/reorder/remove/send-now retry must not apply
    // twice. `list` is a read and never claims a key.
    let is_list = body.get("action").and_then(serde_json::Value::as_str) == Some("list");
    let nonce = if is_list {
        None
    } else {
        super::idempotency::normalize_nonce(body.get("nonce").and_then(serde_json::Value::as_str))?
    };
    let mut canonical = body.clone();
    if let Some(object) = canonical.as_object_mut() {
        object.remove("nonce");
    }
    let digest = super::idempotency::request_digest("steer_session", &canonical);
    let storage = state
        .get_session(&id)
        .and_then(|live| live.storage.clone())
        .or_else(|| state.storage.clone());
    let scope_id = id.clone();
    super::idempotency::run_idempotent(storage, &scope_id, "steer_session", nonce, digest, || {
        steer_session_once(state, id, headers, body)
    })
    .await
}

async fn steer_session_once(
    state: DaemonState,
    id: String,
    headers: HeaderMap,
    body: serde_json::Value,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if state.handoff_intake_is_fenced(&id) {
        return Err(StatusCode::CONFLICT);
    }
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    let action = body
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("enqueue");
    if action == "list" {
        let items = steering::with_session(&id, |queue| queue.list());
        return Ok(Json(serde_json::json!({ "queued": items })));
    }
    let item_id = body.get("id").and_then(serde_json::Value::as_str);
    if matches!(
        action,
        "edit" | "reorder" | "remove" | "sendNow" | "send_now"
    ) {
        let Some(item_id) = item_id else {
            return Err(StatusCode::BAD_REQUEST);
        };
        if action == "edit" {
            let text = body
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or(StatusCode::BAD_REQUEST)?;
            let item = steering::with_session(&id, |queue| queue.edit(item_id, text.to_owned()))
                .map_err(|_| StatusCode::CONFLICT)?;
            return Ok(Json(serde_json::json!({ "edited": true, "item": item })));
        }
        if action == "reorder" {
            let position = body
                .get("position")
                .and_then(serde_json::Value::as_u64)
                .ok_or(StatusCode::BAD_REQUEST)? as usize;
            steering::with_session(&id, |queue| queue.reorder(item_id, position))
                .map_err(|_| StatusCode::CONFLICT)?;
            return Ok(Json(serde_json::json!({ "reordered": true })));
        }
        if action == "remove" {
            steering::with_session(&id, |queue| queue.remove(item_id))
                .map_err(|_| StatusCode::CONFLICT)?;
            return Ok(Json(serde_json::json!({ "removed": true })));
        }
        let Some(tx) = &live.steer_tx else {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        };
        // Peek before sending, but commit the queue state only after the
        // bounded channel accepts the text. A saturated/closed consumer must
        // leave the item retryable rather than reporting it as dispatched.
        let item = steering::with_session(&id, |queue| queue.pending(item_id))
            .map_err(|_| StatusCode::CONFLICT)?;
        tx.try_send(item.text.clone())
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let item = steering::with_session(&id, |queue| queue.mark_dispatched(item_id))
            .map_err(|_| StatusCode::CONFLICT)?;
        touch_session(&live);
        return Ok(Json(serde_json::json!({ "sentNow": true, "item": item })));
    }
    let text = body
        .get("text")
        .and_then(serde_json::Value::as_str)
        .ok_or(StatusCode::BAD_REQUEST)?
        .to_owned();
    let item =
        steering::with_session(&id, |queue| queue.enqueue(text)).map_err(|error| match error {
            steering::QueueError::Full => StatusCode::TOO_MANY_REQUESTS,
            _ => StatusCode::BAD_REQUEST,
        })?;
    match &live.steer_tx {
        Some(tx) => {
            touch_session(&live);
            // A closed or saturated live channel means no turn is currently
            // consuming steer input; the item stays queued for later dispatch
            // rather than failing the request. Only mark it dispatched when it
            // actually reached the live consumer.
            let item = if tx.try_send(item.text.clone()).is_ok() {
                // Return the post-send queue state. Previously the item was
                // marked dispatched but the response returned its stale
                // pre-dispatch snapshot, making UI retry/reorder decisions
                // disagree with live engine state.
                steering::with_session(&id, |queue| queue.mark_dispatched(&item.id))
                    .map_err(|_| StatusCode::CONFLICT)?
            } else {
                item
            };
            Ok(Json(serde_json::json!({ "steered": true, "queued": item })))
        }
        None => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}
pub(crate) async fn set_goal(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SetGoal>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    // CR3 #57: a retried set must not mint a second goal id and reset budgets.
    let nonce = super::idempotency::normalize_nonce(body.nonce.as_deref())?;
    let digest = super::idempotency::request_digest(
        "set_goal",
        &serde_json::json!({
            "text": body.text,
            "budgetTokens": body.budget_tokens,
            "budgetUsd": body.budget_usd,
        }),
    );
    let storage = state
        .get_session(&id)
        .and_then(|live| live.storage.clone())
        .or_else(|| state.storage.clone());
    let scope_id = id.clone();
    super::idempotency::run_idempotent(storage, &scope_id, "set_goal", nonce, digest, || {
        set_goal_once(state, id, headers, body)
    })
    .await
}

async fn set_goal_once(
    state: DaemonState,
    id: String,
    headers: HeaderMap,
    body: SetGoal,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    touch_session(&live);
    *lock_recover(&live.goal, "session.goal") = Some(body.text.clone());

    // F2: create the budget-bounded goal state shared with the engine + persist.
    let goal_id = format!("goal-{}", now_millis());
    let started_at = now_iso();
    let budget_tokens = body.budget_tokens.max(0);
    let budget_usd = body.budget_usd.max(0.0);
    if let Some(storage) = &live.storage {
        let store = storage.lock().await;
        if let Ok(Some(prior)) = store.newest_displayable_goal_for_session(&id) {
            let _ = store.dismiss_goal(&prior.id, &now_iso());
        }
    }
    *lock_recover(&live.goal_state, "session.goal_state") =
        Some(rightkit_agent_engine_core::GoalState {
            id: goal_id.clone(),
            text: body.text.clone(),
            status: "active".into(),
            budget_usd,
            budget_tokens,
            tokens_used: 0,
            cost_usd: 0.0,
            continuation_count: 0,
            no_progress_count: 0,
            gds_score: 0.0,
            verification_present: false,
            verification_approved: false,
            started_at: started_at.clone(),
            closed_at: None,
            dismissed_at: None,
            decomposition_state: "missing".into(),
            plan_revision: 0,
            now: None,
            next: None,
            steps: Vec::new(),
            verification_state: "unverified".into(),
        });
    lock_recover(&live.goal_gap_tracker, "session.goal_gap_tracker").reset();
    if let Some(storage) = &live.storage {
        let row = rightkit_agent_engine_storage::GoalRow {
            id: goal_id.clone(),
            session_id: id.clone(),
            text: body.text.clone(),
            status: rightkit_agent_engine_storage::GoalStatus::Active,
            budget_usd,
            budget_tokens,
            tokens_used: 0,
            continuation_count: 0,
            no_progress_count: 0,
            started_at,
            closed_at: None,
            dismissed_at: None,
            decomposition_state: "missing".into(),
            plan_revision: 0,
            now: None,
            next: None,
            steps: Vec::new(),
            verification_state: "unverified".into(),
        };
        let _ = storage.lock().await.upsert_goal(&row);
    }
    Ok(Json(
        serde_json::json!({ "set": true, "goal": body.text, "goalId": goal_id }),
    ))
}
pub(crate) async fn get_goal_snapshot(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let live = state.get_session(&id);
    if let Some(live) = live.as_ref() {
        if let Some(goal) = lock_recover(&live.goal_state, "session.goal_state").as_ref() {
            return Ok(Json(serde_json::json!({"goal": goal.snapshot()})));
        }
    }
    let storage = live
        .as_ref()
        .and_then(|session| session.storage.clone())
        .or_else(|| state.storage.clone());
    let Some(storage) = storage else {
        return live
            .is_some()
            .then(|| Json(serde_json::json!({"goal": serde_json::Value::Null})))
            .ok_or(StatusCode::NOT_FOUND);
    };
    let store = storage.lock().await;
    if live.is_none()
        && store
            .get_session(&id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .is_none()
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let stored = store
        .newest_displayable_goal_for_session(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let goal = stored
        .map(crate::api::goal_state_from_row)
        .map(|goal| goal.snapshot());
    Ok(Json(serde_json::json!({"goal": goal})))
}
pub(crate) async fn clear_goal(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    touch_session(&live);
    *lock_recover(&live.goal, "session.goal") = None;
    // F2: drop the active goal state + mark any persisted goal unmet/closed.
    let closed_goal = lock_recover(&live.goal_state, "session.goal_state")
        .take()
        .map(|gs| {
            let terminal = gs.closed_at.is_some();
            (gs.id, terminal)
        });
    lock_recover(&live.goal_gap_tracker, "session.goal_gap_tracker").reset();
    if let Some(storage) = &live.storage {
        let dismissed_at = now_iso();
        let store = storage.lock().await;
        if let Some((goal_id, terminal)) = closed_goal {
            if !terminal {
                let _ = store.update_goal_status(
                    &goal_id,
                    rightkit_agent_engine_storage::GoalStatus::Unmet,
                    Some(&dismissed_at),
                );
            }
            let _ = store.dismiss_goal(&goal_id, &dismissed_at);
        } else if let Ok(Some(goal)) = store.newest_displayable_goal_for_session(&id) {
            let _ = store.dismiss_goal(&goal.id, &dismissed_at);
        }
    }
    Ok(Json(serde_json::json!({ "cleared": true })))
}
pub(crate) async fn pause_goal(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    touch_session(&live);
    if let Some(client_id) = headers
        .get(crate::host_identity::header("client-id"))
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
    {
        let _ = crate::api_clients::supervisor_pause_session(client_id, &id);
    }
    let ok = set_goal_status_field(&live, "paused");
    Ok(Json(serde_json::json!({ "paused": ok })))
}
pub(crate) async fn resume_goal(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if let Some(client_id) = headers
        .get(crate::host_identity::header("client-id"))
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
    {
        crate::api_clients::supervisor_resume_session(client_id, &id);
    }
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    touch_session(&live);
    let ok = set_goal_status_field(&live, "active");
    let goal_id = if ok {
        lock_recover(&live.goal_state, "session.goal_state")
            .as_ref()
            .map(|goal| goal.id.clone())
    } else {
        None
    };
    if ok {
        // A deliberate resume starts a fresh identical-gap audit rather than
        // immediately re-pausing on the next unchanged verifier result.
        lock_recover(&live.goal_gap_tracker, "session.goal_gap_tracker").reset();
    }
    if let (Some(storage), Some(goal_id)) = (&live.storage, goal_id) {
        let _ = storage.lock().await.update_goal_status(
            &goal_id,
            rightkit_agent_engine_storage::GoalStatus::Active,
            None,
        );
    }
    Ok(Json(serde_json::json!({ "resumed": ok })))
}
pub(crate) async fn edit_goal(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<EditGoal>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    touch_session(&live);
    let mut edited = false;
    let mut goal_changed = false;
    let mut persisted_goal = None;
    if let Some(goal) = lock_recover(&live.goal_state, "session.goal_state").as_mut() {
        if let Some(text) = &body.text {
            goal_changed |= goal.text != *text;
            goal.text = text.clone();
            *lock_recover(&live.goal, "session.goal") = Some(text.clone());
        }
        if let Some(bt) = body.budget_tokens {
            let budget_tokens = bt.max(0);
            goal_changed |= goal.budget_tokens != budget_tokens;
            goal.budget_tokens = budget_tokens;
        }
        if let Some(bu) = body.budget_usd {
            let budget_usd = bu.max(0.0);
            goal_changed |= goal.budget_usd != budget_usd;
            goal.budget_usd = budget_usd;
        }
        edited = true;
        if goal_changed {
            persisted_goal = Some((
                goal.id.clone(),
                goal.text.clone(),
                goal.budget_tokens,
                goal.budget_usd,
            ));
        }
    }
    if goal_changed {
        lock_recover(&live.goal_gap_tracker, "session.goal_gap_tracker").reset();
    }
    if let (Some(storage), Some((goal_id, text, budget_tokens, budget_usd))) =
        (&live.storage, persisted_goal)
    {
        let store = storage.lock().await;
        let mut row = store
            .get_goal(&goal_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
        row.text = text;
        row.budget_tokens = budget_tokens;
        row.budget_usd = budget_usd;
        store
            .upsert_goal(&row)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(Json(serde_json::json!({ "edited": edited })))
}
pub(crate) async fn rewind_session(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Option<Json<RewindSessionRequest>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    // CR3 #57: each rewind removes another user turn, so a retry must replay
    // the first result rather than rewinding a second time.
    let nonce = super::idempotency::normalize_nonce(
        body.as_ref()
            .and_then(|Json(request)| request.nonce.as_deref()),
    )?;
    let canonical = match body.as_ref() {
        Some(Json(request)) => serde_json::json!({
            "point": request.point.as_ref().map(|point| serde_json::json!({
                "text": point.text,
                "nthFromEnd": point.nth_from_end,
                "createdAt": point.created_at,
            })),
            "mode": request.mode,
        }),
        None => serde_json::Value::Null,
    };
    let digest = super::idempotency::request_digest("rewind_session", &canonical);
    let storage = state
        .get_session(&id)
        .and_then(|live| live.storage.clone())
        .or_else(|| state.storage.clone());
    let scope_id = id.clone();
    super::idempotency::run_idempotent(storage, &scope_id, "rewind_session", nonce, digest, || {
        rewind_session_once(state, id, headers, body)
    })
    .await
}

async fn rewind_session_once(
    state: DaemonState,
    id: String,
    headers: HeaderMap,
    body: Option<Json<RewindSessionRequest>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    if let Some(Json(request)) = body {
        if let Some(point) = request.point {
            let mode = match request.mode.as_deref().unwrap_or("conversation") {
                "conversation" => super::rewind::RewindMode::Conversation,
                "files" => super::rewind::RewindMode::Files,
                "both" => super::rewind::RewindMode::Both,
                _ => return Err(StatusCode::BAD_REQUEST),
            };
            let point = super::rewind::RewindPoint {
                text: point.text,
                nth_from_end: point.nth_from_end,
                created_at: point.created_at,
            };
            let report = super::rewind::rewind_to_message(&state, &id, &point, mode)
                .await
                .map_err(|error| {
                    tracing::info!(session_id = %id, %error, "rewind request refused");
                    StatusCode::CONFLICT
                })?;
            return Ok(Json(serde_json::json!({
                "rewound": report.conversation_rewound,
                "text": report.text,
                "conversationRewound": report.conversation_rewound,
                "filesRestored": report.files_restored,
                "fileConflicts": report.file_conflicts.into_iter().map(|(path, reason)| {
                    serde_json::json!({"path": path, "reason": reason})
                }).collect::<Vec<_>>(),
            })));
        }
        return Err(StatusCode::BAD_REQUEST);
    }
    let live = state.get_session(&id).ok_or(StatusCode::NOT_FOUND)?;
    touch_session(&live);

    // Interrupt any running engine first.
    if let Some(tx) = &live.interrupt_tx {
        let _ = tx.send(true);
    }

    // Send rewind command and wait for engine to respond.
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<Option<String>>();
    live.rewind_tx
        .send(super::rewind::RewindRequest::last_user(resp_tx))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    // Host-injected <system-reminder> blocks belong to the provider request only;
    // a client restoring the composer must see exactly what the user typed.
    let removed_text = resp_rx
        .await
        .unwrap_or(None)
        .map(|text| strip_system_reminders(&text).trim().to_string());

    // Reset interrupt so the session can continue.
    if let Some(tx) = &live.interrupt_tx {
        let _ = tx.send(false);
    }

    // Trim the shared transcript to match.
    if removed_text.is_some() {
        let mut t = lock_recover(&live.transcript, "session.transcript");
        if let Some(pos) = t
            .iter()
            .rposition(|m| matches!(m, TranscriptMessage::User { .. }))
        {
            t.truncate(pos);
        }
    }

    Ok(Json(
        serde_json::json!({ "rewound": true, "text": removed_text }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RewindPointRequest {
    pub text: String,
    pub nth_from_end: usize,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RewindSessionRequest {
    pub point: Option<RewindPointRequest>,
    pub mode: Option<String>,
    /// Durable idempotency key (CR3 #57).
    #[serde(default)]
    pub nonce: Option<String>,
}

pub(crate) async fn rewind_points_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let points = super::rewind::rewind_points(&state, &id).map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!(points
        .into_iter()
        .map(|point| {
            serde_json::json!({
                "text": point.text,
                "nthFromEnd": point.nth_from_end,
                "createdAt": point.created_at,
            })
        })
        .collect::<Vec<_>>())))
}
