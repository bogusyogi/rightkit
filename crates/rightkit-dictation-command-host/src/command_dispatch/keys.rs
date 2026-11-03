// Key sequence dispatch + app launch — SendInput (Windows) / CGEvent (macOS).

use super::entry::{DispatchError, DispatchOutcome, DispatchResult};
#[cfg(target_os = "windows")]
use rightkit_automation::win::{send_input_batch, RawInput, RawKey};

#[cfg(target_os = "windows")]
const MODIFIER_RELEASE_DRAIN_MS: u64 = 30;

// ---------------------------------------------------------------------------
// Key sequence — list of chord strings sent in order via SendInput
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub(super) fn dispatch_key_sequence(
    chords: &[String],
    description: Option<&str>,
) -> DispatchResult {
    if chords.is_empty() {
        return Err(DispatchError::new(
            "E_EMPTY_SEQUENCE",
            "KeySequence with no chords",
        ));
    }
    if chords.len() == 1 && chords[0].eq_ignore_ascii_case("windows+printscreen") {
        return super::screenshot::dispatch_windows_screenshot();
    }
    for (i, chord) in chords.iter().enumerate() {
        send_chord(chord).map_err(|e| {
            DispatchError::new(
                "E_SENDINPUT",
                format!(
                    "chord {} of {} ({:?}): {}",
                    i + 1,
                    chords.len(),
                    chord,
                    e.message
                ),
            )
        })?;
        if chords.len() > 1 {
            std::thread::sleep(std::time::Duration::from_millis(35));
        }
    }
    Ok(DispatchOutcome::new(
        "key_sequence",
        format!("{} chord(s)  desc={:?}", chords.len(), description),
    ))
}

#[cfg(target_os = "macos")]
pub(super) fn dispatch_key_sequence(
    chords: &[String],
    description: Option<&str>,
) -> DispatchResult {
    use rightkit_voice_commands::command_recognition::{macos_command, MacCommand};
    if chords.is_empty() {
        return Err(DispatchError::new(
            "E_EMPTY_SEQUENCE",
            "KeySequence with no chords",
        ));
    }
    if chords.len() == 1 && chords[0].eq_ignore_ascii_case("windows+printscreen") {
        return super::screenshot::dispatch_macos_screenshot();
    }
    let mut sent = 0usize;
    let mut skipped = 0usize;
    let mut media_sent = 0usize;
    for (i, chord) in chords.iter().enumerate() {
        if let Some(key) = macos_media_key(chord) {
            post_macos_media_key(key).map_err(|m| {
                DispatchError::new(
                    "E_MEDIA_KEY",
                    format!("chord {} of {} ({:?}): {}", i + 1, chords.len(), chord, m),
                )
            })?;
            sent += 1;
            media_sent += 1;
            if chords.len() > 1 {
                std::thread::sleep(std::time::Duration::from_millis(35));
            }
            continue;
        }
        // The catalog is Windows-authored; translate per-chord for macOS.
        // `Remap` is a literal macOS chord (cmd/ctrl/opt/shift); `Default` keeps
        // the Windows chord and applies the ctrl→⌘ semantic remap; `Unsupported`
        // is skipped (no macOS equivalent) so the command never errors/hangs.
        let result = match macos_command(chord) {
            MacCommand::Unsupported => {
                skipped += 1;
                continue;
            }
            MacCommand::Remap(mac) => send_literal_macos_chord(mac),
            MacCommand::Default => send_chord_macos(chord),
        };
        result.map_err(|e| {
            DispatchError::new(
                "E_CGEVENT",
                format!(
                    "chord {} of {} ({:?}): {}",
                    i + 1,
                    chords.len(),
                    chord,
                    e.message
                ),
            )
        })?;
        sent += 1;
        if chords.len() > 1 {
            std::thread::sleep(std::time::Duration::from_millis(35));
        }
    }
    if sent == 0 {
        // Whole command has no macOS equivalent — succeed as a no-op so the
        // engine returns to idle instead of erroring (and hanging) the session.
        return Ok(DispatchOutcome::new(
            "key_sequence",
            format!("no macOS equivalent — skipped {skipped} chord(s)  desc={description:?}"),
        ));
    }
    if media_sent == sent {
        return Ok(DispatchOutcome::new(
            "media_key",
            format!("{sent} media key(s)  desc={description:?}"),
        ));
    }
    Ok(DispatchOutcome::new(
        "key_sequence",
        format!("{sent} chord(s)  desc={description:?}"),
    ))
}

/// Map a catalog chord token to a RightKit media key (system volume/media).
#[cfg(target_os = "macos")]
pub(super) fn macos_media_key(token: &str) -> Option<rightkit_automation::MediaKey> {
    use rightkit_automation::MediaKey;
    match token.trim().to_ascii_lowercase().as_str() {
        "volume up" => Some(MediaKey::VolumeUp),
        "volume down" => Some(MediaKey::VolumeDown),
        "volume mute" | "mute" => Some(MediaKey::Mute),
        "play/pause media" | "play pause" | "play/pause" => Some(MediaKey::PlayPause),
        "next track" => Some(MediaKey::NextTrack),
        "previous track" => Some(MediaKey::PreviousTrack),
        _ => None,
    }
}

/// Post a system media key press (down then up) through the RightKit owner.
#[cfg(target_os = "macos")]
fn post_macos_media_key(key: rightkit_automation::MediaKey) -> Result<(), String> {
    rightkit_automation::send_media_key(key).map_err(|e| e.to_string())
}

/// Post a LITERAL macOS chord — tokens are not semantically remapped here:
/// `cmd`=⌘, `ctrl`=real ⌃ Control, `opt`/`alt`=⌥ Option, `shift`=⇧. Used for the
/// `MacCommand::Remap` outputs (e.g. `cmd+tab`, `ctrl+tab`, `opt+backspace`).
#[cfg(target_os = "macos")]
fn send_literal_macos_chord(chord: &str) -> Result<(), DispatchError> {
    use core_graphics::event::CGEventFlags;
    let parts: Vec<&str> = chord.split('+').map(|s| s.trim()).collect();
    if parts.is_empty() || parts[parts.len() - 1].is_empty() {
        return Err(DispatchError::new("E_BAD_CHORD", "empty chord"));
    }
    let key = parts[parts.len() - 1];
    let mods = &parts[..parts.len() - 1];

    let mut flags = CGEventFlags::empty();
    for m in mods {
        let flag = match m.to_ascii_lowercase().as_str() {
            "cmd" | "command" => CGEventFlags::CGEventFlagCommand,
            "ctrl" | "control" => CGEventFlags::CGEventFlagControl, // REAL Control
            "opt" | "option" | "alt" => CGEventFlags::CGEventFlagAlternate,
            "shift" => CGEventFlags::CGEventFlagShift,
            other => {
                return Err(DispatchError::new(
                    "E_BAD_MOD",
                    format!("unknown literal modifier {other:?}"),
                ))
            }
        };
        flags |= flag;
    }
    let keycode = key_keycode_macos(key).ok_or_else(|| {
        DispatchError::new(
            "E_BAD_KEY",
            format!("unknown key {key:?} in chord {chord:?}"),
        )
    })?;

    if crate::macos_input::post_chord(keycode, flags) {
        Ok(())
    } else {
        Err(DispatchError::new(
            "E_CGEVENT",
            "could not post chord (Accessibility permission not granted?)",
        ))
    }
}

/// Parse a chord ("ctrl+shift+v") and post it as a CGEvent. The grammar is
/// Windows-centric, so we apply the **semantic** macOS mapping: `ctrl` (the
/// primary editing modifier on Windows) becomes ⌘ Command on macOS, so the bulk
/// of the command set (copy/paste/cut/undo/save/find/select-all) works natively.
/// A handful of genuinely-Control chords would need per-command overrides.
/// See docs/COMMANDS_AND_LAYERS.md.
#[cfg(target_os = "macos")]
pub(super) fn send_chord_macos(chord: &str) -> Result<(), DispatchError> {
    use core_graphics::event::CGEventFlags;
    let parts: Vec<&str> = chord.split('+').map(|s| s.trim()).collect();
    if parts.is_empty() || parts[parts.len() - 1].is_empty() {
        return Err(DispatchError::new("E_BAD_CHORD", "empty chord"));
    }
    let key = parts[parts.len() - 1];
    let mods = &parts[..parts.len() - 1];

    let mut flags = CGEventFlags::empty();
    for m in mods {
        flags |= modifier_flag_macos(m)
            .ok_or_else(|| DispatchError::new("E_BAD_MOD", format!("unknown modifier {:?}", m)))?;
    }
    let keycode = key_keycode_macos(key).ok_or_else(|| {
        DispatchError::new(
            "E_BAD_KEY",
            format!("unknown key {:?} in chord {:?}", key, chord),
        )
    })?;

    if crate::macos_input::post_chord(keycode, flags) {
        Ok(())
    } else {
        Err(DispatchError::new(
            "E_CGEVENT",
            "could not post chord (Accessibility permission not granted?)",
        ))
    }
}

#[cfg(target_os = "macos")]
fn modifier_flag_macos(name: &str) -> Option<core_graphics::event::CGEventFlags> {
    use core_graphics::event::CGEventFlags;
    match name.to_ascii_lowercase().as_str() {
        // ctrl → ⌘ (semantic remap; see send_chord_macos).
        "ctrl" | "control" | "left ctrl" | "left control" | "right ctrl" | "right control"
        | "win" | "windows" | "super" | "meta" | "command" | "cmd" | "left win" | "right win"
        | "left windows" | "right windows" => Some(CGEventFlags::CGEventFlagCommand),
        "shift" | "left shift" | "right shift" => Some(CGEventFlags::CGEventFlagShift),
        "alt" | "option" | "left alt" | "right alt" | "left option" | "right option" => {
            Some(CGEventFlags::CGEventFlagAlternate)
        }
        _ => None,
    }
}

/// Windows VK key-name → macOS Carbon virtual keycode. Carbon codes are
/// positional (not ASCII), so this is an explicit table.
#[cfg(target_os = "macos")]
pub(super) fn key_keycode_macos(name: &str) -> Option<core_graphics::event::CGKeyCode> {
    let n = name.to_ascii_lowercase();
    let n = n.as_str();
    if n.len() == 1 {
        let c = n.chars().next().unwrap();
        return match c {
            'a' => Some(0x00),
            'b' => Some(0x0B),
            'c' => Some(0x08),
            'd' => Some(0x02),
            'e' => Some(0x0E),
            'f' => Some(0x03),
            'g' => Some(0x05),
            'h' => Some(0x04),
            'i' => Some(0x22),
            'j' => Some(0x26),
            'k' => Some(0x28),
            'l' => Some(0x25),
            'm' => Some(0x2E),
            'n' => Some(0x2D),
            'o' => Some(0x1F),
            'p' => Some(0x23),
            'q' => Some(0x0C),
            'r' => Some(0x0F),
            's' => Some(0x01),
            't' => Some(0x11),
            'u' => Some(0x20),
            'v' => Some(0x09),
            'w' => Some(0x0D),
            'x' => Some(0x07),
            'y' => Some(0x10),
            'z' => Some(0x06),
            '0' => Some(0x1D),
            '1' => Some(0x12),
            '2' => Some(0x13),
            '3' => Some(0x14),
            '4' => Some(0x15),
            '5' => Some(0x17),
            '6' => Some(0x16),
            '7' => Some(0x1A),
            '8' => Some(0x1C),
            '9' => Some(0x19),
            '=' | '+' => Some(0x18),
            '-' | '_' => Some(0x1B),
            '[' => Some(0x21), // ⌘[ Back
            ']' => Some(0x1E), // ⌘] Forward
            _ => None,
        };
    }
    match n {
        "tab" => Some(0x30),
        "enter" | "return" => Some(0x24),
        "space" => Some(0x31),
        "esc" | "escape" => Some(0x35),
        "home" => Some(0x73),
        "end" => Some(0x77),
        "delete" | "del" | "forward delete" => Some(0x75), // forward delete
        "backspace" | "back" => Some(0x33),                // ⌫
        "up" | "arrow up" => Some(0x7E),
        "down" | "arrow down" => Some(0x7D),
        "left" | "arrow left" => Some(0x7B),
        "right" | "arrow right" => Some(0x7C),
        "page up" | "pageup" => Some(0x74),
        "page down" | "pagedown" => Some(0x79),
        "f1" => Some(0x7A),
        "f2" => Some(0x78),
        "f3" => Some(0x63),
        "f4" => Some(0x76),
        "f5" => Some(0x60),
        "f6" => Some(0x61),
        "f7" => Some(0x62),
        "f8" => Some(0x64),
        "f9" => Some(0x65),
        "f10" => Some(0x6D),
        "f11" => Some(0x67),
        "f12" => Some(0x6F),
        "f13" => Some(0x69),
        "f14" => Some(0x6B),
        "f15" => Some(0x71),
        "f16" => Some(0x6A),
        "f17" => Some(0x40),
        "f18" => Some(0x4F),
        "f19" => Some(0x50),
        "f20" => Some(0x5A),
        // Volume and media keys are NOT kVK codes; see `macos_media_key`.
        _ => None, // caps/num/scroll lock, print screen: not wired on macOS yet
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(super) fn dispatch_key_sequence(
    _chords: &[String],
    _description: Option<&str>,
) -> DispatchResult {
    Err(DispatchError::new(
        "E_UNSUPPORTED",
        "KeySequence is unsupported on this platform",
    ))
}

/// Scan code for VKs where `MapVirtualKeyW(_, MAPVK_VK_TO_VSC)` is missing or
/// returns a shell-unusable injected form. PrintScreen (VK_SNAPSHOT) maps to
/// `0x54` on Adrian's Win11 machine, but injected Ctrl/Win+PrtScn with that scan
/// code is ignored; the hardware-style extended make code is `0xE0 0x37`, so
/// wScan `0x37` + KEYEVENTF_EXTENDEDKEY (set by `is_extended_vk`) is the
/// injectable form. Returns 0 for anything without a known override.
#[cfg(target_os = "windows")]
fn known_scancode(vk: u16) -> u16 {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
        VK_SNAPSHOT,
    };
    match vk {
        VK_LCONTROL => 0x1D,
        VK_RCONTROL => 0x1D,
        VK_LSHIFT => 0x2A,
        VK_RSHIFT => 0x36,
        VK_LMENU => 0x38,
        VK_RMENU => 0x38,
        VK_LWIN => 0x5B,
        VK_RWIN => 0x5C,
        VK_SNAPSHOT => 0x37,
        _ => 0,
    }
}

/// VKs that must carry KEYEVENTF_EXTENDEDKEY when injected by scan code — the
/// Win keys, right-hand Ctrl/Alt, the nav cluster, and a few others. Only needed
/// on the scancode path (Win-key chords); the plain-VK path is unaffected.
#[cfg(target_os = "windows")]
fn is_extended_vk(vk: u16) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_DELETE, VK_DOWN, VK_END, VK_HOME, VK_INSERT, VK_LEFT, VK_LWIN, VK_NEXT, VK_NUMLOCK,
        VK_PRIOR, VK_RCONTROL, VK_RIGHT, VK_RMENU, VK_RWIN, VK_SNAPSHOT, VK_UP,
    };
    matches!(
        vk,
        x if x == VK_LWIN
            || x == VK_RWIN
            || x == VK_RCONTROL
            || x == VK_RMENU
            || x == VK_LEFT
            || x == VK_UP
            || x == VK_RIGHT
            || x == VK_DOWN
            || x == VK_PRIOR
            || x == VK_NEXT
            || x == VK_END
            || x == VK_HOME
            || x == VK_INSERT
            || x == VK_DELETE
            || x == VK_NUMLOCK
            || x == VK_SNAPSHOT
    )
}

/// Release any modifier the USER is still physically holding for legacy direct
/// callers. `send_chord` deliberately does not do a separate pre-release:
/// splitting the key-up and chord into separate SendInput calls regressed
/// Alt+Tab while RAlt PTT was held. Chord dispatch prefixes any modifier key-ups
/// into the same SendInput batch instead, and logs the physical async state so
/// shell-hotkey leaks are diagnosable from events.jsonl.
#[cfg(target_os = "windows")]
pub(super) fn release_held_modifiers() -> Result<usize, DispatchError> {
    release_held_modifiers_for_chord("direct")
}

#[cfg(target_os = "windows")]
fn release_held_modifiers_for_chord(chord: &str) -> Result<usize, DispatchError> {
    let started = std::time::Instant::now();
    let pre_release_async_modifiers = ModifierAsyncSnapshot::capture();
    let ups = held_modifier_keyups(&pre_release_async_modifiers);
    if ups.is_empty() {
        return Ok(0);
    }

    let sent = send_raw(&ups);
    let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let chord_label = telemetry_chord_label(chord);
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    let ok = sent as usize == ups.len();
    super::entry::append_command_dispatch_event(&serde_json::json!({
        "event": "shortcut_modifier_release",
        "schema_version": 1,
        "ts_ms": ts_ms,
        "chord": chord_label,
        "count": ups.len(),
        "sent": sent,
        "drain_ms": MODIFIER_RELEASE_DRAIN_MS,
        "elapsed_ms": elapsed_ms,
        "ok": ok,
        "pre_release_async_modifiers": pre_release_async_modifiers.to_json(),
    }));
    if !ok {
        tracing::warn!(
            sent,
            expected = ups.len(),
            chord = %chord_label,
            "SendInput did not release all held modifiers before injected chord"
        );
        return Err(DispatchError::new(
            "E_SENDINPUT",
            format!(
                "SendInput released {}/{} held modifiers before {}",
                sent,
                ups.len(),
                chord_label
            ),
        ));
    }
    tracing::debug!(
        count = ups.len(),
        elapsed_ms,
        chord = %chord_label,
        "Released held modifiers with physical scancode key-ups before injected chord"
    );
    // Let the key-ups drain through the input queue before the chord goes in.
    std::thread::sleep(std::time::Duration::from_millis(MODIFIER_RELEASE_DRAIN_MS));
    Ok(ups.len())
}

#[cfg(target_os = "windows")]
fn telemetry_chord_label(chord: &str) -> String {
    const MAX_CHARS: usize = 80;
    let mut label = String::new();
    let mut truncated = false;
    for (index, ch) in chord.chars().enumerate() {
        if index >= MAX_CHARS {
            truncated = true;
            break;
        }
        if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '_' | ' ') {
            label.push(ch);
        } else {
            label.push('?');
        }
    }
    if truncated {
        label.push_str("...");
    }
    label
}

#[cfg(target_os = "windows")]
fn held_modifier_keyups(snapshot: &ModifierAsyncSnapshot) -> Vec<RawInput> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
    };

    // Walk both sides explicitly and emit physical scan-code key-ups. Plain
    // virtual-key key-ups do not reliably clear right Alt/AltGr for shell
    // shortcuts; right Alt and right Ctrl must carry the extended-key bit.
    let mut ups = Vec::new();
    for (vk, down) in [
        (VK_LCONTROL, snapshot.lctrl),
        (VK_RCONTROL, snapshot.rctrl),
        (VK_LSHIFT, snapshot.lshift),
        (VK_RSHIFT, snapshot.rshift),
        (VK_LMENU, snapshot.lalt),
        (VK_RMENU, snapshot.ralt),
        (VK_LWIN, snapshot.lwin),
        (VK_RWIN, snapshot.rwin),
    ] {
        if down {
            ups.push(key_input(vk, true, true));
        }
    }
    ups
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChordNullifier {
    // Only the tests build nullified chords; no production path selects one.
}

#[cfg(target_os = "windows")]
impl ChordNullifier {
    fn label(self) -> &'static str {
        match self {}
    }
}

#[cfg(target_os = "windows")]
struct ChordInputBatch {
    inputs: Vec<RawInput>,
    use_scancode: bool,
    modifier_keyup_count: usize,
    nullifier_label: Option<&'static str>,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ModifierAsyncSnapshot {
    lctrl: bool,
    rctrl: bool,
    lshift: bool,
    rshift: bool,
    lalt: bool,
    ralt: bool,
    lwin: bool,
    rwin: bool,
}

#[cfg(target_os = "windows")]
impl ModifierAsyncSnapshot {
    fn capture() -> Self {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU,
            VK_RSHIFT, VK_RWIN,
        };

        fn down(vk: u16) -> bool {
            unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
        }

        Self {
            lctrl: down(VK_LCONTROL),
            rctrl: down(VK_RCONTROL),
            lshift: down(VK_LSHIFT),
            rshift: down(VK_RSHIFT),
            lalt: down(VK_LMENU),
            ralt: down(VK_RMENU),
            lwin: down(VK_LWIN),
            rwin: down(VK_RWIN),
        }
    }

    fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "lctrl": self.lctrl,
            "rctrl": self.rctrl,
            "lshift": self.lshift,
            "rshift": self.rshift,
            "lalt": self.lalt,
            "ralt": self.ralt,
            "lwin": self.lwin,
            "rwin": self.rwin,
        })
    }
}

#[cfg(target_os = "windows")]
fn shortcut_telemetry_payload(
    chord: &str,
    phase: &str,
    use_scancode: bool,
    modifier_keyup_count: usize,
    input_count: usize,
    sent: Option<u32>,
    ok: bool,
    nullifier_label: Option<&'static str>,
    pre_release_async_modifiers: &ModifierAsyncSnapshot,
    phase_async_modifiers: &ModifierAsyncSnapshot,
) -> serde_json::Value {
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    let phase_async_modifiers = phase_async_modifiers.to_json();
    serde_json::json!({
        "event": "shortcut_sendinput",
        "schema_version": 1,
        "ts_ms": ts_ms,
        "phase": phase,
        "chord": telemetry_chord_label(chord),
        "use_scancode": use_scancode,
        "modifier_keyup_count": modifier_keyup_count,
        "input_count": input_count,
        "sent": sent,
        "ok": ok,
        "nullifier": nullifier_label,
        "async_modifiers": phase_async_modifiers.clone(),
        "phase_async_modifiers": phase_async_modifiers,
        "pre_release_async_modifiers": pre_release_async_modifiers.to_json(),
    })
}

#[cfg(target_os = "windows")]
fn shortcut_telemetry(
    chord: &str,
    phase: &str,
    use_scancode: bool,
    modifier_keyup_count: usize,
    input_count: usize,
    sent: Option<u32>,
    ok: bool,
    nullifier_label: Option<&'static str>,
    pre_release_async_modifiers: &ModifierAsyncSnapshot,
) {
    let phase_async_modifiers = ModifierAsyncSnapshot::capture();
    super::entry::append_command_dispatch_event(&shortcut_telemetry_payload(
        chord,
        phase,
        use_scancode,
        modifier_keyup_count,
        input_count,
        sent,
        ok,
        nullifier_label,
        pre_release_async_modifiers,
        &phase_async_modifiers,
    ));
}

/// Inject one batch through RightKit's `SendInput` owner and report how many events the
/// system accepted (all of them on success), so the telemetry and error text keep their
/// `sent/expected` counts.
#[cfg(target_os = "windows")]
fn send_raw(inputs: &[RawInput]) -> u32 {
    match send_input_batch(inputs) {
        Ok(()) => inputs.len() as u32,
        Err(rightkit_automation::Error::Platform(inserted, _)) => inserted.max(0) as u32,
        Err(_) => 0,
    }
}

#[cfg(target_os = "windows")]
fn key_input(vk: u16, keyup: bool, use_scancode: bool) -> RawInput {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC};

    if use_scancode {
        let mut scan = known_scancode(vk);
        if scan == 0 {
            scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
        }
        if scan != 0 {
            return RawInput::Key(RawKey {
                vk: 0,
                scan,
                up: keyup,
                extended: is_extended_vk(vk),
                by_scan: true,
            });
        }
    }
    // Plain-VK path (also the fallback when no scan code is known): the extended bit is
    // deliberately NOT set, as before.
    RawInput::Key(RawKey {
        vk,
        scan: 0,
        up: keyup,
        extended: false,
        by_scan: false,
    })
}

#[cfg(target_os = "windows")]
fn nullifier_input(nullifier: ChordNullifier, keyup: bool) -> RawInput {
    // Outside tests `ChordNullifier` has no variants, so `keyup` is unread.
    let _ = keyup;
    match nullifier {}
}

#[cfg(target_os = "windows")]
pub(super) fn send_chord(chord: &str) -> Result<(), DispatchError> {
    send_chord_with_input_mode(chord, None, None)
}

#[cfg(target_os = "windows")]
fn send_chord_with_input_mode(
    chord: &str,
    forced_use_scancode: Option<bool>,
    nullifier: Option<ChordNullifier>,
) -> Result<(), DispatchError> {
    let pre_release_async_modifiers = ModifierAsyncSnapshot::capture();
    let batch = build_chord_inputs(
        chord,
        forced_use_scancode,
        nullifier,
        held_modifier_keyups(&pre_release_async_modifiers),
    )?;
    let input_count = batch.inputs.len();
    let parts: Vec<&str> = chord.split('+').map(|s| s.trim()).collect();
    if parts.is_empty() {
        return Err(DispatchError::new("E_BAD_CHORD", "empty chord"));
    }
    shortcut_telemetry(
        chord,
        "before_send",
        batch.use_scancode,
        batch.modifier_keyup_count,
        input_count,
        None,
        true,
        batch.nullifier_label,
        &pre_release_async_modifiers,
    );
    let sent = send_raw(&batch.inputs);
    let ok = sent as usize == input_count;
    shortcut_telemetry(
        chord,
        "after_send",
        batch.use_scancode,
        batch.modifier_keyup_count,
        input_count,
        Some(sent),
        ok,
        batch.nullifier_label,
        &pre_release_async_modifiers,
    );
    if !ok {
        return Err(DispatchError::new(
            "E_SENDINPUT",
            format!("SendInput sent {}/{} events", sent, input_count),
        ));
    }
    if batch.modifier_keyup_count > 0 {
        tracing::debug!(
            count = batch.modifier_keyup_count,
            chord,
            "Prefixed held-modifier scancode key-ups into injected chord"
        );
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn build_chord_inputs(
    chord: &str,
    forced_use_scancode: Option<bool>,
    nullifier: Option<ChordNullifier>,
    modifier_keyups: Vec<RawInput>,
) -> Result<ChordInputBatch, DispatchError> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_LWIN, VK_RWIN, VK_SNAPSHOT};

    let parts: Vec<&str> = chord.split('+').map(|s| s.trim()).collect();
    if parts.is_empty() {
        return Err(DispatchError::new("E_BAD_CHORD", "empty chord"));
    }
    let key = parts[parts.len() - 1];
    let mods = &parts[..parts.len() - 1];

    let mut vks: Vec<u16> = Vec::with_capacity(parts.len());
    for m in mods {
        let vk = modifier_vk(m)
            .ok_or_else(|| DispatchError::new("E_BAD_MOD", format!("unknown modifier {:?}", m)))?;
        vks.push(vk);
    }
    let key_vk = key_vk(key).ok_or_else(|| {
        DispatchError::new(
            "E_BAD_KEY",
            format!("unknown key {:?} in chord {:?}", key, chord),
        )
    })?;
    vks.push(key_vk);

    // The Windows shell's shortcut handler (win+shift+s snip, win+l lock, etc.)
    // is driven off the low-level keyboard hook, which reads the SCAN CODE — a
    // pure-VK LWIN injection (wScan:0) is silently ignored for Win-combos, so
    // "screenshot" never fired while alt+tab / ctrl+c (VK-only, proven by the
    // paste path) did. PrintScreen is also scan-code-sensitive on modern
    // Windows, especially when bare PrtScn is remapped to screen capture, so
    // screenshot chords stay on the same known-scancode path.
    let default_use_scancode =
        key_vk == VK_SNAPSHOT || vks.iter().any(|&vk| vk == VK_LWIN || vk == VK_RWIN);
    let use_scancode = forced_use_scancode.unwrap_or(default_use_scancode);

    // Press all keys in order, then release in reverse
    let modifier_keyup_count = modifier_keyups.len();
    let nullifier_count = if nullifier.is_some() { 2 } else { 0 };
    let mut inputs = Vec::with_capacity(modifier_keyup_count + vks.len() * 2 + nullifier_count);
    if let Some(nullifier) = nullifier {
        inputs.push(nullifier_input(nullifier, false));
    }
    inputs.extend(modifier_keyups);
    for &vk in &vks {
        inputs.push(key_input(vk, false, use_scancode));
    }
    for &vk in vks.iter().rev() {
        inputs.push(key_input(vk, true, use_scancode));
    }
    let nullifier_label = nullifier.map(ChordNullifier::label);
    if let Some(nullifier) = nullifier {
        inputs.push(nullifier_input(nullifier, true));
    }
    Ok(ChordInputBatch {
        inputs,
        use_scancode,
        modifier_keyup_count,
        nullifier_label,
    })
}

#[cfg(target_os = "windows")]
fn modifier_vk(name: &str) -> Option<u16> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT};
    match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "left ctrl" | "left control" | "right ctrl" | "right control" => {
            Some(VK_CONTROL)
        }
        "shift" | "left shift" | "right shift" => Some(VK_SHIFT),
        "alt" | "option" | "left alt" | "right alt" | "left option" | "right option" => {
            Some(VK_MENU)
        }
        "win" | "windows" | "super" | "meta" | "command" | "cmd" | "left win" | "right win"
        | "left windows" | "right windows" => Some(VK_LWIN),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
pub(super) fn key_vk(name: &str) -> Option<u16> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_BACK, VK_CAPITAL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F10, VK_F11, VK_F12,
        VK_F13, VK_F14, VK_F15, VK_F16, VK_F17, VK_F18, VK_F19, VK_F2, VK_F20, VK_F21, VK_F22,
        VK_F23, VK_F24, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_HOME, VK_INSERT,
        VK_LEFT, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_NEXT,
        VK_NUMLOCK, VK_OEM_MINUS, VK_OEM_PLUS, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SCROLL,
        VK_SNAPSHOT, VK_SPACE, VK_TAB, VK_UP, VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP,
    };
    let n = name.to_ascii_lowercase();
    let n = n.as_str();

    // Single letter a-z → VK 0x41-0x5A
    if n.len() == 1 {
        let c = n.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Some((c.to_ascii_uppercase() as u32) as u16);
        }
        if c.is_ascii_digit() {
            return Some((c as u32) as u16); // '0'..'9' = 0x30..0x39
        }
        // Punctuation we support
        return match c {
            '=' | '+' => Some(VK_OEM_PLUS),
            '-' | '_' => Some(VK_OEM_MINUS),
            _ => None,
        };
    }

    match n {
        "tab" => Some(VK_TAB),
        "enter" | "return" => Some(VK_RETURN),
        "space" => Some(VK_SPACE),
        "esc" | "escape" => Some(VK_ESCAPE),
        "home" => Some(VK_HOME),
        "end" => Some(VK_END),
        "delete" | "del" | "forward delete" => Some(VK_DELETE),
        "insert" => Some(VK_INSERT),
        "backspace" | "back" => Some(VK_BACK),
        "up" | "arrow up" => Some(VK_UP),
        "down" | "arrow down" => Some(VK_DOWN),
        "left" | "arrow left" => Some(VK_LEFT),
        "right" | "arrow right" => Some(VK_RIGHT),
        "page up" | "pageup" => Some(VK_PRIOR),
        "page down" | "pagedown" => Some(VK_NEXT),
        "caps lock" | "capslock" => Some(VK_CAPITAL),
        "num lock" | "numlock" => Some(VK_NUMLOCK),
        "scroll lock" | "scrolllock" => Some(VK_SCROLL),
        "print screen" | "printscreen" => Some(VK_SNAPSHOT),
        "f1" => Some(VK_F1),
        "f2" => Some(VK_F2),
        "f3" => Some(VK_F3),
        "f4" => Some(VK_F4),
        "f5" => Some(VK_F5),
        "f6" => Some(VK_F6),
        "f7" => Some(VK_F7),
        "f8" => Some(VK_F8),
        "f9" => Some(VK_F9),
        "f10" => Some(VK_F10),
        "f11" => Some(VK_F11),
        "f12" => Some(VK_F12),
        "f13" => Some(VK_F13),
        "f14" => Some(VK_F14),
        "f15" => Some(VK_F15),
        "f16" => Some(VK_F16),
        "f17" => Some(VK_F17),
        "f18" => Some(VK_F18),
        "f19" => Some(VK_F19),
        "f20" => Some(VK_F20),
        "f21" => Some(VK_F21),
        "f22" => Some(VK_F22),
        "f23" => Some(VK_F23),
        "f24" => Some(VK_F24),
        "volume up" => Some(VK_VOLUME_UP),
        "volume down" => Some(VK_VOLUME_DOWN),
        "volume mute" | "mute" => Some(VK_VOLUME_MUTE),
        "play/pause media" | "play pause" | "play/pause" => Some(VK_MEDIA_PLAY_PAUSE),
        "next track" => Some(VK_MEDIA_NEXT_TRACK),
        "previous track" | "prev track" => Some(VK_MEDIA_PREV_TRACK),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// App launch — ShellExecuteW (no cmd.exe)
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub(super) fn dispatch_launch_app(name: &str) -> DispatchResult {
    release_held_modifiers()?;
    // `name` is already a validated launch target from `app_launch::resolve`:
    // either a curated alias value ("chrome", "ms-settings:") or the .lnk PATH of
    // an installed Start-Menu app the live scan matched. Curated aliases re-map for
    // safety; a resolved .lnk path is launched as-is.
    let target_name: String = rightkit_voice_commands::command_recognition::app_alias(name)
        .map(|s| s.to_string())
        .unwrap_or_else(|| name.to_string());

    // A settings link is a URL with a registered handler: the owner opens it. Only a resolved
    // `.lnk` or a curated executable alias still goes through ShellExecuteW below.
    if target_name.starts_with("ms-settings:") {
        return rightkit_child_process::open_url(&target_name)
            .map(|()| DispatchOutcome::new("launch_app", format!("name={:?}", target_name)))
            .map_err(|e| {
                DispatchError::new(
                    "E_LAUNCH_FAILED",
                    format!("open_url({:?}): {e}", target_name),
                )
            });
    }

    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let target: Vec<u16> = target_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wide.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns HINSTANCE > 32 on success
    let raw = result as isize;
    if raw <= 32 {
        return Err(DispatchError::new(
            "E_LAUNCH_FAILED",
            format!("ShellExecuteW({:?}) returned {}", target_name, raw),
        ));
    }
    Ok(DispatchOutcome::new(
        "launch_app",
        format!("name={:?}", target_name),
    ))
}

#[cfg(target_os = "macos")]
pub(super) fn dispatch_launch_app(name: &str) -> DispatchResult {
    // `name` is an installed app's display name, already resolved from a live
    // scan in `app_launch::resolve` — so launch it directly. `open -a` matches
    // the registered application; a bad name just fails harmlessly.
    // The owner's `launch_app` takes a bundle identifier, not a display name, so the launcher is
    // an owned detached `open -a` (the same launcher pattern as the owner's `hand_off`): null stdio, reaped by the owner.
    let mut command = rightkit_child_process::OwnedCommand::new("/usr/bin/open");
    command
        .command_mut()
        .arg("-a")
        .arg(name)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
        .spawn_uncontained_detached()
        .map(|_| DispatchOutcome::new("launch_app", format!("name={name:?}")))
        .map_err(|e| DispatchError::new("E_LAUNCH_FAILED", format!("open -a {name:?}: {e}")))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(super) fn dispatch_launch_app(name: &str) -> DispatchResult {
    Err(DispatchError::new(
        "E_UNSUPPORTED",
        format!("LaunchApp not implemented for this OS: {}", name),
    ))
}
