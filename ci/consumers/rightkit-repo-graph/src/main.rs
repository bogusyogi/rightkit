//! Registry consumer for rightkit-repo-graph 0.1.0: builds against the published API only.
//! It does not index a repository; the crate's own tests do.
use rightkit_repo_graph::{lifecycle_state_for_error, PROTOCOL_VERSION};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(PROTOCOL_VERSION == 1, "the graph protocol version is 1");
    let state = lifecycle_state_for_error("consumer-check: unknown error");
    println!("ok: an unknown error maps to {state:?}");
    println!("rightkit-repo-graph consumer: all checks passed");
}
