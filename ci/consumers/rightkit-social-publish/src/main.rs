//! Registry consumer for rightkit-social-publish 0.1.0: builds against the published API only.
//! It never sends a request.
use rightkit_social_publish::http::{check_base_url, parse_retry_after, RetryAfter};
use rightkit_social_publish::platforms::x;
use rightkit_social_publish::redact::redact_text;
use rightkit_social_publish::wire::{PublishPayload, PUBLISH_PLATFORMS};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        PUBLISH_PLATFORMS.contains(&"bluesky"),
        "the platform list names bluesky",
    );
    check(
        x::counted_length("hello") == 5,
        "plain text counts one per character on X",
    );
    check(
        parse_retry_after("30") == RetryAfter::Seconds(30),
        "Retry-After seconds parse",
    );
    check(
        check_base_url("https://api.example.com", false).is_err()
            && check_base_url("http://127.0.0.1:4010", false).is_ok(),
        "only loopback base URLs pass without allow_remote",
    );
    check(
        !redact_text(r#"{"access_token": "SECRET"}"#).contains("SECRET"),
        "OAuth values are redacted",
    );
    let payload = PublishPayload {
        caption: "hi".into(),
        format: "text".into(),
        media: Vec::new(),
        settings: serde_json::Value::Null,
    };
    check(
        x::check(&payload).is_ok(),
        "a short text post passes the X checks",
    );
    println!("rightkit-social-publish consumer: all checks passed");
}
