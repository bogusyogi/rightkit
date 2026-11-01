//! Slash-command detection inside the composer.
//!
//! Adapted from the Codex donor `bottom_pane/chat_composer/slash_input.rs`
//! @ 444da310. The donor resolves against its built-in `SlashCommand`
//! registry; the host app resolves against the host-supplied shared command
//! catalog (`CompletionCandidate`s) plus the six `command_effects` effects.
//! The token-parsing helpers (`parse_slash_name`, `is_editing_command_name`,
//! `command_popup_filter_text`) retain the donor's semantics.

use crate::composer::completion::CompletionCandidate;

/// The result of validating a submission that begins with `/`.
pub(crate) enum SubmissionValidation {
    /// First line is not a slash command (or starts with a space).
    NotCommand,
    /// `/name` resolved to a catalog candidate (possibly with args).
    Recognized,
    /// `/name` did not resolve to any catalog candidate.
    UnknownCommand(String),
}

/// Parse `"/name rest..."` from raw text — verbatim donor
/// `prompt_args::parse_slash_name`.
///
/// Returns `(name, rest, rest_offset)` where `rest` is the trimmed substring
/// after the command token and `rest_offset` is its byte offset in `text`.
/// Returns `None` when `text` does not start with `/` or has an empty name.
pub(crate) fn parse_slash_name(text: &str) -> Option<(&str, &str, usize)> {
    let stripped = text.strip_prefix('/')?;
    let mut name_end_in_stripped = stripped.len();
    for (idx, ch) in stripped.char_indices() {
        if ch.is_whitespace() {
            name_end_in_stripped = idx;
            break;
        }
    }
    let name = &stripped[..name_end_in_stripped];
    if name.is_empty() {
        return None;
    }
    let rest_untrimmed = &stripped[name_end_in_stripped..];
    let rest = rest_untrimmed.trim_start();
    let rest_start_in_stripped = name_end_in_stripped + (rest_untrimmed.len() - rest.len());
    let rest_offset = rest_start_in_stripped + 1;
    Some((name, rest, rest_offset))
}

/// If the cursor is currently within a slash command on the first line,
/// extract the command fragment before the cursor and the rest of the line
/// after it — verbatim donor `command_under_cursor`.
///
/// A cursor at or before the `/` (byte offset ≤ 1) is treated as sitting at
/// the end of the name token, so positions on the slash itself still resolve
/// the whole command and a bare `"/"` reports an empty name rather than
/// panicking on an inverted range.
fn command_under_cursor(first_line: &str, cursor: usize) -> Option<(&str, &str)> {
    if !first_line.starts_with('/') {
        return None;
    }
    if cursor > first_line.len() || !first_line.is_char_boundary(cursor) {
        return None;
    }

    let name_start = 1usize;
    let name_end = first_line[name_start..]
        .find(char::is_whitespace)
        .map(|idx| name_start + idx)
        .unwrap_or_else(|| first_line.len());

    let cursor = if cursor <= name_start {
        name_end
    } else {
        cursor
    };
    if cursor > name_end {
        return None;
    }

    let name = &first_line[name_start..cursor];
    let rest = &first_line[cursor..];

    Some((name, rest))
}

/// Whether the caret is inside the initial `/name` token on the first line
/// and that fragment can still resolve to a catalog command — donor
/// `SlashInput::is_editing_command_name` over the shared catalog: an empty
/// name counts only for a bare `/`, otherwise the fragment must fuzzy-match
/// some command name or alias.
pub(crate) fn is_editing_command_name(
    first_line: &str,
    cursor: usize,
    catalog: &[CompletionCandidate],
) -> bool {
    let Some((name, rest)) = command_under_cursor(first_line, cursor) else {
        return false;
    };
    if name.is_empty() {
        return rest.is_empty();
    }
    let name_lower = name.to_lowercase();
    catalog.iter().any(|candidate| {
        super::fuzzy_match::fuzzy_match(&candidate.name, name).is_some()
            || candidate
                .aliases
                .iter()
                .any(|alias| super::fuzzy_match::fuzzy_match(alias, name).is_some())
            || super::command_popup::description_match_any(&candidate.description, &name_lower)
    })
}

/// The popup filter text for the command under the cursor, if the caret is
/// inside the `/name` token. Returns the fragment *with* its `/` because
/// `CompletionPopup::on_composer_text_change` expects a `/`-prefixed line.
pub(crate) fn command_popup_filter_text(first_line: &str, cursor: usize) -> Option<String> {
    let (name, _rest) = command_under_cursor(first_line, cursor)?;
    Some(format!("/{name}"))
}

/// Whether `first_line` holds the `/name` token under edit (used by
/// `sync_command_popup`; wraps [`is_editing_command_name`] plus the non-empty
/// name requirement).
pub(crate) fn is_command_name_edit(
    first_line: &str,
    cursor: usize,
    catalog: &[CompletionCandidate],
) -> bool {
    is_editing_command_name(first_line, cursor, catalog) && parse_slash_name(first_line).is_some()
}

/// Resolve a command name against the host-supplied catalog (name or alias),
/// case-insensitively, matching donor `find_slash_command` semantics.
pub(crate) fn find_command<'a>(
    catalog: &'a [CompletionCandidate],
    name: &str,
) -> Option<&'a CompletionCandidate> {
    let needle = name.to_lowercase();
    catalog.iter().find(|candidate| {
        candidate.name.to_lowercase() == needle
            || candidate
                .aliases
                .iter()
                .any(|alias| alias.to_lowercase() == needle)
    })
}

/// Validate a submitting text against the catalog.
///
/// Mirrors the donor `validate_submission`: a leading space escapes command
/// parsing; a first line starting with `/` must resolve.
pub(crate) fn validate_submission(
    catalog: &[CompletionCandidate],
    text: &str,
    input_starts_with_space: bool,
) -> SubmissionValidation {
    if input_starts_with_space {
        return SubmissionValidation::NotCommand;
    }
    let Some((name, _, _)) = parse_slash_name(text) else {
        return SubmissionValidation::NotCommand;
    };
    if find_command(catalog, name).is_some() {
        SubmissionValidation::Recognized
    } else {
        SubmissionValidation::UnknownCommand(name.to_string())
    }
}

/// The bare `/name` command in `text`, if the first line is exactly a command
/// token with no arguments. Used by the "type `/diff`, press Enter" dispatch
/// path where the popup may already be hidden.
pub(crate) fn bare_command<'a>(
    catalog: &'a [CompletionCandidate],
    text: &str,
) -> Option<&'a CompletionCandidate> {
    let (name, rest, _) = parse_slash_name(text)?;
    if !rest.trim().is_empty() {
        return None;
    }
    find_command(catalog, name)
}
