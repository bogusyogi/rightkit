//! Shared test scaffolding for the Claude harness (cfg(test) only).
//!
//! The scripted process mirrors the contract the real
//! `StdioProcessIo` implements (`CliProcessIo`): newline JSON frames
//! in, newline frames out, shared interior state through `Arc` the
//! same way `StdioProcessIo` shares its locks.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::driver_host::claude_harness::supervisor::HarnessSpawner;
use crate::driver_host::cli_runtime::adapter::{
    CliLaunchSpec, CliProcessIo, CliRuntimeError, StartedCliProcess,
};
use crate::driver_host::cli_runtime::ids::{CliProviderId, CliTransportKind};
use crate::driver_host::{ClaudeLaunchOptions, RUN_SCOPED_POLICY};

/// Scripted vendor process for harness tests.
pub struct ScriptedProcessIo {
    inner: Arc<ScriptedInner>,
}

pub struct ScriptedInner {
    pub lines: Mutex<VecDeque<String>>,
    pub written: Mutex<Vec<String>>,
    pub dead: Mutex<bool>,
    pub shutdowns: Mutex<u32>,
}

impl ScriptedInner {
    /// Same accessors as [`ScriptedProcessIo`], for tests that hold the shared
    /// `Arc<ScriptedInner>` handle rather than the io wrapper itself.
    pub fn written(&self) -> Vec<String> {
        self.written.lock().unwrap().clone()
    }

    pub fn shutdown_count(&self) -> u32 {
        *self.shutdowns.lock().unwrap()
    }
}

impl ScriptedProcessIo {
    pub fn new(lines: Vec<String>) -> Self {
        Self {
            inner: Arc::new(ScriptedInner {
                lines: Mutex::new(lines.into_iter().collect()),
                written: Mutex::new(Vec::new()),
                dead: Mutex::new(false),
                shutdowns: Mutex::new(0),
            }),
        }
    }

    pub fn shared(handle: &Arc<ScriptedInner>) -> Self {
        Self {
            inner: handle.clone(),
        }
    }

    pub fn inner(&self) -> Arc<ScriptedInner> {
        self.inner.clone()
    }

    pub fn written(&self) -> Vec<String> {
        self.inner.written.lock().unwrap().clone()
    }

    pub fn is_dead(&self) -> bool {
        *self.inner.dead.lock().unwrap()
    }

    pub fn shutdown_count(&self) -> u32 {
        *self.inner.shutdowns.lock().unwrap()
    }
}

impl CliProcessIo for ScriptedProcessIo {
    fn write_line(&self, line: &str) -> Result<(), CliRuntimeError> {
        if self.is_dead() {
            return Err(CliRuntimeError::new("scripted_dead", "process stopped"));
        }
        self.inner.written.lock().unwrap().push(line.to_string());
        Ok(())
    }

    fn read_event(&self) -> Result<Option<serde_json::Value>, CliRuntimeError> {
        if self.is_dead() {
            return Ok(None);
        }
        let next = self.inner.lines.lock().unwrap().pop_front();
        match next {
            Some(line) => serde_json::from_str(&line)
                .map(Some)
                .map_err(|error| CliRuntimeError::new("scripted_json", error.to_string())),
            None => Ok(None),
        }
    }

    fn shutdown(&self) -> Result<(), CliRuntimeError> {
        *self.inner.dead.lock().unwrap() = true;
        *self.inner.shutdowns.lock().unwrap() += 1;
        Ok(())
    }

    fn clone_box(&self) -> Box<dyn CliProcessIo> {
        Box::new(Self {
            inner: self.inner.clone(),
        })
    }
}

/// Build the adapter-shaped launch spec the runtime expects: derived
/// from the legacy `ClaudeLaunchOptions::launch_spec` exactly like
/// `ClaudeAdapter::build_launch` does, with the session workspace as
/// the trust-checked cwd.
pub fn claude_launch_for_workspace(workspace_root: &Path) -> CliLaunchSpec {
    let options = ClaudeLaunchOptions {
        executable: PathBuf::from("/usr/local/bin/claude"),
        cwd: workspace_root.to_path_buf(),
        mcp_config: r#"{"mcpServers":{"host":{"command":"/usr/local/bin/host-mcp","args":[]}}}"#
            .to_string(),
        session_id: Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
        resume_session_id: None,
        model: None,
    };
    let legacy = options.launch_spec().expect("baseline launch spec");
    CliLaunchSpec {
        provider_id: CliProviderId::new("claude"),
        transport: CliTransportKind::ClaudeStreamJson,
        executable: legacy.program,
        argv: legacy.args,
        env_keep: Vec::new(),
        env_remove: legacy.env_remove.into_iter().collect(),
        env: legacy.env.into_iter().collect(),
        cwd: workspace_root.to_path_buf(),
        mcp_server: None,
        profile_hash: "profile-hash".into(),
        session_id: "550e8400-e29b-41d4-a716-446655440000".into(),
    }
}

/// Helper assertion: the baseline launch carries the run-scoped
/// policy verbatim (byte-equivalence anchor from the frozen argv
/// snapshot lineage).
pub fn baseline_launch_carries_run_scoped_policy(launch: &CliLaunchSpec) -> bool {
    launch.argv.contains(&RUN_SCOPED_POLICY.to_string())
}

/// Build a harness spawner backed by one shared scripted process.
pub fn scripted_spawner(
    handle: &Arc<ScriptedInner>,
) -> Box<dyn Fn(&CliLaunchSpec, u64) -> Result<StartedCliProcess, CliRuntimeError> + Send + Sync> {
    let handle = handle.clone();
    Box::new(move |_launch, generation| {
        // A freshly spawned process is alive, even though it shares the
        // scripted inner with the one it replaced.
        *handle.dead.lock().unwrap() = false;
        Ok(StartedCliProcess {
            pid: 4242,
            generation,
            inner: Box::new(ScriptedProcessIo::shared(&handle)),
        })
    })
}

/// A scripted spawner that also records every launch spec it was
/// given, so tests can assert the runtime finalized the spec (trust
/// cwd, managed-home env pinning) before spawning.
#[derive(Clone)]
pub struct ScriptedSpawner {
    inner: Arc<ScriptedSpawnerState>,
}

struct ScriptedSpawnerState {
    handle: Arc<ScriptedInner>,
    launched: Mutex<Vec<CliLaunchSpec>>,
}

impl ScriptedSpawner {
    pub fn new(handle: Arc<ScriptedInner>) -> Self {
        Self {
            inner: Arc::new(ScriptedSpawnerState {
                handle,
                launched: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn harness_spawner(&self) -> HarnessSpawner {
        let inner = self.inner.clone();
        Box::new(move |launch, generation| {
            inner.launched.lock().unwrap().push(launch.clone());
            *inner.handle.dead.lock().unwrap() = false;
            Ok(StartedCliProcess {
                pid: 4242,
                generation,
                inner: Box::new(ScriptedProcessIo::shared(&inner.handle)),
            })
        })
    }

    pub fn launched(&self) -> Vec<CliLaunchSpec> {
        self.inner.launched.lock().unwrap().clone()
    }
}
