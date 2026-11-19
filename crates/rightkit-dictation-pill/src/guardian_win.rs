//! Windows event source for the visibility guardian: a WinEvent pump thread that turns
//! foreground and geometry events into [`GuardianEvent`]s for one guardian generation. The pump
//! thread name is a host input.
//!
//! Lifetime: dropping the sources asks the pump to quit (`WM_QUIT`) and returns; the pump thread
//! is not joined and ends on its own. Until it does, its callbacks name this generation and are
//! dropped once the worker cleared its slot, so they can never wake a newer guardian.

use std::sync::mpsc;
use std::time::Duration;

use rightkit_overlay::{windows::run_foreground_event_pump, ForegroundEvent, ForegroundPumpStop};

use crate::guardian::{notify_generation, GuardianEvent, GuardianTimings};

pub(crate) struct GuardianSources {
    stop: ForegroundPumpStop,
}

impl GuardianSources {
    /// If hook installation fails, the guardian uses its degraded repair interval. Events are
    /// reported to `generation` only.
    pub(crate) fn start(
        generation: u64,
        pump_thread_name: &str,
        timings: &GuardianTimings,
    ) -> Option<Self> {
        // Transfer the stop handle only while the guardian is waiting. If it times
        // out during startup, the pump stops as soon as its hooks are installed.
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        std::thread::Builder::new()
            .name(pump_thread_name.to_string())
            .spawn(move || {
                run_foreground_event_pump(
                    move |stop| {
                        if ready_tx.send(stop).is_err() {
                            stop.stop();
                        }
                    },
                    move |event| {
                        notify_generation(
                            generation,
                            match event {
                                ForegroundEvent::Focus => GuardianEvent::ForegroundChanged,
                                ForegroundEvent::Geometry => GuardianEvent::GeometryChanged,
                            },
                        );
                    },
                );
            })
            .ok()?;
        let stop = ready_rx
            .recv_timeout(Duration::from_millis(timings.os_source_start_timeout_ms))
            .ok()?;
        Some(Self { stop })
    }

    /// The pump updates its hooks when foreground changes.
    pub(crate) fn refresh(&mut self) {}
}

impl Drop for GuardianSources {
    fn drop(&mut self) {
        self.stop.stop();
    }
}
