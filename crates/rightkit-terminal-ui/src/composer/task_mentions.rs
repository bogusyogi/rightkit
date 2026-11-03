//! Task-mention link syntax lifted from the Codex donor (`task_mentions.rs` @
//! 444da310), reduced to the pure codec functions the composer needs. The
//! donor's async task-reference plumbing (`apply_task_references`, app-server
//! request handles) stays with the parent.

/// Return the inner id when `path` is a well-formed `thread://` reference.
pub(crate) fn valid_thread_path(path: &str) -> Option<&str> {
    let thread_id = path.strip_prefix("thread://")?;
    (!thread_id.is_empty()
        && thread_id.len() <= 64
        && thread_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then_some(thread_id)
}

/// Format `[@title](path)` with title escaping that round-trips through
/// [`parse_task_link`].
pub(crate) fn format_task_link(title: &str, path: &str) -> String {
    format!(
        "[@{}]({path})",
        title
            .replace('\\', "\\\\")
            .replace("](", "]\\(")
            .replace(']', "\\]")
    )
}
