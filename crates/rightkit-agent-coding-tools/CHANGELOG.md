# Changelog

## 0.2.1

Additive: no public item is removed or re-typed, and a host that sets none of the new options behaves exactly as 0.2.0 does, except where an item below says otherwise. Written and reviewed by reading; not compiled when written.

Options that turn an environment read off (once per process, `false` until the host sets it, the setter returns `false` when a choice was already made; defaults unchanged, the variable is read as before). When on, the variable is not read at all. A release build sets these:

- `tool_options::set_node_env_disabled`: `repl` does not read `<PREFIX>_NODE` (the Node executable it starts).
- `tool_options::set_browser_no_sandbox_env_disabled`: the browser does not read `<PREFIX>_BROWSER_NO_SANDBOX` (starts Chrome without its sandbox when run as root); a root launch is refused and the refusal does not name the variable.
- `tool_options::set_context7_api_base_env_disabled`: `context7` does not read `<PREFIX>_CONTEXT7_API_BASE`.
- `tool_options::set_web_search_url_env_disabled`: `web_search` does not read `<PREFIX>_WEB_SEARCH_<PROVIDER>_URL`.
- `tool_options::set_skills_dir_env_disabled`: the `skill` tool does not read `<PREFIX>_SKILLS_DIR`, and its description does not name it.
- `tool_options::set_resource_profile_env_disabled`: `local_resource_profile_from_env` does not read `<PREFIX>_RESOURCE_PROFILE` and returns Practical.
- `tool_options::set_workspace_trust_store_env_disabled`: `attach_verdict` and `finalize_artifact` do not read `RIGHTKIT_WORKSPACE_TRUST_STORE` and accept the trust store only under the state root. `rightkit-agent-config` still reads that variable itself in a `host` build.
- `shell::set_sandbox_tier_env_disabled`: the shell and the launch adapters do not read `<PREFIX>_SANDBOX_TIER` (the operator's sandbox tier, which can disable confinement).
- `<PREFIX>_MESSAGES_DIR` already has `tool_options::set_send_message_fixed_dir`; `<PREFIX>_SHELL_WITHHOLD_SECRETS` only strengthens a control and has no option.

Environment names this crate reads, all with the host prefix from `rightkit_agent_config::env_name` unless noted (a host declares each in its release manifest): `NODE` (`repl_tool/helpers.rs`, executable), `BROWSER_NO_SANDBOX` (`browser.rs`, weakens a control), `CONTEXT7_API_BASE` (`context7.rs`, endpoint, `host`), `WEB_SEARCH_<PROVIDER>_URL` (`web_search.rs`, endpoint, `host`), `SKILLS_DIR` (`skill_tool.rs`, data root, `host`), `MESSAGES_DIR` (`send_message.rs`, data root, `host`), `RESOURCE_PROFILE` (`process_resource_authority.rs`, resource limits), `SANDBOX_TIER` (`shell/result_sandbox.rs`, weakens a control), `SHELL_WITHHOLD_SECRETS` (`shell/execution_helpers.rs`, Linux, strengthens a control), `QUAL_EFF006_FAULT` (`file_service.rs`, only with `host` and `qualification`), `INTERNAL_RICH_PARSER_WORKER` and `INTERNAL_RICH_PARSER_DOWNSCALE` (`rich_parser_worker.rs`, the private worker protocol, set by this process for its own child; the first is a token gate), `RIGHTKIT_WORKSPACE_TRUST_STORE` (`attach_verdict.rs`, fixed name, `host`), and the standard `http_proxy`/`https_proxy`/`all_proxy`/`no_proxy` and upper-case forms (`browser.rs`, the egress proxy's upstream), `PATH`, `PATHEXT`, `HOME`, `USERPROFILE`, `SystemRoot`, `DISPLAY`, `XAUTHORITY` (operating-system lookups). The launch environment key `<PREFIX>_SANDBOX_READ_ROOTS` is read from a launch's own environment, not the process's, and only interpreter install directories are honoured. `rightkit-agent-config`, `rightkit-mcp` and `rightkit-llm` read further names in a `host` build through their own `host-env` features.

Without `host` (iOS):

- Images are scaled to the provider's longest edge again when `tool_options::set_images_downscaled(true)`, with the same bounded decode as before (declared size over 40 000 000 pixels is sent as it is, decoder allocation capped at 256 MiB). `image` (PNG and JPEG only) is now a dependency of every build, not only `host`.
- `rightkit-sandbox` is taken with `default-features = false, features = ["execution"]`; its `pty` feature (and with it `rightkit-pty` and `openpty`) comes only with `host`. `execution` stays in every build because `ToolContext` carries its launch-adapter types.

Host-named strings. Computed at use time from `rightkit_agent_config::{app_name, host_slug}`; the default host's bytes are unchanged. A host named `CodeRight` (`CodeRight`, `coderight`) gets what CodeRight's own crate wrote:

- `User-Agent`: `<AppName>/0.1` (`Agent/0.1`, `CodeRight/0.1`) for `browser_fetch` and the crate's HTTP clients, `<AppName>/0.1 web_search`, `Mozilla/5.0 (compatible; <AppName>/1.0)` for the crawler, and `<slug>/1.0` for GitHub (`rightkit-agent/1.0`, `coderight/1.0`).
- Workspace checkpoints: the private gitconfig and index files in `.git` are `<slug>-checkpoint.gitconfig` and `<slug>-checkpoint.index`, the commit identity when no git identity is configured is `<slug>-checkpoint <checkpoint@<slug>.local>` (auto-commit: `<slug> <auto-commit@<slug>.local>`), where `<slug>` is `rightkit-agent` for the default host and the host's slug otherwise. Existing checkpoints are found by their record file `<state dir>/checkpoints/<commit id>.json` and restored from the commit object by id; neither the author nor the private files are consulted, so checkpoints made under another name stay listable and restorable. The private files are scratch (the index is deleted after each snapshot, the gitconfig rewritten), so an older one left behind is harmless.
- The REPL traceback file name `<<slug>-repl>`, thread names `<slug>-run-program-js`, `<slug>-pty-reader`, `<slug>-pty-writer`, the worktree temp folder `<slug>-wt-<name>`, the native launch backend `<slug>-native-policy`, the tunnel default label `<slug>-dev-tunnel`, the visible-window error `could not resolve the <slug> executable path`, the legacy `spawn_subagent` `visible` text `` `<slug> attach` ``, and the product name in the tunnel, cloud-workspace and Fly image texts.
- Left as they are: the Windows AppContainer profile owner prefix `rightkit-agent.sandbox.workspace.*`, its `Temp\rightkit-agent` alias folder and its `\\.\pipe\rightkit-agent-appcontainer-*` pipes (operating-system objects and access-control entries, whose rename would orphan entries), the default `set_shell_notice_label` text (the host sets it), and `PROCESS_RESOURCE_PROFILE_ENV` / `SANDBOX_READ_ROOTS_ENV` (the default names; read the `*_env_name()` functions).

## 0.2.0

Taken from CodeRight's private tools crate so that crate can be deleted (bogusyogi/coderight#567). The two had diverged both ways; this release is a superset. Breaking, so a minor release: `FileService::copy_with_intent` takes the workspace root as a fourth argument, `SubagentSpawnOptions` gains `queue_when_full`, `bare`, `fork`, `handoff_summary` and `tool_call_id`, `ToolContext` gains `advisor`, `SendMessageTool`, `SkillTool`, `TeamCreateTool`, `TeamDeleteTool` and `ScheduleCronTool` are built with `new()`, and the two unused crate-private helpers `spawn_windows_appcontainer` and `local_sandbox_config_with_home` are gone. Written and reviewed by reading against both sources; not compiled when written. CodeRight's unit tests are not ported.

Host names: every state-folder and environment name in the crate now comes from `rightkit_agent_config::{state_dir_name, env_name, app_name, host_slug}`, so the defaults are unchanged and a host that names itself gets its own. Name the host before the first tool, registry build or `rich_parser_worker::run_if_requested`. `PROCESS_RESOURCE_PROFILE_ENV` and `SANDBOX_READ_ROOTS_ENV` stay as the default names; read `process_resource_profile_env_name()` and `sandbox_read_roots_env_name()`.

New once-per-process options, all `false` until the host sets them (defaults unchanged):

- `tool_options`: `recursive_fetch_capped_output`, `browser_fetch_markdown_text`, `browser_compact_observations`, `search_workspace_grep` (the `search` tool on `rightkit-search`), `git_write_full_description`, `repl_eval_timeout`, `refuse_unenforced_resource_limits`, `task_store_merged`, `attach_verdict_confidence`, `send_message_fixed_dir`, `spawn_subagent_extended`, `tool_search_on_demand_description`, `tool_schema_strict_enums`, `code_mode_uncompressed_fallback`, `capability_catalog_newer`, `schedule_cron_sunday_seven`, `mcp_listing_newer`, `mcp_definition_pins` (trust-on-first-use pins for MCP tool definitions through `rightkit-agent-config` 0.1.5), `skill_files_and_search`.
- `shell`: `shell_exec_boundary_guard`, `shell_refuse_unenforced_path_deny`, `shell_background_follows_session`, `sandbox_rerun_grant_unix`, `native_vendor_own_executable_read`, `strip_parent_claude_session_env`, `sandbox_policy_confinement`, `managed_pty_streamed`, `sandbox_strict_driver_profile`, `process_lease_ack_counts`, `background_output_base64_when_lossy`, `shell_approval_for_remote_content_only`, and `set_extra_scrubbed_env_names`. `set_shell_protect_config_files` now selects only the restore guard; the looser "only remote content needs approval" rule is the separate option.
- `sleep_tool::set_sleep_honours_cancel`.

For every host (stricter, or a fix):

- `recursive_fetch` compares parsed origins; the prefix test accepted `https://a.com.evil.com` for `a.com`.
- The EFF-006 crash-window fault hook in `file_service` is compiled only with `host` and the new `qualification` feature; it was in every `host` build.
- MCP error text and binary-resource text are framed as untrusted, and only the canonical Pull tool skips the untrusted wrapper.
- The browser sends Chrome's non-loopback traffic through a loopback egress proxy and refuses an `https`, `socks` or unparseable upstream proxy; owned browsers are closed at exit.
- Media tool output is capped per stream; `git_merge` checks the write-deny rules and reports its write targets; `durable_write` classifies `.mcp.json`, hooks, MCP, config, policy, permission-rule and schedule files; a confined copy refuses symlinks and stays inside the workspace roots.
- `task_create` never reuses an id on disk; a workspace restore takes a `before-restore` checkpoint first.
- Skill files are read up to 256 KiB, only from regular files, and a skill with frontmatter is refused when no `SkillCatalog` is installed.
- GitHub `add_comment` without `confirm` is refused before the token is read; Cloudflare and Fly actions are validated before the credential card.
- On Windows `release_pinned_read_roots` also releases `rightkit-sandbox`'s pins; the visible subagent window's whole process tree is ended with it.
- `browser_fetch` JavaScript rendering honours the cancel signal; iOS gets a host-installed WebKit renderer (`install_webkit_renderer`).
- `time` zones follow daylight saving rules.
- `BgShell::try_lease_snapshot_now` reads the lease without blocking or panicking; background output is read in 64 KiB chunks; `set_anchor_children_to_parent` also ties the child's process group and acts on macOS.
- New: `advisor_tool`, `mcp_tools::direct`, `edit_helpers::{edit_only_inserts, edits_only_insert}` public, `SpawnRuntimeKind::HostNative` also reads `code_right_native`.

Dependencies: `rightkit-search` 0.4.6 (`workspace-grep`), `rightkit-agent-config` 0.1.5, `rightkit-child-process` 0.5.16.

## 0.1.9 (2026-11-16)

- Takes the file-tool behaviour of a host's newer in-tree tools crate, each part behind a once-per-process option in `tool_options` that is `false` until the host sets it (the setter returns `false` and changes nothing when a choice was already made). With none set, every description, schema, result and refusal is 0.1.7's, and the registry's schema digest is unchanged. `set_edit_read_discipline`: `read_file` records the windows it returns and reading a file in successive windows from line 1 to the end counts as a complete read (a window that skips lines, or only the last window, never does; the refusal names the next offset to read); an `edit_file`, `multi_edit` or `apply_patch` edit whose text matched exactly or through a HashLine anchor needs no earlier read and is bound to the revision observed before the edit was computed, an edit that matched only through the whitespace or punctuation tolerance needs a complete current read, a write made on a complete read or creating a file leaves a complete read behind, and `apply_patch` observes the file instead of recording a read the model never made; the `read_file`, `edit_file`, `multi_edit` and `write_file` descriptions say so. `set_read_file_windows`: `read_file` scales its caps for a model window under 131 072 tokens (new `ToolContext::model_window_tokens`, `None` by default), takes `allow_large` (1 MiB, no line cap), answers a file with a NUL byte in its first 8 KiB with a binary-file notice and no read receipt, and names the right line to resume at. `set_edit_file_newer`: `edit_file` and `multi_edit` refuse a file over 10 MiB, count the places a replace-all changed and name the tolerant rung of an inexact match, say "already applied" when old_string is missing and the new text is present, keep the file's indentation on a tolerant match, write CRLF only where the matched text or the whole file uses it, name a non-UTF-8 file as the reason it cannot be edited, and refuse a `multi_edit` entry missing `old_string` or `new_string`. `set_edit_test_file_additions`: an exact, insert-only edit that adds whole lines before or after the matched text of a protected test file goes ahead instead of asking. `set_apply_patch_newer`: `apply_patch` accepts a Codex patch (`Update File` and `Add File`; `Delete File` and `Move to` are refused; `Add File` of an existing path is refused), reads a context line of one space plus `---`, `+++` or `*** ` as that text, refuses text before the first header, a header with no lines and an envelope marker used as a path, locates a run of old lines as one contiguous block, keeps each line's own ending (a CRLF file stays CRLF) and refuses a target over 10 MiB. `set_list_files_newer`: `list_files` globs stop `*` and `?` at `/`, walk in path order (a listing cut at `max` is the same every time) and end a truncated listing with a hint. `set_images_downscaled`: `view_image` and `read_file` scale a PNG or JPEG over 2 000 pixels on its longest edge to 2 000 (`host` feature; the rich-parser worker is told through its environment; a declared size over 40 million pixels is not decoded, and the decoder may allocate at most 256 MiB).
- Unconditional: `edit_file`, `multi_edit`, `undo_edit` and `apply_patch` evaluate the permission policy on the resolved workspace-relative path (as `write_file` and `read_file` already did), not the caller's spelling, so a symlink alias or `..` form of a protected test file no longer escapes the policy. The `apply_patch` nearest-line hint skips lines over 1 024 bytes and looks at most 20 000 lines ahead (its edit-distance table grew with the product of two line lengths). New public items: `FileService::record_read_window`, `observe_when_unread`, `require_read_receipt` and `write_with_receipt_intent_from`; `ToolContext::model_window_tokens`. The `host` feature now also enables the optional `image` dependency (PNG and JPEG only).

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
