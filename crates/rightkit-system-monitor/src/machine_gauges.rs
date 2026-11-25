//! The arithmetic behind a machine card's CPU, memory, disk and network rings, independent of how
//! the OS counters are read (Pulse's Windows notch reads them with `GlobalMemoryStatusEx`,
//! `GetDiskFreeSpaceExW` and `GetIfTable2`; a host on another OS feeds the same shapes).
//!
//! * [`MemInfo`]: used share, the swap line (the page file as the commit limit beyond physical
//!   memory) and the pressure words `normal`, `warning` and `critical` from physical memory
//!   available and commit charge against the commit limit: under 12.5% free or 85% committed is
//!   `warning`, under 5% free or 95% committed `critical`, with 5 percentage points of hysteresis
//!   for a level already held ([`MemInfo::pressure_after`]).
//! * [`PressureHold`]: a change between two pressure levels waits [`PRESSURE_HOLD`] after the
//!   previous change, while the first reading and a lost reading are reported at once.
//! * [`Volume`]: used share of one drive.
//! * [`Reading`]: a value the host may not have yet or may never get, stated apart from zero.
//! * [`net_rate`] over [`AdapterCounters`]: bytes per second since the previous sample across the
//!   adapters present in both, skipping an adapter whose counter went backwards (a reset), with
//!   the kind of the busiest adapter.
//! * [`size_text`] and [`drive_size_text`]: three significant digits and no trailing ".0"
//!   ("22.4 GB", "994 GB", "1 TB"), in binary and decimal units respectively; [`group_thousands`].

use std::time::{Duration, Instant};

/// Least time between two reported memory pressure levels.
pub const PRESSURE_HOLD: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemInfo {
    pub total: u64,
    pub available: u64,
    /// System commit limit and bytes committed (page-file backed).
    pub commit_limit: u64,
    pub commit_used: u64,
    /// The bar's share in thousandths when a source states it apart from the figures (a view
    /// fixture; the Mac's bar is the kernel's pressure level, not used over total).
    pub stated_share: Option<u16>,
}

impl MemInfo {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn used_fraction(&self) -> f32 {
        if let Some(share) = self.stated_share {
            return f32::from(share) / 1000.0;
        }
        (self.used() as f64 / self.total.max(1) as f64).clamp(0.0, 1.0) as f32
    }

    /// The swap line the Mac shows (`vm.swapusage`): Windows' page file is the commit limit
    /// beyond physical memory, and the page-file share of the commit charge is what it holds.
    /// `None` when there is no page file (its total is 0), so the card leaves the line out.
    pub fn swap(&self) -> Option<(u64, u64)> {
        let total = self.commit_limit.saturating_sub(self.total);
        if total == 0 {
            return None;
        }
        let used = self.commit_used.saturating_sub(self.used()).min(total);
        Some((used, total))
    }

    /// The Mac's pressure words from Windows' own inputs; `None` without a total.
    pub fn pressure(&self) -> Option<&'static str> {
        self.pressure_after(None)
    }

    /// [`MemInfo::pressure`] with 5 percentage points of hysteresis: a level already held is kept
    /// until the reading is 5 points clear of its threshold, so a reading sitting on a boundary
    /// does not flip between two words.
    pub fn pressure_after(&self, held: Option<&str>) -> Option<&'static str> {
        if self.total == 0 {
            return None;
        }
        let free = self.available as f64 / self.total as f64;
        let commit = if self.commit_limit > 0 {
            self.commit_used as f64 / self.commit_limit as f64
        } else {
            0.0
        };
        const MARGIN: f64 = 0.05;
        let (critical_held, warning_held) = (
            held == Some("critical"),
            matches!(held, Some("critical" | "warning")),
        );
        let slack = |kept: bool| if kept { MARGIN } else { 0.0 };
        Some(
            if free < 0.05 + slack(critical_held) || commit >= 0.95 - slack(critical_held) {
                "critical"
            } else if free < 0.125 + slack(warning_held) || commit >= 0.85 - slack(warning_held) {
                "warning"
            } else {
                "normal"
            },
        )
    }
}

/// The pressure level a host has reported, and when it last changed.
#[derive(Clone, Copy, Debug, Default)]
pub struct PressureHold {
    level: Option<&'static str>,
    changed: Option<Instant>,
}

impl PressureHold {
    pub const fn new() -> Self {
        Self {
            level: None,
            changed: None,
        }
    }

    /// The level reported so far; feed it to [`MemInfo::pressure_after`] as the held level.
    pub fn level(&self) -> Option<&'static str> {
        self.level
    }

    /// Offers a new reading. The first reading and a lost reading are taken at once; a flip
    /// between two levels waits out [`PRESSURE_HOLD`] since the last change, then reports where
    /// the reading is by then. Returns whether the reported level changed.
    pub fn report(&mut self, reading: Option<&'static str>) -> bool {
        let settled = self.changed.is_none_or(|at| at.elapsed() >= PRESSURE_HOLD);
        if reading != self.level && (settled || self.level.is_none() || reading.is_none()) {
            self.level = reading;
            self.changed = Some(Instant::now());
            return true;
        }
        false
    }
}

/// One drive's capacity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// Root path such as `C:\`.
    pub root: String,
    pub total: u64,
    pub free: u64,
    pub system: bool,
    /// A removable drive or one on the USB bus: the Disks ring leads with it, as the Mac's
    /// gauge leads with an external volume. A second fixed drive on SATA or NVMe is internal.
    pub external: bool,
}

impl Volume {
    pub fn used_fraction(&self) -> f32 {
        let used = self.total.saturating_sub(self.free);
        (used as f64 / self.total.max(1) as f64).clamp(0.0, 1.0) as f32
    }
}

/// A reading the host may not have yet or may never get: the card says which.
#[derive(Clone, Debug, PartialEq)]
pub enum Reading<T> {
    /// Not sampled yet.
    Pending,
    /// This machine cannot provide it.
    Unavailable,
    Value(T),
}

/// Receive and send rates summed over the physical adapters that are up.
#[derive(Clone, Debug, PartialEq)]
pub struct NetRate {
    /// Bytes per second.
    pub down: f64,
    pub up: f64,
    /// "Wi-Fi", "Ethernet", "Cellular" or "Network" (whatever kind label the host gave): the
    /// adapter carrying the most traffic.
    pub kind: String,
}

/// One physical adapter's lifetime byte counters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterCounters {
    pub index: u32,
    pub kind: &'static str,
    pub received: u64,
    pub sent: u64,
}

/// Bytes per second since `before`. An adapter whose counter went backwards (reset) is
/// skipped; `None` when no adapter was present in both samples or `elapsed` is zero.
pub fn net_rate(
    before: &[AdapterCounters],
    now: &[AdapterCounters],
    elapsed: Duration,
) -> Option<NetRate> {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return None;
    }
    let (mut down, mut up) = (0u64, 0u64);
    let mut busiest: Option<(u64, &'static str)> = None;
    for adapter in now {
        let Some(old) = before.iter().find(|o| o.index == adapter.index) else {
            continue;
        };
        let (Some(rx), Some(tx)) = (
            adapter.received.checked_sub(old.received),
            adapter.sent.checked_sub(old.sent),
        ) else {
            continue;
        };
        down = down.saturating_add(rx);
        up = up.saturating_add(tx);
        let traffic = rx.saturating_add(tx);
        if busiest.is_none_or(|(most, _)| traffic > most) {
            busiest = Some((traffic, adapter.kind));
        }
    }
    let (_, kind) = busiest?;
    Some(NetRate {
        down: down as f64 / seconds,
        up: up as f64 / seconds,
        kind: kind.to_string(),
    })
}

/// `1450` as "1,450".
pub fn group_thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `value` in GB (or in TB once it reaches `step` GB), with three significant digits and no
/// trailing ".0".
fn sized(value: f64, step: f64) -> String {
    let (value, unit) = if value >= step {
        (value / step, "TB")
    } else {
        (value, "GB")
    };
    let number = if value >= 100.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}").trim_end_matches(".0").to_string()
    };
    format!("{number} {unit}")
}

/// Binary gigabytes, labelled as the Mac's cards read them: three significant digits and no
/// trailing ".0" ("22.4 GB", "994 GB", "1 TB", "1.5 TB").
pub fn size_text(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    sized(bytes as f64 / GIB, 1024.0)
}

/// Drive sizes in decimal units (1 GB = 10^9 bytes), so a notch's Disks card and a hub's
/// Overview agree. Same formatting as [`size_text`].
pub fn drive_size_text(bytes: u64) -> String {
    sized(bytes as f64 / 1e9, 1000.0)
}
