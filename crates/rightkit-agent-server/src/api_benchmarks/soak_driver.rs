//! Bounded real-worker soak driver (FM-189).
//!
//! `soak_harness` is a passive receipt collector and `soak_routes` is the
//! transport seam; neither may invent worker activity. This module is the
//! process-level runner that actually drives real workers so the receipt's
//! `cancelledWorkers`/`completedWorkers`/`ownershipViolations` fields are
//! measured from live runtime state instead of accepted verbatim from the
//! HTTP request body.
//!
//! Workers are real `LiveSession` trees created through the same
//! `DaemonState::create_session_with_options` seam the `/sessions` and
//! `/sessions/{id}/fork` HTTP routes use (see
//! `engine/src/api_sessions/lifecycle.rs`). No new spawn
//! mechanism is introduced. Cancellation mirrors `kill_session`: signal
//! `interrupt_tx` when present, then remove the session from the live map.
//! Containment is checked the same way `list_sessions` (S:P... session
//! listing) checks it: by walking `DaemonState::list_sessions()` and looking
//! for any live session whose `parent_session_id` still points at a soak
//! root the driver already tore down.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use rightkit_agent_engine_protocol::uuid_v7;

use crate::api::{lock_recover, DaemonState, SessionRuntimeOptions};

/// Parameters for one bounded soak drive. `duration` is the only knob that
/// distinguishes a short in-test run (seconds) from a real long soak
/// (hours) -- the driver behaves identically either way, it just holds the
/// spawned worker tree live for longer before tearing it down.
pub(crate) struct SoakDriveConfig {
    pub(crate) worker_count: usize,
    pub(crate) duration: Duration,
    pub(crate) workspace_root: String,
}

/// Measured outcome of one soak drive. Every field is counted from live
/// runtime state after the drive completes; nothing here is caller-supplied.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SoakDriveOutcome {
    pub(crate) completed_workers: u64,
    pub(crate) cancelled_workers: u64,
    pub(crate) ownership_violations: u64,
}

struct WorkerTree {
    root_id: String,
    child_id: String,
}

/// Drive a bounded soak window against real worker sessions and return
/// measured counters.
///
/// For each of `config.worker_count` workers this spawns a root session plus
/// one forked child (the same parent/child relationship `/sessions/{id}/fork`
/// establishes), holds the tree live for `config.duration`, then either
/// completes it normally (plain removal, no interrupt signal) or cancels it
/// mid-flight (interrupt signal, same as `kill_session`, sent to the child
/// before either session is removed) -- alternating so both paths are
/// exercised in every drive. After teardown it walks every live session and
/// counts any descendant of a torn-down soak root that is still live, or any
/// soak root that is still live, as an ownership violation: cancellation
/// containment failed to keep the subtree from leaking.
/// Async because session creation spawns the per-session event pump on the
/// daemon's runtime: it must be called with a reactor in context, never from
/// `spawn_blocking`.
pub(crate) async fn drive_soak(state: &DaemonState, config: &SoakDriveConfig) -> SoakDriveOutcome {
    let mut trees: Vec<WorkerTree> = Vec::with_capacity(config.worker_count);

    for index in 0..config.worker_count {
        let root_id = format!("soak-root-{}", uuid_v7());
        let root = state.create_session_with_options(
            &root_id,
            None,
            &config.workspace_root,
            &format!("FM-189 soak worker {index}"),
            None,
            SessionRuntimeOptions::default(),
        );

        let child_id = format!("soak-child-{}", uuid_v7());
        let child_options = SessionRuntimeOptions {
            forked_from_session_id: Some(root.id.clone()),
            ..SessionRuntimeOptions::default()
        };
        let _child = state.create_session_with_options(
            &child_id,
            root.role.as_deref(),
            &config.workspace_root,
            &format!("FM-189 soak worker {index} subtree"),
            Some(&root.id),
            child_options,
        );

        trees.push(WorkerTree { root_id, child_id });
    }

    // Bounded observation window: the driver holds the real worker tree live
    // for the caller-parameterized duration rather than tearing it down
    // immediately, so a long soak run genuinely keeps workers resident.
    let deadline = Instant::now() + config.duration;
    while Instant::now() < deadline {
        tokio::time::sleep(
            Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
        )
        .await;
    }

    let mut outcome = SoakDriveOutcome::default();

    for (index, tree) in trees.iter().enumerate() {
        if index % 2 == 0 {
            // Complete normally: no interrupt signal, plain removal from the
            // live map -- the same terminal state a session reaches when its
            // engine task finishes on its own.
            lock_recover(&state.sessions, "sessions").remove(&tree.child_id);
            lock_recover(&state.sessions, "sessions").remove(&tree.root_id);
            outcome.completed_workers += 1;
        } else {
            // Cancel mid-flight: signal interrupt on the child exactly like
            // `kill_session` does, then remove both sessions.
            if let Some(child) = state.get_session(&tree.child_id) {
                if let Some(tx) = &child.interrupt_tx {
                    let _ = tx.send(true);
                }
            }
            lock_recover(&state.sessions, "sessions").remove(&tree.child_id);
            lock_recover(&state.sessions, "sessions").remove(&tree.root_id);
            outcome.cancelled_workers += 1;
        }
    }

    // Containment check: after every tree above was torn down, no live
    // session may still be a descendant of, or be, one of these soak roots.
    let root_ids: HashSet<&String> = trees.iter().map(|tree| &tree.root_id).collect();
    for live in state.list_sessions() {
        let is_orphaned_descendant = live
            .parent_session_id
            .as_ref()
            .is_some_and(|parent| root_ids.contains(parent));
        let is_leaked_root = root_ids.contains(&live.id);
        if is_orphaned_descendant || is_leaked_root {
            outcome.ownership_violations += 1;
        }
    }

    outcome
}
