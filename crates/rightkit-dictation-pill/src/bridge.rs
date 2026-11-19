//! Bounds and validation for what a native pill view (a framework panel linked into the same
//! process, reached through a C ABI the host declares) hands back. Platform-free: the host owns
//! the `extern "C"` symbols, their names, the callback table's layout and the wire encoding of its
//! events (which numbers mean which event, how a state it does not draw is shown); this module
//! owns the generic mechanism that is easy to get subtly wrong: bounded reads of a C string
//! ([`string_from_c`], [`prefix_from_c`]), building one for the view ([`c_string`]), the panic
//! guard for a callback ([`guarded`]), cutting text at a character boundary, a log-safe filter, a
//! name and an id alphabet, and mapping an integer code to a fixed table.
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

use std::ffi::{c_char, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// A label for a native view: at most `max_bytes` bytes, cut at a character boundary, a NUL replaced
/// by U+FFFD (a C string cannot carry it, and the surrounding text stays readable). Unlike
/// [`bounded_text`] the NUL is replaced, not dropped, and the replacement counts against the
/// bound. Reads no more than the bytes it keeps, so an unbounded label costs no more than a short
/// one.
pub fn bounded_label(raw: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(raw.len().min(max_bytes));
    for ch in raw.chars() {
        let ch = if ch == '\0' { '\u{FFFD}' } else { ch };
        if out.len() + ch.len_utf8() > max_bytes {
            break;
        }
        out.push(ch);
    }
    out
}

/// A C string for a native view. A NUL cannot be represented, so it becomes U+FFFD (the
/// surrounding text stays readable); the conversion never panics.
pub fn c_string(value: &str) -> CString {
    CString::new(value).unwrap_or_else(|_| {
        CString::new(
            value
                .chars()
                .map(|ch| if ch == '\0' { '\u{FFFD}' } else { ch })
                .collect::<String>(),
        )
        .unwrap_or_default()
    })
}

/// Copy a NUL-terminated string a native view passed, at most `max_bytes` bytes long. `None` when it
/// is longer: nothing past `max_bytes + 1` bytes is ever read, so a huge string or a missing
/// terminator can neither make this scan nor allocate without limit, and the size check happens
/// before the copy. A null pointer is the empty string. Invalid UTF-8 becomes U+FFFD.
///
/// # Safety
/// `value` is null or points at a NUL-terminated string that stays valid for the whole call (what
/// a native view guarantees for the duration of a callback).
pub unsafe fn string_from_c(value: *const c_char, max_bytes: usize) -> Option<String> {
    if value.is_null() {
        return Some(String::new());
    }
    let mut len = 0usize;
    while len <= max_bytes {
        // SAFETY: the string is NUL-terminated, and the scan stops at the terminator or after
        // `max_bytes + 1` bytes, which exist whenever the string is longer than `max_bytes`.
        if unsafe { *value.add(len) } == 0 {
            break;
        }
        len += 1;
    }
    if len > max_bytes {
        return None;
    }
    // SAFETY: `len` bytes before the terminator were just read.
    let bytes = unsafe { std::slice::from_raw_parts(value.cast::<u8>(), len) };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// As [`string_from_c`], but a longer string is cut at `max_bytes` instead of refused (for text
/// that is only logged).
///
/// # Safety
/// Same as [`string_from_c`].
pub unsafe fn prefix_from_c(value: *const c_char, max_bytes: usize) -> String {
    if value.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while len < max_bytes {
        // SAFETY: as in `string_from_c`; the scan never passes the terminator or `max_bytes`.
        if unsafe { *value.add(len) } == 0 {
            break;
        }
        len += 1;
    }
    // SAFETY: `len` bytes before the terminator (or the bound) were just read.
    let bytes = unsafe { std::slice::from_raw_parts(value.cast::<u8>(), len) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// Run a native-view callback body and return `default` if it panics: a panic must not unwind
/// across the C boundary. `on_panic` is the host's report (a fixed tag, never the panic's text); it
/// runs under its own `catch_unwind`, so a panic in it cannot cross the boundary either.
pub fn guarded<R>(default: R, on_panic: impl FnOnce(), body: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(_) => {
            let _ = catch_unwind(AssertUnwindSafe(on_panic));
            default
        }
    }
}

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
