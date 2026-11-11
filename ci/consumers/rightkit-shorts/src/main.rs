// rightkit-shorts 0.2.0 consumed by exact registry pin, outside the workspace.
//
// Proves the configuration and budgeting a host relies on: the default profile validates, names
// a primary platform whose duration window is ordered, round-trips through JSON, refuses an
// unknown platform, and the LLM request planner splits a long transcript into ordered, bounded requests.
// Does not prove: proposing shorts from a real transcript, LLM calls, rendering.
use rightkit_shorts::{plan_requests, ShortsProfile};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let profile = ShortsProfile::default();
    check(profile.validate().is_ok(), "the default profile validates");
    let p = profile.primary();
    check(
        p.min_duration_ms <= p.ideal_min_ms
            && p.ideal_min_ms <= p.ideal_max_ms
            && p.ideal_max_ms <= p.max_duration_ms,
        "the primary platform's duration window is ordered",
    );
    check(
        profile.platform("no-such-platform").is_err(),
        "an unknown platform is refused",
    );
    let back = ShortsProfile::from_json(&profile.to_json());
    check(
        matches!(&back, Ok(b) if b.primary().id == p.id),
        "the profile round-trips through JSON",
    );
    let lines = vec![120usize; 50];
    let plan = plan_requests(&lines, 400, 2_000, 40);
    let covered: usize = plan.requests.iter().map(|r| r.len()).sum();
    let ordered = plan.requests.windows(2).all(|w| w[0].end <= w[1].start);
    check(
        !plan.requests.is_empty() && covered <= lines.len() && ordered,
        "the request planner splits a long transcript into ordered requests",
    );
    check(
        plan.requests.iter().all(|r| r.len() <= 40),
        "no request exceeds the per-request limit",
    );
    println!("rightkit-shorts consumer: all checks passed");
}
