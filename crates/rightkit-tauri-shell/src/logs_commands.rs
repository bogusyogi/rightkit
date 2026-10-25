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
//!
//! # What a clear does
//!
//! [`LogsRuntime::clear`] clears the renderer log (reported as `renderer`) and then every
//! [`ClearSource`] in [`LogsConfig::clear_sources`], in order, and never stops at a failure. It
//! answers a [`ClearReport`]: the sources it cleared and, for each one that failed, the source name
//! and an error text scrubbed with [`scrub_text`] and [`redact_url_text`] (user names and URL
//! secrets do not leave). `rightkit_logs_clear` answers that report when every source cleared and
//! rejects with the failed sources named when any did not, so the `@rightkit/logs` client, which
//! awaits the invoke, never mistakes a partial clear for a full one.
//!
//! * [`ClearSource::jsonl`] removes one JSONL log and its rotated predecessor (the engine log, say).
//! * A `tracing` family is a closure over the app's own guard (this crate does not enable the
//!   `tracing` feature of `rightkit-logs`):
//!
//! ```ignore
//! let guard = Arc::new(rightkit_logs::install_tracing(TracingFileConfig::new(dir, "shell"))?);
//! let shell = Arc::clone(&guard);
//! config.clear_sources.push(ClearSource::new("shell", move || shell.clear()));
//! config.clear_sources.push(ClearSource::jsonl("engine", app_data.join("logs/engine.jsonl")));
//! ```
//!
//! # The settings snapshot
//!
//! [`LogsConfig::settings`] is a closure the app registers to describe its non-secret settings.
//! Each [`LogsRuntime::collect`] calls it and puts the value in `metadata.settings` of the bundle,
//! key-redacted with [`LogsConfig::redaction`] and scrubbed like every other collected string. A
//! closure that panics yields `{"error":"settings snapshot unavailable"}` instead of failing the
//! collect. With no closure the field stays `{}`.

use std::fmt;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
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
    /// What [`LogsRuntime::clear`] clears after the renderer log, in order (see [`ClearSource`]).
    /// Add every extra source and every `tracing` family the app collects or writes.
    pub clear_sources: Vec<ClearSource>,
    /// The app's non-secret settings for the bundle's `metadata.settings` (see
    /// [`SettingsSnapshot`]). `None` leaves the field `{}`.
    pub settings: Option<SettingsSnapshot>,
}

/// One log that [`LogsRuntime::clear`] clears besides the renderer log: a name for the report and
/// what clears it. The renderer cannot name or add one; the app configures them natively.
#[derive(Clone)]
pub struct ClearSource {
    name: String,
    clear: Arc<dyn Fn() -> io::Result<()> + Send + Sync>,
}

impl ClearSource {
    /// A source cleared by `clear`, for example a `tracing` family:
    /// `ClearSource::new("shell", move || guard.clear())` over the app's
    /// `Arc<rightkit_logs::TracingGuard>`.
    pub fn new(
        name: impl Into<String>,
        clear: impl Fn() -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            clear: Arc::new(clear),
        }
    }

    /// A JSONL log at `path`: removes that file and its rotated predecessor through
    /// [`JsonlStore::clear`]. A log that is already absent clears successfully.
    pub fn jsonl(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        let store = JsonlStore::new(path, 0);
        Self::new(name, move || store.clear())
    }

    /// The name the [`ClearReport`] uses.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Debug for ClearSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClearSource")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Describes the app's non-secret settings for a collected bundle. Called on every collect, so it
/// reports the settings as they are then; its value is redacted and scrubbed before it is stored.
#[derive(Clone)]
pub struct SettingsSnapshot(Arc<dyn Fn() -> Value + Send + Sync>);

impl SettingsSnapshot {
    pub fn new(snapshot: impl Fn() -> Value + Send + Sync + 'static) -> Self {
        Self(Arc::new(snapshot))
    }
}

impl fmt::Debug for SettingsSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SettingsSnapshot").finish_non_exhaustive()
    }
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
            clear_sources: Vec::new(),
            settings: None,
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

/// What one clear did, source by source. Names only: a failure carries its error text scrubbed of
/// user names and URL secrets, and nothing here says what the source held.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearReport {
    /// Sources cleared, in the order they ran (`renderer` first).
    pub cleared: Vec<String>,
    /// Sources that failed. Every source is attempted, so a failure never hides a later one.
    pub failed: Vec<ClearFailure>,
}

/// One source [`LogsRuntime::clear`] could not clear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearFailure {
    pub source: String,
    /// The error, passed through [`scrub_text`] and [`redact_url_text`].
    pub error: String,
}

impl ClearReport {
    /// True when no source failed.
    pub fn is_complete(&self) -> bool {
        self.failed.is_empty()
    }

    /// The command's answer: the report when complete, otherwise an error naming every failed
    /// source (`clear failed for: <source> (<error>), ...`), so a partial clear is never a success.
    pub fn into_result(self) -> Result<Self, ShellError> {
        if self.is_complete() {
            return Ok(self);
        }
        let failed: Vec<String> = self
            .failed
            .iter()
            .map(|f| format!("{} ({})", f.source, f.error))
            .collect();
        Err(ShellError::Other(format!(
            "clear failed for: {}",
            failed.join(", ")
        )))
    }
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
    clear_sources: Vec<ClearSource>,
    settings: Option<SettingsSnapshot>,
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
            clear_sources: config.clear_sources,
            settings: config.settings,
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
    /// key-redacted and scrubbed ([`sanitize_strings`]) a second time over the collected text. The
    /// settings snapshot, when configured, is taken now (see the module documentation).
    pub fn collect(&self, options: Option<CollectOptions>) -> DiagnosticsBundle {
        let mut policy = self.policy.clone();
        if let Some(max) = options.and_then(|o| o.max_events) {
            policy.max_events = max.clamp(1, self.policy.max_events.max(1));
        }
        let mut metadata = self.metadata.clone();
        if let Some(settings) = &self.settings {
            metadata.settings = self.settings_value(settings);
        }
        let mut bundle = collect_diagnostics(&self.sources, metadata, &policy);
        sanitize_strings(&mut bundle.metadata.settings);
        for source in &mut bundle.sources {
            source.events.iter_mut().for_each(sanitize_strings);
        }
        bundle
    }

    /// The settings snapshot, key-redacted then scrubbed. A snapshot that panics is replaced by an
    /// error object: the support bundle is worth more without settings than not at all.
    fn settings_value(&self, settings: &SettingsSnapshot) -> Value {
        let mut value = catch_unwind(AssertUnwindSafe(|| (settings.0)()))
            .unwrap_or_else(|_| serde_json::json!({"error": "settings snapshot unavailable"}));
        self.redaction.redact_value(&mut value);
        sanitize_strings(&mut value);
        value
    }

    /// Clear the renderer log (`renderer`) and then every [`LogsConfig::clear_sources`] entry, in
    /// order. A source that fails, or panics, is reported and the rest still run.
    pub fn clear(&self) -> ClearReport {
        let mut report = ClearReport::default();
        attempt_clear(&mut report, "renderer", || self.store.clear());
        for source in &self.clear_sources {
            attempt_clear(&mut report, &source.name, || (source.clear)());
        }
        report
    }
}

/// Run one clear and record its outcome in `report`.
fn attempt_clear(report: &mut ClearReport, name: &str, clear: impl FnOnce() -> io::Result<()>) {
    match catch_unwind(AssertUnwindSafe(clear)) {
        Ok(Ok(())) => report.cleared.push(name.to_owned()),
        Ok(Err(error)) => report.failed.push(ClearFailure {
            source: name.to_owned(),
            error: redact_url_text(&scrub_text(&error.to_string())),
        }),
        Err(_) => report.failed.push(ClearFailure {
            source: name.to_owned(),
            error: "clear panicked".to_owned(),
        }),
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

/// Clear the renderer log and every configured clear source. Answers the [`ClearReport`] when all
/// cleared; rejects naming each failed source after attempting all of them, otherwise.
#[tauri::command]
pub async fn rightkit_logs_clear(
    webview: Webview<Wry>,
    app: AppHandle<Wry>,
) -> Result<ClearReport, ShellError> {
    authorize(&webview, "clear")?;
    runtime_of(&app)?;
    blocking(move || app.state::<LogsRuntime>().clear().into_result()).await
}
