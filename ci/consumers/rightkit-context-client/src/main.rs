//! Registry consumer for rightkit-context-client 0.1.0: builds against the published API only.
//! It never starts a resident: it only proves the no-resident fallback signal.
use rightkit_context_client::{
    try_resident, ClientError, CONNECT_BUDGET, DEFAULT_REQUEST_DEADLINE,
};
use rightkit_context_protocol::ResidentOp;

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        CONNECT_BUDGET < DEFAULT_REQUEST_DEADLINE,
        "connect budget is shorter than the request deadline",
    );
    let dir = std::env::temp_dir().join(format!(
        "rightkit-context-client-consumer-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let answer = try_resident(&dir, ResidentOp::Status, serde_json::json!({}));
    check(
        matches!(answer, Err(ClientError::NotRunning { .. })),
        "no resident means NotRunning, nothing sent",
    );
    let _ = std::fs::remove_dir_all(&dir);
    println!("rightkit-context-client consumer: all checks passed");
}
