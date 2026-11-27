//! Once-per-process host options of the driver host.
//!
//! Each option is chosen once by the host at start-up, before its first launch, account listing
//! or registry build, and the first call wins: a setter returns `false` and changes nothing when a
//! choice was already made (or, for [`set_built_in_providers`], when the list is refused). With no
//! option set the driver host behaves exactly as 0.2.8 did.
//!
//! A host that is CodeRight sets all four: [`set_linked_profiles`] `true`,
//! [`set_windows_job_best_effort`] `true`, [`set_ignore_sdk_housekeeping_messages`] `true` and
//! [`set_built_in_providers`] `["claude"]`; it also names itself through
//! `rightkit_agent_config::ensure_host_names` and sets
//! `rightkit_agent_coding_tools::shell::set_strip_parent_claude_session_env` and
//! `set_anchor_children_to_parent` to `true`.
//!
//! Two behaviours of the driver host follow options that `rightkit-agent-coding-tools` already
//! owns, so this module has no copy of them: a vendor launch removes the variables of a parent
//! Claude Code session when the host called
//! `rightkit_agent_coding_tools::shell::set_strip_parent_claude_session_env`, and ties the child
//! to the host's lifetime when it called
//! `rightkit_agent_coding_tools::shell::set_anchor_children_to_parent`.

// A bool that is `false` until the host sets it, set at most once, the first call winning (the
// same shape as the `host_option!` of `rightkit-agent-coding-tools`).
macro_rules! host_option {
    ($(#[$doc:meta])* $once:ident, $setter:ident, $getter:ident) => {
        static $once: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

        $(#[$doc])*
        pub fn $setter(enabled: bool) -> bool {
            $once.set(enabled).is_ok()
        }

        /// Whether the option of the same name is on (see its setter).
        pub fn $getter() -> bool {
            $once.get().copied().unwrap_or(false)
        }
    };
}

host_option!(
    /// Linked Claude profiles: an existing Claude Code configuration folder that the user linked
    /// (`rightkit_agent_config::accounts::LinkedProfileStore`) is listed with the managed
    /// accounts (`SubscriptionAccount::source` is then `Linked`), its identity and its platform
    /// (Bedrock or Vertex, read from the folder's `settings.json`) are read, a launch under it
    /// sets `CLAUDE_CONFIG_DIR` to the linked folder (and removes the variable when the folder
    /// is the user's default `~/.claude`, so Claude Code resolves its own default), a persistent
    /// harness pins `HOME` only for a profile under the managed root and never for a linked
    /// folder, and the Claude adapter applies the linked account's environment on launch.
    /// `false` (the default) knows only managed profiles: no linked folder is listed or read, and
    /// a profile's `HOME` is always pinned.
    LINKED_PROFILES,
    set_linked_profiles,
    linked_profiles
);

host_option!(
    /// Windows: a failure to put a vendor CLI into the process-limit job object (the job is
    /// already nested in another one, or the limit cannot be applied) is logged and the vendor
    /// runs without that extra limit; the job still ends the process tree when it can be created.
    /// `false` (the default) fails the launch when the job cannot be created or assigned, so a
    /// vendor never runs unconfined by the job. Other platforms are unaffected.
    WINDOWS_JOB_BEST_EFFORT,
    set_windows_job_best_effort,
    windows_job_best_effort
);

host_option!(
    /// Claude Code housekeeping messages: the Agent SDK messages `tool_progress`,
    /// `tool_use_summary`, `prompt_suggestion`, `active_goal`, `auth_status`,
    /// `conversation_reset`, `system/permission_denied` and `system/mirror_error` carry nothing a
    /// turn depends on, and are skipped with one line on standard error instead of failing the
    /// stream as an unknown required message. Every other unknown message is still refused.
    /// `false` (the default) refuses them like any other unknown message.
    IGNORE_SDK_HOUSEKEEPING_MESSAGES,
    set_ignore_sdk_housekeeping_messages,
    ignore_sdk_housekeeping_messages
);

/// The provider ids of the built-in adapters this crate knows, in registration order.
pub const KNOWN_BUILT_IN_PROVIDERS: [&str; 6] =
    ["claude", "codex", "grok", "cursor", "cline", "commandcode"];

static BUILT_IN_PROVIDERS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Chooses which built-in provider adapters the host offers: only the listed ids are yielded by
/// [`crate::driver_host::cli_runtime::built_in::iter`] and
/// [`crate::driver_host::cli_runtime::built_in::spec_for`], registered by
/// [`crate::driver_host::cli_runtime::built_in::seed_registry`], listed by
/// [`crate::driver_host::cli_runtime::built_in_provider_ids`] and
/// [`crate::driver_host::cli_runtime::profile_layouts`], offered as subscription providers by the
/// account list, and given a vendor login by `login_routes`. Each id must be one of
/// [`KNOWN_BUILT_IN_PROVIDERS`] and the list must not be empty; otherwise nothing is set and
/// `false` is returned. Order and duplicates are ignored (the registration order stays the
/// built-in order). Without a call every built-in provider is offered, as in 0.2.8. Call once at
/// start-up, before `seed_registry` or the first account list. CodeRight chooses `["claude"]`.
pub fn set_built_in_providers(ids: &[&str]) -> bool {
    if ids.is_empty() || ids.iter().any(|id| !KNOWN_BUILT_IN_PROVIDERS.contains(id)) {
        return false;
    }
    let chosen: Vec<String> = KNOWN_BUILT_IN_PROVIDERS
        .iter()
        .filter(|known| ids.contains(known))
        .map(|known| (*known).to_string())
        .collect();
    BUILT_IN_PROVIDERS.set(chosen).is_ok()
}

/// The built-in provider ids the host chose with [`set_built_in_providers`], `None` when it chose
/// none (every built-in provider is offered).
pub fn chosen_built_in_providers() -> Option<&'static [String]> {
    BUILT_IN_PROVIDERS.get().map(Vec::as_slice)
}

/// Whether the built-in provider `id` is offered under the host's choice: always without a
/// choice, only a listed id with one.
pub fn built_in_provider_offered(id: &str) -> bool {
    match chosen_built_in_providers() {
        Some(chosen) => chosen.iter().any(|offered| offered == id),
        None => true,
    }
}
