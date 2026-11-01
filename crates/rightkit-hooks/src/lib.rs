//! Shared hook runtime for Claude Code and Codex: the native pair of `@rightkit/hooks`.
//!
//! - [`event`]: typed payload parsing ([`HookEvent`], [`Wire`], [`EventKind`]);
//! - [`output`]: exact per-event output shapes ([`Reply`]);
//! - [`dispatch`](mod@dispatch): modules under per-module and total deadlines, exit 0 on any failure;
//! - [`receipt`]: `emitted` and `observed` delivery receipts and the transcript-scan module;
//! - [`membrane`]: Membrane's hook wire types (module status, dispatch result, host response).
//!
//! The crate holds no policy: a module decides, this crate only parses, bounds and prints.
//! The optional `guard` feature adds `guard`, a native policy guard that decides
//! through the evidence application; its texts and names come from the host
//! through `guard::set_guard_host`.

#[cfg(feature = "agent_hooks")]
pub mod agent_hooks;
pub mod dispatch;
pub mod event;
#[cfg(feature = "guard")]
pub mod guard;
pub mod membrane;
pub mod output;
pub mod receipt;

pub use dispatch::{
    compose_reply, dispatch, run, run_with, Decision, DispatchOptions, DispatchResult, FnModule,
    HookModule, ModuleContext, ModuleOutput, ModuleResult, Status,
};
pub use event::{EventKind, HookEvent, ParseError, Wire, STDIN_CAP};
pub use output::{OutputError, Reply};
pub use receipt::{
    marker, new_receipt_id, scan_transcript_tail, tag_context, DeliveryLevel, ObserveModule,
    Receipt, ReceiptStore,
};
