//! Session handlers — all families (Tasks A–E): lifecycle, message/steering, SSE,
//! approvals/compaction, specs/orchestration/agents/taskgraph/tasks.
//! Extracted from api.rs (2026-06-21) using the established slice pattern:
//! a `sessions_routes()` free function merged into `build_router`. Behaviour
//! is unchanged — handler bodies moved verbatim into bounded modules under
//! `api_sessions/**`. Built incrementally:
//! Task A (lifecycle) → B (messages) → C (SSE) → D (approvals/compaction) → E (specs).

pub mod approvals;
pub mod credentials;
pub mod elicitation;
pub mod export;
mod idempotency;
pub mod lifecycle;
pub mod messages;
pub mod orchestration;
pub mod rewind;
pub mod router;
pub mod sse;
pub mod state;
pub mod validation;

#[path = "blueprint_init.rs"]
pub(crate) mod blueprint_init;

pub(crate) use router::sessions_routes;

// Re-export handler types for external callers that imported via `api_sessions::*`

#[cfg(test)]
mod tests {
    use super::lifecycle::get_transcript;
    use super::messages::set_goal;
    use super::orchestration::{get_artifacts, get_session_agents, get_taskgraph, get_tasks};
    use crate::api::lock_recover;
    use crate::api::{DaemonState, LiveSession, SessionQuery, SetGoal};
    use axum::{
        extract::{Path, Query, State},
        http::HeaderMap,
        response::Json,
    };
    use std::sync::{Arc, Mutex};

    fn state_with_live_session(id: &str) -> (DaemonState, Arc<LiveSession>) {
        let state = DaemonState::new("test-token-deadbeef");
        let live = state.create_session(id, None, "/tmp", "Test", None);
        (state, live)
    }

    fn poison_mutex<T: Send + 'static>(mutex: Arc<Mutex<T>>) {
        let _ = std::thread::spawn(move || {
            let _guard = mutex.lock().unwrap();
            panic!("poison mutex");
        })
        .join();
    }

    fn poison_agent_roster(live: &Arc<LiveSession>) {
        let live = live.clone();
        let _ = std::thread::spawn(move || {
            let _guard = live.agent_roster.lock().unwrap();
            panic!("poison agent_roster");
        })
        .join();
    }

    #[tokio::test]
    async fn get_transcript_recovers_from_poisoned_live_transcript_mutex() {
        let (state, live) = state_with_live_session("transcript-poison");
        poison_mutex(live.transcript.clone());

        let Json(rows) = get_transcript(State(state), Path("transcript-poison".to_string()))
            .await
            .expect("live transcript route should recover poisoned mutex");

        assert_eq!(rows.len(), 0);
    }

    #[tokio::test]
    async fn set_goal_recovers_from_poisoned_goal_mutexes() {
        let (state, live) = state_with_live_session("goal-poison");
        poison_mutex(live.goal.clone());
        poison_mutex(live.goal_state.clone());

        let response = set_goal(
            State(state),
            Path("goal-poison".to_string()),
            HeaderMap::new(),
            Json(SetGoal {
                text: "ship the phase".to_string(),
                budget_tokens: 42,
                budget_usd: 1.5,
                nonce: None,
            }),
        )
        .await
        .expect("set_goal should recover poisoned mutexes");

        assert_eq!(response.0["set"], true);
        assert_eq!(
            lock_recover(&live.goal, "session.goal").as_deref(),
            Some("ship the phase")
        );
        assert_eq!(
            lock_recover(&live.goal_state, "session.goal_state")
                .as_ref()
                .expect("goal state should be restored")
                .budget_tokens,
            42
        );
    }

    #[tokio::test]
    async fn runtime_read_handlers_recover_from_poisoned_live_mutexes() {
        let (state, live) = state_with_live_session("runtime-poison");
        poison_mutex(live.tasks.clone());
        poison_mutex(live.artifacts.clone());
        poison_agent_roster(&live);

        let Json(task_rows) = get_tasks(
            State(state.clone()),
            Query(SessionQuery {
                session_id: Some("runtime-poison".to_string()),
                workspace_root: String::new(),
            }),
        )
        .await;
        assert_eq!(task_rows.as_array().map(Vec::len), Some(0));

        let Json(artifact_rows) =
            get_artifacts(State(state.clone()), Path("runtime-poison".to_string()))
                .await
                .expect("artifact route should recover poisoned mutex");
        assert_eq!(artifact_rows.as_array().map(Vec::len), Some(0));

        let Json(taskgraph_rows) =
            get_taskgraph(State(state.clone()), Path("runtime-poison".to_string()))
                .await
                .expect("taskgraph route should recover poisoned mutex");
        assert_eq!(taskgraph_rows.as_array().map(Vec::len), Some(0));

        let Json(agent_rows) = get_session_agents(State(state), Path("runtime-poison".to_string()))
            .await
            .expect("agent roster route should recover poisoned mutex");
        assert_eq!(agent_rows.as_array().map(Vec::len), Some(0));
    }
}
