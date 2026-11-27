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
    /// (`<workspace>/<state dir>/tmp/<session>`, with a `.gitignore` of `*` beside it) and exports it as
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
    /// session read is not the user's own, and reports them. `false` (the default) restores
    /// nothing. This option does not change when approval is asked: see
    /// `set_shell_approval_for_remote_content_only`.
    SHELL_PROTECT_CONFIG_FILES,
    set_shell_protect_config_files,
    shell_protect_config_files
);

host_option!(
    /// Asks for durable-write approval of a shell command, and of input or EOF written to a managed
    /// process, only when remote (web or MCP) content influenced the session; content the user
    /// did not type but that is not remote (a file read from the workspace, say) needs none. `false`
    /// (the default) requires approval after any non-user content. This is looser than the
    /// default, so a host that sets it should also set `set_shell_protect_config_files`, whose
    /// restore guard undoes what such a command changes in the files that configure future
    /// sessions.
    SHELL_APPROVAL_FOR_REMOTE_CONTENT_ONLY,
    set_shell_approval_for_remote_content_only,
    shell_approval_for_remote_content_only
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

host_option!(
    /// Records what a shell command changes in the workspace. The workspace's files (up to 20,000,
    /// each up to 1 MiB, 256 MiB in all; `.git`, `node_modules`, `target`, `.venv`, `__pycache__`
    /// and the host's state folder are skipped, and so is what the workspace's ignore files exclude) are read
    /// before a command and compared after it. Each file it changed, created or deleted gets a
    /// restore point in the session's history (`rightkit-agent-engine-history`, the same store the
    /// edit tools use, byte for byte), a file with no restore point is named in the result, and a
    /// `[<label>: changed by this command]` notice with the unified diffs (at most 5 files and
    /// 4 KiB; `.env*` files, binary files and files over 1 MiB are named, not shown) is appended
    /// to the result. A `run_in_background` command is recorded when it exits or when the host
    /// calls `settle_background_file_tracking` (`host` feature) at the end of a turn or before a
    /// rewind. `false` (the default) reads nothing and records nothing; the result is unchanged.
    SHELL_CHANGE_DIFF,
    set_shell_change_diff,
    shell_change_diff
);

host_option!(
    /// Linux only: the Strict tier's shell also withholds the workspace-internal secret files the file
    /// tools withhold (`.env`, `*.pem`, `daemon.port`, `daemon.lock` and `<state dir>/runtime`), so
    /// `cat .env` is a sandbox denial. The environment variable `<prefix>_SHELL_WITHHOLD_SECRETS=1`
    /// (the prefix of `rightkit_agent_config::set_env_prefix`) turns it on too. `false` (the default)
    /// leaves the Strict grant as it was. Landlock cannot deny beneath an allowed folder: every
    /// folder on the path to a withheld file loses read and execute for the folder itself, so a file
    /// a command creates there cannot be read back by that command, and a bounded walk (8 folders
    /// deep, 20,000 entries, `.git`, `node_modules` and `target` skipped) finds the files, so a
    /// secret beyond those bounds stays readable to the shell. macOS and Windows are unaffected.
    SHELL_WITHHOLD_SECRETS,
    set_shell_withhold_secrets,
    shell_withhold_secrets
);

host_option!(
    /// Before a foreground or background shell command runs, reads the file each simple clause of
    /// it runs (the program, or the interpreter's script operand) and refuses the command when
    /// that file contains a catastrophic command (`rightkit_agent_policy::exec_reason`), so a script
    /// written by an earlier tool call is checked at launch and not only the command line. A clause
    /// that names a symbolic link whose target lies outside the workspace is skipped, never read.
    /// `false` (the default) applies only the command-line destructive guard.
    SHELL_EXEC_BOUNDARY_GUARD,
    set_shell_exec_boundary_guard,
    shell_exec_boundary_guard
);

host_option!(
    /// Windows only: refuses a shell command when the session carries `deny-read-globs` entries
    /// of its own (beyond the always-on defaults), because the AppContainer sandbox cannot enforce
    /// them inside a granted folder (a lowbox token ignores DENY entries, rightkit#887) and only the
    /// file tools would. A workspace whose sandbox is turned off is exempt. `false` (the default)
    /// runs the command with those globs unenforced for the shell.
    SHELL_REFUSE_UNENFORCED_PATH_DENY,
    set_shell_refuse_unenforced_path_deny,
    shell_refuse_unenforced_path_deny
);

host_option!(
    /// Makes a `run_in_background` shell follow the session that started it: it gets the network
    /// access the foreground shell would have in the session's permission mode (so a Bypass
    /// session's background command is not network-denied), and its process slot counts against the
    /// per-session limit of the resource profile as well as the host-wide one. `false` (the
    /// default) starts it with the network denied and counts it under the workspace only.
    SHELL_BACKGROUND_FOLLOWS_SESSION,
    set_shell_background_follows_session,
    shell_background_follows_session
);

host_option!(
    /// Applies the one digest-bound rerun grant the engine issues after the operator chose
    /// "Approve rerun" (`ToolContext::sandbox_retry_grant`) on Linux and macOS too, as it already is
    /// on Windows: the granted read and write paths widen that one launch (a missing path falls back
    /// to its parent folder; a filesystem root, a protected secret location, a folder that contains
    /// secrets and the home folder or an ancestor of it are refused), and a network grant leaves the
    /// Strict seccomp network denial off for it. `false` (the default) ignores the grant there.
    SANDBOX_RERUN_GRANT_UNIX,
    set_sandbox_rerun_grant_unix,
    sandbox_rerun_grant_unix
);

host_option!(
    /// Linux and macOS: a sandboxed launch of a vendor program (a sandbox profile that starts with
    /// `vendor-` or `driver`) through `NativeCommandSandboxAdapter` may also read the host's own
    /// executable, which that program starts as its one MCP server. `false` (the default) grants no
    /// such read.
    NATIVE_VENDOR_OWN_EXECUTABLE_READ,
    set_native_vendor_own_executable_read,
    native_vendor_own_executable_read
);

host_option!(
    /// For a vendor program (a launch profile that starts with `vendor-` or `driver`), removes the
    /// variables a parent Claude Code session exports (`CLAUDE_HOST_SESSION_ENV`) from the child's
    /// environment, unless the launch sets a real value itself, so the program describes itself and
    /// cannot attach to the parent's session, messaging socket or token. `false` (the default)
    /// leaves them as inherited.
    STRIP_PARENT_CLAUDE_SESSION_ENV,
    set_strip_parent_claude_session_env,
    strip_parent_claude_session_env
);

host_option!(
    /// Linux: confines the shell, the native launch adapter and the streamed PTY through one
    /// `rightkit_sandbox::SandboxPolicy` built by `SandboxConfig::rightkit_policy`: Landlock grants
    /// only the listed paths (no implicit `/proc`, `/etc`, `/sys` or `/dev` root), the Landlock signal
    /// scope is the child's own domain, and the network is denied by seccomp. `false` (the default)
    /// keeps the crate's own Landlock ruleset (`SandboxConfig::seccomp_policy` for the seccomp half),
    /// which has no signal scope. Other platforms are unaffected.
    SANDBOX_POLICY_CONFINEMENT,
    set_sandbox_policy_confinement,
    sandbox_policy_confinement
);

host_option!(
    /// Starts a managed PTY (`execution_channel: "pty"`) through `rightkit_sandbox`'s streamed PTY
    /// (`Sandbox::spawn_pty_stream`) on Linux, macOS and Windows, with the confinement policy of
    /// [`set_sandbox_policy_confinement`] on Linux and the environment of the launch (inherited
    /// variables minus the scrubbed ones) as an explicit allowlist. `false` (the default) keeps
    /// the crate's own PTY transports: Linux and Windows backends, `portable-pty` on macOS.
    MANAGED_PTY_STREAMED,
    set_managed_pty_streamed,
    managed_pty_streamed
);

host_option!(
    /// macOS: the Seatbelt profile for a vendor driver (`SandboxConfig::driver`) allows no write or
    /// ioctl on terminal devices and reads metadata only on the granted roots and their ancestors,
    /// and lists the certificate store (`/private/etc/ssl`) for reading in place of the blanket
    /// `/etc` read (rightkit#711). `false` (the default) keeps the wider profile.
    SANDBOX_STRICT_DRIVER_PROFILE,
    set_sandbox_strict_driver_profile,
    sandbox_strict_driver_profile
);

host_option!(
    /// Counts an acknowledged input sequence of a managed process as `sequence + 1` (the number of
    /// inputs acknowledged), so `last_acked_input_sequence` equals `next_input_sequence` once
    /// every input was delivered and a delivered write leaves no unacknowledged range; a re-attach
    /// is then no longer refused after the first input. `false` (the default) records the sequence
    /// number itself, which leaves the most recent delivered input looking unacknowledged.
    PROCESS_LEASE_ACK_COUNTS,
    set_process_lease_ack_counts,
    process_lease_ack_counts
);

host_option!(
    /// The background shell poll tool (`get_shell_output`) adds `output_base64` only when the
    /// output is not valid UTF-8 (the `output` text already carries valid UTF-8 exactly). `false`
    /// (the default) always adds it.
    BACKGROUND_OUTPUT_BASE64_WHEN_LOSSY,
    set_background_output_base64_when_lossy,
    background_output_base64_when_lossy
);

/// Names a host adds to the environment scrub of every launch, and the subset of them a launch
/// keeps when it sets the value itself.
struct ExtraScrubbedEnv {
    scrubbed: Vec<String>,
    kept_when_launch_sets: Vec<String>,
}

static EXTRA_SCRUBBED_ENV: std::sync::OnceLock<ExtraScrubbedEnv> = std::sync::OnceLock::new();

/// Adds fixed environment variable names to the scrub every child launch applies (the credential,
/// daemon-token and licence names that the launchers already remove). `scrubbed` are removed from
/// the inherited environment and from the launch's own layered environment; of those,
/// `kept_when_launch_sets` stay when the launch sets them itself (a driver gateway secret layered
/// on purpose). The names are matched exactly. A host uses this to keep its own daemon secrets out
/// of children by a spelling that does not depend on when `rightkit_agent_config::set_env_prefix`
/// ran. Call once at start-up; returns `false` (and changes nothing) when a choice was already made.
pub fn set_extra_scrubbed_env_names(scrubbed: &[&str], kept_when_launch_sets: &[&str]) -> bool {
    EXTRA_SCRUBBED_ENV
        .set(ExtraScrubbedEnv {
            scrubbed: scrubbed.iter().map(|name| name.to_string()).collect(),
            kept_when_launch_sets: kept_when_launch_sets
                .iter()
                .map(|name| name.to_string())
                .collect(),
        })
        .is_ok()
}

/// Whether `name` is one of the names added by [`set_extra_scrubbed_env_names`].
pub(crate) fn extra_scrubbed_env_name(name: &str) -> bool {
    EXTRA_SCRUBBED_ENV
        .get()
        .is_some_and(|extra| extra.scrubbed.iter().any(|scrubbed| scrubbed == name))
}

/// Whether `name` is one of the added names a launch keeps when it sets the value itself.
pub(crate) fn extra_kept_env_name(name: &str) -> bool {
    EXTRA_SCRUBBED_ENV
        .get()
        .is_some_and(|extra| extra.kept_when_launch_sets.iter().any(|kept| kept == name))
}

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
