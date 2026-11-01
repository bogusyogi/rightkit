//! `rightkit-agent-engine-tasks` — the task graph (dependency DAG) and per-session task tracker.

pub mod graph;
pub mod tracker;

pub use graph::{TaskGraph, TaskGraphError};
pub use tracker::SessionTaskTracker;

// Re-export the shared protocol task types for convenience.
pub use rightkit_agent_engine_protocol::{TaskNode, TaskState};
