use rightkit_agent_engine::{TaskStatus, is_legal_transition, transition};

#[test]
fn terminal_statuses_are_final() {
    assert!(TaskStatus::Succeeded.is_terminal());
    assert!(TaskStatus::Failed.is_terminal());
    assert!(TaskStatus::Cancelled.is_terminal());
    assert!(!TaskStatus::CancelRequested.is_terminal());
    assert!(!TaskStatus::Uncertain.is_terminal());
}

#[test]
fn terminal_status_cannot_restart() {
    assert!(!is_legal_transition(
        TaskStatus::Succeeded,
        TaskStatus::Running
    ));
    assert!(transition(TaskStatus::Succeeded, TaskStatus::Running).is_err());
}

#[test]
fn status_uses_kebab_case_on_the_wire() {
    let text = serde_json::to_string(&TaskStatus::WaitingDevice).expect("serialize");
    assert_eq!(text, "\"waiting-device\"");
}
