//! Menu entries (Orthic-Labs/rightkit#386): check items, nested submenus, id validation, role
//! overrides and the runtime-state checks, all without a native menu.

use rightkit_tauri_shell::menu::{
    check_state, entry_ids, override_entry, role_defaults, validate, validate_entries, MenuEntry,
    MenuGroup, MenuItemState, MenuOverride, MenuSpec, OverrideRole, ID_QUIT, MAX_SUBMENU_DEPTH,
};
use rightkit_tauri_shell::tray::{TrayOptions, ID_SHOW};
use rightkit_tauri_shell::TraySpec;

fn spec(entries: Vec<MenuEntry>) -> MenuSpec {
    MenuSpec {
        app_name: "App".into(),
        groups: vec![MenuGroup {
            label: "View".into(),
            entries,
        }],
        ..Default::default()
    }
}

/// `levels` submenus wrapped around one leaf item.
fn nested(levels: usize) -> Vec<MenuEntry> {
    let mut e = vec![MenuEntry::item("leaf", "Leaf")];
    for i in 0..levels {
        e = vec![MenuEntry::submenu(&format!("s{i}"), e)];
    }
    e
}

#[test]
fn check_and_submenu_round_trip_through_json() {
    let entries: Vec<MenuEntry> = serde_json::from_str(
        r#"[{"kind":"check","id":"v.wrap","label":"Word Wrap","checked":true},
            {"kind":"submenu","label":"Zoom","entries":[{"kind":"item","id":"z.in","label":"In"},{"kind":"separator"},{"kind":"check","id":"z.fit","label":"Fit"}]}]"#,
    )
    .unwrap();
    assert_eq!(entries[0], MenuEntry::check("v.wrap", "Word Wrap", true));
    assert!(matches!(
        &entries[1],
        MenuEntry::Submenu { label, entries, .. } if label == "Zoom" && entries.len() == 3
    ));
    let again: Vec<MenuEntry> =
        serde_json::from_str(&serde_json::to_string(&entries).unwrap()).unwrap();
    assert_eq!(again, entries);
    // Defaults: enabled, unchecked, no accelerator; a submenu may be empty.
    let c: MenuEntry = serde_json::from_str(r#"{"kind":"check","id":"a","label":"A"}"#).unwrap();
    assert_eq!(c, MenuEntry::check("a", "A", false));
    let sub: MenuEntry = serde_json::from_str(r#"{"kind":"submenu","label":"Empty"}"#).unwrap();
    assert_eq!(sub, MenuEntry::submenu("Empty", Vec::new()));
}

#[test]
fn constructors_adjust_items_and_checks_and_leave_other_rows_alone() {
    let c = MenuEntry::check("a", "A", true)
        .with_accelerator("CmdOrCtrl+K")
        .disabled();
    let parsed: MenuEntry = serde_json::from_str(
        r#"{"kind":"check","id":"a","label":"A","accelerator":"CmdOrCtrl+K","enabled":false,"checked":true}"#,
    )
    .unwrap();
    assert_eq!(c, parsed);
    let i = MenuEntry::item("b", "B").with_accelerator("CmdOrCtrl+B");
    assert_eq!(i, MenuEntry::accel("b", "B", "CmdOrCtrl+B"));
    assert_eq!(MenuEntry::Separator.disabled(), MenuEntry::Separator);
    let sub = MenuEntry::submenu("S", vec![]);
    assert_eq!(sub.clone().with_accelerator("X"), sub);
}

#[test]
fn entry_ids_walk_nested_submenus_in_order() {
    let entries = vec![
        MenuEntry::item("a", "A"),
        MenuEntry::submenu(
            "Sub",
            vec![
                MenuEntry::check("b", "B", false),
                MenuEntry::Separator,
                MenuEntry::submenu("Deeper", vec![MenuEntry::item("c", "C")]),
            ],
        ),
        MenuEntry::item("d", "D"),
    ];
    assert_eq!(entry_ids(&entries), ["a", "b", "c", "d"]);
}

#[test]
fn duplicate_and_empty_ids_are_refused_across_groups_help_settings_and_overrides() {
    assert!(validate(&spec(vec![
        MenuEntry::item("a", "A"),
        MenuEntry::check("b", "B", true)
    ]))
    .is_ok());
    let dup = validate(&spec(vec![
        MenuEntry::item("a", "A"),
        MenuEntry::submenu("S", vec![MenuEntry::check("a", "A2", false)]),
    ]));
    assert!(dup.unwrap_err().contains("duplicate menu item id a"));
    assert!(validate(&spec(vec![MenuEntry::item(" ", "x")]))
        .unwrap_err()
        .contains("empty id"));
    assert!(validate(&spec(vec![MenuEntry::check("", "x", false)]))
        .unwrap_err()
        .contains("empty id"));
    let mut s = spec(vec![MenuEntry::item("a", "A")]);
    s.help = vec![MenuEntry::item("a", "Help A")];
    assert!(validate(&s).is_err());
    let mut s = spec(vec![MenuEntry::item("settings", "A")]);
    s.settings = Some(("settings".into(), "CmdOrCtrl+,".into()));
    assert!(validate(&s).is_err());
    let mut s = spec(vec![MenuEntry::item("a", "A")]);
    s.overrides = vec![MenuOverride::new(OverrideRole::Undo, "a")];
    assert!(validate(&s).unwrap_err().contains("duplicate"));
    // The shell's own quit id is not an app id.
    assert!(validate(&spec(vec![MenuEntry::item(ID_QUIT, "Quit")]))
        .unwrap_err()
        .contains("reserved"));
}

#[test]
fn an_override_role_can_be_given_only_once() {
    let mut s = spec(vec![]);
    s.overrides = vec![
        MenuOverride::new(OverrideRole::Quit, "app.quit"),
        MenuOverride::new(OverrideRole::Quit, "app.quit2"),
    ];
    assert!(validate(&s).unwrap_err().contains("given twice"));
    s.overrides = vec![
        MenuOverride::new(OverrideRole::Quit, "app.quit"),
        MenuOverride::new(OverrideRole::CloseWindow, "app.close"),
    ];
    assert!(validate(&s).is_ok());
}

#[test]
fn submenu_nesting_is_bounded() {
    // A group's entries are depth 1; each wrapped Submenu adds one level.
    assert!(
        validate(&spec(nested(MAX_SUBMENU_DEPTH))).is_ok(),
        "{MAX_SUBMENU_DEPTH} levels are allowed"
    );
    let too_deep = nested(MAX_SUBMENU_DEPTH + 1);
    assert!(validate(&spec(too_deep.clone()))
        .unwrap_err()
        .contains("nest deeper"));
    assert!(validate_entries(&too_deep).is_err());
    assert!(validate_entries(&nested(MAX_SUBMENU_DEPTH)).is_ok());
    // Help is bounded too.
    let mut s = spec(vec![]);
    s.help = nested(MAX_SUBMENU_DEPTH + 1);
    assert!(validate(&s).unwrap_err().contains("help menu"));
}

#[test]
fn overrides_replace_native_items_with_app_ids_keeping_native_label_and_accelerator() {
    let mut s = spec(vec![]);
    s.app_name = "Notes".into();
    s.overrides = vec![
        MenuOverride::new(OverrideRole::Quit, "app.quit"),
        MenuOverride::new(OverrideRole::Undo, "app.undo")
            .label("Undo edit")
            .accelerator("CmdOrCtrl+Alt+Z"),
    ];
    // Not overridden: the native item stays.
    assert_eq!(override_entry(&s, OverrideRole::Redo, true), None);
    // Quit keeps the native label and accelerator for the platform.
    assert_eq!(
        override_entry(&s, OverrideRole::Quit, true),
        Some(MenuEntry::accel("app.quit", "Quit Notes", "CmdOrCtrl+Q"))
    );
    assert_eq!(
        override_entry(&s, OverrideRole::Quit, false),
        Some(MenuEntry::item("app.quit", "Exit Notes"))
    );
    // Own label and accelerator win.
    assert_eq!(
        override_entry(&s, OverrideRole::Undo, false),
        Some(MenuEntry::accel("app.undo", "Undo edit", "CmdOrCtrl+Alt+Z"))
    );
    assert_eq!(
        role_defaults(OverrideRole::CloseWindow, "Notes", false),
        ("Close Window".to_owned(), Some("CmdOrCtrl+W"))
    );
    let json = serde_json::to_string(&OverrideRole::CloseWindow).unwrap();
    assert_eq!(json, "\"close-window\"");
}

#[test]
fn state_updates_need_a_known_id_and_checked_needs_a_check_item() {
    let is_item = |id: &str| id == "plain";
    let is_check = |id: &str| id == "tick";
    let up = |id: &str| MenuItemState::new(id);
    assert!(check_state(&up("plain").with_enabled(false), is_item, is_check).is_ok());
    assert!(check_state(
        &up("tick").with_enabled(true).with_checked(true),
        is_item,
        is_check
    )
    .is_ok());
    assert!(
        check_state(&up("missing").with_enabled(true), is_item, is_check)
            .unwrap_err()
            .contains("no menu item missing")
    );
    assert!(
        check_state(&up("plain").with_checked(true), is_item, is_check)
            .unwrap_err()
            .contains("not a check item")
    );
    let parsed: Vec<MenuItemState> =
        serde_json::from_str(r#"[{"id":"tick","checked":true},{"id":"plain","enabled":false}]"#)
            .unwrap();
    assert_eq!(parsed[0], up("tick").with_checked(true));
    assert_eq!(parsed[1], up("plain").with_enabled(false));
}

#[test]
fn tray_menus_accept_check_items_and_submenus_and_validate_nested_ids() {
    let spec = TraySpec {
        items: vec![
            MenuEntry::check("mute", "Mute", true),
            MenuEntry::submenu(
                "Sounds",
                vec![MenuEntry::item("chime", "Chime"), MenuEntry::Separator],
            ),
        ],
        ..Default::default()
    };
    let rows = TrayOptions::default().menu_items(&spec).unwrap();
    assert_eq!(
        rows.len(),
        spec.items.len() + 4,
        "show, sep, items, sep, quit"
    );
    assert_eq!(
        TrayOptions::custom_only("t").menu_items(&spec).unwrap(),
        spec.items
    );

    // A nested duplicate or reserved id is caught like a flat one.
    let dup = TraySpec {
        items: vec![
            MenuEntry::item("chime", "Chime"),
            MenuEntry::submenu("Sounds", vec![MenuEntry::check("chime", "Again", false)]),
        ],
        ..Default::default()
    };
    assert!(TrayOptions::default().menu_items(&dup).is_err());
    let reserved = TraySpec {
        items: vec![MenuEntry::submenu(
            "Sounds",
            vec![MenuEntry::item(ID_SHOW, "Mine")],
        )],
        ..Default::default()
    };
    assert!(TrayOptions::default().menu_items(&reserved).is_err());
    assert!(TrayOptions::custom_only("t").menu_items(&reserved).is_ok());
    // And the depth limit applies to tray menus too.
    let deep = TraySpec {
        items: nested(MAX_SUBMENU_DEPTH + 1),
        ..Default::default()
    };
    assert!(TrayOptions::default().menu_items(&deep).is_err());
}

#[test]
fn the_browser_accelerator_switch_is_off_by_default_and_additive() {
    use rightkit_tauri_shell::Hardening;
    let d = Hardening::default();
    assert!(!d.disable_browser_accelerators);
    // The per-key switches keep their defaults when it is turned on.
    let own_keys = Hardening {
        disable_browser_accelerators: true,
        block_print: false,
        ..Hardening::default()
    };
    assert!(own_keys.disable_browser_accelerators && own_keys.block_reload);
    // It is a native WebView2 setting: the DOM script config does not change.
    assert_eq!(
        Hardening::default().script(),
        Hardening {
            disable_browser_accelerators: true,
            ..Hardening::default()
        }
        .script()
    );
}
