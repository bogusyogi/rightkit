//! Public known-value scrub: `KnownSecrets` removes the exact secrets an app holds, in the forms
//! an error path leaves them in, before text is logged or returned.

use rightkit_logs::{scrub_text, KnownSecrets, KNOWN_SECRET_MARKER};

#[test]
fn an_error_string_naming_a_secret_loses_it() {
    let secrets = KnownSecrets::from_values(["sk_live_Zx81Qq", "db-key-77"]);
    let err = "request to https://api.example.test/v1?key=sk_live_Zx81Qq failed: bad db-key-77";
    let out = secrets.redact(err);
    assert!(!secrets.is_exposed(&out));
    assert_eq!(out.matches(KNOWN_SECRET_MARKER).count(), 2);
}

#[test]
fn the_pattern_scrub_alone_misses_a_custom_format_and_the_known_scrub_does_not() {
    let secret = "zq-9981-custom-format";
    let text = format!("token rejected: {secret}");
    assert!(scrub_text(&text).contains(secret));
    assert!(!KnownSecrets::from_values([secret])
        .scrub_text(&text)
        .contains(secret));
}

#[test]
fn encoded_forms_in_urls_and_json_are_removed() {
    let secret = "a b&c=d/\u{e9}";
    let secrets = KnownSecrets::from_values([secret]);
    let url = "GET /x?q=a%20b%26c%3Dd%2F%C3%A9&other=1";
    assert!(!secrets.is_exposed(&secrets.redact(url)));
    assert_eq!(secrets.redact(url), "GET /x?q=[redacted]&other=1");

    let json = serde_json::json!({ "detail": format!("bad {secret}") }).to_string();
    assert!(!secrets.is_exposed(&secrets.redact(&json)));
}

#[test]
fn json_values_are_redacted_in_place_including_keys() {
    let secrets = KnownSecrets::from_values(["hunter2"]);
    let mut value = serde_json::json!({
        "error": "login hunter2 failed",
        "hunter2": ["x", {"deep": "hunter2"}],
        "count": 3,
    });
    secrets.scrub_value(&mut value);
    let text = value.to_string();
    assert!(!text.contains("hunter2"), "{text}");
    assert_eq!(value["count"], 3);
}

#[test]
fn an_empty_set_is_a_no_op_and_debug_hides_values() {
    let none = KnownSecrets::new();
    assert_eq!(none.redact("unchanged"), "unchanged");
    let some = KnownSecrets::from_values(["very-secret"]);
    assert_eq!(format!("{some:?}"), "KnownSecrets(1 values)");
}
