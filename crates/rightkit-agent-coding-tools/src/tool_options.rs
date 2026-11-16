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
    /// to 2 000 before it is encoded for the model, keeping its type. Only in a build with the
    /// `host` feature (the decoder is part of it); elsewhere the image is sent as it is. `false`
    /// (the default) sends the original bytes.
    IMAGES_DOWNSCALED,
    set_images_downscaled,
    images_downscaled
);
