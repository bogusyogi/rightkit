use super::{
    GitHubAppError, GitHubAppStore, GitHubDenial, GitHubOperation, GitHubStoreError,
    InstallationAccessToken, InstallationBinding, InstallationTokenLifecycle, OperationRegistry,
    OperationScope, TokenRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Typed authority events suitable for audit receipts. No credential material
/// is carried by any event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorityAuditEvent {
    Installed,
    Replaced,
    Revoked,
    Authorized,
    TokenIssued,
    TokenCacheHit,
    TokenRefreshed,
    TokenRevoked,
}

/// Non-secret metadata attached to every successful service result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityAudit {
    pub event: AuthorityAuditEvent,
    pub app_id: u64,
    pub installation_id: u64,
    pub generation: u64,
    pub operation: Option<GitHubOperation>,
    pub scope: Option<OperationScope>,
    pub repository_ids: Vec<u64>,
}

/// Service result carrying typed audit metadata without making token-bearing
/// results serializable.
#[derive(Debug, Clone)]
pub struct AuthorityOutcome<T> {
    pub value: T,
    pub audit: AuthorityAudit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHubAuthorityError {
    Store(GitHubStoreError),
    Lifecycle(GitHubAppError),
    Denied(GitHubDenial),
    Invalid(String),
    CacheFull,
    Concurrency(String),
}

impl std::fmt::Display for GitHubAuthorityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => write!(formatter, "GitHub authority store failure: {error}"),
            Self::Lifecycle(error) => write!(formatter, "GitHub token lifecycle failure: {error}"),
            Self::Denied(denial) => write!(formatter, "GitHub authority denied: {denial:?}"),
            Self::Invalid(reason) => write!(formatter, "invalid GitHub authority result: {reason}"),
            Self::CacheFull => write!(formatter, "GitHub token cache is full"),
            Self::Concurrency(reason) => {
                write!(formatter, "GitHub authority concurrency failure: {reason}")
            }
        }
    }
}

impl std::error::Error for GitHubAuthorityError {}

impl From<GitHubStoreError> for GitHubAuthorityError {
    fn from(error: GitHubStoreError) -> Self {
        Self::Store(error)
    }
}

impl From<GitHubAppError> for GitHubAuthorityError {
    fn from(error: GitHubAppError) -> Self {
        match error {
            GitHubAppError::Denied(denial) => Self::Denied(denial),
            other => Self::Lifecycle(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TokenCacheKey {
    app_id: u64,
    installation_id: u64,
    operation: GitHubOperation,
    repository_ids: Vec<u64>,
    permissions: BTreeMap<super::GitHubPermission, super::AccessLevel>,
}

#[derive(Clone)]
struct CachedToken {
    generation: u64,
    token: InstallationAccessToken,
}

/// Production authority coordinator. Store transitions and token lifecycle
/// operations share one lane, so generation invalidation cannot race issuance,
/// refresh, or revocation.
pub struct GitHubAppAuthority<B, L> {
    store: GitHubAppStore<B>,
    lifecycle: L,
    cache: Mutex<BTreeMap<TokenCacheKey, CachedToken>>,
    transition: Mutex<()>,
    max_cached_tokens: usize,
    refresh_skew_seconds: u64,
}

impl<B, L> GitHubAppAuthority<B, L>
where
    B: super::GitHubAppStateBackend + Send + Sync,
    L: InstallationTokenLifecycle + Send + Sync,
{
    pub fn open(
        store: GitHubAppStore<B>,
        lifecycle: L,
        max_cached_tokens: usize,
        refresh_skew_seconds: u64,
    ) -> Result<Self, GitHubAuthorityError> {
        if max_cached_tokens == 0 {
            return Err(GitHubAuthorityError::Invalid(
                "max_cached_tokens must be non-zero".to_string(),
            ));
        }
        Ok(Self {
            store,
            lifecycle,
            cache: Mutex::new(BTreeMap::new()),
            transition: Mutex::new(()),
            max_cached_tokens,
            refresh_skew_seconds,
        })
    }

    pub fn install(
        &self,
        binding: InstallationBinding,
    ) -> Result<AuthorityOutcome<InstallationBinding>, GitHubAuthorityError> {
        let _transition = self.lock_transition()?;
        match self.store.binding(binding.app_id, binding.installation_id) {
            Ok(current) => {
                return Err(GitHubAuthorityError::Store(
                    GitHubStoreError::StaleGeneration {
                        installation_id: binding.installation_id,
                        current: current.generation,
                    },
                ));
            }
            Err(GitHubStoreError::NotFound { .. }) => {}
            Err(error) => return Err(error.into()),
        }
        let installed = self.store.replace_binding(binding)?;
        Ok(self.binding_outcome(installed, AuthorityAuditEvent::Installed))
    }

    pub fn replace(
        &self,
        binding: InstallationBinding,
    ) -> Result<AuthorityOutcome<InstallationBinding>, GitHubAuthorityError> {
        let _transition = self.lock_transition()?;
        let replaced = self.store.replace_binding(binding)?;
        self.invalidate_installation(replaced.app_id, replaced.installation_id)?;
        Ok(self.binding_outcome(replaced, AuthorityAuditEvent::Replaced))
    }

    pub fn revoke(
        &self,
        app_id: u64,
        installation_id: u64,
        expected_generation: Option<u64>,
    ) -> Result<AuthorityOutcome<InstallationBinding>, GitHubAuthorityError> {
        let _transition = self.lock_transition()?;
        let revoked = self
            .store
            .revoke_binding(app_id, installation_id, expected_generation)?;
        self.invalidate_installation(app_id, installation_id)?;
        Ok(self.binding_outcome(revoked, AuthorityAuditEvent::Revoked))
    }

    pub fn authorize(
        &self,
        app_id: u64,
        installation_id: u64,
        operation: GitHubOperation,
        repository_id: Option<u64>,
    ) -> Result<AuthorityOutcome<()>, GitHubAuthorityError> {
        let _transition = self.lock_transition()?;
        let binding = self.store.binding(app_id, installation_id)?;
        binding.authorize(operation, repository_id)?;
        Ok(AuthorityOutcome {
            value: (),
            audit: self.audit(
                AuthorityAuditEvent::Authorized,
                &binding,
                Some(operation),
                repository_id.into_iter().collect(),
            ),
        })
    }

    /// Issue the narrowest installation token supported by requested
    /// repository IDs. IDs are deduplicated and sorted by TokenRequest.
    pub fn issue_token(
        &self,
        app_id: u64,
        installation_id: u64,
        operation: GitHubOperation,
        repository_ids: &[u64],
        now_unix: u64,
    ) -> Result<AuthorityOutcome<InstallationAccessToken>, GitHubAuthorityError> {
        let _transition = self.lock_transition()?;
        let binding = self.store.binding(app_id, installation_id)?;
        let request = binding.token_request(operation, repository_ids)?;
        let key = TokenCacheKey::from_request(&request);

        if let Some(cached) = self.cached(&key)? {
            if cached.generation == binding.generation
                && !cached
                    .token
                    .needs_refresh(now_unix, self.refresh_skew_seconds)
            {
                return Ok(AuthorityOutcome {
                    value: cached.token,
                    audit: self.audit(
                        AuthorityAuditEvent::TokenCacheHit,
                        &binding,
                        Some(operation),
                        request.repository_ids.clone(),
                    ),
                });
            }
            self.remove_cached(&key)?;
            if cached.generation == binding.generation {
                let refreshed = self
                    .lifecycle
                    .refresh(&cached.token, &request, now_unix)
                    .map_err(GitHubAuthorityError::from)?;
                validate_adapter_token(&refreshed, &request, now_unix)?;
                self.insert_cached(
                    key,
                    CachedToken {
                        generation: binding.generation,
                        token: refreshed.clone(),
                    },
                )?;
                return Ok(AuthorityOutcome {
                    value: refreshed,
                    audit: self.audit(
                        AuthorityAuditEvent::TokenRefreshed,
                        &binding,
                        Some(operation),
                        request.repository_ids.clone(),
                    ),
                });
            }
        }

        let issued = self
            .lifecycle
            .issue(&request)
            .map_err(GitHubAuthorityError::from)?;
        validate_adapter_token(&issued, &request, now_unix)?;
        if let Err(error) = self.insert_cached(
            key,
            CachedToken {
                generation: binding.generation,
                token: issued.clone(),
            },
        ) {
            let _ = self.lifecycle.revoke(&issued);
            return Err(error);
        }
        Ok(AuthorityOutcome {
            value: issued,
            audit: self.audit(
                AuthorityAuditEvent::TokenIssued,
                &binding,
                Some(operation),
                request.repository_ids,
            ),
        })
    }

    /// Revoke cached token for exact authority identity, operation, and scope.
    /// Cache entry remains present when adapter revocation fails, permitting a
    /// safe retry without losing track of live credential material.
    pub fn revoke_token(
        &self,
        app_id: u64,
        installation_id: u64,
        operation: GitHubOperation,
        repository_ids: &[u64],
    ) -> Result<AuthorityOutcome<()>, GitHubAuthorityError> {
        let _transition = self.lock_transition()?;
        let binding = self.store.binding(app_id, installation_id)?;
        let request = binding.token_request(operation, repository_ids)?;
        let key = TokenCacheKey::from_request(&request);
        let cached = self
            .cached(&key)?
            .ok_or(GitHubAuthorityError::Denied(GitHubDenial::TokenMissing))?;
        self.lifecycle
            .revoke(&cached.token)
            .map_err(GitHubAuthorityError::from)?;
        self.remove_cached(&key)?;
        Ok(AuthorityOutcome {
            value: (),
            audit: self.audit(
                AuthorityAuditEvent::TokenRevoked,
                &binding,
                Some(operation),
                request.repository_ids,
            ),
        })
    }

    fn binding_outcome(
        &self,
        binding: InstallationBinding,
        event: AuthorityAuditEvent,
    ) -> AuthorityOutcome<InstallationBinding> {
        AuthorityOutcome {
            value: binding.clone(),
            audit: self.audit(event, &binding, None, Vec::new()),
        }
    }

    fn audit(
        &self,
        event: AuthorityAuditEvent,
        binding: &InstallationBinding,
        operation: Option<GitHubOperation>,
        repository_ids: Vec<u64>,
    ) -> AuthorityAudit {
        AuthorityAudit {
            event,
            app_id: binding.app_id,
            installation_id: binding.installation_id,
            generation: binding.generation,
            scope: operation.map(OperationRegistry::scope),
            operation,
            repository_ids,
        }
    }

    fn lock_transition(&self) -> Result<std::sync::MutexGuard<'_, ()>, GitHubAuthorityError> {
        self.transition
            .lock()
            .map_err(|_| GitHubAuthorityError::Concurrency("transition mutex poisoned".to_string()))
    }

    fn cached(&self, key: &TokenCacheKey) -> Result<Option<CachedToken>, GitHubAuthorityError> {
        self.cache
            .lock()
            .map(|cache| cache.get(key).cloned())
            .map_err(|_| {
                GitHubAuthorityError::Concurrency("token cache mutex poisoned".to_string())
            })
    }

    fn insert_cached(
        &self,
        key: TokenCacheKey,
        token: CachedToken,
    ) -> Result<(), GitHubAuthorityError> {
        let mut cache = self.cache.lock().map_err(|_| {
            GitHubAuthorityError::Concurrency("token cache mutex poisoned".to_string())
        })?;
        if !cache.contains_key(&key) && cache.len() >= self.max_cached_tokens {
            return Err(GitHubAuthorityError::CacheFull);
        }
        cache.insert(key, token);
        Ok(())
    }

    fn remove_cached(&self, key: &TokenCacheKey) -> Result<(), GitHubAuthorityError> {
        self.cache
            .lock()
            .map(|mut cache| {
                cache.remove(key);
            })
            .map_err(|_| {
                GitHubAuthorityError::Concurrency("token cache mutex poisoned".to_string())
            })
    }

    fn invalidate_installation(
        &self,
        app_id: u64,
        installation_id: u64,
    ) -> Result<(), GitHubAuthorityError> {
        self.cache
            .lock()
            .map(|mut cache| {
                cache.retain(|key, _| {
                    key.app_id != app_id || key.installation_id != installation_id
                });
            })
            .map_err(|_| {
                GitHubAuthorityError::Concurrency("token cache mutex poisoned".to_string())
            })
    }
}

impl TokenCacheKey {
    fn from_request(request: &TokenRequest) -> Self {
        Self {
            app_id: request.app_id,
            installation_id: request.installation_id,
            operation: request.operation,
            repository_ids: request.repository_ids.clone(),
            permissions: request.permissions.clone(),
        }
    }
}

fn validate_adapter_token(
    token: &InstallationAccessToken,
    request: &TokenRequest,
    now_unix: u64,
) -> Result<(), GitHubAuthorityError> {
    if !token.matches_request(request) {
        return Err(GitHubAuthorityError::Denied(
            GitHubDenial::TokenBindingMismatch,
        ));
    }
    if token.issued_at > now_unix {
        return Err(GitHubAuthorityError::Invalid(
            "adapter returned token issued in the future".to_string(),
        ));
    }
    if token.is_expired(now_unix) {
        return Err(GitHubAuthorityError::Denied(GitHubDenial::TokenExpired));
    }
    Ok(())
}
