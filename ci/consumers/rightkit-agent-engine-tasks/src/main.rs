// rightkit-agent-engine-tasks 0.1.0 consumed by exact registry pin: task graph ordering and readiness. No I/O.
use rightkit_agent_engine_tasks::{TaskGraph, TaskNode, TaskState};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn node(id: &str, deps: &[&str]) -> TaskNode {
    serde_json::from_value(serde_json::json!({
        "id": id, "title": id, "deps": deps, "doneWhen": "", "state": "open",
        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"
    }))
    .or_else(|_| {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": id, "deps": deps, "done_when": "", "state": "Open",
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
        }))
    })
    .expect("task node json")
}

fn main() {
    let mut g = TaskGraph::new();
    check(
        g.add_task(node("a", &[])).is_ok() && g.add_task(node("b", &["a"])).is_ok(),
        "add_task accepts a dependency chain",
    );
    let order = g.topological_order().expect("acyclic");
    check(
        order == vec!["a".to_string(), "b".to_string()],
        "topological_order puts deps first",
    );
    g.mark_done("a").expect("mark a done");
    g.recompute_states();
    check(
        g.get("a")
            .map(|t| t.state == TaskState::Done)
            .unwrap_or(false),
        "mark_done sets Done",
    );
}
