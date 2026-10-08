//! macOS integration test for `launch_offscreen`, using the tiny `offscreen_fixture` example
//! (Accessory-policy NSApplication, one unordered NSWindow + WKWebView; no Dock icon, no activation).
//!
//! `#[ignore]` by default because it creates a real (off-screen) window and needs a window server
//! session. Run explicitly, Mac only:
//!   rightkit cargo test -p rightkit-control --features offscreen-private-spi --test offscreen_macos -- --ignored
//! The window is placed past the right edge of ALL displays and never ordered on a display; the fixture
//! exits on its own after 90 s and `OffscreenLaunch` kills it on drop/end. Fatal signals in the fixture
//! exit silently (no crash dialog). The test asserts the frontmost app did not change.
#![cfg(all(target_os = "macos", feature = "offscreen-private-spi"))]

use rightkit_control::mac;
use rightkit_control::offscreen::{launch_offscreen, LaunchSpec};
use std::time::Duration;

fn fixture() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/offscreen_fixture")
}

#[test]
#[ignore = "creates a real off-screen window; run explicitly on a Mac with a window server"]
fn launches_offscreen_with_native_identity() {
    let front_before = mac::frontmost_pid();
    let mut spec = LaunchSpec::new(fixture());
    spec.report_timeout = Duration::from_secs(30);
    let launch = launch_offscreen(&spec).expect("launch_offscreen");
    let r = launch.report.clone();

    assert!(r.qualified, "app qualified itself");
    assert_eq!(r.pid, launch.pid());
    assert!(r.backing_scale >= 1.0);
    assert!(
        r.placed.x > r.original.x,
        "moved to the right of every display"
    );

    // Native identity: the reported window id exists, belongs to the child, and sits at the placed x (the
    // window server reported ~5 pt of drift from the requested origin on the dev Mac; still far off-display).
    // The CG window list can lag the window server by a moment (seen once as a transient x=0), so poll.
    let mut x = f64::NAN;
    for _ in 0..40 {
        let win = mac::windows(r.pid as i32)
            .into_iter()
            .find(|w| u64::from(w.id) == r.window_id)
            .expect("window id present in the window list");
        x = win.x;
        if (x - r.placed.x).abs() <= 10.0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        (x - r.placed.x).abs() <= 10.0,
        "CG bounds x {x} vs placed {}",
        r.placed.x
    );

    assert_eq!(mac::frontmost_pid(), front_before, "focus untouched");
    launch.end().unwrap();
}

#[test]
#[ignore = "creates a real off-screen window; run explicitly on a Mac with a window server"]
fn identity_cross_check_survives_after_end() {
    let launch = launch_offscreen(&LaunchSpec::new(fixture())).expect("launch_offscreen");
    let pid = launch.pid() as i32;
    launch.end().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(mac::windows(pid).is_empty(), "no window left after end");
}
