//! The whisper.cpp language table (v1.9.3, `whisper_lang_str(0..=99)`): 100
//! codes, `yue` (Cantonese) included. Order is whisper.cpp's language id order.
//! Verified against the pinned library by the gated `whisper_language_table_matches_library`
//! test in `tests/e2e_whisper.rs`.

use crate::error::{Error, Result};

/// Every language code the pinned whisper.cpp build accepts for a fixed language.
pub const WHISPER_LANGUAGES: [&str; 100] = [
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln", "ha",
    "ba", "jw", "su", "yue",
];

/// Whether `code` (case-insensitive, surrounding whitespace ignored) is in [`WHISPER_LANGUAGES`].
pub fn is_whisper_language(code: &str) -> bool {
    let c = code.trim();
    WHISPER_LANGUAGES.iter().any(|l| l.eq_ignore_ascii_case(c))
}

/// Normalize a language code to its table spelling, or reject it. Never guesses:
/// `"english"` and `"auto"` are not codes here (use [`super::Language::Auto`]).
pub fn validate_language(code: &str) -> Result<&'static str> {
    let c = code.trim();
    WHISPER_LANGUAGES
        .iter()
        .copied()
        .find(|l| l.eq_ignore_ascii_case(c))
        .ok_or_else(|| Error::Config(format!("unknown Whisper language code `{code}`")))
}

/// Whisper language codes whose scripts are written without spaces between
/// words, so segment texts are concatenated with nothing inserted. The one place
/// this table lives; extend it here and [`join_segment_text`] follows.
///
/// Japanese, Chinese (`zh`), Cantonese (`yue`), Thai, Lao and Khmer. Every other
/// code (including ones not in this table) keeps the single-space join.
pub const UNSPACED_LANGUAGES: [&str; 6] = ["ja", "zh", "yue", "th", "lo", "km"];

/// Whether `code` (case-insensitive, whitespace ignored) is written without
/// inter-word spaces (see [`UNSPACED_LANGUAGES`]).
pub fn is_unspaced_language(code: &str) -> bool {
    let c = code.trim();
    UNSPACED_LANGUAGES.iter().any(|l| l.eq_ignore_ascii_case(c))
}

/// Join already-trimmed segment texts into a transcript: one space between
/// segments for space-separated languages (the 0.2.0 behaviour), nothing
/// inserted for [`UNSPACED_LANGUAGES`].
pub fn join_segment_text<'a, I>(parts: I, language: &str) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let sep = if is_unspaced_language(language) { "" } else { " " };
    parts.into_iter().collect::<Vec<_>>().join(sep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_100_unique_lowercase_codes_including_yue() {
        assert_eq!(WHISPER_LANGUAGES.len(), 100);
        let mut sorted = WHISPER_LANGUAGES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 100, "codes must be unique");
        assert!(WHISPER_LANGUAGES.iter().all(|c| c.chars().all(|ch| ch.is_ascii_lowercase())));
        assert!(is_whisper_language("yue"));
        assert_eq!(WHISPER_LANGUAGES[0], "en");
    }

    #[test]
    fn validation_normalizes_or_rejects() {
        assert_eq!(validate_language(" EN ").unwrap(), "en");
        assert_eq!(validate_language("Yue").unwrap(), "yue");
        assert!(validate_language("english").is_err());
        assert!(validate_language("auto").is_err());
        assert!(validate_language("").is_err());
    }

    #[test]
    fn unspaced_table_is_valid_codes_and_matches_the_documented_set() {
        assert_eq!(UNSPACED_LANGUAGES, ["ja", "zh", "yue", "th", "lo", "km"]);
        assert!(UNSPACED_LANGUAGES.iter().all(|c| is_whisper_language(c)), "every entry is a real Whisper code");
        assert!(is_unspaced_language(" JA "));
        assert!(is_unspaced_language("yue"));
        assert!(!is_unspaced_language("en"));
        assert!(!is_unspaced_language(""));
    }

    #[test]
    fn join_inserts_a_space_only_for_spaced_languages() {
        let parts = ["Hello there.", "How are you?"];
        assert_eq!(join_segment_text(parts, "en"), "Hello there. How are you?");
        assert_eq!(join_segment_text(parts, "fr"), "Hello there. How are you?");
        assert_eq!(join_segment_text(["こんにちは", "元気ですか"], "ja"), "こんにちは元気ですか");
        assert_eq!(join_segment_text(["你好", "世界"], "ZH"), "你好世界");
        for code in UNSPACED_LANGUAGES {
            assert_eq!(join_segment_text(["a", "b"], code), "ab", "{code}");
        }
        // Unknown or empty language (e.g. a partial with nothing decoded) keeps the spaced join.
        assert_eq!(join_segment_text(["a", "b"], ""), "a b");
        assert_eq!(join_segment_text(["a", "b"], "xx"), "a b");
        assert_eq!(join_segment_text([], "en"), "");
        assert_eq!(join_segment_text(["only"], "ja"), "only");
    }
}
