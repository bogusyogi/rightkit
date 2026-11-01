// rightkit-agent-engine-mobile-runtime 0.1.0 consumed by exact registry pin: the session journal in a
// per-process directory under the OS temp directory.
use rightkit_agent_engine_mobile_runtime::journal::{Journal, JournalEventKind};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let dir = std::env::temp_dir().join(format!("rk-mobile-consumer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut journal = Journal::open(&dir, "s1").expect("open journal");
    check(
        journal.last_sequence() == 0,
        "a new journal starts at sequence 0",
    );
    let seq = journal
        .append(
            "n1".into(),
            JournalEventKind::UserMessage {
                preview: "hi".into(),
            },
        )
        .expect("append");
    check(seq >= 1, "append assigns a sequence");
    check(
        journal.replay(0).map(|e| e.len() == 1).unwrap_or(false),
        "replay returns the event",
    );
    let _ = std::fs::remove_dir_all(&dir);
}
