//! PL-001 daemon provider-auth surface.
//!
//! This module owns one process-lifetime broker, rather than copying auth
//! state into `DaemonState`. Credentials remain in the existing keychain
//! substrate; request/response records contain metadata only.

use axum::{
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use rightkit_llm::agent_providers::credential_broker::{
    CredentialBroker, ProviderCredentialConfig,
};
use rightkit_llm::agent_providers::subscription_auth::ModelAuth;
use rightkit_llm::agent_providers::subscription_auth::{
    AuthError, AuthEvent, AuthInteraction, AuthPrompt, AuthSignal, BoxFuture, ProcessAuthContext,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::oneshot;

static BROKER: OnceLock<Arc<CredentialBroker>> = OnceLock::new();
type PendingLoginMap = Arc<Mutex<HashMap<String, Arc<PendingLogin>>>>;
static PENDING: OnceLock<PendingLoginMap> = OnceLock::new();

struct PendingLogin {
    deadline_ms: i64,
    prompt: Mutex<Option<oneshot::Sender<String>>>,
    events: Mutex<Vec<AuthEvent>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PendingDisk {
    provider: String,
    challenge_id: String,
    deadline_ms: i64,
    state: String,
}

fn pending_path() -> std::path::PathBuf {
    std::path::PathBuf::from(
        rightkit_llm::agent_providers::subscription_auth::expand_tilde(
            "~/.coderight/provider-auth-pending.json",
        ),
    )
}
fn persist_pending(map: &HashMap<String, Arc<PendingLogin>>) {
    let rows: Vec<PendingDisk> = map
        .iter()
        .map(|(id, p)| PendingDisk {
            provider: id.split(':').nth(1).unwrap_or_default().to_string(),
            challenge_id: id.clone(),
            deadline_ms: p.deadline_ms,
            state: "authenticating".to_string(),
        })
        .collect();
    if let Ok(raw) = serde_json::to_vec(&rows) {
        if let Some(parent) = pending_path().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(pending_path(), raw);
    }
}

impl PendingLogin {
    fn new() -> Self {
        Self {
            deadline_ms: rightkit_llm::agent_providers::subscription_auth::now_ms()
                + 10 * 60 * 1000,
            prompt: Mutex::new(None),
            events: Mutex::new(Vec::new()),
        }
    }
}

struct HttpInteraction {
    pending: Arc<PendingLogin>,
}
impl AuthInteraction for HttpInteraction {
    fn signal(&self) -> AuthSignal {
        AuthSignal::default()
    }
    fn prompt<'a>(&'a self, prompt: AuthPrompt) -> BoxFuture<'a, Result<String, AuthError>> {
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            *self.pending.prompt.lock().unwrap() = Some(tx);
            self.pending.notify(AuthEvent::Progress {
                message: format!("prompt: {prompt:?}"),
            });
            rx.await
                .map_err(|_| AuthError::oauth("auth prompt cancelled"))
        })
    }
    fn notify(&self, event: AuthEvent) {
        self.pending.notify(event);
    }
}
impl PendingLogin {
    fn notify(&self, event: AuthEvent) {
        self.events.lock().unwrap().push(event);
    }
}

/// Cross-process refresh-lock directory for the subscription sign-ins:
/// `<state root>/locks/oauth`.
fn oauth_lock_dir() -> std::path::PathBuf {
    rightkit_app_settings::agent_config::state_root()
        .unwrap_or_else(|| std::env::temp_dir().join("coderight"))
        .join("locks")
        .join("oauth")
}

fn broker() -> &'static Arc<CredentialBroker> {
    BROKER.get_or_init(|| {
        let mut broker = CredentialBroker::new(
            Arc::new(rightkit_llm::agent_providers::OsKeyStore),
            "coderight",
            Arc::new(ProcessAuthContext),
        );
        for provider in rightkit_llm::agent_providers::PROVIDERS {
            // Native inference uses one daemon-owned precedence chain. Keep
            // vendor CLI stores out of this broker; provider env names come
            // from provider metadata, while literal keys/keychain entries
            // use RightKit-owned namespaces.
            let mut config =
                ProviderCredentialConfig::new(provider.id).with_secure_store_key(provider.id);
            if !provider.api_key_env.is_empty() {
                config = config.with_env_vars([provider.api_key_env]);
            }
            if matches!(provider.id, "openai" | "commandcode") {
                // The subscription leg is RightKit's `Resolver` over the keychain token
                // store. Store failures surface at use (status reports `unavailable`),
                // never as a fallback credential.
                let leg =
                    rightkit_llm::agent_providers::subscription_auth::open_subscription_store()
                        .ok()
                        .and_then(|store| {
                            rightkit_llm::agent_providers::subscription_auth::subscription_leg(
                                provider.id,
                                store,
                                oauth_lock_dir(),
                            )
                        });
                if let Some(leg) = leg {
                    config = config.with_subscription(leg);
                }
            }
            broker.register(config);
        }
        Arc::new(broker)
    })
}

/// Run native provider login in the calling terminal so OAuth instructions
/// are visible immediately and success is tied to this exact broker attempt.
pub(crate) async fn terminal_login(provider: &str) -> Result<(), String> {
    let interaction = TerminalAuthInteraction::default();
    let result = broker()
        .login(provider, &interaction)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string());
    let reader_result = interaction.finish_reader().await;
    result.and(reader_result)
}

pub(crate) async fn terminal_status(provider: &str) -> Result<serde_json::Value, String> {
    broker()
        .status(provider, AuthSignal::default())
        .await
        .map_err(|error| error.to_string())
        .and_then(provider_status_json)
}

/// Project the public state discriminator as a scalar while retaining typed expiry
/// and redacted diagnostics separately. No credential-bearing value enters
/// this projection; unreadable storage is unavailable, never signed-out.
fn provider_status_json(
    record: rightkit_llm::agent_providers::credential_broker::BrokerRecord,
) -> Result<serde_json::Value, String> {
    let mut value = serde_json::to_value(record).map_err(|error| error.to_string())?;
    let detail = value["subscription_auth_state"].clone();
    value["subscription_auth_state"] = detail
        .get("state")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    value["subscription_auth_details"] = detail;
    Ok(value)
}

/// Drop the native subscription credential for a provider.
pub(crate) async fn terminal_logout(provider: &str) -> Result<(), String> {
    broker()
        .logout(provider, AuthSignal::default())
        .await
        .map_err(|error| error.to_string())
}

/// Browser sign-in for hosts that own the terminal (the TUI): opens the auth
/// URL in the default browser, never prompts or prints, and relies on the
/// loopback callback. Bounded to five minutes.
pub(crate) async fn quiet_login(provider: &str) -> Result<(), String> {
    let interaction = QuietAuthInteraction {
        signal: AuthSignal::default(),
    };
    match tokio::time::timeout(
        std::time::Duration::from_secs(300),
        broker().login(provider, &interaction),
    )
    .await
    {
        Ok(result) => result.map(|_| ()).map_err(|error| error.to_string()),
        Err(_) => {
            interaction.signal.abort();
            Err("sign-in timed out".to_string())
        }
    }
}

struct QuietAuthInteraction {
    signal: AuthSignal,
}

impl AuthInteraction for QuietAuthInteraction {
    fn signal(&self) -> AuthSignal {
        self.signal.clone()
    }

    fn prompt<'a>(&'a self, _prompt: AuthPrompt) -> BoxFuture<'a, Result<String, AuthError>> {
        // No manual paste in the quiet flow: stay pending so the loopback
        // callback wins the race.
        Box::pin(async move { std::future::pending::<Result<String, AuthError>>().await })
    }

    fn notify(&self, event: AuthEvent) {
        if let AuthEvent::AuthUrl { url, .. } = event {
            open_in_browser(&url);
        }
    }
}

fn open_in_browser(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return;
    }
    #[cfg(target_os = "windows")]
    let mut command = rightkit_child_process::command_std("rundll32.exe");
    #[cfg(target_os = "windows")]
    command.arg("url.dll,FileProtocolHandler").arg(url);
    #[cfg(target_os = "macos")]
    let mut command = rightkit_child_process::command_std("open");
    #[cfg(target_os = "macos")]
    command.arg(url);
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = rightkit_child_process::command_std("xdg-open");
    #[cfg(all(unix, not(target_os = "macos")))]
    command.arg(url);
    let _ = command.status();
}

struct TerminalAuthInteraction {
    signal: AuthSignal,
    reader_cancelled: Arc<std::sync::atomic::AtomicBool>,
    reader_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Default for TerminalAuthInteraction {
    fn default() -> Self {
        Self {
            signal: AuthSignal::default(),
            reader_cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reader_task: tokio::sync::Mutex::new(None),
        }
    }
}

impl TerminalAuthInteraction {
    async fn finish_reader(&self) -> Result<(), String> {
        self.reader_cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let task = self.reader_task.lock().await.take();
        if let Some(task) = task {
            tokio::time::timeout(std::time::Duration::from_secs(2), task)
                .await
                .map_err(|_| "terminal input reader did not stop".to_string())?
                .map_err(|_| "terminal input reader failed to join".to_string())?;
        }
        Ok(())
    }
}

struct InputReadCancellation(Arc<std::sync::atomic::AtomicBool>);

impl Drop for InputReadCancellation {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn read_terminal_line(interaction: &TerminalAuthInteraction) -> Result<String, AuthError> {
    use rightkit_terminal_ui::crossterm::{event, terminal};
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    let cancelled = interaction.reader_cancelled.clone();
    let _cancel_on_drop = InputReadCancellation(cancelled.clone());
    let reader_cancelled = cancelled.clone();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let reader = tokio::task::spawn_blocking(move || {
        let raw_mode = terminal::enable_raw_mode().is_ok();
        struct RawModeGuard(bool);
        impl Drop for RawModeGuard {
            fn drop(&mut self) {
                if self.0 {
                    let _ = rightkit_terminal_ui::crossterm::terminal::disable_raw_mode();
                }
            }
        }
        let _raw_mode = RawModeGuard(raw_mode);
        let mut line = String::new();
        loop {
            if reader_cancelled.load(Ordering::SeqCst) {
                let _ = result_tx.send(Err("cancelled".to_string()));
                return;
            }
            match event::poll(Duration::from_millis(40)) {
                Ok(true) if !reader_cancelled.load(Ordering::SeqCst) => match event::read() {
                    Ok(event::Event::Key(key)) if key.kind != event::KeyEventKind::Release => {
                        match key.code {
                            event::KeyCode::Enter => {
                                println!();
                                let _ = result_tx.send(Ok(line));
                                return;
                            }
                            event::KeyCode::Esc => {
                                let _ = result_tx.send(Err("cancelled".into()));
                                return;
                            }
                            event::KeyCode::Backspace => {
                                line.pop();
                            }
                            event::KeyCode::Char('c')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                let _ = result_tx.send(Err("cancelled".into()));
                                return;
                            }
                            event::KeyCode::Char(value) => {
                                line.push(value);
                                print!("*");
                                let _ = std::io::Write::flush(&mut std::io::stdout());
                            }
                            _ => {}
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        let _ = result_tx.send(Err(error.to_string()));
                        return;
                    }
                },
                Ok(_) => {}
                Err(error) => {
                    let _ = result_tx.send(Err(error.to_string()));
                    return;
                }
            }
        }
    });
    *interaction.reader_task.lock().await = Some(reader);
    tokio::select! {
        result = result_rx => result
            .map_err(|_| AuthError::oauth("terminal input task failed"))?
            .map_err(|error| {
                if error == "cancelled" {
                    interaction.signal.abort();
                    AuthError::cancelled()
                } else {
                    AuthError::oauth("terminal input failed")
                }
            }),
        result = tokio::signal::ctrl_c() => {
            if result.is_ok() {
                interaction.signal.abort();
                cancelled.store(true, Ordering::SeqCst);
            }
            Err(AuthError::cancelled())
        }
    }
}

impl AuthInteraction for TerminalAuthInteraction {
    fn signal(&self) -> AuthSignal {
        self.signal.clone()
    }

    fn prompt<'a>(&'a self, prompt: AuthPrompt) -> BoxFuture<'a, Result<String, AuthError>> {
        Box::pin(async move {
            let (message, placeholder) = match prompt {
                AuthPrompt::Text {
                    message,
                    placeholder,
                }
                | AuthPrompt::Secret {
                    message,
                    placeholder,
                }
                | AuthPrompt::ManualCode {
                    message,
                    placeholder,
                } => (message, placeholder),
                AuthPrompt::Select { message, .. } => (message, None),
            };
            print!("{message}");
            if let Some(placeholder) = placeholder {
                print!(" ({placeholder})");
            }
            print!("\n> ");
            let _ = std::io::Write::flush(&mut std::io::stdout());

            // Crossterm polling is bounded and cancellable. A callback
            // winning OAuth sets the reader's drop flag; it exits its poll
            // within 40 ms and restores terminal mode before another TUI read.
            let line = tokio::select! {
                result = read_terminal_line(self) => result,
                result = tokio::signal::ctrl_c() => {
                    if result.is_ok() {
                        self.signal.abort();
                    }
                    return Err(AuthError::cancelled());
                }
            }?;
            Ok(line)
        })
    }

    fn notify(&self, event: AuthEvent) {
        match event {
            AuthEvent::AuthUrl { url, instructions } => {
                println!("Open this URL to sign in:\n{url}");
                if let Some(instructions) = instructions {
                    println!("{instructions}");
                }
            }
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                ..
            } => {
                println!("Open {verification_uri} and enter code {user_code}");
            }
            AuthEvent::Info { message, .. } | AuthEvent::Progress { message } => {
                println!("{message}")
            }
        }
    }
}

#[cfg(test)]
mod terminal_auth_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn dropping_manual_reader_cancels_bounded_terminal_polling() {
        let interaction = TerminalAuthInteraction::default();
        let cancelled = interaction.reader_cancelled.clone();
        let exited = Arc::new(AtomicBool::new(false));
        let reader_exited = exited.clone();
        let task = tokio::task::spawn_blocking(move || {
            while !cancelled.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            reader_exited.store(true, Ordering::SeqCst);
        });
        *interaction.reader_task.lock().await = Some(task);

        interaction.finish_reader().await.unwrap();
        assert!(exited.load(Ordering::SeqCst));
    }
}

/// Resolve one inference credential at request construction time. This is the
/// only runtime seam for subscription credentials: OAuth refresh therefore
/// runs through broker's per-provider lock before any provider request is
/// built. API-key routes still resolve through broker's env/keychain chain.
pub(crate) async fn resolve_provider_model_auth(
    provider: &str,
) -> Result<Option<ModelAuth>, String> {
    broker()
        .resolve(provider, AuthSignal::default())
        .await
        .map(|resolution| resolution.map(|value| value.auth))
        .map_err(|_| "provider credential unavailable".to_string())
}

/// Client version sent to the ChatGPT model catalog. The backend hides
/// models whose minimum client version exceeds it, so it tracks a current
/// Codex CLI release rather than RightKit's own version. Resolved (env
/// override, 24h cache, npm registry, fallback constant) by the module below.
#[path = "codex_client_version.rs"]
mod codex_client_version;

/// One row of the account catalog: the selectable `slug` plus the display
/// label the endpoint reports. Ids newer than the repo's static table only
/// ever surface through this list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogModel {
    pub slug: String,
    pub display_name: String,
}

/// Decode the catalog body into account models: tolerant of missing
/// optional fields and unknown keys, keeps only rows the backend marks
/// `list`-visible, and sorts by ascending `priority` so the picker order
/// matches the account's own ordering.
fn decode_catalog_models(bytes: &[u8]) -> Result<Vec<CatalogModel>, String> {
    #[derive(serde::Deserialize)]
    struct Catalog {
        #[serde(default)]
        models: Vec<Entry>,
    }
    #[derive(serde::Deserialize)]
    struct Entry {
        #[serde(default)]
        slug: String,
        #[serde(default)]
        display_name: String,
        #[serde(default)]
        visibility: String,
        #[serde(default)]
        priority: i64,
    }

    let mut catalog: Catalog = serde_json::from_slice(bytes)
        .map_err(|error| format!("could not decode catalog body: {error}"))?;
    catalog
        .models
        .retain(|entry| entry.visibility.is_empty() || entry.visibility == "list");
    catalog.models.sort_by_key(|entry| entry.priority);
    Ok(catalog
        .models
        .into_iter()
        .filter(|entry| !entry.slug.trim().is_empty())
        .map(|entry| {
            let slug = entry.slug.trim().to_string();
            let display_name = if entry.display_name.trim().is_empty() {
                slug.clone()
            } else {
                entry.display_name
            };
            CatalogModel { slug, display_name }
        })
        .collect())
}

/// Live models the signed-in ChatGPT account can use, in the backend's
/// picker order. Errors are typed by the stage that failed — credential
/// resolution, missing ChatGPT base URL, HTTP status, or decode — so a
/// diagnostic caller (`coderight models`) can tell an empty account list
/// apart from a broken fetch; credential bytes are never part of an error.
/// Callers that only want "no account list" semantics swallow the error.
pub(crate) async fn fetch_chatgpt_account_models(
    provider: &str,
) -> Result<Vec<CatalogModel>, String> {
    let auth = resolve_provider_model_auth(provider)
        .await
        .map_err(|error| {
            format!("credential resolution failed for provider '{provider}': {error}")
        })?
        .ok_or_else(|| format!("no credentials resolved for provider '{provider}'"))?;
    let (Some(token), Some(base)) = (auth.api_key, auth.base_url) else {
        return Err(format!(
            "provider '{provider}' resolved auth carries no token/base URL"
        ));
    };
    if !base.contains("chatgpt.com") {
        return Err(format!(
            "provider '{provider}' resolved base URL is not a ChatGPT sign-in"
        ));
    }
    let client_version = codex_client_version::resolve_client_version().await;
    let url = format!(
        "{}/codex/models?client_version={client_version}",
        base.trim_end_matches('/')
    );
    let client =
        rightkit_http::http_client::endpoint_client(&url, std::time::Duration::from_secs(10))
            .map_err(|error| format!("could not build an HTTP client: {error}"))?;
    let mut request = rightkit_http::Request::get(url).bearer(&token);
    for (name, value) in auth.headers.unwrap_or_default() {
        request = request.header(name, value);
    }
    let response = client
        .send_any(&request)
        .await
        .map_err(|error| format!("catalog request failed: {error}"))?;
    let status = response.status;
    if !rightkit_http::http_client::is_success(status) {
        return Err(format!("catalog endpoint answered HTTP {status}"));
    }
    let bytes = response.body;
    decode_catalog_models(&bytes)
}

#[cfg(test)]
mod catalog_decode_tests {
    use super::{decode_catalog_models, CatalogModel};

    #[test]
    fn decode_filters_non_list_visibility_and_sorts_by_priority() {
        let body = serde_json::json!({
            "models": [
                { "slug": "gpt-6-astra", "display_name": "GPT-6-Astra", "visibility": "list", "priority": 30 },
                { "slug": "gpt-5.1", "display_name": "gpt-5.1", "visibility": "list", "priority": 10 },
                { "slug": "gpt-4-hidden", "visibility": "hide", "priority": 1 },
                { "slug": "gpt-4-none", "visibility": "none", "priority": 2 },
                { "slug": "gpt-6.1-sol", "visibility": "list", "priority": 20 },
            ]
        });
        let models = decode_catalog_models(body.to_string().as_bytes()).unwrap();
        assert_eq!(
            models,
            vec![
                CatalogModel {
                    slug: "gpt-5.1".to_string(),
                    display_name: "gpt-5.1".to_string(),
                },
                CatalogModel {
                    slug: "gpt-6.1-sol".to_string(),
                    display_name: "gpt-6.1-sol".to_string(),
                },
                CatalogModel {
                    slug: "gpt-6-astra".to_string(),
                    display_name: "GPT-6-Astra".to_string(),
                },
            ]
        );
    }

    #[test]
    fn decode_is_tolerant_of_missing_optional_fields_and_unknown_keys() {
        let body = serde_json::json!({
            "models": [
                { "slug": "gpt-6-astra", "future_field": { "nested": true } },
                { "slug": "   " },
            ],
            "server_version": 2,
        });
        let models = decode_catalog_models(body.to_string().as_bytes()).unwrap();
        assert_eq!(
            models,
            vec![CatalogModel {
                slug: "gpt-6-astra".to_string(),
                // A missing display_name falls back to the slug; a blank
                // slug row is dropped.
                display_name: "gpt-6-astra".to_string(),
            }]
        );
    }

    #[test]
    fn decode_reports_the_decode_stage_on_bad_json() {
        let error = decode_catalog_models(b"not json").unwrap_err();
        assert!(error.contains("could not decode catalog body"), "{error}");
    }
}

fn pending() -> &'static Arc<Mutex<HashMap<String, Arc<PendingLogin>>>> {
    PENDING.get_or_init(|| {
        // Recovery is metadata-only. Expired challenges are discarded before
        // becoming visible; no callback or credential bytes are persisted.
        let map = HashMap::new();
        if let Ok(raw) = std::fs::read(pending_path()) {
            if let Ok(rows) = serde_json::from_slice::<Vec<PendingDisk>>(&raw) {
                // Never resume an OAuth exchange after daemon restart.
                // Orphaned authenticating records are expired/cancelled.
                let _ = rows;
            }
        }
        Arc::new(Mutex::new(map))
    })
}

fn trusted(headers: &HeaderMap) -> Result<(), StatusCode> {
    crate::api_permissions::enforce_trusted_source_authority(headers)
        .map_err(|_| StatusCode::FORBIDDEN)
}

pub(crate) fn provider_auth_routes() -> Router<crate::api::DaemonState> {
    Router::new()
        .route("/provider-auth/{provider}", get(status).delete(logout))
        .route("/provider-auth/{provider}/login", post(login_start))
        .route(
            "/provider-auth/{provider}/login/{challenge}/submit",
            post(login_submit),
        )
}

async fn status(Path(provider): Path<String>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(code) = trusted(&headers) {
        return code.into_response();
    }
    match broker().status(&provider, AuthSignal::default()).await {
        Ok(record) => match provider_status_json(record) {
            Ok(value) => (StatusCode::OK, Json(value)).into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn logout(Path(provider): Path<String>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(code) = trusted(&headers) {
        return code.into_response();
    }
    match broker().logout(&provider, AuthSignal::default()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn login_start(Path(provider): Path<String>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(code) = trusted(&headers) {
        return code.into_response();
    }
    // Interactive callback/device transport is supplied by the desktop
    // client; this endpoint advertises the provider flow and challenge id
    // without ever returning credential material.
    let challenge_id = format!(
        "provider-auth:{}:{}",
        provider,
        rightkit_llm::agent_providers::subscription_auth::now_ms()
    );
    let interaction = Arc::new(PendingLogin::new());
    pending()
        .lock()
        .unwrap()
        .insert(challenge_id.clone(), interaction.clone());
    persist_pending(&pending().lock().unwrap());
    let broker = broker().clone();
    let id = challenge_id.clone();
    let login_provider = provider.clone();
    tokio::spawn(async move {
        let result = broker
            .login(
                &login_provider,
                &HttpInteraction {
                    pending: interaction.clone(),
                },
            )
            .await;
        interaction.notify(
            rightkit_llm::agent_providers::subscription_auth::AuthEvent::Progress {
                message: format!("login complete: {}", result.is_ok()),
            },
        );
        pending().lock().unwrap().remove(&id);
        persist_pending(&pending().lock().unwrap());
    });
    (StatusCode::ACCEPTED, Json(serde_json::json!({"provider": provider, "challengeId": challenge_id, "state": "authenticating"}))).into_response()
}

async fn login_submit(
    Path((_provider, challenge)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(code) = trusted(&headers) {
        return code.into_response();
    }
    let Some(pending_login) = pending().lock().unwrap().get(&challenge).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if rightkit_llm::agent_providers::subscription_auth::now_ms() >= pending_login.deadline_ms {
        pending().lock().unwrap().remove(&challenge);
        persist_pending(&pending().lock().unwrap());
        return StatusCode::GONE.into_response();
    }
    let value = body
        .get("value")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let sent = pending_login
        .prompt
        .lock()
        .unwrap()
        .take()
        .map(|sender| sender.send(value).is_ok())
        .unwrap_or(false);
    (if sent {
        StatusCode::ACCEPTED
    } else {
        StatusCode::CONFLICT
    })
    .into_response()
}
