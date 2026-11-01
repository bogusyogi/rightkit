use std::collections::HashMap;
#[cfg(feature = "agent_mcp-os-keyring")]
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(feature = "agent_mcp-os-keyring")]
use rightkit_oauth::browser::{BrowserOpener, SystemBrowser};
use serde::{Deserialize, Serialize};

#[cfg(feature = "agent_mcp-os-keyring")]
use crate::agent_mcp::error::McpError;
#[cfg(feature = "agent_mcp-os-keyring")]
use crate::agent_mcp::types::McpOAuthConfig;

const KEYRING_SERVICE: &str = "rightkit-agent";
const REFRESH_SKEW_SECONDS: u64 = 60;
#[cfg(feature = "agent_mcp-os-keyring")]
const OAUTH_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(feature = "agent_mcp-os-keyring")]
const OAUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(feature = "agent_mcp-os-keyring")]
const OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[cfg(feature = "agent_mcp-os-keyring")]
fn oauth_transport_error(error: rightkit_oauth::OAuthError) -> McpError {
    McpError::Transport(error.to_string())
}

/// RightKit OAuth client for one MCP server's endpoints. The RFC 8707
/// `resource` and the joined `scope` ride on the code exchange and refresh
/// bodies; the authorization request carries `resource` and the scopes.
/// RightKit's client is blocking: run its calls on `spawn_blocking`.
#[cfg(feature = "agent_mcp-os-keyring")]
fn rightkit_oauth_client(
    oauth: &McpOAuthConfig,
    with_revocation: bool,
) -> Result<rightkit_oauth::OAuthClient, McpError> {
    let mut config = rightkit_oauth::OAuthConfig::new(
        oauth.client_id.clone(),
        oauth.authorization_url.clone(),
        oauth.token_url.clone(),
    )
    // Single-use loopback listener: ephemeral 127.0.0.1 port, exact path.
    .with_redirect("127.0.0.1", 0, "/callback")
    .with_authorization_param("resource", oauth.resource.clone());
    config.scopes = oauth.scopes.clone();

    let http = rightkit_http::ClientConfig {
        connect_timeout: Some(OAUTH_CONNECT_TIMEOUT),
        request_timeout: OAUTH_REQUEST_TIMEOUT,
        // OAuth endpoints are credential boundaries; never follow a redirect
        // carrying client credentials or refresh tokens.
        max_redirects: 0,
        // Token endpoints on loopback or an RFC 1918 network (a local or
        // corporate MCP gateway) stay reachable; link-local and metadata
        // addresses do not.
        address_policy: rightkit_http::AddressPolicy::public_web().local_first(),
        ..rightkit_http::ClientConfig::default()
    };

    let mut client = rightkit_oauth::OAuthClient::new(config, rightkit_http::Client::new(http))
        .map_err(oauth_transport_error)?
        .with_token_param("resource", oauth.resource.clone())
        .map_err(oauth_transport_error)?;
    if !oauth.scopes.is_empty() {
        client = client
            .with_token_param("scope", oauth.scopes.join(" "))
            .map_err(oauth_transport_error)?;
    }
    if with_revocation {
        if let Some(url) = oauth.revocation_url.as_deref() {
            client = client
                .with_revocation_url(url)
                .map_err(oauth_transport_error)?;
        }
    }
    Ok(client)
}

#[cfg(feature = "agent_mcp-os-keyring")]
fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// the host app's stored token record from a RightKit token set.
#[cfg(feature = "agent_mcp-os-keyring")]
fn mcp_tokens(
    tokens: &rightkit_oauth::TokenSet,
    resource: &str,
    token_url: &str,
) -> McpOAuthTokens {
    McpOAuthTokens {
        access_token: tokens.access_token().to_string(),
        refresh_token: tokens
            .refresh_token
            .as_ref()
            .map(|token| token.expose().to_string()),
        token_type: tokens.token_type.clone(),
        expires_at_unix: tokens
            .expires_in
            .map(|ttl| unix_secs(tokens.obtained_at).saturating_add(ttl.as_secs())),
        scope: tokens.scope.clone(),
        resource: Some(resource.to_string()),
        token_url: Some(token_url.to_string()),
    }
}

/// `scheme://host[:port]` of a URL, lower-cased with the scheme's default port dropped.
/// `None` when the text is not an absolute http(s) URL.
pub fn url_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    // Credentials in the authority never take part in an origin.
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if host_port.is_empty() {
        return None;
    }
    let host_port = host_port.to_ascii_lowercase();
    let default = if scheme == "https" { ":443" } else { ":80" };
    let host_port = host_port.strip_suffix(default).unwrap_or(&host_port);
    Some(format!("{scheme}://{host_port}"))
}

fn same_origin(a: &str, b: &str) -> bool {
    matches!((url_origin(a), url_origin(b)), (Some(x), Some(y)) if x == y)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default = "default_token_type")]
    pub token_type: String,
    #[serde(default)]
    pub expires_at_unix: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
    /// The resource (the MCP server URL) the token was issued for (RFC 8707).
    #[serde(default)]
    pub resource: Option<String>,
    /// The token endpoint the token was issued by; the only endpoint it may be refreshed at.
    #[serde(default)]
    pub token_url: Option<String>,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

impl McpOAuthTokens {
    /// Contract R2: a stored token may be sent only to the origin it was issued for. A token
    /// with no recorded resource is never attached.
    pub fn issued_for_server(&self, server_url: &str) -> bool {
        self.resource
            .as_deref()
            .is_some_and(|resource| same_origin(resource, server_url))
    }

    /// Contract R2: a refresh goes only to the token endpoint that issued the token, for the
    /// resource it was issued for, whatever a later config file says.
    pub fn may_refresh_with(&self, resource: &str, token_url: &str) -> bool {
        self.resource
            .as_deref()
            .is_some_and(|issued| same_origin(issued, resource))
            && self
                .token_url
                .as_deref()
                .is_some_and(|issued| same_origin(issued, token_url))
    }

    pub fn should_refresh(&self, now_unix: u64) -> bool {
        self.refresh_token.is_some()
            && self
                .expires_at_unix
                .map(|expires| expires <= now_unix.saturating_add(REFRESH_SKEW_SECONDS))
                .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct McpOAuthLoginOptions {
    pub open_browser: bool,
}

#[derive(Debug, Clone)]
pub struct McpOAuthLoginResult {
    pub authorization_url: String,
    pub headless: bool,
    pub tokens: McpOAuthTokens,
}

#[derive(Debug, Clone)]
#[cfg(feature = "agent_mcp-os-keyring")]
pub struct McpOAuthCredentialStore;

#[cfg(feature = "agent_mcp-os-keyring")]
impl McpOAuthCredentialStore {
    pub fn get(server: &str) -> Result<Option<McpOAuthTokens>, McpError> {
        match oauth_store()?.get(&oauth_account(server)) {
            Ok(Some(secret)) => {
                let raw = secret
                    .expose_str()
                    .map_err(|e| McpError::Keyring(e.to_string()))?;
                serde_json::from_str::<McpOAuthTokens>(raw)
                    .map(Some)
                    .map_err(|e| McpError::Serde(e.to_string()))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(McpError::Keyring(e.to_string())),
        }
    }

    pub fn set(server: &str, tokens: &McpOAuthTokens) -> Result<(), McpError> {
        let raw = serde_json::to_string(tokens).map_err(|e| McpError::Serde(e.to_string()))?;
        // Saved from an explicit user login, so macOS may show its prompt.
        oauth_store()?
            .interactive()
            .set_str(&oauth_account(server), &raw)
            .map_err(|e| McpError::Keyring(e.to_string()))
    }

    pub fn delete(server: &str) -> Result<(), McpError> {
        oauth_store()?
            .delete(&oauth_account(server))
            .map(|_| ())
            .map_err(|e| McpError::Keyring(e.to_string()))
    }
}

#[cfg(feature = "agent_mcp-os-keyring")]
fn oauth_account(server: &str) -> String {
    format!("mcp:{server}:oauth")
}

#[cfg(feature = "agent_mcp-os-keyring")]
fn oauth_store() -> Result<rightkit_credentials::Store, McpError> {
    let namespace = rightkit_credentials::Namespace::new(KEYRING_SERVICE)
        .map_err(|e| McpError::Keyring(e.to_string()))?;
    let disabled = std::env::var_os("RIGHTKIT_DISABLE_OS_KEYCHAIN").is_some_and(|v| v == "1");
    let config = rightkit_credentials::Config::new(namespace)
        .guard_when(disabled, rightkit_credentials::GuardMode::Deny)
        .timeout(Duration::from_secs(5));
    Ok(rightkit_credentials::Store::open(config))
}

#[cfg(feature = "agent_mcp-os-keyring")]
pub fn load_stored_oauth_tokens(
    server_names: impl IntoIterator<Item = String>,
) -> HashMap<String, McpOAuthTokens> {
    let mut tokens = HashMap::new();
    for server in server_names {
        match McpOAuthCredentialStore::get(&server) {
            Ok(Some(record)) => {
                tokens.insert(server, record);
            }
            Ok(None) => {}
            Err(e) => eprintln!("[mcp] failed to read OAuth tokens for '{server}': {e}"),
        }
    }
    tokens
}

#[cfg(feature = "agent_mcp-os-keyring")]
pub async fn refresh_oauth_tokens(
    oauth: &McpOAuthConfig,
    tokens: &McpOAuthTokens,
) -> Result<McpOAuthTokens, McpError> {
    let Some(refresh_token) = tokens.refresh_token.as_deref() else {
        return Ok(tokens.clone());
    };
    let client = rightkit_oauth_client(oauth, false)?;
    let current = rightkit_oauth::TokenSet {
        access_token: rightkit_http::Secret::new(tokens.access_token.clone()),
        token_type: tokens.token_type.clone(),
        expires_in: None,
        refresh_token: Some(rightkit_http::Secret::new(refresh_token.to_string())),
        scope: tokens.scope.clone(),
        id_token: None,
        obtained_at: SystemTime::now(),
    };
    // A response without a refresh token keeps the one sent.
    let fresh = tokio::task::spawn_blocking(move || client.refresh(&current))
        .await
        .map_err(|e| McpError::Transport(e.to_string()))?
        .map_err(oauth_transport_error)?;
    Ok(mcp_tokens(&fresh, &oauth.resource, &oauth.token_url))
}

#[cfg(feature = "agent_mcp-os-keyring")]
pub async fn login_oauth(
    oauth: &McpOAuthConfig,
    server: &str,
    options: McpOAuthLoginOptions,
) -> Result<MpOAuthLoginResultAlias, McpError> {
    let client = rightkit_oauth_client(oauth, false)?;
    // PKCE, state and the loopback listener are RightKit's: a wrong-state or
    // probe request cannot end the flow.
    let flow = client.begin_loopback().map_err(oauth_transport_error)?;
    let authorization_url = flow.request.authorization_url.clone();

    if options.open_browser {
        SystemBrowser
            .open(&authorization_url)
            .map_err(oauth_transport_error)?;
    } else {
        println!("{authorization_url}");
    }

    let token_set =
        tokio::task::spawn_blocking(move || client.finish_loopback(flow, OAUTH_CALLBACK_TIMEOUT))
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?
            .map_err(oauth_transport_error)?;
    let tokens = mcp_tokens(&token_set, &oauth.resource, &oauth.token_url);
    McpOAuthCredentialStore::set(server, &tokens)?;
    Ok(McpOAuthLoginResult {
        authorization_url,
        headless: !options.open_browser,
        tokens,
    })
}

type MpOAuthLoginResultAlias = McpOAuthLoginResult;

#[cfg(feature = "agent_mcp-os-keyring")]
pub async fn revoke_oauth_tokens(
    oauth: &McpOAuthConfig,
    tokens: &McpOAuthTokens,
) -> Result<(), McpError> {
    if oauth.revocation_url.is_none() {
        return Ok(());
    }
    let token = tokens
        .refresh_token
        .as_deref()
        .unwrap_or(tokens.access_token.as_str());
    let token = rightkit_http::Secret::new(token.to_string());
    let client = rightkit_oauth_client(oauth, true)?;
    tokio::task::spawn_blocking(move || client.revoke(&token, None))
        .await
        .map_err(|e| McpError::Transport(e.to_string()))?
        .map_err(oauth_transport_error)
}

pub fn detect_headless_from_env(env: &HashMap<String, String>) -> bool {
    env.get("SSH_TTY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
        || (cfg!(not(windows))
            && env
                .get("DISPLAY")
                .map(|v| v.trim().is_empty())
                .unwrap_or(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_tokens_with_refresh_token_should_refresh() {
        let tokens = McpOAuthTokens {
            access_token: "old".to_string(),
            refresh_token: Some("refresh".to_string()),
            token_type: "Bearer".to_string(),
            expires_at_unix: Some(100),
            scope: None,
            resource: None,
            token_url: None,
        };

        assert!(tokens.should_refresh(41));
        assert!(!tokens.should_refresh(1));
    }

    #[test]
    fn headless_detects_ssh_tty_and_missing_unix_display() {
        let env = HashMap::from([("SSH_TTY".to_string(), "/dev/pts/1".to_string())]);
        assert!(detect_headless_from_env(&env));

        if cfg!(windows) {
            assert!(!detect_headless_from_env(&HashMap::new()));
        } else {
            assert!(detect_headless_from_env(&HashMap::new()));
        }
    }

    #[cfg(feature = "agent_mcp-os-keyring")]
    fn oauth() -> McpOAuthConfig {
        McpOAuthConfig {
            authorization_url: "https://auth.example.com/authorize".to_string(),
            token_url: "https://auth.example.com/token".to_string(),
            revocation_url: Some("https://auth.example.com/revoke".to_string()),
            client_id: "rightkit-agent".to_string(),
            scopes: vec!["mcp:tools".to_string()],
            resource: "https://mcp.example.com/mcp".to_string(),
        }
    }

    #[cfg(feature = "agent_mcp-os-keyring")]
    #[test]
    fn authorization_url_includes_pkce_and_resource_indicator() {
        let flow = rightkit_oauth_client(&oauth(), false)
            .unwrap()
            .begin_loopback()
            .unwrap();
        let url = flow.request.authorization_url.clone();

        assert!(url.starts_with("https://auth.example.com/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("resource=https%3A%2F%2Fmcp.example.com%2Fmcp"));
        assert!(url.contains("scope=mcp%3Atools"));
        assert!(flow.request.redirect_uri.starts_with("http://127.0.0.1:"));
        assert!(flow.request.redirect_uri.ends_with("/callback"));
    }

    #[cfg(feature = "agent_mcp-os-keyring")]
    #[test]
    fn stored_tokens_carry_expiry_scope_and_resource() {
        let set = rightkit_oauth::TokenSet {
            access_token: rightkit_http::Secret::new("access"),
            token_type: "Bearer".to_string(),
            expires_in: Some(Duration::from_secs(3600)),
            refresh_token: Some(rightkit_http::Secret::new("refresh")),
            scope: Some("mcp:tools".to_string()),
            id_token: None,
            obtained_at: UNIX_EPOCH + Duration::from_secs(1_000),
        };
        let tokens = mcp_tokens(
            &set,
            "https://mcp.example.com/mcp",
            "https://auth.example.com/token",
        );
        assert_eq!(tokens.access_token, "access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(tokens.expires_at_unix, Some(4_600));
        assert_eq!(tokens.scope.as_deref(), Some("mcp:tools"));
        assert_eq!(
            tokens.resource.as_deref(),
            Some("https://mcp.example.com/mcp")
        );
        assert_eq!(
            tokens.token_url.as_deref(),
            Some("https://auth.example.com/token")
        );
    }
}
