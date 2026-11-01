//! Registry consumer for rightkit-agent-transcript 0.1.0: builds against the published API only.
use rightkit_agent_transcript::{detect_host, PARSER_VERSION};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(PARSER_VERSION.ends_with(".v1"), "the parser version is a v1 id");
    let dir = std::env::temp_dir().join(format!("rightkit-agent-transcript-consumer-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("session.jsonl");
    std::fs::write(&file, "{\"type\":\"session_meta\",\"payload\":{}}\n").expect("write fixture");
    check(detect_host(&file).ok() == Some("codex"), "a session_meta line is a Codex transcript");
    check(detect_host(&dir.join("missing.jsonl")).is_err(), "a missing transcript is an error");
    let _ = std::fs::remove_dir_all(&dir);
    println!("rightkit-agent-transcript consumer: all checks passed");
}
