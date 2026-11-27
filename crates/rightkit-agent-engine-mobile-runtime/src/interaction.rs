//! On-device operator bridge for `ask_user` and `exit_plan_mode` (ENG-29).
//!
//! Mirrors a daemon's questioner and plan-exit handler, minus daemon state.
//! Each call publishes a protocol event on the session conduit, parks a oneshot
//! sender in a pending map, and waits for the host to answer through
//! `MobileRuntime::resolve_question` / `resolve_plan` (the C ABI in `ffi.rs`).
//!
//! Wired only when the host turned on
//! [`crate::host_options::set_interactive_tools`]: a host that never answers
//! must not be offered a tool that waits for it.
//!
//! # Waiting rules
//!
//! * There is no timeout: the engine's own approval wait has none either, and
//!   a person may take the phone out of a pocket hours later.
//! * Every wait also watches the turn interrupt. An interrupted turn removes
//!   its pending id from the store, so Swift can never resolve a stale id, and
//!   the tool future ends (the FFI handle's runtime blocks on drop until it
//!   does).
//! * Resolving never takes the engine mutex (the in-flight `submit` holds it
//!   while waiting for this very answer); it only touches the pending maps.
//!
//! # Authority
//!
//! An approved plan leaves Plan mode (to accept-edits) only when the session is
//! in Plan mode at that moment: a model that calls `exit_plan_mode` in any other
//! mode cannot raise the session's permission by getting a plan approved.

use rightkit_agent_engine_core::conduit::Conduit;
use rightkit_agent_engine_protocol::{now_iso, ProtocolEvent};
use rightkit_agent_policy::PermissionMode;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

pub(crate) type PendingQuestions =
    Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>>;
pub(crate) type PendingPlans = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>;

static SEQ: AtomicU64 = AtomicU64::new(0);

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Resolves once the turn interrupt flag is `true`. If the sender is dropped
/// without ever firing, it never resolves (the answer branch decides).
async fn wait_interrupt(mut rx: tokio::sync::watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await
        }
    }
}

pub(crate) struct MobileQuestioner {
    pub session_id: String,
    pub conduit: Arc<Conduit>,
    pub store: PendingQuestions,
    pub interrupt: tokio::sync::watch::Receiver<bool>,
}

impl rightkit_agent_coding_tools::UserQuestioner for MobileQuestioner {
    fn supports_sandbox_grants(&self) -> bool {
        false
    }

    fn ask(
        &self,
        questions: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send>> {
        let session_id = self.session_id.clone();
        let conduit = self.conduit.clone();
        let store = self.store.clone();
        let interrupt = self.interrupt.clone();
        Box::pin(async move {
            let parsed: Vec<rightkit_agent_engine_protocol::UserQuestion> =
                serde_json::from_value(questions).map_err(|e| format!("invalid questions: {e}"))?;
            let qid = format!("q-{}-{}", now_millis(), SEQ.fetch_add(1, Ordering::Relaxed));
            let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
            store
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(qid.clone(), tx);

            let sid = session_id.clone();
            let asked_id = qid.clone();
            conduit.emit(|seq| ProtocolEvent::UserQuestionAsked {
                session_id: sid,
                seq,
                created_at: now_iso(),
                question_id: asked_id,
                questions: parsed,
            });

            let outcome = tokio::select! {
                answer = rx => Some(answer),
                () = wait_interrupt(interrupt) => None,
            };
            let answer = match outcome {
                Some(Ok(value)) => value,
                Some(Err(_)) => return Err("question was cancelled".to_string()),
                None => {
                    store
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&qid);
                    return Err("question cancelled: turn interrupted".to_string());
                }
            };

            conduit.emit(|seq| ProtocolEvent::UserQuestionAnswered {
                session_id,
                seq,
                created_at: now_iso(),
                question_id: qid,
            });
            Ok(answer)
        })
    }
}

pub(crate) struct MobilePlanExit {
    pub session_id: String,
    pub conduit: Arc<Conduit>,
    pub store: PendingPlans,
    pub mode_tx: tokio::sync::watch::Sender<PermissionMode>,
    pub interrupt: tokio::sync::watch::Receiver<bool>,
}

impl rightkit_agent_coding_tools::PlanExit for MobilePlanExit {
    fn present(&self, plan: String) -> Pin<Box<dyn Future<Output = Result<bool, String>> + Send>> {
        let session_id = self.session_id.clone();
        let conduit = self.conduit.clone();
        let store = self.store.clone();
        let mode_tx = self.mode_tx.clone();
        let interrupt = self.interrupt.clone();
        Box::pin(async move {
            let plan_id = format!(
                "plan-{}-{}",
                now_millis(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            );
            let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
            store
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(plan_id.clone(), tx);

            let presented_id = plan_id.clone();
            conduit.emit(|seq| ProtocolEvent::PlanPresented {
                session_id,
                seq,
                created_at: now_iso(),
                plan_id: presented_id,
                plan,
            });

            let outcome = tokio::select! {
                decision = rx => Some(decision),
                () = wait_interrupt(interrupt) => None,
            };
            let approved = match outcome {
                Some(Ok(value)) => value,
                Some(Err(_)) => return Err("plan approval was cancelled".to_string()),
                None => {
                    store
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&plan_id);
                    return Err("plan approval cancelled: turn interrupted".to_string());
                }
            };
            if approved {
                // Leave Plan mode so the agent can start making changes, and only from Plan
                // mode: an approval never changes the mode of a session that is not in it.
                mode_tx.send_modify(|mode| {
                    if *mode == PermissionMode::Plan {
                        *mode = PermissionMode::AcceptEdits;
                    }
                });
            }
            Ok(approved)
        })
    }
}

/// `true` when `v` is a blank string or an object any of whose values is a blank string.
pub(crate) fn answers_have_blank_text(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::String(text) => text.trim().is_empty(),
        serde_json::Value::Object(map) => map
            .values()
            .any(|value| value.as_str().is_some_and(|text| text.trim().is_empty())),
        _ => false,
    }
}
