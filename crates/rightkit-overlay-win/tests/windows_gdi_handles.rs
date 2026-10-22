//! GDI handle and thread accounting for the Windows path.
//!
//! Creates, shows, animates, hides and closes many overlays and checks what a unit test cannot:
//! that the process GDI object count (`GetGuiResources(GR_GDIOBJECTS)`) comes back to where it
//! started and no owner thread (`rk-overlay-*`) outlives its window, that a window being resized every frame does not accumulate GDI
//! objects, that overlays sharing one name share one window class, and that the class is
//! unregistered when the last of them closes.
//!
//! It needs an interactive Windows desktop, so it is ignored by default. Run it explicitly:
//!
//! ```text
//! cargo test -p rightkit-overlay-win --test windows_gdi_handles -- --ignored --nocapture
//! ```
//!
//! It is one test on purpose: process-wide counts are only meaningful when nothing else in the
//! process is creating threads or GDI objects at the same time.

#![cfg(windows)]

use std::mem::size_of;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_overlay_win::{
    Align, Animation, Easing, Font, FrameInfo, HitRegion, OverlayConfig, OverlayWindow, PaintCtx,
    Rect, Rgba,
};
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetGuiResources, GetThreadDescription, OpenThread,
    GR_GDIOBJECTS, THREAD_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetClassInfoExW, WNDCLASSEXW};

/// Overlays created, used and closed in the main loop.
const CYCLES: usize = 30;
/// Objects Windows itself may create or free in the process between two readings (cursor and icon
/// caches, theme handles). A leak in this crate costs at least a DC, a DIB and a font per overlay,
/// so over `CYCLES` overlays it is dozens of objects; this slack is far below that.
const GDI_SLACK: u32 = 2;
/// Threads the OS may add to the process once a window has existed (the thread pool behind
/// timers and session notifications, DWM callbacks). They are process-wide, not per overlay: on
/// a Windows 11 desktop the count settles at +2 after the first overlay and stays there over
/// `CYCLES` overlays. A leaked owner thread per overlay would show as +`CYCLES`.
const THREAD_SLACK: usize = 4;
/// The prefix the crate gives its owner threads.
const OWNER_THREAD_PREFIX: &str = "rk-overlay-";
/// The window class name the crate derives from the overlay name.
const CLASS_NAME: &str = "RightKitOverlayWin.rk-overlay-gdi";
const NAME: &str = "rk-overlay-gdi";

fn gdi_objects() -> u32 {
    // SAFETY: the pseudo handle of the current process is always valid.
    unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) }
}

fn thread_count() -> usize {
    // SAFETY: a toolhelp snapshot is created, walked with a correctly sized entry and closed.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        assert!(snapshot != INVALID_HANDLE_VALUE, "thread snapshot failed");
        let pid = GetCurrentProcessId();
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut count = 0;
        let mut more = Thread32First(snapshot, &mut entry) != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                count += 1;
            }
            more = Thread32Next(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        count
    }
}

/// Threads in this process whose description starts with [`OWNER_THREAD_PREFIX`].
fn owner_threads() -> usize {
    // SAFETY: a toolhelp snapshot is walked with a correctly sized entry; each thread handle is
    // opened for query only and closed; the description buffer is freed with `LocalFree`.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        assert!(snapshot != INVALID_HANDLE_VALUE, "thread snapshot failed");
        let pid = GetCurrentProcessId();
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut count = 0;
        let mut more = Thread32First(snapshot, &mut entry) != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                let handle = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, entry.th32ThreadID);
                if !handle.is_null() {
                    let mut description: *mut u16 = std::ptr::null_mut();
                    if GetThreadDescription(handle, &mut description) >= 0 && !description.is_null()
                    {
                        let mut len = 0;
                        while *description.add(len) != 0 {
                            len += 1;
                        }
                        let name =
                            String::from_utf16_lossy(std::slice::from_raw_parts(description, len));
                        if name.starts_with(OWNER_THREAD_PREFIX) {
                            count += 1;
                        }
                        LocalFree(description.cast());
                    }
                    CloseHandle(handle);
                }
            }
            more = Thread32Next(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        count
    }
}

fn class_registered() -> bool {
    let wide: Vec<u16> = CLASS_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: a zeroed WNDCLASSEXW with its size set, filled by the call; the name is
    // NUL-terminated and outlives it.
    unsafe {
        let mut class: WNDCLASSEXW = std::mem::zeroed();
        class.cbSize = size_of::<WNDCLASSEXW>() as u32;
        GetClassInfoExW(
            GetModuleHandleW(std::ptr::null()),
            wide.as_ptr(),
            &mut class,
        ) != 0
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// An overlay that paints glass, a squircle, text and an icon-free rim, counting its frames.
fn open(width: f64) -> (OverlayWindow, Arc<AtomicU32>) {
    let window = OverlayWindow::create(OverlayConfig::passive(NAME)).expect("create");
    let overlay = window.handle();
    let frames = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&frames);
    overlay.set_painter(Box::new(move |ctx: &mut PaintCtx, frame: FrameInfo| {
        counter.fetch_add(1, Ordering::SeqCst);
        let size = ctx.logical_size();
        let body = Rect {
            x: 0.0,
            y: 0.0,
            width: f64::from(size.width),
            height: f64::from(size.height),
        };
        ctx.glass_layer(body, 14.0, 6.0, Rgba::new(33, 29, 26, 235));
        ctx.fill_squircle(
            Rect {
                x: 6.0,
                y: 6.0,
                width: 24.0,
                height: 24.0,
            },
            8.0,
            Rgba::rgb(0xFF, 0x56, 0x30).with_alpha(0.5 + 0.5 * frame.t),
        );
        ctx.text("rk 00:12", Font::ui(13.0), Rgba::WHITE, body, Align::Center);
    }));
    overlay.set_frame(Rect {
        x: 200.0,
        y: 200.0,
        width,
        height: 40.0,
    });
    overlay.set_hit_regions(vec![HitRegion {
        id: 1,
        rect: Rect {
            x: 0.0,
            y: 0.0,
            width,
            height: 40.0,
        },
    }]);
    (window, frames)
}

/// Create, show, animate, hide, show again and close one overlay.
fn one_cycle(width: f64) {
    let (window, frames) = open(width);
    let overlay = window.handle();
    overlay.show();
    wait_until("the first paint", || frames.load(Ordering::SeqCst) > 0);
    let before = frames.load(Ordering::SeqCst);
    overlay.animate(Animation::once(60, Easing::Linear));
    wait_until("animation frames", || {
        frames.load(Ordering::SeqCst) >= before + 2
    });
    // Hiding releases the surface and canvas; showing builds them again.
    overlay.hide();
    let hidden_at = frames.load(Ordering::SeqCst);
    overlay.show();
    wait_until("a repaint after show", || {
        frames.load(Ordering::SeqCst) > hidden_at
    });
    window.close();
}

#[test]
#[ignore = "needs an interactive Windows desktop"]
fn gdi_objects_threads_and_window_class_return_to_baseline() {
    // Warm-up: the first overlay in a process creates process-wide state (the class, theme and
    // cursor caches, the default fonts) that lives until exit and is not a leak.
    one_cycle(120.0);
    // `close` joins the owner thread, and the thread drops the window class before it ends.
    assert!(
        !class_registered(),
        "the class is unregistered after the last window"
    );
    std::thread::sleep(Duration::from_millis(200));
    let gdi_before = gdi_objects();
    let threads_before = thread_count();
    println!("baseline: {gdi_before} GDI objects, {threads_before} threads");

    // Phase 1: sequential overlays of slightly different sizes.
    for i in 0..CYCLES {
        one_cycle(120.0 + (i % 3) as f64 * 10.0);
    }
    wait_until("owner threads to end", || owner_threads() == 0);
    let gdi_after = gdi_objects();
    let threads_after = thread_count();
    println!("after {CYCLES} overlays: {gdi_after} GDI objects, {threads_after} threads");
    assert!(
        threads_after <= threads_before + THREAD_SLACK,
        "threads grew from {threads_before} to {threads_after} over {CYCLES} overlays"
    );
    assert!(
        gdi_after <= gdi_before + GDI_SLACK,
        "GDI objects grew from {gdi_before} to {gdi_after} over {CYCLES} overlays"
    );
    assert!(!class_registered(), "no window class is left behind");

    // Phase 2: overlays of one name open together share one class; it goes with the last of them.
    let (a, a_frames) = open(120.0);
    let (b, b_frames) = open(140.0);
    let (c, c_frames) = open(160.0);
    for (window, frames) in [(&a, &a_frames), (&b, &b_frames), (&c, &c_frames)] {
        window.handle().show();
        wait_until("a paint", || frames.load(Ordering::SeqCst) > 0);
    }
    assert!(class_registered());
    a.close();
    assert!(class_registered(), "two windows still use the class");
    c.close();
    assert!(class_registered(), "one window still uses the class");
    b.close();
    assert!(!class_registered(), "the last close unregisters it");
    wait_until("owner threads to end", || owner_threads() == 0);
    assert!(
        gdi_objects() <= gdi_before + GDI_SLACK,
        "GDI objects after overlapping overlays: {} (baseline {gdi_before})",
        gdi_objects()
    );

    // Phase 3: one window resized every frame. The surface is grow-only, so the object count
    // settles instead of tracking the number of size changes.
    let (window, frames) = open(100.0);
    let overlay = window.handle();
    overlay.show();
    wait_until("the first paint", || frames.load(Ordering::SeqCst) > 0);
    let resize = |from: usize, to: usize| {
        for i in from..to {
            overlay.set_frame(Rect {
                x: 200.0,
                y: 200.0,
                width: 100.0 + (i % 50) as f64 * 4.0,
                height: 36.0,
            });
            std::thread::sleep(Duration::from_millis(3));
        }
    };
    resize(0, 60);
    std::thread::sleep(Duration::from_millis(200));
    let steady = gdi_objects();
    let painted = frames.load(Ordering::SeqCst);
    resize(60, 400);
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        frames.load(Ordering::SeqCst) > painted + 20,
        "the resizes were painted"
    );
    let churned = gdi_objects();
    println!("resize churn: {steady} -> {churned} GDI objects over 340 size changes");
    assert!(
        churned <= steady + GDI_SLACK,
        "resizing grew GDI objects from {steady} to {churned}"
    );
    window.close();
    wait_until("the owner thread to end", || owner_threads() == 0);
    assert!(
        gdi_objects() <= gdi_before + GDI_SLACK,
        "GDI objects after the resize phase: {} (baseline {gdi_before})",
        gdi_objects()
    );
}
