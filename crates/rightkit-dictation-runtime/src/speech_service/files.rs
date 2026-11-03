//! Discovery / takeover file I/O and pure decision logic.

use std::io;
use std::path::{Path, PathBuf};

use rightkit_speech_client::{
    service_dir, service_record_path, HostRole, ServiceRecord, TakeoverRequest,
};
use serde::{de::DeserializeOwned, Serialize};

pub fn suite_root() -> PathBuf {
    rightkit_dictation_models::shared::store::paths::suite_root()
}

/// Shared atomic publication retains the historical speech discovery schema.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    rightkit_daemon::write_atomic(path, &bytes)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The service record, only if its pid is alive (stale records are ignored).
pub fn read_live_record(root: &Path, alive: &dyn Fn(u32) -> bool) -> Option<ServiceRecord> {
    let record: ServiceRecord = read_json(&service_record_path(root))?;
    alive(record.pid).then_some(record)
}

/// Takeover is requested only against a live, lower-ranked host.
pub fn should_request_takeover(our: HostRole, record: Option<&ServiceRecord>) -> bool {
    record.is_some_and(|r| our.outranks(r.role) && r.pid != std::process::id())
}

/// A takeover request that obliges a host running as `ours` to yield.
pub fn takeover_obliges_yield(
    ours: HostRole,
    request: Option<&TakeoverRequest>,
    own_pid: u32,
    alive: &dyn Fn(u32) -> bool,
) -> bool {
    request.is_some_and(|r| r.pid != own_pid && r.role.outranks(ours) && alive(r.pid))
}

pub use rightkit_speech_client::{
    clear_takeover_if_ours, pid_alive, request_takeover as write_takeover,
};

pub fn write_record(root: &Path, record: &ServiceRecord) -> io::Result<()> {
    std::fs::create_dir_all(service_dir(root))?;
    write_json_atomic(&service_record_path(root), record)
}

/// Removes service.json on drop, but only while it still names our pid.
pub struct RecordGuard {
    root: PathBuf,
    pid: u32,
}

impl RecordGuard {
    pub fn new(root: PathBuf, pid: u32) -> Self {
        Self { root, pid }
    }

    pub fn remove_now(&self) {
        let path = service_record_path(&self.root);
        if read_json::<ServiceRecord>(&path).is_some_and(|r| r.pid == self.pid) {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Drop for RecordGuard {
    fn drop(&mut self) {
        self.remove_now();
    }
}
