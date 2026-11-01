//! Registry consumer for rightkit-agent-demo 0.1.0: public API only, no Tauri runtime, no recording.
use rightkit_agent_demo::catalog::{denied_ids, find, CATALOG};
use rightkit_agent_demo::session::{new_session_id, next, Phase, Step};
use rightkit_agent_demo::DemoIdentity;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        next(Phase::Idle, Step::Request) == Some(Phase::AwaitingConsent),
        "a request from idle waits for consent",
    );
    check(
        next(Phase::Idle, Step::Started).is_none(),
        "recording cannot start without consent",
    );
    check(
        next(Phase::Recording, Step::Kill) == Some(Phase::Killed),
        "a kill ends a recording",
    );
    let id = new_session_id().expect("session id");
    check(id.starts_with("demo-"), "session ids are demo-prefixed");
    check(!CATALOG.is_empty() && find("no.such.app").is_none(), "unknown apps are not in the catalog");
    let identity = DemoIdentity::new("Example", "example", "com.example.app", "Example.EXE");
    check(identity.own_ids.1 == "example.exe", "the host exe name is lowercased");
    check(
        CATALOG.iter().all(|e| denied_ids().all(|d| d != e.id)),
        "no catalog app is on the deny list",
    );
    println!("rightkit-agent-demo consumer: all checks passed");
}
