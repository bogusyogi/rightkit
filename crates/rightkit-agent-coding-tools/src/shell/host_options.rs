// Once-per-process host options for the shell tool and the native launch adapter.
//
// Every option is `false` until the host sets it, and every setter returns `false` (changing
// nothing) when a choice was already made, as with [`set_anchor_children_to_parent`]. With all of
// them at their default the launch, the shell tool's schema and its results are exactly what they
// were before the options existed. A host sets them once at start-up, before the first tool call.

host_option!(
    /// Removes the credential, daemon-token and licence variables a launch would only get by
    /// inheriting this process's environment, for children of `NativeCommandSandboxAdapter`
    /// (Linux and macOS; Windows already forwards an allowlist), for the shell tool's foreground
    /// command and for the non-Linux PTY launch. A variable the launch sets itself stays. `false`
    /// (the default) leaves the inherited environment untouched.
    SCRUB_INHERITED_CREDENTIALS,
    set_scrub_inherited_credentials,
    scrub_inherited_credentials
);

host_option!(
    /// Hardens the children the launch adapters and the shell tool start: on Linux `LD_PRELOAD` and
    /// `LD_AUDIT` are removed from the child's environment and its core-dump limit is set to 0
    /// before exec; on macOS `DYLD_INSERT_LIBRARIES` is removed. `false` (the default) changes
    /// neither the child's environment nor its limits.
    HARDEN_CHILDREN,
    set_harden_children,
    harden_children
);

host_option!(
    /// Makes `NativeCommandSandboxAdapter` resolve one tier on every OS: an operator-disabled
    /// sandbox tier runs the launch unconfined (macOS used to refuse it; Linux and Windows
    /// confined it anyway), a network-requiring launch keeps filesystem confinement without the
    /// Strict network denial, and on Windows network access follows the tier. `false` (the default)
    /// keeps each platform's earlier rule.
    HONOUR_DISABLED_SANDBOX_TIER,
    set_honour_disabled_sandbox_tier,
    honour_disabled_sandbox_tier
);

host_option!(
    /// Gives each shell command a writable scratch directory inside the workspace
    /// (`<workspace>/.rightkit/tmp/<session>`, with a `.gitignore` of `*` beside it) and exports it as
    /// `TMPDIR`, `TEMP` and `TMP`, because the sandbox denies `/tmp`. Background shells share the
    /// name `background`. `false` (the default) sets none of these variables and creates no folder.
    SESSION_SCRATCH_TMPDIR,
    set_session_scratch_tmpdir,
    session_scratch_tmpdir
);

host_option!(
    /// Adds an optional `workdir` argument (a directory inside the workspace) to the `shell` and
    /// `powershell` tools' schema and runs the command there. `false` (the default) keeps the
    /// schema and runs every command in the workspace root.
    SHELL_WORKDIR,
    set_shell_workdir,
    shell_workdir
);

host_option!(
    /// Rejects a `timeout_ms` that is zero, negative, fractional or not a number with an
    /// `invalid_request` error. `false` (the default) treats such a value as the default timeout.
    SHELL_STRICT_TIMEOUT,
    set_shell_strict_timeout,
    shell_strict_timeout
);

host_option!(
    /// A foreground command killed by its timeout or a cancel returns the last 8 KiB of what each
    /// stream had printed, and a timeout message ends with a hint to raise `timeout_ms` or run in
    /// the background. `false` (the default) returns the kill message alone.
    SHELL_REPORT_OUTPUT_ON_KILL,
    set_shell_report_output_on_kill,
    shell_report_output_on_kill
);

host_option!(
    /// After a foreground shell exits, stops processes it left running that keep its output pipes
    /// open once the command's timeout or an operator cancel is reached (the pipes are otherwise
    /// waited on without a bound), and appends a note to stderr. `false` (the default) waits for
    /// the pipes to close.
    SHELL_STOP_LEFTOVER_PROCESSES,
    set_shell_stop_leftover_processes,
    shell_stop_leftover_processes
);

host_option!(
    /// Keeps the first and the last 4 MiB of a foreground stream (8 MiB less 8 KiB in all) and
    /// reports how many bytes between them were discarded, so a failing build's error at the end
    /// survives. `false` (the default) keeps the first 64 MiB.
    SHELL_HEAD_TAIL_CAPTURE,
    set_shell_head_tail_capture,
    shell_head_tail_capture
);

host_option!(
    /// With a person attached (`CredentialRequester::attended`), gives a foreground command a stdin
    /// pipe; when its output ends in a password prompt the answer is requested through the masked
    /// credential card, written to the command's stdin, redacted from everything the command
    /// prints, and the time spent waiting does not count against the command's timeout. `false`
    /// (the default) gives the command a closed stdin.
    SHELL_PASSWORD_PROMPTS,
    set_shell_password_prompts,
    shell_password_prompts
);

host_option!(
    /// Undoes a foreground shell command's changes to the files that configure future sessions
    /// (hooks, config, MCP servers, policy, rules, skills, instruction files) when the content the
    /// session read is not the user's own, and reports them; and, as the other half of that rule,
    /// asks for approval only when remote (web or MCP) content influenced the session. `false`
    /// (the default) refuses any command influenced by non-user content without approval and
    /// restores nothing.
    SHELL_PROTECT_CONFIG_FILES,
    set_shell_protect_config_files,
    shell_protect_config_files
);

host_option!(
    /// Runs the shell of a session whose toolset is `verifier` with its workspace write access
    /// withheld (read-only), and refuses it where no enforcing sandbox is available. `false` (the
    /// default) gives that session the same shell as any other.
    SHELL_VERIFIER_READ_ONLY,
    set_shell_verifier_read_only,
    shell_verifier_read_only
);

host_option!(
    /// Refuses a foreground shell command when the session configures resource limits (cpu, memory,
    /// disk) that local execution cannot enforce, instead of running it without them. `false`
    /// (the default) runs it.
    SHELL_REFUSE_UNENFORCED_RESOURCE_LIMITS,
    set_shell_refuse_unenforced_resource_limits,
    shell_refuse_unenforced_resource_limits
);

static SHELL_NOTICE_LABEL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Names the host in the bracketed notices the options above add to a result, for example
/// `[label: the command exited but ...]`. The default is `rightkit`. Call once at start-up;
/// returns `false` (and changes nothing) when a choice was already made.
pub fn set_shell_notice_label(label: &str) -> bool {
    SHELL_NOTICE_LABEL.set(label.to_string()).is_ok()
}

/// The label set by [`set_shell_notice_label`].
pub(crate) fn shell_notice_label() -> &'static str {
    SHELL_NOTICE_LABEL
        .get()
        .map(String::as_str)
        .unwrap_or("rightkit")
}
