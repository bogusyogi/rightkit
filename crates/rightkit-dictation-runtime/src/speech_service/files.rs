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

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_speech_client::{
        takeover_request_path, SERVICE_PROTOCOL_MAJOR, SERVICE_PROTOCOL_MINOR,
    };

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hr-speech-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn record(role: HostRole, pid: u32) -> ServiceRecord {
        ServiceRecord {
            protocol_major: SERVICE_PROTOCOL_MAJOR,
            protocol_minor: SERVICE_PROTOCOL_MINOR,
            role,
            pid,
            endpoint: "e".into(),
            engine_version: "v".into(),
        }
    }

    #[test]
    fn takeover_decision_follows_rank() {
        let lower = record(HostRole::ScrapeRight, 4_000_000);
        assert!(should_request_takeover(
            HostRole::HeardRightShell,
            Some(&lower)
        ));
        assert!(should_request_takeover(
            HostRole::ScrapeRight,
            Some(&record(HostRole::CutRight, 4_000_000))
        ));
        assert!(!should_request_takeover(HostRole::CutRight, Some(&lower)));
        assert!(!should_request_takeover(
            HostRole::ScrapeRight,
            Some(&lower)
        ));
        assert!(!should_request_takeover(HostRole::HeardRightShell, None));
    }

    #[test]
    fn yield_requires_live_outranking_requester() {
        let req = TakeoverRequest {
            role: HostRole::HeardRightShell,
            pid: 77,
            requested_at_ms: 1,
        };
        assert!(takeover_obliges_yield(
            HostRole::ScrapeRight,
            Some(&req),
            5,
            &|_| true
        ));
        assert!(!takeover_obliges_yield(
            HostRole::ScrapeRight,
            Some(&req),
            5,
            &|_| false
        ));
        assert!(!takeover_obliges_yield(
            HostRole::ScrapeRight,
            Some(&req),
            77,
            &|_| true
        ));
        assert!(!takeover_obliges_yield(
            HostRole::HeardRightShell,
            Some(&req),
            5,
            &|_| true
        ));
        assert!(!takeover_obliges_yield(
            HostRole::ScrapeRight,
            None,
            5,
            &|_| true
        ));
    }

    #[test]
    fn record_round_trip_stale_and_guard() {
        let root = tmp("record");
        let me = std::process::id();
        write_record(&root, &record(HostRole::HeardRightShell, me)).unwrap();
        assert_eq!(read_live_record(&root, &|_| true).unwrap().pid, me);
        assert!(
            read_live_record(&root, &|_| false).is_none(),
            "dead pid is stale"
        );
        {
            let _guard = RecordGuard::new(root.clone(), me);
        }
        assert!(read_json::<ServiceRecord>(&service_record_path(&root)).is_none());
        // A guard must not remove a record owned by someone else.
        write_record(&root, &record(HostRole::ScrapeRight, me + 1)).unwrap();
        drop(RecordGuard::new(root.clone(), me));
        assert!(read_json::<ServiceRecord>(&service_record_path(&root)).is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn takeover_file_is_cleared_only_by_owner() {
        let root = tmp("takeover");
        write_takeover(&root, HostRole::HeardRightShell).unwrap();
        clear_takeover_if_ours(&root);
        assert!(!takeover_request_path(&root).exists());
        write_json_atomic(
            &takeover_request_path(&root),
            &TakeoverRequest {
                role: HostRole::HeardRightShell,
                pid: std::process::id() + 1,
                requested_at_ms: 0,
            },
        )
        .unwrap();
        clear_takeover_if_ours(&root);
        assert!(takeover_request_path(&root).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn own_pid_is_alive() {
        assert!(pid_alive(std::process::id()));
    }
}
