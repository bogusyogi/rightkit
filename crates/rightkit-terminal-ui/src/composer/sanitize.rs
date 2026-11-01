//! Sanitize text that arrives through paste or history before it reaches the
//! textarea. Lifted verbatim from the Codex donor
//! (`history_cell/messages.rs::sanitize_user_text` @ 444da310).

use std::borrow::Cow;

/// Remove CSI sequences and control characters, preserving tabs and newlines.
pub(crate) fn sanitize_user_text(text: Cow<'_, str>) -> Cow<'_, str> {
    fn sanitize_borrowed(text: &str) -> Cow<'_, str> {
        let mut remaining = Some(text);
        let mut spans = std::iter::from_fn(move || {
            let current = remaining.take()?;
            let mut escaped = false;
            let Some((prefix, suffix)) = current.split_once(|ch: char| {
                escaped = ch == '\x1b';
                escaped || ch.is_control() && !matches!(ch, '\n' | '\t')
            }) else {
                return Some(current);
            };

            remaining = if escaped && let Some(sequence) = suffix.strip_prefix('[') {
                sequence
                    .split_once(|ch: char| ('@'..='~').contains(&ch))
                    .map(|(_, tail)| tail)
            } else {
                Some(suffix)
            };
            Some(prefix)
        })
        .filter(|span| !span.is_empty());

        let first = spans.next().unwrap_or_default();
        let Some(second) = spans.next() else {
            return Cow::Borrowed(first);
        };

        Cow::Owned([first, second].into_iter().chain(spans).fold(
            String::with_capacity(text.len()),
            |mut acc, span| {
                acc.push_str(span);
                acc
            },
        ))
    }

    match text {
        Cow::Borrowed(text) => sanitize_borrowed(text),
        Cow::Owned(mut text) => match sanitize_borrowed(&text) {
            Cow::Owned(sanitized) => Cow::Owned(sanitized),
            Cow::Borrowed(retained) => {
                if retained.is_empty() {
                    text.clear();
                } else if retained.len() != text.len() {
                    // Cannot underflow because retained is a subslice of text.
                    let start = retained.as_ptr().addr() - text.as_ptr().addr();
                    let end = start + retained.len();

                    // Truncate before draining because truncate is constant
                    // time while drain is linear in the size of the receiver.
                    text.truncate(end);
                    drop(text.drain(..start));
                }
                Cow::Owned(text)
            }
        },
    }
}
