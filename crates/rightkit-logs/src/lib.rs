#![deny(unsafe_code)]

#[cfg(feature = "agent_telemetry")]
pub mod agent_telemetry;
pub mod android;
mod atomic;
mod classes;
mod collect;
mod consent;
mod detached;
mod event;
pub mod identity;
mod known;
#[cfg(feature = "log-bridge")]
mod log_bridge;
mod outbox;
mod privacy_scan;
mod redact;
mod rotate;
pub mod scrub;
mod series;
#[cfg(feature = "tracing")]
mod stderr_fmt;
#[cfg(feature = "tracing")]
mod stderr_tracing;
mod store;
pub mod system;
mod textlog;
#[cfg(feature = "tracing")]
mod tracing_file;
mod upload;
mod value_writer;
mod writer;

pub use classes::{ClassRetention, ClassRule, ClassedLog};
pub use collect::{
    collect_diagnostics, prepare_diagnostic_events, BundleMetadata, CollectionPolicy,
    CollectionStats, DiagnosticsBundle, LogSource, PreparedEvents, SourceDiagnostics,
};
pub use consent::{Consent, ConsentStore};
pub use detached::{DetachedLog, DrainReport, Drainer};
pub use event::{cap_batch, EventRecord, Level, DEFAULT_BATCH_CAP, LOGS_DROPPED_EVENT};
pub use known::{KnownSecrets, KNOWN_SECRET_MARKER};
#[cfg(feature = "log-bridge")]
pub use log_bridge::init_log_bridge;
pub use outbox::{EnqueueError, Outbox, OutboxConfig, OutboxEvent, OutboxHealth, OutboxStatus};
pub use privacy_scan::{
    scan_privacy, scan_store_privacy, PrivacyFinding, PrivacyKind, PrivacyReport,
};
pub use redact::RedactionPolicy;
pub use rotate::RotatedNaming;
#[cfg(feature = "token-hash")]
pub use scrub::redact_key_tokens;
pub use scrub::{
    redact_json_pairs, redact_quoted, redact_url_text, scrub_text, scrub_text_counted,
    scrub_text_report, scrub_text_with, scrub_value, scrub_value_counted, RedactionCounts,
    ScrubOptions,
};
pub use series::{
    MetricsConfig, MetricsStore, Point, Sample, SeriesKind, Tier, LATENCY_BUCKETS_MS,
};
#[cfg(feature = "tracing")]
pub use stderr_fmt::stderr_layer;
#[cfg(feature = "tracing")]
pub use stderr_tracing::{install_stderr_tracing, install_stderr_tracing_or_noop};
pub use store::{tail_bytes, JsonlStore, JsonlStoreConfig, RetainReport, StoreStatus};
pub use textlog::{capture_stderr, RotatingTextWriter, StderrCapture, StderrLine};
#[cfg(feature = "tracing")]
pub use tracing_file::{
    collect_tracing_diagnostics, install_tracing, install_tracing_or_noop, tracing_layer,
    tracing_layer_checked, tracing_log_sources, LogWriterStatus, TracingFileConfig, TracingGuard,
};
#[cfg(feature = "http")]
pub use upload::HttpTransport;
pub use upload::{
    backoff_secs, validate_endpoint, EndpointError, FlushReport, FlushState, HttpOutcome,
    Scheduler, Telemetry, Transport, TransportError, UploadConfig, Uploader,
    PAYLOAD_SCHEMA_VERSION,
};
pub use value_writer::{LogSink, ValueWriter, ValueWriterConfig, WriterError};
pub use writer::{AsyncWriter, FailureReporting, FailureStatus, StderrReporter};

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
