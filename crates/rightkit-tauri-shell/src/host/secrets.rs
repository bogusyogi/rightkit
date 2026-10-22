//! API-key slots: `rightkit_secrets_presence`, `rightkit_secrets_set`, `rightkit_secrets_clear`.
//!
//! The rules (locked ruling D6: a typed write-only host API, not a generic credential API):
//!
//! * **No command returns a secret.** The store the app supplies ([`SecretStore`]) has `exists`
//!   and `delete` and no read; the writer ([`SecretWriter`]) has `store` and no read. A secret
//!   value enters through `set` and can never come back through this crate. The responses are a
//!   presence list, `()` and error envelopes whose text is fixed here, never the store's.
//! * **Only app-declared slots.** The app declares each [`ApiKeySlot`] (a [`SecretRef`] and a
//!   length cap) when it builds the service. A call naming any other ref is refused for all three
//!   commands, so a renderer cannot probe, overwrite or delete an arbitrary credential.
//! * **`set` is an explicit act.** The request carries a [`WriteIntent`] (`create` refuses to
//!   replace a stored key; `replace` overwrites it), is accepted only from a caller the SETTINGS
//!   service also trusts, and the value is bounded and checked before it reaches the writer.
//! * **No content window.** Each command takes the invoking webview from the native context and
//!   passes it through [`crate::host::HostCallerPolicy`] before it touches the store.
//!
//! [`SecretValue`] has no `Serialize`, `Clone` or `Display`, and a redacted `Debug`; the
//! `tests/host_services.rs` suite fails if one is added to its declaration.

use std::sync::Arc;

use rightkit_app_settings::SecretRef;
use serde::{Deserialize, Serialize};
use tauri::{Manager, Webview, Wry};

use super::{blocking, policy::ServiceId, HostError, HostServices};
use crate::caller_gate::{self, CallerFacts};

/// The default largest secret value, in bytes.
pub const DEFAULT_MAX_SECRET_BYTES: usize = 4096;

/// A secret on its way into the writer. Holds the value in memory only until it is dropped
/// (overwritten best-effort first). Never serialized, cloned or printed.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct SecretValue(String);

impl SecretValue {
    /// The text, for a [`SecretWriter`] to hand to the credential store. Nothing else should call
    /// this.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for SecretValue {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        // Best effort: the allocator or an earlier copy may still hold the bytes.
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
    }
}

/// What a `set` is allowed to do to a slot that may already hold a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteIntent {
    /// Store the key only when the slot is empty; a stored key is refused (`secret_exists`).
    Create,
    /// Store the key, replacing a stored one.
    Replace,
}

/// One API key the app lets its settings UI enter. The ref is where the credential store keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeySlot {
    secret: SecretRef,
    max_bytes: usize,
}

impl ApiKeySlot {
    pub fn new(secret: SecretRef) -> Self {
        Self {
            secret,
            max_bytes: DEFAULT_MAX_SECRET_BYTES,
        }
    }

    /// Refuse a key longer than `limit` bytes. Default [`DEFAULT_MAX_SECRET_BYTES`].
    pub fn max_bytes(mut self, limit: usize) -> Self {
        self.max_bytes = limit;
        self
    }

    pub fn secret(&self) -> &SecretRef {
        &self.secret
    }
}

/// Presence of a slot's key, as the renderer sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretPresence {
    Present,
    Absent,
    /// The credential store could not answer (locked, unavailable).
    Unknown,
}

/// Why the credential store failed. The text is for the app's own logs: it is NEVER forwarded to
/// the renderer, so an implementation cannot leak through it, but it must still not contain a
/// secret value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretStoreError {
    /// The store cannot be reached right now (locked keychain, missing service).
    Unavailable(String),
    /// A `create` found a stored key (see [`SecretWriter::store`]).
    AlreadyExists,
    /// Any other refusal.
    Failed(String),
}

/// The credential store the app supplies: existence and deletion, and NO read. A credentials
/// crate adapts to it in a few lines; the shell does not depend on one.
pub trait SecretStore: Send + Sync {
    /// Whether a key is stored for `secret`.
    fn exists(&self, secret: &SecretRef) -> Result<bool, SecretStoreError>;

    /// Remove the key for `secret`. Removing an absent key succeeds.
    fn delete(&self, secret: &SecretRef) -> Result<(), SecretStoreError>;
}

/// The write side, supplied separately so an app can offer presence and clear without `set`.
pub trait SecretWriter: Send + Sync {
    /// Store `value` for `secret`. With `overwrite` false the call must fail with
    /// [`SecretStoreError::AlreadyExists`] when a key is stored, atomically with the write.
    fn store(
        &self,
        secret: &SecretRef,
        value: &SecretValue,
        overwrite: bool,
    ) -> Result<(), SecretStoreError>;
}

/// Why a secrets call failed. Each variant has a stable [`SecretsError::code`] and a fixed message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretsError {
    /// The ref is not one of the app's declared slots.
    SlotNotDeclared,
    /// The value is empty, too long or holds a control character.
    BadValue(&'static str),
    /// `set` is not available: the app supplied no writer.
    WriteUnavailable,
    /// A `create` found a stored key.
    Exists,
    /// The credential store could not be reached.
    StoreUnavailable,
    /// The credential store refused the operation.
    StoreFailed,
}

impl SecretsError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::SlotNotDeclared => "slot_not_declared",
            Self::BadValue(_) => "invalid_secret",
            Self::WriteUnavailable => "unsupported",
            Self::Exists => "secret_exists",
            Self::StoreUnavailable => "secret_store_unavailable",
            Self::StoreFailed => "secret_store_failed",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::SlotNotDeclared => "that credential slot is not declared by this app",
            Self::BadValue(reason) => reason,
            Self::WriteUnavailable => "this app does not accept API keys from the settings window",
            Self::Exists => "a key is already stored; replace it explicitly",
            Self::StoreUnavailable => "the credential store is not available",
            Self::StoreFailed => "the credential store refused the operation",
        }
    }
}

impl std::fmt::Display for SecretsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for SecretsError {}

impl From<SecretsError> for HostError {
    fn from(e: SecretsError) -> Self {
        HostError::new(e.code(), e.message())
    }
}

fn store_failure(e: SecretStoreError) -> SecretsError {
    match e {
        SecretStoreError::Unavailable(_) => SecretsError::StoreUnavailable,
        SecretStoreError::AlreadyExists => SecretsError::Exists,
        SecretStoreError::Failed(_) => SecretsError::StoreFailed,
    }
}

/// The registered secrets service: the declared slots over the app's store and writer.
pub struct SecretsService {
    store: Arc<dyn SecretStore>,
    writer: Option<Arc<dyn SecretWriter>>,
    slots: Vec<ApiKeySlot>,
}

impl SecretsService {
    /// Presence and clear over `store` for the declared `slots`. `set` is unavailable until a
    /// writer is added with [`SecretsService::with_writer`].
    pub fn new(
        store: impl SecretStore + 'static,
        slots: impl IntoIterator<Item = ApiKeySlot>,
    ) -> Self {
        Self {
            store: Arc::new(store),
            writer: None,
            slots: slots.into_iter().collect(),
        }
    }

    /// Accept `set` for the declared slots through `writer`.
    pub fn with_writer(mut self, writer: impl SecretWriter + 'static) -> Self {
        self.writer = Some(Arc::new(writer));
        self
    }

    /// The declared slots.
    pub fn slots(&self) -> &[ApiKeySlot] {
        &self.slots
    }

    fn slot(&self, secret: &SecretRef) -> Result<&ApiKeySlot, SecretsError> {
        self.slots
            .iter()
            .find(|slot| slot.secret == *secret)
            .ok_or(SecretsError::SlotNotDeclared)
    }

    /// Presence of each ref, in order. A ref that is not a declared slot refuses the whole call. A
    /// store that cannot answer reports `unknown` for that ref.
    pub fn presence(&self, refs: &[SecretRef]) -> Result<Vec<SecretPresence>, SecretsError> {
        for secret in refs {
            self.slot(secret)?;
        }
        Ok(refs
            .iter()
            .map(|secret| match self.store.exists(secret) {
                Ok(true) => SecretPresence::Present,
                Ok(false) => SecretPresence::Absent,
                Err(_) => SecretPresence::Unknown,
            })
            .collect())
    }

    /// Remove the key of a declared slot.
    pub fn clear(&self, secret: &SecretRef) -> Result<(), SecretsError> {
        self.slot(secret)?;
        self.store.delete(secret).map_err(store_failure)
    }

    /// Store `value` in a declared slot. The value is checked first: non-empty, within the slot's
    /// cap, no control characters (it may later become a header).
    pub fn set(
        &self,
        secret: &SecretRef,
        value: &SecretValue,
        intent: WriteIntent,
    ) -> Result<(), SecretsError> {
        let slot = self.slot(secret)?;
        let writer = self.writer.as_ref().ok_or(SecretsError::WriteUnavailable)?;
        if value.is_empty() {
            return Err(SecretsError::BadValue("the key is empty"));
        }
        if value.len() > slot.max_bytes {
            return Err(SecretsError::BadValue("the key is too long"));
        }
        if value.expose().chars().any(char::is_control) {
            return Err(SecretsError::BadValue("the key holds a control character"));
        }
        writer
            .store(secret, value, intent == WriteIntent::Replace)
            .map_err(store_failure)
    }
}

/// The secrets service `facts` may use for `action`. `set` additionally requires a caller the
/// SETTINGS service trusts: a key is entered only from the settings window.
pub fn secrets_for(
    host: &HostServices,
    action: &str,
    facts: &CallerFacts<'_>,
) -> Result<Arc<SecretsService>, HostError> {
    let service = host.service_for::<SecretsService>(ServiceId::SECRETS, action, facts)?;
    if action == "set" {
        host.policy().decide_caller(ServiceId::SETTINGS, facts)?;
    }
    Ok(service)
}

fn secrets_service(webview: &Webview<Wry>, action: &str) -> Result<Arc<SecretsService>, HostError> {
    let app = webview.app_handle();
    let host = super::managed(app)?;
    let owned = caller_gate::gather(webview)?;
    secrets_for(&host, action, &owned.facts())
}

/// Presence of each ref (`present`, `absent` or `unknown`), never a value.
#[tauri::command]
pub async fn rightkit_secrets_presence(
    webview: Webview<Wry>,
    refs: Vec<SecretRef>,
) -> Result<Vec<SecretPresence>, HostError> {
    let service = secrets_service(&webview, "presence")?;
    blocking(move || service.presence(&refs).map_err(HostError::from)).await
}

/// Store an API key in a declared slot. Write-only: answers `()`. The renderer states the
/// [`WriteIntent`] (`"create"` or `"replace"`).
#[tauri::command]
pub async fn rightkit_secrets_set(
    webview: Webview<Wry>,
    secret: SecretRef,
    value: SecretValue,
    intent: WriteIntent,
) -> Result<(), HostError> {
    let service = secrets_service(&webview, "set")?;
    blocking(move || {
        service
            .set(&secret, &value, intent)
            .map_err(HostError::from)
    })
    .await
}

/// Remove the key of a declared slot.
#[tauri::command]
pub async fn rightkit_secrets_clear(
    webview: Webview<Wry>,
    secret: SecretRef,
) -> Result<(), HostError> {
    let service = secrets_service(&webview, "clear")?;
    blocking(move || service.clear(&secret).map_err(HostError::from)).await
}
