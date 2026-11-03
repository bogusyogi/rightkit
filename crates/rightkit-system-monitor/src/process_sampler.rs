//! One long-lived process table shared by every process listing.
//!
//! Building a `System` per listing re-reads every process from scratch (and `everything()` also
//! reads each command line, environment and working directory), and CPU percent needs two
//! refreshes of the same table, which used to cost a 200 ms sleep per listing. Here the table lives
//! for the whole process: each listing refreshes only CPU and memory, the executable path and owner
//! are read once per process (sysinfo replaces them if the pid is reused), and a listing that
//! follows a recent one is served from the table already held.

use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use sysinfo::{
    MINIMUM_CPU_UPDATE_INTERVAL, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind,
};

/// A table refreshed longer ago than this is primed again first, so CPU percent is a recent
/// reading and not an average over the whole gap.
const WARM_FOR: Duration = Duration::from_secs(15);

struct Table {
    system: System,
    refreshed: Option<Instant>,
}

fn kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_disk_usage()
        .with_exe(UpdateKind::OnlyIfNotSet)
        .with_user(UpdateKind::OnlyIfNotSet)
}

impl Table {
    fn refresh(&mut self) {
        let warm = match self.refreshed {
            // sysinfo does not recompute CPU inside this interval, so a refresh would add cost only.
            Some(at) if at.elapsed() < MINIMUM_CPU_UPDATE_INTERVAL => return,
            Some(at) => at.elapsed() < WARM_FOR,
            None => false,
        };
        if !warm {
            self.system
                .refresh_processes_specifics(ProcessesToUpdate::All, true, kind());
            std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
        }
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::All, true, kind());
        self.refreshed = Some(Instant::now());
    }
}

/// Runs `read` on the process table after refreshing it. Callers are serialised, so two listings
/// never run at once and a second caller reuses the first one's refresh when it follows closely.
/// Fails when the OS refuses sysinfo its system tables (sysinfo 0.40 `System::new`).
pub fn with_processes<R>(read: impl FnOnce(&System) -> R) -> Result<R, crate::SamplerError> {
    static TABLE: OnceLock<Result<Mutex<Table>, String>> = OnceLock::new();
    let table = TABLE
        .get_or_init(|| {
            System::new()
                .map(|system| {
                    Mutex::new(Table {
                        system,
                        refreshed: None,
                    })
                })
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|message| {
            crate::SamplerError::from(sysinfo::Error::Other(message.clone().into()))
        })?;
    let mut table = table.lock().unwrap_or_else(PoisonError::into_inner);
    table.refresh();
    Ok(read(&table.system))
}
