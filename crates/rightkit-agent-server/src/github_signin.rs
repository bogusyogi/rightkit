//! Desktop/CLI GitHub sign-in, backed by shared RightKit OAuth & secrets. The host app's client id
//! and credential namespace come from [`crate::HostIdentity`]; without a client id sign-in reports
//! that it is not configured.

use std::sync::Arc;
use std::time::Duration;

use rightkit_oauth::credentials::{Resolution, Resolver, SecretTokenStore};

const ACCOUNT: &str = "github.com";

pub(crate) type DeviceAuthorization = rightkit_oauth::DeviceAuthorization;

/// Keep existing TUI/CLI call sites stable; the OAuth crate owns polling & cancellation timing.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DeviceWait;

fn secret_store() -> Result<rightkit_credentials::Store, String> {
    let namespace =
        rightkit_credentials::Namespace::new(crate::host_identity::host().credential_namespace())
            .map_err(|error| error.to_string())?;
    let config = rightkit_credentials::Config::new(namespace)
        .guard_when(
            rightkit_llm::agent_providers::keychain_disabled(),
            rightkit_credentials::GuardMode::Deny,
        )
        .timeout(Duration::from_secs(5));
    Ok(rightkit_credentials::Store::open(config))
}

fn oauth_client() -> Result<rightkit_oauth::OAuthClient, String> {
    let client_id = crate::host_identity::host()
        .github_client_id()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            "GitHub sign-in is not configured: the host app has no GitHub client id".to_string()
        })?;
    let config = rightkit_http::ClientConfig {
        connect_timeout: Some(Duration::from_secs(20)),
        request_timeout: Duration::from_secs(20),
        response_timeout: Some(Duration::from_secs(20)),
        ..rightkit_http::ClientConfig::default()
    };
    let http = rightkit_http::Client::new(config);
    rightkit_oauth::OAuthClient::new(
        rightkit_oauth::OAuthConfig::github(client_id, ["repo"]),
        http,
    )
    .map_err(|error| error.to_string())
}

/// The refreshing token resolver over the host app's own OS-credential-store entry. It uses no
/// system sources: the product uses only its own GitHub connection (providers R33).
fn resolver() -> Result<Resolver, String> {
    Ok(Resolver::new(
        oauth_client()?,
        Arc::new(SecretTokenStore::new(secret_store()?)),
        ACCOUNT,
    )
    .lock_dir(
        rightkit_agent_config::state_root()
            .ok_or("the state directory is unavailable")?
            .join("oauth-locks"),
    ))
}

/// Every place the app's own GitHub token may come from, so resolution is testable with fakes.
/// The gh CLI's store entry, the system gh/git login and `GH_TOKEN`/`GITHUB_TOKEN` from the
/// terminal are not sources: the running product uses only its own GitHub connection (providers
/// R33, Adrian 2026-10-31).
pub(crate) struct TokenSources<'a> {
    /// `<state root>/.env`, read once at start, on a host with no store (providers R4).
    pub(crate) dotenv: &'a dyn Fn() -> Result<Option<String>, String>,
}

fn nonblank(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Host with no OS credential store: the token saved in `<state root>/.env`. A read error is
/// returned, not skipped. No config file and no other program's login is read.
pub(crate) fn resolve_from(sources: &TokenSources<'_>) -> Result<Option<String>, String> {
    Ok(nonblank((sources.dotenv)()?))
}

/// Resolve without prompting. The probe at start decides whether this host has a store; the
/// test-build guard counts as no store. With a store, a raw value that does not start with `{`
/// was written by an older build (a non-expiring access token or a PAT): it is returned
/// unchanged, never refreshed and never deleted. Otherwise the entry is the encoded token set
/// and the resolver refreshes it under a cross-process lock when it is near expiry; a dead
/// grant deletes the entry and is reported as not connected (`None`).
pub(crate) fn resolve_access_token() -> Result<Option<String>, String> {
    let host = rightkit_llm::agent_providers::host_credentials::host();
    if !host.has_store() {
        let dotenv = || host.github_token();
        return resolve_from(&TokenSources { dotenv: &dotenv });
    }
    if let Some(secret) = secret_store()?
        .get(ACCOUNT)
        .map_err(|error| error.to_string())?
    {
        let raw = secret.expose_str().map_err(|error| error.to_string())?;
        if !raw.starts_with('{') {
            return Ok(Some(raw.to_owned()));
        }
    }
    match resolver()?.resolve() {
        Ok(Resolution::Token { token, .. }) => Ok(Some(token.expose().to_owned())),
        Ok(Resolution::NeedsSignIn { .. }) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn sign_out() -> Result<(), String> {
    let host = rightkit_llm::agent_providers::host_credentials::host();
    if !host.has_store() {
        for name in rightkit_llm::agent_providers::host_credentials::GITHUB_TOKEN_NAMES {
            host.remove_dotenv_value(name)?;
        }
        return Ok(());
    }
    resolver()?.sign_out().map_err(|error| error.to_string())
}

/// Start RFC 8628 flow, present its public verification data, then store the token.
pub(crate) fn sign_in_device(
    present: impl FnOnce(&DeviceAuthorization),
    _wait: &DeviceWait,
) -> Result<(), String> {
    let flow = begin_device_sign_in()?;
    present(flow.authorization());
    flow.complete(&DeviceWait)
}

pub(crate) fn device_sign_in_with_terminal() -> Result<(), String> {
    use std::io::{self, Write};
    let show = |device: &DeviceAuthorization| {
        let mut out = io::stdout().lock();
        let shown = writeln!(out, "GitHub sign-in: {}", device.verification_uri)
            .and_then(|()| writeln!(out, "Enter code: {}", device.user_code))
            .and_then(|()| {
                writeln!(
                    out,
                    "Approve sign-in in browser; press Enter after approval."
                )
            })
            .and_then(|()| out.flush());
        if let Err(error) = shown {
            tracing::warn!(%error, "could not print the GitHub sign-in prompt");
        }
        let mut line = String::new();
        if let Err(error) = io::stdin().read_line(&mut line) {
            tracing::warn!(%error, "could not read the sign-in confirmation");
        }
    };
    sign_in_device(show, &DeviceWait)
}

/// A started device flow whose public verification data is ready for the TUI.
pub(crate) struct DeviceSignIn {
    client: rightkit_oauth::OAuthClient,
    authorization: DeviceAuthorization,
}

impl DeviceSignIn {
    pub(crate) fn authorization(&self) -> &DeviceAuthorization {
        &self.authorization
    }

    pub(crate) fn complete(self, _wait: &DeviceWait) -> Result<(), String> {
        let tokens = self
            .client
            .finish_device(&self.authorization, &rightkit_oauth::DeviceWait::default())
            .map_err(|error| error.to_string())?;
        let host = rightkit_llm::agent_providers::host_credentials::host();
        if !host.has_store() {
            // A host with no OS credential store keeps the app's own GitHub token in
            // `<state root>/.env`, owner-only (providers R4). No refresh is possible there, so
            // only the access token is kept; with a store the whole token set is saved.
            return host.save_dotenv_value("GH_TOKEN", tokens.access_token());
        }
        resolver()?.save(&tokens).map_err(|error| error.to_string())
    }
}

pub(crate) fn begin_device_sign_in() -> Result<DeviceSignIn, String> {
    let client = oauth_client()?;
    let authorization = client.begin_device().map_err(|error| error.to_string())?;
    Ok(DeviceSignIn {
        client,
        authorization,
    })
}

pub(crate) fn open_verification_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://github.com/") {
        return Err(format!(
            "refusing to open unexpected verification URL {url}"
        ));
    }
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    rightkit_child_process::command_std(opener)
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not open browser: {error}"))
}
