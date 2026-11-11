// rightkit-agent-telemetry 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Records into the in-process sinks and reads the aggregates back, then issues a measurement receipt.
// Process memory only; `init` installs a global tracing subscriber and is never called.
use rightkit_agent_telemetry::{
    estimate_tokens, prompt_overhead_aggregates, record_prompt_overhead, record_repair_event,
    record_round_trip, record_turn_latency, record_wakeup, repair_counters,
    reset_prompt_overhead_telemetry, reset_repair_telemetry, reset_resource_telemetry,
    round_trip_aggregates, turn_latency_percentiles, validator_error_kind, wakeup_summary,
    MeasurementError, MeasurementObservation, MeasurementReceipt, MeasurementScenario,
    PromptOverheadEvent, RepairEvent, RoundTripEvent, TurnLatencyEvent, MEASUREMENT_SCHEMA,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn reset_all() {
    reset_resource_telemetry();
    reset_prompt_overhead_telemetry();
    reset_repair_telemetry();
}

fn main() {
    reset_all();

    // Nothing observed is "not measured", never zero.
    check("no samples is none", turn_latency_percentiles().is_none());
    let window = MeasurementObservation::capture(MeasurementScenario::S0, 1_000, 2_000);
    check("empty run is not observed", !window.has_observed_value());

    // Turn latency: nearest-rank percentiles over 1..=100 ms.
    for ms in 1..=100 {
        record_turn_latency(TurnLatencyEvent { latency_ms: ms });
    }
    let p = turn_latency_percentiles();
    check(
        "turn latency percentiles",
        p.map(|p| (p.samples, p.p50_ms, p.p95_ms)) == Some((100, 50, 95)),
    );

    // Round trips per task, ordered by task.
    for task in ["task-b", "task-a", "task-b"] {
        record_round_trip(RoundTripEvent { task: task.into() });
    }
    check(
        "round trips per task",
        round_trip_aggregates() == vec![("task-a".to_string(), 1), ("task-b".to_string(), 2)],
    );

    record_wakeup();
    record_wakeup();
    check("wakeups counted", wakeup_summary().total == 2);

    // Token estimates round up, so a one-byte prompt is never free.
    check(
        "token estimate is conservative",
        estimate_tokens(0) == 0 && estimate_tokens(1) == 1 && estimate_tokens(8) == 2,
    );
    record_prompt_overhead(PromptOverheadEvent {
        provider: Some("provider-a".into()),
        model: Some("model-a".into()),
        system_prompt_bytes: 400,
        tool_prelude_bytes: 40,
    });
    let rows = prompt_overhead_aggregates();
    check(
        "prompt overhead aggregate",
        rows.len() == 1
            && rows[0].0 == (Some("provider-a".into()), Some("model-a".into()))
            && rows[0].1.observations == 1
            && rows[0].1.total_system_prompt_est_tokens == estimate_tokens(400),
    );

    // Repair counters keep only bounded kinds, never the validator message.
    let kind = validator_error_kind("field `paths` must be an array\nschema: {...}");
    check("validator error kind", kind == "type_mismatch");
    check(
        "unknown validator error is other",
        validator_error_kind("something else") == "other",
    );
    let event = RepairEvent {
        provider: None,
        model: None,
        tool: Some("read_file".into()),
        repair_kind: Some("singleton_to_array".into()),
        validator_error: None,
        success: true,
    };
    record_repair_event(event.clone());
    record_repair_event(event);
    let counters = repair_counters();
    check(
        "repair counter",
        counters.len() == 1 && counters[0].count == 2 && counters[0].success,
    );

    // Measurement receipt over the recorded run.
    let observed = MeasurementObservation::capture(MeasurementScenario::S1, 1_000, 2_000);
    check("recorded run is observed", observed.has_observed_value());
    check(
        "empty run id refused",
        MeasurementReceipt::issue(" ", observed.clone()) == Err(MeasurementError::EmptyRunId),
    );
    let reversed = MeasurementObservation::capture(MeasurementScenario::S1, 2_000, 1_000);
    check(
        "reversed window refused",
        MeasurementReceipt::issue("run-1", reversed) == Err(MeasurementError::InvalidWindow),
    );
    match MeasurementReceipt::issue("run-1", observed) {
        Ok(receipt) => {
            check("receipt schema", receipt.schema == MEASUREMENT_SCHEMA);
            check("receipt marked observed", receipt.observed);
            check(
                "receipt digest checks",
                receipt.receipt_digest.starts_with("sha256:") && receipt.has_valid_digest(),
            );
        }
        Err(e) => check(&format!("receipt issues: {e:?}"), false),
    }

    reset_all();
    check(
        "reset clears the sinks",
        turn_latency_percentiles().is_none()
            && round_trip_aggregates().is_empty()
            && prompt_overhead_aggregates().is_empty()
            && repair_counters().is_empty(),
    );
    println!("OK rightkit-agent-telemetry 0.1.0 (registry consumer)");
}
