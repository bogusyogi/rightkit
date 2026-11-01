// rightkit-agent-engine-api-platform 0.1.0 consumed by exact registry pin: readiness rows. No I/O.
use rightkit_agent_engine_api_platform::readiness::{
    ReadinessRow, ReadinessState, ReadinessVerdict,
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
    let r = ReadinessRow::ready("db", "Database", "up");
    check(
        r.verdict == ReadinessVerdict::Pass && r.state == ReadinessState::Ready,
        "ready row passes",
    );
    let v = serde_json::to_value(&r).expect("serialize");
    check(v["verdict"] == "pass", "rows serialize camelCase");
}
