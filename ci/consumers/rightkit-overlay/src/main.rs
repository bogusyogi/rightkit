// rightkit-overlay 0.2.0 consumed by exact registry pin, outside the workspace.
//
// Proves the product-neutral API: `OverlayModel<Kind>` with a caller-owned `Kind` (the crate
// attaches no native policy to it); `apply` with the event-to-visibility mapping; `hit_test`; and
// the pure geometry functions `fills_monitor`, `rects_overlap` and `covering_window`, fed with
// synthetic candidates. No real windows, no Win32 calls, no timers.
// Does not prove: `place` clamping, the Windows window-procedure and style functions, `OverlayHost`
// (a rightkit-tauri-shell item, PK08-4), or the z-order walk on a live desktop.
// State: none. No network, no files, no real user data.
use rightkit_overlay::{
    covering_window, fills_monitor, rects_overlap, CoveringCandidate, OverlayEvent, OverlayModel,
    PillMode, Rect, WorkArea,
};

// A caller-owned identity. The crate never inspects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Pill,
    Status,
}

fn area(left: f64, top: f64, right: f64, bottom: f64) -> WorkArea {
    WorkArea {
        left,
        top,
        right,
        bottom,
    }
}

fn candidate(
    class_name: &str,
    visible: bool,
    own_process: bool,
    cloaked: bool,
    rect: WorkArea,
) -> CoveringCandidate {
    CoveringCandidate {
        class_name: class_name.to_owned(),
        visible,
        own_process,
        cloaked,
        rect,
    }
}

fn main() {
    // Model: hidden until an event shows it; the hitbox defaults to the frame.
    let frame = Rect {
        x: 100.0,
        y: 50.0,
        width: 200.0,
        height: 40.0,
    };
    let mut pill = OverlayModel::new(Kind::Pill, frame);
    assert_eq!(pill.kind, Kind::Pill);
    assert!(!pill.visible);
    assert!(!pill.hit_test(150.0, 60.0), "hidden overlay takes no hits");

    pill.apply(OverlayEvent::Recording);
    assert!(pill.visible);
    assert_eq!(pill.mode, Some(PillMode::Recording));
    assert!(pill.hit_test(150.0, 60.0));
    assert!(!pill.hit_test(900.0, 900.0));

    pill.apply(OverlayEvent::Hide);
    assert!(!pill.visible);
    assert_eq!(pill.mode, None);

    let status = OverlayModel::new(Kind::Status, frame);
    assert_eq!(status.kind, Kind::Status);
    assert_ne!(status.kind, pill.kind);

    // fills_monitor: only a window that covers the whole monitor counts; a maximised window stops
    // at the work area and does not.
    let monitor = area(0.0, 0.0, 1920.0, 1080.0);
    assert!(fills_monitor(monitor, monitor));
    assert!(!fills_monitor(area(0.0, 0.0, 1920.0, 1040.0), monitor));

    // rects_overlap: interior overlap yes, far apart no.
    assert!(rects_overlap(
        area(0.0, 0.0, 100.0, 100.0),
        area(50.0, 50.0, 150.0, 150.0)
    ));
    assert!(!rects_overlap(
        area(0.0, 0.0, 100.0, 100.0),
        area(200.0, 200.0, 300.0, 300.0)
    ));

    // covering_window: a not-topmost overlay reports so; otherwise the first candidate, nearest
    // above first, that is visible, not ours, not cloaked and overlapping.
    let overlay = area(100.0, 0.0, 300.0, 40.0);
    assert_eq!(covering_window(true, overlay, &[]), None);
    assert_eq!(
        covering_window(false, overlay, &[]),
        Some("not_topmost".to_string())
    );
    let above = vec![
        candidate("Hidden", false, false, false, area(100.0, 0.0, 300.0, 40.0)),
        candidate("Own", true, true, false, area(100.0, 0.0, 300.0, 40.0)),
        candidate("Cloaked", true, false, true, area(100.0, 0.0, 300.0, 40.0)),
        candidate("Far", true, false, false, area(1000.0, 0.0, 1100.0, 40.0)),
        candidate("Browser", true, false, false, area(100.0, 0.0, 500.0, 300.0)),
        candidate("Later", true, false, false, area(100.0, 0.0, 500.0, 300.0)),
    ];
    assert_eq!(
        covering_window(true, overlay, &above),
        Some("Browser".to_string())
    );

    println!("OK rightkit-overlay 0.2.0 (registry consumer)");
}
