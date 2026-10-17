use rightkit_tauri_shell::tray::{TrayOptions, ID_QUIT, ID_SHOW};
use rightkit_tauri_shell::{MenuEntry, TraySpec};

#[test]
fn custom_only_is_exact_even_for_shell_action_names() {
    let spec = TraySpec {
        items: vec![
            MenuEntry::item(ID_SHOW, "Own action"),
            MenuEntry::Separator,
            MenuEntry::item("stop", "Stop demo"),
        ],
        ..Default::default()
    };
    let options = TrayOptions::custom_only("cutright-demo");
    assert_eq!(options.menu_items(&spec).unwrap(), spec.items);
    assert!(options.menu_items(&TraySpec::default()).unwrap().is_empty());
}

#[test]
fn default_options_keep_show_custom_quit_order_and_labels() {
    let spec = TraySpec {
        show_label: "Open".into(),
        quit_label: "Exit".into(),
        items: vec![MenuEntry::item("stop", "Stop")],
        ..Default::default()
    };
    assert_eq!(
        TrayOptions::default().menu_items(&spec).unwrap(),
        vec![
            MenuEntry::item(ID_SHOW, "Open"),
            MenuEntry::Separator,
            MenuEntry::item("stop", "Stop"),
            MenuEntry::Separator,
            MenuEntry::item(ID_QUIT, "Exit")
        ]
    );
}

#[test]
fn ids_are_validated_and_independent_specs_do_not_share_rows() {
    let mut spec = TraySpec {
        items: vec![MenuEntry::item("stop", "Stop")],
        ..Default::default()
    };
    let first = TrayOptions::custom_only("one");
    let second = TrayOptions::custom_only("two");
    assert_eq!(
        first.menu_items(&spec).unwrap(),
        second.menu_items(&spec).unwrap()
    );
    spec.items.push(MenuEntry::item("stop", "Again"));
    assert!(first.menu_items(&spec).is_err());
    assert!(TrayOptions::custom_only("")
        .menu_items(&TraySpec::default())
        .is_err());
    assert!(TrayOptions::default()
        .menu_items(&TraySpec {
            items: vec![MenuEntry::item(ID_QUIT, "Quit")],
            ..Default::default()
        })
        .is_err());
}
