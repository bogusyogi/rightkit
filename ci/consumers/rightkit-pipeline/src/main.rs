//! Registry consumer for rightkit-pipeline 0.2.0 (deprecated; re-exports rightkit-media-gen-client):
//! builds against the published API only. It does not start a generation engine.
#![allow(deprecated)]
use rightkit_pipeline::{
    parse_label_durations, GenRightClient, Orchestrator, ProcessRunner, DEFAULT_CONSUMER,
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
        DEFAULT_CONSUMER == "rightkit",
        "the default consumer is product-neutral",
    );
    let dir =
        std::env::temp_dir().join(format!("rightkit-pipeline-consumer-{}", std::process::id()));
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
    let _ = parse_label_durations;
    let _client: GenRightClient<ProcessRunner> = GenRightClient::new(ProcessRunner::new(&missing));
    let _ = std::fs::remove_dir_all(&dir);
    println!("rightkit-pipeline consumer: all checks passed");
}
