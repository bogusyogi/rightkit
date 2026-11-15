//! Names and rules the host supplies to the agent MCP layer, each fixed once at start-up.
//!
//! The workspace state folder (`<workspace>/<state-dir>/mcp.json`) and the environment-variable
//! prefix (`<PREFIX>_MCP_CONFIG`, `<PREFIX>_DISABLE_OS_KEYCHAIN`, `<PREFIX>_DISABLE_DEFAULT_MCP`)
//! come from `rightkit_agent_config::set_state_dir_name` and `rightkit_agent_config::set_env_prefix`;
//! the folder under `~/.config` comes from [`crate::agent_mcp::set_config_dir_segment`]. This module
//! holds what is left: the client identity sent to servers, the OS keychain service name and the
//! two rules that decide which `${VAR}` names a config may read.

use std::sync::OnceLock;

/// Client name sent to MCP servers (`clientInfo.name`) and used as the default OAuth `client_id`,
/// until the host calls [`set_client_name`].
pub const DEFAULT_CLIENT_NAME: &str = "rightkit-agent";

/// OS keychain service (credential namespace) that holds stored MCP OAuth tokens, until the host
/// calls [`set_keyring_service`].
pub const DEFAULT_KEYRING_SERVICE: &str = "rightkit-agent";

static CLIENT_NAME: OnceLock<String> = OnceLock::new();
static CLIENT_VERSION: OnceLock<String> = OnceLock::new();
static KEYRING_SERVICE: OnceLock<String> = OnceLock::new();
static NEVER_EXPAND: OnceLock<fn(&str) -> bool> = OnceLock::new();
static CREDENTIAL_NAME: OnceLock<fn(&str) -> bool> = OnceLock::new();

/// Sets the client name the layer reports to MCP servers and uses as the default OAuth
/// `client_id` of a server config that names none (for example `myapp`). Call once at start-up,
/// before any config is parsed; returns `false` when one was already set.
pub fn set_client_name(name: &str) -> bool {
    CLIENT_NAME.set(name.to_string()).is_ok()
}

/// The client name in force.
pub fn client_name() -> &'static str {
    CLIENT_NAME
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_CLIENT_NAME)
}

/// Sets the client version the layer reports to MCP servers (`clientInfo.version` of the
/// `initialize` request). Call once at start-up; returns `false` when one was already set. Without
/// a call the version of this crate is reported.
pub fn set_client_version(version: &str) -> bool {
    CLIENT_VERSION.set(version.to_string()).is_ok()
}

/// The client version in force.
pub fn client_version() -> &'static str {
    CLIENT_VERSION
        .get()
        .map(String::as_str)
        .unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// Sets the OS keychain service that holds stored MCP OAuth tokens (for example `myapp`). Tokens
/// are stored under it, so a host that changes it later loses the tokens stored under the old
/// name. Call once at start-up; returns `false` when one was already set.
pub fn set_keyring_service(service: &str) -> bool {
    KEYRING_SERVICE.set(service.to_string()).is_ok()
}

/// The OS keychain service in force.
pub fn keyring_service() -> &'static str {
    KEYRING_SERVICE
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_KEYRING_SERVICE)
}

/// Sets the rule for environment names that no `${VAR}` placeholder or credential reference of an
/// MCP config may read in any scope: licence and device material and the host daemon's own
/// secrets. Without a call the rule is `rightkit_agent_config::licence_env::is_licence_env_name`
/// plus the daemon secret names of `rightkit_llm::agent_providers::host_credentials`. Call once at
/// start-up; returns `false` when one was already set.
pub fn set_never_expanded_env_names(rule: fn(&str) -> bool) -> bool {
    NEVER_EXPAND.set(rule).is_ok()
}

/// Sets the rule for credential environment names (vendor API keys, route keys, the host's own
/// secrets) that a repository-supplied MCP config may not read; the operator's own config may.
/// Without a call the rule is `rightkit_llm::agent_providers::host_credentials::is_credential_env_name`.
/// Call once at start-up; returns `false` when one was already set.
pub fn set_credential_env_names(rule: fn(&str) -> bool) -> bool {
    CREDENTIAL_NAME.set(rule).is_ok()
}

/// Whether `name` may never be read by an MCP config, in any scope.
pub(crate) fn is_never_expanded_env_name(name: &str) -> bool {
    match NEVER_EXPAND.get() {
        Some(rule) => rule(name),
        None => {
            rightkit_agent_config::licence_env::is_licence_env_name(name)
                || rightkit_llm::agent_providers::host_credentials::DAEMON_SECRET_NAMES
                    .contains(&name)
        }
    }
}

/// Whether `name` is a credential a repository-supplied MCP config may not read.
pub(crate) fn is_credential_env_name(name: &str) -> bool {
    match CREDENTIAL_NAME.get() {
        Some(rule) => rule(name),
        None => rightkit_llm::agent_providers::host_credentials::is_credential_env_name(name),
    }
}
