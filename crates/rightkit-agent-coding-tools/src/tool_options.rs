//! Once-per-process host options for individual tools. Every option is `false` until the host sets
//! it, and every setter returns `false` (changing nothing) when a choice was already made. With all
//! of them at their default each tool behaves exactly as before. The shell tool's options are in
//! the `shell` module (feature `host`). A host sets these once at start-up, before the first tool call.

host_option!(
    /// `send_message` refuses the channel names `schedule` and `followup_task` with
    /// `send_message: channel '<name>' is reserved`, because they are the daemon's own channels
    /// and a model naming them would slip past the workspace and lineage scope. `false` (the
    /// default) passes any channel name through.
    SEND_MESSAGE_RESERVED_CHANNELS,
    set_send_message_reserved_channels,
    send_message_reserved_channels
);

host_option!(
    /// `write_file` decides "replace or create" from whether the path exists on disk rather than
    /// from whether its old content could be read as UTF-8, so overwriting an existing file that
    /// is not UTF-8 is a replace, not a create. `false` (the default) keeps the UTF-8 test.
    WRITE_FILE_REPLACE_NON_UTF8,
    set_write_file_replace_non_utf8,
    write_file_replace_non_utf8
);

host_option!(
    /// `context7` `get-library-docs` caps `tokens` at 20 000 and fences the returned community
    /// documentation as untrusted external content, as web fetch results are. `false` (the
    /// default) passes `tokens` through and returns the text as it is.
    CONTEXT7_UNTRUSTED_DOCS,
    set_context7_untrusted_docs,
    context7_untrusted_docs
);

host_option!(
    /// `tool_search` with no query returns a name index of up to 8 192 bytes instead of 2 048, so
    /// the whole built-in index fits. `false` (the default) keeps the 2 048-byte cap.
    TOOL_SEARCH_FULL_INDEX,
    set_tool_search_full_index,
    tool_search_full_index
);

host_option!(
    /// `finalize_artifact` refuses an artifact whose attached verdict did not pass, naming the
    /// missing passing verdict. `false` (the default) finalizes whatever the store allows.
    FINALIZE_REQUIRES_PASSING_VERDICT,
    set_finalize_requires_passing_verdict,
    finalize_requires_passing_verdict
);

host_option!(
    /// Reduced shell output of a successful command keeps its head and its tail (the first and last
    /// 25 KiB, with a note of the bytes dropped between) instead of only the first 50 KiB, and
    /// text that a progress bar redrew with `\r` is cut to each line's final state. `false` (the
    /// default) keeps the head only and leaves carriage returns in.
    OUTPUT_KEEPS_TAIL,
    set_output_keeps_tail,
    output_keeps_tail
);

host_option!(
    /// The read-before-edit rules of the file tools. On: `read_file` records the windows it
    /// returns, and reading a large file in successive windows from line 1 to the end counts as
    /// reading it completely (a window that skips lines never does), with the refusal naming the
    /// offset to read next; an `edit_file`, `multi_edit` or `apply_patch` edit whose text matched
    /// exactly (or through a HashLine anchor) needs no earlier `read_file`, because the match is
    /// itself the proof that the caller knows the text, and the write is still bound to the file's
    /// revision as observed before the edit was computed; an edit that matched only through the
    /// whitespace or punctuation tolerance needs a complete, current read; a write made on top of a
    /// complete read, or that creates a file, leaves a complete read behind for the new content; and
    /// `apply_patch` binds to the revision it observed instead of recording a read the model never
    /// made. The descriptions of `read_file`, `edit_file`, `multi_edit` and `write_file` say so.
    /// `false` (the default) keeps the rule that every non-anchored edit needs a complete read.
    EDIT_READ_DISCIPLINE,
    set_edit_read_discipline,
    edit_read_discipline
);

host_option!(
    /// `read_file` scales its default line and byte caps down in proportion for a model window
    /// under 131 072 tokens (`ToolContext::model_window_tokens`), accepts `allow_large: true`
    /// (up to 1 MiB in one call, no line cap), answers a file with a NUL byte in its first 8 KiB
    /// with a "binary file" notice and records no read for it, and names the right line to resume
    /// at when a read is cut short (the default names the last line it showed). Adds `allow_large`
    /// to the schema and changes the description. `false` (the default) keeps the fixed 2 000 lines
    /// and 128 KiB, reads a binary file lossily and keeps the resume hint.
    READ_FILE_WINDOWS,
    set_read_file_windows,
    read_file_windows
);

host_option!(
    /// `edit_file` and `multi_edit`: refuse a file over 10 MiB before loading it; report the number
    /// of places a replace-all really changed and name the tolerant rung when the match was not
    /// exact; say "already applied" when old_string is missing but the new text is in the file;
    /// keep the file's own indentation when a tolerant match starts at the first non-blank
    /// character; write a new line as CRLF only where the matched text, or the whole file, uses
    /// CRLF (otherwise the replacement is written as given); name a non-UTF-8 file as the reason it
    /// cannot be edited; and refuse a `multi_edit` entry that lacks `old_string` or `new_string`
    /// instead of treating the absent one as empty. Changes the two descriptions. `false` (the
    /// default) keeps the earlier results.
    EDIT_FILE_NEWER,
    set_edit_file_newer,
    edit_file_newer
);

host_option!(
    /// When the permission policy asks before changing a protected test file, an `edit_file` or
    /// `multi_edit` that only inserts whole lines goes ahead instead of asking, so an agent can add
    /// a test case next to the existing ones: the match must be exact (not a HashLine block, not a
    /// tolerant match), `replace_all` unset, and the replacement the matched text with lines added
    /// directly before it or after it, so no existing character is changed and the addition does
    /// not start or end inside a line. A change or removal of existing test text, and an insertion
    /// in the middle of a line, still ask. An added line can still change what a test does (an
    /// early `return`, a skip attribute); the rule narrows the protection to rewrites and does not
    /// prove the addition is honest. Needs no other option. `false` (the default) asks for every
    /// edit of a protected test file.
    EDIT_TEST_FILE_ADDITIONS,
    set_edit_test_file_additions,
    edit_test_file_additions
);

host_option!(
    /// `apply_patch` accepts a Codex patch (`*** Begin Patch` with `*** Update File:` and
    /// `*** Add File:` sections; `Delete File` and `Move to` are refused with a pointer to the right
    /// tool; an `Add File` for an existing path is refused), reads a context line that is one space
    /// plus `---`, `+++` or `*** ` as that text, refuses text before the first header, a header
    /// with no lines and an envelope marker used as a path, locates a run of old lines as one
    /// contiguous block instead of line by line, keeps each original line's own line ending (a
    /// CRLF file stays CRLF, and a new file ends with a newline), and refuses a target file over
    /// 10 MiB. Changes the description. `false` (the default) keeps the line-by-line seek, rewrites
    /// every line ending as LF and loads a target of any size.
    APPLY_PATCH_NEWER,
    set_apply_patch_newer,
    apply_patch_newer
);

host_option!(
    /// `list_files` matches `glob` with `*` and `?` stopping at `/` (`src/*.rs` lists the files of
    /// `src`, not those nested below it), walks in path order so a listing cut at `max` is the same
    /// first `max` paths every time, and ends a truncated listing with a hint to narrow `glob` or
    /// `path`. `false` (the default) lets `*` cross `/` and keeps the walk order.
    LIST_FILES_NEWER,
    set_list_files_newer,
    list_files_newer
);

host_option!(
    /// `view_image` and `read_file` scale a PNG or JPEG whose longest edge is over 2 000 pixels down
    /// to 2 000 before it is encoded for the model, keeping its type, in every build (with or
    /// without the `host` feature). The decode is bounded: an image whose header declares over
    /// 40 000 000 pixels is sent as it is, and the decoder may allocate at most 256 MiB. `false`
    /// (the default) sends the original bytes.
    IMAGES_DOWNSCALED,
    set_images_downscaled,
    images_downscaled
);

host_option!(
    /// `attach_verdict` checks `confidence` (a number from 0 to 1, as its schema requires: a missing
    /// or out-of-range value is refused) and stores it with the verdict. `false` (the default)
    /// stores the verdict without a confidence and accepts a call that omits it.
    ATTACH_VERDICT_CONFIDENCE,
    set_attach_verdict_confidence,
    attach_verdict_confidence
);

host_option!(
    /// `send_message` always writes to the state root's `messages` folder and ignores the
    /// `MESSAGES_DIR` environment variable of the host prefix (see
    /// `rightkit_agent_config::env_name`), which in a build with the `host` feature otherwise
    /// relocates the drop directory. `false` (the default) honours the variable in that build.
    SEND_MESSAGE_FIXED_DIR,
    set_send_message_fixed_dir,
    send_message_fixed_dir
);

host_option!(
    /// `spawn_subagent` offers and honours `background`, `fork`, `handoff_summary` and `bare`:
    /// a background child returns `{agentId, status}` at once and `wait_for_subagents` collects
    /// its final response from an in-process mailbox (`queued` when the spawner reports a full
    /// concurrent cap through `SubagentSpawner::concurrent_cap`); a forked or summarised child
    /// starts from the parent's transcript or a summary of it and a bare child from nothing but
    /// the task, all three carried to the host in `SubagentSpawnOptions`; the model's call id is
    /// passed on as `SubagentSpawnOptions::tool_call_id`. The tool descriptions and schemas say so.
    /// The host's spawner must act on those option fields. `false` (the default) ignores those
    /// four arguments, offers none of them and leaves the schema and descriptions as they were.
    SPAWN_SUBAGENT_EXTENDED,
    set_spawn_subagent_extended,
    spawn_subagent_extended
);

host_option!(
    /// `tool_search` describes itself as the way to find and load every tool that is not in the
    /// current tool list (git, web search, browser, sub-agents, skills and MCP tools), and the
    /// capability catalog (`default_tool_capabilities`) carries the same text. `false` (the
    /// default) keeps the short description.
    TOOL_SEARCH_ON_DEMAND_DESCRIPTION,
    set_tool_search_on_demand_description,
    tool_search_on_demand_description
);

host_option!(
    /// Tool definitions sent to a provider (`tool_defs_for_messages`, `deferred_tool_defs`) and the
    /// MCP schema texts (`list_mcp_tools`, the hint after a failed `mcp_call`) rewrite a `oneOf` or
    /// `anyOf` list of string constants as `{"type": "string", "enum": [...]}`, which strict
    /// validators require. `false` (the default) sends each schema as the tool wrote it.
    TOOL_SCHEMA_STRICT_ENUMS,
    set_tool_schema_strict_enums,
    tool_schema_strict_enums
);

host_option!(
    /// `run_program` (code mode) with no context worker to compress through returns the program's own
    /// output, cut to the result limit, with a note that context is off, instead of an error.
    /// `false` (the default) returns the error "Pull result compression unavailable".
    CODE_MODE_UNCOMPRESSED_FALLBACK,
    set_code_mode_uncompressed_fallback,
    code_mode_uncompressed_fallback
);

host_option!(
    /// The capability catalog (`default_tool_capabilities`) lists the newer descriptions of
    /// `edit_file`, `multi_edit`, `read_file`, `write_file`, `git_write` and `search` (the text those
    /// tools give with their own host options on). `false` (the default) keeps the earlier catalog
    /// text.
    CAPABILITY_CATALOG_NEWER,
    set_capability_catalog_newer,
    capability_catalog_newer
);

host_option!(
    /// `schedule_cron` accepts the value `7` in the weekday field (`0` to `7`). `false` (the default)
    /// accepts `0` to `6` only.
    SCHEDULE_CRON_SUNDAY_SEVEN,
    set_schedule_cron_sunday_seven,
    schedule_cron_sunday_seven
);

host_option!(
    /// `list_mcp_tools` takes `server` (waits for that server to start) and `query` filters, bounds
    /// each description to 2 048 bytes, carries input schemas for a narrowed or short list
    /// (parameter names only for a long one), and `mcp_call`
    /// appends the expected input schema to a failed call (inside the wrapped error). The
    /// description and schema of `list_mcp_tools` say so. `false` (the default) keeps the plain
    /// listing.
    MCP_LISTING_NEWER,
    set_mcp_listing_newer,
    mcp_listing_newer
);

host_option!(
    /// MCP tool-definition pinning (trust on first use): the first time a tool's name, description,
    /// annotations and input schema are seen they are pinned in `pins/mcp-tool-definitions.json` of
    /// the state root; a later different definition is withheld from `list_mcp_tools` and refused
    /// by `mcp_call`, with the remedy named, and `mcp_tools::mcp_definition_changed` reports it.
    /// `false` (the default) pins nothing and refuses nothing.
    MCP_DEFINITION_PINS,
    set_mcp_definition_pins,
    mcp_definition_pins
);

host_option!(
    /// `invoke_skill` accepts `file` (a text file inside the skill's folder or the shared folder next
    /// to the skills) and `query` (a search over the slugs and descriptions of the active skills),
    /// and its description and schema say so; the schema no longer requires `skill`. `false` (the
    /// default) keeps the slug-only schema and ignores both arguments.
    SKILL_FILES_AND_SEARCH,
    set_skill_files_and_search,
    skill_files_and_search
);

host_option!(
    /// `recursive_fetch` caps each page's text at 4 000 bytes and the whole answer at 40 000, and
    /// says so in its description. `false` (the default) returns the pages uncapped.
    RECURSIVE_FETCH_CAPPED_OUTPUT,
    set_recursive_fetch_capped_output,
    recursive_fetch_capped_output
);

host_option!(
    /// `browser_fetch` returns the readable text of a page as Markdown (headings, lists, links)
    /// instead of flattened plain text. `false` (the default) keeps the plain text.
    BROWSER_FETCH_MARKDOWN_TEXT,
    set_browser_fetch_markdown_text,
    browser_fetch_markdown_text
);

host_option!(
    /// `browser` returns compact page observations (the trimmed element list) instead of the full
    /// ones. `false` (the default) keeps the full observations.
    BROWSER_COMPACT_OBSERVATIONS,
    set_browser_compact_observations,
    browser_compact_observations
);

host_option!(
    /// `search` runs on `rightkit-search`'s workspace grep: workspace-relative paths, `multiline`
    /// supported, `max_results` capped at 200. `false` (the default) keeps this crate's own walk
    /// and match, with absolute paths and no cap.
    SEARCH_WORKSPACE_GREP,
    set_search_workspace_grep,
    search_workspace_grep
);

host_option!(
    /// `git_write` shows the model the full description of each action. `false` (the default)
    /// keeps the short description.
    GIT_WRITE_FULL_DESCRIPTION,
    set_git_write_full_description,
    git_write_full_description
);

host_option!(
    /// `repl` accepts `timeout_ms` (default 10 000, max 300 000): the helper interrupts the
    /// evaluation at the limit and keeps the session; the call is cut off 5 s later if the helper
    /// cannot interrupt. Adds `timeout_ms` to the schema. `false` (the default) keeps the fixed
    /// 10 s cut-off that ends the session.
    REPL_EVAL_TIMEOUT,
    set_repl_eval_timeout,
    repl_eval_timeout
);

host_option!(
    /// `repl` start and `task_create` with a command refuse when the session configures resource
    /// limits (cpu, memory, disk) that local execution cannot enforce. `false` (the default) runs
    /// them.
    REFUSE_UNENFORCED_RESOURCE_LIMITS,
    set_refuse_unenforced_resource_limits,
    refuse_unenforced_resource_limits
);

host_option!(
    /// `tasks.json` is written under a process lock, merged by `updated_at` stamp with what is on
    /// disk, and replaced atomically; in-memory copies never roll back a newer disk record. `false`
    /// (the default) overwrites the file with the caller's records.
    TASK_STORE_MERGED,
    set_task_store_merged,
    task_store_merged
);

host_option!(
    /// The `repl` tool no longer reads the `NODE` environment variable of the host prefix (see
    /// `rightkit_agent_config::env_name`), which otherwise names the Node executable it starts;
    /// the variable is not read at all. Node is then the bundled runtime on Windows when present,
    /// else `node` from `PATH`. A release build that must not let the environment choose which
    /// program runs sets this. `false` (the default) reads the variable.
    NODE_ENV_DISABLED,
    set_node_env_disabled,
    node_env_disabled
);

host_option!(
    /// The browser tool no longer reads the `BROWSER_NO_SANDBOX` environment variable of the host
    /// prefix, which otherwise starts the owned Chrome without its sandbox when the process runs
    /// as root; the variable is not read at all, and a root launch is refused as it is when the
    /// variable is unset. `false` (the default) reads the variable. Applies to builds with the
    /// `host` feature.
    BROWSER_NO_SANDBOX_ENV_DISABLED,
    set_browser_no_sandbox_env_disabled,
    browser_no_sandbox_env_disabled
);

host_option!(
    /// `context7` no longer reads the `CONTEXT7_API_BASE` environment variable of the host prefix,
    /// which otherwise redirects its lookups to another endpoint; the variable is not read at all
    /// and the lookups always go to context7.com. `false` (the default) reads the variable in a
    /// build with the `host` feature.
    CONTEXT7_API_BASE_ENV_DISABLED,
    set_context7_api_base_env_disabled,
    context7_api_base_env_disabled
);

host_option!(
    /// `web_search` no longer reads the `WEB_SEARCH_<PROVIDER>_URL` environment variables of the
    /// host prefix, which otherwise redirect a provider's requests to another endpoint; the
    /// variables are not read at all and each provider's own service is used. `false` (the
    /// default) reads them in a build with the `host` feature.
    WEB_SEARCH_URL_ENV_DISABLED,
    set_web_search_url_env_disabled,
    web_search_url_env_disabled
);

host_option!(
    /// The `skill` tool no longer reads the `SKILLS_DIR` environment variable of the host prefix,
    /// which otherwise adds a directory to look for skills in; the variable is not read at all,
    /// the description does not name it, and skills come from the context's skill directories or
    /// the state root's `skills` folder. `false` (the default) reads the variable in a build with
    /// the `host` feature.
    SKILLS_DIR_ENV_DISABLED,
    set_skills_dir_env_disabled,
    skills_dir_env_disabled
);

host_option!(
    /// `local_resource_profile_from_env` no longer reads the `RESOURCE_PROFILE` environment
    /// variable of the host prefix, which otherwise raises or lowers the process resource profile;
    /// the variable is not read at all and the profile is Practical. `false` (the default) reads
    /// the variable. Applies to builds with the `host` feature.
    RESOURCE_PROFILE_ENV_DISABLED,
    set_resource_profile_env_disabled,
    resource_profile_env_disabled
);

host_option!(
    /// `attach_verdict` and `finalize_artifact` no longer honour an override of the workspace
    /// trust store given by the `RIGHTKIT_WORKSPACE_TRUST_STORE` environment variable (read by
    /// `rightkit-agent-config` in a `host` build): this crate does not read the variable, and the
    /// two tools accept the store only when it is under the state root, refusing otherwise.
    /// `false` (the default) also accepts the store the variable names. This does not stop
    /// `rightkit-agent-config` from reading the variable for its own callers.
    WORKSPACE_TRUST_STORE_ENV_DISABLED,
    set_workspace_trust_store_env_disabled,
    workspace_trust_store_env_disabled
);
