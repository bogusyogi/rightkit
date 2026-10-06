#![deny(unsafe_code)]

mod atomic;
mod collect;
mod consent;
mod event;
pub mod identity;
mod outbox;
mod redact;
pub mod scrub;
mod store;
pub mod system;
#[cfg(feature = "testing")]
pub mod testing;
mod upload;
mod writer;

pub use collect::{
    collect_diagnostics, prepare_diagnostic_events, BundleMetadata, CollectionPolicy,
    CollectionStats, DiagnosticsBundle, LogSource, PreparedEvents, SourceDiagnostics,
};
pub use consent::{Consent, ConsentStore};
pub use event::{EventRecord, Level};
pub use outbox::{EnqueueError, Outbox, OutboxConfig, OutboxEvent, OutboxStatus};
pub use redact::RedactionPolicy;
pub use scrub::{redact_quoted, scrub_text, scrub_value};
pub use store::JsonlStore;
#[cfg(feature = "http")]
pub use upload::HttpTransport;
pub use upload::{
    backoff_secs, validate_endpoint, EndpointError, FlushReport, FlushState, HttpOutcome,
    Scheduler, Telemetry, Transport, TransportError, UploadConfig, Uploader,
    PAYLOAD_SCHEMA_VERSION,
};
pub use writer::AsyncWriter;

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
