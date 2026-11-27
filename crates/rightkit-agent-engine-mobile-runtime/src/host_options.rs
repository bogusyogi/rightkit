//! Once-per-process host options for the mobile runtime. Every option is `false` until the host
//! sets it, and every setter returns `false` (changing nothing) when a choice was already made, the
//! first call winning. With all of them at their default the runtime behaves exactly as it did
//! before the option existed. A host sets these once at start-up, before the first runtime, git
//! request or capability query.
//!
//! The host's own names (state folder, application name, environment prefix) are not options:
//! they come from `rightkit_agent_config` (`set_app_name`, `set_state_dir_name`,
//! `set_env_prefix`). Options of the engine and of the tools crate are set on those crates.

// A bool that is `false` until the host sets it, set at most once, the first call winning. Same
// pattern as `rightkit-agent-engine-core` (`host_option!` in its `host_options.rs`), kept private
// to each crate.
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
    /// The session wires `ask_user` and `exit_plan_mode` to the host: each publishes a
    /// `userQuestionAsked` or `planPresented` event on the session conduit and waits, with no
    /// timeout, for the host to answer through `MobileRuntime::resolve_question` /
    /// `resolve_plan` (the `resolve_question` and `resolve_plan` FFI functions). `false` (the
    /// default) leaves both tools out of the model's schema, because a host that never answers
    /// would leave the turn waiting. On, an approved plan also leaves Plan mode (and only from
    /// Plan mode).
    INTERACTIVE_TOOLS,
    set_interactive_tools,
    interactive_tools
);

host_option!(
    /// The session wires `spawn_subagent` to an in-process, model-only child session (one at a time,
    /// at most eight iterations, the parent's permission mode, workspace trust and cost ceiling, no
    /// approval cards, no GitHub or Cloudflare credential, no MCP server). `false` (the default)
    /// leaves the tool out of the model's schema.
    SUBAGENTS,
    set_subagents,
    subagents
);

host_option!(
    /// A git request may carry `loopbackBase` (`http://127.0.0.1:<port>/`, nothing else), which
    /// sends the bytes of a GitHub URL to that loopback address and lets the stored token reach it.
    /// It exists for a simulator fixture only. `false` (the default) refuses every request that
    /// names a loopback base, so a credential can only ever go to `https://github.com/`. On is the
    /// looser setting: leave it off in a build that runs on a device.
    GIT_LOOPBACK_FIXTURE,
    set_git_loopback_fixture,
    git_loopback_fixture
);

host_option!(
    /// The small-model assist client (recap, follow-up, classify, progress audit) asks the provider
    /// for a JSON Schema answer (`response_format`) where the route carries one, and reads that
    /// answer before falling back to the plain-text parser. `false` (the default) sends no schema
    /// and parses text only.
    SMALL_MODEL_STRUCTURED_OUTPUT,
    set_small_model_structured_output,
    small_model_structured_output
);

host_option!(
    /// The capability list names `browserControl` (interactive browser control) as a capability that
    /// is unavailable on this device, with the reason, and a dispatch of it answers with that
    /// reason. `false` (the default) omits the entry and answers an unknown capability.
    BROWSER_CONTROL_CAPABILITY,
    set_browser_control_capability,
    browser_control_capability
);
