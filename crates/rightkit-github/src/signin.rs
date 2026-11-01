//! Desktop/CLI GitHub sign-in (RFC 8628 device flow), backed by RightKit OAuth and the OS
//! credential store. The host installs its OAuth client id and credential namespace with
//! [`configure`] before calling any sign-in function.

use std::sync::OnceLock;
use std::time::Duration;

const ACCOUNT: &str = "github.com";

/// Host-specific sign-in settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInConfig {
    /// Public GitHub OAuth/App client id used for the device flow.
    pub client_id: String,
    /// OS credential-store namespace (service) holding the host's own token entry.
    pub credential_namespace: String,
    /// Environment variable that, when set to `1`, disables OS keychain access.
    pub keychain_disable_env: String,
}

impl Default for SignInConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            credential_namespace: "rightkit-github".to_string(),
            keychain_disable_env: "RIGHTKIT_DISABLE_OS_KEYCHAIN".to_string(),
        }
    }
}

static CONFIG: OnceLock<SignInConfig> = OnceLock::new();

/// Install the host's [`SignInConfig`]. Only the first call takes effect; returns `false`
/// if a configuration was already installed or read.
pub fn configure(config: SignInConfig) -> bool {
    CONFIG.set(config).is_ok()
}

fn config() -> &'static SignInConfig {
    CONFIG.get_or_init(SignInConfig::default)
}

fn keychain_disabled_message() -> String {
    format!(
        "OS keychain access disabled by {}",
        config().keychain_disable_env
    )
}

pub type DeviceAuthorization = rightkit_oauth::DeviceAuthorization;

/// Keep existing TUI/CLI call sites stable; RightKit owns polling & cancellation timing.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeviceWait;

fn keychain_enabled() -> bool {
    std::env::var_os(&config().keychain_disable_env).is_none_or(|value| value != "1")
}

fn secret_store() -> Result<rightkit_credentials::Store, String> {
    let namespace = rightkit_credentials::Namespace::new(&config().credential_namespace)
        .map_err(|error| error.to_string())?;
    let config = rightkit_credentials::Config::new(namespace)
        .guard_when(!keychain_enabled(), rightkit_credentials::GuardMode::Deny)
        .guard_when(cfg!(test), rightkit_credentials::GuardMode::Deny)
        .timeout(Duration::from_secs(5));
    Ok(rightkit_credentials::Store::open(config))
}

fn oauth_client() -> Result<rightkit_oauth::OAuthClient, String> {
    let mut config = rightkit_http::ClientConfig::default();
    config.connect_timeout = Some(Duration::from_secs(20));
    config.request_timeout = Duration::from_secs(20);
    config.response_timeout = Some(Duration::from_secs(20));
    let http = rightkit_http::Client::new(config);
    if config().client_id.trim().is_empty() {
        return Err(
            "GitHub sign-in is not configured: call signin::configure with a client id".to_string(),
        );
    }
    rightkit_oauth::OAuthClient::new(
        rightkit_oauth::OAuthConfig::github(config().client_id.as_str(), ["repo"]),
        http,
    )
    .map_err(|error| error.to_string())
}

/// Keychain service the GitHub CLI (`gh`) uses for github.com tokens.
const GH_SERVICE: &str = "gh:github.com";

/// Environment variables consulted after the OS credential store, in gh's order.
const ENV_TOKEN_VARS: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

fn gh_store() -> Result<rightkit_credentials::Store, String> {
    let namespace =
        rightkit_credentials::Namespace::new(GH_SERVICE).map_err(|error| error.to_string())?;
    let config = rightkit_credentials::Config::new(namespace)
        .guard_when(!keychain_enabled(), rightkit_credentials::GuardMode::Deny)
        .guard_when(cfg!(test), rightkit_credentials::GuardMode::Deny)
        .timeout(Duration::from_secs(5));
    Ok(rightkit_credentials::Store::open(config))
}

/// The active github.com login name from gh's `hosts.yml`. Only the user name is read;
/// tokens are never read from (or written to) any config file.
fn gh_login_names() -> Vec<String> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("GH_CONFIG_DIR") {
        dirs.push(dir.into());
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        dirs.push(std::path::PathBuf::from(dir).join("gh"));
    }
    if let Some(dir) = std::env::var_os("APPDATA") {
        dirs.push(std::path::PathBuf::from(dir).join("GitHub CLI"));
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        dirs.push(std::path::PathBuf::from(home).join(".config").join("gh"));
    }
    dirs.iter()
        .filter_map(|dir| std::fs::read_to_string(dir.join("hosts.yml")).ok())
        .find_map(|text| parse_gh_login(&text))
        .into_iter()
        .collect()
}

fn parse_gh_login(hosts_yml: &str) -> Option<String> {
    let mut in_host = false;
    for line in hosts_yml.lines() {
        let trimmed = line.trim();
        if !line.starts_with(char::is_whitespace) && !trimmed.is_empty() {
            in_host = trimmed.trim_end_matches(':') == "github.com" && trimmed.ends_with(':');
            continue;
        }
        if in_host {
            if let Some(user) = trimmed.strip_prefix("user:") {
                let user = user.trim().trim_matches('"').trim_matches('\'');
                if !user.is_empty() {
                    return Some(user.to_owned());
                }
            }
        }
    }
    None
}

/// Every place a GitHub token may come from, so resolution is testable with fakes.
pub struct TokenSources<'a> {
    /// The host's own OS-credential-store entry; `None` when the keychain is disabled.
    pub host: Option<&'a rightkit_credentials::Store>,
    /// gh-compatible store (service `gh:github.com`); `None` when the keychain is disabled.
    pub gh: Option<&'a rightkit_credentials::Store>,
    pub gh_accounts: Vec<String>,
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// Existing `gh auth token` / git credential helper login (never prompts).
    pub system: &'a dyn Fn() -> Option<String>,
}

fn nonblank(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Read order: OS credential store (the host entry, then gh's `gh:github.com` entry), then
/// `GH_TOKEN`/`GITHUB_TOKEN`, then an existing gh/git login. Tokens never come from config files.
pub fn resolve_from(sources: &TokenSources<'_>) -> Result<Option<String>, String> {
    if let Some(store) = sources.host {
        if let Some(secret) = store.get(ACCOUNT).map_err(|error| error.to_string())? {
            return secret
                .expose_str()
                .map(|token| Some(token.to_owned()))
                .map_err(|error| error.to_string());
        }
    }
    if let Some(store) = sources.gh {
        for account in &sources.gh_accounts {
            if let Ok(Some(secret)) = store.get(account) {
                if let Some(token) = nonblank(secret.expose_str().ok().map(str::to_owned)) {
                    return Ok(Some(token));
                }
            }
        }
    }
    for name in ENV_TOKEN_VARS {
        if let Some(token) = nonblank((sources.env)(name)) {
            return Ok(Some(token));
        }
    }
    Ok((sources.system)())
}

/// Resolve without prompting (see [`resolve_from`] for the order). With the OS keychain
/// disabled the stores are skipped but env tokens still apply.
pub fn resolve_access_token() -> Result<Option<String>, String> {
    let enabled = keychain_enabled();
    let host = if enabled { Some(secret_store()?) } else { None };
    let gh = if enabled { Some(gh_store()?) } else { None };
    let env = |name: &str| std::env::var(name).ok();
    let system = || {
        if !enabled {
            return None;
        }
        rightkit_oauth::system::SystemSource::github()
            .into_iter()
            .find_map(|source| source.lookup(Duration::from_secs(5)))
            .map(|secret| secret.expose().to_owned())
    };
    resolve_from(&TokenSources {
        host: host.as_ref(),
        gh: gh.as_ref(),
        gh_accounts: if enabled {
            gh_login_names()
        } else {
            Vec::new()
        },
        env: &env,
        system: &system,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fake_store(service: &str) -> rightkit_credentials::Store {
        let namespace = rightkit_credentials::Namespace::new(service).unwrap();
        rightkit_credentials::Store::open(
            rightkit_credentials::Config::new(namespace)
                .backend(Arc::new(rightkit_credentials::MemoryBackend::new())),
        )
    }

    fn resolve(
        host: &rightkit_credentials::Store,
        gh: &rightkit_credentials::Store,
        env: &[(&str, &str)],
        system: Option<&str>,
    ) -> Option<String> {
        let env_map: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let env_fn = move |name: &str| {
            env_map
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        let system = system.map(str::to_owned);
        let system_fn = move || system.clone();
        resolve_from(&TokenSources {
            host: Some(host),
            gh: Some(gh),
            gh_accounts: vec!["octocat".to_owned()],
            env: &env_fn,
            system: &system_fn,
        })
        .unwrap()
    }

    #[test]
    fn os_store_beats_env_and_system() {
        let (cr, gh) = (fake_store("host-app"), fake_store(GH_SERVICE));
        cr.set_str(ACCOUNT, "cr-token").unwrap();
        gh.set_str("octocat", "gh-token").unwrap();
        assert_eq!(
            resolve(&cr, &gh, &[("GH_TOKEN", "env")], Some("sys")).as_deref(),
            Some("cr-token")
        );
    }

    #[test]
    fn gh_service_entry_is_read_when_host_has_none() {
        let (cr, gh) = (fake_store("host-app"), fake_store(GH_SERVICE));
        gh.set_str("octocat", "gh-token").unwrap();
        assert_eq!(
            resolve(&cr, &gh, &[("GH_TOKEN", "env")], None).as_deref(),
            Some("gh-token")
        );
    }

    #[test]
    fn env_is_used_after_stores_in_gh_order() {
        let (cr, gh) = (fake_store("host-app"), fake_store(GH_SERVICE));
        assert_eq!(
            resolve(
                &cr,
                &gh,
                &[("GITHUB_TOKEN", "b"), ("GH_TOKEN", "a")],
                Some("sys")
            )
            .as_deref(),
            Some("a")
        );
        assert_eq!(
            resolve(&cr, &gh, &[("GH_TOKEN", "  "), ("GITHUB_TOKEN", "b")], None).as_deref(),
            Some("b")
        );
    }

    #[test]
    fn falls_back_to_system_login_then_none() {
        let (cr, gh) = (fake_store("host-app"), fake_store(GH_SERVICE));
        assert_eq!(resolve(&cr, &gh, &[], Some("sys")).as_deref(), Some("sys"));
        assert_eq!(resolve(&cr, &gh, &[], None), None);
    }

    #[test]
    fn parses_gh_login_without_reading_tokens() {
        let yml =
            "github.com:\n    oauth_token: SECRET\n    user: octocat\n    git_protocol: https\n";
        assert_eq!(parse_gh_login(yml).as_deref(), Some("octocat"));
        assert_eq!(parse_gh_login("example.com:\n    user: x\n"), None);
    }
}

pub fn sign_out() -> Result<(), String> {
    if !keychain_enabled() {
        return Err(keychain_disabled_message());
    }
    secret_store()?
        .delete(ACCOUNT)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Start RFC 8628 flow, present its public verification data, then store the token.
pub fn sign_in_device(
    present: impl FnOnce(&DeviceAuthorization),
    _wait: &DeviceWait,
) -> Result<(), String> {
    let flow = begin_device_sign_in()?;
    present(flow.authorization());
    flow.complete(&DeviceWait)
}

pub fn device_sign_in_with_terminal() -> Result<(), String> {
    use std::io::{self, Write};
    let mut show = |device: &DeviceAuthorization| {
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "GitHub sign-in: {}", device.verification_uri);
        let _ = writeln!(out, "Enter code: {}", device.user_code);
        let _ = writeln!(
            out,
            "Approve sign-in in browser; press Enter after approval."
        );
        let _ = out.flush();
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
    };
    sign_in_device(&mut show, &DeviceWait)
}

/// A started device flow whose public verification data is ready for the TUI.
pub struct DeviceSignIn {
    client: rightkit_oauth::OAuthClient,
    authorization: DeviceAuthorization,
}

impl DeviceSignIn {
    pub fn authorization(&self) -> &DeviceAuthorization {
        &self.authorization
    }

    pub fn complete(self, _wait: &DeviceWait) -> Result<(), String> {
        let tokens = self
            .client
            .finish_device(&self.authorization, &rightkit_oauth::DeviceWait::default())
            .map_err(|error| error.to_string())?;
        secret_store()?
            .set_str(ACCOUNT, tokens.access_token())
            .map_err(|error| error.to_string())
    }
}

pub fn begin_device_sign_in() -> Result<DeviceSignIn, String> {
    if !keychain_enabled() {
        return Err(keychain_disabled_message());
    }
    let client = oauth_client()?;
    let authorization = client.begin_device().map_err(|error| error.to_string())?;
    Ok(DeviceSignIn {
        client,
        authorization,
    })
}

pub fn open_verification_url(url: &str) -> Result<(), String> {
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
