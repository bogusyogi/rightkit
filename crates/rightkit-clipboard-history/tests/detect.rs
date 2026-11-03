use rightkit_clipboard_history::{
    Detected, detect,
    detect::{colour, is_secret, link},
};

#[test]
fn classifies_colours_links_and_text() {
    assert_eq!(
        detect("#ff8800"),
        Detected::Colour {
            hex: "#FF8800".to_string()
        }
    );
    assert!(matches!(
        detect("https://www.example.com/a"),
        Detected::Link { .. }
    ));
    assert_eq!(detect("hello there"), Detected::Text);
    assert!(colour("not a colour").is_none());
    assert!(link("plain words").is_none());
}

#[test]
fn recognises_a_token_shaped_secret_only_as_the_whole_text() {
    let key = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
    assert!(is_secret(&key));
    assert!(!is_secret(&format!("my key is {key} ok")));
    assert!(!is_secret("   "));
}
