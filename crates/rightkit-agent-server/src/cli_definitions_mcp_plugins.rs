#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    Powershell,
    Elvish,
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum AppCommand {
    /// Print a machine-readable app handoff payload for a session.
    Handoff {
        /// Session ID to hand off.
        session_id: String,
    },
    /// Open the desktop app on a session via the registered {app}:// link.
    Open {
        /// Session ID to open.
        session_id: String,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum McpCommand {
    /// List configured MCP servers.
    List {
        /// Workspace root for workspace-scoped MCP config.
        #[arg(long)]
        workspace_root: Option<String>,
        /// Accepted for parity; list already emits JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print RightKit's stdio MCP server status and tool counts.
    Status,
    /// Print one MCP server from the configured list.
    Get {
        /// MCP server name.
        name: String,
        /// Workspace root for workspace-scoped MCP config.
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// Add or replace an MCP server from a daemon request JSON file.
    #[command(alias = "add")]
    AddJson {
        /// JSON file matching the daemon's save-MCP-server request body.
        path: String,
    },
    /// Remove an MCP server from user or workspace config.
    Remove {
        /// MCP server name.
        name: String,
        /// Config scope to edit.
        #[arg(long, default_value = "user")]
        scope: String,
        /// Workspace root when --scope workspace is used.
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// Probe an MCP server launch/connect path.
    Probe {
        /// MCP server name.
        name: String,
        /// Config scope to read.
        #[arg(long, default_value = "user")]
        scope: String,
        /// Workspace root when --scope workspace is used.
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// Test an MCP server connection (probe + pass/fail output).
    Test {
        /// MCP server name.
        name: String,
        /// Config scope to read.
        #[arg(long, default_value = "user")]
        scope: String,
        /// Workspace root when --scope workspace is used.
        #[arg(long)]
        workspace_root: Option<String>,
        /// Emit structured JSON instead of human-readable pass/fail.
        #[arg(long)]
        json: bool,
    },
    /// Trigger OAuth/login flow for an MCP server that requires auth.
    Login {
        /// MCP server name.
        name: String,
        /// Config scope to read.
        #[arg(long, default_value = "user")]
        scope: String,
        /// Workspace root when --scope workspace is used.
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// List curated installable MCP servers from the built-in catalog.
    Catalog {
        /// Accepted for parity; catalog already emits JSON.
        #[arg(long)]
        json: bool,
    },
    /// Resolve a curated catalog entry and add it (config write, same as add-json).
    Install {
        /// Catalog entry name (see `{app} mcp catalog`).
        name: String,
        /// Config scope to write.
        #[arg(long, default_value = "user")]
        scope: String,
        /// Workspace root when --scope workspace is used.
        #[arg(long)]
        workspace_root: Option<String>,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum PluginCommand {
    /// List installed plugins.
    List {
        /// Accepted for parity; list already emits JSON.
        #[arg(long)]
        json: bool,
    },
    /// List marketplace plugins from the local marketplace catalog.
    Marketplace,
    /// Reload plugins from disk.
    Reload,
    /// Install a local plugin directory.
    Install {
        /// Local plugin source directory.
        source_path: String,
        /// Replace an existing plugin with the same id.
        #[arg(long)]
        overwrite: bool,
    },
    /// Install a plugin from the marketplace catalog.
    InstallMarketplace {
        /// Marketplace plugin id.
        id: String,
        /// Expected plugin checksum, when pinning an install.
        #[arg(long)]
        expected_sha256: Option<String>,
        /// Replace an existing plugin with the same id.
        #[arg(long)]
        overwrite: bool,
        /// Allow file:// URLs in the marketplace catalog.
        #[arg(long)]
        allow_file_urls: bool,
    },
    /// Sync the local marketplace catalog from a checksum-pinned URL.
    MarketplaceSync {
        /// Catalog URL.
        catalog_url: String,
        /// Expected catalog checksum.
        #[arg(long)]
        expected_sha256: String,
        /// Allow unsigned marketplace entries.
        #[arg(long)]
        allow_unsigned: bool,
        /// Allow file:// catalog URLs.
        #[arg(long)]
        allow_file_urls: bool,
    },
    /// Enable an installed plugin.
    Enable {
        /// Plugin id.
        id: String,
    },
    /// Disable an installed plugin.
    Disable {
        /// Plugin id.
        id: String,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum PermissionCommand {
    /// Set a live session permission mode.
    Mode {
        /// Session id.
        session_id: String,
        /// plan | ask | accept-edits | auto-safe | bypass.
        mode: String,
    },
    /// List live session permission rules.
    Rules {
        /// Session id.
        session_id: String,
    },
    /// Add a live session permission rule.
    RuleAdd {
        /// Session id.
        session_id: String,
        /// allow | ask | deny.
        effect: String,
        /// Tool or command pattern.
        pattern: String,
    },
    /// Remove a live session permission rule by index.
    RuleRemove {
        /// Session id.
        session_id: String,
        /// Rule index from `permission rules`.
        index: usize,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum ProviderCommand {
    /// Read provider key configuration status.
    Status {
        /// Provider id: openrouter.
        #[arg(value_parser = parse_provider_key_provider)]
        provider: String,
    },
    /// Start supported-subscription OAuth login.
    Login {
        #[arg(value_parser = parse_provider_key_provider)]
        provider: String,
    },
    /// Remove provider OAuth/API credentials owned by daemon.
    Logout {
        #[arg(value_parser = parse_provider_key_provider)]
        provider: String,
    },
    /// Store an API key for a gateway provider.
    Connect {
        /// Provider id: openrouter.
        #[arg(value_parser = parse_provider_key_provider)]
        provider: String,
    },
    /// Test the credential currently selected by runtime resolution.
    Test {
        /// Provider id: openrouter.
        #[arg(value_parser = parse_provider_key_provider)]
        provider: String,
    },
    /// Replace the stored API key for a gateway provider.
    ///
    /// Rotation is a replace, not an append: the previous key is overwritten
    /// and is not recoverable afterwards. The daemon is the only store, so a
    /// failed rotation leaves the previous key in place rather than clearing
    /// it.
    Rotate {
        /// Provider id: openrouter.
        #[arg(value_parser = parse_provider_key_provider)]
        provider: String,
    },
    /// Remove the stored API key for a gateway provider.
    Disconnect {
        /// Provider id: openrouter.
        #[arg(value_parser = parse_provider_key_provider)]
        provider: String,
    },
    /// List models available through the inference catalog.
    Models {
        /// Provider id: openrouter. Accepted for parity; the catalog is not
        /// currently filterable by provider key.
        #[arg(value_parser = parse_provider_key_provider)]
        provider: String,
    },
    /// Toggle a model's visibility for a gateway provider.
    #[command(name = "set-visibility")]
    SetVisibility {
        /// Provider id: openrouter.
        #[arg(value_parser = parse_provider_key_provider)]
        provider: String,
        /// Model slug.
        model: String,
        /// Set the model visible (pass --visible=false to hide).
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        visible: bool,
    },
}

fn parse_provider_key_provider(value: &str) -> Result<String, String> {
    match value {
        "commandcode" | "opencode" | "opencode-go" | "openrouter" | "minimax" | "openai"
        | "google" | "mistral" | "claude-code" | "antigravity" => Ok(value.to_string()),
        _ => Err(format!(
            "unknown provider `{value}`; expected a registered provider"
        )),
    }
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum LocalModelCommand {
    /// Print local hardware readiness for model setup.
    Hardware,
    /// Print the local model catalog recommended for this machine.
    Catalog,
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum HooksCommand {
    /// Print redacted hook configuration status from the running daemon.
    Status,
    /// List registered hooks (event, kind, source env var, id, revoked state).
    List,
    /// Dry-run matching hooks against a synthetic event. Does NOT mutate state.
    Test {
        /// Hook event to dry-run: PreToolUse | PostToolUse | UserPromptSubmit |
        /// Notification | SubagentStop | Stop.
        event: String,
        /// Optional substring filter on the env var name (e.g. "PRE").
        #[arg(long)]
        matcher: Option<String>,
    },
    /// Revoke a hook id (env var name) for this daemon session/restore it.
    Revoke {
        /// Hook id (env var name, e.g. `RIGHTKIT_HOOK_PRE_TOOL_USE`).
        id: String,
        /// Restore a previously-revoked hook instead of revoking it.
        #[arg(long)]
        restore: bool,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum ToolsetCommand {
    /// List available toolset profiles.
    List {
        /// Accepted for parity; list already emits JSON.
        #[arg(long)]
        json: bool,
    },
    /// List tool gateway readiness.
    Gateways {
        /// Accepted for parity; gateways already emits JSON.
        #[arg(long)]
        json: bool,
    },
}
