use rightkit_chat_bridge::chat::MemberRole;
use rightkit_chat_bridge::chat::brand::{brand, branded, product};
use rightkit_chat_bridge::chat::envelope::plain_label;
use rightkit_chat_bridge::chat::links::Link;
use serde_json::json;

#[test]
fn the_default_brand_is_neutral_for_display_and_keeps_wire_ids() {
    let brand = brand();
    assert_eq!(brand.product, "RightKit");
    assert_eq!(brand.cli, "rightkit");
    assert_eq!(brand.chat_label(), "RightKit Chat");
    assert_eq!(brand.chat_marker(), "rightkit chat");
    // Identifiers that existing data and installs already use.
    assert_eq!(brand.envelope_domain, "pulse-chat/envelope/v1");
    assert_eq!(brand.skill_name, "pulse-chat");
    assert!(
        brand.skill_markdown.is_empty(),
        "the host supplies the skill"
    );
}

#[test]
fn branded_text_fills_the_markers() {
    assert_eq!(
        branded("run `@cli@ chat peers` in @product@"),
        format!("run `{} chat peers` in {}", brand().cli, product())
    );
}

#[test]
fn a_link_reads_the_old_and_the_new_key_and_writes_the_old_one() {
    let old: Link = serde_json::from_value(json!({"device": "d", "ssh": "h", "pulse": "/x/app"}))
        .expect("old file");
    let new: Link =
        serde_json::from_value(json!({"device": "d", "ssh": "h", "cli": "/x/app"})).expect("new");
    assert_eq!(old.cli, "/x/app");
    assert_eq!(old, new);
    let plain: Link = serde_json::from_value(json!({"device": "d", "ssh": "h"})).expect("plain");
    assert_eq!(plain.cli, brand().cli, "no command means the one on PATH");
    assert_eq!(plain.shell, "posix");
    let written = serde_json::to_value(&old).expect("write");
    assert_eq!(written["pulse"], "/x/app");
    assert!(written.get("cli").is_none());
}

#[test]
fn labels_cannot_carry_quotes_or_markup() {
    assert_eq!(plain_label("a \"b\"  <c>/d", 20), "a b c d");
    assert_eq!(plain_label("abcdefghij", 4), "abcd");
}

#[test]
fn an_unclassified_member_needs_the_phone() {
    assert!(MemberRole::default().needs_phone());
    assert!(!MemberRole::Computer.needs_phone());
}
