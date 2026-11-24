//! Structured `ui_*` log lines for the shell watchdog's events. Moved from host
//! `app_run.rs` (`log_watchdog_event`). No event carries a path, a value or a secret beyond the
//! artifact file name (the file name only: the directory is under the user's profile).

use crate::watchdog::WatchdogEvent as E;

/// Host event sink: `(event, detail)`.
pub type EventSink = fn(&str, &str);

/// Write `event` through `sink`. `sample_stalls` is the host's capture setting, reported on the
/// `ui_watchdog_started` line; `capture_cooldown_ms` is the cooldown the host configured.
pub fn log_event(event: &E, sample_stalls: bool, capture_cooldown_ms: u64, sink: EventSink) {
    let pid = std::process::id();
    match event {
        E::Started {
            threshold_ms,
            heartbeat_ms,
        } => sink(
            "ui_watchdog_started",
            &format!(
                "threshold_ms={threshold_ms} heartbeat_ms={heartbeat_ms} capture_cooldown_ms={capture_cooldown_ms} sample_stalls={sample_stalls}"
            ),
        ),
        E::Stall {
            episode,
            stalled_ms,
            threshold_ms,
        } => sink(
            "ui_main_thread_stall",
            &format!("episode={episode} pid={pid} gap_ms={stalled_ms} threshold_ms={threshold_ms}"),
        ),
        E::Recovered {
            episode,
            stalled_ms,
        } => sink(
            "ui_main_thread_recovered",
            &format!("episode={episode} gap_ms={stalled_ms}"),
        ),
        E::ObserverGap { gap_ms } => sink("ui_watchdog_observer_gap", &format!("gap_ms={gap_ms}")),
        E::CaptureSkipped { episode, reason } => sink(
            "ui_stall_capture_skipped",
            &format!("episode={episode} reason={reason}"),
        ),
        E::CaptureStarted { episode } => sink(
            "ui_stall_capture_started",
            &format!("episode={episode} pid={pid}"),
        ),
        E::CaptureFinished {
            episode,
            elapsed_ms,
            artifact,
        } => sink(
            "ui_stall_capture_finished",
            &format!(
                "episode={episode} success={} elapsed_ms={elapsed_ms} file={}",
                artifact.is_some(),
                artifact
                    .as_deref()
                    .and_then(|path| std::path::Path::new(path).file_name())
                    .and_then(|name| name.to_str())
                    .unwrap_or("none")
            ),
        ),
        E::CaptureFailed { episode, reason } => sink(
            "ui_stall_capture_failed",
            &format!("episode={episode} reason={reason}"),
        ),
        E::Stopped { reason } => sink("ui_watchdog_stopped", &format!("reason={reason}")),
    }
}
