//! `set_viewport` core: apply, read back, settle, report what was observed.
//!
//! Platform-free so it is testable with a fake window. The native layers only
//! `apply` a size and `read` the page's CSS viewport (`innerWidth`/`innerHeight`).
//! The result is never the request echoed back: it is the last size read from
//! the page, and it says whether that size held still (`settled`) and whether it
//! differs from the request (`clamped`).
//!
//! Late shell placement: a shell that restores or fits a window after the webview
//! is ready announces `rk-shell:placement` `{label, phase: "begin"|"done"}` on the
//! app event bus (`rightkit-tauri-shell` does). [`PlacementGate`] records that, and a
//! viewport request waits (bounded) for `done` before applying. A placement timeout
//! cannot report success. Hidden exact-size adapters also reapply late geometry
//! within their settle bound. No announcement means no placement wait.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// App event name the shell emits; payload `{"label": "...", "phase": "begin"|"done"}`.
pub const PLACEMENT_EVENT: &str = "rk-shell:placement";
/// Default total bound for the size to stop changing.
pub const DEFAULT_SETTLE: Duration = Duration::from_secs(2);
/// Gap between the two consecutive readings that must agree.
pub const SETTLE_INTERVAL: Duration = Duration::from_millis(500);
/// Default bound for waiting on the shell's initial placement.
pub const DEFAULT_PLACEMENT_WAIT: Duration = Duration::from_secs(5);
/// Largest per-request `settleMs`.
pub const MAX_SETTLE: Duration = Duration::from_secs(10);

/// Labels whose shell placement has begun and not yet finished.
#[derive(Default)]
pub struct PlacementGate {
    pending: Mutex<HashSet<String>>,
    changed: Condvar,
}

impl PlacementGate {
    pub fn begin(&self, label: &str) {
        self.lock().insert(label.to_owned());
    }
    pub fn done(&self, label: &str) {
        self.lock().remove(label);
        self.changed.notify_all();
    }
    /// Apply one `rk-shell:placement` payload; unknown shapes are ignored.
    pub fn on_event(&self, payload: &str) {
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            return;
        };
        let (Some(label), Some(phase)) = (
            v.get("label").and_then(Value::as_str),
            v.get("phase").and_then(Value::as_str),
        ) else {
            return;
        };
        match phase {
            "begin" => self.begin(label),
            "done" => self.done(label),
            _ => {}
        }
    }
    /// `true` when no placement is pending for `label` (now or within `bound`);
    /// `false` when it was still pending at the bound.
    pub fn wait(&self, label: &str, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        let mut pending = self.lock();
        while pending.contains(label) {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            pending = match self.changed.wait_timeout(pending, deadline - now) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
        true
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Native operations & sizing policy used by `set_viewport`.
pub trait ViewportWindow {
    /// Ask the window for a viewport of this CSS size (no show, no focus).
    fn apply(&self, width: u32, height: u32) -> Result<(), String>;
    /// Whether a different size must be corrected rather than accepted as clamped.
    /// Native hidden macOS windows require exact dimensions; default allows OS limits.
    fn requires_exact_size(&self) -> Result<bool, String> {
        Ok(false)
    }
    /// The page's current CSS viewport.
    fn read(&self, timeout: Duration) -> Result<(f64, f64), String>;
}

#[derive(Clone, Copy, Debug)]
pub struct ViewportCfg {
    /// Total bound for the size to stop changing.
    pub settle: Duration,
    /// Gap between consecutive readings.
    pub interval: Duration,
    /// Bound for the shell's initial placement.
    pub placement_wait: Duration,
}

impl Default for ViewportCfg {
    fn default() -> Self {
        Self {
            settle: DEFAULT_SETTLE,
            interval: SETTLE_INTERVAL,
            placement_wait: DEFAULT_PLACEMENT_WAIT,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct ViewportOutcome {
    pub requested: (u32, u32),
    /// The last size read from the page.
    pub size: (u32, u32),
    /// Two consecutive non-empty readings, separated by `ViewportCfg::interval`, agreed.
    pub settled: bool,
    /// The shell's placement was still pending at the wait bound.
    pub placement_timed_out: bool,
}

impl ViewportOutcome {
    /// Settled at a size other than the request (OS work-area clamp, min/max size).
    pub fn clamped(&self) -> bool {
        self.settled && self.size != self.requested
    }
    pub fn to_json(&self) -> Value {
        let reason = if self.placement_timed_out {
            Some("placement timed out")
        } else if !self.settled {
            Some("unsettled")
        } else if self.clamped() {
            Some("clamped")
        } else {
            None
        };
        let mut v = json!({
            "ok": self.settled && !self.placement_timed_out,
            "innerWidth": self.size.0,
            "innerHeight": self.size.1,
            "requestedWidth": self.requested.0,
            "requestedHeight": self.requested.1,
            "settled": self.settled,
            "clamped": self.clamped(),
            "placementTimedOut": self.placement_timed_out,
        });
        if let (Some(r), Value::Object(m)) = (reason, &mut v) {
            m.insert("reason".into(), json!(r));
            if !self.settled || self.placement_timed_out {
                m.insert(
                    "error".into(),
                    json!(format!(
                        "viewport {r}: requested {}x{}, measured {}x{}",
                        self.requested.0, self.requested.1, self.size.0, self.size.1
                    )),
                );
            }
        }
        v
    }
}

fn px(v: f64) -> u32 {
    v.round().clamp(0.0, u32::MAX as f64) as u32
}

/// Read until two consecutive non-empty readings agree, or `cfg.settle` elapses.
/// With `target`, only matching requested dimensions can settle.
/// At the bound the last reading is returned with `settled == false`; with no
/// reading at all the last error is returned.
pub fn settle(
    cfg: &ViewportCfg,
    target: Option<(u32, u32)>,
    mut read: impl FnMut(Duration) -> Result<(f64, f64), String>,
    mut elapsed: impl FnMut() -> Duration,
    mut wait: impl FnMut(Duration),
) -> Result<((u32, u32), bool), String> {
    let mut last: Option<(u32, u32)> = None;
    let mut last_error: Option<String> = None;
    let mut consecutive = None;
    loop {
        let remaining = cfg.settle.saturating_sub(elapsed());
        match read(
            remaining
                .max(Duration::from_millis(1))
                .min(Duration::from_millis(250)),
        ) {
            Ok((w, h)) if w.is_finite() && h.is_finite() && px(w) > 0 && px(h) > 0 => {
                let size = (px(w), px(h));
                // Even a matching target can be transient while native layout is pending.
                if consecutive == Some(size)
                    && target.is_none_or(|target| target == size)
                    && elapsed() < cfg.settle
                {
                    return Ok((size, true));
                }
                last = Some(size);
                consecutive = Some(size);
            }
            Ok(_) => {
                consecutive = None;
                last_error = Some("page reported an empty or invalid viewport".into());
            }
            Err(e) => {
                consecutive = None;
                last_error = Some(e);
            }
        }
        let remaining = cfg.settle.saturating_sub(elapsed());
        if remaining.is_zero() {
            return match last {
                Some(size) => Ok((size, false)),
                None => Err(format!(
                    "CSS viewport readback failed: {}",
                    last_error.unwrap_or_else(|| "no measurement".into())
                )),
            };
        }
        wait(cfg.interval.min(remaining));
    }
}

/// Wait for shell placement, apply the request, read back until settled.
pub fn set_viewport<W: ViewportWindow>(
    win: &W,
    gate: &PlacementGate,
    label: &str,
    width: u32,
    height: u32,
    cfg: &ViewportCfg,
) -> Result<ViewportOutcome, String> {
    if !(1..=16384).contains(&width) || !(1..=16384).contains(&height) {
        return Err("viewport dimensions must be in 1..=16384 CSS pixels".into());
    }
    let placed = gate.wait(label, cfg.placement_wait);
    let target = (width, height);
    let exact = win.requires_exact_size()?;
    win.apply(width, height)?;
    let start = Instant::now();
    let (size, settled) = settle(
        cfg,
        exact.then_some(target),
        |t| {
            let size = win.read(t)?;
            if exact
                && size.0.is_finite()
                && size.1.is_finite()
                && size.0 > 0.0
                && size.1 > 0.0
                && (px(size.0), px(size.1)) != target
                && start.elapsed() < cfg.settle
            {
                // A late shell restore can land after apply. Reassert within this
                // request's bound; do not make every caller own the retry loop.
                win.apply(width, height)?;
            }
            Ok(size)
        },
        || start.elapsed(),
        std::thread::sleep,
    )?;
    Ok(ViewportOutcome {
        requested: (width, height),
        size,
        settled,
        placement_timed_out: !placed,
    })
}
