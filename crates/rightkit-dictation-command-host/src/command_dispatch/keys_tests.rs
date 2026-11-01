use super::*;

#[cfg(target_os = "macos")]
#[test]
fn key_keycode_macos_known_letters() {
    assert_eq!(key_keycode_macos("a"), Some(0x00));
    assert_eq!(key_keycode_macos("A"), Some(0x00)); // case-insensitive
    assert_eq!(key_keycode_macos("z"), Some(0x06));
}

#[cfg(target_os = "macos")]
#[test]
fn key_keycode_macos_known_named_keys() {
    assert_eq!(key_keycode_macos("escape"), Some(0x35));
    assert_eq!(key_keycode_macos("esc"), Some(0x35));
    assert_eq!(key_keycode_macos("tab"), Some(0x30));
    assert_eq!(key_keycode_macos("enter"), Some(0x24));
    assert_eq!(key_keycode_macos("return"), Some(0x24));
}

#[cfg(target_os = "macos")]
#[test]
fn key_keycode_macos_unknown_returns_none() {
    // caps/num/scroll lock, print screen, media keys are not wired on macOS.
    assert_eq!(key_keycode_macos("caps lock"), None);
    assert_eq!(key_keycode_macos("nonexistent_key_xyz"), None);
    assert_eq!(key_keycode_macos(""), None);
}

#[cfg(target_os = "macos")]
#[test]
fn modifier_flag_macos_ctrl_remaps_to_command() {
    use core_graphics::event::CGEventFlags;
    // Documented semantic remap: Windows ctrl -> macOS Command.
    assert_eq!(
        modifier_flag_macos("ctrl"),
        Some(CGEventFlags::CGEventFlagCommand)
    );
    assert_eq!(
        modifier_flag_macos("win"),
        Some(CGEventFlags::CGEventFlagCommand)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn modifier_flag_macos_unknown_is_none() {
    assert_eq!(modifier_flag_macos("bogus_modifier"), None);
}

#[cfg(target_os = "windows")]
#[test]
fn shortcut_telemetry_payload_carries_pre_release_snapshot_separately() {
    let pre_release = ModifierAsyncSnapshot::from_states([
        ("lctrl", false),
        ("rctrl", false),
        ("lshift", false),
        ("rshift", false),
        ("lalt", false),
        ("ralt", true),
        ("lwin", false),
        ("rwin", false),
    ]);
    let phase = ModifierAsyncSnapshot::from_states([
        ("lctrl", false),
        ("rctrl", false),
        ("lshift", false),
        ("rshift", false),
        ("lalt", false),
        ("ralt", false),
        ("lwin", false),
        ("rwin", false),
    ]);

    let payload = shortcut_telemetry_payload(
        "windows+printscreen",
        "before_send",
        true,
        1,
        7,
        None,
        true,
        Some("sc0e8"),
        &pre_release,
        &phase,
    );

    assert_eq!(payload["pre_release_async_modifiers"]["ralt"], true);
    assert_eq!(payload["phase_async_modifiers"]["ralt"], false);
    assert_eq!(payload["async_modifiers"]["ralt"], false);
}

#[cfg(target_os = "windows")]
#[test]
fn key_vk_known_letters_and_digits() {
    // 'a' -> VK 0x41 ('A'), '0' -> VK 0x30
    assert_eq!(key_vk("a"), Some(0x41));
    assert_eq!(key_vk("A"), Some(0x41));
    assert_eq!(key_vk("0"), Some(0x30));
    assert_eq!(key_vk("9"), Some(0x39));
}

#[cfg(target_os = "windows")]
#[test]
fn key_vk_known_named_keys() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN, VK_TAB};
    assert_eq!(key_vk("escape"), Some(VK_ESCAPE));
    assert_eq!(key_vk("esc"), Some(VK_ESCAPE));
    assert_eq!(key_vk("enter"), Some(VK_RETURN));
    assert_eq!(key_vk("tab"), Some(VK_TAB));
}

#[cfg(target_os = "windows")]
#[test]
fn key_vk_unknown_returns_none() {
    assert_eq!(key_vk("nonexistent_key_xyz"), None);
    assert_eq!(key_vk(""), None);
}

#[cfg(target_os = "windows")]
fn raw_key(input: &RawInput) -> RawKey {
    match input {
        RawInput::Key(key) => *key,
        other => panic!("expected a keyboard input, got {other:?}"),
    }
}

#[cfg(target_os = "windows")]
#[test]
fn held_modifier_keyups_use_physical_scancodes() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_LMENU, VK_RMENU, VK_RSHIFT};

    let right_alt = key_input(VK_RMENU, true, true);
    let right_alt_ki = raw_key(&right_alt);
    assert_eq!(right_alt_ki.vk, 0);
    assert_eq!(right_alt_ki.scan, 0x38);
    assert!(right_alt_ki.up);
    assert!(right_alt_ki.by_scan);
    assert!(right_alt_ki.extended);

    let left_alt = key_input(VK_LMENU, true, true);
    let left_alt_ki = raw_key(&left_alt);
    assert_eq!(left_alt_ki.vk, 0);
    assert_eq!(left_alt_ki.scan, 0x38);
    assert!(!left_alt_ki.extended);

    let right_shift = key_input(VK_RSHIFT, true, true);
    let right_shift_ki = raw_key(&right_shift);
    assert_eq!(right_shift_ki.vk, 0);
    assert_eq!(right_shift_ki.scan, 0x36);
    assert!(!right_shift_ki.extended);
}

#[cfg(target_os = "windows")]
#[test]
fn held_modifier_keyups_are_derived_from_pre_release_snapshot() {
    let snapshot = ModifierAsyncSnapshot::from_states([
        ("lctrl", true),
        ("rctrl", false),
        ("lshift", false),
        ("rshift", false),
        ("lalt", false),
        ("ralt", true),
        ("lwin", false),
        ("rwin", false),
    ]);
    let ups = held_modifier_keyups(&snapshot);

    assert_eq!(ups.len(), 2);
    let left_ctrl = raw_key(&ups[0]);
    assert_eq!(left_ctrl.vk, 0);
    assert_eq!(left_ctrl.scan, 0x1D);
    assert!(left_ctrl.up);
    assert!(left_ctrl.by_scan);
    assert!(!left_ctrl.extended);

    let right_alt = raw_key(&ups[1]);
    assert_eq!(right_alt.vk, 0);
    assert_eq!(right_alt.scan, 0x38);
    assert!(right_alt.up);
    assert!(right_alt.by_scan);
    assert!(right_alt.extended);
}

#[cfg(target_os = "windows")]
#[test]
fn generic_alt_release_keyups_use_vk_menu_variants() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_LMENU, VK_MENU, VK_RMENU};

    let ups = generic_alt_release_keyups();
    assert_eq!(ups.len(), 3);
    let expected = [VK_MENU, VK_LMENU, VK_RMENU];
    for (input, vk) in ups.iter().zip(expected) {
        let ki = raw_key(input);
        assert_eq!(ki.vk, vk);
        assert_eq!(ki.scan, 0);
        assert!(ki.up);
        assert!(!ki.by_scan);
    }
}

#[cfg(target_os = "windows")]
#[test]
fn scancode_nullifier_wraps_win_printscreen_batch() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_RMENU;

    let right_alt_up = key_input(VK_RMENU, true, true);
    let batch = build_chord_inputs(
        "windows+printscreen",
        Some(true),
        Some(ChordNullifier::ScanCodeE8),
        vec![right_alt_up],
    )
    .expect("build chord inputs");

    assert!(batch.use_scancode);
    assert_eq!(batch.nullifier_label, Some("sc0e8"));
    assert_eq!(batch.modifier_keyup_count, 1);
    assert_eq!(batch.inputs.len(), 7);

    let nullifier_down = raw_key(&batch.inputs[0]);
    assert_eq!(nullifier_down.vk, 0);
    assert_eq!(nullifier_down.scan, 0xE8);
    assert!(nullifier_down.by_scan);
    assert!(!nullifier_down.up);
    assert!(!nullifier_down.extended);

    let masked_right_alt_up = raw_key(&batch.inputs[1]);
    assert_eq!(masked_right_alt_up.scan, 0x38);
    assert!(masked_right_alt_up.up);
    assert!(masked_right_alt_up.extended);

    let win_down = raw_key(&batch.inputs[2]);
    assert_eq!(win_down.vk, 0);
    assert_eq!(win_down.scan, 0x5B);
    assert!(win_down.by_scan);
    assert!(win_down.extended);

    let printscreen_down = raw_key(&batch.inputs[3]);
    assert_eq!(printscreen_down.vk, 0);
    assert_eq!(printscreen_down.scan, 0x37);
    assert!(printscreen_down.by_scan);
    assert!(printscreen_down.extended);

    let nullifier_up = raw_key(&batch.inputs[6]);
    assert_eq!(nullifier_up.vk, 0);
    assert_eq!(nullifier_up.scan, 0xE8);
    assert!(nullifier_up.by_scan);
    assert!(nullifier_up.up);
}

#[cfg(target_os = "windows")]
#[test]
fn vk_nullifier_wraps_vk_printscreen_batch() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_LWIN, VK_SNAPSHOT};

    let batch = build_chord_inputs(
        "windows+printscreen",
        Some(false),
        Some(ChordNullifier::VirtualKeyE8),
        Vec::new(),
    )
    .expect("build chord inputs");

    assert!(!batch.use_scancode);
    assert_eq!(batch.nullifier_label, Some("vkE8"));
    assert_eq!(batch.inputs.len(), 6);

    let nullifier_down = raw_key(&batch.inputs[0]);
    assert_eq!(nullifier_down.vk, 0xE8);
    assert_eq!(nullifier_down.scan, 0);
    assert!(!nullifier_down.by_scan);
    assert!(!nullifier_down.up);

    let win_down = raw_key(&batch.inputs[1]);
    assert_eq!(win_down.vk, VK_LWIN);
    assert_eq!(win_down.scan, 0);
    assert!(!win_down.by_scan);

    let printscreen_down = raw_key(&batch.inputs[2]);
    assert_eq!(printscreen_down.vk, VK_SNAPSHOT);
    assert_eq!(printscreen_down.scan, 0);
    assert!(!printscreen_down.by_scan);
}

#[cfg(target_os = "windows")]
#[test]
fn telemetry_chord_label_is_bounded_ascii() {
    let label = telemetry_chord_label("windows+printscreen\nsecret");
    assert_eq!(label, "windows+printscreen?secret");

    let long = telemetry_chord_label(&"a".repeat(90));
    assert_eq!(long.len(), 83);
    assert!(long.ends_with("..."));
}

#[test]
fn dispatch_key_sequence_rejects_empty_chords() {
    let result = dispatch_key_sequence(&[], None);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, "E_EMPTY_SEQUENCE");
}

#[cfg(target_os = "macos")]
#[test]
fn macos_media_key_maps_catalog_tokens() {
    assert_eq!(
        macos_media_key("volume up").map(|k| k.mac_key_type()),
        Some(0)
    );
    assert_eq!(
        macos_media_key("volume down").map(|k| k.mac_key_type()),
        Some(1)
    );
    assert_eq!(
        macos_media_key("volume mute").map(|k| k.mac_key_type()),
        Some(7)
    );
    assert_eq!(macos_media_key("Mute").map(|k| k.mac_key_type()), Some(7));
    assert_eq!(
        macos_media_key("play/pause media").map(|k| k.mac_key_type()),
        Some(16)
    );
    assert_eq!(
        macos_media_key("play pause").map(|k| k.mac_key_type()),
        Some(16)
    );
    assert_eq!(
        macos_media_key("play/pause").map(|k| k.mac_key_type()),
        Some(16)
    );
    assert_eq!(
        macos_media_key("next track").map(|k| k.mac_key_type()),
        Some(17)
    );
    assert_eq!(
        macos_media_key("previous track").map(|k| k.mac_key_type()),
        Some(18)
    );
    assert_eq!(macos_media_key("cmd+c"), None);
}
