//! The `/providers` screen's backing logic, shared by the TUI host and the
//! `{app} provider ...` CLI so both call the same in-process functions
//! (no daemon): the fixed provider list, status projection, API-key
//! add/remove, the connection test, and vendor-CLI facts.
//!
//! Declared inside `mod main_runtime` (next to `onboarding`) so it can reuse
//! the onboarding credential helpers. Credential values only ever flow into
//! the keychain writer or an `Authorization` header — never into a return
//! value, log line, or error string.

use std::time::Duration;

use rightkit_terminal_ui::nav::{ProviderAction, ProviderChoice, ProviderKind, ProviderModel};

use super::onboarding::{RouteChoice, RouteKind};

/// One provider on the `/providers` screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenProvider {
    pub id: &'static str,
    pub display: &'static str,
    pub kind: ProviderKind,
}

/// Exactly the providers `/providers` and `/model` show, in screen order.
/// minimax / opencode / custom are intentionally absent: their code and
/// stored keys stay, they are just not offered.
pub const SCREEN_PROVIDERS: [ScreenProvider; 6] = [
    ScreenProvider {
        id: "openai",
        display: "OpenAI (ChatGPT sign-in)",
        kind: ProviderKind::OAuth,
    },
    ScreenProvider {
        id: "claude-code",
        display: "Claude Code (CLI)",
        kind: ProviderKind::VendorCli,
    },
    ScreenProvider {
        id: "antigravity",
        display: "Antigravity (CLI)",
        kind: ProviderKind::VendorCli,
    },
    ScreenProvider {
        id: "opencode-go",
        display: "OpenCode Go (API key)",
        kind: ProviderKind::ApiKey,
    },
    ScreenProvider {
        id: "commandcode",
        display: "Command Code (API key)",
        kind: ProviderKind::ApiKey,
    },
    ScreenProvider {
        id: "openrouter",
        display: "OpenRouter (API key)",
        kind: ProviderKind::ApiKey,
    },
];

pub fn screen_provider(id: &str) -> Option<&'static ScreenProvider> {
    SCREEN_PROVIDERS
        .iter()
        .find(|provider| provider.id.eq_ignore_ascii_case(id))
}

/// Route-picker rows for the catalog-backed (non vendor-CLI) screen
/// providers. This feeds `/model`'s catalog build, so hidden providers are
/// absent from `/model` as well as `/providers`.
pub fn screen_route_choices() -> Vec<RouteChoice> {
    SCREEN_PROVIDERS
        .iter()
        .filter(|provider| provider.kind != ProviderKind::VendorCli)
        .filter_map(|provider| {
            let catalog = rightkit_llm::agent_providers::find_provider(provider.id)?;
            Some(RouteChoice {
                kind: RouteKind::Direct,
                provider: catalog.id.to_string(),
                model: String::new(),
                base_url: catalog.base_url.to_string(),
                display_name: provider.display.to_string(),
                native_subscription: provider.kind == ProviderKind::OAuth,
            })
        })
        .collect()
}

/// What detection learned about one vendor CLI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VendorFacts {
    pub installed: bool,
    pub version: Option<String>,
    /// `Some(true)` only when the CLI itself reports a signed-in account.
    pub signed_in: Option<bool>,
}

/// Level-1 status text. Pure: the screen contract, unit-tested.
pub fn project_status(
    kind: ProviderKind,
    key_set: bool,
    oauth_signed_in: bool,
    vendor: Option<&VendorFacts>,
) -> &'static str {
    match kind {
        ProviderKind::OAuth => {
            if oauth_signed_in {
                "signed in"
            } else {
                "not set up"
            }
        }
        ProviderKind::ApiKey => {
            if key_set {
                "key set"
            } else {
                "not set up"
            }
        }
        ProviderKind::VendorCli => match vendor {
            Some(facts) if facts.installed => match facts.signed_in {
                Some(true) => "installed · signed in",
                Some(false) => "installed · not signed in",
                None => "installed",
            },
            _ => "not installed",
        },
    }
}

/// `Name (kind) · status`, the level-1 row text.
pub fn screen_label(provider: &ScreenProvider, status: &str) -> String {
    format!("{} · {}", provider.display, status)
}

// ── Vendor CLIs (detection lives in {app}-driver-host) ───────────────

fn vendor_cli(provider_id: &str) -> Option<rightkit_agents::driver_host::vendor_cli::VendorCli> {
    rightkit_agents::driver_host::vendor_cli::VendorCli::from_provider_id(provider_id)
}

/// Detect one vendor CLI. Unknown ids read as not installed.
pub fn vendor_facts(provider_id: &str) -> VendorFacts {
    match vendor_cli(provider_id) {
        Some(kind) => {
            let status = rightkit_agents::driver_host::vendor_cli::detect(kind);
            VendorFacts {
                installed: status.installed,
                version: status.version,
                signed_in: status.signed_in,
            }
        }
        None => VendorFacts::default(),
    }
}

/// `(install_hint, login_hint)` for a vendor CLI.
pub fn vendor_hints(provider_id: &str) -> Option<(String, String)> {
    vendor_cli(provider_id).map(|kind| {
        (
            kind.install_hint().to_string(),
            kind.login_hint().to_string(),
        )
    })
}

/// Live model list from the vendor CLI as `(id, display)`; empty when the
/// binary is missing.
pub fn vendor_models(provider_id: &str) -> Vec<(String, String)> {
    vendor_cli(provider_id)
        .map(rightkit_agents::driver_host::vendor_cli::list_models)
        .unwrap_or_default()
}

/// Human status block for a vendor CLI (the `Status` action).
pub fn vendor_status_text(provider_id: &str, facts: &VendorFacts) -> String {
    let (install, login) = vendor_hints(provider_id).unwrap_or_default();
    if !facts.installed {
        return format!("Not installed. {install}");
    }
    let version = facts.version.as_deref().unwrap_or("version unknown");
    let signed = match facts.signed_in {
        Some(true) => "signed in",
        Some(false) => "not signed in",
        None => "sign-in state unknown",
    };
    format!("Installed ({version}), {signed}. {login}")
}

/// Fill a vendor-CLI [`ProviderChoice`] from detection results: status,
/// note (install hint when missing, login command otherwise), and the
/// `provider/<model>` route rows. Pure so it is unit-testable.
pub fn apply_vendor_facts(
    choice: &mut ProviderChoice,
    facts: &VendorFacts,
    hints: &(String, String),
    models: &[(String, String)],
    hidden: &[String],
    committed: Option<(&str, &str)>,
) {
    let Some(screen) = screen_provider(&choice.id) else {
        return;
    };
    let status = project_status(ProviderKind::VendorCli, false, false, Some(facts));
    choice.label = screen_label(screen, status);
    choice.kind = ProviderKind::VendorCli;
    choice.configured = facts.installed;
    choice.ready = facts.installed && facts.signed_in == Some(true);
    choice.note = Some(if facts.installed {
        hints.1.clone()
    } else {
        hints.0.clone()
    });
    choice.models = if facts.installed {
        models
            .iter()
            .map(|(id, display)| {
                let route_id = format!("{}/{}", choice.id, id);
                let is_current = committed.is_some_and(|(model, provider)| {
                    provider == choice.id && (model == id || model == route_id)
                });
                ProviderModel {
                    model_id: id.clone(),
                    hidden: hidden.iter().any(|h| h == id),
                    choice: rightkit_terminal_ui::nav::ModelChoice::new(route_id, display.clone())
                        .provider(choice.id.clone())
                        .current(is_current),
                }
            })
            .collect()
    } else {
        Vec::new()
    };
}

// ── API keys ─────────────────────────────────────────────────────────────

/// Whether a key is available for this provider (keychain, legacy file, or
/// the provider's env var). Presence only.
pub fn key_is_set(provider_id: &str) -> bool {
    rightkit_llm::agent_providers::load_api_key(provider_id).is_some()
}

pub fn store_key(provider_id: &str, key: &str) -> Result<(), String> {
    super::onboarding::store_credential(
        &rightkit_llm::agent_providers::OsKeyStore,
        provider_id,
        key.trim(),
    )
}

pub fn remove_key(provider_id: &str) -> Result<(), String> {
    super::onboarding::remove_credential(&rightkit_llm::agent_providers::OsKeyStore, provider_id)
}

// ── Test connection ──────────────────────────────────────────────────────

/// The one request "Test connection" sends: an authenticated, read-only,
/// non-chat GET. Never a completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRequest {
    pub method: &'static str,
    pub url: String,
}

pub fn test_request(provider_id: &str) -> Result<ProbeRequest, String> {
    let catalog = rightkit_llm::agent_providers::find_provider(provider_id)
        .filter(|p| !p.base_url.is_empty())
        .ok_or_else(|| format!("{provider_id} has no API endpoint to test"))?;
    let base = catalog.base_url.trim_end_matches('/');
    // OpenRouter's /models is public (it would pass with no key), so use
    // its authenticated key-info route; the others list models with auth.
    let path = if provider_id == "openrouter" {
        "/key"
    } else {
        "/models"
    };
    Ok(ProbeRequest {
        method: "GET",
        url: format!("{base}{path}"),
    })
}

/// Map an HTTP status to the user-facing verdict.
pub fn probe_verdict(status: u16) -> Result<String, String> {
    match status {
        200..=299 => Ok("Connection OK: the key was accepted.".to_string()),
        401 | 403 => Err(format!("The provider rejected the key (HTTP {status}).")),
        404 => Err(
            "The provider answered 404 for its models route; the key could not be verified."
                .to_string(),
        ),
        429 => Ok(
            "Provider reachable (HTTP 429, rate limited); the key was not rejected.".to_string(),
        ),
        other => Err(format!("Unexpected provider response (HTTP {other}).")),
    }
}

pub async fn test_connection(provider_id: &str) -> Result<String, String> {
    let request = test_request(provider_id)?;
    let key = rightkit_llm::agent_providers::load_api_key(provider_id)
        .ok_or_else(|| "No key is set for this provider.".to_string())?;
    let client = rightkit_http::http_client::endpoint_client(&request.url, Duration::from_secs(10))
        .map_err(|_| "could not build the HTTP client".to_string())?;
    let response = client
        .send_any(&rightkit_http::Request::get(request.url.clone()).bearer(&key))
        .await
        .map_err(|error| {
            if matches!(error, rightkit_http::HttpError::Timeout { .. }) {
                "The provider did not answer within 10 seconds.".to_string()
            } else {
                "Could not reach the provider.".to_string()
            }
        })?;
    probe_verdict(response.status)
}

// ── Actions (TUI + CLI share these) ──────────────────────────────────────

/// Run one `/providers` action in-process. `secret` is the typed key for
/// add/replace. Returns the message line to show.
pub async fn perform_action(
    provider_id: &str,
    action: ProviderAction,
    secret: Option<&str>,
) -> Result<String, String> {
    let screen = screen_provider(provider_id)
        .ok_or_else(|| format!("{provider_id} is not available on this screen"))?;
    match (screen.kind, action) {
        (ProviderKind::ApiKey, ProviderAction::AddKey | ProviderAction::ReplaceKey) => {
            let key = secret
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .ok_or_else(|| "No key entered; nothing saved.".to_string())?;
            store_key(provider_id, key)?;
            Ok("Key saved to the OS keychain.".to_string())
        }
        (ProviderKind::ApiKey, ProviderAction::RemoveKey) => {
            remove_key(provider_id)?;
            Ok("Key removed.".to_string())
        }
        (ProviderKind::ApiKey, ProviderAction::TestConnection) => {
            test_connection(provider_id).await
        }
        (ProviderKind::OAuth, ProviderAction::SignIn) => {
            let provider = provider_id.to_string();
            // The browser flow can take minutes; run it detached so the TUI
            // stays responsive. Status updates the next time the screen opens.
            tokio::spawn(async move {
                if crate::api_provider_auth::quiet_login(&provider)
                    .await
                    .is_ok()
                {
                    let _ = super::onboarding::ConfigPersister::persist_native_subscription_route(
                        &provider,
                    );
                }
            });
            Ok("Opened your browser to sign in to ChatGPT. Finish there, then reopen this screen to see the new status.".to_string())
        }
        (ProviderKind::OAuth, ProviderAction::SignOut) => {
            crate::api_provider_auth::terminal_logout(provider_id).await?;
            Ok("Signed out.".to_string())
        }
        (ProviderKind::VendorCli, ProviderAction::Status) => {
            Ok(vendor_status_text(provider_id, &vendor_facts(provider_id)))
        }
        (ProviderKind::VendorCli, ProviderAction::SignIn) => {
            let (install, login) = vendor_hints(provider_id).unwrap_or_default();
            if vendor_facts(provider_id).installed {
                Ok(login)
            } else {
                Ok(format!("Not installed. {install}"))
            }
        }
        _ => Err("That action is not available for this provider.".to_string()),
    }
}
