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
    /// A provider key may come from the environment variable a route names in `api-key-env`, and
    /// the setup hints, the readiness row and the doctor tell the user to set one. `true` (the
    /// default) allows it and says so; `false` makes those messages point to the credential store
    /// (Settings > Providers) only. `false` does not stop the `<PREFIX>_<NAME>_API_KEY` lookups
    /// of the provider crates: those follow the provider crates' own host options.
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
    /// `--allow-mutate`), and `/mcp/server/status`, its contract entry and the `mcp status`
    /// command exist. `true` (the default) offers them; `false` refuses a server that is not
    /// bound to a session, removes the status route, entry and command, and hides `mcp-server`
    /// from the help as an internal driver helper.
    MCP_PUBLIC_SERVER,
    set_mcp_public_server,
    mcp_public_server,
    default = true
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

host_option!(
    /// The state stores (teams, messages, instincts, permission rules, permission denials,
    /// workspace trust, Telegram chat bindings, peer-team boards) and the run-changes folder may
    /// be moved with the environment variables `<PREFIX>_TEAM_STORE`, `<PREFIX>_MESSAGE_STORE`,
    /// `<PREFIX>_INSTINCTS_STORE`, `<PREFIX>_PERMISSION_RULE_STORE`,
    /// `<PREFIX>_PERMISSION_DENIAL_STORE`, `<PREFIX>_WORKSPACE_TRUST_STORE`,
    /// `<PREFIX>_TELEGRAM_BINDINGS`, `<PREFIX>_PEER_TEAM_STORE_ROOT` and
    /// `<PREFIX>_RUN_CHANGES_DIR`. With the option on, every
    /// reader of the workspace trust store in this crate reads the file the variable names, so
    /// the file that grants trust is the file that is checked. `true` (the default) reads the
    /// variables; `false` ignores them and always uses the files under the state root. Without
    /// the cargo feature `state-store-env` the variables are not compiled in at all: the setter
    /// still returns `true` or `false`, but the value has no effect (the files under the state
    /// root are always used).
    STATE_STORE_PATHS_FROM_ENVIRONMENT,
    set_state_store_paths_from_environment,
    state_store_paths_from_environment,
    default = true
);

host_option!(
    /// The plugin marketplace exists: the marketplace, rollback and versions routes (with their
    /// contract entries), the matching `plugin` subcommands and the signature and provenance
    /// fields of the plugin listing. `true` (the default) offers them; `false` removes all of
    /// them. Either way a rollback or an install from the marketplace needs the caller's
    /// explicit trust acceptance.
    PLUGIN_MARKETPLACE,
    set_plugin_marketplace,
    plugin_marketplace,
    default = true
);

host_option!(
    /// The six Adapt routes (`/runtime/context/adapt/status`, `review-taste`, `adjudicate-taste`,
    /// `apply-insights`, `apply-taste`, `recall`) exist, with their OpenAPI and contract entries.
    /// `true` (the default) mounts them, with every client-supplied path confined to the
    /// session's workspace; `false` mounts only the `insights` route.
    ADAPT_ROUTES,
    set_adapt_routes,
    adapt_routes,
    default = true
);

host_option!(
    /// The `acp-server` command (alias `acp`) exists: it bridges the Agent Client Protocol on
    /// standard input and output to a running daemon. `true` (the default) offers it; `false`
    /// lists it as a deleted command.
    ACP_SERVER,
    set_acp_server,
    acp_server,
    default = true
);

host_option!(
    /// `run --auto-approve` and `review --auto-approve` exist as flags. `true` (the default)
    /// accepts them; `false` removes them from the command grammar, so they are refused as
    /// unknown flags.
    LEGACY_AUTO_APPROVE_FLAGS,
    set_legacy_auto_approve_flags,
    legacy_auto_approve_flags,
    default = true
);

host_option!(
    /// The licence clock is also kept in a file in the licence folder the licence owner names
    /// (`ENV_LICENSE_DIR` of `rightkit_agent_config::licence_env`). `true` (the default) keeps it: a licence owner that names no
    /// folder is refused (closed), and the "clock was initialised" mark the owner delivers is
    /// passed unchanged to the file clock, so a missing or older clock file is refused. `false`
    /// ignores the folder variable and runs the delivered mark clock only.
    LICENSE_FILE_CLOCK,
    set_license_file_clock,
    license_file_clock,
    default = true
);

host_option!(
    /// `require_token` also accepts the bearer in the legacy `x-{app}-token` header. `false` (the
    /// default) accepts `Authorization: Bearer` only.
    LEGACY_TOKEN_HEADER,
    set_legacy_token_header,
    legacy_token_header,
    default = false
);

host_option!(
    /// A turn that nobody is attached to (a webhook trigger, a scheduled run) runs in the
    /// `auto-safe` permission mode. `false` (the default) runs it in `ask`, where a call that
    /// needs approval is denied at once as unattended.
    UNATTENDED_TURNS_AUTO_SAFE,
    set_unattended_turns_auto_safe,
    unattended_turns_auto_safe,
    default = false
);

host_option!(
    /// A notification that arrives in quiet hours is dropped, and the response says so without a
    /// release time. `false` (the default) holds it and answers with the time it is released.
    NOTIFICATIONS_DROPPED_IN_QUIET_HOURS,
    set_notifications_dropped_in_quiet_hours,
    notifications_dropped_in_quiet_hours,
    default = false
);

host_option!(
    /// The server prepares the ONNX Runtime for the local embedder: it finds the runtime library
    /// and the embedder model folder and sets the variables that name them in the process (the
    /// names are fixed by the ONNX crate), and a build with the `fastembed` feature refuses to
    /// start `serve` without a usable runtime. `true` (the default) does all that; `false`
    /// leaves the embedder alone.
    EMBEDDER_RUNTIME_SETUP,
    set_embedder_runtime_setup,
    embedder_runtime_setup,
    default = true
);

host_option!(
    /// The six `/peer-teams` routes exist (a shared task board, a mailbox and a mission log for
    /// peers that work on one team, kept in a store of their own under the state folder). `true`
    /// (the default) mounts them; `false` does not mount them, and no route, contract entry or
    /// command mentions them. The store folder follows the stores option
    /// (`state_store_paths_from_environment`).
    PEER_TEAMS,
    set_peer_teams,
    peer_teams,
    default = true
);

host_option!(
    /// `POST /runtime/membrane/background-review` exists: it runs one restricted review request
    /// from the context service against the configured model provider, which costs a provider
    /// call. `true` (the default) mounts it; `false` does not mount it and the contract does
    /// not list it.
    MEMBRANE_BACKGROUND_REVIEW,
    set_membrane_background_review,
    membrane_background_review,
    default = true
);
