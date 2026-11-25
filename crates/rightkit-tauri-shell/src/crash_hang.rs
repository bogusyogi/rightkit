//! Main-thread hang evidence through `rightkit_crash::HangWatchdog`, pumped by the main thread.
//!
//! The host supplies the crash configuration, the threshold and the log vocabulary; this module
//! starts the watchdog, beats it from the main thread (`run_on_main_thread`), logs each stall and
//! recovery, and keeps the watchdog alive for the whole process.

use std::time::Duration;

use rightkit_crash::{CrashConfig, HangEvent, HangOptions, HangWatchdog};
use tauri::{AppHandle, Runtime};

/// Names and timing for [`install_main_thread`].
pub struct MainThreadHang {
    /// Silence longer than this is a hang (one report per episode).
    pub threshold: Duration,
    /// How often the watchdog checks.
    pub check_interval: Duration,
    /// Name of the pumped loop, recorded in each event.
    pub loop_name: &'static str,
    /// Logged when a stalled loop beats again: `loop=<n> gap_ms=<ms>`.
    pub recovered_event: &'static str,
    /// Logged on a stall: `loop=<n> gap_ms=<ms> threshold_ms=<t> report=<bool>`.
    pub stall_event: &'static str,
    /// Logged when the watchdog could not start or be pumped: `error=<e>`.
    pub failed_event: &'static str,
}

/// Start the hang watchdog for the main thread. Failures are logged as `failed_event` and leave
/// the app running without the watchdog.
///
/// `crash` is the host's crash configuration (report folder, sink, native capture) and `log` its
/// `(event, detail)` sink.
pub fn install_main_thread<R: Runtime>(
    app: &AppHandle<R>,
    crash: CrashConfig,
    spec: MainThreadHang,
    log: fn(&str, &str),
) {
    let threshold_ms = spec.threshold.as_millis();
    let (recovered_event, stall_event) = (spec.recovered_event, spec.stall_event);
    let options = HangOptions::new()
        .threshold(spec.threshold)
        .check_interval(spec.check_interval)
        .on_hang(move |event: &HangEvent| {
            if event.recovered {
                log(
                    recovered_event,
                    &format!("loop={} gap_ms={}", event.name, event.stalled.as_millis()),
                );
            } else {
                log(
                    stall_event,
                    &format!(
                        "loop={} gap_ms={} threshold_ms={threshold_ms} report={}",
                        event.name,
                        event.stalled.as_millis(),
                        event.report.is_some()
                    ),
                );
            }
        });
    let watchdog = match HangWatchdog::start(crash, options) {
        Ok(watchdog) => watchdog,
        Err(error) => {
            log(spec.failed_event, &format!("error={error}"));
            return;
        }
    };
    let handle = app.clone();
    if let Err(error) = watchdog.pump_via(spec.loop_name, move |beat| {
        handle.run_on_main_thread(beat).is_ok()
    }) {
        log(spec.failed_event, &format!("error={error}"));
        return;
    }
    // Lives for the whole process: dropping it would stop the watchdog.
    std::mem::forget(watchdog);
}
