//! Once-per-process host options for the agent engine. Every option is `false` until the host sets
//! it, and every setter returns `false` (changing nothing) when a choice was already made, the
//! first call winning. With all of them at their default the engine behaves exactly as it did
//! before the option existed. A host sets these once at start-up, before the first session starts.
//!
//! The host's own names (state folder, environment prefix) are not options: they come from
//! `rightkit_agent_config` (`set_app_name`, `set_state_dir_name`, `set_env_prefix`).

// A bool that is `false` until the host sets it, set at most once, the first call winning. Same
// pattern as `rightkit-agent-coding-tools` (`host_option!` in its `lib.rs`), kept private to each
// crate.
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
    /// A `todo_write` tool result is trusted engine-authored text: its trust envelope is
    /// `ToolTrusted` with `instructions_are_data` off, because it answers with the model's own list
    /// and fixed status text and reads nothing from the workspace or a person. Every other tool
    /// result stays untrusted data. `false` (the default) wraps every tool result, `todo_write`
    /// included, as untrusted data. On is the looser setting: a todo list the model filled from
    /// untrusted content is then replayed to it unwrapped.
    TODO_WRITE_RESULT_TRUSTED,
    set_todo_write_result_trusted,
    todo_write_result_trusted
);

host_option!(
    /// The default model summarizer keeps its input inside the compaction stage's context budget
    /// (the budget less the output ceiling, at three bytes per token): when the formatted
    /// messages exceed it, the oldest messages are dropped from the summarizer's input first, with
    /// a note of how many were omitted, so the newest evidence survives and the call fits the
    /// window. `false` (the default) sends every message, and an input over the model's window
    /// fails the attempt.
    COMPACTION_INPUT_BUDGET,
    set_compaction_input_budget,
    compaction_input_budget
);

host_option!(
    /// The verbatim tail kept after an authoritative compaction is also bounded by size: the newest
    /// messages that fit 40 000 bytes (always at least one, pinned messages free, tool results
    /// counted at most 6 000 bytes each), when that keeps fewer than the configured number of
    /// complete user turns. `false` (the default) keeps every message after the Nth most recent
    /// user message, however large. On keeps less in the model's context and summarizes more.
    COMPACTION_TAIL_BYTE_BUDGET,
    set_compaction_tail_byte_budget,
    compaction_tail_byte_budget
);

host_option!(
    /// An authoritative compaction carries earlier summaries forward verbatim (capped at 60 000
    /// bytes, oldest trimmed first, in their own section after the new summary) and hands the
    /// summarizer only the new span, instead of feeding the prior summary back to be re-summarized.
    /// A prefired summary gets the same carry when it is committed. `false` (the default) hands
    /// the prior summary to the summarizer to fold in.
    COMPACTION_CARRY_PRIOR_SUMMARY,
    set_compaction_carry_prior_summary,
    compaction_carry_prior_summary
);

host_option!(
    /// The bounded tool evidence given to the compaction summarizer is filled newest result first
    /// (the budget of 12 000 bytes runs out on the oldest results) and put back in chronological
    /// order. `false` (the default) fills it oldest first, so the budget runs out on the newest
    /// results.
    COMPACTION_NEWEST_EVIDENCE_FIRST,
    set_compaction_newest_evidence_first,
    compaction_newest_evidence_first
);

host_option!(
    /// An open-obligation tool result (one whose text carries `OPEN OBLIGATION`) larger than 6 000
    /// bytes is kept through compaction as the text from the marker on, cut to 6 000 bytes under a
    /// `[tool output bounded for compaction; open obligation kept]` header, so one large log that
    /// mentions the marker cannot hold the context above the trigger forever. `false` (the default)
    /// keeps the whole result verbatim and pinned. On retains less of an obligation result longer
    /// than the cap (the text before the marker, and anything past the cap).
    BOUND_OBLIGATION_RESULTS,
    set_bound_obligation_results,
    bound_obligation_results
);

host_option!(
    /// Before the first compaction, stale tool results are cleared from the model's request: once
    /// the history passes half the window, or once more than `protect_tokens` of newer tool output
    /// lies behind it (the host's `TOOL_PROTECT_TOKENS` and `TOOL_PRUNE_MIN_TOKENS` environment
    /// variables set the two sizes), results older than a step-quantized horizon are replaced by a
    /// short marker (size, a ten-line preview, where the full output is stored). The stored
    /// transcript is never changed. `false` (the default) projects nothing away before the first
    /// compaction.
    PRE_COMPACTION_TOOL_CLEARING,
    set_pre_compaction_tool_clearing,
    pre_compaction_tool_clearing
);

host_option!(
    /// A hook that fails without blocking is reported as a `tracing` event on the `rightkit_hook`
    /// target (filterable with `RUST_LOG`), at warn level, with the hook event name and the error.
    /// `false` (the default) prints a `[hook] <event> error (non-blocking): <error>` line on
    /// standard error. Applies to the `UserPromptSubmit` hook and to every lifecycle hook the
    /// engine runs.
    HOOK_ERRORS_AS_TRACING,
    set_hook_errors_as_tracing,
    hook_errors_as_tracing
);

host_option!(
    /// The completion gate's own refusals (the assurance contract and the sentinel verdict) are
    /// bounded per turn: after eight repairs the turn finishes with an explicit warning instead of
    /// re-entering again. `false` (the default) re-enters on every refusal until the iteration
    /// budget ends the turn. On is the looser setting: a turn can finish without the gate's
    /// sign-off.
    COMPLETION_REPAIR_CAP,
    set_completion_repair_cap,
    completion_repair_cap
);

host_option!(
    /// A new user message starts the stall guardrail and the tool-loop idempotency axis afresh, so
    /// a repeat count of one turn never carries into the next and a resumed process behaves as
    /// the live one did. `false` (the default) lets those counts live as long as the engine does,
    /// carrying over from turn to turn.
    RESET_LOOP_COUNTERS_PER_TURN,
    set_reset_loop_counters_per_turn,
    reset_loop_counters_per_turn
);

host_option!(
    /// Before each request the loop shows the loop detector what the latest tool calls returned:
    /// a repeated call whose result is new counts as progress and resets the repeat count of the
    /// meltdown detector. `false` (the default) counts every repeat of the same call, whatever it
    /// returned.
    LOOP_PROGRESS_RESETS_REPEATS,
    set_loop_progress_resets_repeats,
    loop_progress_resets_repeats
);

host_option!(
    /// Results of the tools a model asks for in order to read them whole (`read_file`, `search`,
    /// `list_files`, `spawn_subagent`, `wait_for_subagents`) stay inline in the transcript up to
    /// 32 KB. `false` (the default) keeps every tool result inline only up to 4,000 bytes and
    /// externalizes the rest to a preview with a retrievable reference, so a model re-reads a
    /// larger file in slices.
    WIDE_READ_TOOL_INLINE_LIMIT,
    set_wide_read_tool_inline_limit,
    wide_read_tool_inline_limit
);

host_option!(
    /// The result of an edit carries a `<file_diagnostics>` block: the language server's
    /// diagnostics for each code file the edit changed (within five seconds in all, nothing added
    /// when no server answers), when the session has post-edit diagnostics on. `false` (the
    /// default) leaves the edit result as the tool wrote it; the end-of-turn gate still runs.
    INLINE_FILE_DIAGNOSTICS,
    set_inline_file_diagnostics,
    inline_file_diagnostics
);

host_option!(
    /// An allow that came from an ABAC rule (a policy source starting `abac:`) is a positive
    /// authorization for the call, like a permission rule or session grant: the tool's own
    /// permission guard no longer asks again for it. The clamps that must still ask have already
    /// turned such an allow into an ask before this point. `false` (the default) lets only a
    /// permission rule or grant authorize this way; an ABAC allow then reaches the tool's own guard
    /// under the ambient permission mode. On is the looser setting.
    ABAC_ALLOW_AUTHORIZES,
    set_abac_allow_authorizes,
    abac_allow_authorizes
);

host_option!(
    /// The built-in verifier commands run after an edit in a Rust workspace are `cargo check`,
    /// `cargo fmt --check` and `cargo metadata`. `false` (the default) also runs
    /// `cargo clippy --workspace --all-targets` and `cargo test --workspace --lib`. On runs fewer
    /// checks per edit.
    TRIMMED_BUILTIN_VERIFIERS,
    set_trimmed_builtin_verifiers,
    trimmed_builtin_verifiers
);

host_option!(
    /// A built-in `cargo check` or `cargo fmt` verifier is narrowed to the packages the edit
    /// touched (`-p <package>` for the nearest manifest of each touched path), falling back to the
    /// whole workspace when a touched path belongs to no package. `false` (the default) runs the
    /// workspace-wide command after every edit.
    SCOPE_CARGO_TO_TOUCHED_PACKAGES,
    set_scope_cargo_to_touched_packages,
    scope_cargo_to_touched_packages
);

host_option!(
    /// When an edit leaves a `.json`, `.toml`, `.yaml` or `.yml` file unparseable, the edit result
    /// names the parse error unless the file already failed with the same error (line and column
    /// ignored) before the edit: that fault is not the edit's. The end-of-turn verification
    /// follows the same rule: the config check of such a file reports clean. `false` (the default)
    /// reports the parse error, in the edit result and in the verification, whether or not the
    /// file was already broken.
    CONFIG_ERROR_PREDATING_EDIT_NOT_REPORTED,
    set_config_error_predating_edit_not_reported,
    config_error_predating_edit_not_reported
);

host_option!(
    /// The `mcp_auth` tool, which waits on a person finishing a sign-in, is not bound by the
    /// turn-budget deadline other MCP and network awaits get. `false` (the default) cancels it
    /// with a timeout result after the derived deadline (60 to 900 seconds, or the configured
    /// tool timeout).
    MCP_AUTH_UNSUPERVISED,
    set_mcp_auth_unsupervised,
    mcp_auth_unsupervised
);

host_option!(
    /// The compaction trigger measures context occupancy as input only (uncached prompt plus cache
    /// read and creation; completion tokens are output, not context) and puts the reserve-derived
    /// boundary a full reserve below the send gate (`window - 2 * reserve`, ignored when it does
    /// not exist), with the prefire boundary a fixed lead below that effective trigger. The
    /// account's context figure follows the same rule. `false` (the default) counts completion
    /// tokens as context and keeps a margin of `min(window / 16, 16384)` tokens below the send
    /// gate, with the prefire boundary computed from the ratio.
    COMPACTION_FULL_RESERVE_TRIGGER,
    set_compaction_full_reserve_trigger,
    compaction_full_reserve_trigger
);

host_option!(
    /// The compaction trigger counts each inline image once at a flat 765 tokens: the transcript
    /// estimate blanks the base64 payload and adds the flat cost, and the prefire gate uses that
    /// same figure. `false` (the default) estimates the transcript with the base64 text counted at
    /// its length and projects the flat image cost on top only when the provider's measured
    /// context is below that estimate.
    COMPACTION_FLAT_IMAGE_TOKENS,
    set_compaction_flat_image_tokens,
    compaction_flat_image_tokens
);

host_option!(
    /// An `mcp_call` whose target the last tool listing showed with the server's own
    /// `readOnlyHint` set, and that is allowed without a card, runs beside other read-only calls
    /// of the same reply (when no approval reviewer is configured). The hint is the MCP server's
    /// claim, not the engine's. That concurrent path does not suppress a replayed effect, does not
    /// open the mutation-diagnostics transaction and does not record a reviewer verdict, which the
    /// serial path does for an `mcp_call`. `false` (the default) runs every `mcp_call` serially.
    MCP_READ_ONLY_HINT_CONCURRENT,
    set_mcp_read_only_hint_concurrent,
    mcp_read_only_hint_concurrent
);
