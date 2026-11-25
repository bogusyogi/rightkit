//! Policy for a process that guards its own memory (Pulse #501, #571). A watchdog that only
//! reports is no safety net: an installed engine once grew to 58 GB in 21 h (iroh 1.3.0,
//! n0-computer/iroh#4390) with nothing to stop it. The host runs the loop (its thread, its log,
//! its settings, its exit code); this module owns the decisions and the restart marker.
//!
//! * [`cap_bytes`]: the configured cap in MB, never below [`MIN_CAP_MB`] (a smaller figure would
//!   restart a healthy process that idles near 260 MB in a loop).
//! * [`judge`]: over the cap is [`Verdict::Exceeded`]; otherwise [`Verdict::Log`] at most every
//!   [`LOG_EVERY`] and whenever the footprint first rises past 25, 50 and 75 percent of the cap
//!   ([`LEVELS`]), else [`Verdict::Fine`].
//! * [`next_wait`]: adaptive sampling, one read a [`TICK`] while the footprint is small and
//!   steady, one every [`FAST_TICK`] once it is at or above half of the cap or has grown by more
//!   than [`FAST_GROWTH_MB`] since the last read, so a runaway at 6 GB a minute is caught within
//!   seconds of the cap.
//! * [`footprint_bytes`]: the process's own footprint (macOS `phys_footprint`, Windows private
//!   bytes through [`crate::process_window`]; Linux `VmRSS`); `None` when unreadable, which a host
//!   logs ([`UNREADABLE_LOG_EVERY`]) and never treats as zero.
//! * [`write_marker`] / [`parse_marker`]: when the guard ends the process it leaves a marker with
//!   numbers only (`why`, `footprintMb`, `capMb`, `pid`, `atMs`); the process that starts next
//!   reads it, so the restart is visible to the user and not only to the supervisor. A marker
//!   older than a day is dropped ([`MARKER_MAX_AGE_MS`]).

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

/// How often the footprint is read while it is small and steady.
pub const TICK: Duration = Duration::from_secs(60);
/// How often it is read once it is large (at or above half of the cap) or growing fast.
pub const FAST_TICK: Duration = Duration::from_secs(5);
/// Growth between two reads, in MB, that switches to the fast tick.
pub const FAST_GROWTH_MB: u64 = 128;
/// An unreadable footprint is logged at most this often.
pub const UNREADABLE_LOG_EVERY: Duration = Duration::from_secs(3600);
/// The footprint event is written at most this often when nothing crossed a threshold.
pub const LOG_EVERY: Duration = Duration::from_secs(600);
/// The default cap, in MB.
pub const DEFAULT_CAP_MB: f64 = 1024.0;
/// The lowest cap honoured, in MB.
pub const MIN_CAP_MB: f64 = 512.0;
/// Bytes in one MB as this module counts them.
pub const MB: u64 = 1024 * 1024;
/// Footprint thresholds, in percent of the cap, that each ask for a log line once (rising).
pub const LEVELS: [u64; 3] = [25, 50, 75];
/// A restart marker older than this is dropped without a report.
pub const MARKER_MAX_AGE_MS: u64 = 24 * 3600 * 1000;
/// The `why` a restart marker carries when the cap ended the process.
pub const MARKER_WHY: &str = "memory_cap";

/// The cap in bytes for a configured figure in MB, raised to [`MIN_CAP_MB`].
pub fn cap_bytes(configured_mb: f64) -> u64 {
    (configured_mb.max(MIN_CAP_MB) as u64).saturating_mul(MB)
}

/// What one footprint reading calls for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Fine,
    Log,
    Exceeded,
}

/// Judges one reading. `level` is how many of [`LEVELS`] the footprint was at or above on the
/// previous call (start at 0, reset to 0 while the guard is off); it is updated here.
pub fn judge(bytes: u64, cap: u64, level: &mut usize, last_logged: Option<Instant>) -> Verdict {
    if bytes > cap {
        return Verdict::Exceeded;
    }
    let reached = LEVELS
        .iter()
        .filter(|percent| bytes >= cap / 100 * **percent)
        .count();
    let rose = reached > *level;
    *level = reached;
    if rose || last_logged.is_none_or(|at| at.elapsed() >= LOG_EVERY) {
        Verdict::Log
    } else {
        Verdict::Fine
    }
}

/// The time to the next read: [`FAST_TICK`] when the footprint is at or above half of the cap or
/// has grown by more than [`FAST_GROWTH_MB`] since `previous`, [`TICK`] otherwise.
pub fn next_wait(bytes: u64, cap: u64, previous: Option<u64>) -> Duration {
    let large = bytes >= cap / 2;
    let growing = previous.is_some_and(|before| bytes.saturating_sub(before) > FAST_GROWTH_MB * MB);
    if large || growing { FAST_TICK } else { TICK }
}

/// This process's footprint in bytes: macOS `phys_footprint` and Windows private bytes.
#[cfg(any(target_os = "macos", windows))]
pub fn footprint_bytes() -> Option<u64> {
    crate::process_window::read().and_then(|sample| sample.footprint)
}

/// Linux (servers, CI): resident set size from `/proc/self/status`.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn footprint_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// Milliseconds since the Unix epoch, 0 when the clock is before it.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Writes the restart marker to `path` (numbers only, not durable: a lost marker only loses the
/// report). Returns the error text on failure; the caller logs it and ends the process anyway.
pub fn write_marker(path: &Path, footprint_mb: u64, cap_mb: u64) -> Result<(), String> {
    let body = serde_json::json!({
        "why": MARKER_WHY,
        "footprintMb": footprint_mb,
        "capMb": cap_mb,
        "pid": std::process::id(),
        "atMs": now_ms(),
    })
    .to_string();
    let options = rightkit_fs::AtomicOptions {
        durable: false,
        ..Default::default()
    };
    rightkit_fs::atomic_write_with(path, options, |file| file.write_all(body.as_bytes()))
        .map_err(|error| error.to_string())
}

/// What a restart marker says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Restarted {
    pub footprint_mb: u64,
    pub cap_mb: u64,
    pub previous_pid: Option<u64>,
    /// Seconds between the marker and `now_ms`.
    pub ago_s: u64,
}

/// Reads a marker's bytes: `None` for anything that is not a `memory_cap` marker no older than
/// [`MARKER_MAX_AGE_MS`] at `now_ms`.
pub fn parse_marker(bytes: &[u8], now_ms: u64) -> Option<Restarted> {
    let marker: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let at_ms = marker["atMs"].as_u64().unwrap_or(0);
    if marker["why"] != MARKER_WHY || now_ms.saturating_sub(at_ms) > MARKER_MAX_AGE_MS {
        return None;
    }
    Some(Restarted {
        footprint_mb: marker["footprintMb"].as_u64().unwrap_or(0),
        cap_mb: marker["capMb"].as_u64().unwrap_or(0),
        previous_pid: marker["pid"].as_u64(),
        ago_s: now_ms.saturating_sub(at_ms) / 1000,
    })
}

/// Reads and removes the marker at `path` (removed even when stale or unreadable, so it is
/// reported once), returning what it said when it is a fresh `memory_cap` marker.
pub fn take_marker(path: &Path) -> Option<Restarted> {
    let bytes = std::fs::read(path).ok()?;
    let _ = std::fs::remove_file(path);
    parse_marker(&bytes, now_ms())
}
