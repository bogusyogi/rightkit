//! Pre-warmed reusable git-worktree pool with acquire/release lifecycle &
//! durable pool-readiness state (canon atom VG-004).
//!
//! DIRECT_PORT_RUST (lane A17-vg-rust-ports): `db` ports grok-build
//! `crates/codegen/xai-fast-worktree/src/db/mod.rs`; `sync` ports
//! `crates/codegen/xai-fast-worktree/src/sync.rs`. `pool` is original glue
//! composing the two — see its module doc for exactly what is and is not a
//! port. Full donor citations, NOTICE entries, and orchestrator wiring are
//! in `docs/canon/daemon-master-canon-atoms.md`.

pub mod db;
pub mod pool;
pub mod sync;

pub use db::{
    DbStats, ListFilter, RegistryOpen, WorktreeDb, WorktreeKind, WorktreeRecord, WorktreeStatus,
};
pub use pool::{PoolLease, WorktreePool};
pub use sync::{collect_source_dirty_state, SourceDirtyState, SyncReport, WorktreeSync};
