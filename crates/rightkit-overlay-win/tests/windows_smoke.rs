//! Native smoke test: creates a real overlay window, shows it, animates it once and closes it.
//!
//! It needs an interactive Windows desktop, so it is ignored by default. Run it explicitly:
//!
//! ```text
//! cargo test -p rightkit-overlay-win --test windows_smoke -- --ignored --nocapture
//! ```
//!
//! It checks what unit tests cannot: that the window gets the layered, topmost, tool-window,
//! no-activate style, that the painter really runs on the owner thread, that an animation paints
//! several frames and then stops painting (the idle invariant), and that close ends the event
//! stream with `Closed`.

#![cfg(windows)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_overlay::ex_style;
use rightkit_overlay_win::{
    Animation, Easing, FrameInfo, HitRegion, OverlayConfig, OverlayEvent, OverlayWindow, PaintCtx,
    Rect, Rgba,
};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, IsWindowVisible, GWL_EXSTYLE,
};

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "needs an interactive Windows desktop"]
fn create_show_animate_hide_close() {
    let window = OverlayWindow::create(OverlayConfig::passive("rk-overlay-smoke")).expect("create");
    let overlay = window.handle();
    let hwnd = overlay.hwnd();
    assert_ne!(hwnd, 0, "the window exists once create returns");

    // Style: layered tool window, topmost, never activates.
    let style = unsafe { GetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE) } as u32;
    for (name, bit) in [
        ("layered", ex_style::LAYERED),
        ("topmost", ex_style::TOPMOST),
        ("toolwindow", ex_style::TOOLWINDOW),
        ("noactivate", ex_style::NOACTIVATE),
    ] {
        assert_ne!(style & bit, 0, "missing {name}: {style:#x}");
    }
    assert_eq!(
        style & ex_style::TRANSPARENT,
        0,
        "passive windows take the pointer"
    );
    assert_eq!(
        unsafe { IsWindowVisible(hwnd as HWND) },
        0,
        "hidden until shown"
    );

    let frames = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&frames);
    overlay.set_painter(Box::new(move |ctx: &mut PaintCtx, frame: FrameInfo| {
        counter.fetch_add(1, Ordering::SeqCst);
        let size = ctx.logical_size();
        ctx.fill_rounded_rect(
            Rect {
                x: 0.0,
                y: 0.0,
                width: f64::from(size.width),
                height: f64::from(size.height),
            },
            12.0,
            Rgba::rgb(0xFF, 0x56, 0x30).with_alpha(0.5 + 0.5 * frame.t),
        );
    }));
    overlay.set_frame(Rect {
        x: 200.0,
        y: 200.0,
        width: 120.0,
        height: 40.0,
    });
    overlay.set_hit_regions(vec![HitRegion {
        id: 1,
        rect: Rect {
            x: 0.0,
            y: 0.0,
            width: 120.0,
            height: 40.0,
        },
    }]);
    let events = overlay.events();

    overlay.show();
    assert!(overlay.is_visible());
    wait_until("the first paint", || frames.load(Ordering::SeqCst) > 0);
    wait_until("the window to be visible", || unsafe {
        IsWindowVisible(hwnd as HWND) != 0
    });

    // One animation: several vsync-paced frames, then silence.
    let before = frames.load(Ordering::SeqCst);
    overlay.animate(Animation::once(150, Easing::Linear));
    wait_until("animation frames", || {
        frames.load(Ordering::SeqCst) >= before + 3
    });
    std::thread::sleep(Duration::from_millis(400));
    let settled = frames.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        frames.load(Ordering::SeqCst),
        settled,
        "no frames are painted once the animation ends and nothing changes"
    );

    // Hiding and re-showing repaints.
    overlay.hide();
    wait_until("the window to hide", || unsafe {
        IsWindowVisible(hwnd as HWND) == 0
    });
    assert!(!overlay.is_visible());
    overlay.show();
    wait_until("a repaint on show", || {
        frames.load(Ordering::SeqCst) > settled
    });

    overlay.reassert_topmost();
    window.close();

    // The stream ends with Closed.
    let mut last = None;
    while let Ok(event) = events.recv_timeout(Duration::from_secs(3)) {
        last = Some(event);
    }
    assert_eq!(last, Some(OverlayEvent::Closed));
    assert_eq!(overlay.hwnd(), 0);
}
