use super::*;

#[test]
fn diagnostic_redaction_covers_content_and_target_fields_recursively() {
    let v = serde_json::json!({
        "state": "pasted",
        "delivery_id": "d1",
        "transcript": "secret spoken words",
        "message": "requested=hide resolved=hidden visible_before=true",
        "target": {
            "process_name": "PrivateApp",
            "window_title": "Confidential document"
        },
        "nested": {
            "selected_text": "private selection",
            "prompt": "private prompt"
        }
    });
    let r = redact_diagnostic_event(v);
    assert_eq!(r["transcript"], "[redacted:diagnostics]");
    // Operational message payloads survive so local logs stay debuggable.
    assert_eq!(
        r["message"],
        "requested=hide resolved=hidden visible_before=true"
    );
    assert_eq!(r["target"], "[redacted:diagnostics]");
    assert_eq!(r["nested"]["selected_text"], "[redacted:diagnostics]");
    assert_eq!(r["nested"]["prompt"], "[redacted:diagnostics]");
    assert_eq!(r["state"], "pasted");
    assert_eq!(r["delivery_id"], "d1");
}

#[test]
fn diagnostic_redaction_handles_null_sensitive_fields_without_panic() {
    let v = serde_json::json!({
        "state": "error",
        "last_transcript": null
    });
    let r = redact_diagnostic_event(v);
    assert_eq!(r["last_transcript"], "[redacted:diagnostics]");
    assert_eq!(r["state"], "error");
}

#[test]
fn diagnostic_redaction_removes_secret_shaped_values_under_unknown_keys() {
    let v = serde_json::json!({
        "event": "provider_failure",
        "detail": "request failed with gsk_live_1234567890abcdef",
        "context": ["safe status", "Bearer eyJhbGciOiJIUzI1NiJ9.payload.signature"]
    });
    let r = redact_diagnostic_event(v);
    assert_eq!(r["detail"], "[redacted:diagnostics]");
    assert_eq!(r["context"][0], "safe status");
    assert_eq!(r["context"][1], "[redacted:diagnostics]");
    assert_eq!(r["event"], "provider_failure");
}

#[test]
fn diagnostic_message_scrub_keeps_operational_tokens_and_redacts_sensitive_ones() {
    let v = serde_json::json!({
        "event": "pill_compositor_recompose_result",
        "message": "reason=recording_start phase=deferred active=true show_ok=true visible_after=true",
    });
    let r = redact_diagnostic_event(v);
    assert_eq!(
        r["message"],
        "reason=recording_start phase=deferred active=true show_ok=true visible_after=true"
    );

    let v = serde_json::json!({
        "event": "provider_failure",
        "message": "retry=2 api_key=sk-live-123456789012 transcript=private Bearer eyJhbGciOiJIUzI1NiJ9.p.s done",
    });
    let r = redact_diagnostic_event(v);
    let message = r["message"].as_str().unwrap();
    assert!(message.contains("retry=2"));
    assert!(message.contains("done"));
    assert!(!message.contains("sk-live-123456789012"));
    assert!(!message.contains("transcript=private"));
    assert!(!message.contains("eyJhbGciOiJIUzI1NiJ9"));
}

#[test]
fn payload_text_keeps_quoted_transcript_readable_and_redacts_the_key() {
    let out = redact_payload_text("said \"hello world\" sk-abcdefghijklmnop1234", 1000);
    assert!(out.contains("hello world"), "{out}");
    assert!(!out.contains("sk-abcdefghijklmnop1234"), "{out}");
}

#[test]
fn payload_floor_prefixes_the_owner_lacks_stay_redacted() {
    for secret in [
        "xoxa-1234567890abcdef",
        "xoxr-1234567890abcdef",
        "xoxs-1234567890abcdef",
    ] {
        let out = redact_payload_text(&format!("token is {secret} ok"), 1000);
        assert!(!out.contains(secret), "{out}");
        assert!(out.contains("[redacted]"), "{out}");
    }
}

#[test]
fn payload_prefixes_match_case_insensitively() {
    for secret in [
        "AIZASyA1234567890abcdefghijk",
        "aizaSyA1234567890abcdefghijk",
        "GHP_abcdefghijklmnop1234567890",
        "Sk-abcdefghijklmnop1234",
        "GITHUB_PAT_abcdefghijklmnop1234",
    ] {
        let out = redact_payload_text(&format!("value {secret} end"), 1000);
        assert!(!out.contains(secret), "leaked {secret}: {out}");
        assert!(out.contains("[redacted]"), "{out}");
    }
}

#[test]
fn diagnostic_message_redacts_colon_labelled_secrets() {
    for (label, secret) in [
        ("key", "sk-abcdefghijklmnop1234"),
        ("token", concat!("gh", "p_abcdefghijklmnop1234567890")),
        ("id", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig12345678"),
        ("ref", "AIZASyA1234567890abcdefghijk"),
        ("k", concat!("GH", "P_abcdefghijklmnop1234567890")),
    ] {
        let r = redact_diagnostic_event(serde_json::json!({
            "event": "provider_failure",
            "message": format!("call failed {label}:{secret} retry"),
        }));
        let message = r["message"].as_str().unwrap();
        assert!(!message.contains(secret), "leaked {secret}: {message}");
        assert!(message.contains("retry"), "{message}");
    }
}

#[test]
fn diagnostic_message_masks_the_user_segment_of_a_home_path() {
    let r = redact_diagnostic_event(serde_json::json!({
        "event": "model_load",
        "message": "loaded /Users/ada/models/encoder.onnx elapsed_ms=42",
    }));
    assert_eq!(
        r["message"],
        "loaded /Users/<user>/models/encoder.onnx elapsed_ms=42"
    );
}

#[test]
fn diagnostic_message_keeps_the_app_prefix_floor() {
    let r = redact_diagnostic_event(serde_json::json!({
        "event": "provider_failure",
        "message": "key csk_abcdefghijklmnop used",
    }));
    let message = r["message"].as_str().unwrap();
    assert!(!message.contains("csk_abcdefghijklmnop"), "{message}");
    assert!(message.contains("used"));
}

#[test]
fn payload_redaction_covers_bearer_jwt_aiza_and_common_secret_values() {
    let value = redact_payload_value(
        serde_json::json!({
            "transcript": "Keep this dictated sentence intact.",
            "unknown": "Bearer arbitrary-opaque-secret-value",
            "jwt": "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3OCJ9.signature-value-123456",
            "google": "AIzaSyDUMMY123456789012345678901234",
            "provider": "gsk_common-api-key-value-123456",
            "pin": "4821",
            "token_count": 37,
            "secret_message": "dictated secret recipe",
            "path": "/Users/adrian/support.json",
        }),
        16 * 1024,
    );
    assert_eq!(value["transcript"], "Keep this dictated sentence intact.");
    for key in ["unknown", "jwt", "google", "provider"] {
        assert!(value[key].as_str().unwrap().contains("[redacted]"));
    }
    assert_eq!(value["pin"], "4821");
    assert_eq!(value["token_count"], 37);
    assert_eq!(value["secret_message"], "dictated secret recipe");
    assert_eq!(value["path"], "/Users/adrian/support.json");
}
