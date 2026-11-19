//! Bounds and validation for what a native pill view (a framework panel linked into the same
//! process, reached through a C ABI the host declares) hands back. Platform-free: the host owns
//! the `extern "C"` symbols, their names, the wire encoding of its events (which numbers mean
//! which event, how a state it does not draw is shown) and the unsafe pointer handling; this
//! module owns the generic checks that are easy to get subtly wrong: cutting text at a character
//! boundary, a log-safe filter, a name and an id alphabet, and mapping an integer code to a fixed
//! table.
//!
//! # Who may cause an action
//!
//! A message from the native view can start, stop or cancel dictation, open a window or copy
//! text, so the question is how the sender is known. In this protocol it is known by *possession
//! of a function pointer*: the host registers a table of callbacks with the native view at
//! startup, and the native view can only call what the table holds. No action is chosen by a
//! notification name, a URL, a pipe or any other name another process could also use. The native
//! view is code linked into the host process, so the trust boundary is the process; anything
//! that can call these pointers can already run arbitrary code in the host.
//!
//! What a host must still do with what arrives: treat every string and integer as untrusted
//! input. [`bounded_text`], [`log_text`], [`valid_event_name`] and [`valid_token`] bound and
//! filter strings, [`choice_or`] and
//! [`choice`] map an integer code to one of a fixed set of host values (an unknown code never
//! becomes a route, a path or a command), and the action itself stays the host's own call. A
//! sender that must be authenticated *across* processes needs the host's signed-helper check,
//! which this module does not provide and the host must not route through it.

/// Truncate `raw` to at most `max_bytes` bytes without splitting a character, and drop NUL
/// characters (a C string cannot carry them). This is for text that is shown (a capsule label);
/// it keeps every other character, control characters included. Text that is logged goes through
/// [`log_text`].
pub fn bounded_text(raw: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(raw.len().min(max_bytes));
    for ch in raw.chars() {
        if ch == '\0' {
            continue;
        }
        if out.len() + ch.len_utf8() > max_bytes {
            break;
        }
        out.push(ch);
    }
    out
}

/// A message from the native view, made safe to write to a log line: at most `max_bytes` bytes of
/// ASCII letters, digits, space and `_ - . = : , ;`; every other character (control characters,
/// line breaks, path separators, quotes, all non-ASCII text) becomes `?`. A native view can
/// therefore neither inject a line break nor carry a path or a sentence of user text through a
/// diagnostic; what survives is `key=value` shaped codes. The native view still owns what it puts
/// in them.
pub fn log_text(raw: &str, max_bytes: usize) -> String {
    raw.chars()
        .take(max_bytes)
        .map(|ch| {
            if ch.is_ascii_alphanumeric()
                || matches!(ch, ' ' | '_' | '-' | '.' | '=' | ':' | ',' | ';')
            {
                ch
            } else {
                '?'
            }
        })
        .collect()
}

/// A telemetry or surface name from the native view: ASCII letters, digits, `_`, `-` and `.`,
/// non-empty, at most `max_len` bytes. Anything else is refused (`None`), so a native view cannot
/// forge arbitrary event names or inject line breaks into a log.
pub fn valid_event_name(name: &str, max_len: usize) -> Option<&str> {
    let ok = !name.is_empty()
        && name.len() <= max_len
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    ok.then_some(name)
}

/// An opaque identifier from the native view (a record id): non-empty, at most `max_len` bytes and
/// no control characters. The host looks it up in its own store; an id it does not know changes
/// nothing.
pub fn valid_token(value: &str, max_len: usize) -> Option<&str> {
    let ok = !value.is_empty() && value.len() <= max_len && !value.chars().any(char::is_control);
    ok.then_some(value)
}

/// The value for an integer `code` from a fixed table, `None` when the code is outside it.
pub fn choice(code: i32, table: &[&'static str]) -> Option<&'static str> {
    usize::try_from(code)
        .ok()
        .and_then(|index| table.get(index).copied())
}

/// [`choice`] with a default for an unknown code.
pub fn choice_or(code: i32, table: &[&'static str], default: &'static str) -> &'static str {
    choice(code, table).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_never_splits_a_character() {
        // Each "é" is two bytes; a 5-byte bound keeps two of them plus nothing.
        assert_eq!(bounded_text("ééé", 5), "éé");
        assert_eq!(bounded_text("abc", 0), "");
        assert_eq!(bounded_text("a\0b", 10), "ab");
    }

    #[test]
    fn event_names_are_a_small_safe_alphabet() {
        assert_eq!(
            valid_event_name("pill_native.copy-1", 32),
            Some("pill_native.copy-1")
        );
        assert_eq!(valid_event_name("", 32), None);
        assert_eq!(valid_event_name("a b", 32), None);
        assert_eq!(valid_event_name("a\nb", 32), None);
        assert_eq!(valid_event_name("abcdef", 5), None);
    }

    #[test]
    fn tokens_are_bounded_and_free_of_control_characters() {
        assert_eq!(
            valid_token("0123456789abcdef", 64),
            Some("0123456789abcdef")
        );
        assert_eq!(valid_token("", 64), None);
        assert_eq!(valid_token("a\nb", 64), None);
        assert_eq!(valid_token("abcd", 3), None);
        assert_eq!(valid_token("ck:rec/1@x", 64), Some("ck:rec/1@x"));
    }

    #[test]
    fn an_unknown_code_never_becomes_a_value() {
        const TABLE: [&str; 2] = ["home", "history"];
        assert_eq!(choice(0, &TABLE), Some("home"));
        assert_eq!(choice(1, &TABLE), Some("history"));
        assert_eq!(choice(2, &TABLE), None);
        assert_eq!(choice(-1, &TABLE), None);
        assert_eq!(choice_or(99, &TABLE, "home"), "home");
    }
}
