//! Journey helpers for native overlay windows the app reports itself (a floating capsule, a hint, a
//! recent-items card): an ordered step report that prints when a journey fails, readers for the app's
//! overlay-rects report, and waits for an overlay to open or close.
//!
//! The report's keys (`pill`, `pill_hitbox`, `clipboard`, `hint`, ...) are the host app's: they are
//! passed as arguments. Real desktop calls (top-level windows, hit testing, clicks) are in
//! `desktop_win` on Windows.

use std::time::Duration;

use serde_json::{json, Value};

use crate::live_app::wait_until;

/// One named observation per step, kept in order and printed when the journey fails (the report
/// prints in `Drop` while the thread is panicking).
pub struct StepReport {
    /// Heading of the failure print.
    pub title: String,
    /// The steps so far: `{name, ok, data}`.
    pub steps: Vec<Value>,
}

impl StepReport {
    /// An empty report titled `title`.
    pub fn new(title: &str) -> Self {
        StepReport {
            title: title.to_string(),
            steps: Vec::new(),
        }
    }

    /// Record one step.
    pub fn add(&mut self, name: &str, ok: bool, data: Value) {
        self.steps
            .push(json!({ "name": name, "ok": ok, "data": data }));
    }
}

impl Drop for StepReport {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "{} steps so far:\n{}",
                self.title,
                serde_json::to_string_pretty(&self.steps).unwrap_or_default()
            );
        }
    }
}

/// A number field of `value`, or NaN when absent.
pub fn num(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN)
}

/// Whether a window record's `visible` field is true.
pub fn is_visible(window: &Value) -> bool {
    window
        .get("visible")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `(x, y, w, h)` of an overlay's hit box in screen coordinates: the window record at `window_key`
/// gives the origin and the record at `hitbox_key` the box inside it.
pub fn hitbox_on_screen(rects: &Value, window_key: &str, hitbox_key: &str) -> (f64, f64, f64, f64) {
    let (window, hit) = (&rects[window_key], &rects[hitbox_key]);
    (
        num(window, "x") + num(hit, "x"),
        num(window, "y") + num(hit, "y"),
        num(hit, "w"),
        num(hit, "h"),
    )
}

/// Wait (up to `within`) until the overlay at `key` of the report `read` returns is no longer visible.
pub fn wait_closed(
    what: &str,
    key: &str,
    within: Duration,
    mut read: impl FnMut() -> Result<Value, String>,
) {
    wait_until(what, within, || {
        let r = read()?;
        Ok(if is_visible(&r[key]) { None } else { Some(()) })
    });
}

/// Wait (up to `within`) until the overlay at `key` is visible; returns the whole report.
pub fn wait_open(
    what: &str,
    key: &str,
    within: Duration,
    mut read: impl FnMut() -> Result<Value, String>,
) -> Value {
    wait_until(what, within, || {
        let r = read()?;
        Ok(if is_visible(&r[key]) { Some(r) } else { None })
    })
}
