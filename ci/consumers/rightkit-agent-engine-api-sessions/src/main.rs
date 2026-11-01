// rightkit-agent-engine-api-sessions 0.1.0 consumed by exact registry pin: bounded route validation. No I/O.
use rightkit_agent_engine_api_sessions::validation::{validate_session_id, validate_tag};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(
        validate_session_id("abc").is_ok(),
        "valid session id accepted",
    );
    check(
        validate_session_id("  ").is_err(),
        "blank session id rejected",
    );
    check(
        validate_tag(&"x".repeat(65)).is_err(),
        "over-long tag rejected",
    );
}
