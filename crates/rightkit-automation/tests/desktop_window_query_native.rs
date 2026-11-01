//! Production window-identity route under native frame commits, without an installer.
#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    use objc2::{class, msg_send, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSBackingStoreType, NSWindow, NSWindowStyleMask};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use rightkit_automation::desktop::macos::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    #[link(name = "QuartzCore", kind = "framework")]
    extern "C" {}

    if run_window_query_helper() {
        return;
    }
    static HELPER_PID: AtomicU32 = AtomicU32::new(0);
    set_window_query_diagnostic_hook(|phase, pid, _| {
        if phase == "started" {
            HELPER_PID.store(pid, Ordering::Release);
        }
    });
    let direct = std::env::args().any(|arg| arg == "--in-process");
    if !direct {
        enable_isolated_window_queries();
    }
    let mtm = MainThreadMarker::new().expect("native test main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.finishLaunching();
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(12., 12.), NSSize::new(64., 10.)),
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe {
        window.setReleasedWhenClosed(false);
        window.orderFrontRegardless();
    }
    let pid = std::process::id() as i32;
    let warmup = Instant::now();
    let initial = loop {
        if let Some(window) = frontmost_window_identity(pid) {
            break window;
        }
        assert!(
            warmup.elapsed() < Duration::from_secs(4),
            "test window/helper unavailable"
        );
        core_foundation::runloop::CFRunLoop::run_in_mode(
            unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
            Duration::from_millis(20),
            false,
        );
    };
    let running = Arc::new(AtomicBool::new(true));
    let stop = running.clone();
    let query = std::thread::spawn(move || {
        let mut total = 0;
        let mut misses = 0;
        while stop.load(Ordering::Acquire) {
            match frontmost_window_identity(pid) {
                Some(identity) => assert_eq!(identity, initial, "window ID changed across IPC"),
                None => misses += 1,
            }
            total += 1;
            std::thread::sleep(Duration::from_millis(2));
        }
        (total, misses)
    });
    let duration = if std::env::args().any(|arg| arg == "--stress") {
        20
    } else {
        3
    };
    let started = Instant::now();
    let mut previous = started;
    let mut max_gap = Duration::ZERO;
    let mut ticks = 0;
    while started.elapsed() < Duration::from_secs(duration) {
        let now = Instant::now();
        max_gap = max_gap.max(now - previous);
        previous = now;
        let phase = (ticks % 40) as f64 / 40.;
        unsafe {
            window.setFrame_display(
                NSRect::new(
                    NSPoint::new(12., 12.),
                    NSSize::new(64. + phase * 42., 10. + phase * 26.),
                ),
                true,
            );
            let _: () = msg_send![class!(CATransaction), flush];
        }
        core_foundation::runloop::CFRunLoop::run_in_mode(
            unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
            Duration::from_millis(8),
            false,
        );
        ticks += 1;
    }
    running.store(false, Ordering::Release);
    let (queries, misses) = query.join().unwrap();
    if !direct {
        let old_helper = HELPER_PID.load(Ordering::Acquire);
        assert!(old_helper > 0);
        extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        unsafe {
            assert_eq!(kill(old_helper as i32, 15), 0);
        }
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            frontmost_window_identity(pid).is_none(),
            "crash must not return cached identity"
        );
        let recovery = Instant::now();
        loop {
            if let Some(identity) = frontmost_window_identity(pid) {
                assert_eq!(identity, initial);
                assert_ne!(HELPER_PID.load(Ordering::Acquire), old_helper);
                break;
            }
            assert!(
                recovery.elapsed() < Duration::from_secs(5),
                "helper must recover"
            );
            core_foundation::runloop::CFRunLoop::run_in_mode(
                unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
                Duration::from_millis(20),
                false,
            );
        }
    }
    window.close();
    println!("{{\"mode\":\"{}\",\"ticks\":{ticks},\"queries\":{queries},\"misses\":{misses},\"max_frame_gap_ms\":{}}}", if direct { "in_process" } else { "isolated" }, max_gap.as_millis());
    assert!(
        queries > 30,
        "must exercise repeated production identity queries"
    );
    assert_eq!(misses, 0, "isolation must preserve window identity");
    assert!(
        max_gap < Duration::from_millis(200),
        "main frame loop stalled"
    );
}
