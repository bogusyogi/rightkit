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
pub struct AccessTokens {
    resolver: Resolver,
    store: Arc<MemoryTokenStore>,
    account: String,
    refresh_token: Secret,
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
        let store = Arc::new(MemoryTokenStore::new());
        let client = google_client(client_id, client_secret)?;
        let resolver = Resolver::new(client, store.clone(), account);
        Ok(Self {
            resolver,
            store,
            account: account.to_string(),
            refresh_token: Secret::new(refresh_token),
        })
    }

    /// Store a token that is already expired and carries only the refresh token, so the
    /// next resolve refreshes it.
    fn seed(&self) -> Result<(), OAuthError> {
        self.store.save(
            &self.account,
            &TokenSet {
                access_token: Secret::new(""),
                token_type: "Bearer".to_string(),
                expires_in: Some(Duration::ZERO),
                refresh_token: Some(self.refresh_token.clone()),
                scope: None,
                id_token: None,
                obtained_at: SystemTime::now(),
            },
        )
    }

    /// A usable access token: the stored one while fresh, else a refreshed one.
    pub fn access_token(&self) -> Result<String, OAuthError> {
        // A refresh that found the grant dead deletes the entry; seed again so the next
        // call reports it afresh rather than as a missing sign-in.
        if self.store.load(&self.account)?.is_none() {
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
        self.seed()
    }
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

#[cfg(test)]
mod tests {
    #[test]
    fn scope_set_splits_into_individual_google_scopes() {
        let scopes: Vec<&str> = super::SCOPE.split(' ').collect();
        assert!(scopes.iter().all(|s| !s.is_empty()));
        assert_eq!(scopes[0], "openid");
        assert!(scopes.contains(&"https://www.googleapis.com/auth/gmail.modify"));
    }

    #[test]
    fn scopes_make_the_id_token_carry_the_email() {
        // The account email is read from the ID token: `openid` yields the token and
        // `userinfo.email` puts the email claim in it.
        let scopes: Vec<&str> = super::SCOPE.split(' ').collect();
        assert!(scopes.contains(&"openid"));
        assert!(scopes.contains(&"https://www.googleapis.com/auth/userinfo.email"));
    }
}
