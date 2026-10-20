//! The renderer-facing log commands over `rightkit-logs` (feature `logs`, Orthic-Labs/rightkit#390).
//!
//! Six apps hand-copy this trio (CodeRight `src-tauri/src/logs.rs`, CuteRight, HeardRight, ...):
//! `rightkit_logs_write`, `rightkit_logs_collect` and `rightkit_logs_clear`, the names the
//! `@rightkit/logs` JS client invokes (`RIGHTKIT_LOGS_*_COMMAND`). They are shared here once. The
//! renderer sends no path and no URL: everything that decides where logs live and what leaves them is
//! configured natively on a [`LogsRuntime`] the app registers with [`install`].
//!
//! ```ignore
//! let mut config = LogsConfig::new("MyApp", env!("CARGO_PKG_VERSION"), app_data.join("logs/events.jsonl"));
//! config.build = if cfg!(debug_assertions) { "debug" } else { "release" }.into();
//! logs_commands::install(app.handle(), config)?;           // in the app's setup
//! // invoke_handler: rightkit_tauri_shell::handler![...]   // lists the three log commands itself
//! ```
//!
//! With the `logs` feature on, [`crate::handler!`] registers the three commands; an app no longer
//! lists them (the candidate that asked apps to list them was never published).
//!
//! # What a write does
//!
//! * The batch is checked (each record an object with a valid event name and size, never the shell's
//!   own `logs_dropped` marker), redacted by key ([`RedactionPolicy`]) and scrubbed
//!   ([`rightkit_logs::scrub_text`], [`rightkit_logs::redact_url_text`]) before anything is stored.
//! * [`cap_batch`] keeps the newest `batch_cap` ([`DEFAULT_BATCH_CAP`]) records and puts one
//!   `logs_dropped` warning first when it dropped any, so the gap shows in the log.
//! * The recording gate ([`LogsRuntime::set_recording`]) is the app's diagnostics switch. While off,
//!   nothing is stored, errors included, and nothing counts as dropped.
//! * Records below `error` go through the non-blocking [`AsyncWriter`]. An `error` record is
//!   appended synchronously with [`JsonlStore::append`], so it is in the file when the command
//!   answers; the writer's queue is flushed first, bounded by [`ORDER_FLUSH_TIMEOUT`], to keep the
//!   file in arrival order.
//! * [`AsyncWriter::flush`] is a bounded acknowledgement that earlier records were attempted. It is
//!   never a durability claim, and no field of the report says "durable" for the queued records.
//!
//! Every command first checks its caller, then answers [`ShellError::Unsupported`] when no
//! [`LogsRuntime`] was registered. The check is [`crate::caller_gate::require_app_caller`] (an app
//! webview only); when the app installed host services (feature `host-services`) it is the
//! `logs` service's trust in the central `HostCallerPolicy` instead (trusted labels and origins,
//! enabled actions `write`, `collect`, `clear`), which is never weaker than the app caller gate.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rightkit_logs::{
    cap_batch, collect_diagnostics, redact_url_text, scrub_text, AsyncWriter, BundleMetadata,
    CollectionPolicy, DiagnosticsBundle, EventRecord, JsonlStore, Level, LogSource,
    RedactionPolicy, StoreStatus, DEFAULT_BATCH_CAP, LOGS_DROPPED_EVENT,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager, Webview, Wry};

use crate::error::ShellError;

/// Largest serialised record accepted from the renderer, in bytes.
pub const MAX_EVENT_BYTES: usize = 16 * 1024;
/// Longest event name accepted from the renderer, in bytes.
pub const MAX_EVENT_NAME_BYTES: usize = 128;
/// How long a write waits for the queue to drain before an `error` record is appended, to keep
/// arrival order. The wait is an acknowledgement bound, not a durability claim.
pub const ORDER_FLUSH_TIMEOUT: Duration = Duration::from_millis(250);

/// Key fragments the six hand-copied implementations all add on top of the default policy: a
/// collected bundle never carries a path, root, URL, query, free text or command output field.
pub const SUITE_KEY_FRAGMENTS: [&str; 6] = ["path", "root", "url", "query", "text", "output"];

/// Native configuration of a [`LogsRuntime`].
#[derive(Debug, Clone)]
pub struct LogsConfig {
    pub app_name: String,
    pub app_version: String,
    /// `"debug"` or `"release"`, as the bundle metadata states it.
    pub build: String,
    /// The renderer's JSONL log file.
    pub log_path: PathBuf,
    /// Size at which the log rotates; `0` disables rotation.
    pub max_file_bytes: u64,
    /// Bounded queue of the non-blocking writer.
    pub queue_capacity: usize,
    /// Most records one write keeps (see [`cap_batch`]).
    pub batch_cap: usize,
    /// Create the log owner-only (see [`JsonlStore::with_owner_only`]). Default off.
    pub owner_only: bool,
    /// Redaction applied to every written and collected record.
    pub redaction: RedactionPolicy,
    /// Bounds of a collected bundle. Its `redaction` is replaced by [`LogsConfig::redaction`].
    pub collection: CollectionPolicy,
    /// Other logs the app owns, collected next to the renderer log (for example the engine log).
    pub extra_sources: Vec<LogSource>,
}

impl LogsConfig {
    /// The configuration the six apps share: a 4 MiB log, a 1024-record queue,
    /// [`DEFAULT_BATCH_CAP`], the default collection bounds and [`SUITE_KEY_FRAGMENTS`] redacted.
    pub fn new(
        app_name: impl Into<String>,
        app_version: impl Into<String>,
        log_path: impl Into<PathBuf>,
    ) -> Self {
        let mut redaction = RedactionPolicy::default();
        for fragment in SUITE_KEY_FRAGMENTS {
            redaction
                .additional_key_fragments
                .insert(fragment.to_owned());
        }
        Self {
            app_name: app_name.into(),
            app_version: app_version.into(),
            build: "release".into(),
            log_path: log_path.into(),
            max_file_bytes: 4 * 1024 * 1024,
            queue_capacity: 1024,
            batch_cap: DEFAULT_BATCH_CAP,
            owner_only: false,
            redaction,
            collection: CollectionPolicy::default(),
            extra_sources: Vec::new(),
        }
    }
}

/// What one write did with its batch. Counts only; nothing here claims the queued records are on
/// disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteReport {
    /// Records the renderer sent.
    pub received: usize,
    /// Dropped by the recording gate (nothing is stored while it is off).
    pub muted: usize,
    /// Not an object, a bad event name or level, the reserved `logs_dropped` name.
    pub invalid: usize,
    /// Serialised larger than [`MAX_EVENT_BYTES`].
    pub oversize: usize,
    /// Dropped by [`cap_batch`] (the batch also starts with one `logs_dropped` record).
    pub capped: usize,
    /// Queued on the non-blocking writer.
    pub queued: usize,
    /// Refused by a full writer queue.
    pub queue_dropped: usize,
    /// `error` records appended synchronously.
    pub errors_appended: usize,
    /// `error` records whose synchronous append failed.
    pub errors_failed: usize,
}

/// The renderer's collect options. Only bounded numeric knobs exist: no path, URL or policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectOptions {
    /// Fewer events than the native policy allows; never more.
    pub max_events: Option<usize>,
}

/// The configured logging runtime: store, writer, recording gate and collect policy. Plain Rust, no
/// Tauri types, so it is tested without a webview.
pub struct LogsRuntime {
    store: Arc<JsonlStore>,
    writer: AsyncWriter,
    sources: Vec<LogSource>,
    metadata: BundleMetadata,
    policy: CollectionPolicy,
    redaction: RedactionPolicy,
    batch_cap: usize,
}

/// Replace every string leaf with its scrubbed form: free-text identifiers first
/// ([`scrub_text`]), then URLs and secret parameters ([`redact_url_text`]).
pub fn sanitize_strings(value: &mut Value) {
    match value {
        Value::String(text) => {
            let clean = redact_url_text(&scrub_text(text));
            if clean != *text {
                *text = clean;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(sanitize_strings),
        Value::Object(map) => map.values_mut().for_each(sanitize_strings),
        _ => {}
    }
}

/// True for a usable event name: non-empty, bounded, no control characters, and not the marker name
/// only [`cap_batch`] may write.
pub fn valid_event_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_EVENT_NAME_BYTES
        && !name.chars().any(char::is_control)
        && name != LOGS_DROPPED_EVENT
}

/// A batch after checking, before the cap: the usable records and the counts of what was refused.
#[derive(Debug, Default)]
pub struct PreparedBatch {
    pub records: Vec<EventRecord>,
    pub invalid: usize,
    pub oversize: usize,
}

/// Check, redact and scrub a renderer batch (see the module documentation).
pub fn prepare_batch(events: Vec<Value>, redaction: &RedactionPolicy) -> PreparedBatch {
    let mut out = PreparedBatch::default();
    for mut event in events {
        if !event.is_object() {
            out.invalid += 1;
            continue;
        }
        let size = serde_json::to_vec(&event).map_or(usize::MAX, |bytes| bytes.len());
        if size > MAX_EVENT_BYTES {
            out.oversize += 1;
            continue;
        }
        redaction.redact_value(&mut event);
        let Ok(mut record) = serde_json::from_value::<EventRecord>(event) else {
            out.invalid += 1;
            continue;
        };
        if !valid_event_name(&record.event) {
            out.invalid += 1;
            continue;
        }
        for field in record.fields.values_mut() {
            sanitize_strings(field);
        }
        out.records.push(record);
    }
    out
}

impl LogsRuntime {
    /// Build the store and spawn the writer. The log file is created by the first append.
    pub fn new(config: LogsConfig) -> io::Result<Self> {
        let mut store = JsonlStore::new(config.log_path.clone(), config.max_file_bytes);
        if config.owner_only {
            store = store.with_owner_only(true);
        }
        let store = Arc::new(store);
        let writer = AsyncWriter::spawn(Arc::clone(&store), config.queue_capacity)?;
        let mut sources = vec![LogSource::new("renderer", config.log_path)];
        sources.extend(config.extra_sources);
        let mut policy = config.collection;
        policy.redaction = config.redaction.clone();
        Ok(Self {
            store,
            writer,
            sources,
            metadata: BundleMetadata {
                app_name: config.app_name,
                app_version: config.app_version,
                build: config.build,
                os: std::env::consts::OS.to_owned(),
                settings: Value::Object(Default::default()),
            },
            policy,
            redaction: config.redaction,
            batch_cap: config.batch_cap.max(1),
        })
    }

    /// The app's diagnostics switch. Off: writes store nothing (errors included) and count nothing as
    /// dropped. Records already queued are still written.
    pub fn set_recording(&self, enabled: bool) {
        self.writer.set_recording(enabled);
    }

    pub fn recording(&self) -> bool {
        self.writer.recording()
    }

    /// The store, for the app's own synchronous records.
    pub fn store(&self) -> &Arc<JsonlStore> {
        &self.store
    }

    /// The non-blocking writer, for the app's own records.
    pub fn writer(&self) -> &AsyncWriter {
        &self.writer
    }

    /// Failures the store absorbed (see [`StoreStatus`]).
    pub fn store_status(&self) -> StoreStatus {
        self.store.status()
    }

    /// Wait up to `timeout` for the writer to attempt everything queued before this call. A bounded
    /// acknowledgement only: `true` is not a claim that the records are on stable storage.
    pub fn flush(&self, timeout: Duration) -> bool {
        self.writer.flush(timeout)
    }

    /// Store a renderer batch (see the module documentation). Blocking: an `error` record is
    /// appended on the calling thread.
    pub fn write(&self, events: Vec<Value>) -> WriteReport {
        let mut report = WriteReport {
            received: events.len(),
            ..WriteReport::default()
        };
        if !self.writer.recording() {
            report.muted = report.received;
            return report;
        }
        let prepared = prepare_batch(events, &self.redaction);
        report.invalid = prepared.invalid;
        report.oversize = prepared.oversize;
        let before = prepared.records.len();
        let batch = cap_batch(prepared.records, self.batch_cap);
        // `cap_batch` counts its marker toward the cap; it exists only when it dropped records.
        report.capped = if before > self.batch_cap.max(1) {
            before - (batch.len() - 1)
        } else {
            0
        };
        let mut unflushed = false;
        for record in batch {
            if record.level == Level::Error {
                if unflushed {
                    // Arrival order across the two paths. Bounded; a miss only reorders.
                    self.writer.flush(ORDER_FLUSH_TIMEOUT);
                    unflushed = false;
                }
                match self.store.append(&record) {
                    Ok(()) => report.errors_appended += 1,
                    Err(_) => report.errors_failed += 1,
                }
            } else if self.writer.append(record) {
                report.queued += 1;
                unflushed = true;
            } else {
                report.queue_dropped += 1;
            }
        }
        report
    }

    /// The support bundle: the renderer log and the extra sources, bounded by the collect policy,
    /// key-redacted and scrubbed ([`sanitize_strings`]) a second time over the collected text.
    pub fn collect(&self, options: Option<CollectOptions>) -> DiagnosticsBundle {
        let mut policy = self.policy.clone();
        if let Some(max) = options.and_then(|o| o.max_events) {
            policy.max_events = max.clamp(1, self.policy.max_events.max(1));
        }
        let mut bundle = collect_diagnostics(&self.sources, self.metadata.clone(), &policy);
        sanitize_strings(&mut bundle.metadata.settings);
        for source in &mut bundle.sources {
            source.events.iter_mut().for_each(sanitize_strings);
        }
        bundle
    }

    /// Remove the renderer log and its rotated predecessors.
    pub fn clear(&self) -> io::Result<()> {
        self.store.clear()
    }
}

/// Create the runtime and register it as managed state for the commands.
pub fn install(app: &AppHandle<Wry>, config: LogsConfig) -> io::Result<()> {
    let _ = app.manage(LogsRuntime::new(config)?);
    Ok(())
}

/// A command whose state was never registered is `unsupported`, not a silent success.
pub fn require_registered(registered: bool) -> Result<(), ShellError> {
    if registered {
        Ok(())
    } else {
        Err(ShellError::Unsupported(
            "diagnostic logging is not configured".into(),
        ))
    }
}

/// The caller check of every log command: the central host policy when host services are
/// installed, the shell-wide app caller gate otherwise.
fn authorize(webview: &Webview<Wry>, action: &str) -> Result<(), ShellError> {
    #[cfg(feature = "host-services")]
    crate::host::authorize_or_app_gate(webview, crate::host::ServiceId::LOGS, action)?;
    #[cfg(not(feature = "host-services"))]
    {
        let _ = action;
        crate::caller_gate::require_app_caller(webview)?;
    }
    Ok(())
}

fn runtime_of(app: &AppHandle<Wry>) -> Result<(), ShellError> {
    require_registered(app.try_state::<LogsRuntime>().is_some())
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ShellError> + Send + 'static,
) -> Result<T, ShellError> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| ShellError::Other(e.to_string()))?
}

/// Store a batch of renderer records (`{ events }`). Answers the [`WriteReport`].
#[tauri::command]
pub async fn rightkit_logs_write(
    webview: Webview<Wry>,
    app: AppHandle<Wry>,
    events: Vec<Value>,
) -> Result<WriteReport, ShellError> {
    authorize(&webview, "write")?;
    runtime_of(&app)?;
    blocking(move || Ok(app.state::<LogsRuntime>().write(events))).await
}

/// Collect the support bundle (`{ opts }`, optional).
#[tauri::command]
pub async fn rightkit_logs_collect(
    webview: Webview<Wry>,
    app: AppHandle<Wry>,
    opts: Option<CollectOptions>,
) -> Result<DiagnosticsBundle, ShellError> {
    authorize(&webview, "collect")?;
    runtime_of(&app)?;
    blocking(move || Ok(app.state::<LogsRuntime>().collect(opts))).await
}

/// Remove the renderer log.
#[tauri::command]
pub async fn rightkit_logs_clear(
    webview: Webview<Wry>,
    app: AppHandle<Wry>,
) -> Result<(), ShellError> {
    authorize(&webview, "clear")?;
    runtime_of(&app)?;
    blocking(move || app.state::<LogsRuntime>().clear().map_err(ShellError::from)).await
}
