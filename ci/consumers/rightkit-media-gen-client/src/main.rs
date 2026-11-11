//! Registry consumer for rightkit-media-gen-client 0.1.0: builds against the published API only.
//! It does not start a generation engine (that needs an installed engine and provider keys).
use rightkit_media_gen_client::{
    parse_label_durations, progress_event, DurationRule, Error, GenerationClient, Orchestrator,
    ProcessRunner, DEFAULT_CONSUMER,
};
use serde_json::json;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        DEFAULT_CONSUMER == "rightkit",
        "the default consumer is product-neutral",
    );
    let dir = std::env::temp_dir().join(format!(
        "rightkit-media-gen-client-consumer-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let missing = dir.join("no-such-engine");
    let orch = Orchestrator::for_engine(&dir, &missing).with_consumer("consumer-check");
    check(
        orch.ensure_ready().is_err(),
        "a missing engine is refused, not run",
    );
    check(
        orch.requests().map(|r| r.is_empty()).unwrap_or(true),
        "a fresh project has no requests",
    );
    let relative = GenerationClient::new(ProcessRunner::new("relative-engine"));
    check(
        matches!(relative.check_capabilities(), Err(Error::Spawn { .. })),
        "a relative engine path is refused before anything runs",
    );
    let ev = progress_event(
        &json!({"event": "job.progress", "job_id": "j1", "status": "running", "stage": "render", "attempt": 1, "at": "t"}),
        Some("r1"),
        None,
    );
    check(
        ev["event"] == "media_gen.job_progress" && ev["request_id"] == "r1",
        "progress lines become media_gen events",
    );
    check(
        parse_label_durations("Model (5 or 10 s)") == DurationRule::Set { secs: vec![5, 10] },
        "label durations parse",
    );
    let _ = std::fs::remove_dir_all(&dir);
    println!("rightkit-media-gen-client consumer: all checks passed");
}
