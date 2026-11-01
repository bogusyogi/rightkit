//! Registry consumer for rightkit-privileged-helper 0.1.0: builds against the published API only.
use rightkit_privileged_helper::{complete, next, run};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let ran = run(false, "/Applications/Example.app", || 7);
    check(ran == 7, "work without elevation runs directly");
    if cfg!(unix) {
        let polled = next(&serde_json::json!({"waitMs": 0})).expect("poll");
        check(polled["request"].is_null(), "an empty poll answers no request");
    } else {
        check(next(&serde_json::json!({})).is_err(), "Windows has no helper");
    }
    check(complete(&serde_json::json!({"id": 999, "moved": []})).is_err(), "an unknown answer is refused");
    println!("rightkit-privileged-helper consumer: all checks passed");
}
