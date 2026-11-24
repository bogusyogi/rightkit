//! End to end on one store file: two registries opened on the same `runtime-middleware.json`
//! (two processes of one account, CodeRight #539). A guard switched back on by one is not
//! switched off again by the other's older snapshot, and both writers' spans survive.

use rightkit_agent_engine_runtime_middleware::{
    MiddlewarePhase, RuntimeMiddlewareRegistry, RuntimeSpan,
};

const GUARD: &str = "builtin.guard-tool-arguments";

fn span(id: &str, session: &str) -> RuntimeSpan {
    RuntimeSpan {
        id: id.to_string(),
        parent_id: None,
        session_id: session.to_string(),
        turn_id: None,
        phase: MiddlewarePhase::PreToolCall,
        status: "ok".to_string(),
        started_at: "2026-11-22T00:00:00Z".to_string(),
        ended_at: None,
        attributes: serde_json::json!({}),
    }
}

fn enabled(registry: &RuntimeMiddlewareRegistry, id: &str) -> bool {
    registry
        .middleware()
        .into_iter()
        .find(|d| d.id == id)
        .map(|d| d.enabled)
        .expect("the guard is a built-in")
}

#[test]
fn a_guard_switched_back_on_stays_on_and_both_writers_spans_survive() {
    let dir = std::env::temp_dir().join(format!("rk-middleware-two-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("runtime-middleware.json");

    let mut first = RuntimeMiddlewareRegistry::with_store_path(&path);
    first.set_enabled(GUARD, false).expect("guard exists");
    // The second writer starts while the guard is off.
    let mut second = RuntimeMiddlewareRegistry::with_store_path(&path);
    assert!(
        !enabled(&second, GUARD),
        "the second writer should load the guard as off"
    );

    first.set_enabled(GUARD, true).expect("guard exists");
    first.record_span(span("span-first", "session-a"));
    first.flush();
    // The second writer still holds its older snapshot (guard off) and writes a span.
    second.record_span(span("span-second", "session-b"));
    second.flush();

    let reopened = RuntimeMiddlewareRegistry::with_store_path(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        enabled(&reopened, GUARD),
        "the guard came back off after the older writer wrote"
    );
    let ids: Vec<String> = reopened.spans(None).into_iter().map(|s| s.id).collect();
    assert!(
        ids.contains(&"span-first".to_string()),
        "the first writer's span was dropped: {ids:?}"
    );
    assert!(
        ids.contains(&"span-second".to_string()),
        "the second writer's span was dropped: {ids:?}"
    );
    // The older writer now sees the guard as on too.
    assert!(
        enabled(&second, GUARD),
        "the second writer did not pick up the guard switched on"
    );
}
