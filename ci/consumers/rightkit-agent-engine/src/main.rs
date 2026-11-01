//! Registry consumer for rightkit-agent-engine 0.1.0: builds against the published API only.
use rightkit_agent_engine::{is_legal_transition, transition, TaskStatus};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        TaskStatus::Succeeded.is_terminal() && !TaskStatus::Uncertain.is_terminal(),
        "terminal statuses are succeeded, failed and cancelled only",
    );
    check(
        !is_legal_transition(TaskStatus::Succeeded, TaskStatus::Running)
            && transition(TaskStatus::Succeeded, TaskStatus::Running).is_err(),
        "a finished task cannot restart",
    );
    check(
        serde_json::to_string(&TaskStatus::WaitingDevice)
            .ok()
            .as_deref()
            == Some("\"waiting-device\""),
        "statuses serialise in kebab-case",
    );
}
