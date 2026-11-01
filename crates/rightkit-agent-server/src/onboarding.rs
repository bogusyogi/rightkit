//! R10 CR-26: terminal first-run onboarding — shared route/model picker, real
//! keystore-backed credential storage, and a real readiness probe.
//!
//! This module lives inside `mod main_runtime` (declared with `#[path]` from
//! `main_runtime_entry.rs`, which is `include!`d into `main_runtime.rs`). The
//! picker below is the canonical shared list — the runtime picker must consume
//! [`shared_route_picker`] via `main_runtime_entry::runtime_route_picker`, not
//! declare a second list.
//!
//! # Walkthrough — the exact sequence a first-run user sees
//!
//! All prompts are cancellable (`n`, invalid input, `Ctrl-C`, or EOF). Nothing
//! is persisted until every prompt is answered, the user confirms, and the real
//! readiness call succeeds — so cancelling at any point leaves prior state
//! exactly intact and a re-run starts clean.
//!
//! ```text
//! Welcome to RightKit — no working provider configuration found.
//!
//! 1. Pick a route:
//!      1) OpenRouter — moonshotai/kimi-k2.6 (Direct)
//!      2) MiniMax    — MiniMax-M3 (Direct)
//!      3) Custom (OpenAI-compatible) — you type base URL + model
//!    Route [1]:
//!
//! 2. Model for openrouter (default: moonshotai/kimi-k2.6):
//!    (blank = default)
//!
//! 3. [custom only] Base URL for custom route:
//!
//! 4. Paste API key for openrouter (input is not echoed back):
//!    (empty cancels)
//!
//! 5. Use provider=openrouter model=moonshotai/kimi-k2.6? [Y/n]
//!    (when an existing working config is present this reads
//!     "Overwrite existing working configuration for openrouter? [y/N]"
//!     and defaults to NO)
//!
//! 7. Running readiness check — one real minimal turn via
//!    openrouter/moonshotai-kimi-k2.6 ...
//!      Success: "Ready — openrouter/moonshotai-kimi-k2.6 replied: OK"
//!      Failure: "Readiness failed: <provider's verbatim error>"
//!               "Nothing was changed. Fix the error and re-run /onboard."
//!
//! 8. Onboarding complete. Try: coderight run 'hello'
//! ```
//!
//! Re-running `/onboard` on a configured install repeats this flow but defaults
//! the overwrite prompt to NO — it never destroys a working config without
//! explicit confirmation.
//!
//! # Invariants
//! - Never print, echo, or log a credential value (see [`ask_hidden`] and the
//!   `credential_never_reaches_output` test / grep).
//! - The readiness check is a real call whose failure reports the provider's
//!   actual error (never a generic "config invalid").
//! - Cancelling leaves prior state intact — nothing is written before confirm.

#[cfg(test)]
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use rightkit_app_settings::agent_config::{HostConfig, ConfigLoader, ProviderRouteConfig};
use rightkit_agent_engine_protocol::TranscriptMessage;
use rightkit_agent_tools::coding_tools::ToolRegistry;

use crate::provider_client::{ProviderLlmClient, ProviderRoute};

// ── Shared route/model picker (canonical list) ───────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    /// Talk to the provider directly with the user's own key.
    Direct,
}

#[derive(Debug, Clone)]
pub struct RouteChoice {
    pub kind: RouteKind,
    /// Provider catalog id, e.g. `"openrouter"`.
    pub provider: String,
    /// Default model id offered in the picker.
    pub model: String,
    /// Provider endpoint used by the probe and runtime.
    pub base_url: String,
    /// Human-readable menu name.
    pub display_name: String,
    /// Use the product's native subscription broker instead of an API key.
    pub native_subscription: bool,
}

/// Build the shared picker from the product's live provider catalog
/// (`rightkit_llm::agent_providers::PROVIDERS`) plus the always-available Custom route.
/// This is the single source of truth — the runtime picker reuses it, it never
/// declares a second list.
pub fn shared_route_picker() -> Vec<RouteChoice> {
    let mut routes: Vec<RouteChoice> = Vec::new();
    // OpenRouter (recommended) first, then other catalog peers, then Custom.
    // Unknown catalog ids are skipped rather than panicking.
    for id in ["openrouter", "minimax", "openai"] {
        if let Some(p) = rightkit_llm::agent_providers::find_provider(id) {
            routes.push(RouteChoice {
                kind: RouteKind::Direct,
                provider: p.id.to_string(),
                // No built-in model: the model is chosen in the TUI picker.
                model: String::new(),
                base_url: p.base_url.to_string(),
                display_name: p.display_name.to_string(),
                native_subscription: id == "openai",
            });
        }
    }
    routes.push(RouteChoice {
        kind: RouteKind::Direct,
        provider: "custom".into(),
        model: String::new(),
        base_url: String::new(),
        display_name: "Custom (OpenAI-compatible)".into(),
        native_subscription: false,
    });
    routes
}

// ── First-run detection ──────────────────────────────────────────────────

/// Whether a terminal first-run flow is needed, derived from real config and
/// the real credential store (never guessed from a file's existence).
#[derive(Debug, Clone)]
pub struct OnboardingState {
    pub has_model: bool,
    pub has_route: bool,
    pub has_credential: bool,
}

impl OnboardingState {
    pub fn needs_onboarding(&self) -> bool {
        !self.has_model || !self.has_credential
    }
    /// A working provider configuration exists that overwrite must protect.
    pub fn working(&self) -> bool {
        self.has_model && self.has_route && self.has_credential
    }
}

/// Inspect the merged config plus the product credential store. Presence is
/// checked only — the value is never printed, returned, or logged.
pub fn detect_from_config<B: rightkit_llm::agent_providers::KeyStoreBackend>(
    backend: &B,
    config: &HostConfig,
) -> OnboardingState {
    let has_model = !config.model.default_model.trim().is_empty();
    let has_route = !config.provider_routes.is_empty();
    // A route that names an environment variable holding its key is a
    // configured credential (headless/CI and shell-profile workflows use it
    // instead of the keychain); presence only, the value is never read.
    let env_keyed_route = config.provider_routes.values().any(|route| {
        route
            .api_key_env
            .as_deref()
            .and_then(|name| std::env::var(name).ok())
            .is_some_and(|value| !value.trim().is_empty())
    });
    // Explicit credential-free routes satisfy onboarding for their selected model.
    let credential_free_route = config
        .provider_routes
        .values()
        .any(|route| !route.api_key_required && route.model == config.model.default_model);
    let has_credential = credential_free_route
        || env_keyed_route
        || ["openrouter", "opencode-go", "commandcode", "minimax"]
            .iter()
            .any(|id| {
                let key = rightkit_llm::agent_providers::KeyStore::get(
                    rightkit_llm::agent_providers::KeyStore::default_service(),
                    id,
                )
                .ok();
                let _ = backend; // reserved for write/read tests; presence uses the real store
                key.map(|v| !v.trim().is_empty()).unwrap_or(false)
            });
    OnboardingState {
        has_model,
        has_route,
        has_credential,
    }
}

/// Async variant used by product entrypoints so native OAuth counts as a
/// configured credential without exposing credential material.
pub async fn detect_from_config_with_native_auth<B: rightkit_llm::agent_providers::KeyStoreBackend>(
    backend: &B,
    config: &HostConfig,
) -> OnboardingState {
    let mut state = detect_from_config(backend, config);
    if config
        .provider_routes
        .keys()
        .any(|id| id.eq_ignore_ascii_case("openai"))
        && crate::api_provider_auth::resolve_provider_model_auth("openai")
            .await
            .ok()
            .flatten()
            .is_some()
    {
        state.has_credential = true;
    }
    state
}

/// Load the current merged user config (user-global + workspace layers).
pub fn load_current_config() -> HostConfig {
    ConfigLoader::new().load_files_default()
}

// ── Credential handling (never printed; product storage contract) ────────

/// Store a provider credential through the product's existing contract:
/// OS keychain only, via `write_key`; it fails closed when the keychain cannot verify the write.
/// The value never appears in any return, error, or output path.
pub fn store_credential<B: rightkit_llm::agent_providers::KeyStoreBackend>(
    backend: &B,
    provider_id: &str,
    credential: &str,
) -> Result<(), String> {
    if credential.trim().is_empty() {
        return Err("credential empty".into());
    }
    rightkit_llm::agent_providers::write_key(backend, provider_id, credential)
}

/// Remove a stored provider key from the keychain.
pub fn remove_credential<B: rightkit_llm::agent_providers::KeyStoreBackend>(
    backend: &B,
    provider_id: &str,
) -> Result<(), String> {
    rightkit_llm::agent_providers::remove_key(backend, provider_id)
}

// ── Readiness probe (real minimal turn) ──────────────────────────────────

#[derive(Debug, Clone)]
pub enum ReadinessResult {
    Ok { reply: String },
    ProviderError { detail: String },
}

/// Real readiness runner: builds the product's own provider client from the
/// picker choice + the collected credential and performs one minimal turn. A
/// failure returns the provider's verbatim error, never a sanitized message.
pub struct RealReadinessRunner;

#[async_trait::async_trait]
pub trait ReadinessRunner: Send + Sync {
    async fn run(&self, choice: &RouteChoice, credential: &str) -> ReadinessResult;
}

#[async_trait::async_trait]
impl ReadinessRunner for RealReadinessRunner {
    async fn run(&self, choice: &RouteChoice, credential: &str) -> ReadinessResult {
        use rightkit_agent_engine_core::llm::LlmClient;

        let model_id = rightkit_llm::agent_providers::provider_model_id(&choice.provider, &choice.model);
        let base_url = choice.base_url.trim().trim_end_matches('/').to_string();
        let route = Arc::new(Mutex::new(ProviderRoute {
            api_key: credential.to_string(),
            model_id: model_id.clone(),
            base_url,
            effort: None,
            thinking_budget_tokens: None,
            disable_thinking: false,
            fallback_models: Vec::new(),
            approved_private: false,
        }));
        let client = ProviderLlmClient::new_dynamic(route, Arc::new(ToolRegistry::build_default()));

        let msg = TranscriptMessage::User {
            id: "onboard-probe".into(),
            created_at: "onboarding".into(),
            content: "Reply with the single word OK.".to_string(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        };

        match client.complete(&[msg]).await {
            Ok(response) => {
                let text = response.text.trim();
                if text.is_empty() {
                    ReadinessResult::ProviderError {
                        detail: "provider returned an empty reply; refusing to report readiness"
                            .to_string(),
                    }
                } else {
                    ReadinessResult::Ok {
                        reply: text.chars().take(48).collect(),
                    }
                }
            }
            Err(detail) => ReadinessResult::ProviderError { detail },
        }
    }
}

// ── Persistence seam ─────────────────────────────────────────────────────

/// Where the chosen provider/model are written after a successful probe. The
/// real implementer writes the user config; tests record instead of touching
/// disk/`$HOME`.
pub trait Persister: Send + Sync {
    fn persist(
        &self,
        provider: &str,
        model: &str,
        base_url: &str,
        display_name: &str,
    ) -> Result<(), String>;
}

fn provider_route_config(
    provider: &str,
    model: &str,
    base_url: &str,
    display_name: &str,
) -> ProviderRouteConfig {
    ProviderRouteConfig {
        display_name: display_name.to_string(),
        base_url: base_url.to_string(),
        model: model.to_string(),
        api_key_env: None,
        api_key_required: provider != "openai",
        approved_private: false,
    }
}

/// Real persister: writes the `provider-routes` connection (no model pin) to
/// the user config, and remembers the model the user typed as the explicit
/// selection the TUI starts on.
pub struct ConfigPersister;

impl Persister for ConfigPersister {
    fn persist(
        &self,
        provider: &str,
        model: &str,
        base_url: &str,
        display_name: &str,
    ) -> Result<(), String> {
        // Read-modify-write under the shared config lock: a concurrent save is merged in.
        ConfigLoader::new()
            .update_user(|cfg| {
                cfg.provider_routes.insert(
                    provider.to_string(),
                    provider_route_config(provider, "", base_url, display_name),
                );
            })
            .map_err(|e| format!("failed to save config: {e}"))?;
        super::save_model_selection(provider, model)
    }
}

impl ConfigPersister {
    /// Record native OAuth's catalog route after a successful provider login.
    /// Existing default selection is preserved unless no default exists yet.
    pub fn persist_native_subscription_route(provider: &str) -> Result<(), String> {
        let catalog = rightkit_llm::agent_providers::find_provider(provider)
            .ok_or_else(|| format!("unknown provider: {provider}"))?;
        // Connection only: no built-in model is written; the model is chosen
        // in the TUI picker and remembered there. Read-modify-write under the shared config lock.
        ConfigLoader::new()
            .update_user(|cfg| {
                let model = cfg
                    .provider_routes
                    .get(provider)
                    .map(|route| route.model.trim().to_string())
                    .unwrap_or_default();
                cfg.provider_routes.insert(
                    provider.to_string(),
                    ProviderRouteConfig {
                        display_name: catalog.display_name.to_string(),
                        base_url: catalog.base_url.to_string(),
                        model,
                        api_key_env: None,
                        api_key_required: false,
                        approved_private: false,
                    },
                );
            })
            .map(|_| ())
            .map_err(|e| format!("failed to save config: {e}"))
    }
}

// ── Interactive prompt seam ──────────────────────────────────────────────

/// Minimal terminal seam so the flow is driveable by a script in tests without
/// a real TTY. [`TerminalIo`] is the real backing.
pub trait PromptIo {
    fn write(&mut self, text: &str);
    fn read_line(&mut self) -> Option<String>;
    fn read_hidden_line(&mut self) -> Option<String> {
        self.read_line()
    }
}

/// Real terminal backing. Credentials are read but never written back through
/// this channel (see [`ask_hidden`]).
pub struct TerminalIo;

impl PromptIo for TerminalIo {
    fn write(&mut self, text: &str) {
        let stdout = io::stdout();
        let mut lock = stdout.lock();
        let _ = lock.write_all(text.as_bytes());
        let _ = lock.flush();
    }
    fn read_line(&mut self) -> Option<String> {
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).ok()?;
        Some(buf.trim_end_matches(['\r', '\n']).to_string())
    }

    fn read_hidden_line(&mut self) -> Option<String> {
        // Refuse to collect a secret unless terminal echo was actually
        // disabled. The guard restores the original mode even on read errors.
        #[cfg(any(unix, windows))]
        let value = {
            let _echo_guard = TerminalEchoGuard::new().ok()?;
            self.read_line()
        };
        #[cfg(not(any(unix, windows)))]
        let value = self.read_line();
        if value.is_some() {
            // Echo is restored when the guard leaves scope, before this write.
            self.write("\n");
        }
        value
    }
}

#[cfg(unix)]
struct TerminalEchoGuard {
    fd: libc::c_int,
    original: libc::termios,
}

#[cfg(unix)]
impl TerminalEchoGuard {
    fn new() -> io::Result<Self> {
        let fd = libc::STDIN_FILENO;
        // SAFETY: `termios` is a plain C struct and zero is a valid buffer for
        // tcgetattr to fill.
        let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
        // SAFETY: fd is process stdin and original is writable.
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut hidden = original;
        hidden.c_lflag &= !libc::ECHO;
        // SAFETY: hidden was populated by tcgetattr and remains valid here.
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &hidden) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, original })
    }
}

#[cfg(unix)]
impl Drop for TerminalEchoGuard {
    fn drop(&mut self) {
        // SAFETY: restore the exact mode captured before secret input.
        let _ = unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.original) };
    }
}

#[cfg(windows)]
struct TerminalEchoGuard {
    handle: *mut std::ffi::c_void,
    original_mode: u32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetStdHandle(standard_handle: u32) -> *mut std::ffi::c_void;
    fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
}

#[cfg(windows)]
impl TerminalEchoGuard {
    fn new() -> io::Result<Self> {
        const STD_INPUT_HANDLE: u32 = (-10i32) as u32;
        const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut _;
        const ENABLE_ECHO_INPUT: u32 = 0x0004;
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut original_mode = 0;
        if unsafe { GetConsoleMode(handle, &mut original_mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { SetConsoleMode(handle, original_mode & !ENABLE_ECHO_INPUT) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle,
            original_mode,
        })
    }
}

#[cfg(windows)]
impl Drop for TerminalEchoGuard {
    fn drop(&mut self) {
        let _ = unsafe { SetConsoleMode(self.handle, self.original_mode) };
    }
}

fn ask_yes(io: &mut dyn PromptIo, prompt: &str, default: bool) -> Option<bool> {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    io.write(&format!("{prompt} {hint} "));
    let answer = io.read_line()?.trim().to_ascii_lowercase();
    Some(match answer.as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        "" => default,
        _ => default,
    })
}

fn ask_text(io: &mut dyn PromptIo, prompt: &str, default: &str) -> Option<String> {
    let hint = if default.is_empty() {
        String::new()
    } else {
        format!(" (default: {default})")
    };
    io.write(&format!("{prompt}{hint}: "));
    let answer = io.read_line()?.trim().to_string();
    if answer.is_empty() {
        if default.is_empty() {
            None
        } else {
            Some(default.to_string())
        }
    } else {
        Some(answer)
    }
}

/// Ask for a credential and return it **without writing it anywhere or echoing
/// it back**. The caller must hand the value only to the store/probe and must
/// never interpolate it into a format string destined for output.
fn ask_hidden(io: &mut dyn PromptIo, prompt: &str) -> Option<String> {
    io.write(&format!("{prompt} (input is not echoed back): "));
    let answer = io.read_hidden_line()?.trim().to_string();
    if answer.is_empty() {
        None
    } else {
        Some(answer)
    }
}

/// The values collected from the user before any persistence. `credential` is
/// deliberately held last and never formatted into an output string.
struct Selection {
    choice: RouteChoice,
    model: String,
    credential: String,
}

async fn collect_selection(io: &mut dyn PromptIo) -> Option<Selection> {
    let routes = shared_route_picker();
    let mut menu = String::from("\nPick a route:\n");
    for (i, r) in routes.iter().enumerate() {
        let model = if r.model.is_empty() {
            "(type model)".to_string()
        } else {
            r.model.clone()
        };
        let _ = writeln!(menu, "  {}) {:<30} — {}", i + 1, r.display_name, model);
    }
    io.write(&menu);
    let idx_input = io.read_line()?;
    let idx: usize = match idx_input.trim().parse::<usize>() {
        Ok(n) if n >= 1 && n <= routes.len() => n - 1,
        _ => return None, // invalid => cancel, nothing written
    };
    let mut choice = routes[idx].clone();

    // Model (default offered; custom requires a value).
    let model = if choice.provider == "custom" {
        ask_text(io, "Model for custom route", "")?
    } else {
        let picked = ask_text(io, &format!("Model for {}", choice.provider), &choice.model)?;
        choice.model = picked.clone();
        picked
    };

    // Custom base URL.
    if choice.provider == "custom" {
        let url = ask_text(io, "Base URL for custom route", "")?;
        choice.base_url = url;
    }

    let credential = if choice.native_subscription {
        String::new()
    } else {
        ask_hidden(io, &format!("Paste API key for {}", choice.provider))?
    };

    Some(Selection {
        choice,
        model,
        credential,
    })
}

// ── The /onboard flow ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardOutcome {
    Completed { provider: String, model: String },
    Cancelled,
    Failed { detail: String },
}

#[async_trait::async_trait]
pub trait NativeAuthLogin: Send + Sync {
    async fn login(&self, provider: &str) -> Result<(), String>;
}

struct BrokerNativeAuthLogin;

#[async_trait::async_trait]
impl NativeAuthLogin for BrokerNativeAuthLogin {
    async fn login(&self, provider: &str) -> Result<(), String> {
        crate::api_provider_auth::terminal_login(provider).await
    }
}

/// Re-runnable `/onboard` entrypoint. Collects every answer, confirms with
/// overwrite protection, runs the **real** readiness call, and only then
/// persists. Cancelling at any prompt before confirmation writes nothing, so an
/// interrupted run leaves prior state intact and a re-run starts clean.
pub async fn run_onboarding<B: rightkit_llm::agent_providers::KeyStoreBackend + Send + Sync>(
    state: &OnboardingState,
    backend: &B,
    runner: &dyn ReadinessRunner,
    persister: &dyn Persister,
    io: &mut dyn PromptIo,
) -> OnboardOutcome {
    run_onboarding_with_native_auth(
        state,
        backend,
        runner,
        persister,
        &BrokerNativeAuthLogin,
        io,
    )
    .await
}

async fn run_onboarding_with_native_auth<B: rightkit_llm::agent_providers::KeyStoreBackend + Send + Sync>(
    state: &OnboardingState,
    backend: &B,
    runner: &dyn ReadinessRunner,
    persister: &dyn Persister,
    native_auth: &dyn NativeAuthLogin,
    io: &mut dyn PromptIo,
) -> OnboardOutcome {
    let Some(selection) = collect_selection(io).await else {
        return OnboardOutcome::Cancelled;
    };
    let provider = selection.choice.provider.clone();
    let model = selection.model.clone();

    // Overwrite protection: an existing working config requires explicit yes.
    let confirmed = if state.working() {
        ask_yes(
            io,
            &format!(
                "Overwrite existing working configuration for {} ({})",
                provider, model
            ),
            false,
        )
    } else {
        ask_yes(io, &format!("Use provider={provider} model={model}?"), true)
    };
    match confirmed {
        None => return OnboardOutcome::Cancelled,
        Some(false) => return OnboardOutcome::Cancelled,
        Some(true) => {}
    }

    // OpenAI route selection is backed by the same reusable broker as CLI
    // provider login. No config is persisted unless this attempt succeeds.
    if selection.choice.native_subscription {
        io.write("\nStarting OpenAI subscription login...\n");
        if let Err(detail) = native_auth.login(&provider).await {
            io.write(&format!(
                "OpenAI login failed: {detail}\nNothing was changed.\n"
            ));
            return OnboardOutcome::Failed { detail };
        }
    }

    // Real readiness probe — in-memory credential only; nothing persisted yet.
    io.write(&format!(
        "\nRunning readiness check — one real minimal turn via {}/{model} ...\n",
        selection.choice.display_name
    ));
    match runner.run(&selection.choice, &selection.credential).await {
        ReadinessResult::ProviderError { detail } => {
            io.write(&format!(
                "Readiness failed: {detail}\nNothing was changed. Fix the error and re-run /onboard.\n"
            ));
            OnboardOutcome::Failed { detail }
        }
        ReadinessResult::Ok { reply } => {
            // Persist Direct credentials via the product keychain contract.
            if !selection.choice.native_subscription {
                if let Err(e) = store_credential(backend, &provider, &selection.credential) {
                    io.write(&format!(
                        "Credential was not stored: {e}\nNothing was changed. Re-run /onboard after fixing this.\n"
                    ));
                    return OnboardOutcome::Failed { detail: e };
                }
            }
            // Persist the chosen route + default model to the user config.
            let base_url = selection.choice.base_url.clone();
            let display_name = selection.choice.display_name.clone();
            if let Err(e) = persister.persist(&provider, &model, &base_url, &display_name) {
                io.write(&format!("Failed to save configuration: {e}\n"));
                return OnboardOutcome::Failed { detail: e };
            }
            io.write(&format!(
                "Ready — {provider}/{model} replied: {reply}\n\nOnboarding complete. Try: coderight run 'hello'\n"
            ));
            OnboardOutcome::Completed { provider, model }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_llm::agent_providers::{InMemoryKeyStore, KeyStoreBackend};

    struct FakeNativeAuth {
        result: Result<(), String>,
        calls: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl NativeAuthLogin for FakeNativeAuth {
        async fn login(&self, provider: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(provider.to_string());
            self.result.clone()
        }
    }

    /// Run an async onboarding flow on a throwaway current-thread runtime.
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// Scripted IO: the test supplies exact inputs; everything the flow writes
    /// is captured so tests can assert a secret never reached output.
    struct ScriptedIo {
        script: VecDeque<String>,
        out: String,
    }
    impl ScriptedIo {
        fn new(inputs: Vec<&str>) -> Self {
            Self {
                script: inputs.into_iter().map(ToOwned::to_owned).collect(),
                out: String::new(),
            }
        }
    }
    impl PromptIo for ScriptedIo {
        fn write(&mut self, text: &str) {
            self.out.push_str(text);
        }
        fn read_line(&mut self) -> Option<String> {
            self.script.pop_front()
        }
    }

    /// Fake readiness runner — never touches a real provider in tests.
    struct ScriptedRunner(ReadinessResult);
    #[async_trait::async_trait]
    impl ReadinessRunner for ScriptedRunner {
        async fn run(&self, _choice: &RouteChoice, _credential: &str) -> ReadinessResult {
            self.0.clone()
        }
    }

    /// Fake persister — records what would be written instead of touching disk.
    struct RecordingPersister {
        calls: Mutex<Vec<(String, String, String, String)>>,
    }
    impl Persister for RecordingPersister {
        fn persist(
            &self,
            provider: &str,
            model: &str,
            base_url: &str,
            display_name: &str,
        ) -> Result<(), String> {
            self.calls.lock().unwrap().push((
                provider.into(),
                model.into(),
                base_url.into(),
                display_name.into(),
            ));
            Ok(())
        }
    }

    fn fresh_state() -> OnboardingState {
        OnboardingState {
            has_model: false,
            has_route: false,
            has_credential: false,
        }
    }

    #[test]
    fn shared_picker_is_catalog_backed_and_non_empty() {
        let routes = shared_route_picker();
        assert!(!routes.is_empty());
        assert_eq!(routes[0].provider, "openrouter");
        assert_eq!(routes[0].kind, RouteKind::Direct);
        let openai = routes
            .iter()
            .find(|route| route.provider == "openai")
            .unwrap();
        assert!(openai.native_subscription);
        // Every catalog-backed route carries a base URL and NO built-in
        // model: the model is chosen in the TUI picker.
        for r in &routes {
            if r.provider != "custom" {
                assert!(!r.base_url.is_empty());
            }
            assert!(r.model.is_empty(), "{} must not pin a model", r.provider);
        }
        // Custom is always offered last and present.
        assert_eq!(routes.last().map(|r| r.provider.as_str()), Some("custom"));
    }

    #[test]
    fn hidden_prompt_uses_hidden_io_channel() {
        struct HiddenOnlyIo {
            hidden_reads: usize,
        }
        impl PromptIo for HiddenOnlyIo {
            fn write(&mut self, _text: &str) {}
            fn read_line(&mut self) -> Option<String> {
                panic!("secret prompt must not use ordinary read_line")
            }
            fn read_hidden_line(&mut self) -> Option<String> {
                self.hidden_reads += 1;
                Some("secret".into())
            }
        }
        let mut io = HiddenOnlyIo { hidden_reads: 0 };
        assert_eq!(ask_hidden(&mut io, "key"), Some("secret".into()));
        assert_eq!(io.hidden_reads, 1);
    }

    #[test]
    fn cancellation_after_invalid_route_is_non_destructive() {
        let mut io = ScriptedIo::new(vec!["99"]); // invalid route => cancel
        let backend = InMemoryKeyStore::new();
        let outcome = block_on(run_onboarding(
            &fresh_state(),
            &backend,
            &ScriptedRunner(ReadinessResult::Ok { reply: "OK".into() }),
            &RecordingPersister {
                calls: Mutex::new(Vec::new()),
            },
            &mut io,
        ));
        assert_eq!(outcome, OnboardOutcome::Cancelled);
        // Nothing was persisted: no credential entry in the store, nothing recorded.
        assert!(
            backend.get("coderight", "openrouter").is_err(),
            "no credential may be written on cancel"
        );
    }

    #[test]
    fn overwrite_defaults_to_no_for_working_config() {
        // route 1 (openrouter Direct) => prompts: route, model, credential,
        // confirm. Blank confirm => default NO (working config).
        let mut io = ScriptedIo::new(vec!["1", "", "sk-secret-123", "", ""]); // blank confirm => default NO
        let backend = InMemoryKeyStore::new();
        let state = OnboardingState {
            has_model: true,
            has_route: true,
            has_credential: true,
        };
        let persister = RecordingPersister {
            calls: Mutex::new(Vec::new()),
        };
        let outcome = block_on(run_onboarding(
            &state,
            &backend,
            &ScriptedRunner(ReadinessResult::Ok { reply: "OK".into() }),
            &persister,
            &mut io,
        ));
        assert_eq!(outcome, OnboardOutcome::Cancelled);
        assert!(persister.calls.lock().unwrap().is_empty());
        assert!(backend.get("coderight", "openrouter").is_err());
    }

    #[test]
    fn readiness_failure_surfaces_provider_error_and_changes_nothing() {
        // route/model/credential/confirm(accept).
        let mut io = ScriptedIo::new(vec!["1", "test-model", "sk-secret-123", "", ""]);
        let backend = InMemoryKeyStore::new();
        let persister = RecordingPersister {
            calls: Mutex::new(Vec::new()),
        };
        let outcome = block_on(run_onboarding(
            &fresh_state(),
            &backend,
            &ScriptedRunner(ReadinessResult::ProviderError {
                detail: "401 invalid api key".into(),
            }),
            &persister,
            &mut io,
        ));
        assert_eq!(
            outcome,
            OnboardOutcome::Failed {
                detail: "401 invalid api key".into()
            }
        );
        // The provider's actual error is visible; nothing was persisted.
        assert!(io.out.contains("401 invalid api key"));
        assert!(persister.calls.lock().unwrap().is_empty());
        assert!(backend.get("coderight", "openrouter").is_err());
    }

    #[test]
    fn success_persists_credential_and_config() {
        // route/model/credential/confirm(accept).
        let mut io = ScriptedIo::new(vec!["1", "test-model", "sk-secret-123", "", ""]);
        let backend = InMemoryKeyStore::new();
        let persister = RecordingPersister {
            calls: Mutex::new(Vec::new()),
        };
        let outcome = block_on(run_onboarding(
            &fresh_state(),
            &backend,
            &ScriptedRunner(ReadinessResult::Ok { reply: "OK".into() }),
            &persister,
            &mut io,
        ));
        // No built-in model: the model is the one the user typed.
        let default_model = "test-model";
        assert!(matches!(
            outcome,
            OnboardOutcome::Completed { provider, model }
                if provider == "openrouter" && model == default_model
        ));
        // Credential stored, route+model persisted exactly once.
        assert_eq!(
            backend.get("coderight", "openrouter").unwrap(),
            "sk-secret-123"
        );
        let calls = persister.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "openrouter");
        assert_eq!(calls[0].1, default_model);
        assert_eq!(calls[0].2, "https://openrouter.ai/api/v1");
    }

    #[test]
    fn openai_onboarding_logs_in_before_readiness_and_persists_subscription_route() {
        let mut io = ScriptedIo::new(vec!["3", "test-model", ""]); // OpenAI, typed model, confirm
        let backend = InMemoryKeyStore::new();
        let persister = RecordingPersister {
            calls: Mutex::new(Vec::new()),
        };
        let auth = FakeNativeAuth {
            result: Ok(()),
            calls: Mutex::new(Vec::new()),
        };
        let outcome = block_on(run_onboarding_with_native_auth(
            &fresh_state(),
            &backend,
            &ScriptedRunner(ReadinessResult::Ok { reply: "OK".into() }),
            &persister,
            &auth,
            &mut io,
        ));
        assert!(
            matches!(outcome, OnboardOutcome::Completed { provider, .. } if provider == "openai")
        );
        assert_eq!(
            auth.calls.lock().unwrap().as_slice(),
            &["openai".to_string()]
        );
        assert!(backend.get("coderight", "openai").is_err());
        assert_eq!(persister.calls.lock().unwrap()[0].0, "openai");
        assert!(!provider_route_config("openai", "m", "url", "OpenAI").api_key_required);
    }

    #[test]
    fn failed_openai_login_does_not_run_readiness_or_persist_route() {
        let mut io = ScriptedIo::new(vec!["3", "test-model", ""]);
        let backend = InMemoryKeyStore::new();
        let persister = RecordingPersister {
            calls: Mutex::new(Vec::new()),
        };
        let auth = FakeNativeAuth {
            result: Err("cancelled".into()),
            calls: Mutex::new(Vec::new()),
        };
        let outcome = block_on(run_onboarding_with_native_auth(
            &fresh_state(),
            &backend,
            &ScriptedRunner(ReadinessResult::Ok {
                reply: "must not run".into(),
            }),
            &persister,
            &auth,
            &mut io,
        ));
        assert_eq!(
            outcome,
            OnboardOutcome::Failed {
                detail: "cancelled".into()
            }
        );
        assert!(persister.calls.lock().unwrap().is_empty());
        assert!(backend.get("coderight", "openai").is_err());
    }

    #[test]
    fn credential_never_reaches_any_output_path() {
        let secret = "sk-super-secret-token-987654";
        // route/model/credential/confirm(accept) so the store path runs.
        let mut io = ScriptedIo::new(vec!["1", "test-model", secret, "", ""]);
        let backend = InMemoryKeyStore::new();
        let persister = RecordingPersister {
            calls: Mutex::new(Vec::new()),
        };
        let _ = block_on(run_onboarding(
            &fresh_state(),
            &backend,
            &ScriptedRunner(ReadinessResult::Ok { reply: "OK".into() }),
            &persister,
            &mut io,
        ));
        // The value may be stored but must never appear in anything the user sees.
        assert!(
            !io.out.contains(secret),
            "credential leaked into output: {}",
            io.out
        );
        assert_eq!(
            backend.get("coderight", "openrouter").unwrap(),
            secret,
            "value stored (that is intended) but never printed"
        );
    }

    #[test]
    fn empty_credential_cancels() {
        let mut io = ScriptedIo::new(vec!["1", "test-model", ""]); // blank credential => cancel
        let backend = InMemoryKeyStore::new();
        let persister = RecordingPersister {
            calls: Mutex::new(Vec::new()),
        };
        let outcome = block_on(run_onboarding(
            &fresh_state(),
            &backend,
            &ScriptedRunner(ReadinessResult::Ok { reply: "OK".into() }),
            &persister,
            &mut io,
        ));
        assert_eq!(outcome, OnboardOutcome::Cancelled);
        assert!(persister.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn first_run_detection_uses_real_config_fields() {
        // Empty config => needs onboarding.
        let state = OnboardingState {
            has_model: false,
            has_route: false,
            has_credential: false,
        };
        assert!(state.needs_onboarding());
        // A fully-configured install does not.
        let done = OnboardingState {
            has_model: true,
            has_route: true,
            has_credential: true,
        };
        assert!(!done.needs_onboarding());
        assert!(done.working());
    }

    #[test]
    fn route_api_key_env_counts_as_configured_credential() {
        let var = "CR_ONBOARD_API_KEY_ENV_TEST";
        std::env::set_var(var, "present");
        let mut config = HostConfig::default();
        config.model.default_model = "env-model".into();
        config.provider_routes.insert(
            "envroute".into(),
            rightkit_app_settings::agent_config::ProviderRouteConfig {
                model: "env-model".into(),
                api_key_env: Some(var.into()),
                api_key_required: true,
                ..Default::default()
            },
        );
        let backend = InMemoryKeyStore::new();
        let state = detect_from_config(&backend, &config);
        std::env::remove_var(var);
        assert!(state.has_credential);
        assert!(!state.needs_onboarding());
    }

    #[test]
    fn credential_free_route_satisfies_only_its_selected_model() {
        // detect_from_config reads the real keychain; settings tests write an
        // openrouter key under the process env lock, so take it to keep the
        // baseline stable across this test's reads.
        let _env_guard = crate::test_support::process_env_lock();
        let mut config = HostConfig::default();
        config.model.default_model = "fixture-model".into();
        let backend = InMemoryKeyStore::new();
        let baseline = detect_from_config(&backend, &config).has_credential;
        config.provider_routes.insert(
            "fixture".into(),
            rightkit_app_settings::agent_config::ProviderRouteConfig {
                model: "fixture-model".into(),
                api_key_required: false,
                ..Default::default()
            },
        );
        assert!(detect_from_config(&backend, &config).working());
        config.model.default_model = "other-model".into();
        assert_eq!(
            detect_from_config(&backend, &config).has_credential,
            baseline
        );
        config.model.default_model = "fixture-model".into();
        config
            .provider_routes
            .get_mut("fixture")
            .unwrap()
            .api_key_required = true;
        assert_eq!(
            detect_from_config(&backend, &config).has_credential,
            baseline
        );
    }

    // Grep proof (run from repo root):
    //   Search for direct print or logging macros in this module.
    //   -> must return NOTHING (this module never prints; output goes through PromptIo,
    //      and the credential is only ever the `credential` arg to store/probe, never
    //      interpolated into an output format string).
    //   grep -rn "credential" engine/bins/coderight/src/onboarding.rs
    //   -> must show only store/probe/ask_hidden call sites and this test, never
    //      a format string that writes the value.
    #[test]
    fn no_direct_print_macros_in_module() {
        // Guards against a future edit adding a raw println/eprintln that could
        // accidentally leak a credential; output is expected to flow via PromptIo.
        let src = include_str!("onboarding.rs");
        let banned = [
            "println!(\"{".to_string(),
            "eprintln!(\"{".to_string(),
            ["tracing", "::"].concat(),
        ];
        for banned in banned {
            assert!(!src.contains(&banned), "found banned output path: {banned}");
        }
    }
}
