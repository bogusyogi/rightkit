//! `bypassDoNotDisturb` on `rk_shell_notify`. The suppression decision is pure so no native
//! notification is ever posted from a test.

use rightkit_tauri_shell::notify::{suppression, NotificationRequest};

fn req(json: serde_json::Value) -> NotificationRequest {
    serde_json::from_value(json).unwrap()
}

#[test]
fn bypass_defaults_to_false_and_dnd_still_suppresses() {
    let r = req(serde_json::json!({"title": "t"}));
    assert!(!r.bypass_do_not_disturb);
    assert_eq!(suppression(&r, true), Some("dnd"));
    assert_eq!(suppression(&r, false), None);
}

#[test]
fn qa_background_selector_is_compiled_out_of_product_releases() {
    // A test binary runs with debug assertions, so the release shape is pinned at the source:
    // one debug-only env read in lifecycle, a constant `false` without debug assertions, and
    // no notification code reading the selector itself.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lifecycle = std::fs::read_to_string(root.join("src/lifecycle.rs")).unwrap();
    assert!(lifecycle
        .contains("#[cfg(not(debug_assertions))]\npub fn qa_background() -> bool {\n    false\n}"));
    assert!(lifecycle
        .contains("#[cfg(not(debug_assertions))]\npub fn qa_hidden() -> bool {\n    false\n}"));
    let notify = std::fs::read_to_string(root.join("src/notify.rs")).unwrap();
    assert!(!notify.contains("RIGHTKIT_QA_BACKGROUND"));
    assert!(notify.contains("crate::lifecycle::qa_background()"));
}

#[test]
fn bypass_skips_only_the_dnd_check() {
    let r = req(serde_json::json!({"title": "t", "bypassDoNotDisturb": true}));
    assert!(r.bypass_do_not_disturb);
    assert_eq!(suppression(&r, true), None);
    let blank = req(serde_json::json!({"title": "  ", "bypassDoNotDisturb": true}));
    assert_eq!(suppression(&blank, true), Some("empty-title"));
}
