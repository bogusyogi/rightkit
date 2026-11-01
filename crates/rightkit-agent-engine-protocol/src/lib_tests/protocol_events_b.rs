use super::*;

#[test]
fn protocol_event_task_graph_updated_kind() {
    let ev = ProtocolEvent::TaskGraphUpdated {
        session_id: "s1".into(),
        seq: 8,
        created_at: "t".into(),
        summary: "3 tasks".into(),
        tasks: vec![],
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"taskGraphUpdated""#),
        "got: {json}"
    );
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::TaskGraphUpdated { .. }));
}

#[test]
fn protocol_event_task_lifecycle_kinds() {
    let task = ProtocolEvent::TaskStatusUpdated {
        session_id: "s1".into(),
        seq: 9,
        created_at: "t".into(),
        task_id: "task-1".into(),
        context_id: "ctx-1".into(),
        state: "completed".into(),
        task: serde_json::json!({ "id": "task-1" }),
    };
    let artifact = ProtocolEvent::TaskArtifactUpdated {
        session_id: "s1".into(),
        seq: 10,
        created_at: "t".into(),
        task_id: "task-1".into(),
        context_id: "ctx-1".into(),
        artifact_id: "artifact-1".into(),
        artifact: serde_json::json!({ "artifactId": "artifact-1" }),
    };
    let lineage = ProtocolEvent::ArtifactLineageUpdated {
        session_id: "s1".into(),
        seq: 11,
        created_at: "t".into(),
        task_id: "task-1".into(),
        context_id: "ctx-1".into(),
        artifact_id: "artifact-1".into(),
        lineage: serde_json::json!({ "artifactId": "artifact-1", "accepted": true }),
    };

    let task_json = serde_json::to_string(&task).unwrap();
    let artifact_json = serde_json::to_string(&artifact).unwrap();
    let lineage_json = serde_json::to_string(&lineage).unwrap();
    assert!(
        task_json.contains(r#""$kind":"taskStatusUpdated""#),
        "got: {task_json}"
    );
    assert!(
        artifact_json.contains(r#""$kind":"taskArtifactUpdated""#),
        "got: {artifact_json}"
    );
    assert!(
        lineage_json.contains(r#""$kind":"artifactLineageUpdated""#),
        "got: {lineage_json}"
    );
    assert!(matches!(
        serde_json::from_str::<ProtocolEvent>(&task_json).unwrap(),
        ProtocolEvent::TaskStatusUpdated { .. }
    ));
    assert!(matches!(
        serde_json::from_str::<ProtocolEvent>(&artifact_json).unwrap(),
        ProtocolEvent::TaskArtifactUpdated { .. }
    ));
    assert!(matches!(
        serde_json::from_str::<ProtocolEvent>(&lineage_json).unwrap(),
        ProtocolEvent::ArtifactLineageUpdated { .. }
    ));
}

#[test]
fn protocol_event_run_interrupted_kind() {
    let ev = ProtocolEvent::RunInterrupted {
        session_id: "s1".into(),
        seq: 12,
        created_at: "t".into(),
        reason: Some("user cancelled".into()),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""$kind":"runInterrupted""#), "got: {json}");
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::RunInterrupted { .. }));
}

#[test]
fn protocol_event_run_failed_kind() {
    let ev = ProtocolEvent::RunFailed {
        session_id: "s1".into(),
        seq: 10,
        created_at: "t".into(),
        error: "timeout".into(),
        kind: "timeout".into(),
        hint: "retry later".into(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""$kind":"runFailed""#), "got: {json}");
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::RunFailed { .. }));
}

#[test]
fn protocol_event_session_compacted_kind() {
    let ev = ProtocolEvent::SessionCompacted {
        session_id: "s1".into(),
        seq: 11,
        created_at: "t".into(),
        summary: "compacted 5 turns".into(),
        dropped_count: 5,
        tier: "summary".into(),
        tokens_before: 900,
        tokens_after: 100,
        epoch: 2,
        command_id: "compact-1".into(),
        retained_turns: 8,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"sessionCompacted""#),
        "got: {json}"
    );
    assert!(json.contains("\"retainedTurns\":8"), "got: {json}");
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(
        matches!(rt, ProtocolEvent::SessionCompacted { command_id, .. } if command_id == "compact-1")
    );
}

#[test]
fn session_compacted_retained_turns_round_trips() {
    for retained in [0u32, 3, 8] {
        let ev = ProtocolEvent::SessionCompacted {
            session_id: "s1".into(),
            seq: 11,
            created_at: "t".into(),
            summary: "s".into(),
            dropped_count: 5,
            tier: "summary".into(),
            tokens_before: 900,
            tokens_after: 100,
            epoch: 2,
            command_id: "compact-1".into(),
            retained_turns: retained,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"retainedTurns\""), "got: {json}");
        let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(rt, ProtocolEvent::SessionCompacted { retained_turns, .. } if retained_turns == retained)
        );
    }
}

#[test]
fn session_compacted_without_retained_turns_is_rejected() {
    let json = r#"{"$kind":"sessionCompacted","sessionId":"s1","seq":11,"createdAt":"t","summary":"old","droppedCount":2}"#;
    assert!(serde_json::from_str::<ProtocolEvent>(json).is_err());
}

#[test]
fn protocol_event_usage_updated_kind() {
    let ev = ProtocolEvent::UsageUpdated {
        session_id: "s1".into(),
        seq: 12,
        created_at: "t".into(),
        prompt_tokens: 100,
        completion_tokens: 50,
        cost_usd: 0.001,
        context_tokens: 150,
        context_percent: 3.5,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        cost_known: true,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""$kind":"usageUpdated""#), "got: {json}");
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::UsageUpdated { .. }));
}

#[test]
fn protocol_event_turn_usage_wire_shape_and_roundtrip() {
    let ev = ProtocolEvent::TurnUsage {
        session_id: "s1".into(),
        seq: 13,
        created_at: "t".into(),
        input_tokens: 2006,
        cached_input_tokens: 1920,
        cache_creation_tokens: 0,
        output_tokens: 300,
        reasoning_tokens: 192,
        context_window: 272_000,
        context_used_tokens: 2306,
        context_used_pct: 0.85,
        cache_hit_pct: 95.6,
        session_cache_hit_pct: 90.0,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""$kind":"turnUsage""#), "got: {json}");
    for field in [
        "inputTokens",
        "cachedInputTokens",
        "cacheCreationTokens",
        "outputTokens",
        "reasoningTokens",
        "contextWindow",
        "contextUsedTokens",
        "contextUsedPct",
        "cacheHitPct",
        "sessionCacheHitPct",
    ] {
        assert!(json.contains(field), "missing {field}: {json}");
    }
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(serde_json::to_string(&rt).unwrap(), json);
    // Older/partial senders: optional fields default.
    let minimal = r#"{"$kind":"turnUsage","sessionId":"s","seq":1,"createdAt":"t","inputTokens":5,"cachedInputTokens":2,"outputTokens":1}"#;
    match serde_json::from_str::<ProtocolEvent>(minimal).unwrap() {
        ProtocolEvent::TurnUsage {
            context_window,
            reasoning_tokens,
            ..
        } => assert_eq!((context_window, reasoning_tokens), (0, 0)),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn protocol_event_tool_output_delta_is_a_live_projection() {
    let ev = ProtocolEvent::ToolOutputDelta {
        session_id: "s1".into(),
        seq: 14,
        created_at: "t".into(),
        call_id: "call-1".into(),
        stream: "stdout".into(),
        text: "line\n".into(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""$kind":"toolOutputDelta""#), "got: {json}");
    assert!(json.contains(r#""callId":"call-1""#), "got: {json}");
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(serde_json::to_string(&rt).unwrap(), json);
    assert!(
        !ev.is_durable_fact(),
        "streamed output must not be persisted"
    );
    assert!(ProtocolEvent::TurnUsage {
        session_id: "s".into(),
        seq: 1,
        created_at: "t".into(),
        input_tokens: 1,
        cached_input_tokens: 0,
        cache_creation_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: 0,
        context_window: 0,
        context_used_tokens: 0,
        context_used_pct: 0.0,
        cache_hit_pct: 0.0,
        session_cache_hit_pct: 0.0,
    }
    .is_durable_fact());
}

#[test]
fn protocol_event_prompt_cache_break_wire_shape_roundtrip_and_durability() {
    let ev = ProtocolEvent::PromptCacheBreak {
        session_id: "s1".into(),
        seq: 21,
        created_at: "t".into(),
        reason: "system".into(),
        expected: false,
        tokens_lost: 48_000,
        estimated_cost_lost: Some(0.18),
        turn: 4,
        detail: "system prompt changed (+12 chars)".into(),
        previous_cache_read_tokens: 50_000,
        cache_read_tokens: 2_000,
        cache_creation_tokens: 48_500,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"promptCacheBreak""#),
        "got: {json}"
    );
    for field in [
        "reason",
        "expected",
        "tokensLost",
        "estimatedCostLost",
        "turn",
        "previousCacheReadTokens",
        "cacheReadTokens",
        "cacheCreationTokens",
    ] {
        assert!(json.contains(field), "missing {field}: {json}");
    }
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(serde_json::to_string(&rt).unwrap(), json);
    assert!(
        ev.is_durable_fact(),
        "a cache break must live in session history"
    );

    // Unpriced model: cost omitted from the wire, and absent decodes to None.
    let unpriced = ProtocolEvent::PromptCacheBreak {
        session_id: "s1".into(),
        seq: 22,
        created_at: "t".into(),
        reason: "unknown".into(),
        expected: false,
        tokens_lost: 1,
        estimated_cost_lost: None,
        turn: 5,
        detail: String::new(),
        previous_cache_read_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    };
    let json = serde_json::to_string(&unpriced).unwrap();
    assert!(!json.contains("estimatedCostLost"), "got: {json}");
    match serde_json::from_str::<ProtocolEvent>(&json).unwrap() {
        ProtocolEvent::PromptCacheBreak {
            estimated_cost_lost,
            ..
        } => assert_eq!(estimated_cost_lost, None),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn protocol_event_budget_exceeded_kind() {
    let ev = ProtocolEvent::BudgetExceeded {
        session_id: "s1".into(),
        seq: 13,
        created_at: "t".into(),
        scope: "session".into(),
        current_usd: 5.0,
        ceiling_usd: 4.0,
        hint: "reduce context".into(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""$kind":"budgetExceeded""#), "got: {json}");
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::BudgetExceeded { .. }));
}

#[test]
fn protocol_event_turn_backpressure_contracts_round_trip() {
    let turn = ProtocolEvent::SessionTurnFinished {
        session_id: "s1".into(),
        seq: 14,
        created_at: "t".into(),
        turn_id: "turn-1".into(),
        status: "completed".into(),
    };
    let resync = ProtocolEvent::StreamResyncRequired {
        session_id: "s1".into(),
        seq: 15,
        created_at: "t".into(),
        after_seq: 4,
        oldest_available_seq: 8,
        high_water_mark: 12,
    };
    let overloaded = ProtocolEvent::Overloaded {
        session_id: "s1".into(),
        seq: 16,
        created_at: "t".into(),
        scope: "sseClient".into(),
        limit: 128,
        current: 128,
        retry_after_ms: 250,
    };

    for event in [turn, resync, overloaded] {
        let json = serde_json::to_string(&event).unwrap();
        let decoded: ProtocolEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.seq(), event.seq());
        assert_eq!(decoded.session_id(), "s1");
    }
}

#[test]
fn protocol_event_provider_route_updated_kind() {
    let ev = ProtocolEvent::ProviderRouteUpdated {
        session_id: "s1".into(),
        seq: 14,
        created_at: "t".into(),
        state: "failover".into(),
        provider: "openai".into(),
        model: "gpt-4o".into(),
        failure_kind: "rate_limited".into(),
        detail: "".into(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"providerRouteUpdated""#),
        "got: {json}"
    );
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::ProviderRouteUpdated { .. }));
}

#[test]
fn protocol_event_account_rate_limited_and_usage_kinds() {
    let capped = ProtocolEvent::AccountRateLimited {
        session_id: "s1".into(),
        seq: 15,
        created_at: "t".into(),
        provider: "claude".into(),
        account: "a@example.com".into(),
        account_id: Some("subscription/claude/work".into()),
        account_kind: Some("subscription".into()),
        email: Some("a@example.com".into()),
        organization: None,
        status: "rejected".into(),
        rate_limit_type: Some("five_hour".into()),
        resets_at: Some(1_790_000_000),
        utilization: Some(1.0),
        overage_status: None,
        error_code: None,
    };
    let json = serde_json::to_string(&capped).unwrap();
    assert!(
        json.contains(r#""$kind":"accountRateLimited""#),
        "got: {json}"
    );
    assert!(json.contains(r#""resetsAt":1790000000"#), "got: {json}");
    assert!(
        json.contains(r#""accountKind":"subscription""#),
        "got: {json}"
    );
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(serde_json::to_string(&rt).unwrap(), json);
    assert_eq!(rt.seq(), 15);
    // Older payloads without the typed identity fields still parse.
    let legacy =
        r#"{"$kind":"accountRateLimited","sessionId":"s","seq":1,"createdAt":"t","account":"x"}"#;
    match serde_json::from_str::<ProtocolEvent>(legacy).unwrap() {
        ProtocolEvent::AccountRateLimited {
            account_id, email, ..
        } => {
            assert!(account_id.is_none() && email.is_none());
        }
        other => panic!("unexpected {other:?}"),
    }

    let usage = ProtocolEvent::AccountUsageReported {
        session_id: "s1".into(),
        seq: 16,
        created_at: "t".into(),
        provider: "claude".into(),
        account: "Team".into(),
        account_id: Some("api_key/claude/team".into()),
        account_kind: Some("api_key".into()),
        email: None,
        total_cost_usd: Some(0.5),
        duration_ms: Some(10),
        num_turns: Some(2),
        input_tokens: 5,
        output_tokens: 6,
        cache_read_tokens: 1,
        cache_creation_tokens: 2,
        models: vec!["m".into()],
    };
    let json = serde_json::to_string(&usage).unwrap();
    assert!(
        json.contains(r#""$kind":"accountUsageReported""#),
        "got: {json}"
    );
    assert!(json.contains(r#""accountKind":"api_key""#), "got: {json}");
    assert!(json.contains(r#""email":null"#), "got: {json}");
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(serde_json::to_string(&rt).unwrap(), json);
}

#[test]
fn protocol_event_orchestration_observation_recorded_kind() {
    let ev = ProtocolEvent::OrchestrationObservationRecorded {
        session_id: "s1".into(),
        seq: 15,
        created_at: "t".into(),
        observation: OrchestrationObservation {
            id: "obs1".into(),
            kind: "plan".into(),
            subject: "task1".into(),
            summary: "started".into(),
            fields: Default::default(),
            created_at: "t".into(),
        },
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"orchestrationObservationRecorded""#),
        "got: {json}"
    );
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        rt,
        ProtocolEvent::OrchestrationObservationRecorded { .. }
    ));
}

#[test]
fn protocol_event_agent_query_issued_kind() {
    let ev = ProtocolEvent::AgentQueryIssued {
        session_id: "s1".into(),
        seq: 16,
        created_at: "t".into(),
        query_id: "q1".into(),
        from_role: "orchestrator".into(),
        to_role: "coder".into(),
        query_type: "clarify".into(),
        expected_format: "text".into(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"agentQueryIssued""#),
        "got: {json}"
    );
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::AgentQueryIssued { .. }));
}

#[test]
fn protocol_event_agent_query_responded_kind() {
    let ev = ProtocolEvent::AgentQueryResponded {
        session_id: "s1".into(),
        seq: 17,
        created_at: "t".into(),
        query_id: "q1".into(),
        from_role: "coder".into(),
        to_role: "orchestrator".into(),
        confidence: 0.9,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"agentQueryResponded""#),
        "got: {json}"
    );
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::AgentQueryResponded { .. }));
}

#[test]
fn protocol_event_human_steering_injected_kind() {
    let ev = ProtocolEvent::HumanSteeringInjected {
        session_id: "s1".into(),
        seq: 18,
        created_at: "t".into(),
        injected_by: "alice".into(),
        content_preview: "focus on tests".into(),
        text: Some("focus on tests".into()),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"humanSteeringInjected""#),
        "got: {json}"
    );
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::HumanSteeringInjected { .. }));
}

#[test]
fn protocol_event_model_route_decision_kind() {
    let ev = ProtocolEvent::ModelRouteDecision {
        session_id: "s1".into(),
        seq: 19,
        created_at: "t".into(),
        decision: ModelRouteDecisionPayload {
            agent_role: "coder".into(),
            model_id: "deepseek/deepseek-v4-flash".into(),
            tier: "cheap".into(),
            reason: "routed coder -> deepseek/deepseek-v4-flash (tier=Cheap, confidence=0.82)"
                .into(),
            signals_digest: "sig-abc123".into(),
            price_snapshot: PriceSnapshot {
                prompt_per_m: 0.435,
                completion_per_m: 0.87,
                source: "bundled".into(),
                fetched_at: "t".into(),
                hash: "hash-abc123".into(),
            },
            est_cost_usd: 0.0021,
            schema_version: 1,
        },
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"modelRouteDecision""#),
        "got: {json}"
    );
    // #[serde(flatten)] means the payload fields sit alongside sessionId/seq —
    // assert the flatten actually happened (no nested "decision" object).
    assert!(json.contains(r#""agentRole":"coder""#), "got: {json}");
    assert!(
        json.contains(r#""modelId":"deepseek/deepseek-v4-flash""#),
        "got: {json}"
    );
    assert!(
        json.contains(r#""priceSnapshot":{"promptPerM":0.435"#),
        "got: {json}"
    );
    assert!(json.contains(r#""estCostUsd":0.0021"#), "got: {json}");
    assert!(json.contains(r#""schemaVersion":1"#), "got: {json}");
    assert!(
        !json.contains(r#""decision":"#),
        "flatten leaked a nested object: {json}"
    );
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    match rt {
        ProtocolEvent::ModelRouteDecision { decision, .. } => {
            assert_eq!(decision.model_id, "deepseek/deepseek-v4-flash");
            assert_eq!(decision.tier, "cheap");
            assert_eq!(decision.price_snapshot.source, "bundled");
        }
        other => panic!("expected ModelRouteDecision, got {other:?}"),
    }
}

// ── TranscriptMessage $kind field tests ───────────────────────────────────

// ── CRA-09: compaction-federation-v1 joint wire fixture ───────────────────
//
// `interface-handoffs.json` row `compaction-federation-v1`: the host app's
// compactor-consumer and Membrane's federation-catalog both round-trip the
// exact same `CompactionFederationDecisionV1` bytes. The golden file at
// `engine/crates/protocol/fixtures/compaction-federation-v1.golden.json` is
// the cross-repo drift detector — if either side's wire shape moves without
// updating the other, this test (or Membrane's equivalent) fails.

const COMPACTION_FEDERATION_GOLDEN: &str =
    include_str!("../../fixtures/compaction-federation-v1.golden.json");

fn compaction_federation_golden_decision() -> crate::event::CompactionFederationDecisionV1 {
    serde_json::from_str(COMPACTION_FEDERATION_GOLDEN)
        .expect("golden fixture must deserialize into CompactionFederationDecisionV1")
}

#[test]
fn compaction_federation_decision_round_trips_against_golden() {
    let golden_value: serde_json::Value = serde_json::from_str(COMPACTION_FEDERATION_GOLDEN)
        .expect("golden fixture must parse as JSON");

    let decision = compaction_federation_golden_decision();

    // Wire keys: identity/fence/scope + decision + selection receipt +
    // monotonic-elapsed expiry, all camelCase.
    let wire = serde_json::to_value(&decision).unwrap();
    assert_eq!(wire["identity"], golden_value["identity"]);
    assert_eq!(wire["fence"], golden_value["fence"]);
    assert_eq!(wire["scope"], golden_value["scope"]);
    assert_eq!(wire["decision"], golden_value["decision"]);
    assert_eq!(
        wire["selectionReceipt"]["selectedItemIds"],
        golden_value["selectionReceipt"]["selectedItemIds"]
    );
    assert_eq!(
        wire["selectionReceipt"]["selectionHash"],
        golden_value["selectionReceipt"]["selectionHash"]
    );
    assert_eq!(
        wire["issuedMonotonicElapsedMs"],
        golden_value["issuedMonotonicElapsedMs"]
    );
    assert_eq!(
        wire["expiresAfterElapsedMs"],
        golden_value["expiresAfterElapsedMs"]
    );
    assert_eq!(wire["restartEpoch"], golden_value["restartEpoch"]);

    // Byte-for-byte cross-repo equality: re-serializing the host app's decision
    // must equal the golden value exactly, not just field-by-field.
    assert_eq!(
        wire, golden_value,
        "wire shape diverged from golden fixture"
    );

    // Round trip back through the concrete type.
    let replayed: crate::event::CompactionFederationDecisionV1 =
        serde_json::from_value(wire).unwrap();
    assert_eq!(replayed, decision);
}

#[test]
fn compaction_federation_decision_diverging_from_golden_fails_round_trip() {
    // Negative control: a wire payload that diverges from the golden (here,
    // a substituted selection hash — as if a stale/tampered selection were
    // relayed) must NOT be treated as equal to the golden fixture. This is
    // the drift detector: the host app and Membrane must observe the same
    // bytes for the same decision, or federation must refuse it.
    let golden_value: serde_json::Value = serde_json::from_str(COMPACTION_FEDERATION_GOLDEN)
        .expect("golden fixture must parse as JSON");

    let mut diverged = compaction_federation_golden_decision();
    diverged.selection_receipt.selection_hash = "sha256:tampered0000000000000000".into();

    let wire = serde_json::to_value(&diverged).unwrap();
    assert_ne!(
        wire, golden_value,
        "diverging payload must not match the golden fixture"
    );
}

#[test]
fn compaction_federation_decision_replayed_after_expiry_is_refused() {
    // Negative control: "Commit after expiry fails." (CRA-09 negativeControls[0])
    let decision = compaction_federation_golden_decision();
    let issued = decision.issued_monotonic_elapsed_ms;
    let expiry_bound = decision.expires_after_elapsed_ms;
    let same_epoch = decision.restart_epoch;

    // Still inside the server-issued window: live.
    assert!(decision.is_live(issued + expiry_bound, same_epoch));

    // One monotonic millisecond past the bound: must be refused. This is
    // checked purely against monotonic elapsed time, never a wall clock.
    let now_past_expiry = issued + expiry_bound + 1;
    assert!(
        !decision.is_live(now_past_expiry, same_epoch),
        "a decision replayed after its server-issued expiry must be refused"
    );
}

#[test]
fn compaction_federation_decision_second_commit_is_refused() {
    // Negative control: "Reuse of a decision fails." (CRA-09 negativeControls[1])
    //
    // The wire type itself is stateless; one-use is enforced by the
    // consumer's local commit ledger. Simulate that ledger here the same
    // way a real consumer would: check `is_live`, then commit-once by
    // identity.
    use std::collections::HashSet;

    let decision = compaction_federation_golden_decision();
    let now = decision.issued_monotonic_elapsed_ms;
    let epoch = decision.restart_epoch;

    let mut committed: HashSet<String> = HashSet::new();

    fn try_commit(
        decision: &crate::event::CompactionFederationDecisionV1,
        now_monotonic_elapsed_ms: u64,
        current_restart_epoch: u64,
        committed: &mut HashSet<String>,
    ) -> bool {
        if !decision.is_live(now_monotonic_elapsed_ms, current_restart_epoch) {
            return false;
        }
        // one-use: refuse if this identity was already committed
        committed.insert(decision.identity.clone())
    }

    assert!(
        try_commit(&decision, now, epoch, &mut committed),
        "first commit of a live, unused decision must succeed"
    );
    assert!(
        !try_commit(&decision, now, epoch, &mut committed),
        "a second commit of the same one-use decision must be refused"
    );
}

#[test]
fn compaction_federation_decision_pending_across_restart_is_invalidated() {
    // Negative control: restart invalidates a pending decision — required by
    // the compaction-federation-v1 contract ("restart invalidates pending
    // decision"), proven here rather than left to prose.
    let decision = compaction_federation_golden_decision();
    let now = decision.issued_monotonic_elapsed_ms + 1; // well inside expiry
    let issuing_epoch = decision.restart_epoch;

    // Same epoch: decision is still honourable.
    assert!(decision.is_live(now, issuing_epoch));

    // A restart bumps the monotonic-clock epoch. The decision was pending
    // across that restart and must now be invalidated, not honoured, even
    // though its elapsed-time expiry window has not lapsed.
    let post_restart_epoch = issuing_epoch + 1;
    assert!(
        !decision.is_live(now, post_restart_epoch),
        "a decision pending across a restart must be invalidated, not honoured"
    );
}

#[test]
fn persistence_failed_round_trips_as_json_and_is_live_only() {
    let event = ProtocolEvent::PersistenceFailed {
        session_id: "s1".into(),
        seq: 21,
        created_at: "t".into(),
        reason: "disk full".into(),
    };
    let json = serde_json::to_string(&event).unwrap();
    let decoded: ProtocolEvent = serde_json::from_str(&json).unwrap();
    // ProtocolEvent has no PartialEq: compare the serialized strings.
    assert_eq!(serde_json::to_string(&decoded).unwrap(), json);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["$kind"], "persistenceFailed");
    assert_eq!(value["sessionId"], "s1");
    assert_eq!(value["reason"], "disk full");
    assert_eq!(decoded.seq(), 21);
    assert_eq!(decoded.session_id(), "s1");
    assert!(!decoded.is_durable_fact());
}

#[test]
fn protocol_event_background_shell_exited_wire_shape_roundtrip_and_durability() {
    use crate::ShellExitStatus;
    let ev = ProtocolEvent::BackgroundShellExited {
        session_id: "s1".into(),
        seq: 9,
        created_at: "t".into(),
        shell_id: "bg-1".into(),
        command: "sleep 1".into(),
        status: ShellExitStatus::Exited { code: 2 },
        output_bytes: 12,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert_eq!(
        json,
        r#"{"$kind":"backgroundShellExited","sessionId":"s1","seq":9,"createdAt":"t","shellId":"bg-1","command":"sleep 1","status":{"state":"exited","code":2},"outputBytes":12}"#
    );
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(serde_json::to_string(&rt).unwrap(), json);
    assert!(
        ev.is_durable_fact(),
        "a settled exit is canonical replay truth"
    );
    assert_eq!(ev.seq(), 9);
    assert_eq!(ev.session_id(), "s1");
}

#[test]
fn shell_exit_status_variants_serialize_with_typed_state() {
    use crate::ShellExitStatus;
    let cases = [
        (
            ShellExitStatus::Signaled { signal: Some(9) },
            r#"{"state":"signaled","signal":9}"#,
        ),
        (
            ShellExitStatus::KilledByTimeout { timeout_ms: 500 },
            r#"{"state":"killedByTimeout","timeoutMs":500}"#,
        ),
        (
            ShellExitStatus::KilledByCancel,
            r#"{"state":"killedByCancel"}"#,
        ),
    ];
    for (status, expected) in cases {
        assert_eq!(serde_json::to_string(&status).unwrap(), expected);
        assert_eq!(
            serde_json::from_str::<ShellExitStatus>(expected).unwrap(),
            status
        );
    }
}
