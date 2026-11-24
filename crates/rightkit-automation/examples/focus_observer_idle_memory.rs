//! Idle probe for the macOS focus-observer thread, in two modes, so the process can be watched from
//! outside (`footprint -p <pid>`, `sample <pid>`, `top -pid <pid>`):
//!   focus_observer_idle_memory [seconds]          idle: no coverage armed. Must show NO
//!       `macos-focus-continuity` thread and no periodic wakeups (the on-demand contract).
//!   focus_observer_idle_memory [seconds] cover    coverage armed for the whole run: the 40 ms
//!       observer cycle runs; a per-cycle leak shows up here as steady growth.

#[cfg(target_os = "macos")]
fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(90);
    rightkit_automation::desktop::start_focus_observer();
    if std::env::args().nth(2).as_deref() == Some("cover") {
        let ready = rightkit_automation::desktop::begin_focus_coverage();
        println!("coverage ready={ready}");
    }
    println!("pid {}", std::process::id());
    std::thread::sleep(std::time::Duration::from_secs(seconds));
}

#[cfg(not(target_os = "macos"))]
fn main() {}
