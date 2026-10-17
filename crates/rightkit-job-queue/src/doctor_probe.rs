//! Client-side discovery probes. Readiness is always live; only capabilities are cached.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use rightkit_child_process::{OutputCapture, OwnedCommand};
use serde_json::{json, Value};

use crate::{JobError, JobResult};

/// Default doctor deadline (also covers draining stdout).
pub const DEFAULT_DOCTOR_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum accepted stdout bytes. Larger configured limits are clipped to this ceiling.
pub const MAX_DOCTOR_BYTES: usize = 8 * 1024 * 1024;
/// Maximum binary identities retained by one [`DoctorProbe`]. Oldest entries are evicted.
pub const MAX_DOCTOR_CACHE_ENTRIES: usize = 128;
/// Additional bounded cleanup time after a failed probe; reap continues in background if needed.
pub const DOCTOR_CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);

/// Limits for one doctor invocation. Non-zero exit is accepted when stdout is valid doctor JSON.
#[derive(Clone, Copy, Debug)]
pub struct DoctorOptions {
    /// Budget for process execution & stdout EOF, starting before spawn.
    pub timeout: Duration,
    /// First bytes retained while excess output is drained & discarded. Clipped to [`MAX_DOCTOR_BYTES`].
    pub max_stdout_bytes: usize,
}

impl Default for DoctorOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_DOCTOR_TIMEOUT,
            max_stdout_bytes: MAX_DOCTOR_BYTES,
        }
    }
}

/// Extracts `app_version`, `contract_version`, `capabilities` & optional `readiness`.
/// Other doctor fields are ignored. Capability entries remain engine-defined JSON.
/// Errors use `doctor_bad_json` or `doctor_bad_shape`, with supervisor origin.
pub fn parse_doctor(stdout: &str) -> JobResult<Value> {
    let doc: Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        JobError::supervisor("doctor_bad_json", format!("doctor output is not JSON: {e}"))
    })?;
    let (Some(version), Some(contract), Some(caps)) = (
        doc.get("app_version").and_then(Value::as_str),
        doc.get("contract_version").and_then(Value::as_u64),
        doc.get("capabilities").and_then(Value::as_array),
    ) else {
        return Err(JobError::supervisor(
            "doctor_bad_shape",
            "doctor output lacks app_version, contract_version, or capabilities",
        ));
    };
    Ok(json!({
        "app_version": version, "contract_version": contract, "capabilities": caps,
        "readiness": doc.get("readiness").cloned().unwrap_or(Value::Null),
    }))
}

/// Runs `<engine> doctor` with default output limit & supplied deadline, without caching.
/// `engine` must be an explicit absolute file path; PATH lookup is never used.
pub fn run_doctor(engine: impl AsRef<Path>, timeout: Duration) -> JobResult<Value> {
    run_doctor_with(
        engine.as_ref(),
        DoctorOptions {
            timeout,
            ..Default::default()
        },
    )
}

fn run_doctor_with(engine: &Path, options: DoctorOptions) -> JobResult<Value> {
    if !engine.is_absolute() {
        return Err(JobError::supervisor(
            "bad_input",
            "doctor needs an absolute engine path",
        ));
    }
    let started = Instant::now();
    let timeout = || {
        JobError::supervisor(
            "doctor_timeout",
            "doctor did not finish within its deadline",
        )
    };
    if options.timeout.is_zero() {
        return Err(timeout());
    }
    let mut command = OwnedCommand::new(engine);
    command.windows_hide();
    command
        .command_mut()
        .arg("doctor")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|e| {
        JobError::supervisor(
            "spawn_failed",
            format!("cannot run {}: {e}", engine.display()),
        )
    })?;
    child.set_drop_reap_timeout(DOCTOR_CLEANUP_TIMEOUT);
    let stdout = child.take_stdout().expect("stdout piped");
    let capture =
        OutputCapture::try_capture(stdout, options.max_stdout_bytes.min(MAX_DOCTOR_BYTES))
            .map_err(|e| {
                JobError::supervisor("spawn_failed", format!("cannot capture doctor stdout: {e}"))
            })?;
    // Wait for EOF before reaping the leader. If descendants hold stdout open, timeout
    // drops an unreaped OwnedChild so its containment still kills the whole tree.
    while !capture.is_finished() {
        let remaining = options.timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(timeout());
        }
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
    if let Some(e) = capture.take_error() {
        return Err(JobError::supervisor("doctor_read_failed", e.to_string()));
    }
    match child.wait_timeout(options.timeout.saturating_sub(started.elapsed())) {
        Ok(Some(_)) => {}
        Ok(None) => return Err(timeout()),
        Err(e) => return Err(JobError::supervisor("wait_failed", e.to_string())),
    }
    let output = capture.snapshot();
    if output.truncated {
        return Err(JobError::supervisor(
            "doctor_output_limit",
            "doctor stdout exceeded capture limit",
        ));
    }
    let text = std::str::from_utf8(&output.bytes).map_err(|e| {
        JobError::supervisor(
            "doctor_bad_json",
            format!("doctor stdout is not UTF-8: {e}"),
        )
    })?;
    parse_doctor(text)
}

// Metadata identity is intentionally not a content digest. Stat before & after probes
// prevents caching a result across replacement during execution. Unix also detects inode
// replacement & same-length writes whose mtime was restored.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BinaryIdentity {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    #[cfg(unix)]
    unix: (u64, u64, i64, i64),
}

impl BinaryIdentity {
    fn read(path: &Path) -> Option<Self> {
        if !path.is_absolute() {
            return None;
        }
        let path = path.canonicalize().ok()?;
        let metadata = std::fs::metadata(&path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Some(Self {
            path,
            len: metadata.len(),
            modified: metadata.modified().ok()?,
            created: metadata.created().ok(),
            #[cfg(unix)]
            unix: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        })
    }
}

/// Share across discovery requests to remember capabilities of unchanged engine binaries.
/// Every [`probe`](Self::probe) runs doctor live, even after a cache hit. Successful probes
/// cache only `capabilities`; readiness, errors & full documents are never cached.
/// Metadata identity includes canonical path, size & modification time (plus Unix inode/ctime).
/// It detects ordinary upgrades; it is not a cryptographic authenticity check.
#[derive(Default)]
pub struct DoctorProbe {
    options: DoctorOptions,
    capabilities: Mutex<VecDeque<(BinaryIdentity, Value)>>,
}

impl DoctorProbe {
    /// Creates a probe with its own bounded cache. Use one shared instance per discovery client.
    pub fn new(options: DoctorOptions) -> Self {
        Self {
            options,
            capabilities: Mutex::default(),
        }
    }

    /// Runs doctor live, remembering capabilities only if binary identity stayed unchanged.
    /// Concurrent probes do not hold the cache mutex while running processes.
    pub fn probe(&self, engine: impl AsRef<Path>) -> JobResult<Value> {
        let engine = engine.as_ref();
        let before = BinaryIdentity::read(engine);
        let doc = run_doctor_with(engine, self.options)?;
        if let Some(identity) =
            before.filter(|key| Some(key) == BinaryIdentity::read(engine).as_ref())
        {
            self.remember(identity, doc["capabilities"].clone());
        }
        Ok(doc)
    }

    fn remember(&self, identity: BinaryIdentity, capabilities: Value) {
        let mut cache = self.capabilities.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|(key, _)| key.path != identity.path);
        cache.push_back((identity, capabilities));
        while cache.len() > MAX_DOCTOR_CACHE_ENTRIES {
            cache.pop_front();
        }
    }

    /// Last capabilities for this exact current binary identity. Missing/unreadable/changed
    /// binaries return `None`. This does not spawn doctor or return cached readiness.
    pub fn cached_capabilities(&self, engine: impl AsRef<Path>) -> Option<Value> {
        let identity = BinaryIdentity::read(engine.as_ref())?;
        let cache = self.capabilities.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .iter()
            .find(|(key, _)| key == &identity)
            .map(|(_, caps)| caps.clone())
    }

    /// Discards remembered capabilities without affecting running probes.
    pub fn clear_cache(&self) {
        self.capabilities
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_document_without_caching_readiness_or_extra_fields() {
        let doc = parse_doctor(r#"{"app_version":"1","contract_version":1,"capabilities":[{"name":"render","version":1}],"extra":true}"#).unwrap();
        assert_eq!(doc["readiness"], Value::Null);
        assert!(doc.get("extra").is_none());
        assert_eq!(doc["capabilities"][0]["name"], "render");
    }

    #[test]
    fn distinguishes_bad_json_and_shape() {
        assert_eq!(parse_doctor("oops").unwrap_err().code, "doctor_bad_json");
        for text in [
            "null",
            "[]",
            "{}",
            r#"{"app_version":"1","contract_version":-1,"capabilities":[]}"#,
            r#"{"app_version":1,"contract_version":1,"capabilities":[]}"#,
            r#"{"app_version":"1","contract_version":1,"capabilities":{}}"#,
        ] {
            assert_eq!(parse_doctor(text).unwrap_err().code, "doctor_bad_shape");
        }
    }

    #[test]
    fn explicit_path_and_zero_budget() {
        assert_eq!(
            run_doctor("engine-on-path", Duration::from_secs(1))
                .unwrap_err()
                .code,
            "bad_input"
        );
        assert_eq!(
            run_doctor(std::env::current_exe().unwrap(), Duration::ZERO)
                .unwrap_err()
                .code,
            "doctor_timeout"
        );
    }
    #[test]
    fn cache_evicts_oldest_identity_and_replaces_old_versions() {
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        std::fs::write(&engine, "binary").unwrap();
        let identity = BinaryIdentity::read(&engine).unwrap();
        let probe = DoctorProbe::default();
        for i in 0..=MAX_DOCTOR_CACHE_ENTRIES {
            let mut key = identity.clone();
            key.path = dir.path().join(format!("engine-{i}"));
            probe.remember(key, json!([i]));
        }
        {
            let cache = probe.capabilities.lock().unwrap();
            assert_eq!(cache.len(), MAX_DOCTOR_CACHE_ENTRIES);
            assert_eq!(cache.front().unwrap().1, json!([1]));
        }
        probe.remember(identity.clone(), json!(["old"]));
        let mut upgraded = identity;
        upgraded.len += 1;
        probe.remember(upgraded, json!(["new"]));
        let cache = probe.capabilities.lock().unwrap();
        assert_eq!(cache.len(), MAX_DOCTOR_CACHE_ENTRIES);
        assert_eq!(cache.back().unwrap().1, json!(["new"]));
        assert!(!cache.iter().any(|(_, caps)| caps == &json!(["old"])));
    }
}
