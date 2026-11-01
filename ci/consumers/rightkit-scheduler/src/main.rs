// rightkit-scheduler 0.1.0 consumed by exact registry pin: legacy schedule parsing and time zones. No I/O.
use rightkit_scheduler::{normalize_time_zone, ScheduleSpec};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    let spec = ScheduleSpec::from_legacy("once:1700000000000", "UTC");
    check(
        matches!(
            spec,
            Ok(ScheduleSpec::Once {
                at_unix_ms: 1_700_000_000_000
            })
        ),
        "once: legacy spec parses",
    );
    check(normalize_time_zone("UTC").is_ok(), "UTC normalises");
    check(
        normalize_time_zone("Not/AZone").is_err(),
        "unknown zones are refused",
    );
}
