//! Helpers used by the ACP contract suite. Kept in a sibling file
//! so the contract tests stay focused on the assertion surface.
//!
//! Anything exported here is `pub(crate)` and intended only for the
//! integration tests in `tests/acp_contract_suite.rs`.

use std::path::PathBuf;

use rightkit_agents::driver_host::cli_runtime::adapter::{CliLaunchSpec, CliMcpServerSpec};
use rightkit_agents::driver_host::cli_runtime::ids::{CliProviderId, CliTransportKind};

/// Build a launch spec whose `mcp_server` slot is whatever the
/// caller wants. Used by the one-MCP-rule tests to cover both the
/// "missing server" and "wrong server name" failure paths.
pub fn bad_launch_spec(mcp: Option<CliMcpServerSpec>) -> CliLaunchSpec {
    CliLaunchSpec {
        provider_id: CliProviderId::new("grok"),
        transport: CliTransportKind::Acp,
        executable: PathBuf::from("/usr/local/bin/grok"),
        argv: vec!["--acp".into()],
        env_keep: Vec::new(),
        env_remove: Vec::new(),
        env: Vec::new(),
        cwd: PathBuf::from("/work"),
        mcp_server: mcp,
        profile_hash: "profile".into(),
        session_id: "session-1".into(),
    }
}
