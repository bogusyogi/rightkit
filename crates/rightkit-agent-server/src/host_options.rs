//! Host options: conventions one host wants and another may not.
//!
//! Each option is set at most once per process, before the host calls [`crate::run`]. A setter
//! returns `false` when the option was already set (the first value stays). An option nobody sets
//! reads as its default, which is this crate's own behaviour, so a host that sets nothing gets the
//! server as it is documented. Names of things (state folder, environment prefix, display name)
//! are not options here: they come from `rightkit_agent_config` and [`crate::host_identity`].

use std::sync::OnceLock;

macro_rules! host_option {
    ($(#[$doc:meta])* $once:ident, $setter:ident, $getter:ident, default = $default:expr) => {
        static $once: OnceLock<bool> = OnceLock::new();

        $(#[$doc])*
        pub fn $setter(enabled: bool) -> bool {
            $once.set(enabled).is_ok()
        }

        /// Whether the option of the same name is on (see its setter).
        pub fn $getter() -> bool {
            $once.get().copied().unwrap_or($default)
        }
    };
}

host_option!(
    /// `require_token` accepts the bearer as `?access_token=` on the session event stream route
    /// only, for browser `EventSource` clients that cannot set a header. `true` (the default)
    /// accepts it; `false` refuses a token in the query string everywhere.
    QUERY_TOKEN_ON_SSE_ROUTE,
    set_query_token_on_sse_route,
    query_token_on_sse_route,
    default = true
);

host_option!(
    /// The global `--yes` flag also means `--auto-approve` on `run`, `review` and `recipe run`.
    /// `true` (the default) keeps that meaning; `false` makes `--yes` only confirm destructive
    /// verbs and never change the permission mode.
    YES_FLAG_IS_AUTO_APPROVE,
    set_yes_flag_is_auto_approve,
    yes_flag_is_auto_approve,
    default = true
);

host_option!(
    /// Custom slash commands substitute arguments with the template rules (`$@`, `${@:N:L}`,
    /// `${N:-default}`, quoting) instead of the plain positional substitution. Default `false`.
    CUSTOM_COMMAND_TEMPLATE_ARGUMENTS,
    set_custom_command_template_arguments,
    custom_command_template_arguments,
    default = false
);

host_option!(
    /// Custom slash commands are also read from `~/.claude/commands`, before the host's own
    /// folders. Default `false`.
    CUSTOM_COMMANDS_CLAUDE_USER_FOLDER,
    set_custom_commands_claude_user_folder,
    custom_commands_claude_user_folder,
    default = false
);

host_option!(
    /// The Membrane context pull is sized to what is left of the model's window (measured, with
    /// one smaller retry) and carries the workspace-memory block. `false` (the default) pulls a
    /// tenth of the window, at most 12,000 tokens.
    MEMBRANE_CONTEXT_FILLS_WINDOW,
    set_membrane_context_fills_window,
    membrane_context_fills_window,
    default = false
);

host_option!(
    /// Replies report the provider host and model that produced them. Default `false`.
    MESSAGE_ORIGIN_ON_REPLIES,
    set_message_origin_on_replies,
    message_origin_on_replies,
    default = false
);

host_option!(
    /// The split role-model mode (a different model per role) needs no licence feature.
    /// `false` (the default) requires the feature.
    ROLE_MODEL_SPLIT_OPEN_TO_ALL,
    set_role_model_split_open_to_all,
    role_model_split_open_to_all,
    default = false
);

host_option!(
    /// The conductor gets its planning memory from one Membrane context pull and notes in the run
    /// report when context was off or a memory write failed. `false` (the default) uses the
    /// session memory retrieval and adds no note.
    CONDUCTOR_CONTEXT_PULL,
    set_conductor_context_pull,
    conductor_context_pull,
    default = false
);

host_option!(
    /// A missing licence feature refuses the route that needs it. `true` (the default) enforces;
    /// `false` lets every feature check pass (the licence state is still reported).
    LICENSE_ENFORCED,
    set_license_enforced,
    license_enforced,
    default = true
);

host_option!(
    /// Stored event payloads carry a trailing schema version field (`"v":1`). Default `false`.
    STAMP_EVENT_SCHEMA_VERSION,
    set_stamp_event_schema_version,
    stamp_event_schema_version,
    default = false
);

host_option!(
    /// Skills are also discovered in the shared user folders `~/.agents/skills` and
    /// `~/.claude/skills`. Default `false`.
    SHARED_USER_SKILL_DIRS,
    set_shared_user_skill_dirs,
    shared_user_skill_dirs,
    default = false
);

host_option!(
    /// A provider key may come from the environment variable a route names in `api-key-env`.
    /// `true` (the default) allows it; `false` takes keys only from the credential store.
    PROVIDER_KEYS_FROM_ENVIRONMENT,
    set_provider_keys_from_environment,
    provider_keys_from_environment,
    default = true
);

host_option!(
    /// A blocked `doctor` verdict exits with status 1, so status 2 never means "blocked".
    /// `false` (the default) exits 2.
    DOCTOR_BLOCK_EXITS_WITH_ONE,
    set_doctor_block_exits_with_one,
    doctor_block_exits_with_one,
    default = false
);

host_option!(
    /// `mcp-server` without `--session-id` runs the stand-alone server (read-only unless
    /// `--allow-mutate`), and `/mcp/server/status` exists. `true` (the default) offers it;
    /// `false` refuses a server that is not bound to a session.
    MCP_PUBLIC_SERVER,
    set_mcp_public_server,
    mcp_public_server,
    default = true
);

host_option!(
    /// The driver `memory_recall` tool asks the memory backend directly and labels every item
    /// unverified. `false` (the default) uses the session memory retrieval with provenance.
    MEMORY_RECALL_UNVERIFIED_ITEMS,
    set_memory_recall_unverified_items,
    memory_recall_unverified_items,
    default = false
);

host_option!(
    /// The verbs that remove something for good (session delete, provider logout, MCP remove,
    /// plugin remove, permission rule removal, migrate apply) are dry runs without `--yes`.
    /// Default `false`.
    DESTRUCTIVE_CLI_VERBS_NEED_YES,
    set_destructive_cli_verbs_need_yes,
    destructive_cli_verbs_need_yes,
    default = false
);

host_option!(
    /// A client may be pointed at a daemon by the daemon URL and token environment variables.
    /// `true` (the default) reads them; `false` uses only the published discovery record.
    DAEMON_ENDPOINT_FROM_ENVIRONMENT,
    set_daemon_endpoint_from_environment,
    daemon_endpoint_from_environment,
    default = true
);

host_option!(
    /// The forge connection and cloud workspace routes exist (with their contract entries). Default `false`.
    CLOUD_WORKSPACES,
    set_cloud_workspaces,
    cloud_workspaces,
    default = false
);

host_option!(
    /// The owner can mint guest links for a session: the share routes, the guest verifier on the
    /// loopback host and the `share` command exist. Default `false`.
    GUEST_SHARES,
    set_guest_shares,
    guest_shares,
    default = false
);

host_option!(
    /// `serve --lan` starts the guest-only LAN listener and `discover` browses for daemons. Needs
    /// the guest shares option as well. Default `false`.
    LAN_LISTENER,
    set_lan_listener,
    lan_listener,
    default = false
);

host_option!(
    /// The browser consent routes and the `browser-consent` slash command exist. Default `false`.
    BROWSER_CONSENT_ROUTES,
    set_browser_consent_routes,
    browser_consent_routes,
    default = false
);

host_option!(
    /// `GET /license/status` exists. Default `false`.
    LICENSE_STATUS_ROUTE,
    set_license_status_route,
    license_status_route,
    default = false
);

host_option!(
    /// The `rpc` command (line-framed JSON over standard input and output) exists. Default `false`.
    RPC_MODE,
    set_rpc_mode,
    rpc_mode,
    default = false
);

host_option!(
    /// The `ssh` command (open the client against a daemon on another machine) exists. Default `false`.
    REMOTE_SSH,
    set_remote_ssh,
    remote_ssh,
    default = false
);

host_option!(
    /// The `contract` command, the per-call journal rows and the dry-run receipt for tools that
    /// need a yes exist. Default `false`.
    CLI_CONTRACT,
    set_cli_contract,
    cli_contract,
    default = false
);

static ACTIVE_STREAM_LIMIT: OnceLock<u32> = OnceLock::new();

/// The default cap on simultaneous cloud inference streams. Zero is refused (the default stays).
pub fn set_active_stream_limit(limit: u32) -> bool {
    limit > 0 && ACTIVE_STREAM_LIMIT.set(limit).is_ok()
}

/// The default cap on simultaneous cloud inference streams: 4 unless the host set another.
pub fn active_stream_limit() -> u32 {
    ACTIVE_STREAM_LIMIT.get().copied().unwrap_or(4)
}

/// A skill compiled into the host: its folder name and the text of its `SKILL.md`.
pub type BundledSkill = (&'static str, &'static str);

static BUNDLED_SKILLS: OnceLock<&'static [BundledSkill]> = OnceLock::new();

/// The skills the host ships inside its binary. The server writes them under the state root at
/// first use and loads them like any other skill.
pub fn set_bundled_skills(skills: &'static [BundledSkill]) -> bool {
    BUNDLED_SKILLS.set(skills).is_ok()
}

/// The host's bundled skills: none unless the host set some.
pub fn bundled_skills() -> &'static [BundledSkill] {
    BUNDLED_SKILLS.get().copied().unwrap_or(&[])
}

/// A function the server runs on every tool registry it builds, after the default tools are in.
pub type ToolRegistryControls = fn(&mut rightkit_agent_coding_tools::ToolRegistry);

static TOOL_REGISTRY_CONTROLS: OnceLock<ToolRegistryControls> = OnceLock::new();

/// Let the host replace or remove tools in every registry the server builds (session catalogues,
/// listing endpoints, driver gateways), so a tool the host must control is never offered in its
/// default form by one path and its controlled form by another.
pub fn set_tool_registry_controls(controls: ToolRegistryControls) -> bool {
    TOOL_REGISTRY_CONTROLS.set(controls).is_ok()
}

/// Apply the host's registry controls, if it set any.
pub fn apply_tool_registry_controls(registry: &mut rightkit_agent_coding_tools::ToolRegistry) {
    if let Some(controls) = TOOL_REGISTRY_CONTROLS.get() {
        controls(registry);
    }
}

/// A browser-client file: its route path (`/web` or `/web/<file>`), content type and body.
pub type WebAsset = (&'static str, &'static str, &'static str);

static WEB_CLIENT_ASSETS: OnceLock<&'static [WebAsset]> = OnceLock::new();

/// The browser-client files the host serves under `/web`. With none set there is no `/web` route
/// and the `web` command refuses.
pub fn set_web_client_assets(assets: &'static [WebAsset]) -> bool {
    WEB_CLIENT_ASSETS.set(assets).is_ok()
}

/// The host's browser-client files: none unless the host set some.
pub fn web_client_assets() -> &'static [WebAsset] {
    WEB_CLIENT_ASSETS.get().copied().unwrap_or(&[])
}
