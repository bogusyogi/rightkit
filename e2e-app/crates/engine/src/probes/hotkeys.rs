//! rightkit-hotkeys: chord parsing and display, the in-process conflict registry, the RegisterHotKey bookkeeping against
//! a recording OS fake, the low-level hook matcher over raw key events, and then the real OS registration of a chord:
//! register, a second registration of the same chord must be refused as a conflict, unregister, register again.
//! Press/release delivery needs synthetic input (Accessibility/Input Monitoring on macOS, a foreground desktop on
//! Windows) and the host's main run loop: that is covered by an app-level scenario.
use rightkit_hotkeys::registration::{HotkeyApi, Registrations, UnregisterFailure};
use rightkit_hotkeys::{
    parse_chord, ChordRegistry, HookChords, HotkeyConflict, HotkeyState, Modifier, Platform,
    PttAlias, RegisterFailure,
};
use serde_json::{json, Value};

/// A chord no shipping app or the OS binds.
const CHORD: &str = "Ctrl+Alt+Shift+F19";

struct Recorder {
    calls: Vec<String>,
    fail_register_with: Option<u32>,
}

impl HotkeyApi for Recorder {
    fn register(&mut self, id: i32, modifiers: u32, vk: u32) -> Result<(), u32> {
        self.calls
            .push(format!("register {id} {modifiers:#x} {vk:#x}"));
        match self.fail_register_with {
            Some(code) => Err(code),
            None => Ok(()),
        }
    }
    fn unregister(&mut self, id: i32) -> Result<(), u32> {
        self.calls.push(format!("unregister {id}"));
        Ok(())
    }
}

fn pure_checks() -> Result<Value, String> {
    // Parsing and display.
    let a = parse_chord("shift + ctrl + K").map_err(|e| e.to_string())?;
    let b = parse_chord("Ctrl+Shift+K").map_err(|e| e.to_string())?;
    if a != b || a.canonical() != "Ctrl+Shift+K" {
        return Err(format!(
            "modifier order is not normalised: {} vs {}",
            a.canonical(),
            b.canonical()
        ));
    }
    let round = parse_chord(&a.canonical()).map_err(|e| e.to_string())?;
    if round != a {
        return Err("canonical() does not round-trip through parse_chord".into());
    }
    for bad in ["", "Ctrl+Shift", "Ctrl+K+J", "Ctrl+Ctrl+K", "Ctrl+Bogus"] {
        if parse_chord(bad).is_ok() {
            return Err(format!("parse_chord accepted the invalid chord '{bad}'"));
        }
    }
    let mac = a.format(Platform::Mac);
    if !mac.contains('\u{2303}') || !mac.contains('\u{21E7}') {
        return Err(format!(
            "macOS display lacks the Control/Shift glyphs: {mac}"
        ));
    }
    if a.format(Platform::Windows) != "Ctrl+Shift+K" {
        return Err(format!(
            "Windows display is {}",
            a.format(Platform::Windows)
        ));
    }
    if PttAlias::parse("Right Ctrl", Platform::Windows) != Ok(PttAlias::RightCtrl)
        || PttAlias::parse("lctrl", Platform::Windows).is_ok()
    {
        return Err("push-to-talk alias rules differ from the documented Windows set".into());
    }

    // In-process conflict registry.
    let mut registry = ChordRegistry::new();
    registry.bind(&a, "palette").map_err(|e| format!("{e:?}"))?;
    let conflict = registry
        .bind(&b, "search")
        .err()
        .ok_or("the registry let two owners bind one chord")?;
    if conflict.held_by != "palette" || registry.owner_of(&a) != Some("palette") {
        return Err(format!(
            "registry conflict names the wrong holder: {conflict:?}"
        ));
    }
    registry.release(&a);
    registry.bind(&b, "search").map_err(|e| format!("{e:?}"))?;
    let hc = HotkeyConflict::from_registration_error(
        "register Ctrl+Shift+K failed: x",
        &["Ctrl+Shift+K", "Alt+Space"],
        "m",
    );
    if hc.shortcuts != ["Ctrl+Shift+K"] {
        return Err(format!("HotkeyConflict named {:?}", hc.shortcuts));
    }
    if RegisterFailure::from_win32(1409) != RegisterFailure::Conflict
        || RegisterFailure::from_osstatus(-9878) != RegisterFailure::Conflict
    {
        return Err("a duplicate registration status is not classified as Conflict".into());
    }

    // RegisterHotKey bookkeeping against a recording fake.
    let chord = parse_chord("Ctrl+Alt+K").map_err(|e| e.to_string())?;
    let mut regs = Registrations::new();
    let mut api = Recorder {
        calls: vec![],
        fail_register_with: None,
    };
    let id = regs
        .register(&mut api, &chord, false)
        .map_err(|c| format!("fake register failed {c}"))?;
    if regs.id_of(&chord) != Some(id) || regs.len() != 1 {
        return Err("Registrations lost the chord it registered".into());
    }
    // MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, VK_K.
    if api.calls[0] != "register 1 0x4003 0x4b" {
        return Err(format!(
            "RegisterHotKey was asked for {} instead of 'register 1 0x4003 0x4b'",
            api.calls[0]
        ));
    }
    regs.unregister(&mut api, &chord)
        .map_err(|e| format!("{e:?}"))?;
    if regs.unregister(&mut api, &chord) != Err(UnregisterFailure::NotRegistered)
        || !regs.is_empty()
    {
        return Err("a second unregister was not NotRegistered".into());
    }
    api.fail_register_with = Some(1409);
    if regs.register(&mut api, &chord, false) != Err(1409) || !regs.is_empty() {
        return Err("a refused registration was recorded".into());
    }

    // Hook matcher over raw key events (VK_LMENU 0xA4, VK_SPACE 0x20).
    let mut hook = HookChords::new();
    let id = hook
        .bind(&parse_chord("Alt+Space").map_err(|e| e.to_string())?)
        .map_err(|e| format!("{e:?}"))?;
    if hook.on_key(0xA4, true, false).swallow {
        return Err("a bare modifier press was swallowed".into());
    }
    let press = hook.on_key(0x20, true, false);
    if press.events != [(id, HotkeyState::Pressed)] || !press.swallow {
        return Err(format!("Alt+Space press was {press:?}"));
    }
    let repeat = hook.on_key(0x20, true, false);
    if !repeat.events.is_empty() || !repeat.swallow {
        return Err(format!("auto-repeat was {repeat:?}"));
    }
    let release = hook.on_key(0x20, false, false);
    if release.events != [(id, HotkeyState::Released)] {
        return Err(format!("Alt+Space release was {release:?}"));
    }
    if !hook.on_key(0x20, true, true).events.is_empty() {
        return Err("injected input matched a hook chord".into());
    }
    let _ = Modifier::Ctrl;
    Ok(json!({ "chord": a.canonical(), "mac_display": mac, "register_hotkey_call": api.calls[0] }))
}

#[cfg(target_os = "macos")]
fn os_registration() -> Result<Value, String> {
    use rightkit_hotkeys::macos::global::{GlobalHotkeys, RegisterError};
    let chord = parse_chord(CHORD).map_err(|e| e.to_string())?;
    let mut hotkeys = GlobalHotkeys::new(|_, _| {})
        .map_err(|status| format!("Carbon InstallEventHandler on the application event target failed (OSStatus {status}); needs a process with an application event dispatcher"))?;
    let id = hotkeys.register(&chord).map_err(|e| e.message())?;
    if hotkeys.registered() != 1 {
        return Err("GlobalHotkeys.registered() is not 1 after a registration".into());
    }
    match hotkeys.register(&chord) {
        Err(RegisterError::Conflict { .. }) => {}
        Err(other) => {
            return Err(format!(
                "a duplicate registration was refused as {other:?}, expected Conflict"
            ))
        }
        Ok(_) => return Err("a duplicate registration of the same chord succeeded".into()),
    }
    if !hotkeys.unregister(id) || hotkeys.registered() != 0 {
        return Err("unregister did not release the hotkey".into());
    }
    let again = hotkeys
        .register(&chord)
        .map_err(|e| format!("re-register after unregister: {}", e.message()))?;
    hotkeys.unregister(again);
    // F21-F24 have no macOS keycode: refused, never mapped to a wrong key.
    let f21 = parse_chord("Ctrl+Alt+Shift+F21").map_err(|e| e.to_string())?;
    if !matches!(hotkeys.register(&f21), Err(RegisterError::Unsupported(_))) {
        return Err("F21 was not refused as Unsupported on macOS".into());
    }
    Ok(json!({ "backend": "carbon RegisterEventHotKey", "id": id.0, "duplicate": "Conflict" }))
}

#[cfg(windows)]
fn os_registration() -> Result<Value, String> {
    use rightkit_hotkeys::windows::{GlobalHotkeys, RegisterError};
    let chord = parse_chord(CHORD).map_err(|e| e.to_string())?;
    let hotkeys = GlobalHotkeys::new(|_| {}).map_err(|e| format!("GlobalHotkeys::new: {e}"))?;
    let id = hotkeys.register(&chord).map_err(|e| e.message())?;
    match hotkeys.register(&chord) {
        Err(RegisterError::Conflict { .. }) => {}
        Err(other) => {
            return Err(format!(
                "a duplicate registration was refused as {other:?}, expected Conflict"
            ))
        }
        Ok(_) => return Err("a duplicate registration of the same chord succeeded".into()),
    }
    let released = hotkeys.unregister(&chord).map_err(|e| e.to_string())?;
    if released != id {
        return Err(format!(
            "unregister released id {released}, registered {id}"
        ));
    }
    let again = hotkeys
        .register(&chord)
        .map_err(|e| format!("re-register after unregister: {}", e.message()))?;
    hotkeys.unregister(&chord).map_err(|e| e.to_string())?;
    Ok(
        json!({ "backend": "RegisterHotKey", "id": id, "re_registered_id": again, "duplicate": "Conflict" }),
    )
}

#[cfg(not(any(target_os = "macos", windows)))]
fn os_registration() -> Result<Value, String> {
    Err("needs macOS (Carbon RegisterEventHotKey) or Windows (RegisterHotKey)".into())
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let pure = pure_checks()?;
    let os = os_registration()?;
    Ok(json!({ "pure": pure, "os": os }))
}
