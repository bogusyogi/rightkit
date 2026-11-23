//! Launch-origin attribution and a bounded startup deferral for login launches.
//!
//! A host that registers itself to start at login passes a marker argument in the login entry
//! (`rightkit_autostart` takes `AutostartOptions::args`). [`is_login_launch`] reads it, and for an
//! entry that predates the marker falls back to boot uptime. [`StartupGate`] then lets the host
//! defer its heavy startup work (engine, model load, prewarm) while the machine is still settling
//! from login, and never otherwise. The host chooses what the heavy work is: it calls
//! [`StartupGate::wait`] from a background thread immediately before that work.
//!
//! The decision logic is pure and compiles on every OS so it can be unit tested; the uptime read
//! and the load-average gate are macOS only. Off macOS `StartupGate::wait` returns at once.
//!
//! ## Restart marker
//!
//! A restart the host itself initiated ([`crate::relaunch::relaunch_self`] with a marker) lands on
//! an already-warm machine, where waiting out a login storm would only burn the ceiling. The
//! incoming process consumes the marker with [`consume_marker`] (consume-once: deleted first, so a
//! marker left behind by a crash between write and relaunch costs at most one fast launch instead
//! of disabling the gate permanently).

use std::path::Path;
use std::time::Duration;

/// Pure decision: is this process launch attributable to login? Takes an argv iterator (minus
/// `argv[0]`) and an optional uptime so it is testable without a running app or real `sysctl` state.
///
/// 1. `login_arg` present in argv -> login launch (explicit marker from the login entry).
/// 2. Otherwise fall back to boot uptime: within `boot_window` of boot -> login launch (covers
///    installs whose login entry predates the marker).
/// 3. Uptime unreadable -> NOT a login launch. This deliberately fails toward the fast
///    manual-launch path: a missed gate costs some startup contention, a wrongly-applied gate
///    costs the user a long splash, and manual launches are the common case.
pub fn is_login_launch<I, S>(
    args: I,
    uptime: Option<Duration>,
    login_arg: &str,
    boot_window: Duration,
) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if args.into_iter().any(|arg| arg.as_ref() == login_arg) {
        return true;
    }
    match uptime {
        Some(uptime) => uptime < boot_window,
        None => false,
    }
}

/// Consume-once restart marker: true when the marker existed and was deleted.
pub fn consume_marker(marker: &Path) -> bool {
    std::fs::remove_file(marker).is_ok()
}

/// Seconds since boot, via `sysctl(KERN_BOOTTIME)`. Returns `None` if the
/// syscall fails or the clock looks implausible (boottime in the future), which callers must treat
/// as "unknown" rather than "long uptime".
#[cfg(target_os = "macos")]
pub fn seconds_since_boot() -> Option<std::time::Duration> {
    let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    let mut boottime = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = std::mem::size_of::<libc::timeval>();
    // SAFETY: mib points at a 2-element array matching namelen; boottime is
    // sized exactly per `len`, matching sysctl's documented contract.
    let ok = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            &mut boottime as *mut _ as *mut std::ffi::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if ok != 0 || len != std::mem::size_of::<libc::timeval>() || boottime.tv_sec <= 0 {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let uptime = now - boottime.tv_sec;
    if uptime < 0 {
        return None;
    }
    Some(std::time::Duration::from_secs(uptime as u64))
}

/// 1-minute load average, normalized by logical CPU count. `None` if the
/// platform call fails (treated as "can't tell, don't block on it").
#[cfg(target_os = "macos")]
pub fn normalized_load_average() -> Option<f64> {
    let mut samples: [f64; 3] = [0.0; 3];
    // SAFETY: getloadavg writes at most `nelem` f64s into `samples`; nelem=1
    // matches the buffer we pass.
    let got = unsafe { libc::getloadavg(samples.as_mut_ptr(), 1) };
    if got != 1 {
        return None;
    }
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1) as f64;
    Some(samples[0] / cpus)
}

/// Host-supplied policy of the startup deferral. Free memory is deliberately not read: the gate
/// runs on load average alone, which on the machines it exists for (a boot-storm load spike on a
/// small laptop) coincides with swapping and memory starvation.
#[derive(Clone, Debug)]
pub struct StartupGate {
    /// Argument the host's login entry carries. Required.
    pub login_arg: String,
    /// Without the marker, a process observed within this long of boot counts as a login launch.
    pub boot_window: Duration,
    /// Load-average sampling interval while waiting.
    pub poll: Duration,
    /// Normalized load below which the wait ends.
    pub load_threshold: f64,
    /// The longest the gate ever waits. KEEP THIS TIGHT: when the host's heavy startup work is also
    /// what ends its splash, every second here is a second the splash stays up, and a long wait
    /// does not make the app usable sooner, only later with less contention.
    pub hard_ceiling: Duration,
    /// After this long a `startup_gate_slow` event is emitted once.
    pub slow_threshold: Duration,
    /// Restart marker the host writes through `relaunch_self`; consumed once per launch.
    pub restart_marker: Option<std::path::PathBuf>,
    /// Receives `(event, detail)` diagnostics. Without one they are dropped.
    pub event_sink: Option<fn(&str, &str)>,
}

impl StartupGate {
    /// Defaults: boot window 60 s, poll 2 s, load threshold 1.5, ceiling 20 s, slow after 15 s.
    pub fn new(login_arg: &str) -> Self {
        Self {
            login_arg: login_arg.to_owned(),
            boot_window: Duration::from_secs(60),
            poll: Duration::from_secs(2),
            load_threshold: 1.5,
            hard_ceiling: Duration::from_secs(20),
            slow_threshold: Duration::from_secs(15),
            restart_marker: None,
            event_sink: None,
        }
    }

    #[cfg(target_os = "macos")]
    fn event(&self, event: &str, detail: &str) {
        if let Some(sink) = self.event_sink {
            sink(event, detail);
        }
    }

    /// Adaptive readiness gate for the host's heavy startup work, and ONLY for login-triggered
    /// launches. A manual launch (Finder/Dock/Spotlight/`open`) skips the gate and proceeds
    /// immediately; blocking it regresses perceived launch time on ordinary relaunches. Never call
    /// this on the main thread. Returns immediately (no polling, no event) when
    /// `skip_before_marker()` is true, which the host uses for modes that must never wait (capture
    /// or test modes); then consumes the restart marker (a hit returns at once); then returns
    /// immediately when `skip_after_marker()` is true; then returns when the launch is not a login
    /// launch.
    #[cfg(target_os = "macos")]
    pub fn wait(
        &self,
        skip_before_marker: impl FnOnce() -> bool,
        skip_after_marker: impl FnOnce() -> bool,
    ) {
        if skip_before_marker() {
            return;
        }

        // Consume-once: delete first, so a marker left behind by a crash between
        // write and relaunch costs at most one fast launch instead of disabling the
        // gate permanently.
        if let Some(marker) = &self.restart_marker {
            if consume_marker(marker) {
                self.event(
                    "startup_gate",
                    "proceed_reason=update_relaunch elapsed_ms=0",
                );
                return;
            }
        }
        if skip_after_marker() {
            return;
        }

        let uptime = seconds_since_boot();
        if !is_login_launch(
            std::env::args_os()
                .skip(1)
                .filter_map(|arg| arg.into_string().ok()),
            uptime,
            &self.login_arg,
            self.boot_window,
        ) {
            self.event(
                "startup_gate",
                "proceed_reason=not_login_launch elapsed_ms=0",
            );
            return;
        }

        let started = std::time::Instant::now();
        let mut slow_logged = false;
        loop {
            let elapsed = started.elapsed();
            if elapsed >= self.hard_ceiling {
                self.event(
                    "startup_gate",
                    &format!(
                        "proceed_reason=timeout elapsed_ms={} load=n/a",
                        elapsed.as_millis()
                    ),
                );
                return;
            }

            let load = normalized_load_average();
            self.event(
                "startup_gate",
                &format!(
                    "poll load={} free_mb=n/a",
                    load.map(|l| format!("{l:.2}"))
                        .unwrap_or_else(|| "unknown".to_string())
                ),
            );

            let load_ok = match load {
                Some(l) => l < self.load_threshold,
                // Can't read load average at all: don't hold the engine hostage
                // on an unreadable signal; treat it as "ok" and let the hard
                // ceiling be the only remaining backstop for the failure case
                // that matters (an actually overloaded, readable machine).
                None => true,
            };
            if load_ok {
                self.event(
                    "startup_gate",
                    &format!(
                        "proceed_reason=load_ok elapsed_ms={} load={}",
                        elapsed.as_millis(),
                        load.map(|l| format!("{l:.2}"))
                            .unwrap_or_else(|| "unknown".to_string())
                    ),
                );
                return;
            }

            if !slow_logged && elapsed >= self.slow_threshold {
                slow_logged = true;
                self.event(
                    "startup_gate_slow",
                    &format!("elapsed_ms={}", elapsed.as_millis()),
                );
            }

            let remaining = self.hard_ceiling.saturating_sub(started.elapsed());
            // A zero interval must not turn a login storm into a busy loop, and a
            // host-selected long interval must not extend the configured ceiling.
            std::thread::sleep(self.poll.max(Duration::from_millis(1)).min(remaining));
        }
    }

    /// Off macOS there is no login-storm detector: returns at once.
    #[cfg(not(target_os = "macos"))]
    pub fn wait(
        &self,
        _skip_before_marker: impl FnOnce() -> bool,
        _skip_after_marker: impl FnOnce() -> bool,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_login_argument_wins_over_unknown_or_late_uptime() {
        for uptime in [None, Some(Duration::from_secs(600))] {
            assert!(is_login_launch(
                ["--login-launch"],
                uptime,
                "--login-launch",
                Duration::from_secs(60),
            ));
        }
    }

    #[test]
    fn fallback_requires_known_uptime_strictly_inside_boot_window() {
        for (uptime, expected) in [
            (None, false),
            (Some(Duration::from_secs(59)), true),
            (Some(Duration::from_secs(60)), false),
            (Some(Duration::from_secs(61)), false),
        ] {
            assert_eq!(
                is_login_launch(
                    ["--login-launch-extra"],
                    uptime,
                    "--login-launch",
                    Duration::from_secs(60),
                ),
                expected,
            );
        }
    }

    #[test]
    fn marker_is_consumed_once_without_removing_parent_or_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("restart-marker");
        let sibling = dir.path().join("settings");
        std::fs::write(&marker, b"1").unwrap();
        std::fs::write(&sibling, b"preserve").unwrap();
        assert!(consume_marker(&marker));
        assert!(!consume_marker(&marker));
        assert!(dir.path().is_dir());
        assert_eq!(std::fs::read(&sibling).unwrap(), b"preserve");
        assert!(!consume_marker(dir.path()));
        assert!(dir.path().is_dir());
    }
}
