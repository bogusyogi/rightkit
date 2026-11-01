//! rightkit-overlay-win (Windows only: mod.rs compiles this file under cfg(windows)). Pure geometry and the software
//! painter run first; then a REAL layered window is created, painted, shown without activation, and driven by the real
//! pointer: SetCursorPos over the window must produce HoverEnter, a click must produce Click, leaving must produce
//! HoverLeave, and the window must never take the foreground. The extended style bits are read back from the OS.
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_overlay::{ex_style, MonitorPlacement, WorkArea};
use rightkit_overlay_win::{
    device_regions, hit_test, place_frame, scale_for_dpi, Font, FrameInfo, HitRegion,
    OverlayConfig, OverlayEvent, OverlayWindow, PaintCtx, Rect, Rgba,
};
use serde_json::{json, Value};

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct WinPoint {
    x: i32,
    y: i32,
}

#[link(name = "user32")]
extern "system" {
    fn IsWindowVisible(hwnd: isize) -> i32;
    fn GetWindowRect(hwnd: isize, rect: *mut WinRect) -> i32;
    fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
    fn GetForegroundWindow() -> isize;
    fn GetCursorPos(point: *mut WinPoint) -> i32;
    fn SetCursorPos(x: i32, y: i32) -> i32;
    fn mouse_event(flags: u32, dx: u32, dy: u32, data: u32, extra: usize);
}

const GWL_EXSTYLE: i32 = -20;
const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
const DEADLINE: Duration = Duration::from_secs(8);

fn ensure(ok: bool, what: impl FnOnce() -> String) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(what())
    }
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < DEADLINE {
        if cond() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(format!(
        "timed out after {}s waiting for {what}",
        DEADLINE.as_secs()
    ))
}

fn expect_event(
    rx: &std::sync::mpsc::Receiver<OverlayEvent>,
    what: &str,
    mut pick: impl FnMut(&OverlayEvent) -> bool,
) -> Result<(), String> {
    let start = Instant::now();
    let mut seen = Vec::new();
    while start.elapsed() < DEADLINE {
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(100)) {
            if pick(&ev) {
                return Ok(());
            }
            seen.push(format!("{ev:?}"));
        }
    }
    Err(format!(
        "no {what} within {}s (saw {seen:?})",
        DEADLINE.as_secs()
    ))
}

fn pure_checks() -> Result<Value, String> {
    // Hit regions: edge-inclusive, the later region wins.
    let regions = [
        HitRegion {
            id: 1,
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 36.0,
            },
        },
        HitRegion {
            id: 2,
            rect: Rect {
                x: 50.0,
                y: 0.0,
                width: 50.0,
                height: 36.0,
            },
        },
    ];
    ensure(
        hit_test(&regions, 10.0, 10.0) == Some(1) && hit_test(&regions, 60.0, 10.0) == Some(2),
        || "hit_test picked the wrong region".into(),
    )?;
    ensure(
        hit_test(&regions, 100.0, 36.0) == Some(2) && hit_test(&regions, 100.1, 10.0).is_none(),
        || "hit_test edges are not inclusive".into(),
    )?;
    ensure(
        device_regions(&regions[..1], 1.5) == vec![[0, 0, 150, 54]],
        || "device_regions did not scale to device pixels".into(),
    )?;
    ensure((scale_for_dpi(144) - 1.5).abs() < f32::EPSILON, || {
        "scale_for_dpi(144) is not 1.5".into()
    })?;

    // Placement at 150%: size is logical, position physical, the frame is held in the work area.
    let monitor = MonitorPlacement {
        bounds: WorkArea {
            left: 0.0,
            top: 0.0,
            right: 1920.0,
            bottom: 1080.0,
        },
        work: WorkArea {
            left: 0.0,
            top: 0.0,
            right: 1920.0,
            bottom: 1040.0,
        },
        dpi: 144,
    };
    let p = place_frame(
        Rect {
            x: 1900.0,
            y: 1030.0,
            width: 120.0,
            height: 36.0,
        },
        true,
        |_, _| Some(monitor),
    )
    .ok_or("place_frame returned None")?;
    ensure(
        p.device.width == 180
            && p.device.height == 54
            && p.device.x == 1740
            && p.device.y == 986
            && p.dpi == 144,
        || format!("place_frame gave {:?}", p.device),
    )?;
    ensure(
        place_frame(
            Rect {
                x: f64::NAN,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            true,
            |_, _| Some(monitor),
        )
        .is_none(),
        || "a non-finite frame was placed".into(),
    )?;

    // The software painter: a filled rounded rect is opaque in the middle and transparent in the corner.
    let mut canvas = PaintCtx::offscreen(120, 36, 96);
    canvas.fill_rounded_rect(
        Rect {
            x: 0.0,
            y: 0.0,
            width: 120.0,
            height: 36.0,
        },
        18.0,
        Rgba::new(200, 40, 40, 255),
    );
    let (mid, corner) = (canvas.pixel(60, 18), canvas.pixel(0, 0));
    ensure(mid >> 24 == 0xFF && corner >> 24 == 0, || {
        format!("painter alpha: middle {mid:#010x}, corner {corner:#010x}")
    })?;
    let measured = canvas.measure_text("Recording", Font::ui(14.0));
    ensure(measured.width > 20.0 && measured.height > 5.0, || {
        format!("GDI text measured {measured:?}")
    })?;
    Ok(
        json!({ "painted_middle": format!("{mid:#010x}"), "text_width": measured.width, "device_at_150pct": [p.device.width, p.device.height] }),
    )
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let pure = pure_checks()?;

    let window = OverlayWindow::create(OverlayConfig::passive("rke2e-overlay-probe"))
        .map_err(|e| format!("OverlayWindow::create: {e}"))?;
    let overlay = window.handle();
    let hwnd = overlay.hwnd();
    ensure(hwnd != 0, || "the overlay has no HWND".into())?;

    let frames = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&frames);
    overlay.set_painter(Box::new(move |ctx: &mut PaintCtx, _frame: FrameInfo| {
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
            Rgba::new(30, 30, 30, 235),
        );
    }));
    overlay.set_frame(Rect {
        x: 300.0,
        y: 300.0,
        width: 120.0,
        height: 36.0,
    });
    overlay.set_hit_regions(vec![HitRegion {
        id: 1,
        rect: Rect {
            x: 0.0,
            y: 0.0,
            width: 120.0,
            height: 36.0,
        },
    }]);
    let events = overlay.events();
    let foreground_before = unsafe { GetForegroundWindow() };
    overlay.show();

    wait_for(
        "the overlay window to become visible",
        || unsafe { IsWindowVisible(hwnd) } != 0,
    )?;
    wait_for("the painter to run", || frames.load(Ordering::SeqCst) > 0)?;
    ensure(overlay.is_visible(), || {
        "OverlayHandle::is_visible is false after show".into()
    })?;

    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    for (bit, name) in [
        (ex_style::TOPMOST, "WS_EX_TOPMOST"),
        (ex_style::NOACTIVATE, "WS_EX_NOACTIVATE"),
        (ex_style::LAYERED, "WS_EX_LAYERED"),
        (ex_style::TOOLWINDOW, "WS_EX_TOOLWINDOW"),
    ] {
        ensure(ex & bit != 0, || {
            format!("the live window lacks {name} (ex style {ex:#x})")
        })?;
    }
    let mut rect = WinRect::default();
    ensure(unsafe { GetWindowRect(hwnd, &mut rect) } != 0, || {
        "GetWindowRect failed".into()
    })?;
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    ensure(w >= 120 && h >= 36, || {
        format!("the window is {w}x{h} device pixels, expected at least 120x36")
    })?;

    // Drive the real pointer. A locked or non-interactive desktop refuses this.
    let mut original = WinPoint::default();
    ensure(unsafe { GetCursorPos(&mut original) } != 0, || {
        "needs an interactive desktop session (GetCursorPos failed)".into()
    })?;
    let inside = (rect.left + 10, rect.top + 10);
    if unsafe { SetCursorPos(inside.0, inside.1) } == 0 {
        return Err("needs an unlocked interactive desktop (SetCursorPos was refused)".into());
    }
    let hover = expect_event(
        &events,
        "HoverEnter(1) with the pointer over the window",
        |e| matches!(e, OverlayEvent::HoverEnter(1)),
    );
    let click = if hover.is_ok() {
        unsafe {
            mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
            mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
        }
        expect_event(&events, "Click(1) after a real mouse click", |e| {
            matches!(e, OverlayEvent::Click(1))
        })
    } else {
        Ok(())
    };
    let foreground_after = unsafe { GetForegroundWindow() };
    unsafe { SetCursorPos(rect.right + 300, rect.bottom + 300) };
    let leave = expect_event(&events, "HoverLeave(1) after the pointer left", |e| {
        matches!(e, OverlayEvent::HoverLeave(1))
    });
    unsafe { SetCursorPos(original.x, original.y) };
    hover?;
    click?;
    leave?;
    ensure(foreground_after != hwnd, || {
        "showing and clicking the overlay stole the foreground".into()
    })?;

    overlay.hide();
    wait_for(
        "the overlay window to hide",
        || unsafe { IsWindowVisible(hwnd) } == 0,
    )?;
    overlay.close();
    expect_event(&events, "Closed", |e| matches!(e, OverlayEvent::Closed))?;
    drop(window);

    Ok(json!({
        "pure": pure, "hwnd_live": true, "ex_style": format!("{ex:#x}"), "device_size": [w, h],
        "frames_painted": frames.load(Ordering::SeqCst), "hover_enter": true, "click": true, "hover_leave": true,
        "foreground_unchanged": foreground_before == foreground_after || foreground_after != hwnd,
    }))
}
