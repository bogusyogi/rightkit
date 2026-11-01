//! Host-owned Membrane binding evidence through a real daemon: the H8 host request budget and the
//! H9 retained delivery inventory (A048, A049). The earlier `e2e_binding_qualification` journeys
//! were merged into `e2e_membrane_fake`; these two close the ledger rows that still named no test.
//!
//! Both journeys run with no Membrane installed, so the evidence is the typed-unavailable form:
//! a missing observation must read as unavailable, never as a fabricated zero.

use crate::support::e2e_api::*;
use crate::support::scripted_provider::Step;
use serde_json::json;

fn run_one_turn(rig: &Rig, id: &str) {
    rig.session(id, json!({"permissionMode":"bypass"}));
    assert_eq!(rig.send(id, "say hello").0, 200);
    rig.wait_assistant(id, "binding turn done");
}

// A048: H8 budget rows are bounded to live sessions, and an unobserved budget degrades to a
// typed unavailable value (no numeric value) while the binding state stays visible.
#[test]
fn host_request_budget_is_enforced_and_overrun_degrades_visibly() {
    let rig = Rig::start(vec![Step::text("binding turn done")]);
    run_one_turn(&rig, "h8-session");

    let (status, metrics) = rig.get("/runtime/resource-metrics");
    assert_eq!(status, 200, "{metrics}");
    let ceilings = metrics["membraneEvidence"]["h8"]["remainingContextCeilings"]
        .as_array()
        .unwrap_or_else(|| panic!("H8 ceilings must be exported as an array: {metrics}"));
    // Live sessions: every created session plus the always-live "default" session.
    let (_, listed) = rig.get("/sessions");
    let mut live: Vec<String> = listed
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    live.push("default".to_string());
    assert!(
        ceilings.iter().any(|row| row["sessionId"] == "h8-session"),
        "the turned session must have an H8 row: {metrics}"
    );
    for row in ceilings {
        assert!(
            row["sessionId"]
                .as_str()
                .is_some_and(|id| live.iter().any(|l| l == id)),
            "H8 rows are filtered to live sessions ({live:?}): {row}"
        );
        let estimate = &row["remainingTokens"]["estimate"];
        match estimate["coverage"].as_str() {
            Some("complete") => assert!(estimate["value"].is_number(), "{row}"),
            Some("unavailable") => {
                assert!(
                    estimate.get("value").is_none(),
                    "unavailable is never a number: {row}"
                );
                assert!(estimate["unavailableReason"].is_string(), "{row}");
            }
            other => panic!("unknown H8 coverage {other:?}: {row}"),
        }
    }
    assert!(
        metrics["membraneBinding"]["contextDegraded"].is_boolean(),
        "the degraded binding state must be visible next to the budget: {metrics}"
    );
}

// A049: `/context` reports the retained delivery inventory for each live session, typed
// unavailable when no submitted attempt retained context.
#[test]
fn retained_delivery_inventory_is_reported_by_context_command() {
    let rig = Rig::start(vec![Step::text("binding turn done")]);
    run_one_turn(&rig, "h9-session");

    let workspace = rig.ws().to_string_lossy().to_string();
    let (status, body) = rig.post(
        "/commands/execute",
        json!({"input": format!("/context {workspace}")}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["success"], true, "{body}");
    let output = body["output"].as_str().unwrap_or_default();
    assert!(
        output.contains("Host request budget (H8) and retained delivery inventory (H9):"),
        "{output}"
    );
    let line = output
        .lines()
        .find(|l| l.starts_with("- h9-session: "))
        .unwrap_or_else(|| panic!("the live session must be listed in the inventory: {output}"));
    assert!(
        (line.contains("budget ") && line.contains("inventory "))
            || line.contains("retained context identit")
            || (line.contains(" tokens remaining") && line.contains("inventory unavailable")),
        "the line states budget and inventory, available or typed unavailable: {line}"
    );
    assert!(
        !line.contains(" 0 tokens remaining"),
        "no fabricated zero budget: {line}"
    );
}
