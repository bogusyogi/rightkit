use rightkit_dictation_pill::guardian::{Coalescer, GuardianEvent};
use rightkit_dictation_pill::{EdgeTolerance, GuardianTimings};
use std::time::Instant;

fn main() {
    let timings = GuardianTimings {
        focus_coalesce_ms: 40,
        geometry_coalesce_ms: 150,
        settle_pass_ms: 500,
        degraded_repair_interval_ms: 30_000,
        probe_min_interval_ms: 30_000,
        ax_messaging_timeout_ms: 100,
        os_source_start_timeout_ms: 3000,
    };
    assert!(timings.is_valid());
    assert!(EdgeTolerance::new(2.0).is_some());
    assert!(EdgeTolerance::new(f64::INFINITY).is_none());

    let mut scheduler = Coalescer::new(timings);
    assert!(scheduler.is_idle());
    scheduler.note(GuardianEvent::ForegroundChanged, Instant::now());
    let evaluation = scheduler.next_deadline().expect("focus arms an evaluation");
    assert!(scheduler.take_due(evaluation).any());
    let settle = scheduler.next_deadline().expect("focus arms a settle pass");
    assert!(scheduler.take_due(settle).any());
    assert!(scheduler.is_idle());

    println!("OK rightkit-dictation-pill 0.1.0 (registry consumer)");
}
