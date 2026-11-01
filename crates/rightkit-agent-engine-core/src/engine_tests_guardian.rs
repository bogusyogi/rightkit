//! Tests for the optional guardian LLM approval layer (FEATURE-MATRIX §#87).
use super::*;
use crate::guardian::{GuardianRequest, GuardianReviewer, GuardianVerdict};
use crate::llm::{LlmResponse, ScriptedLlm};
use std::sync::atomic::{AtomicUsize, Ordering};

struct MockGuardian {
    verdict: GuardianVerdict,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl GuardianReviewer for MockGuardian {
    async fn review(&self, request: &GuardianRequest) -> Result<GuardianVerdict, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.verdict == GuardianVerdict::Allow {
            return Ok(request.correlated_allow());
        }
        Ok(self.verdict.clone())
    }
}

struct ErrorGuardian;

#[async_trait::async_trait]
impl GuardianReviewer for ErrorGuardian {
    async fn review(&self, _request: &GuardianRequest) -> Result<GuardianVerdict, String> {
        Err("review service unavailable".into())
    }
}

struct MismatchedAllowGuardian;

#[async_trait::async_trait]
impl GuardianReviewer for MismatchedAllowGuardian {
    async fn review(&self, _request: &GuardianRequest) -> Result<GuardianVerdict, String> {
        Ok(GuardianVerdict::AllowFor {
            session_id: "stale-session".into(),
            active_run_id: "stale-run".into(),
            tool_call_id: "stale-call".into(),
        })
    }
}

fn write_call() -> ToolCall {
    ToolCall {
        id: "tc-write".into(),
        name: "write_file".into(),
        arguments_json: serde_json::json!({"path": "out.txt", "content": "hi"}).to_string(),
    }
}

fn read_call() -> ToolCall {
    ToolCall {
        id: "tc-read".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": "in.txt"}).to_string(),
    }
}

#[tokio::test]
async fn guardian_block_denies_mutating_call_and_emits_denial() {
    let calls = Arc::new(AtomicUsize::new(0));
    let guardian = Arc::new(MockGuardian {
        verdict: GuardianVerdict::Block("looks adversarial".into()),
        calls: calls.clone(),
    });
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(write_call()),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let tracker = Arc::new(std::sync::Mutex::new(
        rightkit_agent_policy::DenialTracker::new(),
    ));
    let cfg = SessionEngineConfig {
        session_id: "guardian-session".into(),
        permission_mode: PermissionMode::Bypass,
        workspace_root: isolated_workspace(),
        denial_tracker: Some(tracker.clone()),
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit.clone(), cfg);
    e.set_guardian_reviewer(guardian);

    let out = e.submit_user_message("write a file").await.unwrap();
    assert_eq!(out, "done");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "guardian called exactly once"
    );

    let denied = conduit.events_after(0).iter().any(|event| {
        matches!(
            event,
            ProtocolEvent::ToolResultAdded { result, .. }
                if matches!(
                    result,
                    TranscriptMessage::Tool {
                        tool_call_id,
                        summary,
                        is_error: true,
                        ..
                    } if tool_call_id == "tc-write"
                        && summary.contains("guardian denied")
                        && summary.contains("looks adversarial")
                        && summary.contains("correlation=corr-")
                )
        )
    });
    assert!(denied, "expected a guardian denial ToolResultAdded event");
    assert!(
        !conduit.events_after(0).iter().any(|event| matches!(
            event,
            ProtocolEvent::ToolCallStarted { call, .. } if call.id == "tc-write"
        )),
        "guardian denial must occur before durable origin publication"
    );
    let tracker = tracker.lock().unwrap();
    let records = tracker.denials_for_correlation(
        &rightkit_agent_policy::denial_tracking::correlation_token("guardian-session", "tc-write"),
    );
    assert_eq!(records.len(), 1);
    assert!(records[0].reason.contains("guardian denied"));
}

/// Fresh per-test workspace so stray files in the shared temp dir cannot
/// trip the verify gate and exhaust the scripted LLM.
fn isolated_workspace() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cr-guardian-{}",
        rightkit_agent_engine_protocol::uuid_v7()
    ));
    std::fs::create_dir_all(&dir).expect("create isolated workspace");
    dir
}

#[tokio::test]
async fn guardian_allow_lets_mutating_call_through() {
    let calls = Arc::new(AtomicUsize::new(0));
    let guardian = Arc::new(MockGuardian {
        verdict: GuardianVerdict::Allow,
        calls: calls.clone(),
    });
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(write_call()),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: isolated_workspace(),
        // Post-edit verification is not under test; with no assurance broker
        // it would fence the write and force an extra, unscripted model turn.
        auto_post_edit_diagnostics: false,
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit.clone(), cfg);
    e.set_guardian_reviewer(guardian);

    let out = e.submit_user_message("write a file").await.unwrap();
    assert_eq!(out, "done");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "guardian called exactly once"
    );

    let denied = conduit.events_after(0).iter().any(|event| {
        matches!(
            event,
            ProtocolEvent::ToolResultAdded { result, .. }
                if matches!(
                    result,
                    TranscriptMessage::Tool { tool_call_id, summary, is_error: true, .. }
                        if tool_call_id == "tc-write" && summary.contains("guardian denied")
                )
        )
    });
    assert!(
        !denied,
        "an Allow verdict must not produce a guardian denial"
    );
}

#[tokio::test]
async fn guardian_reviewer_error_fails_closed() {
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(write_call()),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: isolated_workspace(),
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit.clone(), cfg);
    e.set_guardian_reviewer(Arc::new(ErrorGuardian));

    assert_eq!(e.submit_user_message("write a file").await.unwrap(), "done");
    let denied = conduit.events_after(0).iter().any(|event| {
        matches!(
            event,
            ProtocolEvent::ToolResultAdded { result, .. }
                if matches!(
                    result,
                    TranscriptMessage::Tool { summary, is_error: true, .. }
                        if summary.contains("guardian denied")
                            && summary.contains("reviewer failed")
                )
        )
    });
    assert!(
        denied,
        "reviewer errors must deny configured guardian calls"
    );
}

#[tokio::test]
async fn guardian_mismatched_allow_fails_closed() {
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(write_call()),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: isolated_workspace(),
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit.clone(), cfg);
    e.set_guardian_reviewer(Arc::new(MismatchedAllowGuardian));

    assert_eq!(e.submit_user_message("write a file").await.unwrap(), "done");
    let denied = conduit.events_after(0).iter().any(|event| {
        matches!(
            event,
            ProtocolEvent::ToolResultAdded { result, .. }
                if matches!(
                    result,
                    TranscriptMessage::Tool { summary, is_error: true, .. }
                        if summary.contains("guardian denied")
                            && summary.contains("mismatched request identity")
                )
        )
    });
    assert!(
        denied,
        "stale guardian allows must not authorize another call"
    );
}

#[tokio::test]
async fn guardian_unconfigured_is_never_invoked() {
    // No `set_guardian_reviewer` call at all — the field stays `None` and the
    // check must be a complete no-op (can't assert non-invocation on a mock
    // that was never attached, so this proves the call succeeds exactly as it
    // did before the guardian existed: mutating call executes normally).
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(write_call()),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: isolated_workspace(),
        // Post-edit verification is not under test (see the allow test).
        auto_post_edit_diagnostics: false,
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit.clone(), cfg);
    assert!(e.guardian.is_none());

    let out = e.submit_user_message("write a file").await.unwrap();
    assert_eq!(out, "done");

    let denied = conduit.events_after(0).iter().any(|event| {
        matches!(
            event,
            ProtocolEvent::ToolResultAdded { result, .. }
                if matches!(
                    result,
                    TranscriptMessage::Tool { summary, is_error: true, .. }
                        if summary.contains("guardian denied")
                )
        )
    });
    assert!(!denied, "unconfigured guardian must never deny anything");
}

#[tokio::test]
async fn guardian_never_triggered_for_read_only_tool() {
    let calls = Arc::new(AtomicUsize::new(0));
    // Even a guardian that would BLOCK everything must never see a read-only call.
    let guardian = Arc::new(MockGuardian {
        verdict: GuardianVerdict::Block("would block anything".into()),
        calls: calls.clone(),
    });
    let dir =
        std::env::temp_dir().join(format!("cr-guardian-readonly-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("in.txt"), "hello").unwrap();

    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(read_call()),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: dir.clone(),
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit.clone(), cfg);
    e.set_guardian_reviewer(guardian);

    let out = e.submit_user_message("read a file").await.unwrap();
    assert_eq!(out, "done");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a read-only tool call must never reach the guardian"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
