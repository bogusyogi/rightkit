//! Runtime module root — owns `RuntimeApp` (the supervised
//! engine sidecar runtime) and the `EngineRuntime` orchestration
//! around it.
//!
//! # Locking discipline (audit §2.1 MED / §2.3)
//!
//! There is still exactly ONE global `Mutex<RuntimeApp>`
//! (`runtime_lock::APP`, taken via `runtime_lock()`/`runtime_lock_for_commands()`).
//! It serializes every state-machine mutation, history write, delivery
//! finalize, and session/cancel transition — command ordering and the
//! transactional stop/cancel pipeline depend on that single-writer
//! serialization, so it is deliberately kept.
//!
//! What changed: the HOT READS and the LONG WAITS no longer go through that
//! mutex.
//!
//! * `EngineRuntime.state` is a `SharedState` (`Arc<RwLock<AppState>>`). The
//!   same `Arc` is cloned into the `SidecarHandle`, which is published once in
//!   `shared::ENGINE_SIDECAR`. `app_state_lock_free()` reads it without the
//!   global lock, so `get_app_state` and the transcribe watchdog never queue
//!   behind prewarm.
//! * `SidecarHandle` is `Clone` and internally synchronized
//!   (`Arc<EngineSupervisor>` plus `Arc<Mutex<_>>` cells). The whole handle is
//!   registered in `shared::ENGINE_SIDECAR` at `open_sidecar()` time, giving
//!   lock-free access to: supervisor (start/is_running/call), the mirrored
//!   `engine_version` cell, the mirrored `active_session` cell, the shared
//!   state snapshot, and the `info_ready` flag.
//! * `get_engine_info` (`shared::engine_info_lock_free`) is a pure snapshot
//!   read. It preserves the old "restart a dead engine" side effect by
//!   kicking `supervisor.start()` on a detached thread instead of blocking the
//!   query behind the model load.
//! * Prewarm is staged: `SidecarHandle::warm_handshake()` (process start +
//!   ready-wait + the up-to-120s `EngineInfo` handshake) runs lock-free; only
//!   the short config/vocab snapshot push (`sync_sidecar_runtime_snapshot_*`)
//!   still takes the global lock.
//!
//! Lock order: `BACKEND_TRANSITION_GATE` (runtime_lock.rs) may wrap a
//! `runtime_lock()` acquisition; nothing may acquire the transition gate while
//! holding the runtime lock (debug-asserted). The `RwLock`/`Mutex` cells inside
//! `SidecarHandle` are leaf locks — never take `runtime_lock()` or the
//! transition gate while holding one, and never hold one across a sidecar RPC.
//!
//! Module map:
//!   prepared.rs      — CancelSource, RuntimeApp enum, Prepared{Start,Stop,Cancel}
//!   app.rs           — `impl RuntimeApp` dispatch to the engine runtime
//!   engine.rs        — EngineRuntime + SidecarHandle structs, history-store
//!                      construction, storage generation, orphan-draft recovery
//!   session.rs       — `impl EngineRuntime`: session/stop/cancel/delivery ops
//!   rpc.rs           — `impl EngineRuntime`: sidecar RPC frame helpers
//!   history_ops.rs   — `impl EngineRuntime`: history CRUD, engine_info,
//!                      prewarm, wake listen, shutdown
//!   control_tail.rs  — spoken "zephyr stop/send/cancel" tail parsing + tests
//!   shared.rs        — SharedState, ENGINE_SIDECAR registry, lock-free facades

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver},
    Arc, Mutex as StdMutex, OnceLock, RwLock,
};
use std::time::{Duration, Instant};

use crate::delivery::{
    copy_fallback_record, CopyFallbackReason, DeliveryBackend, DeliveryRecord, PendingStopContext,
    PendingStopContextLatch, StopOrigin,
};
use crate::engine_process::{EngineEvent, EngineInfoResult, WakeListenParams};
use crate::engine_supervisor::{self, EngineSupervisor, SupervisorEvent};
use crate::history::{HistoryFilter, HistoryPage, HistoryQuery, HistoryStore};
use crate::state::{
    delivery_enter_was_submitted, delivery_outcome_event_with_send, transition, AppEvent, AppState,
};
use rightkit_dictation_core::engine::{EngineFrame, EnginePayload, EngineSchemaName, StopKind};

mod app;
mod control_tail;
mod engine;
mod history_ops;
mod prepared;
mod rpc;
mod session;
mod shared;

// `pub use` so `crate::runtime::X` paths (in-crate callers AND the
// host-application integration tests) resolve exactly as they did
// when these items lived flat in this module via include!.
pub use control_tail::engine_state_snapshot_is_transcribing_session;
pub use engine::recover_orphaned_drafts_in_store;
pub use prepared::{
    CancelSource, PreparedCancel, PreparedStart, PreparedStartError, PreparedStartResult,
    PreparedStop, PreparedStopError, RuntimeApp,
};

pub use control_tail::{
    apply_sidecar_control_tail, sidecar_file_transcribe_timeout, SidecarControlTail,
};
pub use engine::{
    build_history_store, history_storage_generation, recover_orphaned_drafts, EngineRuntime,
    SidecarHandle, DRAFT_UPSERT_MIN_INTERVAL, SIDECAR_FILE_TRANSCRIBE_TIMEOUT_MAX_SECS,
    SIDECAR_FILE_TRANSCRIBE_TIMEOUT_MIN_SECS, SIDECAR_RPC_TIMEOUT,
};
// Cloud sync (macOS only) opens and invalidates history off the runtime mutex.
pub use engine::{mark_background_history_changed, open_history_store_for_background};
pub use prepared::{PreparedEngineCancel, PreparedEngineStart, PreparedEngineStop};
pub use shared::{
    app_state_lock_free, engine_info_lock_free, prewarm_handshake_lock_free,
    register_engine_sidecar, sidecar_final_wait_expired_lock_free,
    sidecar_is_transcribing_session_lock_free, sidecar_revoke_session_lock_free, state_is_current,
    terminal_error_generation_lock_free, SharedState,
};
