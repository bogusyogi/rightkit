//! rightkit-overlay: the placement, hit-testing, hover, style and full-screen policy every overlay host shares, run on
//! known geometry with exact expected values. On Windows it also asks the OS for the monitor under a point and checks
//! the answer is a real monitor. Creating and showing an actual overlay window needs a UI host: app-level scenario.
use rightkit_overlay::stack::{self, StackLayout};
use rightkit_overlay::{
    covering_window, ex_style, fills_monitor, is_foreign_borderless_fullscreen, rects_overlap,
    CoveringCandidate, FullScreenPolicy, HoverGate, OverlayEvent, OverlayModel, OverlayRect,
    OverlayStyle, PillMode, Rect, WorkArea,
};
use serde_json::{json, Value};

fn ensure(ok: bool, what: impl FnOnce() -> String) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(what())
    }
}

fn area(left: f64, top: f64, right: f64, bottom: f64) -> WorkArea {
    WorkArea {
        left,
        top,
        right,
        bottom,
    }
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    // ---- hit testing: only a visible overlay is hit, edges are inclusive, NaN never hits
    let frame = Rect {
        x: 100.0,
        y: 50.0,
        width: 200.0,
        height: 40.0,
    };
    let mut model = OverlayModel::new((), frame);
    ensure(!model.hit_test(150.0, 60.0), || {
        "a hidden overlay was hit".into()
    })?;
    model.apply(OverlayEvent::Recording);
    ensure(
        model.visible && model.mode == Some(PillMode::Recording),
        || format!("Recording left the model {:?}", model.mode),
    )?;
    for (x, y, want) in [
        (150.0, 60.0, true),
        (100.0, 50.0, true),
        (300.0, 90.0, true),
        (99.9, 60.0, false),
        (300.1, 60.0, false),
        (150.0, 90.1, false),
        (f64::NAN, 60.0, false),
    ] {
        ensure(model.hit_test(x, y) == want, || {
            format!("hit_test({x}, {y}) was not {want}")
        })?;
    }
    model.hitbox = Some(Rect {
        x: 120.0,
        y: 55.0,
        width: 20.0,
        height: 20.0,
    });
    ensure(
        model.hit_test(130.0, 60.0) && !model.hit_test(200.0, 60.0),
        || "a narrower hitbox did not replace the frame".into(),
    )?;
    model.apply(OverlayEvent::Hide);
    ensure(!model.visible && !model.hit_test(130.0, 60.0), || {
        "Hide left the overlay hittable".into()
    })?;
    let wire = serde_json::to_value(OverlayEvent::Processing { cancellable: true })
        .map_err(|e| e.to_string())?;
    ensure(
        wire == json!({ "kind": "processing", "cancellable": true }),
        || format!("OverlayEvent wire form is {wire}"),
    )?;
    let back: OverlayEvent = serde_json::from_value(wire).map_err(|e| e.to_string())?;
    ensure(
        back == OverlayEvent::Processing { cancellable: true },
        || "OverlayEvent did not round-trip".into(),
    )?;

    // ---- placement: the frame is clamped into the work area, never resized
    let work = area(0.0, 0.0, 1920.0, 1040.0);
    for (input, want) in [
        (
            Rect {
                x: -20.0,
                y: 10.0,
                width: 200.0,
                height: 40.0,
            },
            (0.0, 10.0),
        ),
        (
            Rect {
                x: 1900.0,
                y: 1100.0,
                width: 200.0,
                height: 40.0,
            },
            (1720.0, 1000.0),
        ),
        (
            Rect {
                x: 500.0,
                y: 500.0,
                width: 200.0,
                height: 40.0,
            },
            (500.0, 500.0),
        ),
        (
            Rect {
                x: 500.0,
                y: 500.0,
                width: 3000.0,
                height: 40.0,
            },
            (0.0, 500.0),
        ),
    ] {
        let placed = work.clamp(input);
        ensure(
            (placed.x, placed.y) == want
                && placed.width == input.width
                && placed.height == input.height,
            || format!("clamp({input:?}) gave {placed:?}, expected {want:?}"),
        )?;
    }
    let broken = Rect {
        x: f64::NAN,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };
    ensure(!work.clamp(broken).finite(), || {
        "a non-finite frame was silently repaired".into()
    })?;
    let mut placed = OverlayModel::new(
        (),
        Rect {
            x: 1900.0,
            y: 10.0,
            width: 200.0,
            height: 40.0,
        },
    );
    placed.place(work);
    ensure(placed.frame.x == 1720.0, || {
        format!("OverlayModel::place left x at {}", placed.frame.x)
    })?;

    // ---- hover: one tick in to enter, three ticks out to leave
    let mut hover = HoverGate::default();
    ensure(!hover.update(false), || "hover started outside".into())?;
    ensure(hover.update(true), || {
        "one tick inside did not enter hover".into()
    })?;
    ensure(hover.update(false) && hover.update(false), || {
        "hover collapsed before the leave delay".into()
    })?;
    ensure(!hover.update(false), || {
        "hover did not collapse after three ticks outside".into()
    })?;

    // ---- native style contract
    let passive = OverlayStyle::PASSIVE.ex_style(ex_style::APPWINDOW);
    ensure(
        passive
            == ex_style::LAYERED | ex_style::TOOLWINDOW | ex_style::TOPMOST | ex_style::NOACTIVATE,
        || format!("PASSIVE ex style is {passive:#x}"),
    )?;
    let keyboard = OverlayStyle::KEYBOARD.ex_style(0);
    ensure(
        keyboard & ex_style::NOACTIVATE == 0 && keyboard & ex_style::TOPMOST != 0,
        || format!("KEYBOARD ex style is {keyboard:#x}"),
    )?;
    let through = OverlayStyle::CLICK_THROUGH.ex_style(0);
    ensure(through & ex_style::TRANSPARENT != 0, || {
        "CLICK_THROUGH lacks WS_EX_TRANSPARENT".into()
    })?;
    ensure(
        OverlayStyle::PASSIVE.unlayered_ex_style(0) & ex_style::LAYERED == 0,
        || "unlayered style kept WS_EX_LAYERED".into(),
    )?;
    ensure(
        rightkit_overlay::should_reassert_windows_topmost(true, false)
            && !rightkit_overlay::should_reassert_windows_topmost(true, true),
        || "topmost reassert rule is wrong".into(),
    )?;

    // ---- full-screen policy and covering-window detection
    let monitor = area(0.0, 0.0, 1920.0, 1080.0);
    ensure(
        fills_monitor(monitor, monitor)
            && !fills_monitor(area(10.0, 10.0, 1900.0, 1000.0), monitor),
        || "fills_monitor is wrong".into(),
    )?;
    ensure(
        !FullScreenPolicy::Hide.shows(true)
            && FullScreenPolicy::Hide.shows(false)
            && FullScreenPolicy::StayAbove.shows(true),
        || "FullScreenPolicy::shows is wrong".into(),
    )?;
    ensure(
        is_foreign_borderless_fullscreen(0, monitor, monitor, false),
        || "a foreign borderless fullscreen window was missed".into(),
    )?;
    ensure(
        !is_foreign_borderless_fullscreen(0, monitor, monitor, true),
        || "our own fullscreen window counted as foreign".into(),
    )?;
    ensure(
        !is_foreign_borderless_fullscreen(
            rightkit_overlay::WS_CAPTION_BITS,
            monitor,
            monitor,
            false,
        ),
        || "a captioned window counted as borderless".into(),
    )?;
    ensure(
        rects_overlap(area(0.0, 0.0, 10.0, 10.0), area(9.0, 9.0, 20.0, 20.0))
            && !rects_overlap(area(0.0, 0.0, 10.0, 10.0), area(10.0, 0.0, 20.0, 10.0)),
        || "rects_overlap treats touching edges as overlap".into(),
    )?;
    ensure(
        covering_window(false, monitor, &[]).as_deref() == Some("not_topmost"),
        || "a non-topmost overlay did not report not_topmost".into(),
    )?;
    let cand = |class: &str, visible, own, cloaked, rect| CoveringCandidate {
        class_name: class.into(),
        visible,
        own_process: own,
        cloaked,
        rect,
    };
    let overlay = area(100.0, 100.0, 300.0, 140.0);
    let above = [
        cand("hidden", false, false, false, overlay),
        cand("own", true, true, false, overlay),
        cand("cloaked", true, false, true, overlay),
        cand(
            "elsewhere",
            true,
            false,
            false,
            area(500.0, 500.0, 600.0, 600.0),
        ),
        cand("Chrome_WidgetWin_1", true, false, false, overlay),
    ];
    let cover = covering_window(true, overlay, &above);
    ensure(cover.as_deref() == Some("Chrome_WidgetWin_1"), || {
        format!("covering window was {cover:?}")
    })?;

    // ---- stack placement beside an anchor (screen pixels, top-left origin)
    let sw = OverlayRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1040,
    };
    let heights = [100u32, 80];
    let anchor = OverlayRect {
        x: 900,
        y: 500,
        width: 40,
        height: 40,
    };
    let layout = StackLayout::new(anchor, sw, 300, 190, 10, 8);
    ensure(layout.stack_above, || {
        "a stack that fits above the anchor was placed below".into()
    })?;
    let p0 = stack::place(&layout, 0, &heights, anchor, sw, 8, (0, 0));
    let p1 = stack::place(&layout, 1, &heights, anchor, sw, 8, (0, 0));
    ensure(p0 == (770, 390) && p1 == (770, 300), || {
        format!(
            "stack above placed windows at {p0:?} and {p1:?}, expected (770, 390) and (770, 300)"
        )
    })?;
    let top = OverlayRect {
        x: 900,
        y: 20,
        width: 40,
        height: 40,
    };
    let below = StackLayout::new(top, sw, 300, 190, 10, 8);
    ensure(!below.stack_above, || {
        "a stack with no room above was placed above".into()
    })?;
    let b0 = stack::place(&below, 0, &heights, top, sw, 8, (0, 0));
    let b1 = stack::place(&below, 1, &heights, top, sw, 8, (0, 0));
    ensure(b0 == (770, 70) && b1 == (770, 180), || {
        format!(
            "stack below placed windows at {b0:?} and {b1:?}, expected (770, 70) and (770, 180)"
        )
    })?;
    let edge = OverlayRect {
        x: 1880,
        y: 500,
        width: 40,
        height: 40,
    };
    let clamped = stack::place(&layout, 0, &heights, edge, sw, 8, (0, 0));
    ensure(clamped.0 == 1920 - 300 - 8, || {
        format!(
            "a stack at the right edge was placed at x {}, expected {}",
            clamped.0,
            1920 - 300 - 8
        )
    })?;

    #[allow(unused_mut)]
    let mut detail = json!({ "stack_above": [p0, p1], "stack_below": [b0, b1], "covering": cover });

    // ---- the OS: the monitor under a point is a real monitor
    #[cfg(windows)]
    {
        let m = rightkit_overlay::windows::monitor_at(0, 0)
            .ok_or("monitor_at(0, 0) returned no monitor")?;
        ensure(
            m.dpi >= 96 && m.bounds.right > m.bounds.left && m.bounds.bottom > m.bounds.top,
            || format!("monitor_at(0, 0) is not a real monitor: {m:?}"),
        )?;
        ensure(
            m.work.left >= m.bounds.left && m.work.right <= m.bounds.right,
            || format!("work area escapes the monitor: {m:?}"),
        )?;
        detail["monitor"] = json!({ "dpi": m.dpi, "scale": m.scale(), "right": m.bounds.right, "bottom": m.bounds.bottom });
        detail["foreground_fills_monitor"] =
            json!(rightkit_overlay::windows::foreground_fills_monitor());
    }
    Ok(detail)
}
