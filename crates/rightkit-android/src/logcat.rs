//! A debug-only logcat sink. Privacy-safe JSONL (`rightkit-logs`) stays the record; this exists so
//! a developer sees Rust messages in `adb logcat`. It is off unless `RightKit.init(debugLogging =
//! true)`, and the caller is responsible for never passing user text, transcripts or secrets.

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};

static DEBUG_LOGGING: AtomicBool = AtomicBool::new(false);

/// logcat tags are limited to 23 characters on older Android releases.
pub const MAX_TAG_CHARS: usize = 23;
/// logd truncates a single entry near 4 KiB; leave headroom.
pub const MAX_MESSAGE_BYTES: usize = 4000;

/// Log level, mapped to `android_LogPriority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LogLevel {
    Verbose,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// `ANDROID_LOG_*` value.
    pub fn priority(self) -> i32 {
        match self {
            LogLevel::Verbose => 2,
            LogLevel::Debug => 3,
            LogLevel::Info => 4,
            LogLevel::Warn => 5,
            LogLevel::Error => 6,
        }
    }
}

pub(crate) fn set_debug_logging(on: bool) {
    DEBUG_LOGGING.store(on, Ordering::Relaxed);
}

pub fn debug_logging_enabled() -> bool {
    DEBUG_LOGGING.load(Ordering::Relaxed)
}

fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn truncate_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn no_nul(s: &str) -> String {
    s.chars().filter(|c| *c != '\0').collect()
}

/// Builds the arguments of one logcat write, or `None` when debug logging is off.
pub fn prepare(
    debug: bool,
    level: LogLevel,
    tag: &str,
    message: &str,
) -> Option<(i32, CString, CString)> {
    if !debug {
        return None;
    }
    let tag = no_nul(truncate_chars(tag, MAX_TAG_CHARS));
    let tag = if tag.is_empty() {
        "RightKit".to_string()
    } else {
        tag
    };
    let message = no_nul(truncate_bytes(message, MAX_MESSAGE_BYTES));
    Some((
        level.priority(),
        CString::new(tag).ok()?,
        CString::new(message).ok()?,
    ))
}

#[cfg(target_os = "android")]
mod sys {
    use std::os::raw::{c_char, c_int};

    #[link(name = "log")]
    extern "C" {
        pub fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }
}

/// Writes to logcat when debug logging is on. A no-op off Android.
pub fn write(level: LogLevel, tag: &str, message: &str) {
    let Some((_prio, _tag, _msg)) = prepare(debug_logging_enabled(), level, tag, message) else {
        return;
    };
    #[cfg(target_os = "android")]
    // SAFETY: both pointers come from live NUL-terminated `CString`s that outlive the call.
    unsafe {
        sys::__android_log_write(_prio, _tag.as_ptr(), _msg.as_ptr());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_written_when_debug_logging_is_off() {
        assert!(prepare(false, LogLevel::Error, "t", "m").is_none());
    }

    #[test]
    fn priorities_match_android_log_priority() {
        assert_eq!(LogLevel::Verbose.priority(), 2);
        assert_eq!(LogLevel::Debug.priority(), 3);
        assert_eq!(LogLevel::Info.priority(), 4);
        assert_eq!(LogLevel::Warn.priority(), 5);
        assert_eq!(LogLevel::Error.priority(), 6);
    }

    #[test]
    fn long_tags_are_cut_on_a_character_boundary() {
        let tag = "é".repeat(40);
        let (_, t, _) = prepare(true, LogLevel::Info, &tag, "m").unwrap();
        assert_eq!(t.to_str().unwrap().chars().count(), MAX_TAG_CHARS);
    }

    #[test]
    fn an_empty_tag_gets_a_default() {
        let (_, t, _) = prepare(true, LogLevel::Info, "", "m").unwrap();
        assert_eq!(t.to_str().unwrap(), "RightKit");
    }

    #[test]
    fn nul_bytes_are_removed_rather_than_failing() {
        let (_, t, m) = prepare(true, LogLevel::Info, "a\0b", "x\0y").unwrap();
        assert_eq!(t.to_str().unwrap(), "ab");
        assert_eq!(m.to_str().unwrap(), "xy");
    }

    #[test]
    fn long_messages_are_cut_on_a_character_boundary() {
        let msg = "é".repeat(MAX_MESSAGE_BYTES);
        let (_, _, m) = prepare(true, LogLevel::Info, "t", &msg).unwrap();
        assert!(m.as_bytes().len() <= MAX_MESSAGE_BYTES);
        assert!(m.to_str().is_ok());
    }

    #[test]
    fn the_flag_round_trips() {
        set_debug_logging(true);
        assert!(debug_logging_enabled());
        set_debug_logging(false);
        assert!(!debug_logging_enabled());
    }
}
