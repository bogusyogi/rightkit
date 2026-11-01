//! The always-visible "agent is driving" badge: a separate always-on-top window, placed beside the
//! target and never over it.
//!
//! Recording is per window (`Source.window_id`), so a window of this app is outside the captured
//! frames by construction. Placement keeps it off the target's rectangle too, so it also does not
//! cover the thing being demonstrated. If no position beside the target fits on the monitor, the
//! session is refused: an agent never drives without a visible badge.

use rightkit_overlay::{Rect, WorkArea};
use tauri::{AppHandle, Manager, WebviewUrl};

/// Window label. The frontend decides what to draw from it (src/main.tsx).
pub const LABEL: &str = "demo-badge";
pub const BADGE_W: f64 = 300.0;
pub const BADGE_H: f64 = 48.0;
const GAP: f64 = 8.0;
/// Room for the macOS menu bar, which the monitor bounds include.
const TOP_INSET: f64 = if cfg!(target_os = "macos") { 32.0 } else { 0.0 };

fn intersects(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

fn inside(r: &Rect, area: &WorkArea) -> bool {
    r.x >= area.left
        && r.y >= area.top
        && r.x + r.width <= area.right
        && r.y + r.height <= area.bottom
}

/// Top-left of a badge of `size` beside `target`, inside `work`, off the target. Order: above, below,
/// right, left; the first that fits wins. `None` when the target leaves no room.
pub fn place(target: Rect, size: (f64, f64), work: WorkArea) -> Option<(f64, f64)> {
    if !target.finite() {
        return None;
    }
    let (w, h) = size;
    let work = WorkArea {
        top: work.top + TOP_INSET,
        ..work
    };
    let candidates = [
        (target.x, target.y - h - GAP),
        (target.x, target.y + target.height + GAP),
        (target.x + target.width + GAP, target.y),
        (target.x - w - GAP, target.y),
    ];
    candidates.into_iter().find_map(|(x, y)| {
        let badge = Rect {
            x,
            y,
            width: w,
            height: h,
        };
        (inside(&badge, &work) && !intersects(&badge, &target)).then_some((x, y))
    })
}

/// The monitor bounds, in logical units, that contain the centre of `target`; else the first monitor.
fn work_area(app: &AppHandle, target: &Rect) -> Option<WorkArea> {
    let monitors = app.available_monitors().ok()?;
    let (cx, cy) = (
        target.x + target.width / 2.0,
        target.y + target.height / 2.0,
    );
    let area = |m: &tauri::Monitor| {
        let s = m.scale_factor();
        let (p, z) = (m.position(), m.size());
        WorkArea {
            left: f64::from(p.x) / s,
            top: f64::from(p.y) / s,
            right: (f64::from(p.x) + f64::from(z.width)) / s,
            bottom: (f64::from(p.y) + f64::from(z.height)) / s,
        }
    };
    let areas: Vec<WorkArea> = monitors.iter().map(area).collect();
    areas
        .iter()
        .copied()
        .find(|a| cx >= a.left && cx <= a.right && cy >= a.top && cy <= a.bottom)
        .or_else(|| areas.first().copied())
}

/// Shows the badge beside `target` (logical points, as `Source.bounds_pt`). Errors when it cannot be
/// placed or created: the caller refuses the session.
pub fn show(app: &AppHandle, target: Rect) -> Result<(), String> {
    let work =
        work_area(app, &target).ok_or("no monitor information, so the badge cannot be placed")?;
    let (x, y) = place(target, (BADGE_W, BADGE_H), work).ok_or(
        "there is no room beside the target window for the demo badge; make the window smaller",
    )?;
    if let Some(existing) = app.get_webview_window(LABEL) {
        existing
            .set_position(tauri::LogicalPosition::new(x, y))
            .map_err(|e| e.to_string())?;
        existing.show().map_err(|e| e.to_string())?;
        return Ok(());
    }
    // The shell's builder carries its window policy; it starts hidden, so the badge shows itself.
    let window =
        rightkit_tauri_shell::windows::builder(app, LABEL, WebviewUrl::App("index.html".into()))
            .visible(true)
            .title(format!("{} demo", crate::identity().product_name))
            .inner_size(BADGE_W, BADGE_H)
            .position(x, y)
            .decorations(false)
            .resizable(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible_on_all_workspaces(true)
            .focused(false)
            .shadow(false)
            .build()
            .map_err(|e| format!("the demo badge window could not be created: {e}"))?;
    style(app, &window);
    Ok(())
}

/// Windows: no-activate, tool-window, topmost, through the shared overlay policy. Best effort; the
/// badge is already always-on-top from the window setup. Unverified on a Windows host.
#[cfg(windows)]
fn style(app: &AppHandle, window: &tauri::WebviewWindow) {
    if let Ok(hwnd) = window.hwnd() {
        let hwnd = hwnd.0 as isize;
        // The overlay crate installs its window-procedure guard on the thread that owns the window.
        let _ = app.run_on_main_thread(move || {
            rightkit_overlay::windows::make_noactivate(hwnd);
            rightkit_overlay::windows::apply_style(hwnd, rightkit_overlay::OverlayStyle::PASSIVE);
            rightkit_overlay::windows::show_noactivate(hwnd);
        });
    }
}

#[cfg(not(windows))]
fn style(_app: &AppHandle, _window: &tauri::WebviewWindow) {}

pub fn hide(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.close();
    }
}

/// The badge's current rectangle in logical points, for the acceptance check that it does not
/// intersect the target.
pub fn bounds(app: &AppHandle) -> Option<(f64, f64, f64, f64)> {
    let w = app.get_webview_window(LABEL)?;
    let s = w.scale_factor().ok()?;
    let (p, z) = (w.outer_position().ok()?, w.outer_size().ok()?);
    Some((
        f64::from(p.x) / s,
        f64::from(p.y) / s,
        f64::from(z.width) / s,
        f64::from(z.height) / s,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: WorkArea = WorkArea {
        left: 0.0,
        top: 0.0,
        right: 1440.0,
        bottom: 900.0,
    };

    // Unit tests on purpose: placement is pure geometry, and the visible acceptance check needs a
    // window server. The e2e covers the real window; these cover the corner cases it cannot provoke.
    #[test]
    fn the_badge_never_overlaps_the_target() {
        let targets = [
            Rect {
                x: 200.0,
                y: 200.0,
                width: 800.0,
                height: 500.0,
            },
            Rect {
                x: 0.0,
                y: 40.0,
                width: 1440.0,
                height: 500.0,
            },
            Rect {
                x: 400.0,
                y: 40.0,
                width: 1040.0,
                height: 860.0,
            },
            Rect {
                x: 0.0,
                y: 0.0,
                width: 600.0,
                height: 900.0,
            },
        ];
        for t in targets {
            if let Some((x, y)) = place(t, (BADGE_W, BADGE_H), SCREEN) {
                let b = Rect {
                    x,
                    y,
                    width: BADGE_W,
                    height: BADGE_H,
                };
                assert!(!intersects(&b, &t), "{t:?} vs {b:?}");
                assert!(inside(&b, &SCREEN), "{b:?}");
            }
        }
    }

    #[test]
    fn a_target_that_fills_the_screen_leaves_no_place() {
        let full = Rect {
            x: 0.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
        };
        assert_eq!(place(full, (BADGE_W, BADGE_H), SCREEN), None);
        let bad = Rect {
            x: f64::NAN,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        assert_eq!(place(bad, (BADGE_W, BADGE_H), SCREEN), None);
    }
}
