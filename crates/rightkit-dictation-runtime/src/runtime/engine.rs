#![allow(unused_imports)]
#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{delivery_history::*, engine_impl::*, engine_support::*, finalize_transcript::*};

// Engine runtime — owns the dictation state machine and the recent
// transcript ring buffer.
//
// The IPC layer (`ipc.rs`) drives this runtime from the shell. The runtime
// is transport-agnostic so the same code can be used by:
//  - this sidecar binary (driven over stdio JSON-RPC)
//  - the Tauri shell (when `RIGHTKIT_NEXT_ENGINE=sidecar`, a separate
//    supervisor in the shell opens stdio to the sidecar and uses the
//    same `EngineRuntime` shape via the contract)
//
// Audio capture and ASR live in `asr.rs`; this runtime owns the sidecar state
// machine, focus-target tracking, delivery precedence, command dispatch, and
// recent transcript ring.

pub(crate) use std::collections::VecDeque;
pub(crate) use std::path::PathBuf;
pub(crate) use std::sync::mpsc::{channel, Receiver};
pub(crate) use std::sync::Arc;
pub(crate) use std::time::Instant;

pub(crate) use parking_lot::Mutex;
pub(crate) use serde::{Deserialize, Serialize};

pub(crate) use crate::asr::AsrEp;
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) use crate::delivery::copy_fallback_record;
pub(crate) use crate::delivery::{
    copy_text, deliver_text, snapshot_target, DeliveryOutcome, DeliveryRecord, TargetSnapshot,
};
pub(crate) use crate::focus::FocusTracker;
pub(crate) use crate::owner_diagnostics;
pub(crate) use crate::worker::{spawn_worker, WorkerCmd, WorkerEvent, WorkerHandle};
pub(crate) use rightkit_dictation_core::engine::FileTranscript;
pub(crate) use rightkit_transcript_library::delivery::CopyFallbackReason;

pub(crate) const RECENT_HISTORY_CAP: usize = 100;
pub(crate) const TERMINAL_TOMBSTONE_CAP: usize = 64;

pub(crate) fn engine_test_mode() -> bool {
    // `#[cfg]`, not `cfg!()`: a release binary must not contain the switch name.
    #[cfg(any(test, feature = "test-mode"))]
    {
        std::env::var("RIGHTKIT_ENGINE_TEST_MODE")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }
    #[cfg(not(any(test, feature = "test-mode")))]
    {
        false
    }
}

/// Engine info surfaced over the IPC contract as the `EngineInfo` payload.
#[derive(Debug, Clone)]
pub struct EngineInfo {
    pub mode: String,
    pub cold_load_s: Option<f64>,
    pub engine_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineState {
    Idle,
    Recording { session_id: String },
    Transcribing { session_id: String },
    Error { session_id: String, message: String },
}

pub struct EngineRuntime {
    pub(crate) state: EngineState,
    pub(crate) last_delivery: Option<DeliveryRecord>,
    pub(crate) recent: VecDeque<DeliveryRecord>,
    pub(crate) focus: Arc<Mutex<FocusTracker>>,
    pub(crate) sequence: u64,
    pub(crate) terminal_tombstones: VecDeque<String>,
    pub(crate) models_base: PathBuf,
    pub(crate) worker: Option<WorkerHandle>,
    pub(crate) pending_send_enter: bool,
    /// Mirrors `pending_send_enter`: set by `begin_stop` only when the stop
    /// is a `StopKind::CancelToHistory` finalize (engine-side name for the
    /// shell's cancel-to-history flow). Consumed (taken) in
    /// `finalize_phase1_capture` and threaded through to phase 2, which
    /// skips the cloud L1/L2/L3 polish lanes entirely for this stop — the
    /// finalize still runs ASR and local (L0) polish to completion exactly
    /// like a normal Stop.
    pub(crate) pending_local_only: bool,
    pub(crate) stop_started_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub enum FinalizeOutcome {
    Delivery {
        record: DeliveryRecord,
        send_enter: bool,
    },
    Transcript {
        text: String,
        send_enter: bool,
        raw_text: Option<String>,
        recording_ms: Option<u64>,
        /// True for a cancel-to-history finalize (`local_only_stop`): the
        /// shell stores the text to history and must not deliver it.
        cancelled: bool,
        context_request_attempted: Option<bool>,
        context_original_binding: Option<bool>,
    },
    ResetToArmed,
    CommandDispatched {
        action: String,
        detail: String,
    },
    /// A recognized standalone command failed to dispatch (missing macOS
    /// Accessibility grant, no platform equivalent, unknown key). The engine
    /// still recovers to armed; the message lets the shell surface the failure
    /// instead of it looking identical to success.
    CommandFailed {
        message: String,
    },
    NoOp,
}
