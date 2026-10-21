//! One sign-in path for every Right Suite app: stored sign-in, refreshed when it expires, then
//! sign-ins other tools already hold, then a typed [`Resolution::NeedsSignIn`] the app turns
//! into its own prompt. Nothing here is interactive unless the caller starts a sign-in.
//!
//! Tokens live in the OS credential store through `rightkit-credentials`, in the app's own
//! namespace (each app signs in for itself). A refresh runs under a cross-process lock and
//! re-reads the store first, so two processes of the same app never spend one rotating
//! refresh grant twice.

use crate::token_codec::{decode, encode};
use std::fmt;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rightkit_http::Secret;

use crate::{DeviceAuthorization, DeviceWait, OAuthClient, OAuthError, TokenSet, TokenStore};

/// Refresh this long before the provider's expiry.
pub const DEFAULT_REFRESH_SKEW: Duration = Duration::from_secs(120);

/// [`TokenStore`] over the OS credential store (Keychain / Credential Manager).
pub struct SecretTokenStore {
    store: rightkit_credentials::Store,
}

impl SecretTokenStore {
    pub fn new(store: rightkit_credentials::Store) -> Self {
        Self { store }
    }
}

impl fmt::Debug for SecretTokenStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretTokenStore")
            .field("namespace", &self.store.namespace().service())
            .finish()
    }
}

fn storage(e: impl fmt::Display) -> OAuthError {
    OAuthError::Storage(e.to_string())
}

impl TokenStore for SecretTokenStore {
    fn load(&self, account: &str) -> Result<Option<TokenSet>, OAuthError> {
        match self.store.get(account).map_err(storage)? {
            None => Ok(None),
            Some(secret) => decode(secret.expose_str().map_err(storage)?).map(Some),
        }
    }

    fn save(&self, account: &str, tokens: &TokenSet) -> Result<(), OAuthError> {
        self.store
            .set_str(account, &encode(tokens))
            .map_err(storage)
    }

    fn delete(&self, account: &str) -> Result<(), OAuthError> {
        self.store.delete(account).map(|_| ()).map_err(storage)
    }
}

/// Where a resolved token came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// This app's stored sign-in, still valid.
    Stored,
    /// This app's stored sign-in, refreshed just now.
    Refreshed,
    /// Another tool's sign-in on this machine.
    #[cfg(feature = "system-sources")]
    System(crate::system::SystemSource),
}

/// The outcome of [`Resolver::resolve`].
pub enum Resolution {
    Token {
        token: Secret,
        source: Source,
    },
    /// Nothing usable: the app should offer sign-in. `device` says whether the provider
    /// supports device sign-in (codes for CLI/TUI or another device).
    NeedsSignIn {
        device: bool,
    },
}

impl fmt::Debug for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Resolution::Token { source, .. } => f
                .debug_struct("Token")
                .field("token", &"[redacted]")
                .field("source", source)
                .finish(),
            Resolution::NeedsSignIn { device } => f
                .debug_struct("NeedsSignIn")
                .field("device", device)
                .finish(),
        }
    }
}

/// Resolves one account's sign-in for one provider.
pub struct Resolver {
    client: OAuthClient,
    store: Arc<dyn TokenStore>,
    account: String,
    refresh_skew: Duration,
    lock_dir: PathBuf,
    #[cfg(feature = "system-sources")]
    system: Vec<crate::system::SystemSource>,
    #[cfg(feature = "system-sources")]
    system_timeout: Duration,
}

impl fmt::Debug for Resolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolver")
            .field("account", &self.account)
            .field("token_url", &self.client.config.token_url)
            .finish_non_exhaustive()
    }
}

impl Resolver {
    /// `account` names this sign-in inside the store (for example `github` or
    /// `google:user@example.com`).
    pub fn new(
        client: OAuthClient,
        store: Arc<dyn TokenStore>,
        account: impl Into<String>,
    ) -> Self {
        Self {
            client,
            store,
            account: account.into(),
            refresh_skew: DEFAULT_REFRESH_SKEW,
            lock_dir: std::env::temp_dir().join("rightkit-oauth"),
            #[cfg(feature = "system-sources")]
            system: Vec::new(),
            #[cfg(feature = "system-sources")]
            system_timeout: crate::system::DEFAULT_TIMEOUT,
        }
    }

    pub fn refresh_skew(mut self, skew: Duration) -> Self {
        self.refresh_skew = skew;
        self
    }

    /// Directory for the cross-process refresh locks (default: a temp subdirectory).
    pub fn lock_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.lock_dir = dir.into();
        self
    }

    /// Fall back to sign-ins other tools hold (e.g. [`crate::system::SystemSource::github`]).
    #[cfg(feature = "system-sources")]
    pub fn system_sources(
        mut self,
        sources: impl IntoIterator<Item = crate::system::SystemSource>,
        timeout: Duration,
    ) -> Self {
        self.system = sources.into_iter().collect();
        self.system_timeout = timeout;
        self
    }

    pub fn client(&self) -> &OAuthClient {
        &self.client
    }

    /// A usable token, or [`Resolution::NeedsSignIn`]. Never interactive.
    pub fn resolve(&self) -> Result<Resolution, OAuthError> {
        if let Some(tokens) = self.store.load(&self.account)? {
            if !tokens.is_expired(self.refresh_skew) {
                return Ok(token(&tokens, Source::Stored));
            }
            match self.refresh_locked() {
                Ok(Some(resolution)) => return Ok(resolution),
                Ok(None) => {}
                Err(OAuthError::NeedsReauth) => {}
                Err(e) => return Err(e),
            }
        }
        #[cfg(feature = "system-sources")]
        for source in &self.system {
            if let Some(found) = source.lookup(self.system_timeout) {
                return Ok(Resolution::Token {
                    token: found,
                    source: Source::System(source.clone()),
                });
            }
        }
        Ok(Resolution::NeedsSignIn {
            device: self.client.config.device_authorization_url.is_some(),
        })
    }

    /// Refresh under the cross-process lock. Re-reads the store first: another process may
    /// already have refreshed. A dead grant deletes the stored sign-in.
    fn refresh_locked(&self) -> Result<Option<Resolution>, OAuthError> {
        std::fs::create_dir_all(&self.lock_dir).map_err(storage)?;
        let key = format!("{}|{}", self.client.config.token_url, self.account);
        let lock_path = self.lock_dir.join(format!("{:016x}.lock", fnv1a(&key)));
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(storage)?;
        lock.lock().map_err(storage)?;
        let Some(current) = self.store.load(&self.account)? else {
            return Ok(None);
        };
        if !current.is_expired(self.refresh_skew) {
            return Ok(Some(token(&current, Source::Stored)));
        }
        match self.client.refresh(&current) {
            Ok(fresh) => {
                self.store.save(&self.account, &fresh)?;
                Ok(Some(token(&fresh, Source::Refreshed)))
            }
            Err(OAuthError::NeedsReauth) => {
                self.store.delete(&self.account)?;
                Err(OAuthError::NeedsReauth)
            }
            Err(e) => Err(e),
        }
    }

    /// Device sign-in (interactive: only when the app asks). `show` receives the code and
    /// page to present; the result is stored for this account.
    pub fn sign_in_device(
        &self,
        show: impl FnOnce(&DeviceAuthorization),
        wait: &DeviceWait,
    ) -> Result<Secret, OAuthError> {
        let authorization = self.client.begin_device()?;
        show(&authorization);
        let tokens = self.client.finish_device(&authorization, wait)?;
        self.store.save(&self.account, &tokens)?;
        Ok(tokens.access_token)
    }

    /// Store tokens obtained by another flow (e.g. loopback sign-in).
    pub fn save(&self, tokens: &TokenSet) -> Result<(), OAuthError> {
        self.store.save(&self.account, tokens)
    }

    pub fn sign_out(&self) -> Result<(), OAuthError> {
        self.store.delete(&self.account)
    }
}

fn token(tokens: &TokenSet, source: Source) -> Resolution {
    Resolution::Token {
        token: tokens.access_token.clone(),
        source,
    }
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn stored_form_round_trips_without_losing_expiry() {
        let tokens = TokenSet {
            access_token: Secret::new("a-1"),
            token_type: "bearer".into(),
            expires_in: Some(Duration::from_secs(3600)),
            refresh_token: Some(Secret::new("r-1")),
            scope: Some("repo".into()),
            id_token: Some(Secret::new("i.d.t")),
            obtained_at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        };
        let back = decode(&encode(&tokens)).unwrap();
        assert_eq!(back.id_token.as_ref().unwrap().expose(), "i.d.t");
        assert_eq!(back.access_token(), "a-1");
        assert_eq!(back.refresh_token.unwrap().expose(), "r-1");
        assert_eq!(back.expires_in, tokens.expires_in);
        assert_eq!(back.obtained_at, tokens.obtained_at);
        assert_eq!(back.scope.as_deref(), Some("repo"));
    }

    #[test]
    fn malformed_stored_expiry_refuses_instead_of_never_expiring() {
        for bad in [
            r#"{"access":"a-1","expires_in":"3600","obtained_at":1700000000}"#,
            r#"{"access":"a-1","expires_in":-5,"obtained_at":1700000000}"#,
            r#"{"access":"a-1","expires_in":36.5,"obtained_at":1700000000}"#,
            r#"{"access":"a-1","expires_in":3600,"obtained_at":"soon"}"#,
        ] {
            assert!(
                decode(bad).is_err(),
                "accepted malformed stored form: {bad}"
            );
        }
        let open =
            decode(r#"{"access":"a-1","expires_in":null,"obtained_at":1700000000}"#).unwrap();
        assert_eq!(open.expires_in, None);
    }

    #[test]
    fn resolutions_never_print_the_token() {
        let r = Resolution::Token {
            token: Secret::new("very-private-value"),
            source: Source::Stored,
        };
        assert!(!format!("{r:?}").contains("very-private-value"));
    }
}
