//! Shared tool-mention syntax and history encoding. Callers decide which linked paths to accept.

use std::collections::HashMap;
use std::collections::VecDeque;

use crate::composer::mention_syntax::PLUGIN_TEXT_MENTION_SIGIL;
use crate::composer::mention_syntax::TOOL_MENTION_SIGIL;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkedMention {
    pub(crate) sigil: char,
    pub(crate) mention: String,
    pub(crate) path: String,
}

#[allow(dead_code)]
pub(crate) fn encode_history_mentions(text: &str, mentions: &[LinkedMention]) -> String {
    encode_history_mentions_at_elements(text, mentions, &[])
}

pub(crate) fn encode_history_mentions_at_elements(
    text: &str,
    mentions: &[LinkedMention],
    elements: &[crate::composer::text_element::TextElement],
) -> String {
    if mentions.is_empty() || text.is_empty() {
        return text.to_string();
    }

    let mut mentions_by_token: HashMap<(char, &str), VecDeque<&str>> = HashMap::new();
    for mention in mentions {
        if !matches!(
            mention.sigil,
            TOOL_MENTION_SIGIL | PLUGIN_TEXT_MENTION_SIGIL
        ) {
            continue;
        }
        mentions_by_token
            .entry((mention.sigil, mention.mention.as_str()))
            .or_default()
            .push_back(mention.path.as_str());
    }

    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;

    while index < bytes.len() {
        if matches!(
            bytes[index],
            byte if byte == TOOL_MENTION_SIGIL as u8 || byte == PLUGIN_TEXT_MENTION_SIGIL as u8
        ) {
            let sigil = bytes[index] as char;
            if sigil == PLUGIN_TEXT_MENTION_SIGIL
                && let Some(element) = elements
                    .iter()
                    .find(|element| element.byte_range.start == index)
                && let Some(name) = text.get(index + 1..element.byte_range.end)
                && mentions_by_token
                    .get(&(sigil, name))
                    .and_then(VecDeque::front)
                    .is_some_and(|path| {
                        crate::composer::task_mentions::valid_thread_path(path).is_some()
                    })
                && let Some(path) = mentions_by_token
                    .get_mut(&(sigil, name))
                    .and_then(VecDeque::pop_front)
            {
                out.push_str(&crate::composer::task_mentions::format_task_link(
                    name, path,
                ));
                index += 1 + name.len();
                continue;
            }
            if sigil == TOOL_MENTION_SIGIL || starts_plaintext_mention(text, index) {
                let name_start = index + 1;
                if let Some(first) = bytes.get(name_start)
                    && is_mention_name_char(*first)
                {
                    let mut name_end = name_start + 1;
                    while let Some(next) = bytes.get(name_end)
                        && is_mention_name_char(*next)
                    {
                        name_end += 1;
                    }

                    let name = &text[name_start..name_end];
                    if (sigil == TOOL_MENTION_SIGIL || ends_plaintext_mention(bytes, name_end))
                        && mentions_by_token
                            .get(&(sigil, name))
                            .and_then(VecDeque::front)
                            .is_some_and(|path| {
                                crate::composer::task_mentions::valid_thread_path(path).is_none()
                            })
                        && let Some(path) = mentions_by_token
                            .get_mut(&(sigil, name))
                            .and_then(VecDeque::pop_front)
                    {
                        out.push('[');
                        out.push(sigil);
                        out.push_str(name);
                        out.push_str("](");
                        out.push_str(path);
                        out.push(')');
                        index = name_end;
                        continue;
                    }
                }
            }
        }

        let Some(ch) = text[index..].chars().next() else {
            break;
        };
        out.push(ch);
        index += ch.len_utf8();
    }

    out
}

pub(crate) fn is_mention_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-')
}

/// Verbatim donor `ends_plaintext_at_mention` (chat_composer.rs @ 444da310):
/// is byte `index` a boundary that leaves a `@name` token in plaintext?
pub(crate) fn ends_plaintext_at_mention(bytes: &[u8], index: usize) -> bool {
    bytes.get(index).is_none_or(|byte| {
        byte.is_ascii_whitespace()
            || *byte == b'.'
                && bytes.get(index + 1).is_none_or(|next| {
                    next.is_ascii_whitespace()
                        || !next.is_ascii_alphanumeric() && *next != b'_' && *next != b'-'
                })
            || !matches!(*byte, b'.' | b'/' | b'\\')
                && !byte.is_ascii_alphanumeric()
                && *byte != b'_'
                && *byte != b'-'
    })
}

/// Verbatim donor `ends_plaintext_at_dollar_mention`.
pub(crate) fn ends_plaintext_at_dollar_mention(bytes: &[u8], index: usize) -> bool {
    bytes
        .get(index)
        .is_none_or(|byte| !is_mention_name_char(*byte))
}

fn starts_plaintext_mention(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }

    text.get(..index)
        .and_then(|prefix| prefix.chars().next_back())
        .is_some_and(|ch| ch.is_whitespace() || !is_mention_name_char_char(ch))
}

fn ends_plaintext_mention(text_bytes: &[u8], index: usize) -> bool {
    text_bytes.get(index).is_none_or(|byte| {
        byte.is_ascii_whitespace()
            || *byte == b'.'
                && text_bytes.get(index + 1).is_none_or(|next| {
                    next.is_ascii_whitespace()
                        || !next.is_ascii_alphanumeric() && *next != b'_' && *next != b'-'
                })
            || !matches!(*byte, b'.' | b'/' | b'\\')
                && !byte.is_ascii_alphanumeric()
                && *byte != b'_'
                && *byte != b'-'
    })
}

fn is_mention_name_char_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

pub(crate) fn is_common_env_var(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "SHELL"
            | "PWD"
            | "TMPDIR"
            | "TEMP"
            | "TMP"
            | "LANG"
            | "TERM"
            | "XDG_CONFIG_HOME"
    )
}
