// rightkit-evidence-decisions consumed by the exact registry pin, outside the workspace.
// Proves: an in-memory store initialises, inserts a record once (idempotent) and reads it back.
use rightkit_evidence_decisions::{
    DecisionRecord, DecisionStatus, DecisionStore, InsertDisposition,
};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let mut store = DecisionStore::in_memory().expect("in-memory store");
    store.initialize().expect("initialize");
    let record = DecisionRecord::new(
        "repo-a",
        "scope-1",
        "task-1",
        "because reasons",
        DecisionStatus::Accepted,
    );
    check(!record.id.is_empty(), "record id is derived");
    check(
        matches!(store.insert(&record), Ok(InsertDisposition::Inserted)),
        "first insert inserts",
    );
    check(
        matches!(store.insert(&record), Ok(InsertDisposition::AlreadyPresent)),
        "second insert is idempotent",
    );
    check(
        store.get(&record.id).ok().flatten().is_some(),
        "record reads back by id",
    );
}
