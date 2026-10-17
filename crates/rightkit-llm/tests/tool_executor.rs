//! The tool executor: gate, budget and result re-entry (Orthic-Labs/rightkit#297). Runs
//! against a scripted provider, so no network is involved.

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_llm::{
    AllowAll, AllowTools, ChatRequest, ChatResponse, ContentBlock, GateDecision, LlmError, Message,
    Metering, Provider, Role, StopCause, StopReason, StreamEvent, ToolBudget, ToolExecutor,
    ToolOutcome, ToolRegistry, ToolSpec, Usage,
};
use serde_json::{json, Value};

/// Answers from a script and records every request it received.
struct Scripted {
    script: Mutex<Vec<Result<ChatResponse, LlmError>>>,
    seen: Mutex<Vec<ChatRequest>>,
}

impl Scripted {
    fn new(script: Vec<Result<ChatResponse, LlmError>>) -> Self {
        let mut script = script;
        script.reverse();
        Self {
            script: Mutex::new(script),
            seen: Mutex::new(Vec::new()),
        }
    }
    fn requests(&self) -> Vec<ChatRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl Provider for Scripted {
    fn id(&self) -> &str {
        "scripted"
    }
    fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        self.seen.lock().unwrap().push(req.clone());
        self.script
            .lock()
            .unwrap()
            .pop()
            .expect("script has another answer")
    }
    fn stream(
        &self,
        _req: &ChatRequest,
        _on_event: &mut dyn FnMut(StreamEvent) -> ControlFlow<()>,
    ) -> Result<ChatResponse, LlmError> {
        unreachable!("the executor does not stream")
    }
}

fn answer(content: Vec<ContentBlock>, usage: Option<(u64, u64)>) -> ChatResponse {
    ChatResponse {
        id: "r".into(),
        model: "m".into(),
        stop_reason: if content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
        {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        },
        content,
        usage: usage
            .map(|(i, o)| Usage {
                input_tokens: i,
                output_tokens: o,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            })
            .unwrap_or_default(),
        usage_known: usage.is_some(),
        metering: Metering::PerToken,
    }
}

fn tool_use(id: &str, name: &str, input: Value) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.into(),
        name: name.into(),
        input,
    }
}

fn calls(uses: Vec<ContentBlock>, usage: Option<(u64, u64)>) -> Result<ChatResponse, LlmError> {
    Ok(answer(uses, usage))
}

fn text(t: &str, usage: Option<(u64, u64)>) -> Result<ChatResponse, LlmError> {
    Ok(answer(vec![ContentBlock::text(t)], usage))
}

fn spec(name: &str) -> ToolSpec {
    ToolSpec::new(name, "test tool", json!({"type": "object"}))
}

fn echo_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(spec("echo"), |input: &Value| {
        Ok(format!("echo:{}", input["x"].as_str().unwrap_or("")))
    })
    .unwrap();
    r
}

fn request() -> ChatRequest {
    ChatRequest::new("m", vec![Message::user("go")])
}

#[test]
fn results_re_enter_in_one_user_message_in_call_order() {
    let provider = Scripted::new(vec![
        calls(
            vec![
                tool_use("a", "echo", json!({"x": "1"})),
                tool_use("b", "echo", json!({"x": "2"})),
            ],
            Some((10, 5)),
        ),
        text("done", Some((20, 3))),
    ]);
    let executor = ToolExecutor::new(echo_registry(), AllowAll, ToolBudget::default());
    let out = executor.run(&provider, request()).unwrap();

    assert_eq!(out.stop, StopCause::Completed);
    assert_eq!(out.rounds, 2);
    assert_eq!(out.response.as_ref().unwrap().text(), "done");
    assert_eq!(out.usage.input_tokens, 30);
    assert_eq!(out.usage.output_tokens, 8);
    assert!(out.usage_known);

    // The second model call saw: user, assistant (with the calls), user (both results).
    let second = &provider.requests()[1];
    assert_eq!(second.tools.len(), 1);
    assert_eq!(second.messages.len(), 3);
    assert_eq!(second.messages[1].role, Role::Assistant);
    assert_eq!(
        second.messages[2].content,
        vec![
            ContentBlock::ToolResult {
                tool_use_id: "a".into(),
                content: "echo:1".into(),
                is_error: false
            },
            ContentBlock::ToolResult {
                tool_use_id: "b".into(),
                content: "echo:2".into(),
                is_error: false
            },
        ]
    );
    assert_eq!(out.transcript.len(), 2);
    assert_eq!(out.calls.len(), 2);
    assert!(out.calls.iter().all(|c| c.outcome == ToolOutcome::Ok));
}

#[test]
fn a_denied_unknown_failing_and_panicking_call_each_get_an_error_result() {
    let mut registry = echo_registry();
    registry
        .register(spec("fails"), |_: &Value| Err("no luck".to_string()))
        .unwrap();
    registry
        .register(spec("boom"), |_: &Value| -> Result<String, String> {
            panic!("handler bug")
        })
        .unwrap();
    registry
        .register(spec("secret"), |_: &Value| Ok("never".to_string()))
        .unwrap();
    let gate = |call: &rightkit_llm::ToolInvocation<'_>| {
        if call.name == "secret" {
            GateDecision::Deny("not for this task".into())
        } else {
            GateDecision::Allow
        }
    };
    let provider = Scripted::new(vec![
        calls(
            vec![
                tool_use("1", "secret", json!({})),
                tool_use("2", "missing", json!({})),
                tool_use("3", "fails", json!({})),
                tool_use("4", "boom", json!({})),
            ],
            None,
        ),
        text("ok", None),
    ]);
    let executor = ToolExecutor::new(registry, gate, ToolBudget::default());
    let out = executor.run(&provider, request()).unwrap();

    let outcomes: Vec<&ToolOutcome> = out.calls.iter().map(|c| &c.outcome).collect();
    assert_eq!(
        outcomes[0],
        &ToolOutcome::Denied("not for this task".into())
    );
    assert_eq!(outcomes[1], &ToolOutcome::Unknown);
    assert_eq!(outcomes[2], &ToolOutcome::Failed("no luck".into()));
    assert_eq!(outcomes[3], &ToolOutcome::Panicked);
    let results = &provider.requests()[1].messages[2].content;
    assert_eq!(results.len(), 4);
    assert!(results
        .iter()
        .all(|b| matches!(b, ContentBlock::ToolResult { is_error: true, .. })));
    // Usage was not reported by either round, so the total is a lower bound.
    assert!(!out.usage_known);
}

#[test]
fn allow_tools_denies_everything_else() {
    let provider = Scripted::new(vec![
        calls(vec![tool_use("1", "echo", json!({"x": "a"}))], None),
        text("ok", None),
    ]);
    let executor = ToolExecutor::new(
        echo_registry(),
        AllowTools::new(["something_else"]),
        ToolBudget::default(),
    );
    let out = executor.run(&provider, request()).unwrap();
    assert!(matches!(out.calls[0].outcome, ToolOutcome::Denied(_)));
}

#[test]
fn the_call_budget_answers_the_rest_without_running_them_and_stops() {
    let ran = Arc::new(AtomicU32::new(0));
    let counter = ran.clone();
    let mut registry = ToolRegistry::new();
    registry
        .register(spec("count"), move |_: &Value| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok("n".to_string())
        })
        .unwrap();
    let provider = Scripted::new(vec![calls(
        vec![
            tool_use("1", "count", json!({})),
            tool_use("2", "count", json!({})),
            tool_use("3", "count", json!({})),
        ],
        None,
    )]);
    let budget = ToolBudget {
        max_tool_calls: 1,
        ..ToolBudget::default()
    };
    let out = ToolExecutor::new(registry, AllowAll, budget)
        .run(&provider, request())
        .unwrap();
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert_eq!(out.stop, StopCause::CallLimit);
    assert_eq!(out.rounds, 1);
    assert_eq!(
        out.calls[1].outcome,
        ToolOutcome::Skipped(StopCause::CallLimit)
    );
    assert_eq!(
        out.calls[2].outcome,
        ToolOutcome::Skipped(StopCause::CallLimit)
    );
    // All three calls were answered in the transcript.
    assert_eq!(out.transcript[1].content.len(), 3);
}

#[test]
fn the_round_limit_stops_without_running_tools_nobody_would_read() {
    let ran = Arc::new(AtomicU32::new(0));
    let counter = ran.clone();
    let mut registry = ToolRegistry::new();
    registry
        .register(spec("count"), move |_: &Value| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok("n".to_string())
        })
        .unwrap();
    let provider = Scripted::new(vec![
        calls(vec![tool_use("1", "count", json!({}))], None),
        calls(vec![tool_use("2", "count", json!({}))], None),
    ]);
    let budget = ToolBudget {
        max_rounds: 2,
        ..ToolBudget::default()
    };
    let out = ToolExecutor::new(registry, AllowAll, budget)
        .run(&provider, request())
        .unwrap();
    assert_eq!(out.stop, StopCause::RoundLimit);
    assert_eq!(out.rounds, 2);
    // Round 1's tool ran; round 2's did not, and its call is still on the response.
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert_eq!(out.response.as_ref().unwrap().tool_uses().len(), 1);
}

#[test]
fn a_large_result_is_cut_and_marked() {
    let mut registry = ToolRegistry::new();
    registry
        .register(spec("big"), |_: &Value| Ok("x".repeat(100)))
        .unwrap();
    let provider = Scripted::new(vec![
        calls(vec![tool_use("1", "big", json!({}))], None),
        text("ok", None),
    ]);
    let budget = ToolBudget {
        max_result_bytes: 10,
        ..ToolBudget::default()
    };
    let out = ToolExecutor::new(registry, AllowAll, budget)
        .run(&provider, request())
        .unwrap();
    assert!(out.calls[0].truncated);
    let ContentBlock::ToolResult { content, .. } = &out.transcript[1].content[0] else {
        panic!("expected a tool result");
    };
    assert!(content.starts_with("xxxxxxxxxx\n[truncated: 90 bytes omitted]"));
}

#[test]
fn the_token_bound_stops_before_running_tools() {
    let provider = Scripted::new(vec![calls(
        vec![tool_use("1", "echo", json!({"x": "a"}))],
        Some((60, 50)),
    )]);
    let budget = ToolBudget {
        max_total_tokens: Some(100),
        ..ToolBudget::default()
    };
    let out = ToolExecutor::new(echo_registry(), AllowAll, budget)
        .run(&provider, request())
        .unwrap();
    assert_eq!(out.stop, StopCause::TokenLimit);
    assert!(out.calls.is_empty());
}

#[test]
fn a_cancel_flag_stops_the_run_before_the_first_model_call() {
    let flag = Arc::new(AtomicBool::new(true));
    let provider = Scripted::new(vec![]);
    let out = ToolExecutor::new(echo_registry(), AllowAll, ToolBudget::default())
        .with_cancel(flag)
        .run(&provider, request())
        .unwrap();
    assert_eq!(out.stop, StopCause::Cancelled);
    assert_eq!(out.rounds, 0);
    assert!(provider.requests().is_empty());
}

#[test]
fn a_provider_failure_keeps_the_spend_of_earlier_rounds() {
    let provider = Scripted::new(vec![
        calls(
            vec![tool_use("1", "echo", json!({"x": "a"}))],
            Some((10, 5)),
        ),
        Err(LlmError::Cancelled),
    ]);
    let err = ToolExecutor::new(echo_registry(), AllowAll, ToolBudget::default())
        .run(&provider, request())
        .unwrap_err();
    assert!(matches!(err.error, LlmError::Cancelled));
    assert_eq!(err.partial.stop, StopCause::Failed);
    assert_eq!(err.partial.rounds, 1);
    assert_eq!(err.partial.usage.input_tokens, 10);
    assert_eq!(err.partial.calls.len(), 1);
}

#[test]
fn a_forced_tool_is_rejected_and_duplicates_cannot_register() {
    let mut req = request();
    req.force_tool = Some("echo".into());
    let provider = Scripted::new(vec![]);
    let err = ToolExecutor::new(echo_registry(), AllowAll, ToolBudget::default())
        .run(&provider, req)
        .unwrap_err();
    assert!(matches!(err.error, LlmError::InvalidRequest(_)));
    assert!(provider.requests().is_empty());

    let mut registry = echo_registry();
    assert!(registry
        .register(spec("echo"), |_: &Value| Ok(String::new()))
        .is_err());
    assert!(registry
        .register(spec(" "), |_: &Value| Ok(String::new()))
        .is_err());
}
