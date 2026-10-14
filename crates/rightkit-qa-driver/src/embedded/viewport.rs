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
//! viewport request waits (bounded) for `done` before it applies, so placement can
//! never land after the request and undo it. No shell, or a shell that never
//! announced, means no wait.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// App event name the shell emits; payload `{"label": "...", "phase": "begin"|"done"}`.
pub const PLACEMENT_EVENT: &str = "rk-shell:placement";
/// Default total bound for the size to stop changing.
pub const DEFAULT_SETTLE: Duration = Duration::from_secs(2);
/// Gap between the two consecutive readings that must agree.
pub const SETTLE_INTERVAL: Duration = Duration::from_millis(50);
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

/// The two window operations `set_viewport` needs.
pub trait ViewportWindow {
    /// Ask the window for a viewport of this CSS size (no show, no focus).
    fn apply(&self, width: u32, height: u32) -> Result<(), String>;
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
    /// Two consecutive readings agreed within the bound.
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
        let reason = if !self.settled {
            Some("unsettled")
        } else if self.clamped() {
            Some("clamped")
        } else {
            None
        };
        let mut v = json!({
            "ok": true,
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
        }
        v
    }
}

fn px(v: f64) -> u32 {
    v.round().clamp(0.0, u32::MAX as f64) as u32
}

/// Read until two consecutive non-empty readings agree, or `cfg.settle` elapses.
/// At the bound the last reading is returned with `settled == false`; with no
/// reading at all the last error is returned.
pub fn settle(
    cfg: &ViewportCfg,
    mut read: impl FnMut(Duration) -> Result<(f64, f64), String>,
    mut elapsed: impl FnMut() -> Duration,
    mut wait: impl FnMut(Duration),
) -> Result<((u32, u32), bool), String> {
    let mut last: Option<(u32, u32)> = None;
    let mut last_error: Option<String> = None;
    loop {
        let remaining = cfg.settle.saturating_sub(elapsed());
        match read(
            remaining
                .max(Duration::from_millis(1))
                .min(Duration::from_millis(250)),
        ) {
            Ok((w, h)) if w > 0.0 && h > 0.0 => {
                let size = (px(w), px(h));
                if last == Some(size) {
                    return Ok((size, true));
                }
                last = Some(size);
            }
            Ok(_) => last_error = Some("page reported an empty viewport".into()),
            Err(e) => last_error = Some(e),
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
    let placed = gate.wait(label, cfg.placement_wait);
    win.apply(width, height)?;
    let start = Instant::now();
    let (size, settled) = settle(cfg, |t| win.read(t), || start.elapsed(), std::thread::sleep)?;
    Ok(ViewportOutcome {
        requested: (width, height),
        size,
        settled,
        placement_timed_out: !placed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// A window whose size the OS may clamp, and that lets a test land a late
    /// shell placement.
    struct Fake {
        size: Mutex<(f64, f64)>,
        clamp: Option<(f64, f64)>,
        /// When set, every read grows the height (never settles).
        drift: bool,
        reads: AtomicU32,
    }
    impl Fake {
        fn new(clamp: Option<(f64, f64)>, drift: bool) -> Self {
            Self {
                size: Mutex::new((800.0, 600.0)),
                clamp,
                drift,
                reads: AtomicU32::new(0),
            }
        }
        fn place(&self, w: f64, h: f64) {
            *self.size.lock().unwrap() = (w, h);
        }
    }
    impl ViewportWindow for Fake {
        fn apply(&self, w: u32, h: u32) -> Result<(), String> {
            let (mut w, mut h) = (w as f64, h as f64);
            if let Some((cw, ch)) = self.clamp {
                w = w.min(cw);
                h = h.min(ch);
            }
            self.place(w, h);
            Ok(())
        }
        fn read(&self, _: Duration) -> Result<(f64, f64), String> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let mut s = self.size.lock().unwrap();
            if self.drift {
                s.1 += 1.0;
            }
            Ok(*s)
        }
    }

    fn fast() -> ViewportCfg {
        ViewportCfg {
            settle: Duration::from_millis(400),
            interval: Duration::from_millis(5),
            placement_wait: Duration::from_secs(2),
        }
    }

    #[test]
    fn late_placement_cannot_override_the_request() {
        let gate = Arc::new(PlacementGate::default());
        let win = Arc::new(Fake::new(None, false));
        gate.on_event(r#"{"label":"main","phase":"begin"}"#);
        let (g, w) = (gate.clone(), win.clone());
        let placement = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            w.place(1440.0, 789.0); // late restore / fit-to-display
            g.on_event(r#"{"label":"main","phase":"done"}"#);
        });
        let out = set_viewport(&*win, &gate, "main", 1440, 900, &fast()).unwrap();
        placement.join().unwrap();
        assert_eq!(out.size, (1440, 900));
        assert!(out.settled && !out.clamped() && !out.placement_timed_out);
    }

    #[test]
    fn placement_that_never_finishes_is_bounded_and_reported() {
        let gate = PlacementGate::default();
        gate.begin("main");
        let mut cfg = fast();
        cfg.placement_wait = Duration::from_millis(30);
        let out = set_viewport(&Fake::new(None, false), &gate, "main", 1000, 700, &cfg).unwrap();
        assert!(out.placement_timed_out);
        assert_eq!(out.to_json()["placementTimedOut"], json!(true));
    }

    #[test]
    fn no_shell_means_no_wait() {
        let gate = PlacementGate::default();
        let t = Instant::now();
        assert!(gate.wait("main", Duration::from_secs(5)));
        assert!(t.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn clamped_window_reports_the_observed_size() {
        let win = Fake::new(Some((1600.0, 789.0)), false);
        let out =
            set_viewport(&win, &PlacementGate::default(), "main", 1440, 900, &fast()).unwrap();
        assert_eq!(out.size, (1440, 789));
        assert!(out.settled && out.clamped());
        let j = out.to_json();
        assert_eq!(j["innerHeight"], json!(789));
        assert_eq!(j["requestedHeight"], json!(900));
        assert_eq!(j["clamped"], json!(true));
        assert_eq!(j["reason"], json!("clamped"));
        assert_eq!(j["ok"], json!(true));
    }

    #[test]
    fn unsettled_size_returns_last_observed_and_is_marked() {
        let win = Fake::new(None, true);
        let out =
            set_viewport(&win, &PlacementGate::default(), "main", 1440, 900, &fast()).unwrap();
        assert!(!out.settled);
        assert!(out.size.1 > 900, "last observed, not the request: {out:?}");
        assert!(win.reads.load(Ordering::SeqCst) > 2);
        let j = out.to_json();
        assert_eq!(j["settled"], json!(false));
        assert_eq!(j["reason"], json!("unsettled"));
        assert_eq!(j["innerHeight"], json!(out.size.1));
    }

    #[test]
    fn settle_needs_two_equal_readings_and_skips_empty_ones() {
        let samples = [
            Ok((0.0, 0.0)),
            Ok((900.0, 640.0)),
            Ok((1440.0, 900.0)),
            Ok((1440.0, 900.0)),
        ];
        let mut it = samples.iter().cloned();
        let clock = Cell::new(Duration::ZERO);
        let cfg = ViewportCfg::default();
        let r = settle(
            &cfg,
            |_| it.next().unwrap(),
            || clock.get(),
            |d| clock.set(clock.get() + d),
        )
        .unwrap();
        assert_eq!(r, ((1440, 900), true));
        assert_eq!(clock.get(), cfg.interval * 3);
    }

    #[test]
    fn settle_without_any_reading_reports_the_error() {
        let clock = Cell::new(Duration::ZERO);
        let e = settle(
            &ViewportCfg::default(),
            |_| Err("eval timed out".to_string()),
            || clock.get(),
            |d| clock.set(clock.get() + d),
        )
        .unwrap_err();
        assert!(e.contains("eval timed out"), "{e}");
        assert_eq!(clock.get(), DEFAULT_SETTLE);
    }
}
