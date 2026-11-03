//! Native HostServices adapter. IPC carries no owner, challenge, PIN or success proof.
use super::{HostError, HostServices};
#[cfg(feature = "engine")]
use crate::app_lock::EngineLockError;
use crate::app_lock::{
    self, AppLockService, EngineLock, LockError, OwnerSession, Status, UnlockChallenge,
};
use rightkit_credentials::os_auth::Request;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tauri::{AppHandle, Manager, Webview, Wry};

/// Optional adapter for apps whose actual call path uses shell EngineHost. Other native
/// daemon/local-engine owners implement EngineLock directly over their real admission path.
#[cfg(feature = "engine")]
pub struct EngineHostLock {
    engine: Arc<crate::engine::EngineHost>,
    native: Arc<dyn EngineLock>,
}
#[cfg(feature = "engine")]
impl EngineHostLock {
    pub fn new(engine: Arc<crate::engine::EngineHost>, native: Arc<dyn EngineLock>) -> Self {
        Self { engine, native }
    }
}
#[cfg(feature = "engine")]
impl EngineLock for EngineHostLock {
    fn revoke(&self) -> Result<(), EngineLockError> {
        // Fence new calls first; never claim Locked while an admitted call still runs.
        self.engine.app_lock_revoke()?;
        self.native.revoke()
    }
    fn resume(&self) -> Result<(), EngineLockError> {
        self.native.resume()?;
        self.engine.app_lock_resume()
    }
}
struct Document {
    owner: Arc<OwnerSession>,
    pending: Mutex<Option<UnlockChallenge>>,
}
#[derive(Default)]
struct Documents {
    generation: u64,
    entries: BTreeMap<String, Arc<Document>>,
}

/// Native product policy, never IPC arguments. Invalidation always rejects old document
/// challenges; Lock additionally revokes engine authority before Locked can be announced.
#[derive(Clone, Copy)]
pub enum LifecycleAction {
    InvalidateOnly,
    Lock,
}
#[derive(Clone, Copy)]
pub struct LifecyclePolicy {
    pub navigation: LifecycleAction,
    pub destruction: LifecycleAction,
    pub resume: LifecycleAction,
}
#[derive(Clone, Copy)]
pub(crate) enum LifecycleEvent {
    Navigation,
    Destruction,
    Resume,
}
impl LifecyclePolicy {
    fn action(self, event: LifecycleEvent) -> LifecycleAction {
        match event {
            LifecycleEvent::Navigation => self.navigation,
            LifecycleEvent::Destruction => self.destruction,
            LifecycleEvent::Resume => self.resume,
        }
    }
}

pub struct NativeAppLock {
    pub(crate) service: Arc<AppLockService>,
    documents: Mutex<Documents>,
    reason: String,
    timeout: Duration,
    lifecycle: LifecyclePolicy,
}
impl NativeAppLock {
    /// `native` revokes daemon/session authority, including cached authority & boot replay.
    /// It owns actual privileged admission across every caller & engine restart. Use optional
    /// EngineHostLock only if those callers really use shell EngineHost. Resume restores local
    /// admission only: it must not create a remote approval, signature or PIN grant.
    /// Product explicitly chooses lifecycle policy; no credential/PIN verifier is accepted here.
    pub fn new(
        native: Arc<dyn EngineLock>,
        reason: String,
        timeout: Duration,
        challenge_ttl: Duration,
        proof_max_age: Duration,
        lifecycle: LifecyclePolicy,
    ) -> Result<Self, LockError> {
        if reason.trim().is_empty()
            || reason.len() > Request::MAX_REASON_BYTES
            || reason.chars().any(char::is_control)
            || !(Request::MIN_TIMEOUT..=Request::MAX_TIMEOUT).contains(&timeout)
        {
            return Err(LockError::Invalid);
        }
        Ok(Self {
            service: Arc::new(AppLockService::new(native, challenge_ttl, proof_max_age)?),
            documents: Mutex::new(Documents::default()),
            reason,
            timeout,
            lifecycle,
        })
    }
    fn document(
        &self,
        host: &HostServices,
        webview: &Webview<Wry>,
        action: &str,
    ) -> Result<Arc<Document>, HostError> {
        // Capture can dispatch webview.url() to main thread. Never hold documents across it.
        let (mut docs, facts) = self
            .document_snapshot(|| crate::caller_gate::gather(webview).map_err(HostError::from))?;
        app_lock::service_for(host, action, &facts.facts(), None).map_err(error)?;
        if let Some(doc) = docs.entries.get(&facts.label) {
            app_lock::service_for(host, action, &facts.facts(), Some(&doc.owner)).map_err(error)?;
            return Ok(Arc::clone(doc));
        }
        let (_, owner) = app_lock::bind_owner(host, &facts.facts()).map_err(error)?;
        let doc = Arc::new(Document {
            owner: Arc::new(owner),
            pending: Mutex::new(None),
        });
        docs.entries.insert(facts.label.clone(), Arc::clone(&doc));
        Ok(doc)
    }
    fn document_snapshot<T>(
        &self,
        capture: impl FnOnce() -> Result<T, HostError>,
    ) -> Result<(MutexGuard<'_, Documents>, T), HostError> {
        let generation = self
            .documents
            .lock()
            .map_err(|_| error(LockError::Poisoned))?
            .generation;
        let facts = capture()?;
        let docs = self
            .documents
            .lock()
            .map_err(|_| error(LockError::Poisoned))?;
        if docs.generation != generation {
            return Err(error(LockError::Stale));
        }
        Ok((docs, facts))
    }
    fn invalidate(&self, label: Option<&str>, event: LifecycleEvent) -> Result<(), LockError> {
        let mut docs = self.documents.lock().map_err(|_| {
            // Even poisoned document bookkeeping cannot leave engine admission open.
            let _ = self.service.lock();
            LockError::Poisoned
        })?;
        // Advance even for empty maps, so an in-flight capture cannot bind stale facts.
        docs.generation = docs.generation.checked_add(1).ok_or_else(|| {
            let _ = self.service.lock();
            LockError::Stale
        })?;
        if let Some(label) = label {
            if let Some(doc) = docs.entries.remove(label) {
                self.service.invalidate_owner(&doc.owner)?;
            }
        } else {
            // Revoke every owner before dropping map entries, including running OS prompts.
            for doc in docs.entries.values() {
                self.service.invalidate_owner(&doc.owner)?;
            }
            docs.entries.clear();
        }
        if matches!(self.lifecycle.action(event), LifecycleAction::Lock) {
            self.service.lock()?;
        }
        Ok(())
    }
}
fn error(e: LockError) -> HostError {
    HostError::new(e.code(), e.code())
}
fn native(webview: &Webview<Wry>, action: &str) -> Result<Arc<NativeAppLock>, HostError> {
    let host = super::managed(webview.app_handle())?;
    let facts = crate::caller_gate::gather(webview)?;
    // Every command, even status/lock/cancel, uses closed service action policy.
    app_lock::service_for(&host, action, &facts.facts(), None).map_err(error)?;
    host.app_lock
        .clone()
        .ok_or_else(|| HostError::unsupported("app-lock is not installed"))
}

#[tauri::command]
pub fn rightkit_app_lock_status(webview: Webview<Wry>) -> Result<Status, HostError> {
    let native = native(&webview, "status")?;
    native.service.status().map_err(error)
}
#[tauri::command]
pub async fn rightkit_app_lock_lock(webview: Webview<Wry>) -> Result<Status, HostError> {
    let native = native(&webview, "lock")?;
    super::blocking(move || native.service.lock().map_err(error)).await
}
#[tauri::command]
pub fn rightkit_app_lock_begin_unlock(webview: Webview<Wry>) -> Result<Status, HostError> {
    let native = native(&webview, "begin_unlock")?;
    let host = super::managed(webview.app_handle())?;
    let doc = native.document(&host, &webview, "begin_unlock")?;
    let mut pending = doc.pending.lock().map_err(|_| error(LockError::Poisoned))?;
    // Old expired handles may be replaced; service enforces single live challenge globally.
    let challenge = native.service.begin_unlock(&doc.owner).map_err(error)?;
    *pending = Some(challenge);
    Ok(Status::Unlocking)
}
#[tauri::command]
pub fn rightkit_app_lock_cancel_unlock(webview: Webview<Wry>) -> Result<Status, HostError> {
    let native = native(&webview, "cancel_unlock")?;
    let host = super::managed(webview.app_handle())?;
    let doc = native.document(&host, &webview, "cancel_unlock")?;
    let pending = doc
        .pending
        .lock()
        .map_err(|_| error(LockError::Poisoned))?
        .take();
    if let Some(challenge) = pending {
        native
            .service
            .cancel_unlock(&doc.owner, &challenge)
            .map_err(error)?;
    } else {
        // In-flight complete owns its handle: lock invalidates that prompt's late result.
        native.service.lock().map_err(error)?;
    }
    native.service.status().map_err(error)
}
#[tauri::command]
pub async fn rightkit_app_lock_complete_unlock(webview: Webview<Wry>) -> Result<Status, HostError> {
    let native = native(&webview, "complete_unlock")?;
    let host = super::managed(webview.app_handle())?;
    let doc = native.document(&host, &webview, "complete_unlock")?;
    let challenge = doc
        .pending
        .lock()
        .map_err(|_| error(LockError::Poisoned))?
        .take()
        .ok_or_else(|| error(LockError::Stale))?;
    #[cfg(target_os = "windows")]
    let hwnd = webview
        .window()
        .hwnd()
        .map_err(|_| {
            error(LockError::Authentication(
                rightkit_credentials::os_auth::OsAuthError::Unavailable,
            ))
        })?
        .0 as isize;
    super::blocking(move || {
        let request = Request::new(&native.reason).timeout(native.timeout);
        #[cfg(target_os = "windows")]
        let request = request.owner_window(hwnd);
        native
            .service
            .complete_unlock(&doc.owner, challenge, &request)
            .map_err(error)
    })
    .await
}

/// Native lifecycle only; failure exits instead of continuing with uncertain engine authority.
pub(crate) fn invalidate(app: &AppHandle<Wry>, label: Option<&str>, event: LifecycleEvent) -> bool {
    let Some(host) = app.try_state::<HostServices>() else {
        return true;
    };
    let Some(native) = &host.app_lock else {
        return true;
    };
    if let Err(e) = native.invalidate(label, event) {
        eprintln!("rightkit app-lock lifecycle: {}", e.code());
        app.exit(1);
        return false;
    }
    true
}
