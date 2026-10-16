//! Provider presets for the PostRight engine: Mastodon (per-instance) and X (OAuth 2.0 PKCE),
//! plus Mastodon dynamic app registration (`POST {instance}/api/v1/apps`).
//!
//! Sources read 2026-10-16: docs.joinmastodon.org/methods/apps (registration request and
//! response) and docs.x.com OAuth 2.0 authorization-code page (token URL, client
//! authentication, `offline.access`). Everything else is marked UNVERIFIED where it appears.
use super::sensitive::SensitiveJson;
use super::{ClientAuth, ServerAuthError, ServerProvider};
use crate::OAuthConfig;
use rightkit_http::{Client, Request, RetryPolicy, Secret};
use std::fmt;

/// Default Mastodon scopes for a posting client. UNVERIFIED as a PostRight requirement;
/// callers pass their own via `begin(.., scopes)` or [`ServerProvider::with_default_scopes`].
pub const MASTODON_DEFAULT_SCOPES: &[&str] = &["read:accounts", "write:statuses", "write:media"];
/// X scopes for posting with a refresh token (`offline.access` is what makes X issue one).
pub const X_DEFAULT_SCOPES: &[&str] =
    &["tweet.read", "tweet.write", "users.read", "offline.access"];
/// Authorization endpoint from X's current OAuth 2.0 docs (docs.x.com). Override
/// `config.authorization_url` if an app registration still requires `twitter.com`.
pub const X_AUTHORIZATION_URL: &str = "https://x.com/i/oauth2/authorize";
pub const X_TOKEN_URL: &str = "https://api.x.com/2/oauth2/token";

/// Validate a Mastodon instance base URL and return `(origin, host_with_port)`. Only an
/// `https` origin is accepted: no credentials, path, query or fragment.
fn instance_origin(instance: &str) -> Result<(String, String), ServerAuthError> {
    let url = url::Url::parse(instance).map_err(|_| ServerAuthError::InvalidConfig)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ServerAuthError::InvalidConfig);
    }
    let host = url.host_str().unwrap_or_default();
    let authority = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    Ok((format!("https://{authority}"), authority))
}

impl ServerProvider {
    /// Mastodon on one instance. `instance` is the base URL (`https://mastodon.social`); the
    /// provider id is `mastodon:<host>` so several instances can share a flow. Endpoints are
    /// `/oauth/authorize` and `/oauth/token`; client credentials go in the token request body.
    /// Use [`register_mastodon_app`] to obtain `client_id` and `client_secret` per instance.
    /// PKCE S256 is sent; instances that predate PKCE support ignore it (UNVERIFIED: which
    /// Mastodon versions enforce it; the docs page read does not say).
    pub fn mastodon(
        instance: &str,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        redirect: impl Into<String>,
    ) -> Result<Self, ServerAuthError> {
        let (origin, authority) = instance_origin(instance)?;
        let mut config = OAuthConfig::new(
            client_id,
            format!("{origin}/oauth/authorize"),
            format!("{origin}/oauth/token"),
        )
        .with_client_secret(client_secret);
        config.scopes = MASTODON_DEFAULT_SCOPES
            .iter()
            .map(|s| s.to_string())
            .collect();
        Ok(Self::new(format!("mastodon:{authority}"), config, redirect))
    }

    /// X OAuth 2.0 authorization code with PKCE for a confidential client (web app or
    /// automated app): HTTP Basic client authentication at the token endpoint, as the X
    /// docs describe ("valid Authorization header"). Refresh tokens on X are issued only with
    /// the `offline.access` scope (included by default). Whether X rotates the refresh token on
    /// every refresh is not stated on the page read (the PostRight request says it does);
    /// `refresh` persists a returned replacement either way.
    pub fn x_confidential(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        redirect: impl Into<String>,
    ) -> Self {
        let mut p = Self::x_base(client_id, redirect);
        p.config.client_secret = Some(Secret::new(client_secret));
        p.client_auth = ClientAuth::SecretBasic;
        p
    }

    /// X public client (native or single-page app): no secret, `client_id` in the token body,
    /// PKCE required.
    pub fn x_public(client_id: impl Into<String>, redirect: impl Into<String>) -> Self {
        let mut p = Self::x_base(client_id, redirect);
        p.client_auth = ClientAuth::None;
        p
    }

    fn x_base(client_id: impl Into<String>, redirect: impl Into<String>) -> Self {
        let mut config = OAuthConfig::new(client_id, X_AUTHORIZATION_URL, X_TOKEN_URL);
        config.scopes = X_DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect();
        Self::new("x", config, redirect)
    }

    /// Replace the descriptor's default scopes (used when `begin` is given none).
    pub fn with_default_scopes(
        mut self,
        scopes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.config.scopes = scopes.into_iter().map(Into::into).collect();
        self
    }
}

fn field<'a>(value: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    value
        .get(name)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// An OAuth app registered on a Mastodon instance.
pub struct RegisteredApp {
    pub instance: String,
    pub client_id: String,
    pub client_secret: Secret,
    pub redirect_uris: Vec<String>,
}

impl fmt::Debug for RegisteredApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RegisteredApp { [redacted] }")
    }
}

/// Dynamic app registration: `POST {instance}/api/v1/apps` with a JSON body
/// (`client_name`, `redirect_uris` array, space-separated `scopes`, optional `website`), the
/// documented example form. Returns `client_id` and `client_secret`; persist them (the secret
/// in the OS credential store) and build the provider with [`ServerProvider::mastodon`]. The
/// secret is returned once per registration, so register once per instance, not per sign-in.
/// No retry. Error values carry the HTTP status only, never the body.
pub fn register_mastodon_app(
    http: &Client,
    instance: &str,
    client_name: &str,
    redirect_uris: &[&str],
    scopes: &[&str],
    website: Option<&str>,
) -> Result<RegisteredApp, ServerAuthError> {
    let (origin, _) = instance_origin(instance)?;
    register_at(http, &origin, client_name, redirect_uris, scopes, website)
}

/// Registration against an already validated origin (split out so tests can target a local
/// plain-HTTP mock; the public entry point only accepts `https`).
fn register_at(
    http: &Client,
    origin: &str,
    client_name: &str,
    redirect_uris: &[&str],
    scopes: &[&str],
    website: Option<&str>,
) -> Result<RegisteredApp, ServerAuthError> {
    if client_name.trim().is_empty()
        || redirect_uris.is_empty()
        || redirect_uris.iter().any(|u| u.is_empty())
        || scopes
            .iter()
            .any(|s| s.is_empty() || s.chars().any(char::is_whitespace))
    {
        return Err(ServerAuthError::InvalidConfig);
    }
    let mut body = serde_json::json!({
        "client_name": client_name,
        "redirect_uris": redirect_uris,
    });
    if !scopes.is_empty() {
        body["scopes"] = scopes.join(" ").into();
    }
    if let Some(site) = website {
        body["website"] = site.into();
    }
    let request = Request::post(format!("{origin}/api/v1/apps"))
        .header("Accept", "application/json")
        .retry(RetryPolicy::none())
        .json(&body)
        .map_err(|_| ServerAuthError::InvalidConfig)?;
    let response = http
        .send_any(&request)
        .map_err(|_| ServerAuthError::RegistrationFailure { status: None })?;
    let status = response.status;
    let fail = || ServerAuthError::RegistrationFailure {
        status: Some(status),
    };
    if !(200..300).contains(&status) {
        return Err(fail());
    }
    let value = SensitiveJson::parse(&response.body).map_err(|_| fail())?;
    let client_id = field(&value, "client_id").ok_or_else(fail)?.to_string();
    let client_secret = Secret::new(field(&value, "client_secret").ok_or_else(fail)?);
    let returned: Vec<String> = match value.get("redirect_uris").and_then(|v| v.as_array()) {
        Some(list) => list
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        // Deprecated single-value field on Mastodon before 4.3.
        None => field(&value, "redirect_uri")
            .map(|s| vec![s.to_string()])
            .unwrap_or_default(),
    };
    Ok(RegisteredApp {
        instance: origin.to_string(),
        client_id,
        client_secret,
        redirect_uris: returned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
    use serde_json::json;

    #[test]
    fn registration_posts_documented_json_and_returns_credentials() {
        let server = MockServer::start(|_, _| {
            MockResponse::json(
                200,
                &json!({"id":"1","name":"PostRight","client_id":"cid","client_secret":"csecret",
                        "redirect_uris":["https://app.example/cb"],"scopes":["write:statuses"]}),
            )
        });
        let http = Client::new(loopback_config());
        let app = register_at(
            &http,
            &server.base_url(),
            "PostRight",
            &["https://app.example/cb"],
            &["write:statuses", "read:accounts"],
            Some("https://postright.example"),
        )
        .unwrap();
        assert_eq!(app.client_id, "cid");
        assert_eq!(app.client_secret.expose(), "csecret");
        assert_eq!(
            app.redirect_uris,
            vec!["https://app.example/cb".to_string()]
        );
        assert!(!format!("{app:?}").contains("csecret"));
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path(), "/api/v1/apps");
        let body = requests[0].body_json();
        assert_eq!(body["client_name"], "PostRight");
        assert_eq!(body["redirect_uris"], json!(["https://app.example/cb"]));
        assert_eq!(body["scopes"], "write:statuses read:accounts");
        assert_eq!(body["website"], "https://postright.example");
    }

    #[test]
    fn registration_failures_carry_status_only() {
        for (status, body) in [
            (422, json!({"error":"Validation failed: csecret"})),
            (200, json!({"client_id":"cid"})),
            (200, json!({"client_secret":"csecret"})),
        ] {
            let server = MockServer::start(move |_, _| MockResponse::json(status, &body));
            let http = Client::new(loopback_config());
            let err = register_at(
                &http,
                &server.base_url(),
                "App",
                &["https://a.example/cb"],
                &[],
                None,
            )
            .unwrap_err();
            assert!(
                matches!(err, ServerAuthError::RegistrationFailure { status: Some(s) } if s == status)
            );
            assert!(!format!("{err} {err:?}").contains("csecret"));
            assert_eq!(server.requests().len(), 1, "never retried");
        }
    }

    #[test]
    fn registration_rejects_bad_input_before_any_request() {
        let http = Client::new(loopback_config());
        for instance in [
            "http://mastodon.example",
            "https://user:pw@mastodon.example",
            "https://mastodon.example/path",
            "https://mastodon.example/?q=1",
            "https://mastodon.example/#f",
            "not a url",
        ] {
            assert!(matches!(
                register_mastodon_app(&http, instance, "App", &["https://a.example/cb"], &[], None),
                Err(ServerAuthError::InvalidConfig)
            ));
        }
        assert!(matches!(
            register_mastodon_app(
                &http,
                "https://mastodon.example",
                "",
                &["https://a.example/cb"],
                &[],
                None
            ),
            Err(ServerAuthError::InvalidConfig)
        ));
        assert!(matches!(
            register_mastodon_app(&http, "https://mastodon.example", "App", &[], &[], None),
            Err(ServerAuthError::InvalidConfig)
        ));
    }
}
