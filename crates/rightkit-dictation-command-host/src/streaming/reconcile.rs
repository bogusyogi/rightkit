//! Pure reconcile logic for streaming insertion: where the live-typed text and the final
//! transcript stop agreeing. I/O-free.

/// Where to cut: the shared prefix, moved back to a word boundary unless the
/// rest of `final_text` attaches there (space or closing punctuation), so the
/// delivered remainder never splits a word.
pub fn reconcile_point(typed: &str, final_text: &str) -> usize {
    let common = common_prefix_bytes(typed, final_text);
    let attaches = final_text[common..].chars().next().is_none_or(|c| {
        c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
    });
    if attaches {
        return common;
    }
    typed[..common].rfind(char::is_whitespace).unwrap_or(0)
}

fn common_prefix_bytes(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or_else(|| a.len().min(b.len()), |((index, _), _)| index)
}

#[cfg(test)]
mod tests {
    use super::reconcile_point;

    #[test]
    fn identical_text_cuts_at_the_end() {
        assert_eq!(reconcile_point("hello world", "hello world"), 11);
    }

    #[test]
    fn a_split_word_moves_back_to_the_word_boundary() {
        assert_eq!(reconcile_point("hello wor", "hello world"), 5);
    }

    #[test]
    fn attached_punctuation_keeps_the_shared_prefix() {
        assert_eq!(reconcile_point("hello world", "hello world."), 11);
    }

    #[test]
    fn nothing_shared_cuts_at_the_start() {
        assert_eq!(reconcile_point("abc", "xyz"), 0);
    }
}
