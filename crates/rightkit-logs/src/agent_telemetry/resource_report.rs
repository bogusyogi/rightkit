//! OPS-009 resource-report contract.
//!
//! Canon: "Resource claims distinguish incremental the host app cost from combined
//! the host app + canonical Membrane steady state; never advertise a whole-system
//! one-process result under the current Membrane canon."
//!
//! This module is the *contract and instrument*, not the numbers. Every metric
//! starts as `None` and stays `None` unless a real process was actually
//! observed on a host that supports that counter. Nothing here estimates,
//! interpolates, or substitutes a plausible value — consistent with the
//! `baseline` instrument in this crate.
//!
//! The single property this module exists to enforce: a the host app-only
//! measurement can never be rendered as a whole-system result. Membrane is
//! separate first-party infrastructure and is always accounted separately.

use std::collections::BTreeMap;

/// Who owns a measured process. Ownership is labelled, never inferred from a
/// total: a report that saw no Membrane process is not a whole-system report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProcessOwnership {
    /// The resident the host app process itself.
    HostPrimary,
    /// A bounded task-scoped the host app helper/child process. Permitted by
    /// practical/expanded profiles; the strict profile permits zero of these.
    HostHelper,
    /// Canonical Membrane infrastructure. Never folded into the host app cost.
    Membrane,
}

impl ProcessOwnership {
    pub fn label(&self) -> &'static str {
        match self {
            Self::HostPrimary => "rightkit-agent-primary",
            Self::HostHelper => "rightkit-agent-helper",
            Self::Membrane => "membrane",
        }
    }

    fn is_host(&self) -> bool {
        matches!(self, Self::HostPrimary | Self::HostHelper)
    }
}

/// One observed process. Every counter is optional because host support
/// genuinely varies (PSS is Linux-only; commit size is Windows-only; FD counts
/// require procfs or an equivalent). `None` means "not measured on this host",
/// never zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessResourceSample {
    pub pid: Option<u32>,
    /// Resident set size, bytes.
    pub rss_bytes: Option<u64>,
    /// Proportional set size, bytes. Linux-only; the honest figure when
    /// processes share mappings.
    pub pss_bytes: Option<u64>,
    /// Windows commit size, bytes.
    pub commit_bytes: Option<u64>,
    pub threads: Option<u32>,
    /// Open file descriptors (Unix) or handles (Windows).
    pub open_descriptors: Option<u32>,
}

impl ProcessResourceSample {
    /// True when nothing at all was measured for this process. Such a sample
    /// contributes to the child/helper count but to no resource total.
    pub fn is_empty(&self) -> bool {
        self.rss_bytes.is_none()
            && self.pss_bytes.is_none()
            && self.commit_bytes.is_none()
            && self.threads.is_none()
            && self.open_descriptors.is_none()
    }
}

/// A labelled process observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelledProcess {
    pub ownership: ProcessOwnership,
    /// Human-readable identity for the report (e.g. the executable name).
    pub name: String,
    pub sample: ProcessResourceSample,
}

/// When the sample was taken relative to the workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReclamationPoint {
    /// Steady state before the task runs.
    PreTask,
    /// Peak/steady state during the task.
    DuringTask,
    /// After the task completed and helpers were reaped — the point at which
    /// reclamation is asserted.
    PostTask,
}

/// Aggregated cost for one ownership group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceTotals {
    pub process_count: usize,
    pub rss_bytes: Option<u64>,
    pub pss_bytes: Option<u64>,
    pub commit_bytes: Option<u64>,
    pub threads: Option<u32>,
    pub open_descriptors: Option<u32>,
}

/// Summing rule: a total is `Some` only if at least one contributing process
/// reported that counter, and it sums only the processes that did. A group with
/// no measurement for a counter reports `None` ("not measured"), never `0`.
fn total_of<'a, T, F>(processes: impl Iterator<Item = &'a LabelledProcess>, pick: F) -> Option<T>
where
    T: std::ops::Add<Output = T> + Copy + 'a,
    F: Fn(&ProcessResourceSample) -> Option<T>,
{
    processes.fold(None, |acc, process| match (acc, pick(&process.sample)) {
        (Some(sum), Some(value)) => Some(sum + value),
        (None, Some(value)) => Some(value),
        (acc, None) => acc,
    })
}

/// Why a combined the host app + Membrane claim is unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CombinedClaimRefusal {
    /// No Membrane process was observed. A the host app-only measurement must not
    /// be presented as a whole-system steady state.
    NoMembraneProcessObserved,
    /// Membrane processes were seen but reported no usable counter.
    MembraneProcessesUnmeasured,
}

impl std::fmt::Display for CombinedClaimRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoMembraneProcessObserved => f.write_str(
                "no canonical Membrane process was observed; \
                 a Host-only measurement is not a whole-system result",
            ),
            Self::MembraneProcessesUnmeasured => f.write_str(
                "Membrane processes were observed but reported no resource counter on this host",
            ),
        }
    }
}

/// One resource report at one reclamation point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceReport {
    pub point: ReclamationPoint,
    pub processes: Vec<LabelledProcess>,
}

impl ResourceReport {
    pub fn new(point: ReclamationPoint) -> Self {
        Self {
            point,
            processes: Vec::new(),
        }
    }

    pub fn observe(&mut self, process: LabelledProcess) -> &mut Self {
        self.processes.push(process);
        self
    }

    fn group(&self, ownership: ProcessOwnership) -> impl Iterator<Item = &LabelledProcess> {
        self.processes
            .iter()
            .filter(move |process| process.ownership == ownership)
    }

    fn totals_for(&self, mut include: impl FnMut(&LabelledProcess) -> bool) -> ResourceTotals {
        let selected = self
            .processes
            .iter()
            .filter(|process| include(process))
            .collect::<Vec<_>>();
        ResourceTotals {
            process_count: selected.len(),
            rss_bytes: total_of(selected.iter().copied(), |s| s.rss_bytes),
            pss_bytes: total_of(selected.iter().copied(), |s| s.pss_bytes),
            commit_bytes: total_of(selected.iter().copied(), |s| s.commit_bytes),
            threads: total_of(selected.iter().copied(), |s| s.threads),
            open_descriptors: total_of(selected.iter().copied(), |s| s.open_descriptors),
        }
    }

    /// (1) Incremental the host app runtime cost: the the host app process plus any
    /// the host app helpers. Membrane is deliberately excluded.
    pub fn incremental_host(&self) -> ResourceTotals {
        self.totals_for(|process| process.ownership.is_host())
    }

    /// Membrane's own cost, always reported separately.
    pub fn membrane(&self) -> ResourceTotals {
        self.totals_for(|process| process.ownership == ProcessOwnership::Membrane)
    }

    /// (2) Combined the host app + canonical Membrane steady state.
    ///
    /// Refuses rather than silently returning a the host app-only figure when no
    /// Membrane process participated. This refusal is the whole point of the
    /// atom: it makes "advertise the host app's topology as a whole-system
    /// one-process result" unrepresentable rather than merely discouraged.
    pub fn combined_with_membrane(&self) -> Result<ResourceTotals, CombinedClaimRefusal> {
        let membrane = self.group(ProcessOwnership::Membrane).collect::<Vec<_>>();
        if membrane.is_empty() {
            return Err(CombinedClaimRefusal::NoMembraneProcessObserved);
        }
        if membrane.iter().all(|process| process.sample.is_empty()) {
            return Err(CombinedClaimRefusal::MembraneProcessesUnmeasured);
        }
        Ok(self.totals_for(|_| true))
    }

    /// the host app child/helper processes only. The strict profile requires zero.
    pub fn host_helper_count(&self) -> usize {
        self.group(ProcessOwnership::HostHelper).count()
    }

    /// Per-ownership breakdown for rendering, so a report can never present a
    /// single undifferentiated total.
    pub fn by_ownership(&self) -> BTreeMap<ProcessOwnership, ResourceTotals> {
        [
            ProcessOwnership::HostPrimary,
            ProcessOwnership::HostHelper,
            ProcessOwnership::Membrane,
        ]
        .into_iter()
        .map(|ownership| {
            (
                ownership,
                self.totals_for(|process| process.ownership == ownership),
            )
        })
        .collect()
    }
}

/// Post-task reclamation assertion: helpers spawned during a task must be gone
/// afterwards, and the host app's own resident cost must not have grown without
/// bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclamationOutcome {
    pub helpers_during: usize,
    pub helpers_after: usize,
    /// `None` when either side lacked an RSS measurement.
    pub host_rss_delta_bytes: Option<i128>,
}

impl ReclamationOutcome {
    pub fn evaluate(during: &ResourceReport, after: &ResourceReport) -> Self {
        let rss_before = during.incremental_host().rss_bytes;
        let rss_after = after.incremental_host().rss_bytes;
        Self {
            helpers_during: during.host_helper_count(),
            helpers_after: after.host_helper_count(),
            host_rss_delta_bytes: match (rss_before, rss_after) {
                (Some(before), Some(after)) => Some(after as i128 - before as i128),
                _ => None,
            },
        }
    }

    /// True when every helper observed during the task was reclaimed.
    pub fn helpers_fully_reclaimed(&self) -> bool {
        self.helpers_after == 0
    }
}

#[cfg(unix)]
pub mod procfs {
    //! Best-effort Linux sampling. Every counter this host cannot supply stays
    //! `None`; nothing here fabricates a value.
    use super::ProcessResourceSample;

    fn kib_field(status: &str, key: &str) -> Option<u64> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key))?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
            .map(|kib| kib * 1024)
    }

    /// Sample `/proc/<pid>`. Returns an all-`None` sample on a host without
    /// procfs rather than pretending the process used nothing.
    pub fn sample_pid(pid: u32) -> ProcessResourceSample {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        let pss_bytes = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
            .ok()
            .and_then(|rollup| kib_field(&rollup, "Pss:"));
        ProcessResourceSample {
            pid: Some(pid),
            rss_bytes: kib_field(&status, "VmRSS:"),
            pss_bytes,
            commit_bytes: None, // Windows-only concept.
            threads: status
                .lines()
                .find_map(|line| line.strip_prefix("Threads:"))
                .and_then(|value| value.trim().parse::<u32>().ok()),
            open_descriptors: std::fs::read_dir(format!("/proc/{pid}/fd"))
                .ok()
                .map(|entries| entries.count() as u32),
        }
    }

    /// Sample the current process.
    pub fn sample_self() -> ProcessResourceSample {
        sample_pid(std::process::id())
    }

    /// Enumerate the direct children of `pid` by scanning `/proc/*/stat` for
    /// a matching `PPid`. An empty vec is a real measurement ("no children");
    /// callers needing "host cannot enumerate" must treat a non-procfs host
    /// (anything but Linux) as unsupported, since this scan only works with procfs.
    pub fn children_of(pid: u32) -> Vec<u32> {
        let mut children = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return children;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.parse::<u32>().is_err() {
                continue;
            }
            let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else {
                continue;
            };
            let parent = status
                .lines()
                .find_map(|line| line.strip_prefix("PPid:")?.trim().parse::<u32>().ok());
            if parent == Some(pid) {
                if let Ok(child) = name.parse::<u32>() {
                    children.push(child);
                }
            }
        }
        children.sort_unstable();
        children
    }
}

#[cfg(not(target_os = "linux"))]
pub mod platform_topology {
    //! Hosts without procfs report child enumeration as unsupported rather
    //! than returning a fabricated zero-child topology. macOS has no procfs,
    //! so an empty listing there would be a false "no children" measurement.

    /// `None` — this host cannot enumerate children without a platform API
    /// this crate does not bind (Windows Toolhelp is bound by the daemon's
    /// own topology sampler in `resource_metrics.rs`).
    pub fn children_of(_pid: u32) -> Option<Vec<u32>> {
        None
    }
}

#[cfg(target_os = "linux")]
pub mod platform_topology {
    /// `Some(children)` — procfs enumeration is a real measurement.
    pub fn children_of(pid: u32) -> Option<Vec<u32>> {
        Some(super::procfs::children_of(pid))
    }
}
