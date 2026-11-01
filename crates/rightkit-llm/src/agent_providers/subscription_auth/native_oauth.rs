//! Reusable native provider authentication flows.
//!
//! OpenAI follows Pi's ChatGPT/Codex OAuth contract. Command Code follows
//! `pi-commandcode-provider`: browser-assisted API-key delivery with pasted
//! key fallback. Neither flow imports a foreign CLI credential store.

use std::collections::BTreeMap;
use std::sync::Arc;

use rightkit_http::{percent_decode, percent_encode as form_encode, CancelToken, Secret};
use rightkit_oauth::callback::{CallbackBind, CallbackListener, CallbackOptions, LoopbackHost};
use rightkit_oauth::pkce::{generate_state, Pkce};
use rightkit_oauth::{OAuthConfig, OAuthError, TokenSet};
use serde::Deserialize;

use crate::agent_providers::subscription_auth::eligibility::SubscriptionEligibility;
use crate::agent_providers::subscription_auth::types::{
    AuthError, AuthEvent, AuthInteraction, AuthPrompt, AuthSignal, BoxFuture, ModelAuth,
};

pub const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const OPENAI_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const OPENAI_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
pub const OPENAI_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
pub const OPENAI_CODEX_DEFAULT_MODEL: &str = "gpt-6.1-sol";
pub const OPENAI_SCOPE: &str = "openid profile email offline_access";
pub const OPENAI_JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
pub const OPENAI_CALLBACK_PORT: u16 = 1455;
pub const OPENAI_CALLBACK_PATH: &str = "/auth/callback";

pub const COMMAND_CODE_STUDIO_BASE: &str = "https://commandcode.ai";
pub const COMMAND_CODE_API_BASE: &str = "https://api.commandcode.ai";
pub const COMMAND_CODE_PROVIDER_BASE: &str = "https://api.commandcode.ai/provider/v1";
pub const COMMAND_CODE_AUTH_TIMEOUT_MS: u64 = 120_000;
pub const COMMAND_CODE_KEY_LIFETIME_MS: i64 = 10 * 365 * 24 * 60 * 60 * 1000;

/// RightKit OAuth configuration for the OpenAI / ChatGPT (Codex) sign-in, built from the
/// constants above. Used by `rightkit_oauth::credentials::Resolver` for the refresh; the
/// interactive login in this file keeps its own authorization-code exchange (P-4).
pub fn openai_oauth_config() -> OAuthConfig {
    let mut config = OAuthConfig::new(OPENAI_CLIENT_ID, OPENAI_AUTHORIZE_URL, OPENAI_TOKEN_URL)
        .with_redirect("localhost", OPENAI_CALLBACK_PORT, OPENAI_CALLBACK_PATH);
    config.scopes = OPENAI_SCOPE
        .split_whitespace()
        .map(str::to_string)
        .collect();
    config
}

/// RightKit OAuth configuration for Command Code. Command Code issues a long-lived API key,
/// not an OAuth grant: the stored token has no refresh token, so `Resolver` never calls the
/// token endpoint (a missing refresh token is "sign in again"). The client id and the token
/// URL below exist only to satisfy `OAuthClient::new` validation and are never contacted.
pub fn commandcode_oauth_config() -> OAuthConfig {
    OAuthConfig::new(
        "rightkit-agent-commandcode-api-key",
        format!("{COMMAND_CODE_STUDIO_BASE}/studio/auth/cli"),
        format!("{COMMAND_CODE_API_BASE}/oauth/token"),
    )
}

/// Interactive sign-in plus request-auth derivation for one subscription provider. Refresh,
/// storage and the cross-process refresh lock belong to `rightkit_oauth::credentials::Resolver`;
/// this trait owns only what is the host app-specific (the login UX and the provider's request
/// shape).
pub trait SubscriptionLogin: Send + Sync {
    /// Display name, e.g. "ChatGPT Plus/Pro (Codex Subscription)".
    fn name(&self) -> &str;

    /// Tri-state eligibility; only `Supported` may enable subscription inference in core.
    fn subscription(&self) -> SubscriptionEligibility;

    /// Selector label for the login option.
    fn login_label(&self) -> Option<&str> {
        None
    }

    /// Interactive login. Returns the tokens to persist (via `Resolver::save`).
    fn login<'a>(
        &'a self,
        interaction: &'a dyn AuthInteraction,
    ) -> BoxFuture<'a, Result<TokenSet, AuthError>>;

    /// Side-effect-free derivation of request auth from a valid access token.
    fn to_auth(&self, access_token: &str) -> Result<ModelAuth, AuthError>;
}

fn token_set(access: String, refresh: Option<String>, lifetime: std::time::Duration) -> TokenSet {
    TokenSet {
        access_token: Secret::new(access),
        token_type: "Bearer".to_string(),
        expires_in: Some(lifetime),
        refresh_token: refresh.filter(|value| !value.is_empty()).map(Secret::new),
        scope: None,
        // The id_token is deliberately dropped: it is large, the host app never reads it, and
        // the Windows credential store cannot hold it next to the other two values.
        id_token: None,
        obtained_at: std::time::SystemTime::now(),
    }
}

/// Form-encoded POST seam used by OpenAI's token endpoint.
pub trait FormPoster: Send + Sync {
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        body: BTreeMap<String, String>,
        signal: AuthSignal,
    ) -> BoxFuture<'a, Result<String, String>>;
}

/// Form POST through the shared provider HTTP client (`rightkit-http`): no redirects, bounded by
/// `timeout`.
pub struct HttpFormPoster {
    timeout: std::time::Duration,
}

impl Default for HttpFormPoster {
    fn default() -> Self {
        Self {
            timeout: std::time::Duration::from_secs(30),
        }
    }
}

impl FormPoster for HttpFormPoster {
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        body: BTreeMap<String, String>,
        signal: AuthSignal,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            signal.check().map_err(|error| error.message)?;
            let pairs: Vec<(&str, &str)> = body
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            let http = crate::agent_providers::http::provider_client_with_ceiling(
                url,
                crate::agent_providers::http::Route::Ambient,
                self.timeout,
            );
            let request = rightkit_http::Request::post(url.to_string())
                .header("Accept", "application/json")
                .form(&pairs);
            let response = http
                .send_any(&request)
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status;
            if !(200..300).contains(&status) {
                return Err(format!("HTTP request failed. status={status}; url={url}"));
            }
            Ok(response.text())
        })
    }
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

fn parse_token_response(body: &str, provider: &str) -> Result<OAuthTokenResponse, AuthError> {
    let token: OAuthTokenResponse = serde_json::from_str(body).map_err(|error| {
        AuthError::oauth(format!(
            "{provider} token response was invalid JSON: {error}"
        ))
    })?;
    if token.access_token.trim().is_empty()
        || token.refresh_token.trim().is_empty()
        || token.expires_in <= 0
    {
        return Err(AuthError::oauth(format!(
            "{provider} token response omitted required fields"
        )));
    }
    Ok(token)
}

fn parse_query(input: &str, name: &str) -> Option<String> {
    input
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or(input)
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(key, value)| (key == name).then(|| percent_decode(value)))
}

/// Parse a pasted redirect URL, query, `code#state`, or bare code.
pub fn parse_openai_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }
    if value.contains("#") {
        let (code, state) = value.split_once('#').unwrap_or((value, ""));
        return (
            (!code.is_empty()).then(|| code.trim().to_string()),
            (!state.is_empty()).then(|| state.trim().to_string()),
        );
    }
    if value.contains("code=") || value.contains("://") {
        return (parse_query(value, "code"), parse_query(value, "state"));
    }
    (Some(value.to_string()), None)
}

fn authorization_url(state: &str, challenge: &str, originator: &str) -> String {
    let params = [
        ("response_type", "code".to_string()),
        ("client_id", OPENAI_CLIENT_ID.to_string()),
        ("redirect_uri", OPENAI_REDIRECT_URI.to_string()),
        ("scope", OPENAI_SCOPE.to_string()),
        ("code_challenge", challenge.to_string()),
        ("code_challenge_method", "S256".to_string()),
        ("state", state.to_string()),
        ("id_token_add_organizations", "true".to_string()),
        ("codex_cli_simplified_flow", "true".to_string()),
        ("originator", originator.to_string()),
    ];
    let query = params
        .iter()
        .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{OPENAI_AUTHORIZE_URL}?{query}")
}

// RIGHTKIT-SERIOUS: OAuthClient::exchange_code cannot take a pasted code with our own verifier, so sign-in fails when port 1455 is busy (report P-4)
async fn exchange_openai_code(
    poster: &dyn FormPoster,
    code: &str,
    verifier: &str,
    signal: AuthSignal,
) -> Result<TokenSet, AuthError> {
    let body = BTreeMap::from([
        ("grant_type".into(), "authorization_code".into()),
        ("client_id".into(), OPENAI_CLIENT_ID.into()),
        ("code".into(), code.into()),
        ("code_verifier".into(), verifier.into()),
        ("redirect_uri".into(), OPENAI_REDIRECT_URI.into()),
    ]);
    let text = poster
        .post_form(OPENAI_TOKEN_URL, body, signal)
        .await
        .map_err(|error| {
            AuthError::oauth(format!("OpenAI Codex token exchange failed: {error}"))
        })?;
    let token = parse_token_response(&text, "OpenAI Codex")?;
    // Sign-in is refused without the routing claim every later request needs.
    crate::agent_providers::client::openai_account_id_from_token(&token.access_token)
        .ok_or_else(|| AuthError::oauth("OpenAI Codex token did not contain chatgpt_account_id"))?;
    Ok(token_set(
        token.access_token,
        Some(token.refresh_token),
        std::time::Duration::from_secs(token.expires_in.max(1) as u64),
    ))
}

// Do not transplant Anthropic's OAuth free-port (dynamic redirect) patch into
// this flow (#48). The OpenAI redirect URI is registered for the fixed port
// `OPENAI_CALLBACK_PORT`; a dynamic port needs provider-specific contract
// evidence first. When the port is occupied, fall back to manual redirect
// entry (see `openai_callback_from_bind`) instead of binding another port.
//
// The loopback listener is rightkit-oauth's `CallbackListener` (single use,
// tolerant of probes and wrong-state requests, constant-time state check).
//
// The listener binds loopback only: no environment variable can move it.
async fn openai_callback(state: &str, cancel: CancelToken) -> Option<String> {
    let timeout = std::time::Duration::from_millis(COMMAND_CODE_AUTH_TIMEOUT_MS);
    let bind = CallbackBind::new(CallbackOptions {
        host: LoopbackHost::V4,
        port: OPENAI_CALLBACK_PORT,
        path: OPENAI_CALLBACK_PATH.into(),
    });
    let listener = CallbackListener::bind_with(state, bind);
    openai_callback_from_bind(listener, timeout, cancel).await
}

async fn openai_callback_from_bind(
    listener: Result<CallbackListener, OAuthError>,
    timeout: std::time::Duration,
    cancel: CancelToken,
) -> Option<String> {
    let Ok(listener) = listener else {
        // A different process may own Pi's fixed callback port. Keep manual
        // redirect entry available until the same finite auth deadline.
        tokio::time::sleep(timeout).await;
        return None;
    };
    // The wait is blocking; `cancel` ends it when the manual paste wins the
    // race, so the fixed port is released for the next attempt.
    tokio::task::spawn_blocking(move || listener.wait(timeout, Some(&cancel)))
        .await
        .ok()?
        .ok()
        .map(|code| code.expose().to_string())
}

async fn race_openai_input(
    callback: impl std::future::Future<Output = Option<String>> + Send,
    manual: impl std::future::Future<Output = Result<String, AuthError>> + Send,
) -> Result<String, AuthError> {
    tokio::select! {
        result = callback => result.ok_or_else(|| AuthError::oauth("Missing authorization code")),
        result = manual => result,
    }
}

const COMMAND_CODE_CALLBACK_PATH: &str = "/callback";
const COMMAND_CODE_ORIGIN: &str = "https://commandcode.ai";

/// Bind Command Code's browser callback: the first free port in 5959..=5968,
/// else an ephemeral one. `state` must be at least 16 visible ASCII characters
/// (`generate_state` output).
fn bind_command_code_callback(state: &str) -> Option<CallbackListener> {
    (5959..=5968u16).chain(std::iter::once(0)).find_map(|port| {
        CallbackListener::bind(
            state,
            CallbackOptions {
                host: LoopbackHost::V4,
                port,
                path: COMMAND_CODE_CALLBACK_PATH.into(),
            },
        )
        .ok()
    })
}

/// Command Code's `pi-commandcode-provider` callback contract. The browser
/// delivers its API key by a CORS-preflighted JSON POST
/// (`CallbackListener::wait_post_json`, exact origin, exact path, constant-time
/// `state` check). The POST must carry the API key, user id, display name, and
/// key name; a provider-reported `error` or an incomplete body leaves the
/// paste fallback in charge. `cancel` ends the blocking wait so the port is
/// released when the paste wins.
async fn command_code_callback(listener: CallbackListener, cancel: CancelToken) -> Option<String> {
    let timeout = std::time::Duration::from_millis(COMMAND_CODE_AUTH_TIMEOUT_MS);
    let payload = tokio::task::spawn_blocking(move || {
        listener.wait_post_json(timeout, Some(&cancel), COMMAND_CODE_ORIGIN)
    })
    .await
    .ok()?
    .ok()?;
    if payload.get("error").is_some() {
        return None;
    }
    let api_key = payload.get("apiKey").and_then(serde_json::Value::as_str)?;
    let required = ["userId", "userName", "keyName"];
    if required.iter().any(|name| {
        payload
            .get(*name)
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty)
    }) {
        return None;
    }
    Some(sanitize_command_code_key(api_key))
}

pub struct OpenAICodexOAuth {
    poster: Arc<dyn FormPoster>,
    originator: String,
}

impl OpenAICodexOAuth {
    pub fn new(poster: Arc<dyn FormPoster>) -> Self {
        Self {
            poster,
            originator: "pi".into(),
        }
    }
    pub fn with_originator(mut self, originator: impl Into<String>) -> Self {
        self.originator = originator.into();
        self
    }
}

impl Default for OpenAICodexOAuth {
    fn default() -> Self {
        Self::new(Arc::new(HttpFormPoster::default()))
    }
}

impl SubscriptionLogin for OpenAICodexOAuth {
    fn name(&self) -> &str {
        "ChatGPT Plus/Pro (Codex Subscription)"
    }
    fn subscription(&self) -> SubscriptionEligibility {
        SubscriptionEligibility::Supported
    }
    fn login_label(&self) -> Option<&str> {
        Some("OpenAI / ChatGPT browser login")
    }
    fn login<'a>(
        &'a self,
        interaction: &'a dyn AuthInteraction,
    ) -> BoxFuture<'a, Result<TokenSet, AuthError>> {
        Box::pin(async move {
            let Pkce {
                verifier,
                challenge,
            } = Pkce::generate().map_err(|error| AuthError::oauth(error.to_string()))?;
            let state = generate_state().map_err(|error| AuthError::oauth(error.to_string()))?;
            let url = authorization_url(&state, &challenge, &self.originator);
            interaction.notify(AuthEvent::AuthUrl {
                url: url.clone(),
                instructions: Some("Complete login in your browser; paste the final redirect URL if callback fails.".into()),
            });
            let cancel = CancelToken::new();
            let callback = openai_callback(&state, cancel.clone());
            let input = race_openai_input(
                callback,
                interaction.prompt(AuthPrompt::ManualCode {
                    message: "Paste the authorization code or full redirect URL:".into(),
                    placeholder: Some(OPENAI_REDIRECT_URI.into()),
                }),
            )
            .await;
            // Release the fixed callback port whichever side won.
            cancel.cancel();
            let input = input?;
            let (code, callback_state) = parse_openai_authorization_input(&input);
            if let Some(callback_state) = callback_state {
                if callback_state != state {
                    return Err(AuthError::oauth("OAuth state mismatch"));
                }
            }
            let code = code.ok_or_else(|| AuthError::oauth("Missing authorization code"))?;
            exchange_openai_code(
                self.poster.as_ref(),
                &code,
                verifier.expose(),
                interaction.signal(),
            )
            .await
        })
    }
    fn to_auth(&self, access_token: &str) -> Result<ModelAuth, AuthError> {
        let account_id = crate::agent_providers::client::openai_account_id_from_token(access_token)
            .ok_or_else(|| AuthError::oauth("OpenAI Codex credential has no account id"))?;
        Ok(ModelAuth {
            api_key: Some(access_token.to_string()),
            headers: Some(BTreeMap::from([
                ("chatgpt-account-id".into(), account_id),
                ("originator".into(), self.originator.clone()),
            ])),
            base_url: Some(OPENAI_CODEX_BASE_URL.into()),
        })
    }
}

fn sanitize_command_code_key(input: &str) -> String {
    let esc = char::from(27);
    input
        .replace(&format!("{esc}[200~"), "")
        .replace(&format!("{esc}[201~"), "")
        .replace("[200~", "")
        .replace("[201~", "")
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn sanitize_command_code_api_key(input: &str) -> String {
    sanitize_command_code_key(input)
}

pub trait ApiKeyValidator: Send + Sync {
    fn validate<'a>(&'a self, api_key: &'a str) -> BoxFuture<'a, Result<(), AuthError>>;
}

#[derive(Default)]
pub struct CommandCodeApiKeyValidator;

impl ApiKeyValidator for CommandCodeApiKeyValidator {
    fn validate<'a>(&'a self, api_key: &'a str) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            let url = format!("{COMMAND_CODE_API_BASE}/alpha/whoami");
            let http = crate::agent_providers::http::provider_client(
                &url,
                crate::agent_providers::http::Route::Ambient,
            );
            let request = rightkit_http::Request::get(url.clone()).bearer(api_key);
            let response = http.send_any(&request).await.map_err(|error| {
                AuthError::oauth(format!("Command Code key validation failed: {error}"))
            })?;
            let status = response.status;
            if status == 401 {
                return Err(AuthError::oauth("Invalid Command Code API key"));
            }
            if !(200..300).contains(&status) {
                return Err(AuthError::oauth(format!(
                    "Command Code key validation failed: HTTP {status}"
                )));
            }
            Ok(())
        })
    }
}

pub struct CommandCodeOAuth {
    validator: Arc<dyn ApiKeyValidator>,
}
impl CommandCodeOAuth {
    pub fn new(validator: Arc<dyn ApiKeyValidator>) -> Self {
        Self { validator }
    }
}
impl Default for CommandCodeOAuth {
    fn default() -> Self {
        Self::new(Arc::new(CommandCodeApiKeyValidator::default()))
    }
}
impl SubscriptionLogin for CommandCodeOAuth {
    fn name(&self) -> &str {
        "Command Code"
    }
    fn subscription(&self) -> SubscriptionEligibility {
        SubscriptionEligibility::Supported
    }
    fn login_label(&self) -> Option<&str> {
        Some("Command Code browser login or API key")
    }
    fn login<'a>(
        &'a self,
        interaction: &'a dyn AuthInteraction,
    ) -> BoxFuture<'a, Result<TokenSet, AuthError>> {
        Box::pin(async move {
            let choice = sanitize_command_code_key(&interaction.prompt(AuthPrompt::Text {
                message: "Command Code login: press Enter for browser login, type 'key' to paste an API key, or paste the API key directly:".into(),
                placeholder: None,
            }).await?);
            let key = if choice.is_empty()
                || matches!(choice.to_ascii_lowercase().as_str(), "1" | "b" | "browser")
            {
                let state =
                    generate_state().map_err(|error| AuthError::oauth(error.to_string()))?;
                let listener = bind_command_code_callback(&state)
                    .ok_or_else(|| AuthError::oauth("Could not start Command Code browser auth"))?;
                let port = listener.port();
                let callback = format!("http://localhost:{port}/callback");
                let url = format!(
                    "{COMMAND_CODE_STUDIO_BASE}/studio/auth/cli?callback={}&state={}",
                    form_encode(&callback),
                    form_encode(&state)
                );
                interaction.notify(AuthEvent::AuthUrl { url, instructions: Some("Complete Command Code browser login; paste API key if automatic transfer fails.".into()) });
                let cancel = CancelToken::new();
                let callback_result = command_code_callback(listener, cancel.clone());
                tokio::pin!(callback_result);
                let outcome: Result<String, AuthError> = tokio::select! {
                    result = &mut callback_result => Ok(result.unwrap_or_default()),
                    result = interaction.prompt(AuthPrompt::Secret { message: "Paste your Command Code API key:".into(), placeholder: None }) => result.map(|text| sanitize_command_code_key(&text)),
                };
                // Release the callback port whichever side won.
                cancel.cancel();
                let callback_key = outcome?;
                if callback_key.is_empty() {
                    sanitize_command_code_key(&interaction.prompt(AuthPrompt::Secret { message: "Automatic transfer failed or timed out. Paste your Command Code API key:".into(), placeholder: None }).await?)
                } else {
                    callback_key
                }
            } else if matches!(
                choice.to_ascii_lowercase().as_str(),
                "2" | "k" | "key" | "api" | "paste"
            ) {
                sanitize_command_code_key(
                    &interaction
                        .prompt(AuthPrompt::Secret {
                            message: "Paste your Command Code API key:".into(),
                            placeholder: None,
                        })
                        .await?,
                )
            } else {
                choice
            };
            if key.is_empty() {
                return Err(AuthError::oauth("No Command Code API key provided"));
            }
            self.validator.validate(&key).await?;
            // An API key, not an OAuth grant: no refresh token, a ten-year window.
            Ok(token_set(
                key,
                None,
                std::time::Duration::from_millis(COMMAND_CODE_KEY_LIFETIME_MS as u64),
            ))
        })
    }
    fn to_auth(&self, access_token: &str) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth::api_key(access_token.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_code_key_sanitization_matches_pi() {
        assert_eq!(
            sanitize_command_code_api_key("\u{1b}[200~ user_key \u{1b}[201~"),
            "user_key"
        );
    }
    #[test]
    fn openai_authorize_url_contains_pi_contract() {
        let url = authorization_url("state", "challenge", "pi");
        for item in [
            "client_id=app_EMoamEEZ73f0CkXaXp7hrann",
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
            "id_token_add_organizations=true",
            "codex_cli_simplified_flow=true",
            "originator=pi",
        ] {
            assert!(url.contains(item), "missing {item}: {url}");
        }
    }

    #[tokio::test]
    async fn openai_callback_ignores_wrong_state_until_valid_callback_arrives() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = CallbackListener::bind(
            "expected-state-0123456789",
            CallbackOptions {
                host: LoopbackHost::V4,
                port: 0,
                path: OPENAI_CALLBACK_PATH.into(),
            },
        )
        .unwrap();
        let address = listener.local_addrs()[0];
        let callback = tokio::spawn(openai_callback_from_bind(
            Ok(listener),
            std::time::Duration::from_secs(2),
            CancelToken::new(),
        ));

        for path in [
            "/auth/callback?code=wrong&state=other-state",
            "/auth/callback?code=right&state=expected-state-0123456789",
        ] {
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            socket
                .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = Vec::new();
            socket.read_to_end(&mut response).await.unwrap();
            let response = String::from_utf8_lossy(&response);
            if path.contains("other-state") {
                assert!(response.starts_with("HTTP/1.1 400"));
            } else {
                assert!(response.starts_with("HTTP/1.1 200"));
            }
        }
        assert_eq!(callback.await.unwrap().as_deref(), Some("right"));
    }

    #[tokio::test]
    async fn occupied_openai_callback_port_keeps_manual_entry_active() {
        let occupied = CallbackListener::bind(
            "occupied-state-0123456789",
            CallbackOptions {
                host: LoopbackHost::V4,
                port: 0,
                path: OPENAI_CALLBACK_PATH.into(),
            },
        )
        .unwrap();
        let port = occupied.port();
        let bind_failure = CallbackListener::bind(
            "second-state-0123456789",
            CallbackOptions {
                host: LoopbackHost::V4,
                port,
                path: OPENAI_CALLBACK_PATH.into(),
            },
        )
        .unwrap_err();
        let callback = openai_callback_from_bind(
            Err(bind_failure),
            std::time::Duration::from_secs(1),
            CancelToken::new(),
        );
        let manual = async { Ok("manual-code".to_string()) };
        assert_eq!(
            race_openai_input(callback, manual).await.unwrap(),
            "manual-code"
        );
    }

    #[tokio::test]
    async fn callback_deadline_ends_wait_without_persisting_manual_input() {
        let callback = async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            None
        };
        let manual = std::future::pending::<Result<String, AuthError>>();
        assert!(race_openai_input(callback, manual).await.is_err());
    }
    #[test]
    fn authorization_input_accepts_redirect_and_fragment() {
        assert_eq!(
            parse_openai_authorization_input(
                "http://localhost:1455/auth/callback?code=a%2Bb&state=s"
            ),
            (Some("a+b".into()), Some("s".into()))
        );
        assert_eq!(
            parse_openai_authorization_input("code#state"),
            (Some("code".into()), Some("state".into()))
        );
    }
    #[test]
    fn command_code_token_set_is_a_long_lived_key_without_refresh_grant() {
        let tokens = token_set(
            "key".to_string(),
            None,
            std::time::Duration::from_millis(COMMAND_CODE_KEY_LIFETIME_MS as u64),
        );
        assert_eq!(tokens.access_token(), "key");
        assert!(tokens.refresh_token.is_none());
        assert!(tokens.id_token.is_none());
        assert!(!tokens.is_expired(std::time::Duration::from_secs(5 * 60)));
    }

    #[test]
    fn oauth_configs_pass_rightkit_validation() {
        for config in [openai_oauth_config(), commandcode_oauth_config()] {
            rightkit_oauth::OAuthClient::new(
                config,
                rightkit_http::Client::new(rightkit_http::ClientConfig::default()),
            )
            .expect("fixed OAuth config must validate");
        }
        let openai = openai_oauth_config();
        assert_eq!(openai.redirect.port, OPENAI_CALLBACK_PORT);
        assert_eq!(openai.redirect.path, OPENAI_CALLBACK_PATH);
        assert!(openai.scopes.iter().any(|scope| scope == "offline_access"));
    }

    #[test]
    fn openai_to_auth_requires_the_account_routing_claim() {
        // Not a JWT: no account id, no auth.
        assert!(OpenAICodexOAuth::default().to_auth("not-a-jwt").is_err());
    }

    #[test]
    fn openai_token_errors_do_not_echo_response_body() {
        let error = parse_token_response(
            r#"{"access_token":"secret","refresh_token":"secret2"}"#,
            "OpenAI",
        )
        .unwrap_err();
        assert!(!error.message.contains("secret"));
    }
}
