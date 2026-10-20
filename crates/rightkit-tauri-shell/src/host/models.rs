//! Native model commands over the existing revisioned `rightkit-models::Store`.
//! Catalog, paths, revisions, download URLs & credentials are native configuration;
//! IPC accepts only a declared model id. `cancel` pauses, preserving resumable parts.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rightkit_models::{ErrorCode, Event, InstallOptions, ModelId, ModelSpec, Store, VerifyMode};
use serde::Serialize;
use tauri::{Emitter, Manager, Webview, Wry};

use super::{blocking, managed, HostError, ServiceId};
use crate::caller_gate;

pub const EVENT_MODELS_ENTRY: &str = "rightkit://models-entry";
const MAX_MODELS: usize = 256;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelState {
    Missing,
    Installed,
    Downloading,
    Verifying,
    Error,
}

/// Structural `ModelEntry` consumed by settings-ui & onboarding. No store paths or URLs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub id: String,
    pub label: String,
    pub state: ModelState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub removable: bool,
}

/// One immutable native catalog entry. Install options never cross IPC.
pub struct DeclaredModel {
    pub spec: ModelSpec,
    pub options: InstallOptions,
    pub removable: bool,
}

impl DeclaredModel {
    pub fn new(spec: ModelSpec, options: InstallOptions) -> Self {
        Self {
            spec,
            options,
            removable: false,
        }
    }

    /// Removal still goes through Store::uninstall_unreferenced: any registered
    /// app reference wins over this UI permission. Runtimes must retain their refs.
    pub fn with_removal(mut self, removable: bool) -> Self {
        self.removable = removable;
        self
    }
}

pub struct ModelsHostService {
    store: Store,
    catalog: BTreeMap<String, DeclaredModel>,
    active: Mutex<BTreeMap<String, Arc<AtomicBool>>>,
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|error| error.into_inner())
}

impl ModelsHostService {
    pub fn new(
        store: Store,
        catalog: impl IntoIterator<Item = DeclaredModel>,
    ) -> Result<Self, HostError> {
        if !store.root.is_absolute() {
            return Err(HostError::invalid(
                "model store requires an absolute native root",
            ));
        }
        let mut entries = BTreeMap::new();
        for mut entry in catalog {
            ModelId::parse(&entry.spec.id)
                .map_err(|_| HostError::invalid("invalid declared model id"))?;
            if entries.len() >= MAX_MODELS || entry.spec.label.len() > 512 {
                return Err(HostError::invalid("model catalog exceeds limits"));
            }
            let revision = entry
                .spec
                .select(
                    entry.options.revision.as_deref(),
                    entry.options.quant.as_deref(),
                )
                .ok_or_else(|| HostError::invalid("declared model has no selected revision"))?;
            revision
                .checked_key()
                .map_err(|_| HostError::invalid("invalid declared model revision"))?;
            // The owner validates file/platform pins at install admission. Fail closed for
            // every native entry, regardless of an app's looser InstallOptions default.
            entry.options.require_pinned = true;
            entry.options.max_download_redirects = 0;
            let id = entry.spec.id.clone();
            if entries.insert(id, entry).is_some() {
                return Err(HostError::invalid("duplicate declared model id"));
            }
        }
        Ok(Self {
            store,
            catalog: entries,
            active: Mutex::new(BTreeMap::new()),
        })
    }

    fn declared(&self, id: &str) -> Result<&DeclaredModel, HostError> {
        self.catalog
            .get(id)
            .ok_or_else(|| HostError::new("not_found", "model is not declared"))
    }

    fn entry(&self, id: &str) -> Result<ModelEntry, HostError> {
        let declared = self.declared(id)?;
        let state = self.store.state(id).map_err(model_error)?;
        let selected = declared
            .spec
            .select(
                declared.options.revision.as_deref(),
                declared.options.quant.as_deref(),
            )
            .ok_or_else(|| HostError::invalid("declared model has no selected revision"))?;
        let key = selected
            .checked_key()
            .map_err(|_| HostError::invalid("invalid declared model revision"))?;
        let installed = state.active.as_deref() == Some(key.as_str())
            && self.store.verify(id, key.as_str(), VerifyMode::Size);
        let revision = installed.then(|| selected.revision.clone());
        Ok(ModelEntry {
            id: id.into(),
            label: declared.spec.label.clone(),
            state: if lock(&self.active).contains_key(id) {
                ModelState::Downloading
            } else if installed {
                ModelState::Installed
            } else {
                ModelState::Missing
            },
            progress: None,
            revision,
            removable: declared.removable,
        })
    }

    pub fn list(&self) -> Result<Vec<ModelEntry>, HostError> {
        self.catalog.keys().map(|id| self.entry(id)).collect()
    }

    fn begin(self: &Arc<Self>, id: &str) -> Result<Operation, HostError> {
        self.declared(id)?;
        let mut active = lock(&self.active);
        if active.contains_key(id) {
            return Err(HostError::new("busy", "model operation is already active"));
        }
        let pause = Arc::new(AtomicBool::new(false));
        active.insert(id.into(), pause.clone());
        Ok(Operation {
            service: self.clone(),
            id: id.into(),
            pause,
        })
    }

    pub fn cancel(&self, id: &str) -> Result<(), HostError> {
        self.declared(id)?;
        if let Some(pause) = lock(&self.active).get(id) {
            pause.store(true, Ordering::Release);
        }
        Ok(())
    }

    pub fn install(
        self: &Arc<Self>,
        id: &str,
        emit: impl FnMut(ModelEntry),
    ) -> Result<(), HostError> {
        let operation = self.begin(id)?;
        self.install_admitted(operation, emit)
    }

    fn install_admitted(
        &self,
        operation: Operation,
        mut emit: impl FnMut(ModelEntry),
    ) -> Result<(), HostError> {
        let id = operation.id.clone();
        let id = id.as_str();
        let declared = self.declared(id)?;
        let mut options = declared.options.clone();
        options.pause = operation.pause.clone();
        options.cancel = Arc::new(AtomicBool::new(false));
        let mut view = self.entry(id)?;
        emit(view.clone());
        let result = self.store.install(&declared.spec, &options, |event| {
            match event {
                Event::Progress {
                    overall_done,
                    overall_total,
                    ..
                } => {
                    view.state = ModelState::Downloading;
                    view.progress = Some(if overall_total == 0 {
                        0.0
                    } else {
                        (overall_done as f64 / overall_total as f64).clamp(0.0, 1.0)
                    });
                }
                Event::FileVerified { .. } => {
                    view.state = ModelState::Verifying;
                }
                _ => return,
            }
            emit(view.clone());
        });
        drop(operation);
        let mut terminal = self.entry(id)?;
        if let Err(error) = &result {
            if !matches!(error.code, ErrorCode::Paused | ErrorCode::Cancelled) {
                terminal.state = ModelState::Error;
            }
        }
        emit(terminal);
        match result {
            Ok(_) => Ok(()),
            Err(error) if matches!(error.code, ErrorCode::Paused | ErrorCode::Cancelled) => Ok(()),
            Err(error) => Err(model_error(error)),
        }
    }

    pub fn remove(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        if !self.declared(id)?.removable {
            return Err(HostError::new(
                "permission_denied",
                "model removal is disabled",
            ));
        }
        let _operation = self.begin(id)?;
        self.store.uninstall_unreferenced(id).map_err(model_error)
    }
}

struct Operation {
    service: Arc<ModelsHostService>,
    id: String,
    pause: Arc<AtomicBool>,
}

impl Drop for Operation {
    fn drop(&mut self) {
        lock(&self.service.active).remove(&self.id);
    }
}

fn model_error(error: rightkit_models::Error) -> HostError {
    let code = match error.code {
        ErrorCode::Cancelled => "cancelled",
        ErrorCode::Paused => "paused",
        ErrorCode::Conflict => "conflict",
        ErrorCode::DiskSpace => "disk_space",
        ErrorCode::Download => "download",
        ErrorCode::SizeMismatch => "size_mismatch",
        ErrorCode::HashMismatch => "hash_mismatch",
        ErrorCode::Extract => "extract",
        ErrorCode::Io => "io",
        ErrorCode::Busy => "busy",
        ErrorCode::NotFound => "not_found",
        ErrorCode::NoRollback => "no_rollback",
        ErrorCode::NoFiles => "no_files",
        ErrorCode::InvalidSpec => "invalid_spec",
        ErrorCode::AllMirrorsFailed => "all_mirrors_failed",
    };
    // Owner diagnostics may contain artifact URLs, credentials or paths. Expose code only.
    HostError::new(code, "model operation could not complete")
}

fn models_service(
    webview: &Webview<Wry>,
    action: &str,
) -> Result<Arc<ModelsHostService>, HostError> {
    let host = managed(webview.app_handle())?;
    let caller = caller_gate::gather(webview)?;
    host.service_for(ServiceId::MODELS, action, &caller.facts())
}

#[tauri::command]
pub async fn rightkit_models_list(webview: Webview<Wry>) -> Result<Vec<ModelEntry>, HostError> {
    let service = models_service(&webview, "list")?;
    blocking(move || service.list()).await
}

#[tauri::command]
pub async fn rightkit_models_install(webview: Webview<Wry>, id: String) -> Result<(), HostError> {
    let service = models_service(&webview, "install")?;
    // Admit before dispatching blocking work so an immediately following cancel
    // cannot miss a queued install. Dropping the queued task also releases admission.
    let operation = service.begin(&id)?;
    let window = webview.label().to_owned();
    let app = webview.app_handle().clone();
    blocking(move || {
        service.install_admitted(operation, |entry| {
            let Some(webview) = app.get_webview(&window) else {
                return;
            };
            if super::authorize(&webview, ServiceId::MODELS, "install").is_err() {
                return;
            }
            let _ = app.emit_to(&window, EVENT_MODELS_ENTRY, entry);
        })
    })
    .await
}

#[tauri::command]
pub fn rightkit_models_cancel(webview: Webview<Wry>, id: String) -> Result<(), HostError> {
    let service = models_service(&webview, "cancel")?;
    service.cancel(&id)
}

#[tauri::command]
pub async fn rightkit_models_remove(webview: Webview<Wry>, id: String) -> Result<(), HostError> {
    let service = models_service(&webview, "remove")?;
    blocking(move || service.remove(&id)).await
}
