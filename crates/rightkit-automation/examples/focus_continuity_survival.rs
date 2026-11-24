//! Measures how often a start-time focus-continuity token survives against
//! the live frontmost app. Read-only: no activation, input, or AX writes.
//! Usage: focus_continuity_survival [seconds]
//!
//! Run (macOS only): cargo run -p rightkit-automation --features desktop-context --example focus_continuity_survival

#[cfg(all(target_os = "macos", feature = "desktop-context"))]
fn main() {
    use std::time::{Duration, Instant};
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    rightkit_automation::desktop::start_focus_observer();
    // Observer is on demand: cover for the probe's whole run (no `end`; the process exits).
    rightkit_automation::desktop::begin_focus_coverage();
    std::thread::sleep(Duration::from_millis(1_500));
    let delays_ms = [20u64, 200, 1_500];
    let (mut attempts, mut no_token, mut settled) = (0u32, 0u32, 0u32);
    let mut survived = [0u32; 3];
    let mut app = String::new();
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        let Some(front) = rightkit_automation::desktop::macos::frontmost_fast() else {
            continue;
        };
        let Some(window) =
            rightkit_automation::desktop::macos::frontmost_window_identity(front.pid)
        else {
            continue;
        };
        app = front.bundle.unwrap_or_default();
        attempts += 1;
        let Some((captured_at, instance)) =
            rightkit_automation::desktop::capture_focus_continuity(front.pid as u32, window)
        else {
            no_token += 1;
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        if rightkit_automation::desktop::validate_focus_continuity_settled(
            front.pid as u32,
            window,
            captured_at,
            instance,
            None,
            Duration::from_millis(150),
        ) {
            settled += 1;
        }
        let started = Instant::now();
        for (slot, delay) in delays_ms.iter().enumerate() {
            std::thread::sleep(Duration::from_millis(*delay).saturating_sub(started.elapsed()));
            if rightkit_automation::desktop::validate_focus_continuity(
                front.pid as u32,
                window,
                captured_at,
                instance,
                None,
            ) {
                survived[slot] += 1;
            }
        }
    }
    println!(
        "app={app} attempts={attempts} no_token={no_token} tokens={} settled_immediately={settled} survived_20ms={} survived_200ms={} survived_1500ms={}",
        attempts - no_token, survived[0], survived[1], survived[2]
    );
}

#[cfg(not(all(target_os = "macos", feature = "desktop-context")))]
fn main() {}
