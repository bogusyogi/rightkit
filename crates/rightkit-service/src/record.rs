use crate::suite::Suite;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Bumped on any incompatible change to discovery files, frames or the
/// allowlist semantics. A client rejects a different major.
pub const PROTOCOL_MAJOR: u32 = 1;
pub const PROTOCOL_MINOR: u32 = 0;

/// Rank of a record or takeover request that carries none (HeardRight's
/// `service.json` / `takeover.json` name only a `role`). Never wins a takeover:
/// a host resolves it through [`crate::ServiceConfig::known_roles`] or treats
/// the incumbent as unbeatable.
pub const RANK_UNKNOWN: u8 = u8::MAX;

fn is_zero(v: &u64) -> bool {
    *v == 0
}

fn rank_unknown() -> u8 {
    RANK_UNKNOWN
}

/// `<suite>/runtime/<service>/service.json`.
///
/// A superset of HeardRight's `ServiceRecord` (`protocol_major`, `protocol_minor`,
/// `role`, `pid`, `endpoint`, `engine_version`): the fields HeardRight does not
/// write default on read, and `engine_version` is written alongside `version`
/// so an unchanged HeardRight client still parses a record written here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRecord {
    pub protocol_major: u32,
    pub protocol_minor: u32,
    #[serde(default)]
    pub service: String,
    /// Product label of the host (HeardRight: `heard_right_shell`, ...).
    pub role: String,
    /// Lower rank wins the service. [`RANK_UNKNOWN`] when the writer gave none.
    #[serde(default = "rank_unknown")]
    pub rank: u8,
    pub pid: u32,
    /// Unix socket path or named-pipe name.
    pub endpoint: String,
    #[serde(default)]
    pub version: String,
    /// Publication time (ms since epoch) of this record. HeardRight-compatible
    /// meaning; never used to validate the PID.
    #[serde(default)]
    pub started_ms: u64,
    /// Birth time of the owning process (ms since epoch), when the writer knows
    /// it. Only when present is it compared against the live process to reject a
    /// reused PID; records without it fall back to plain PID liveness.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub process_started_ms: u64,
    /// HeardRight's name for `version`; mirrored so either reader works.
    #[serde(default)]
    pub engine_version: String,
}

impl ServiceRecord {
    /// The host version, from whichever field the writer used.
    pub fn host_version(&self) -> &str {
        if self.version.is_empty() {
            &self.engine_version
        } else {
            &self.version
        }
    }
}

/// `<suite>/runtime/<service>/takeover.json`: written by a would-be host that
/// outranks the incumbent; deleted by the requester once hosting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverRequest {
    pub role: String,
    /// HeardRight's `takeover.json` has no rank; [`RANK_UNKNOWN`] then.
    #[serde(default = "rank_unknown")]
    pub rank: u8,
    pub pid: u32,
    pub requested_at_ms: u64,
}

pub fn record_path(suite: &Suite, service: &str) -> PathBuf {
    suite.service_dir(service).join("service.json")
}
pub fn takeover_path(suite: &Suite, service: &str) -> PathBuf {
    suite.service_dir(service).join("takeover.json")
}

pub fn read_record(suite: &Suite, service: &str) -> Option<ServiceRecord> {
    serde_json::from_slice(&std::fs::read(record_path(suite, service)).ok()?).ok()
}
pub fn read_takeover(suite: &Suite, service: &str) -> Option<TakeoverRequest> {
    serde_json::from_slice(&std::fs::read(takeover_path(suite, service)).ok()?).ok()
}

/// The record only when its owner process is alive (HeardRight's staleness
/// rule: a dead pid means the record is stale). Does not probe the endpoint;
/// connecting does that and reports `service_unavailable`.
pub fn read_live_record(suite: &Suite, service: &str) -> Option<ServiceRecord> {
    read_record(suite, service).filter(crate::discovery::record_process_matches)
}

/// Publish a takeover request (atomic, owner-only).
pub fn write_takeover(suite: &Suite, service: &str, req: &TakeoverRequest) -> std::io::Result<()> {
    crate::suite::make_private_dir(&suite.service_dir(service)).map_err(std::io::Error::other)?;
    write_atomic(
        &takeover_path(suite, service),
        &serde_json::to_vec_pretty(req).unwrap_or_default(),
    )
}

/// Remove `takeover.json` only if `pid` wrote it.
pub fn clear_takeover_if_owner(suite: &Suite, service: &str, pid: u32) {
    if read_takeover(suite, service).is_some_and(|t| t.pid == pid) {
        let _ = std::fs::remove_file(takeover_path(suite, service));
    }
}

/// Temp file + rename in the same directory, owner-only.
pub fn write_atomic(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?
        };
        #[cfg(not(unix))]
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body)?;
        f.sync_all()?;
    }
    replace_with_retry(&tmp, path)
}

/// Windows refuses to rename over a file another process has open without delete sharing (the
/// incumbent host and clients poll these records), failing transiently with "Access is denied".
/// Retry briefly; elsewhere rename never fails that way.
fn replace_with_retry(tmp: &Path, path: &Path) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match std::fs::rename(tmp, path) {
            Err(e)
                if cfg!(windows)
                    && e.kind() == std::io::ErrorKind::PermissionDenied
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                let _ = std::fs::remove_file(tmp);
                return Err(e);
            }
            Ok(()) => return Ok(()),
        }
    }
}

/// Which methods each calling app may use. `"*"` as an app key is the default
/// for apps with no entry of their own; `"*"` as a method allows every method
/// for that app. `ping` is always allowed. An empty allowlist denies everyone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Allowlist {
    apps: BTreeMap<String, BTreeSet<String>>,
}

impl Allowlist {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn allow<I, S>(mut self, app: &str, methods: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.apps
            .entry(app.to_string())
            .or_default()
            .extend(methods.into_iter().map(Into::into));
        self
    }
    fn entry(&self, app: &str) -> Option<&BTreeSet<String>> {
        self.apps.get(app).or_else(|| self.apps.get("*"))
    }
    pub fn app_known(&self, app: &str) -> bool {
        self.entry(app).is_some()
    }
    pub fn permits(&self, app: &str, method: &str) -> bool {
        method == "ping"
            || self
                .entry(app)
                .is_some_and(|m| m.contains(method) || m.contains("*"))
    }
    pub fn methods_for(&self, app: &str) -> Vec<String> {
        self.entry(app)
            .map(|m| m.iter().cloned().collect())
            .unwrap_or_default()
    }
}
