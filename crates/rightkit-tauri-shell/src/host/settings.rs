//! The settings bridge: `describe`, `get`, `patch(expectedRevision)`, `reset(keys)` and
//! `subscribe` over an `rightkit-app-settings` [`Document`].
//!
//! [`SettingsBridge`] is plain Rust (no Tauri types), so every rule below is tested without a
//! webview. [`SettingsHostService`] and the `rightkit_settings_*` commands are the thin adapter.
//!
//! # What a mutation does
//!
//! 1. The patch is checked: a JSON object, every key allowed by the app's [`SettingsSchema`], no
//!    value over [`SettingsSchema::max_value_bytes`].
//! 2. The CURRENT committed document is read and the COMPLETE candidate (current values with the
//!    patch prepared by [`SettingsSchema::prepare_patch`]) is checked: each canonical value
//!    within its limit, no larger than
//!    [`SettingsSchema::max_document_bytes`], then [`SettingsSchema::validate`].
//! 3. The candidate is committed with a compare-and-swap on the revision it was built from, so what
//!    was validated is exactly what is stored. A caller's `expectedRevision` that differs from the
//!    committed revision is a typed `revision_conflict` before anything is validated or written;
//!    without one, the bridge retries a few times when another writer got in first.
//! 4. Only after the commit are subscribers told, with the top-level keys that changed since the
//!    last state the bridge saw (so an external edit noticed on the way is reported too). A
//!    subscriber that panics neither fails the call nor stops the others: the data is stored.
//!
//! A newer-schema document is refused for reads and writes (the frontend never overwrites it), a
//! read-only bridge or backend refuses every mutation with `read_only`, and a write that reached
//! the disk but failed a later step (`Error::Replaced`) is reported as committed with a `warning`,
//! never as a failure to retry.
//!
//! # Who chooses the file
//!
//! The renderer supplies a patch and a revision, never a namespace, path or document: those are
//! fixed when the app builds the [`Document`].

use std::collections::BTreeSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rightkit_app_settings::{Document, DocumentState, Error as StoreError};
use serde::Serialize;
use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter, Manager, Webview, Wry};

use super::policy::{ServiceId, ServiceTrust};
use super::{blocking, HostError, HostServices};
use crate::caller_gate;

/// The event a settings change reaches subscribed windows on: `{ keys, revision }`, never values.
pub const EVENT_SETTINGS_CHANGED: &str = "rightkit://settings-changed";
/// The default largest serialized value of one top-level key.
pub const DEFAULT_MAX_VALUE_BYTES: usize = 64 * 1024;
/// The default largest serialized document.
pub const DEFAULT_MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
/// The longest top-level key accepted from a renderer.
pub const MAX_KEY_BYTES: usize = 128;
/// How often a patch with no `expectedRevision` retries after losing a race to another writer.
const UNCONDITIONAL_ATTEMPTS: usize = 3;

/// The app's declaration of its settings: schema version, UI description and limits, and the
/// checks the bridge runs before it commits.
pub trait SettingsSchema: Send + Sync {
    /// The version the app writes the document under. A stored document with a higher version is
    /// refused, never overwritten.
    fn schema_version(&self) -> u32;

    /// What `describe` answers for the UI (sections, labels, defaults): opaque to the bridge.
    fn descriptor(&self) -> Value {
        Value::Null
    }

    /// Largest serialized value of one top-level key a patch may set.
    fn max_value_bytes(&self) -> usize {
        DEFAULT_MAX_VALUE_BYTES
    }

    /// Largest serialized document a commit may produce.
    fn max_document_bytes(&self) -> usize {
        DEFAULT_MAX_DOCUMENT_BYTES
    }

    /// Whether the renderer may set or reset this top-level key. Default: every key.
    fn allows_key(&self, _key: &str) -> bool {
        true
    }

    /// Prepare the complete canonical candidate from the exact committed base and renderer patch.
    /// The default replaces top-level values, removes nulls, and replaces nested objects whole.
    /// Called after the revision check and again from a fresh base after a CAS conflict.
    /// This hook must be pure: perform no storage writes or other effects.
    /// Return a renderer-safe error to refuse the mutation before storage is changed.
    fn prepare_patch(
        &self,
        current: &Map<String, Value>,
        patch: &Map<String, Value>,
    ) -> Result<Map<String, Value>, String> {
        Ok(merged(current, patch))
    }

    /// Check the COMPLETE candidate document before it is committed. The message goes to the
    /// renderer, so it must name no secret.
    fn validate(&self, _candidate: &Map<String, Value>) -> Result<(), String> {
        Ok(())
    }
}

/// A [`SettingsSchema`] assembled from values, for apps whose checks are a key list and a closure.
#[derive(Clone)]
pub struct StaticSchema {
    version: u32,
    descriptor: Value,
    keys: Option<BTreeSet<String>>,
    max_value_bytes: usize,
    max_document_bytes: usize,
    validator: Option<Arc<dyn Fn(&Map<String, Value>) -> Result<(), String> + Send + Sync>>,
}

impl StaticSchema {
    pub fn new(version: u32) -> Self {
        Self {
            version,
            descriptor: Value::Null,
            keys: None,
            max_value_bytes: DEFAULT_MAX_VALUE_BYTES,
            max_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
            validator: None,
        }
    }

    pub fn descriptor(mut self, descriptor: Value) -> Self {
        self.descriptor = descriptor;
        self
    }

    /// Only these top-level keys may be set or reset by the renderer.
    pub fn keys<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.keys = Some(keys.into_iter().map(Into::into).collect());
        self
    }

    pub fn max_value_bytes(mut self, limit: usize) -> Self {
        self.max_value_bytes = limit;
        self
    }

    pub fn max_document_bytes(mut self, limit: usize) -> Self {
        self.max_document_bytes = limit;
        self
    }

    /// Check the complete candidate (see [`SettingsSchema::validate`]).
    pub fn validate_with(
        mut self,
        validator: impl Fn(&Map<String, Value>) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.validator = Some(Arc::new(validator));
        self
    }
}

impl SettingsSchema for StaticSchema {
    fn schema_version(&self) -> u32 {
        self.version
    }

    fn descriptor(&self) -> Value {
        self.descriptor.clone()
    }

    fn max_value_bytes(&self) -> usize {
        self.max_value_bytes
    }

    fn max_document_bytes(&self) -> usize {
        self.max_document_bytes
    }

    fn allows_key(&self, key: &str) -> bool {
        self.keys.as_ref().is_none_or(|keys| keys.contains(key))
    }

    fn validate(&self, candidate: &Map<String, Value>) -> Result<(), String> {
        match &self.validator {
            Some(check) => check(candidate),
            None => Ok(()),
        }
    }
}

/// Why a settings call failed. Each variant has a stable [`SettingsError::code`].
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsError {
    /// `expectedRevision` is not the committed revision.
    Conflict { expected: u64, actual: u64 },
    /// The request is malformed: not an object, a key the schema does not allow, nothing to reset.
    Invalid(String),
    /// The complete candidate failed the app's validation. The message is the app's.
    Rejected(String),
    /// One value is larger than the schema allows.
    ValueTooLarge {
        key: String,
        len: usize,
        limit: usize,
    },
    /// The document is, or would become, larger than the schema (or the backend) allows.
    DocumentTooLarge {
        len: Option<u64>,
        limit: Option<u64>,
    },
    /// The stored document was written by a newer build.
    NewerSchema { found: u32, supported: u32 },
    /// The bridge or its backend is read-only.
    ReadOnly(&'static str),
    /// The write-security profile the store asked for could not be established.
    Protection,
    /// Any other storage failure. The text is fixed: no path or OS message reaches the renderer.
    Storage(&'static str),
}

impl SettingsError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Conflict { .. } => "revision_conflict",
            Self::Invalid(_) => "invalid",
            Self::Rejected(_) => "validation_failed",
            Self::ValueTooLarge { .. } => "value_too_large",
            Self::DocumentTooLarge { .. } => "document_too_large",
            Self::NewerSchema { .. } => "newer_schema",
            Self::ReadOnly(_) => "read_only",
            Self::Protection => "protection_unavailable",
            Self::Storage(_) => "storage",
        }
    }
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { expected, actual } => write!(
                f,
                "settings changed since revision {expected}; now at revision {actual}"
            ),
            Self::Invalid(m) | Self::Rejected(m) => f.write_str(m),
            Self::ValueTooLarge { key, len, limit } => {
                write!(f, "the value of {key:?} is {len} bytes, over the {limit} byte limit")
            }
            Self::DocumentTooLarge { .. } => f.write_str("the settings document is too large"),
            Self::NewerSchema { found, supported } => write!(
                f,
                "settings were written by a newer version (schema {found}, this build supports {supported})"
            ),
            Self::ReadOnly(m) | Self::Storage(m) => f.write_str(m),
            Self::Protection => {
                f.write_str("the requested protection for the settings file could not be established")
            }
        }
    }
}

impl std::error::Error for SettingsError {}

impl From<SettingsError> for HostError {
    fn from(e: SettingsError) -> Self {
        let details = match &e {
            SettingsError::Conflict { expected, actual } => {
                Some(serde_json::json!({"expected": expected, "actual": actual}))
            }
            SettingsError::ValueTooLarge { key, len, limit } => {
                Some(serde_json::json!({"key": key, "len": len, "limit": limit}))
            }
            SettingsError::DocumentTooLarge { len, limit } => {
                Some(serde_json::json!({"len": len, "limit": limit}))
            }
            SettingsError::NewerSchema { found, supported } => {
                Some(serde_json::json!({"found": found, "supported": supported}))
            }
            _ => None,
        };
        let out = HostError::new(e.code(), e.to_string());
        match details {
            Some(d) => out.with_details(d),
            None => out,
        }
    }
}

/// Classify a store error. Messages are fixed strings: an `io::Error` text or a path would reach the
/// renderer otherwise.
fn map_store_error(e: StoreError) -> SettingsError {
    match e {
        StoreError::Conflict { expected, actual } => SettingsError::Conflict { expected, actual },
        StoreError::NewerSchema { found, supported } => {
            SettingsError::NewerSchema { found, supported }
        }
        StoreError::Protection { .. } => SettingsError::Protection,
        // The file backend's documented size refusal always reads "... byte limit".
        StoreError::Invalid(m) if m.contains("byte limit") => SettingsError::DocumentTooLarge {
            len: None,
            limit: None,
        },
        StoreError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
            SettingsError::ReadOnly("the settings storage is read-only")
        }
        StoreError::Io(_) => SettingsError::Storage("the settings storage failed"),
        StoreError::LockTimeout(_) => {
            SettingsError::Storage("the settings are locked by another writer")
        }
        StoreError::Corrupt(_) => SettingsError::Storage("the settings file is damaged"),
        StoreError::PreserveFailed { .. } => {
            SettingsError::Storage("the settings backup step failed; nothing was written")
        }
        _ => SettingsError::Storage("the settings storage failed"),
    }
}

/// The committed document as the renderer sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSnapshot {
    /// The document's top-level values: always a JSON object.
    pub values: Value,
    /// Monotonic committed revision, `0` when nothing is stored yet.
    pub revision: u64,
    pub schema_version: u32,
    /// Set when the write is stored but a later step failed (for example the directory sync). The
    /// write must not be repeated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// What `describe` answers.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDescription {
    pub schema_version: u32,
    pub revision: u64,
    /// The app's UI description, opaque here.
    pub descriptor: Value,
    /// False when the bridge is read-only or the stored document is refused.
    pub writable: bool,
    /// The error code that makes the document unreadable (`newer_schema`, `storage`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<&'static str>,
    pub limits: SettingsLimits,
}

/// The size limits a renderer can show before it submits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsLimits {
    pub max_value_bytes: usize,
    pub max_document_bytes: usize,
}

/// Top-level keys whose value changed, at the revision that produced the change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsChange {
    pub keys: Vec<String>,
    pub revision: u64,
}

type Listener = Arc<dyn Fn(&SettingsChange) + Send + Sync>;
type Listeners = Arc<Mutex<Vec<(u64, Listener)>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Unsubscribes on drop.
pub struct SettingsSubscription {
    id: u64,
    listeners: Listeners,
}

impl Drop for SettingsSubscription {
    fn drop(&mut self) {
        lock(&self.listeners).retain(|(id, _)| *id != self.id);
    }
}

/// The last state the bridge saw, the baseline the next change is measured against.
struct Observed {
    revision: u64,
    values: Map<String, Value>,
}

/// The committed document read for one operation.
struct Current {
    values: Map<String, Value>,
    revision: u64,
}

/// The settings bridge. See the module documentation.
pub struct SettingsBridge {
    document: Document,
    schema: Arc<dyn SettingsSchema>,
    read_only: bool,
    observed: Mutex<Option<Observed>>,
    listeners: Listeners,
    next_id: AtomicU64,
    notify_failures: AtomicU64,
}

impl SettingsBridge {
    /// Bridge `document` under `schema`. Nothing is read or written until the first call.
    pub fn new(document: Document, schema: impl SettingsSchema + 'static) -> Self {
        Self {
            document,
            schema: Arc::new(schema),
            read_only: false,
            observed: Mutex::new(None),
            listeners: Arc::new(Mutex::new(Vec::new())),
            next_id: AtomicU64::new(0),
            notify_failures: AtomicU64::new(0),
        }
    }

    /// Refuse every mutation (`patch`, `reset`) with `read_only`, leaving reads and subscriptions
    /// working. For a build or mode that must not change the settings file.
    pub fn read_only(mut self, yes: bool) -> Self {
        self.read_only = yes;
        self
    }

    /// Subscribers that panicked since the bridge was built. A panic is contained: the commit it
    /// followed stands and later subscribers still run.
    pub fn notify_failures(&self) -> u64 {
        self.notify_failures.load(Ordering::Relaxed)
    }

    /// Be told, after a commit or an observed external edit, which top-level keys changed. The
    /// callback runs on the thread that made the change, outside every lock of the bridge; keep it
    /// short. Dropping the returned value unsubscribes.
    pub fn subscribe(
        &self,
        f: impl Fn(&SettingsChange) + Send + Sync + 'static,
    ) -> SettingsSubscription {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        lock(&self.listeners).push((id, Arc::new(f)));
        SettingsSubscription {
            id,
            listeners: Arc::clone(&self.listeners),
        }
    }

    /// The schema's description plus the live state: revision, writability and why a document is
    /// refused. Always answers, even when the stored document is unreadable.
    pub fn describe(&self) -> SettingsDescription {
        let (revision, refusal) = match self.read() {
            Ok(current) => (current.revision, None),
            Err(e) => (0, Some(e.code())),
        };
        SettingsDescription {
            schema_version: self.schema.schema_version(),
            revision,
            descriptor: self.schema.descriptor(),
            writable: !self.read_only && refusal.is_none(),
            refusal,
            limits: SettingsLimits {
                max_value_bytes: self.schema.max_value_bytes(),
                max_document_bytes: self.schema.max_document_bytes(),
            },
        }
    }

    /// The committed document. Subscribers hear about an external edit this read notices.
    pub fn get(&self) -> Result<SettingsSnapshot, SettingsError> {
        let current = self.read()?;
        let _ = self.observe(&current);
        Ok(self.snapshot(current, None))
    }

    /// Merge `patch` (top-level keys; `null` removes a key). See the module documentation for the
    /// order of checks. `expected_revision` makes the write a compare-and-swap.
    pub fn patch(
        &self,
        patch: &Value,
        expected_revision: Option<u64>,
    ) -> Result<SettingsSnapshot, SettingsError> {
        self.require_writable()?;
        let patch = self.check_patch(patch)?;
        self.commit(&patch, expected_revision)
    }

    /// Remove `keys` from the stored document so the frontend's defaults apply again. The
    /// complete candidate is validated like any other write, so a key the app requires cannot be
    /// reset away.
    pub fn reset(
        &self,
        keys: &[String],
        expected_revision: Option<u64>,
    ) -> Result<SettingsSnapshot, SettingsError> {
        self.require_writable()?;
        if keys.is_empty() {
            return Err(SettingsError::Invalid(
                "name at least one key to reset".into(),
            ));
        }
        let mut patch = Map::new();
        for key in keys {
            self.check_key(key)?;
            patch.insert(key.clone(), Value::Null);
        }
        self.commit(&patch, expected_revision)
    }

    /// Look for a change another writer committed (another window's store, another process) and
    /// tell the subscribers. Returns it. The bridge starts no thread: the app calls this on its own
    /// timer or file watcher. The first observation only records the baseline.
    pub fn poll_external(&self) -> Result<Option<SettingsChange>, SettingsError> {
        let current = self.read()?;
        Ok(self.observe(&current))
    }

    fn require_writable(&self) -> Result<(), SettingsError> {
        if self.read_only {
            Err(SettingsError::ReadOnly("the settings are read-only"))
        } else {
            Ok(())
        }
    }

    fn check_key(&self, key: &str) -> Result<(), SettingsError> {
        let well_formed =
            !key.is_empty() && key.len() <= MAX_KEY_BYTES && !key.chars().any(char::is_control);
        if !well_formed || !self.schema.allows_key(key) {
            return Err(SettingsError::Invalid(format!(
                "the setting {key:?} is not allowed"
            )));
        }
        Ok(())
    }

    fn check_patch(&self, patch: &Value) -> Result<Map<String, Value>, SettingsError> {
        let Value::Object(map) = patch else {
            return Err(SettingsError::Invalid(
                "a settings patch must be a JSON object".into(),
            ));
        };
        let limit = self.schema.max_value_bytes();
        for (key, value) in map {
            self.check_key(key)?;
            if value.is_null() {
                continue;
            }
            let len = serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len());
            if len > limit {
                return Err(SettingsError::ValueTooLarge {
                    key: key.clone(),
                    len,
                    limit,
                });
            }
        }
        Ok(map.clone())
    }

    fn check_candidate(&self, candidate: &Map<String, Value>) -> Result<(), SettingsError> {
        let value_limit = self.schema.max_value_bytes();
        for (key, value) in candidate {
            let len = serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len());
            if len > value_limit {
                return Err(SettingsError::ValueTooLarge {
                    key: key.clone(),
                    len,
                    limit: value_limit,
                });
            }
        }
        let limit = self.schema.max_document_bytes();
        let len = serde_json::to_vec(candidate).map_or(usize::MAX, |bytes| bytes.len());
        if len > limit {
            return Err(SettingsError::DocumentTooLarge {
                len: u64::try_from(len).ok(),
                limit: u64::try_from(limit).ok(),
            });
        }
        self.schema
            .validate(candidate)
            .map_err(SettingsError::Rejected)
    }

    /// Read the committed document and refuse one a newer build wrote.
    fn read(&self) -> Result<Current, SettingsError> {
        let stored = self.document.get().map_err(map_store_error)?;
        let supported = self.schema.schema_version();
        match stored {
            None => Ok(Current {
                values: Map::new(),
                revision: 0,
            }),
            Some(state) if state.version > supported => Err(SettingsError::NewerSchema {
                found: state.version,
                supported,
            }),
            Some(state) => Ok(current_of(state)),
        }
    }

    fn snapshot(&self, current: Current, warning: Option<String>) -> SettingsSnapshot {
        SettingsSnapshot {
            values: Value::Object(current.values),
            revision: current.revision,
            schema_version: self.schema.schema_version(),
            warning,
        }
    }

    fn commit(
        &self,
        patch: &Map<String, Value>,
        expected_revision: Option<u64>,
    ) -> Result<SettingsSnapshot, SettingsError> {
        let attempts = if expected_revision.is_some() {
            1
        } else {
            UNCONDITIONAL_ATTEMPTS
        };
        let mut lost_race = None;
        for _ in 0..attempts {
            let before = self.read()?;
            // A baseline for the diff, and a notice of anything another writer changed meanwhile.
            let _ = self.observe(&before);
            if let Some(expected) = expected_revision {
                if expected != before.revision {
                    return Err(SettingsError::Conflict {
                        expected,
                        actual: before.revision,
                    });
                }
            }
            let candidate = self
                .schema
                .prepare_patch(&before.values, patch)
                .map_err(SettingsError::Rejected)?;
            self.check_candidate(&candidate)?;
            match self.document.put(
                self.schema.schema_version(),
                Value::Object(candidate.clone()),
                Some(before.revision),
            ) {
                Ok(state) => {
                    let after = current_of(state);
                    let _ = self.observe(&after);
                    return Ok(self.snapshot(after, None));
                }
                Err(StoreError::Replaced { .. }) => {
                    // The new file is in place; only a later step failed. Never report a failure
                    // the caller would retry.
                    let after = self.read().unwrap_or(Current {
                        values: candidate,
                        revision: before.revision.saturating_add(1),
                    });
                    let _ = self.observe(&after);
                    return Ok(self.snapshot(
                        after,
                        Some("saved, but a step after the save failed".into()),
                    ));
                }
                Err(StoreError::Conflict { expected, actual }) => {
                    lost_race = Some(SettingsError::Conflict { expected, actual });
                }
                Err(other) => return Err(map_store_error(other)),
            }
        }
        Err(lost_race.unwrap_or(SettingsError::Storage("the settings storage failed")))
    }

    /// Measure `current` against the last state seen, remember it, and tell subscribers about a
    /// difference. The first observation records the baseline and tells nobody.
    fn observe(&self, current: &Current) -> Option<SettingsChange> {
        let change = {
            let mut seen = lock(&self.observed);
            match seen.as_ref() {
                None => {
                    *seen = Some(Observed {
                        revision: current.revision,
                        values: current.values.clone(),
                    });
                    None
                }
                Some(prev)
                    if prev.revision == current.revision && prev.values == current.values =>
                {
                    None
                }
                Some(prev) => {
                    let keys = changed_keys(&prev.values, &current.values);
                    *seen = Some(Observed {
                        revision: current.revision,
                        values: current.values.clone(),
                    });
                    (!keys.is_empty()).then_some(SettingsChange {
                        keys,
                        revision: current.revision,
                    })
                }
            }
        };
        if let Some(change) = &change {
            self.notify(change);
        }
        change
    }

    /// Tell every subscriber, outside the bridge's locks. A panicking subscriber is counted and
    /// skipped: the change is already committed, and the others still run.
    fn notify(&self, change: &SettingsChange) {
        let listeners: Vec<Listener> = lock(&self.listeners)
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            if catch_unwind(AssertUnwindSafe(|| listener(change))).is_err() {
                self.notify_failures.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn current_of(state: DocumentState) -> Current {
    Current {
        values: match state.value {
            Value::Object(map) => map,
            _ => Map::new(),
        },
        revision: state.revision,
    }
}

/// `values` with `patch` applied the way `Document::patch` applies it: each key replaces the
/// stored one, `null` removes it, nested objects are replaced whole.
fn merged(values: &Map<String, Value>, patch: &Map<String, Value>) -> Map<String, Value> {
    let mut next = values.clone();
    for (key, value) in patch {
        if value.is_null() {
            next.remove(key);
        } else {
            next.insert(key.clone(), value.clone());
        }
    }
    next
}

/// The top-level keys whose value differs, in key order.
fn changed_keys(before: &Map<String, Value>, after: &Map<String, Value>) -> Vec<String> {
    let mut keys: BTreeSet<&String> = BTreeSet::new();
    for (key, value) in before {
        if after.get(key) != Some(value) {
            keys.insert(key);
        }
    }
    for key in after.keys() {
        if !before.contains_key(key) {
            keys.insert(key);
        }
    }
    keys.into_iter().cloned().collect()
}

/// The registered settings service: the bridge plus the windows subscribed to its events.
pub struct SettingsHostService {
    bridge: Arc<SettingsBridge>,
    windows: Arc<Mutex<BTreeSet<String>>>,
    emit_failures: Arc<AtomicU64>,
    emitter: Mutex<Option<SettingsSubscription>>,
}

impl SettingsHostService {
    pub fn new(bridge: SettingsBridge) -> Self {
        Self {
            bridge: Arc::new(bridge),
            windows: Arc::new(Mutex::new(BTreeSet::new())),
            emit_failures: Arc::new(AtomicU64::new(0)),
            emitter: Mutex::new(None),
        }
    }

    pub fn bridge(&self) -> &Arc<SettingsBridge> {
        &self.bridge
    }

    /// Windows that asked for [`EVENT_SETTINGS_CHANGED`].
    pub fn subscribed_windows(&self) -> Vec<String> {
        lock(&self.windows).iter().cloned().collect()
    }

    /// Events the shell failed to deliver to a window (window gone, runtime shutting down). A
    /// delivery failure never fails a commit.
    pub fn emit_failures(&self) -> u64 {
        self.emit_failures.load(Ordering::Relaxed)
    }

    pub(crate) fn subscribe_window(&self, label: &str) {
        lock(&self.windows).insert(label.to_owned());
    }

    pub(crate) fn unsubscribe_window(&self, label: &str) {
        lock(&self.windows).remove(label);
    }

    /// Emit [`EVENT_SETTINGS_CHANGED`] to every subscribed window the service still trusts.
    pub(crate) fn attach_emitter(&self, app: AppHandle<Wry>, trust: Option<ServiceTrust>) {
        let windows = Arc::clone(&self.windows);
        let failures = Arc::clone(&self.emit_failures);
        let subscription = self.bridge.subscribe(move |change| {
            let targets: Vec<String> = lock(&windows)
                .iter()
                .filter(|label| trust.as_ref().is_some_and(|t| t.trusts_label(label)))
                .cloned()
                .collect();
            for label in targets {
                // Subscriptions survive navigation; authorize current native caller
                // before delivering document keys or revision to that webview.
                let Some(webview) = app.get_webview(&label) else {
                    continue;
                };
                if super::authorize(&webview, ServiceId::SETTINGS, "subscribe").is_err() {
                    continue;
                }
                if app
                    .emit_to(label, EVENT_SETTINGS_CHANGED, change.clone())
                    .is_err()
                {
                    failures.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        *lock(&self.emitter) = Some(subscription);
    }
}

/// Gather the caller from the invocation context and resolve the settings service for `action`.
fn settings_service(
    webview: &Webview<Wry>,
    action: &str,
) -> Result<Arc<SettingsHostService>, HostError> {
    let app = webview.app_handle();
    let host = super::managed(app)?;
    let owned = caller_gate::gather(webview)?;
    host.service_for::<SettingsHostService>(ServiceId::SETTINGS, action, &owned.facts())
}

/// The settings bridge `facts` may use for `action`; the first call of every settings command.
pub fn settings_for(
    host: &HostServices,
    action: &str,
    facts: &caller_gate::CallerFacts<'_>,
) -> Result<Arc<SettingsHostService>, HostError> {
    host.service_for::<SettingsHostService>(ServiceId::SETTINGS, action, facts)
}

/// The schema's description and the live state.
#[tauri::command]
pub async fn rightkit_settings_describe(
    webview: Webview<Wry>,
) -> Result<SettingsDescription, HostError> {
    let service = settings_service(&webview, "describe")?;
    blocking(move || Ok(service.bridge().describe())).await
}

/// The committed document and its revision.
#[tauri::command]
pub async fn rightkit_settings_get(webview: Webview<Wry>) -> Result<SettingsSnapshot, HostError> {
    let service = settings_service(&webview, "get")?;
    blocking(move || service.bridge().get().map_err(HostError::from)).await
}

/// Merge `patch` into the document. `expected_revision` (`expectedRevision` from JS) makes the
/// write a compare-and-swap.
#[tauri::command]
pub async fn rightkit_settings_patch(
    webview: Webview<Wry>,
    patch: Value,
    expected_revision: Option<u64>,
) -> Result<SettingsSnapshot, HostError> {
    let service = settings_service(&webview, "patch")?;
    blocking(move || {
        service
            .bridge()
            .patch(&patch, expected_revision)
            .map_err(HostError::from)
    })
    .await
}

/// Remove `keys` from the document.
#[tauri::command]
pub async fn rightkit_settings_reset(
    webview: Webview<Wry>,
    keys: Vec<String>,
    expected_revision: Option<u64>,
) -> Result<SettingsSnapshot, HostError> {
    let service = settings_service(&webview, "reset")?;
    blocking(move || {
        service
            .bridge()
            .reset(&keys, expected_revision)
            .map_err(HostError::from)
    })
    .await
}

/// Subscribe the invoking window (its native label, not an argument) to
/// [`EVENT_SETTINGS_CHANGED`]. Answers the current revision.
#[tauri::command]
pub async fn rightkit_settings_subscribe(webview: Webview<Wry>) -> Result<u64, HostError> {
    let service = settings_service(&webview, "subscribe")?;
    service.subscribe_window(webview.label());
    blocking(move || {
        service
            .bridge()
            .get()
            .map(|snapshot| snapshot.revision)
            .map_err(HostError::from)
    })
    .await
}

/// Stop sending [`EVENT_SETTINGS_CHANGED`] to the invoking window.
#[tauri::command]
pub async fn rightkit_settings_unsubscribe(webview: Webview<Wry>) -> Result<(), HostError> {
    let service = settings_service(&webview, "subscribe")?;
    service.unsubscribe_window(webview.label());
    Ok(())
}
