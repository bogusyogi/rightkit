//! The names this module writes into vendor launches, environments, messages and persisted
//! strings, taken from the host's names at the moment of use.
//!
//! A host names itself once at start-up through `rightkit_agent_config`
//! ([`rightkit_agent_config::ensure_host_names`], or `set_env_prefix`, `set_state_dir_name` and
//! `set_app_name`). Every function here reads those names when it is called, never in a static
//! initialised earlier, so a name set before the first call is always the one used. A host that
//! never names itself is the default host: each function then returns exactly the string that
//! 0.2.8 of this crate hard-coded.
//!
//! The one place that tells the default host from a named one is [`is_default_host`]: the
//! environment prefix is still `RIGHTKIT`. Where 0.2.8 spelled a name in a way that is not the
//! plain host slug (for example the MCP server `rightkit-agent`), the default host keeps that
//! spelling and a named host gets its slug.

use rightkit_agent_config as config;

/// Whether the host never named itself: the environment prefix is still the default `RIGHTKIT`.
pub fn is_default_host() -> bool {
    config::env_prefix() == config::DEFAULT_ENV_PREFIX
}

/// The name of the host's MCP server as a vendor CLI sees it: `rightkit-agent` for the default
/// host, the host slug (`coderight` for the host `CODERIGHT`) for a named host.
pub fn mcp_server_name() -> String {
    if is_default_host() {
        "rightkit-agent".to_string()
    } else {
        config::host_slug()
    }
}

/// The allowed-tools scope that pins a Claude Code run to the host's MCP server:
/// `mcp__<host slug>` (`mcp__rightkit` for the default host, `mcp__coderight` for `CODERIGHT`).
pub fn mcp_tool_scope() -> String {
    format!("mcp__{}", config::host_slug())
}

/// The single policy injected into each vendor run: [`crate::driver_host::RUN_SCOPED_POLICY`]
/// for the default host, the same text naming the host's app name and MCP server for a named
/// host.
pub fn run_scoped_policy() -> String {
    if is_default_host() {
        return crate::driver_host::RUN_SCOPED_POLICY.to_string();
    }
    let app = config::app_name();
    let server = mcp_server_name();
    format!(
        "{app} is the sole authority for execution, approvals, memory writes, and delegation in this run. Use the {server} MCP server for every shell command, file mutation, memory write, and delegation. Native tools may only perform read-only inspection. Never request or perform a native side effect; ask {app} through its capability gateway."
    )
}

/// Suffixes of the three credential-bearing variables the host owns: its daemon token, the
/// scoped gateway token of one run and the driver instance secret.
const HOST_SECRET_SUFFIXES: [&str; 3] = ["DAEMON_TOKEN", "GATEWAY_TOKEN", "DRIVER_INSTANCE_SECRET"];

/// Names of the host's own secrets: the `RIGHTKIT_` spelling always, and the host's prefix
/// spelling too when the host named itself. Never shorter than the three `RIGHTKIT_` names.
pub(crate) fn host_secret_env_names() -> Vec<String> {
    let mut names: Vec<String> = HOST_SECRET_SUFFIXES
        .iter()
        .map(|suffix| format!("{}_{suffix}", config::DEFAULT_ENV_PREFIX))
        .collect();
    for suffix in HOST_SECRET_SUFFIXES {
        let name = config::env_name(suffix);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Environment variable names stripped from every vendor subprocess before launch: the provider
/// keys that pay for the host's own model calls, plus the host's own daemon token, gateway token
/// and driver instance secret under the `RIGHTKIT_` spelling and under the host's own prefix.
/// For the default host this is exactly
/// [`crate::driver_host::DENIED_VENDOR_SUBPROCESS_ENV_VARS`]; for any other host it is that list
/// plus the three prefixed names, so the list never shrinks.
pub fn denied_vendor_subprocess_env_vars() -> Vec<String> {
    let mut names: Vec<String> = crate::driver_host::DENIED_VENDOR_SUBPROCESS_ENV_VARS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    for name in host_secret_env_names() {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Every environment variable name a vendor launch must not inherit: the BYOK and
/// provider-API names of [`crate::driver_host::cli_runtime::auth::FORBIDDEN_BYOK_ENV_VARS`] plus
/// the host's own secrets under the host's prefix. For the default host this is exactly that
/// constant.
pub fn forbidden_byok_env_vars() -> Vec<String> {
    let mut names: Vec<String> = crate::driver_host::cli_runtime::auth::FORBIDDEN_BYOK_ENV_VARS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    for name in host_secret_env_names() {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The environment variable that reports every vendor CLI as not installed (isolated hosts):
/// `RIGHTKIT_DISABLE_VENDOR_CLIS` for the default host, `<PREFIX>_DISABLE_VENDOR_CLIS` for a
/// named host. A named host does not read the `RIGHTKIT_` spelling.
pub fn disable_vendor_clis_env_name() -> String {
    config::env_name("DISABLE_VENDOR_CLIS")
}

/// The variable that carries the model id of a native launch to the host's MCP gateway.
pub fn internal_model_id_env_name() -> String {
    config::env_name("INTERNAL_MODEL_ID")
}

/// The variable that carries the scoped gateway token of one run.
pub fn gateway_token_env_name() -> String {
    config::env_name("GATEWAY_TOKEN")
}

/// The label of the isolation mode in which the vendor has no native route and every capability
/// is an MCP tool of the host: `exclusive_<host slug>_gateway`. It is part of the attestation
/// digest and of stored attestation rows, so it is the same text the host persisted before this
/// module took its name from the host (`exclusive_rightkit_gateway` for the default host,
/// `exclusive_coderight_gateway` for the host `CODERIGHT`).
pub fn exclusive_gateway_label() -> String {
    format!("exclusive_{}_gateway", config::host_slug())
}

/// [`exclusive_gateway_label`] as a `&'static str`, for
/// [`HarnessIsolationMode::label`](crate::driver_host::cli_runtime::HarnessIsolationMode::label).
/// Each distinct label is leaked once (one entry for a host that names itself before its first
/// call, two at most for one that names itself after it) and reused afterwards.
pub(crate) fn exclusive_gateway_label_static() -> &'static str {
    static LABELS: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let wanted = exclusive_gateway_label();
    let mut labels = LABELS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found) = labels.iter().copied().find(|label| *label == wanted) {
        return found;
    }
    let leaked: &'static str = Box::leak(wanted.into_boxed_str());
    labels.push(leaked);
    leaked
}

/// The label of a qualification probe that names the host: `pos.<host slug>_<suffix>`.
pub fn host_probe_id(suffix: &str) -> String {
    format!("pos.{}_{suffix}", config::host_slug())
}

/// The tool-ceiling label of a qualification probe turn: `rightkit_mcp::agent_mcp` for the
/// default host, `<host slug>_mcp` for a named host.
pub fn tool_ceiling_label() -> String {
    if is_default_host() {
        "rightkit_mcp::agent_mcp".to_string()
    } else {
        format!("{}_mcp", config::host_slug())
    }
}

/// The prefix of the lines this module writes to standard error:
/// `rightkit-agent-driver-host` for the default host, `<host slug>-driver-host` for a named host.
pub fn stderr_prefix() -> String {
    if is_default_host() {
        "rightkit-agent-driver-host".to_string()
    } else {
        format!("{}-driver-host", config::host_slug())
    }
}

/// How the host is named in text shown to the user: the app name (`Agent` by default). The
/// recovery hints keep the word `Host` for the default host, as before.
pub fn display_name() -> String {
    config::app_name().to_string()
}

/// The word recovery hints use for the host: `Host` for the default host, the app name for a
/// named host.
pub fn recovery_name() -> String {
    if is_default_host() {
        "Host".to_string()
    } else {
        display_name()
    }
}
