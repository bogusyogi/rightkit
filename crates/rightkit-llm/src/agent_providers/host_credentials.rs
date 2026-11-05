//! Providers R4, R33, R46: the credential variable names a host reads, and the one rule that says which
//! variables every child process loses (R46). Pure name checks only; where a key is stored and read is
//! not decided here.

use std::collections::BTreeSet;

/// File name of the key file inside the state root.
pub const DOTENV_FILE: &str = ".env";

/// The GitHub token names R33 and R4 list. Read from `.env` only.
pub const GITHUB_TOKEN_NAMES: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

/// The web-search key name (storage rule 32).
pub const WEB_SEARCH_KEY_NAME: &str = "RIGHTKIT_WEB_SEARCH_API_KEY";

/// Daemon and driver-gateway secrets that no child process receives (R46).
pub const DAEMON_SECRET_NAMES: [&str; 3] = [
    "RIGHTKIT_DAEMON_TOKEN",
    "RIGHTKIT_GATEWAY_TOKEN",
    "RIGHTKIT_DRIVER_INSTANCE_SECRET",
];

/// Names this crate never reads but every child must lose: a variable of a vendor CLI or of the
/// coding agent that would override the account the host selected (R27, R46, #344).
const SCRUB_ONLY_NAMES: [&str; 3] = [
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CODEX_API_KEY",
];

/// Names read from `.env` only, never from the process environment: the GitHub token names
/// (R33: the product uses its own GitHub connection, not a token from the terminal).
pub fn is_dotenv_only_name(name: &str) -> bool {
    GITHUB_TOKEN_NAMES.contains(&name)
}

/// The names a provider's key may come from on a host with no OS credential store, in the order
/// they are tried. The first is `<PROVIDER>_API_KEY` with `-` turned into `_`, the name
/// the host's `provider set-key` command writes to `.env`; the others are the names the peers use.
pub fn provider_env_names(provider_id: &str) -> &'static [&'static str] {
    match provider_id {
        "openai" => &["OPENAI_API_KEY"],
        "commandcode" => &["COMMANDCODE_API_KEY", "COMMAND_CODE_API_KEY"],
        "opencode" => &["OPENCODE_API_KEY"],
        "opencode-go" => &["OPENCODE_GO_API_KEY", "OPENCODE_API_KEY"],
        "anthropic" => &["ANTHROPIC_API_KEY"],
        // The first name is what `set-key` writes; `GEMINI_API_KEY` is the name the Gemini CLI reads.
        "google" => &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY"],
        "antigravity" => &["ANTIGRAVITY_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "tinyfish" => &["TINYFISH_API_KEY"],
        _ => &[],
    }
}

fn provider_ids_with_names() -> [&'static str; 11] {
    [
        "google",
        "mistral",
        "antigravity",
        "anthropic",
        "openai",
        "commandcode",
        "opencode",
        "opencode-go",
        "minimax",
        "openrouter",
        "tinyfish",
    ]
}

/// Every credential name this module reads (providers, GitHub, web search), without the route
/// names a caller adds.
pub fn credential_env_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for id in provider_ids_with_names() {
        names.extend(provider_env_names(id).iter().map(|n| n.to_string()));
    }
    names.extend(GITHUB_TOKEN_NAMES.iter().map(|n| n.to_string()));
    names.insert(WEB_SEARCH_KEY_NAME.to_string());
    names
}

/// A route credential name: `RIGHTKIT_<NAME>_API_KEY` (for example
/// `RIGHTKIT_ANTHROPIC_API_KEY`). A `provider-routes.<id>.api-key-env` may point at such a name
/// and at nothing else, so a configuration file can never name an arbitrary variable of the
/// environment and have its value sent to a route's endpoint. The names this module reads for
/// providers, GitHub and web search are not route names.
pub fn is_route_key_name(name: &str) -> bool {
    const PREFIX: &str = "RIGHTKIT_";
    const SUFFIX: &str = "_API_KEY";
    name.len() > PREFIX.len() + SUFFIX.len()
        && name.starts_with(PREFIX)
        && name.ends_with(SUFFIX)
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && !credential_env_names().contains(name)
        && !rightkit_app_settings::agent_config::licence_env::is_licence_env_name(name)
}

/// Whether a variable must be removed from every child process's environment (R46): a credential
/// name this module reads, a route credential name, a name only the scrub knows, a daemon secret
/// or a licence locator. The one launcher calls this for each variable it would pass on.
pub fn is_credential_env_name(name: &str) -> bool {
    credential_env_names().contains(name)
        || is_route_key_name(name)
        || SCRUB_ONLY_NAMES.contains(&name)
        || DAEMON_SECRET_NAMES.contains(&name)
        || rightkit_app_settings::agent_config::licence_env::is_licence_env_name(name)
}
