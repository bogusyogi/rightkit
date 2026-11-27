//! Names this crate writes into strings a host cannot set one by one (HTTP `User-Agent` values,
//! thread names, the workspace checkpoint's commit identity and file names, labels). Every name is
//! computed from the host's names (`rightkit_agent_config::{app_name, host_slug}`) at the moment it
//! is used and is never kept in a static, so it follows whatever the host named itself before the
//! first tool ran. For the default host every result is the exact string the crate always wrote.

/// `<AppName>/<version>`, the `User-Agent` of the crate's own HTTP clients: `Agent/0.1` for the
/// default host (the default app name is `Agent`), `CodeRight/0.1` for a host named `CodeRight`.
pub(crate) fn user_agent(version: &str) -> String {
    format!("{}/{version}", rightkit_agent_config::app_name())
}

/// The lower-case name the crate puts in identifiers and file names: `rightkit-agent` for the
/// default host (the one whose environment prefix is the default `RIGHTKIT`), the host's slug
/// (`rightkit_agent_config::host_slug`, `coderight` for a host named `CodeRight`) once it has
/// named itself. The same rule `rightkit-agent-config` uses for the prefix of its start-up lines.
pub(crate) fn agent_slug() -> String {
    let slug = rightkit_agent_config::host_slug();
    if slug == "rightkit" {
        "rightkit-agent".to_string()
    } else {
        slug
    }
}
