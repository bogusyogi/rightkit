use super::*;

fn fill(t: &mut Transcript, n: usize) {
    for i in 0..n {
        t.push_user(format!("msg {i}"));
        t.push_assistant(format!("reply {i}"), None, vec![]);
    }
}

const TEST_SESSION: &str = "test-session";
const TEST_WORKSPACE: &str = "/tmp";

#[tokio::test]
async fn no_compaction_below_trigger() {
    let c = Compactor::new(100, 4);
    let mut t = Transcript::new();
    fill(&mut t, 3); // 6 messages
    assert!(!c.should_compact(&t));
    assert_eq!(
        c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
            .await,
        0
    );
    assert_eq!(t.len(), 6);
}

#[tokio::test]
async fn compaction_drops_old_messages() {
    let c = Compactor::new(10, 4);
    let mut t = Transcript::new();
    fill(&mut t, 10); // 20 messages
    assert!(c.should_compact(&t));
    let dropped = c
        .compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;
    assert!(dropped > 0);
    // After: a compact marker + kept recent tail; should be much smaller.
    assert!(t.len() < 20);
}

#[tokio::test]
async fn compaction_inserts_compact_marker() {
    let c = Compactor::new(10, 4);
    let mut t = Transcript::new();
    fill(&mut t, 10);
    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;
    let compact_at = t.messages().iter().find_map(|m| {
        if let TranscriptMessage::Compact { created_at, .. } = m {
            Some(created_at.as_str())
        } else {
            None
        }
    });
    let compact_at = compact_at.expect("compact marker");
    assert_ne!(compact_at, "t");
    assert!(compact_at.contains('T'));
    assert!(compact_at.ends_with('Z'));
}

#[tokio::test]
async fn compaction_preserves_pinned_system() {
    let c = Compactor::new(10, 2);
    let mut t = Transcript::new();
    t.push_system("system prompt"); // pinned
    fill(&mut t, 10);
    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;
    let has_system = t
        .messages()
        .iter()
        .any(|m| matches!(m, TranscriptMessage::System { .. }));
    assert!(has_system);
}

#[tokio::test]
async fn compaction_preserves_recent_skill_invocation_block() {
    let c = Compactor::new(6, 2);
    let mut t = Transcript::new();
    fill(&mut t, 2);
    t.push_assistant(
        "",
        None,
        vec![ToolCall {
            id: "skill-call".into(),
            name: "invoke_skill".into(),
            arguments_json: serde_json::json!({"skill": "reviewer"}).to_string(),
        }],
    );
    t.push_tool_result(
        "skill-call",
        "Skill: reviewer\nPath: skills/reviewer.md\n\n# Reviewer\n\nReview carefully.",
        false,
    );
    fill(&mut t, 6);

    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;

    let messages = t.messages();
    let assistant_index = messages.iter().position(|message| {
        matches!(
            message,
            TranscriptMessage::Assistant { tool_calls, .. }
                if tool_calls.iter().any(|call| call.id == "skill-call")
        )
    });
    let tool_index = messages.iter().position(|message| {
        matches!(
            message,
            TranscriptMessage::Tool {
                tool_call_id,
                summary,
                ..
            } if tool_call_id == "skill-call" && summary.contains("# Reviewer")
        )
    });

    let assistant_index = assistant_index.expect("preserved skill assistant");
    let tool_index = tool_index.expect("preserved skill result");
    assert!(assistant_index < tool_index);
}

#[tokio::test]
async fn compaction_preserves_recent_instruction_read_block() {
    let c = Compactor::new(6, 2);
    let mut t = Transcript::new();
    fill(&mut t, 2);
    t.push_assistant(
        "",
        None,
        vec![ToolCall {
            id: "agents-read".into(),
            name: "read_file".into(),
            arguments_json: serde_json::json!({"path": "AGENTS.md"}).to_string(),
        }],
    );
    t.push_tool_result(
        "agents-read",
        "# AGENTS.md\n\nUse the Rust/Tauri stack.",
        false,
    );
    fill(&mut t, 6);

    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;

    let messages = t.messages();
    let assistant_index = messages.iter().position(|message| {
        matches!(
            message,
            TranscriptMessage::Assistant { tool_calls, .. }
                if tool_calls.iter().any(|call| call.id == "agents-read")
        )
    });
    let tool_index = messages.iter().position(|message| {
        matches!(
            message,
            TranscriptMessage::Tool {
                tool_call_id,
                summary,
                ..
            } if tool_call_id == "agents-read" && summary.contains("Rust/Tauri")
        )
    });

    let assistant_index = assistant_index.expect("preserved instruction assistant");
    let tool_index = tool_index.expect("preserved instruction read result");
    assert!(assistant_index < tool_index);
}

#[tokio::test]
async fn compaction_preserves_tool_block_crossing_keep_boundary() {
    let c = Compactor::new(6, 2);
    let mut t = Transcript::new();
    fill(&mut t, 2);
    t.push_assistant(
        "",
        None,
        vec![ToolCall {
            id: "tc-boundary".into(),
            name: "read_file".into(),
            arguments_json: serde_json::json!({"path": "README.md"}).to_string(),
        }],
    );
    t.push_tool_result("tc-boundary", "first result", false);
    t.push_tool_result("tc-boundary", "second result", false);
    t.push_tool_result("tc-boundary", "third result", false);

    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;

    let messages = t.messages();
    let assistant_index = messages.iter().position(|message| {
        matches!(
            message,
            TranscriptMessage::Assistant { tool_calls, .. }
                if tool_calls.iter().any(|call| call.id == "tc-boundary")
        )
    });
    let tool_count = messages
        .iter()
        .filter(|message| {
            matches!(
                message,
                TranscriptMessage::Tool { tool_call_id, .. } if tool_call_id == "tc-boundary"
            )
        })
        .count();

    assert!(assistant_index.is_some(), "preserved boundary assistant");
    assert_eq!(tool_count, 3, "preserved all boundary tool results");
}

#[tokio::test]
async fn compaction_preserves_latest_user_message_before_boundary() {
    let c = Compactor::new(8, 2);
    let mut t = Transcript::new();
    fill(&mut t, 2);
    t.push_user("audit the repo and write the final report");
    t.push_assistant(
        "",
        None,
        vec![ToolCall {
            id: "tc-loop".into(),
            name: "read_file".into(),
            arguments_json: serde_json::json!({"path": "missing.tsx"}).to_string(),
        }],
    );
    t.push_tool_result("tc-loop", "missing.tsx not found", true);
    t.push_assistant(
        "",
        None,
        vec![ToolCall {
            id: "tc-loop-2".into(),
            name: "read_file".into(),
            arguments_json: serde_json::json!({"path": "still-missing.tsx"}).to_string(),
        }],
    );
    t.push_tool_result("tc-loop-2", "still missing", true);

    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;

    let preserved = t.messages().iter().any(|message| {
        matches!(
            message,
            TranscriptMessage::User { content, .. }
                if content == "audit the repo and write the final report"
        )
    });
    assert!(preserved, "preserved latest user task");
}

#[tokio::test]
async fn compaction_is_idempotent() {
    let c = Compactor::new(10, 4);
    let mut t = Transcript::new();
    fill(&mut t, 10);
    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;
    let len_after_first = t.len();
    // Second compaction on the now-small transcript is a no-op.
    let dropped2 = c
        .compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;
    assert_eq!(dropped2, 0);
    assert_eq!(t.len(), len_after_first);
}

#[tokio::test]
async fn compaction_with_prior_summary() {
    // Mock LLM summarizer that concatenates prior summary with message count.
    let mock_call_fn: Arc<LlmCallFn> = Arc::new(|messages| {
        Box::pin(async move {
            // Simple mock: count messages and return a summary.
            Ok(format!("LLM summary of {} messages", messages.len()))
        })
    });

    let summarizer = Arc::new(LlmSummarizer {
        compaction_prompt: "Summarize concisely".into(),
        call_fn: mock_call_fn,
    });

    let c = Compactor::new(10, 4).with_summarizer(summarizer);
    let mut t = Transcript::new();
    fill(&mut t, 10);
    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;

    // Fill with more messages.
    fill(&mut t, 5);

    // Compact again; the prior summary should be included.
    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;
    // After second compaction, should still have messages.
    assert!(!t.is_empty());
    // Verify a Compact marker exists.
    let has_compact = t
        .messages()
        .iter()
        .any(|m| matches!(m, TranscriptMessage::Compact { .. }));
    assert!(has_compact);
}

#[tokio::test]
async fn compaction_does_not_duplicate_post_compaction_continuation() {
    let c = Compactor::new(10, 4);
    let mut t = Transcript::new();
    fill(&mut t, 10);
    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;
    fill(&mut t, 10);
    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;

    let continuation_count = t
        .messages()
        .iter()
        .filter(|message| {
            matches!(
                message,
                TranscriptMessage::System { id, .. } if id == POST_COMPACTION_CONTINUATION_ID
            )
        })
        .count();
    assert_eq!(continuation_count, 1);
}

#[tokio::test]
async fn compaction_injects_post_compaction_continuation_system_message() {
    let c = Compactor::new(10, 4);
    let mut t = Transcript::new();
    fill(&mut t, 10);

    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;

    let content = t.messages().iter().find_map(|message| match message {
        TranscriptMessage::System { id, content, .. } if id == POST_COMPACTION_CONTINUATION_ID => {
            Some(content.as_str())
        }
        _ => None,
    });
    let content = content.expect("post-compaction continuation system message");
    for continuation in POST_COMPACTION_CONTINUATION_STRINGS {
        assert!(content.contains(continuation));
    }
}

// S:E4 — post-compact context reinjection ordering.
//
// After a compaction the transcript is rebuilt in a fixed order: pinned
// messages, then the continuation system message, then the memory block (when
// one is provided), then the compact summary marker, then the recent tail.
// The order is load-bearing — the model must see standing instructions and
// recalled context *before* the summary that explains what was dropped, and
// the surviving tail last so it reads as the most recent turns. Each part is
// covered individually elsewhere; this pins their relative sequence, which is
// what actually breaks if `rebuild_after_compaction` is reordered.
#[tokio::test]
async fn post_compaction_reinjection_follows_pinned_continuation_summary_tail_order() {
    let c = Compactor::new(10, 4);
    let mut t = Transcript::new();
    t.push_system("standing instruction"); // pinned by construction
    fill(&mut t, 10);

    c.compact(&mut t, TEST_SESSION, std::path::Path::new(TEST_WORKSPACE))
        .await;

    let messages = t.messages();
    let index_of =
        |pred: &dyn Fn(&TranscriptMessage) -> bool| messages.iter().position(|m| pred(m));

    let pinned_at = index_of(&|m: &TranscriptMessage| {
        matches!(
            m,
            TranscriptMessage::System { id, content, .. }
                if id != POST_COMPACTION_CONTINUATION_ID && content == "standing instruction"
        )
    })
    .expect("pinned system message must survive compaction");
    let continuation_at = index_of(&|m: &TranscriptMessage| {
        matches!(m, TranscriptMessage::System { id, .. } if id == POST_COMPACTION_CONTINUATION_ID)
    })
    .expect("continuation message must be injected");
    let compact_at =
        index_of(&|m: &TranscriptMessage| matches!(m, TranscriptMessage::Compact { .. }))
            .expect("compact marker must be written");

    assert!(
        pinned_at < continuation_at,
        "pinned messages must precede the continuation message ({pinned_at} vs {continuation_at})"
    );
    assert!(
        continuation_at < compact_at,
        "continuation must precede the compact summary ({continuation_at} vs {compact_at})"
    );
    assert!(
        compact_at < messages.len() - 1,
        "the surviving recent tail must follow the compact summary, not precede it"
    );
    assert!(
        messages[compact_at + 1..]
            .iter()
            .all(|m| !matches!(m, TranscriptMessage::Compact { .. })),
        "exactly one compact marker may appear, at the boundary"
    );
}
