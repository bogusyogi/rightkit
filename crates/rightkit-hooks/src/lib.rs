//! Shared hook runtime for Claude Code and Codex: the native pair of `@rightkit/hooks`.
//!
//! - [`event`]: typed payload parsing ([`HookEvent`], [`Wire`], [`EventKind`]);
//! - [`output`]: exact per-event output shapes ([`Reply`]);
//! - [`dispatch`]: modules under per-module and total deadlines, exit 0 on any failure;
//! - [`receipt`]: `emitted` and `observed` delivery receipts and the transcript-scan module.
//!
//! The crate holds no policy: a module decides, this crate only parses, bounds and prints.

pub mod dispatch;
pub mod event;
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
