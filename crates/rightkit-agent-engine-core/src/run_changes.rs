//! Post-effect workspace-change capture seam.
//!
//! Tools already return typed [`WorkspaceMutation`] evidence after a filesystem
//! effect lands. The engine owns the hand-off boundary; the daemon owns the
//! durable review store. Keeping that dependency inverted lets every desktop
//! run publish the exact mutations it produced without making the engine depend
//! on the daemon binary.

use rightkit_agent_tools::coding_tools::WorkspaceMutation;
use std::sync::Mutex;

/// Durable sink for post-effect workspace mutations.
///
/// This is deliberately synchronous: the tool result is not published to the
/// transcript until capture acknowledges it, so the visible result cannot
/// claim an edit that the review surface cannot account for.
pub trait RunChangesSink: Send + Sync {
    /// Establish a stable run identity and capture its git baseline before the
    /// first tool side effect. Implementations must make repeated calls for the
    /// same `run_id` idempotent.
    fn begin_run(
        &self,
        _session_id: &str,
        _run_id: &str,
        _workspace_root: &std::path::Path,
    ) -> Result<(), String> {
        Ok(())
    }

    fn record_mutations(
        &self,
        session_id: &str,
        run_id: &str,
        workspace_root: &std::path::Path,
        tool_call_id: &str,
        mutations: &[WorkspaceMutation],
        tool_failed: bool,
    ) -> Result<(), String>;
}

/// Test sink that records calls without touching a daemon-owned store.
#[derive(Debug, Default)]
pub struct InMemoryRunChangesSink {
    writes: Mutex<Vec<(String, String, usize, bool)>>,
}

impl InMemoryRunChangesSink {
    pub fn writes(&self) -> Vec<(String, String, usize, bool)> {
        self.writes.lock().unwrap().clone()
    }
}

impl RunChangesSink for InMemoryRunChangesSink {
    fn record_mutations(
        &self,
        session_id: &str,
        run_id: &str,
        _workspace_root: &std::path::Path,
        tool_call_id: &str,
        mutations: &[WorkspaceMutation],
        tool_failed: bool,
    ) -> Result<(), String> {
        self.writes.lock().unwrap().push((
            session_id.to_string(),
            format!("{run_id}:{tool_call_id}"),
            mutations.len(),
            tool_failed,
        ));
        Ok(())
    }
}
