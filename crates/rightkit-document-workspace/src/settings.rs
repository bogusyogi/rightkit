//! The durable settings document and the explorer record, stored as `rightkit-app-settings`
//! envelopes under `<data_dir>/<namespace>/` (or directly in the app's own folder, `in_app_folder`).

use std::path::PathBuf;
use std::sync::OnceLock;

use rightkit_app_settings::{Document, Error as SettingsError, Schema, StoreOptions};
use serde_json::{json, Map, Value};

use crate::error::WorkspaceError;

/// The settings key the frontend stores its document under.
pub const SETTINGS_KEY: &str = "vr.settings.v1";
/// The version the frontend stores the document under. The frontend's `migrate()` works on
/// marker fields inside the object, so this stays 1: existing `settings.json` envelopes and older
/// builds read the file unchanged.
const DOCUMENT_VERSION: u32 = 1;

fn parse_object(raw: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// The outcome of [`SettingsStore::set`].
#[derive(Debug)]
pub struct SettingsWrite {
    /// The answer to hand back to the renderer.
    pub answer: Value,
    /// The committed settings object when the settings document (not the explorer record) was
    /// written, so the host can apply side effects such as the diagnostics gate.
    pub committed: Option<Value>,
}

impl SettingsWrite {
    /// The diagnostics choice in the committed settings document (default on), or `None` when the
    /// write did not touch the settings document.
    pub fn diagnostics_enabled(&self) -> Option<bool> {
        self.committed.as_ref().map(|settings| {
            settings
                .get("diagnostics")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
    }
}

/// The settings document (`<data_dir>/<namespace>/settings.json`) and the explorer sidebar's
/// record (`explorer.json`, an object of key to text). The TypeScript side owns the settings
/// shape, so the store keeps one opaque JSON object. Only a successful open is cached; a failed
/// open is reported on every call and retried.
pub struct SettingsStore {
    /// The folder holding `settings.json` and `explorer.json` (`None` when unavailable).
    folder: Option<PathBuf>,
    namespace: String,
    document: OnceLock<Document>,
    explorer: OnceLock<Document>,
}

impl SettingsStore {
    /// `data_dir` is the host's per-user data directory (`None` when unavailable) and
    /// `namespace` the app's folder name beneath it.
    pub fn new(data_dir: Option<PathBuf>, namespace: impl Into<String>) -> Self {
        let namespace = namespace.into();
        Self {
            folder: data_dir.map(|dir| dir.join(&namespace)),
            namespace,
            document: OnceLock::new(),
            explorer: OnceLock::new(),
        }
    }

    /// The store in the app's own data folder, as rightkit-fs resolves it (`AppDirs::data`):
    /// `settings.json` and `explorer.json` live directly in `app_folder`, so the host keeps no
    /// parent-folder resolver of its own (viewright#57). `namespace` names the document schema.
    pub fn in_app_folder(app_folder: PathBuf, namespace: impl Into<String>) -> Self {
        Self {
            folder: Some(app_folder),
            namespace: namespace.into(),
            document: OnceLock::new(),
            explorer: OnceLock::new(),
        }
    }

    fn document_path(&self, name: &str) -> Result<PathBuf, WorkspaceError> {
        let folder = self
            .folder
            .as_ref()
            .ok_or_else(|| WorkspaceError::Other("settings directory is unavailable".into()))?;
        Ok(folder.join(name))
    }

    fn options(&self) -> StoreOptions {
        StoreOptions::new(Schema::new(self.namespace.clone(), DOCUMENT_VERSION))
    }

    fn explorer_document(&self) -> Result<&Document, WorkspaceError> {
        if let Some(document) = self.explorer.get() {
            return Ok(document);
        }
        let opened =
            Document::open_path_with(self.document_path("explorer.json")?, &self.options())
                .map_err(settings_error)?;
        Ok(self.explorer.get_or_init(|| opened))
    }

    /// Open the settings document once. An open failure is returned to the caller; no second
    /// store is opened and no in-memory fallback is used.
    fn document(&self) -> Result<&Document, WorkspaceError> {
        if let Some(document) = self.document.get() {
            return Ok(document);
        }
        let opened =
            Document::open_path_with(self.document_path("settings.json")?, &self.options())
                .map_err(settings_error)?;
        Ok(self.document.get_or_init(|| opened))
    }

    /// The diagnostics choice for the native log gate before the settings document is opened
    /// (early startup events): an absent or unreadable choice means on (the default).
    pub fn persisted_diagnostics_enabled(&self) -> bool {
        let Ok(path) = self.document_path("settings.json") else {
            return true;
        };
        match Document::open_path_with(path, &self.options()) {
            Ok(document) => diagnostics_of(&document),
            Err(_) => true,
        }
    }

    /// Open the settings document and answer the diagnostics choice, so the opt-out is in force
    /// before the first native event. If the document cannot be opened, recording stays off.
    pub fn startup_diagnostics_enabled(&self) -> bool {
        match self.document() {
            Ok(document) => diagnostics_of(document),
            Err(_) => false,
        }
    }

    /// Read a durable value. The [`SETTINGS_KEY`] key is the settings document (JSON string, or
    /// `null` when nothing has ever been saved). With `with_revision`, the document read answers
    /// `{ value, revision }` where `revision` is the committed revision from the store (`0` when
    /// nothing is stored). Other keys (the explorer record) read the explorer document.
    pub fn get(&self, key: &str, with_revision: Option<bool>) -> Result<Value, WorkspaceError> {
        if key == SETTINGS_KEY {
            let state = self.document()?.get().map_err(settings_error)?;
            let revision = state.as_ref().map_or(0, |state| state.revision);
            let value = state
                .filter(|state| state.value.as_object().is_some_and(|map| !map.is_empty()))
                .map(|state| state.value.to_string());
            if with_revision == Some(true) {
                return Ok(json!({ "value": value, "revision": revision }));
            }
            return Ok(value.map_or(Value::Null, Value::String));
        }
        let state = self.explorer_document()?.get().map_err(settings_error)?;
        Ok(state
            .and_then(|state| {
                state
                    .value
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .map_or(Value::Null, Value::String))
    }

    /// Persist a value: the settings document, or (any other key) the explorer document.
    ///
    /// For the settings document, `expected_revision` makes the commit a compare-and-swap on the
    /// store's committed revision (a stale one fails as a write conflict, nothing is written),
    /// and `merge` applies `value` as a top-level patch (`null` removes a key) inside the store's
    /// write lock instead of replacing the whole document. The answer is `{ value, revision }`
    /// with the committed document; other keys answer `null`.
    pub fn set(
        &self,
        key: &str,
        value: &str,
        expected_revision: Option<u64>,
        merge: Option<bool>,
    ) -> Result<SettingsWrite, WorkspaceError> {
        if key == SETTINGS_KEY {
            let map = parse_object(value)
                .ok_or_else(|| WorkspaceError::Other("settings must be a JSON object".into()))?;
            let document = self.document()?;
            let state = if merge == Some(true) {
                document.patch(DOCUMENT_VERSION, &Value::Object(map), expected_revision)
            } else {
                document.put(DOCUMENT_VERSION, Value::Object(map), expected_revision)
            }
            .map_err(settings_error)?;
            let committed = state.value.to_string();
            return Ok(SettingsWrite {
                answer: json!({ "value": committed, "revision": state.revision }),
                committed: Some(state.value),
            });
        }
        self.explorer_document()?
            .patch(DOCUMENT_VERSION, &json!({ key: value }), None)
            .map_err(settings_error)?;
        Ok(SettingsWrite {
            answer: Value::Null,
            committed: None,
        })
    }
}

fn diagnostics_of(document: &Document) -> bool {
    document
        .get()
        .ok()
        .flatten()
        .and_then(|state| state.value.get("diagnostics").and_then(Value::as_bool))
        .unwrap_or(true)
}

fn settings_error(error: SettingsError) -> WorkspaceError {
    match error {
        // Another writer committed first (compare-and-swap on the committed revision). The
        // frontend maps this to the settings store's `revision_conflict` and reloads.
        SettingsError::Conflict { expected, actual } => WorkspaceError::WriteConflict(format!(
            "settings revision conflict: expected {expected}, found {actual}"
        )),
        other => WorkspaceError::Other(format!("settings store: {other}")),
    }
}
