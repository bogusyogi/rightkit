//! Parity with the CodeRight support-bundle redactor
//! (`engine/crates/support_bundle/src/lib.rs`, `redact_text`): every
//! free-text pattern it applies has a positive and a no-false-positive case
//! here. Owner gap M15 O5.
//!
//! Expected strings name the placeholders `scrub.rs` emits. Patterns that
//! CodeRight covers but `scrub_text` deliberately does not (absolute paths
//! outside a user directory) are pinned as shape-preserving.

use rightkit_logs::scrub::{redact_quoted, scrub_text, scrub_text_with, scrub_value, ScrubOptions};

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

// Shared JS/Rust parity vectors: the identical (input, expected) pairs run in
// `packages/logs/src/index.test.ts` against `scrubLogText`, and a subset in
// `crash.test.ts` against `scrubWindowCrashText`.

/// The shared vector file `tests/scrub-parity.v1.json` (shipped with the crate) is the single source the
/// JS suites (`packages/logs/src/index.test.ts`, `crash.test.ts`) also run, so both scrubbers are held
/// to the same expectations (PK11-3).
#[test]
fn shared_vector_file_matches_scrub_text() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scrub-parity.v1.json");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read the shared vectors"))
            .expect("vectors are JSON");
    assert_eq!(doc["schema"], 1);
    let vectors = doc["vectors"].as_array().expect("a vectors array");
    assert!(
        vectors.len() >= 30,
        "the shared file lost entries: {}",
        vectors.len()
    );
    for v in vectors {
        let name = v["name"].as_str().expect("every vector has a name");
        let input: String = match v["input"].as_str() {
            Some(s) => s.to_string(),
            None => v["inputParts"]
                .as_array()
                .expect("input or inputParts")
                .iter()
                .map(|p| p.as_str().expect("inputParts are strings"))
                .collect(),
        };
        assert_eq!(
            scrub_text(&input),
            v["expected"].as_str().expect("expected"),
            "{name}"
        );
    }
}

#[test]
fn js_rust_parity_vectors() {
    let vectors: &[(&str, &str)] = &[
        (
            "Authorization: Bearer abcdefghijklmnop failed",
            "Authorization: Bearer [redacted:credential] failed",
        ),
        (
            "contact person@example.com now",
            "contact [redacted:email] now",
        ),
        (
            "GET https://user:pw@example.com/v1/items?key=abc&x=1#frag failed",
            "GET https://example.com/v1/items?[redacted] failed",
        ),
        (
            "callback code=4/abc&state=xyz failed",
            "callback code=[redacted]&state=[redacted] failed",
        ),
        (
            "token eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJlLXBhcnQ done",
            "token [redacted:credential] done",
        ),
        (
            concat!("key sk-", "a1b2", "c3d4e5f6g7h8 used"),
            "key [redacted:credential] used",
        ),
        (
            "crash at /Users/ada/app/src/main.rs:4",
            "crash at /Users/<user>/app/src/main.rs:4",
        ),
    ];
    for (input, expected) in vectors {
        assert_eq!(scrub_text(input), *expected, "{input}");
    }
}

// URL/OAuth composition lives inside `scrub_text` itself (D16), not only in callers.

#[test]
fn url_and_oauth_secrets_leave_plain_scrub_text() {
    assert_eq!(
        scrub_text("open https://api.example.com/cb?access_token=abc123&next=/home#section now"),
        "open https://api.example.com/cb?[redacted] now"
    );
    assert_eq!(
        scrub_text("refresh_token=rt-value-9, scope=mail"),
        "refresh_token=[redacted], scope=mail"
    );
}

// Quoted/Debug redaction: double-quoted Debug spans are dropped like backticks.

#[test]
fn debug_double_quoted_values_are_redacted() {
    assert_eq!(
        redact_quoted("Err value: Custom(\"send the numbers\") at `x`"),
        "Err value: Custom(\"<redacted:16>\") at `<redacted:1>`"
    );
    assert_eq!(
        redact_quoted("x \"a\\\"b\" y"),
        "x \"<redacted:4>\" y",
        "an escaped quote does not close the span"
    );
    let unmatched = "say \"hi there";
    assert_eq!(redact_quoted(unmatched), unmatched);
}

#[test]
fn scrub_value_composes_quoted_url_and_text_scrubbing() {
    let mut value = serde_json::json!({
        "note": "failed \"secret words\" for ada@example.com via https://h.example/x?code=abc123",
        "count": 3,
    });
    scrub_value(&mut value);
    assert_eq!(
        value["note"],
        "failed \"<redacted:12>\" for [redacted:email] via https://h.example/x?[redacted]"
    );
    assert_eq!(value["count"], 3);
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
