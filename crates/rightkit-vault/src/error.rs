//! Error taxonomy. No variant carries key material, tokens or secret values, and no message
//! quotes a parser's text (serde messages can echo the value they choked on).

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The ID token was rejected (401). The caller refreshes it and retries.
    #[error("id token rejected by the entitlement service")]
    Unauthorized,
    /// The service withholds the server half (403): the subject is not entitled (Pro lapsed,
    /// not subscribed). The cached server key has been cleared. The blob is untouched and is
    /// readable again if entitlement returns and the service still holds the same key.
    #[error("entitlement service denies the wrapping key (not entitled)")]
    Denied,
    /// The service says the vault was erased (410 `vault_revoked`, after an account delete or
    /// an explicit erase). The cached server key has been cleared and the blob is unreadable
    /// by design. `Vault::reset` starts over with an empty vault.
    #[error("vault erased on the entitlement service")]
    Revoked,
    /// No usable cached server key (none, or past the offline grace) and the service is
    /// unreachable.
    #[error("offline: wrapping key unavailable")]
    Offline,
    #[error("entitlement service error: {0}")]
    Service(String),
    #[error("blob store error: {0}")]
    Store(String),
    #[error("server key cache error: {0}")]
    Cache(String),
    #[error("client secret store error: {0}")]
    ClientSecret(String),
    /// A blob exists but this device holds no client secret for it (a new device that has not
    /// received it). Nothing was changed.
    #[error("no client secret on this device for the existing vault")]
    ClientSecretMissing,
    /// Authentication failed: wrong server key or client secret, wrong subject/app, or a
    /// tampered blob.
    #[error("blob failed authentication")]
    Decrypt,
    /// The blob names a wrapping key id the service no longer serves (retired before this
    /// blob was re-wrapped).
    #[error("blob is sealed under a wrapping key the service no longer serves")]
    KeyUnavailable,
    #[error("malformed data: {0}")]
    Format(String),
    #[error("unsupported blob version {0}")]
    Version(u32),
    #[error("entropy source failed")]
    Entropy,
    #[error("cipher failure")]
    Crypto,
}

pub type Result<T> = std::result::Result<T, Error>;
