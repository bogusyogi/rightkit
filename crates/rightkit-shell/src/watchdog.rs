//! Main-thread hang watchdog.
//!
//! Base: HeardRight `hang_watchdog` (a dedicated OS thread polls a heartbeat that a recurring
//! `run_on_main_thread` ping refreshes; the report fires once per stall episode and re-arms after a
//! fresh beat; neither thread takes an app lock, so the watchdog can observe a deadlock without
//! joining it). Changes: the report goes to a caller callback and a `rightkit-shell://stall`
//! event instead of HeardRight's crash-dump module, and the threshold/interval are configurable.
//! HeardRight's opt-in macOS stack-sampling variant (`ui_stall_watchdog`) is not generalised.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter, Wry};

use crate::EVENT_STALL;

#[derive(Debug, Clone, Copy)]
pub struct StallReport {
    pub stalled_ms: u64,
    pub threshold_ms: u64,
}

pub type StallCallback = Arc<dyn Fn(&StallReport) + Send + Sync>;

#[derive(Clone)]
pub struct WatchdogConfig {
    pub threshold: Duration,
    pub heartbeat_interval: Duration,
    pub on_stall: Option<StallCallback>,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self { threshold: Duration::from_secs(5), heartbeat_interval: Duration::from_secs(1), on_stall: None }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Edge-triggered decision, separated from the loop.
pub fn should_fire(last_beat_ms: u64, now_ms: u64, already_fired: bool, threshold_ms: u64) -> bool {
    !already_fired && now_ms.saturating_sub(last_beat_ms) > threshold_ms
}

pub fn install(app: AppHandle<Wry>, cfg: WatchdogConfig) {
    let last = Arc::new(AtomicU64::new(now_ms()));
    let fired = Arc::new(AtomicBool::new(false));
    {
        let (app, last, interval) = (app.clone(), Arc::clone(&last), cfg.heartbeat_interval);
        std::thread::Builder::new()
            .name("rk-shell-heartbeat".into())
            .spawn(move || loop {
                std::thread::sleep(interval);
                let last = Arc::clone(&last);
                // Err means the event loop is gone (app exiting): stop.
                if app.run_on_main_thread(move || last.store(now_ms(), Ordering::SeqCst)).is_err() {
                    break;
                }
            })
            .ok();
    }
    let threshold = cfg.threshold.as_millis() as u64;
    let poll = cfg.heartbeat_interval.min(Duration::from_millis(250)).max(Duration::from_millis(50));
    std::thread::Builder::new()
        .name("rk-shell-watchdog".into())
        .spawn(move || loop {
            std::thread::sleep(poll);
            let (beat, now) = (last.load(Ordering::SeqCst), now_ms());
            if should_fire(beat, now, fired.load(Ordering::SeqCst), threshold) {
                fired.store(true, Ordering::SeqCst);
                let report = StallReport { stalled_ms: now.saturating_sub(beat), threshold_ms: threshold };
                if let Some(cb) = &cfg.on_stall {
                    cb(&report);
                }
                if app.emit(EVENT_STALL, serde_json::json!({"stalledMs": report.stalled_ms, "thresholdMs": report.threshold_ms})).is_err() {
                    break;
                }
            } else if fired.load(Ordering::SeqCst) && now.saturating_sub(beat) <= threshold {
                fired.store(false, Ordering::SeqCst);
            }
        })
        .ok();
}
