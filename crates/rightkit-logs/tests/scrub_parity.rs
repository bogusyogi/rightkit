//! Parity with the CodeRight support-bundle redactor
//! (`engine/crates/support_bundle/src/lib.rs`, `redact_text`): every
//! free-text pattern it applies has a positive and a no-false-positive case
//! here. Owner gap M15 O5.
//!
//! Expected strings name the placeholders `scrub.rs` emits. Patterns that
//! CodeRight covers but `scrub_text` deliberately does not (absolute paths
//! outside a user directory) are pinned as shape-preserving.

use rightkit_logs::scrub::{scrub_text, scrub_text_with, ScrubOptions};

// Bearer: CodeRight drops the token after a `Bearer` marker.

#[test]
fn bearer_token_after_marker_is_dropped() {
    assert_eq!(
        scrub_text("Authorization: Bearer abc123SECRETtoken failed"),
        "Authorization: Bearer [redacted:credential] failed"
    );
}

#[test]
fn bearer_prose_and_marker_without_token_are_kept() {
    let prose = "bearers and overbearing tones; the bearer";
    assert_eq!(scrub_text(prose), prose);
}

// Telegram bot token: `<numeric id>:<secret>`.

#[test]
fn telegram_bot_token_is_redacted() {
    assert_eq!(
        scrub_text("bot 123456789:AAFabcdefGHIJKLMNOPQRSTUV123 failed"),
        "bot [redacted:credential] failed"
    );
}

#[test]
fn telegram_shape_without_long_secret_or_long_id_is_kept() {
    let text = "step 12:AAFabcdefGHIJKLMNOPQRSTUV123 and 123456789:short";
    assert_eq!(scrub_text(text), text);
}

// API keys: CodeRight's `csk-` and `or-` prefixes.

#[test]
fn csk_api_key_is_redacted() {
    assert_eq!(
        scrub_text("key csk-abcdef1234567890 used"),
        "key [redacted:credential] used"
    );
}

#[test]
fn csk_short_word_is_kept() {
    let text = "the csk-short flag";
    assert_eq!(scrub_text(text), text);
}

#[test]
fn or_api_key_is_redacted() {
    assert_eq!(
        scrub_text("token or-7f3a9c2e1b4d5a6f7e8d9c0b1a2f3e4d done"),
        "token [redacted:credential] done"
    );
}

#[test]
fn or_hyphenated_words_without_digits_are_kept() {
    let text = "or-ordinary-words-without-digits";
    assert_eq!(scrub_text(text), text);
}

// Email: CodeRight redacts any local@domain.tld token.

#[test]
fn email_address_is_redacted() {
    assert_eq!(
        scrub_text("contact person@example.com now"),
        "contact [redacted:email] now"
    );
}

#[test]
fn email_shape_without_dotted_domain_is_kept() {
    let text = "npm i @scope/pkg and git@host";
    assert_eq!(scrub_text(text), text);
}

// Paths: CodeRight redacts absolute paths whole. `scrub_text` masks only the
// user segment of a home directory and keeps the rest of the shape, so
// reports stay diagnosable (scrub.rs module contract). This test pins that
// divergence.

#[test]
fn user_home_segment_is_masked() {
    assert_eq!(
        scrub_text("crash at /Users/ada/app/src/main.rs:4"),
        "crash at /Users/<user>/app/src/main.rs:4"
    );
}

#[test]
fn non_user_absolute_path_keeps_shape() {
    let text = "open /Volumes/D/claude now";
    assert_eq!(scrub_text(text), text);
}

// Opt-in full-path redaction (`ScrubOptions::redact_paths`), CodeRight parity:
// every absolute path token becomes `[path]`. The default keeps path shape.

fn redact_paths() -> ScrubOptions {
    ScrubOptions { redact_paths: true }
}

#[test]
fn absolute_path_tokens_are_redacted_whole() {
    assert_eq!(
        scrub_text_with(
            "open /Volumes/D/claude/app.rs:4 and C:\\Work\\x.txt and \\\\server\\share\\f.bin and ~/notes.txt",
            &redact_paths()
        ),
        "open [path] and [path] and [path] and [path]"
    );
}

#[test]
fn quoted_or_bracketed_path_is_redacted() {
    assert_eq!(
        scrub_text_with("failed (\"/var/log/app.log\")", &redact_paths()),
        "failed (\"[path]\")"
    );
}

#[test]
fn urls_ratios_and_comment_markers_are_kept_with_path_redaction() {
    let text = "see https://x/y and ratio 1/2 and //comment here";
    assert_eq!(scrub_text_with(text, &redact_paths()), text);
}

#[test]
fn default_options_keep_path_shape() {
    assert_eq!(
        scrub_text_with("open /Volumes/D/claude now", &ScrubOptions::default()),
        "open /Volumes/D/claude now"
    );
    assert_eq!(
        scrub_text_with(
            "crash at /Users/ada/app/src/main.rs:4",
            &ScrubOptions::default()
        ),
        scrub_text("crash at /Users/ada/app/src/main.rs:4")
    );
}
