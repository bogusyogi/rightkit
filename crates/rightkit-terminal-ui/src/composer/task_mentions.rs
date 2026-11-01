//! Task-mention link syntax lifted from the Codex donor (`task_mentions.rs` @
//! 444da310), reduced to the pure codec functions the composer needs. The
//! donor's async task-reference plumbing (`apply_task_references`, app-server
//! request handles) stays with the parent.

/// Maximum grapheme count accepted in a `[@title](thread://…)` task link.
pub(crate) const MAX_TASK_TITLE_CHARS: usize = 160;

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

/// Parse `[@title](thread://id)` at `start`, returning title, path, and the
/// offset just past the closing `)`.
pub(crate) fn parse_task_link(text: &str, start: usize) -> Option<(String, String, usize)> {
    let remaining = text.get(start..)?.strip_prefix("[@")?;
    let mut title = String::new();
    let mut chars = remaining.char_indices();
    for _ in 0..=MAX_TASK_TITLE_CHARS {
        let (index, ch) = chars.next()?;
        match ch {
            '\\' => title.push(chars.next()?.1),
            ']' if !title.is_empty() => {
                let suffix = remaining.get(index + 1..)?.strip_prefix('(')?;
                let (path, _) = suffix.split_once(')')?;
                valid_thread_path(path)?;
                return Some((title, path.to_string(), start + index + path.len() + 5));
            }
            ']' => return None,
            _ => title.push(ch),
        }
    }
    None
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
