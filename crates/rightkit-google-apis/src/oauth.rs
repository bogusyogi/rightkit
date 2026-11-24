// Desktop OAuth for Google through RightKit's `rightkit-oauth`: PKCE loopback
// sign-in in the user's default browser (CSPRNG verifier + state, S256,
// 127.0.0.1:<ephemeral>), the account email read from the sign-in's ID token, and
// access-token resolution from a stored refresh token. The host app owns only the
// Workspace scope set registered in its Cloud project; the refresh token goes to
// the OS credential store.

use rightkit_http::{Client, ClientConfig, Secret};
use rightkit_oauth::browser::SystemBrowser;
use rightkit_oauth::credentials::{Resolution, Resolver};
use rightkit_oauth::google::GoogleIdVerifier;
use rightkit_oauth::{
    MemoryTokenStore, OAuthClient, OAuthConfig, OAuthError, TokenSet, TokenStore,
};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// The Google Workspace scope set a sign-in requests (space separated).
pub const SCOPE: &str = concat!(
    "openid",
    " https://www.googleapis.com/auth/userinfo.email",
    " https://www.googleapis.com/auth/userinfo.profile",
    " https://www.googleapis.com/auth/gmail.modify",
    " https://www.googleapis.com/auth/gmail.settings.basic",
    " https://www.googleapis.com/auth/calendar.events",
    " https://www.googleapis.com/auth/calendar.calendarlist.readonly",
    " https://www.googleapis.com/auth/calendar.freebusy",
    // Writable contacts (read + create/update/delete) for import/merge; the
    // readonly scope it replaces is a subset. Existing accounts re-consent on
    // their next reconnect — until then write ops 403 with a clear error.
    " https://www.googleapis.com/auth/contacts",
    " https://www.googleapis.com/auth/contacts.other.readonly",
    " https://www.googleapis.com/auth/directory.readonly",
    " https://www.googleapis.com/auth/meetings.space.created",
    " https://www.googleapis.com/auth/meetings.space.settings",
    " https://www.googleapis.com/auth/meetings.space.readonly",
    " https://www.googleapis.com/auth/tasks",
    // Full Drive scope (not drive.readonly/drive.file) is deliberate: the Drive
    // view supports move/copy/rename/share across the user's existing folders,
    // which the app-scoped drive.file grant cannot reach. Least-privilege would
    // narrow this only if those cross-folder operations are dropped.
    " https://www.googleapis.com/auth/drive",
    // The app's own hidden Drive folder (`appDataFolder`), for sync through the owner's own Drive
    // (rightkit-sync's `drive` backend). It reaches nothing else in Drive. Existing accounts re-consent on
    // their next reconnect: until then calls against the app data folder 403, and nothing else changes.
    " https://www.googleapis.com/auth/drive.appdata",
);
/// Budget for the user to finish consent in the browser.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

pub struct OauthResult {
    pub email: String,
    pub refresh_token: String,
}

/// Google OAuth client for the host app's registered desktop app.
fn google_client(client_id: &str, client_secret: &str) -> Result<OAuthClient, OAuthError> {
    let config = OAuthConfig::google(client_id, SCOPE.split(' ')).with_client_secret(client_secret);
    OAuthClient::new(config, Client::new(ClientConfig::default()))
}

/// One account's access token, resolved by rightkit-oauth: reused until it nears expiry,
/// refreshed under the resolver's lock so concurrent callers coalesce behind one refresh,
/// and a revoked or expired grant surfaces as `OAuthError::NeedsReauth`. Blocking: call
/// from `spawn_blocking`.
///
/// Two modes. [`AccessTokens::new`] holds a refresh token the caller passes in and keeps it
/// in memory only. [`AccessTokens::with_store`] reads, refreshes and saves the grant in a
/// caller-supplied [`TokenStore`], so the refresh token lives where the caller keeps it
/// (for example a sealed file store) and is never passed through this crate's arguments.
/// Neither mode shows a token in `Debug`, logs or error text.
pub struct AccessTokens {
    resolver: Resolver,
    store: Arc<dyn TokenStore>,
    account: String,
    /// `Some` in the in-memory mode, which re-seeds the store from it; `None` when the
    /// caller's store is the only home of the grant.
    refresh_token: Option<Secret>,
}

impl AccessTokens {
    /// `account` names this grant inside the store and the resolver's lock; it must not
    /// carry secrets.
    pub fn new(
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
        account: &str,
    ) -> Result<Self, OAuthError> {
        let store: Arc<dyn TokenStore> = Arc::new(MemoryTokenStore::new());
        let client = google_client(client_id, client_secret)?;
        let resolver = Resolver::new(client, store.clone(), account);
        Ok(Self {
            resolver,
            store,
            account: account.to_string(),
            refresh_token: Some(Secret::new(refresh_token)),
        })
    }

    /// Resolve access tokens from a grant held in the caller's `store` under `account`.
    ///
    /// The store must already hold the grant (see [`provision_refresh_token`]); this
    /// constructor reads nothing. A missing entry, or a grant Google has revoked, resolves
    /// to `OAuthError::NeedsReauth` and is never retried here. A refresh saves the fresh
    /// token set back to the store, and a revoked grant deletes the entry (the resolver's
    /// rule), so the next call reports `NeedsReauth` again instead of looping.
    /// `account` names the grant in the store and the resolver's lock; it must not carry
    /// secrets.
    pub fn with_store(
        client_id: &str,
        client_secret: &str,
        account: &str,
        store: Arc<dyn TokenStore>,
    ) -> Result<Self, OAuthError> {
        let client = google_client(client_id, client_secret)?;
        let resolver = Resolver::new(client, store.clone(), account);
        Ok(Self {
            resolver,
            store,
            account: account.to_string(),
            refresh_token: None,
        })
    }

    /// Store a token that is already expired and carries only the refresh token, so the
    /// next resolve refreshes it. In-memory mode only.
    fn seed(&self) -> Result<(), OAuthError> {
        match &self.refresh_token {
            Some(refresh_token) => self
                .store
                .save(&self.account, &expired_token_set(refresh_token.clone())),
            None => Ok(()),
        }
    }

    /// A usable access token: the stored one while fresh, else a refreshed one.
    pub fn access_token(&self) -> Result<String, OAuthError> {
        // A refresh that found the grant dead deletes the entry; seed again so the next
        // call reports it afresh rather than as a missing sign-in.
        // A caller's store is never re-seeded: an absent entry there means "sign in again".
        if self.refresh_token.is_some() && self.store.load(&self.account)?.is_none() {
            self.seed()?;
        }
        match self.resolver.resolve()? {
            Resolution::Token { token, .. } => Ok(token.expose().to_string()),
            Resolution::NeedsSignIn { .. } => Err(OAuthError::NeedsReauth),
        }
    }

    /// Discard the held access token (the provider rejected it); the next
    /// `access_token` refreshes.
    pub fn invalidate(&self) -> Result<(), OAuthError> {
        if self.refresh_token.is_some() {
            return self.seed();
        }
        // Caller's store: keep the grant, drop only the access token, so the next resolve
        // refreshes. An absent entry stays absent.
        if let Some(mut tokens) = self.store.load(&self.account)? {
            tokens.access_token = Secret::new("");
            tokens.expires_in = Some(Duration::ZERO);
            tokens.obtained_at = SystemTime::now();
            self.store.save(&self.account, &tokens)?;
        }
        Ok(())
    }
}

/// A token set that is already expired and carries only a refresh token.
fn expired_token_set(refresh_token: Secret) -> TokenSet {
    TokenSet {
        access_token: Secret::new(""),
        token_type: "Bearer".to_string(),
        expires_in: Some(Duration::ZERO),
        refresh_token: Some(refresh_token),
        scope: None,
        id_token: None,
        obtained_at: SystemTime::now(),
    }
}

/// Write a refresh token into the caller's `store` under `account` as an expired grant, so
/// the first access-token request through [`AccessTokens::with_store`] (or
/// `Google::with_store`) refreshes it. This is how a sign-in's refresh token (see
/// [`run_flow`]) reaches a store on another host. It replaces any existing entry for
/// `account`. The token is not logged or returned.
pub fn provision_refresh_token(
    store: &dyn TokenStore,
    account: &str,
    refresh_token: &str,
) -> Result<(), OAuthError> {
    store.save(account, &expired_token_set(Secret::new(refresh_token)))
}

/// A sealed, encrypted file store for refresh tokens, over `rightkit-oauth`'s
/// `SealedFileTokenStore`, ready for `AccessTokens::with_store` and `Google::with_store`.
///
/// `key` is 32 random bytes the caller obtains from a credential its service manager
/// provides (for example a systemd credential); it must never be read from a file beside
/// `root`. `root` is a private (0700) directory on a local Unix filesystem; on Windows this
/// fails closed. Grants are stored under the provider name `"google"`. Feature
/// `sealed-store`.
#[cfg(feature = "sealed-store")]
pub fn sealed_file_store(
    root: impl AsRef<std::path::Path>,
    key: [u8; 32],
) -> Result<Arc<dyn TokenStore>, OAuthError> {
    use rightkit_oauth::server::{SealedFileTokenStore, ServerStoreAdapter};
    let unavailable = || OAuthError::Storage("token store unavailable".to_string());
    let sealed = SealedFileTokenStore::new(root, key).map_err(|_| unavailable())?;
    let adapter = ServerStoreAdapter::new(sealed, "google").map_err(|_| unavailable())?;
    Ok(Arc::new(adapter))
}

/// Run the full interactive PKCE loopback flow in the default browser. Returns the
/// refresh token + the account email. Does NOT touch the DB or credential store — the
/// caller persists.
pub async fn run_flow(client_id: String, client_secret: String) -> Result<OauthResult, String> {
    let (tokens, email) =
        tokio::task::spawn_blocking(move || -> Result<(TokenSet, String), String> {
            let client = google_client(&client_id, &client_secret).map_err(|e| e.to_string())?;
            let tokens = client
                .sign_in_with_browser(&SystemBrowser, CONSENT_TIMEOUT)
                .map_err(|e| match e {
                    OAuthError::StateMismatch => {
                        "OAuth state mismatch — possible CSRF, aborted".to_string()
                    }
                    other => other.to_string(),
                })?;
            let email = email_from_sign_in(&client_id, &tokens)?;
            Ok((tokens, email))
        })
        .await
        .map_err(|e| format!("OAuth task failed: {e}"))??;

    let refresh_token = tokens
        .refresh_token
        .as_ref()
        .map(|token| token.expose().to_string())
        .ok_or_else(|| {
            "no refresh token returned (re-consent required: revoke at myaccount.google.com)"
                .to_string()
        })?;

    Ok(OauthResult {
        email,
        refresh_token,
    })
}

/// The account email, from the ID token Google returned in this sign-in's own code
/// exchange (the `openid` + `userinfo.email` scopes make it carry the `email` claim).
fn email_from_sign_in(client_id: &str, tokens: &TokenSet) -> Result<String, String> {
    let id_token = tokens
        .id_token
        .as_ref()
        .ok_or("sign-in returned no ID token (re-consent required)")?;
    let verifier = GoogleIdVerifier::new(Client::new(ClientConfig::default()), [client_id])
        .map_err(|e| e.to_string())?;
    verifier
        .read_trusted(id_token.expose())
        .map_err(|e| format!("read account identity: {e}"))?
        .email
        .filter(|email| !email.is_empty())
        .ok_or_else(|| "sign-in identity carries no email address".to_string())
}
