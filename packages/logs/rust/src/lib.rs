#![forbid(unsafe_code)]

mod collect;
mod event;
mod redact;
mod store;

pub use collect::{
    collect_diagnostics, prepare_diagnostic_events, BundleMetadata, CollectionPolicy,
    CollectionStats, DiagnosticsBundle, LogSource, PreparedEvents, SourceDiagnostics,
};
pub use event::{EventRecord, Level};
pub use redact::RedactionPolicy;
pub use store::JsonlStore;
