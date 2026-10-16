//! Cross-platform durable [`TokenStore`] over the OS credential store (macOS Keychain,
//! Windows Credential Manager) through `rightkit-credentials`. Tokens are never written to
//! a file. The credential store has no cross-process lock, so `lock` takes an advisory file lock
//! (no secret content) in a caller-chosen per-user directory; that lock spans the read, the
//! network refresh and the atomic replacement, so two processes never spend one rotating
//! refresh grant twice.
use super::lock;
use super::{ServerAuthError, TokenStore, TokenStoreGuard};
use crate::TokenSet;
use rightkit_credentials::Secret as CredSecret;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroize::Zeroizing;

/// Keyed by `(account, provider)`: one credential per pair, named by the lower-case hex
/// SHA-256 of the JSON tuple (so no delimiter collisions, no case-folding collisions on
/// Windows, and no account or provider text in the OS item name). The credential namespace
/// (Keychain service / Credential Manager prefix) belongs to the app: use the `Store` the app
/// already opened, with `Interaction::NonInteractive` for daemons.
///
/// The Windows credential blob limit is 2560 bytes; a stored token set (access and refresh
/// token plus metadata) must fit, which holds for Mastodon and X tokens but not for providers
/// issuing very large access tokens. `put` then fails with `Storage` and nothing is replaced.
pub struct KeychainTokenStore {
    credentials: rightkit_credentials::Store,
    lock_dir: PathBuf,
    lock_timeout: Duration,
}

impl std::fmt::Debug for KeychainTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeychainTokenStore")
            .field("namespace", &self.credentials.namespace().service())
            .finish()
    }
}

impl KeychainTokenStore {
    /// `lock_dir` holds only empty lock files. On Unix it must be, or is created as, a 0700
    /// directory owned by the effective user; on Windows put it under `%LOCALAPPDATA%`.
    ///
    /// Fails with [`ServerAuthError::StoreNotDurable`] when `credentials` is not durable (a guard
    /// put it in memory or deny mode): rotated tokens would be lost, so there is no silent
    /// memory fallback.
    pub fn new(
        credentials: rightkit_credentials::Store,
        lock_dir: impl AsRef<Path>,
    ) -> Result<Self, ServerAuthError> {
        if !credentials.is_durable() {
            return Err(ServerAuthError::StoreNotDurable);
        }
        Ok(Self {
            credentials,
            lock_dir: lock::prepare_dir(lock_dir.as_ref())?,
            lock_timeout: Duration::from_secs(65),
        })
    }
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }
}

struct KeychainGuard<'a> {
    store: &'a KeychainTokenStore,
    _lock: File,
    item: String,
}

impl TokenStore for KeychainTokenStore {
    fn lock(
        &self,
        account: &str,
        provider: &str,
    ) -> Result<Box<dyn TokenStoreGuard + '_>, ServerAuthError> {
        if account.is_empty() || provider.is_empty() {
            return Err(ServerAuthError::InvalidConfig);
        }
        let id = lock::hex_digest(&serde_json::json!([account, provider]).to_string());
        let file = lock::acquire(&self.lock_dir, &format!("{id}.lock"), self.lock_timeout)?;
        Ok(Box::new(KeychainGuard {
            store: self,
            _lock: file,
            item: format!("oauth-token/{id}"),
        }))
    }
}

impl TokenStoreGuard for KeychainGuard<'_> {
    fn get(&mut self) -> Result<Option<TokenSet>, ServerAuthError> {
        match self
            .store
            .credentials
            .get(&self.item)
            .map_err(|_| ServerAuthError::Storage)?
        {
            None => Ok(None),
            Some(secret) => {
                let text = secret.expose_str().map_err(|_| ServerAuthError::Storage)?;
                crate::token_codec::decode(text)
                    .map(Some)
                    .map_err(|_| ServerAuthError::Storage)
            }
        }
    }
    fn put(&mut self, tokens: &TokenSet) -> Result<(), ServerAuthError> {
        let mut text = Zeroizing::new(crate::token_codec::encode(tokens));
        // Move the allocation into the secret (zeroed on drop) instead of copying it.
        let secret = CredSecret::from_string(std::mem::take(&mut *text));
        self.store
            .credentials
            .set(&self.item, &secret)
            .map_err(|_| ServerAuthError::Storage)
    }
    fn delete(&mut self) -> Result<(), ServerAuthError> {
        self.store
            .credentials
            .delete(&self.item)
            .map(|_| ())
            .map_err(|_| ServerAuthError::Storage)
    }
}
