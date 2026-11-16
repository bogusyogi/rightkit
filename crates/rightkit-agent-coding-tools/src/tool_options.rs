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
