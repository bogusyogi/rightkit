# Changelog

## 0.1.8 (2026-11-16)

- Breaking for a host outside this repository that relied on the implied OS keyring: the crate no longer names `rightkit-mcp`'s `agent_mcp-os-keyring` or `rightkit-llm`'s `agent_providers-os-keyring` unconditionally, so a build with default features no longer links the OS credential store (`rightkit-credentials`). New feature `os-keyring` adds both; `host` implies it, so a `host` user is unchanged. A host without `host` that needs the keyring names `os-keyring`. Nothing in the crate's own code needs the keyring, so no item changes.
- New host option `shell::set_shell_withhold_secrets` / `shell::shell_withhold_secrets`, once per process, `false` until the host sets it (the setter returns `false` and changes nothing when a choice was already made); the environment variable `<prefix>_SHELL_WITHHOLD_SECRETS=1`, with the prefix of `rightkit_agent_config::set_env_prefix`, turns it on too. Linux, Strict tier, `host` feature: the native command adapter's Landlock ruleset withholds the workspace's `.env`, `*.pem`, `daemon.port`, `daemon.lock` and `<state dir>/runtime` (found by a bounded walk: 8 folders deep, 20,000 entries, `.git`, `node_modules` and `target` skipped), so `cat .env` is a sandbox denial. Landlock cannot deny beneath an allowed folder, so each folder on the path to a withheld file keeps its write rights but not read and execute, and a file a command creates there cannot be read back by that command until the next launch. `SandboxConfig` gains `withheld_read_paths` and `withhold_read`. Off, the grant is exactly 0.1.7's.

## 0.1.7 (2026-11-16)

- New host option `shell::set_shell_change_diff` / `shell::shell_change_diff`, once per process, `false` until the host sets it (the setter returns `false` and changes nothing when a choice was already made). Off, the shell tool's output, schema and file effects are exactly 0.1.6's. On, the workspace is read before each shell command and compared after it (up to 20,000 files of at most 1 MiB, 256 MiB in all; `.git`, `node_modules`, `target`, `.venv`, `__pycache__` and `.rightkit` are skipped, as are files the workspace's ignore files exclude): every file the command changed, created or deleted gets a restore point in the session's history (`rightkit-agent-engine-history` 0.1.3 byte snapshots, so a rewind restores it exactly), a file that could not get one is named in a `[<label>: no restore point could be stored for ...]` notice, and a `[<label>: changed by this command]` notice with the unified diffs (at most 5 files and 4 KiB; `.env*`, binary and over-1 MiB files are named, not shown) is appended. `<label>` is `set_shell_notice_label`. A `run_in_background` command is recorded when it exits or when the host calls the new `shell::settle_background_file_tracking(session_id)` (feature `host`) at the end of a turn or before a rewind.
- Bounded reads: the files the change record compares and records after a command are read at most 1 MiB; a larger file is named as too large to diff and its post-command state is recorded by streaming.
- `undo_edit` restores a snapshot's bytes exactly (a snapshot that is not valid UTF-8 restores instead of failing to read), and refuses to restore an older snapshot over a file the newest edit created. For valid UTF-8 snapshots with no created-file marker above them the result is what 0.1.6 produced.
- Requires `rightkit-agent-engine-history` 0.1.3.
- The package no longer ships `src/browser_control.rs` and `src/mcp_auth.rs`, two tool files that `src/lib.rs` never declared and the registry never registered. No public item, feature or behaviour changes; the compiled library is unchanged.

## 0.1.6 (2026-11-16)

Takes the shell and sandbox-adapter behaviour of a host's newer in-tree tools crate. Every item is behind a once-per-process host option that is `false` until the host sets it (setters return `false` and change nothing when a choice was already made, as with `set_anchor_children_to_parent`), so with no option set every launch, schema and result is what 0.1.5 produced. Set them once at start-up, before the first tool call. A host that wants the newer behaviour turns them on.

Launch adapters and the shell tool (`rightkit_agent_coding_tools::shell`):

- `set_scrub_inherited_credentials`: children of `NativeCommandSandboxAdapter` (Linux and macOS), the shell tool's foreground command and the non-Linux PTY launch no longer inherit a credential, daemon-token or licence variable from this process's environment; a variable the launch sets itself stays.
- `set_harden_children`: Linux children lose `LD_PRELOAD` and `LD_AUDIT` and get a core-dump limit of 0 before exec (macOS drops `DYLD_INSERT_LIBRARIES`), registered before the sandbox hooks. Also applied to launches through `process_launch`.
- `set_honour_disabled_sandbox_tier`: one tier rule for `NativeCommandSandboxAdapter` on every OS (an operator-disabled tier runs unconfined; a network-requiring launch keeps filesystem confinement without the Strict network denial; Windows network access follows the tier).
- `set_session_scratch_tmpdir`: shell commands (foreground, PTY and background) get `TMPDIR`, `TEMP` and `TMP` pointing at `<workspace>/.rightkit/tmp/<session>` (`background` for background shells), with a `.gitignore` of `*` beside it.
- `set_shell_workdir` (a `workdir` argument in the `shell` schema), `set_shell_strict_timeout` (a zero, negative, fractional or non-numeric `timeout_ms` is an `invalid_request` error), `set_shell_report_output_on_kill` (the last 8 KiB of each stream and a hint on a timeout or cancel), `set_shell_stop_leftover_processes` (a bounded wait for pipes held open by processes the command left running), `set_shell_head_tail_capture` (first and last 4 MiB of a stream instead of the first 64 MiB), `set_shell_password_prompts` (with a person attached, a password prompt is answered through the masked credential card and redacted from the output; the command's timeout does not run meanwhile), `set_shell_protect_config_files` (changes a command makes to hook, config, MCP, policy, rule, skill and instruction files are undone when the session's content is not the user's, and remote content alone requires durable-write approval), `set_shell_verifier_read_only` (a `verifier` toolset gets a read-only shell), `set_shell_refuse_unenforced_resource_limits`.
- `set_shell_notice_label`: names the host in the bracketed notices the options above add (default `rightkit`).
- Unconditional: `NativeCommandSandboxAdapter` on Windows refuses a launch that requires a sandbox but names no profile (`SandboxMissing`), as Linux and macOS already did.

Individual tools (`rightkit_agent_coding_tools::tool_options`): `set_send_message_reserved_channels`, `set_write_file_replace_non_utf8`, `set_context7_untrusted_docs`, `set_tool_search_full_index`, `set_finalize_requires_passing_verdict`, `set_output_keeps_tail` (reduced output of a successful command keeps head and tail, and progress-bar `\r` redraws are collapsed).

Traits: `CredentialRequester::attended` (default `false`) and `PlanExit::take_context_reset` / `PlanExit::context_reset_pending` (defaults `None` / `false`) are new provided methods, so existing implementations compile and behave as before. `ToolContext::unenforced_resource_limits_error` is new.

## 0.1.5 (2026-11-16)

- New `shell::set_anchor_children_to_parent` / `shell::anchor_children_to_parent` (feature `host`), fixed once at start-up, off by default. Off, every launch is exactly as before. On, on Linux, children of `NativeCommandSandboxAdapter` (pipe launches) and of the shell tool die with the host process: the kernel SIGKILLs each one (`PR_SET_PDEATHSIG`, registered before the sandbox hooks) and every such spawn is made from one process-lifetime thread, because the signal is tied to the thread that forked. Grandchildren are not covered (the session-wide tree kill reaches them). If that thread cannot start, the launch fails instead of running unanchored. macOS and Windows launches are unchanged. A host that restarts and reattaches background processes must leave it off. A host (CodeRight) turns it on.

## 0.1.4 (2026-11-16)

- Requires rightkit-llm 0.7.0 and rightkit-mcp 0.10.0. Names `agent_mcp-os-keyring` and `agent_providers-os-keyring` explicitly, so the build is unchanged. No behaviour change.

## 0.1.3

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.1.2

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.1.1

- Builds against `rightkit-agent-engine-protocol` 0.4.0: `attach_verdict` leaves `ArtifactVerdict::confidence` unset. No behaviour change.

- Fix: `wrap_untrusted_web_text` fences web text with the protocol's configured untrusted-context tag (`rightkit_agent_engine_protocol::untrusted_context_tag()`, default `rightkit-untrusted-context`); it was a hard-coded `coderight-untrusted-context`. Changes the fence element name for hosts that do not set a tag (from `coderight-untrusted-context` to `rightkit-untrusted-context`).

## 0.1.0 (unpublished)

- New crate. The coding agent's host tools moved here from `rightkit-agent-tools` 0.3.5, where they were the `coding_tools` module behind the `coding_tools` feature. They never referenced the rest of that crate, which was left with the tool contract, gate and backends every app uses, and no longer depends on `rightkit-agent-config` or `rightkit-agent-telemetry` (2026-11-12 shape review).
- Paths: `rightkit_agent_tools::coding_tools::X` is now `rightkit_agent_coding_tools::X`; the module tree under it is unchanged (for example `rightkit_agent_coding_tools::shell::NativeCommandSandboxAdapter`).
- Features: `coding_tools-host` is now `host`, `coding_tools-code-mode` is `code-mode`, `coding_tools-embeddings` is `embeddings`, `coding_tools-test-support` is `test-support`. There is no `coding_tools` feature; the whole crate is the tools. The browser, OCR, media, plugin and PTY dependencies stay behind `host`.
- No behaviour change.
