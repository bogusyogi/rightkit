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
fn bypass_skips_only_the_dnd_check() {
    let r = req(serde_json::json!({"title": "t", "bypassDoNotDisturb": true}));
    assert!(r.bypass_do_not_disturb);
    assert_eq!(suppression(&r, true), None);
    let blank = req(serde_json::json!({"title": "  ", "bypassDoNotDisturb": true}));
    assert_eq!(suppression(&blank, true), Some("empty-title"));
}
