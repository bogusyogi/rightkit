//! Registry consumer for rightkit-agent-task-journal 0.1.0: builds against the published API only.
use rightkit_agent_task_journal::{
    Lane, DEFAULT_LINK_PRUNE_MARGIN_MS, JOURNAL_SCHEMA_VERSION, MAX_ATTEMPTS, TOPICS,
    UNAVAILABLE_CODE,
};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        JOURNAL_SCHEMA_VERSION == 2,
        "the journal schema version is the published one",
    );
    check(MAX_ATTEMPTS == 3, "a task gets a bounded number of runs");
    check(
        TOPICS.len() == 10 && TOPICS.iter().all(|t| t.starts_with("task.")),
        "every lifecycle topic is a task topic",
    );
    check(
        UNAVAILABLE_CODE == "journal_unavailable",
        "the unavailable refusal code is stable",
    );
    check(
        DEFAULT_LINK_PRUNE_MARGIN_MS == 30_000,
        "the replay prune margin is the published one",
    );
    check(
        Lane::General == Lane::General && Lane::General != Lane::Agent,
        "lanes compare",
    );
    println!("rightkit-agent-task-journal consumer: all checks passed");
}
