//! Windows in-process input via WebView2 `CallDevToolsProtocolMethod`
//! (`Input.dispatchMouseEvent` / `Input.dispatchKeyEvent` / `Runtime.evaluate`
//! / `Page.captureScreenshot`). No `SendInput`, no window activation.
//!
//! UNVERIFIED: written against webview2-com 0.39 / windows 0.61 but not yet
//! compiled or run on a Windows host (see design.md section 8).

use super::{Button, Mouse};
use crate::json::str_field;
use std::sync::mpsc::channel;
use std::time::Duration;
use tauri::{Runtime, WebviewWindow};
use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
use windows::core::HSTRING;

fn cdp<R: Runtime>(win: &WebviewWindow<R>, method: &str, params: String) -> Result<String, String> {
    let (tx, rx) = channel::<Result<String, String>>();
    let method = method.to_string();
    win.with_webview(move |pv| unsafe {
        let core = match pv.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Err(e.to_string()));
                return;
            }
        };
        let tx2 = tx.clone();
        let handler =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |res, json| {
                let _ = tx2.send(res.map(|_| json).map_err(|e| e.to_string()));
                Ok(())
            }));
        if let Err(e) = core.CallDevToolsProtocolMethod(
            &HSTRING::from(method),
            &HSTRING::from(params),
            &handler,
        ) {
            let _ = tx.send(Err(e.to_string()));
        }
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(15))
        .map_err(|_| "devtools call timed out".to_string())?
}

fn cdp_mods(flags: u64) -> u32 {
    use crate::keys::*;
    (if flags & FLAG_ALT != 0 { 1 } else { 0 })
        | (if flags & FLAG_CTRL != 0 { 2 } else { 0 })
        | (if flags & FLAG_CMD != 0 { 4 } else { 0 })
        | (if flags & FLAG_SHIFT != 0 { 8 } else { 0 })
}

pub fn mouse<R: Runtime>(
    win: &WebviewWindow<R>,
    kind: Mouse,
    button: Button,
    x: f64,
    y: f64,
    clicks: i64,
    flags: u64,
) -> Result<(), String> {
    // CDP modifiers: Alt=1, Ctrl=2, Meta=4, Shift=8.
    let modifiers = [
        (crate::keys::FLAG_ALT, 1),
        (crate::keys::FLAG_CTRL, 2),
        (crate::keys::FLAG_CMD, 4),
        (crate::keys::FLAG_SHIFT, 8),
    ]
    .iter()
    .filter(|(f, _)| flags & f != 0)
    .map(|(_, m)| m)
    .sum::<u32>();
    let (b, mask) = match button {
        Button::Left => ("left", 1),
        Button::Right => ("right", 2),
    };
    let (ty, button, buttons) = match kind {
        Mouse::Down => ("mousePressed", b, mask),
        Mouse::Up => ("mouseReleased", b, 0),
        Mouse::Move => ("mouseMoved", "none", 0),
        Mouse::Drag => ("mouseMoved", b, mask),
    };
    cdp(win, "Input.dispatchMouseEvent", format!(r#"{{"type":"{ty}","x":{x},"y":{y},"button":"{button}","buttons":{buttons},"clickCount":{clicks},"modifiers":{modifiers}}}"#)).map(|_| ())
}

pub fn wheel<R: Runtime>(
    win: &WebviewWindow<R>,
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
) -> Result<(), String> {
    cdp(
        win,
        "Input.dispatchMouseEvent",
        format!(r#"{{"type":"mouseWheel","x":{x},"y":{y},"deltaX":{dx},"deltaY":{dy}}}"#),
    )
    .map(|_| ())
}

/// Windows VK for a US-layout printable character: letters and digits are their ASCII
/// uppercase value; punctuation uses the `VK_OEM_*` codes (the ASCII value would collide
/// with `VK_SNAPSHOT`, `VK_DELETE`, `VK_LWIN`, `VK_RIGHT`, ...). Shifted symbols map to
/// their base key. Anything else has no virtual key (0).
fn char_vk(c: char) -> u32 {
    match c {
        'a'..='z' | 'A'..='Z' | '0'..='9' => c.to_ascii_uppercase() as u32,
        ';' | ':' => 186,
        '=' | '+' => 187,
        ',' | '<' => 188,
        '-' | '_' => 189,
        '.' | '>' => 190,
        '/' | '?' => 191,
        '`' | '~' => 192,
        '[' | '{' => 219,
        '\\' | '|' => 220,
        ']' | '}' => 221,
        '\'' | '"' => 222,
        '!' => 49,
        '@' => 50,
        '#' => 51,
        '$' => 52,
        '%' => 53,
        '^' => 54,
        '&' => 55,
        '*' => 56,
        '(' => 57,
        ')' => 48,
        _ => 0,
    }
}

/// `code` is the macOS virtual keycode used across the crate; map it to a Windows VK.
fn vk(code: u16, chars: &str) -> u32 {
    match code {
        0x24 => 13,
        0x30 => 9,
        0x31 => 32,
        0x33 => 8,
        0x35 => 27,
        0x75 => 46,
        0x7B => 37,
        0x7E => 38,
        0x7C => 39,
        0x7D => 40,
        0x73 => 36,
        0x77 => 35,
        0x74 => 33,
        0x79 => 34,
        // F1..F12 (mac codes -> VK_F1 = 112 ..).
        0x7A => 112,
        0x78 => 113,
        0x63 => 114,
        0x76 => 115,
        0x60 => 116,
        0x61 => 117,
        0x62 => 118,
        0x64 => 119,
        0x65 => 120,
        0x6D => 121,
        0x67 => 122,
        0x6F => 123,
        _ => crate::keys::ansi_char(code)
            .map(char_vk)
            .filter(|v| *v != 0)
            .unwrap_or_else(|| chars.chars().next().map(char_vk).unwrap_or(0)),
    }
}

/// CDP `key` value for the named keys the crate maps by virtual code; otherwise the character itself.
fn key_value(vk: u32, chars: &str) -> String {
    match vk {
        13 => "Enter".into(),
        9 => "Tab".into(),
        8 => "Backspace".into(),
        27 => "Escape".into(),
        37 => "ArrowLeft".into(),
        38 => "ArrowUp".into(),
        39 => "ArrowRight".into(),
        40 => "ArrowDown".into(),
        36 => "Home".into(),
        35 => "End".into(),
        33 => "PageUp".into(),
        34 => "PageDown".into(),
        _ => chars.to_string(),
    }
}

/// The full `Input.dispatchKeyEvent` payload, serialized by serde_json so quotes,
/// backslashes, control characters and non-ASCII text can never break the JSON.
/// `text` is sent only on key-down of a character-producing key without Ctrl/Meta;
/// control characters other than Enter/Tab are never sent as text.
fn key_event_params(down: bool, code: u16, chars: &str, flags: u64) -> String {
    let vk = vk(code, chars);
    let mods = cdp_mods(flags);
    let producing = |c: char| !c.is_control() || c == '\r' || c == '\t';
    let text: String = if down && mods & 6 == 0 && chars.chars().all(producing) {
        chars.to_string()
    } else {
        String::new()
    };
    let ty = if !down {
        "keyUp"
    } else if text.is_empty() {
        "rawKeyDown"
    } else {
        "keyDown"
    };
    let mut v = serde_json::json!({
        "type": ty,
        "modifiers": mods,
        "windowsVirtualKeyCode": vk,
        "nativeVirtualKeyCode": vk,
        "key": key_value(vk, chars),
    });
    if !text.is_empty() {
        v["text"] = serde_json::Value::String(text.clone());
        v["unmodifiedText"] = serde_json::Value::String(text);
    }
    v.to_string()
}

pub fn key<R: Runtime>(
    win: &WebviewWindow<R>,
    down: bool,
    code: u16,
    chars: &str,
    flags: u64,
) -> Result<(), String> {
    cdp(
        win,
        "Input.dispatchKeyEvent",
        key_event_params(down, code, chars, flags),
    )
    .map(|_| ())
}

/// Insert text exactly as given (any Unicode, emoji, newlines) with `Input.insertText`;
/// per-character key events cannot express characters with no virtual key.
pub fn insert_text<R: Runtime>(win: &WebviewWindow<R>, text: &str) -> Result<(), String> {
    cdp(
        win,
        "Input.insertText",
        serde_json::json!({ "text": text }).to_string(),
    )
    .map(|_| ())
}

pub fn eval<R: Runtime>(win: &WebviewWindow<R>, js: &str) -> Result<String, String> {
    let r = cdp(
        win,
        "Runtime.evaluate",
        serde_json::json!({ "expression": js, "returnByValue": true }).to_string(),
    )?;
    Ok(str_field(&r, "value").unwrap_or_default())
}

pub fn screenshot<R: Runtime>(win: &WebviewWindow<R>) -> Result<Vec<u8>, String> {
    let r = cdp(win, "Page.captureScreenshot", r#"{"format":"png"}"#.into())?;
    let b64 = str_field(&r, "data").ok_or("no screenshot data")?;
    let mut out = Vec::with_capacity(b64.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in b64.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hostile inputs must round-trip through the CDP payload unchanged.
    #[test]
    fn key_payload_is_valid_json_for_hostile_text() {
        for t in [
            "\"",
            "\\",
            "a\"b\\c",
            "\n",
            "é",
            "日本",
            "\u{1f600}",
            "\u{7f}",
        ] {
            let v: serde_json::Value =
                serde_json::from_str(&key_event_params(true, 0xFF, t, 0)).expect("valid JSON");
            let text = v.get("text").and_then(|x| x.as_str());
            if t.chars().all(|c| !c.is_control()) {
                assert_eq!(text, Some(t), "{t:?}");
            } else {
                assert_eq!(text, None, "control text is not inserted: {t:?}");
            }
        }
        let enter: serde_json::Value =
            serde_json::from_str(&key_event_params(true, 0x24, "\r", 0)).unwrap();
        assert_eq!(enter["key"], "Enter");
        assert_eq!(enter["text"], "\r");
    }
}

#[cfg(test)]
mod vk_tests {
    use super::*;
    use crate::keys::{chord, named};

    fn vk_of(spec: &str) -> u32 {
        let (code, _) = chord(spec).unwrap();
        let last = spec
            .rsplit('+')
            .next()
            .filter(|l| !l.is_empty())
            .unwrap_or("+");
        let ch = crate::keys::named_char(last)
            .map(String::from)
            .or_else(|| (last.chars().count() == 1).then(|| last.to_string()))
            .unwrap_or_default();
        vk(code, &ch)
    }

    #[test]
    fn named_keys_map_to_windows_vk() {
        for (n, v) in [
            ("enter", 13),
            ("tab", 9),
            ("space", 32),
            ("backspace", 8),
            ("esc", 27),
            ("left", 37),
            ("up", 38),
            ("right", 39),
            ("down", 40),
            ("home", 36),
            ("end", 35),
            ("pageup", 33),
            ("pagedown", 34),
            ("forwarddelete", 46),
        ] {
            assert_eq!(vk(named(n).unwrap(), ""), v, "{n}");
        }
    }

    #[test]
    fn function_keys_map_to_vk_f1_through_f12() {
        for n in 1..=12u32 {
            assert_eq!(vk_of(&format!("f{n}")), 111 + n, "f{n}");
        }
    }

    #[test]
    fn digits_and_letters_use_ascii_vk() {
        for d in '0'..='9' {
            assert_eq!(vk_of(&format!("cmd+{d}")), d as u32, "{d}");
        }
        for l in 'a'..='z' {
            assert_eq!(
                vk_of(&format!("ctrl+{l}")),
                l.to_ascii_uppercase() as u32,
                "{l}"
            );
        }
    }

    #[test]
    fn punctuation_uses_oem_vk_not_colliding_ascii() {
        for (spec, v) in [
            ("cmd+;", 186),
            ("cmd+=", 187),
            ("cmd+,", 188),
            ("cmd+-", 189),
            ("cmd+.", 190),
            ("cmd+/", 191),
            ("cmd+`", 192),
            ("cmd+[", 219),
            ("cmd+\\", 220),
            ("cmd+]", 221),
            ("cmd+'", 222),
            ("cmd+comma", 188),
            ("cmd+period", 190),
            ("cmd+slash", 191),
            ("cmd+minus", 189),
            ("cmd+equal", 187),
        ] {
            assert_eq!(vk_of(spec), v, "{spec}");
        }
    }

    #[test]
    fn cmd_plus_is_shifted_oem_plus() {
        let (code, flags) = chord("cmd++").unwrap();
        assert_eq!(flags, crate::keys::FLAG_CMD | crate::keys::FLAG_SHIFT);
        assert_eq!(vk(code, "+"), 187);
        assert_eq!(cdp_mods(flags), 4 | 8);
    }

    #[test]
    fn typed_symbols_never_collide_with_navigation_vk() {
        // Unmapped-code typed characters fall back to the character's own VK.
        assert_eq!(vk(0xFF, "!"), 49);
        assert_eq!(vk(0xFF, "%"), 53);
        assert_eq!(vk(0xFF, "'"), 222);
        assert_eq!(vk(0xFF, "("), 57);
        assert_eq!(vk(0xFF, "é"), 0);
    }

    #[test]
    fn modifier_flags_map_to_cdp_modifiers() {
        use crate::keys::*;
        assert_eq!(cdp_mods(FLAG_ALT), 1);
        assert_eq!(cdp_mods(FLAG_CTRL), 2);
        assert_eq!(cdp_mods(FLAG_CMD), 4);
        assert_eq!(cdp_mods(FLAG_SHIFT), 8);
        assert_eq!(cdp_mods(FLAG_ALT | FLAG_CTRL | FLAG_CMD | FLAG_SHIFT), 15);
        assert_eq!(cdp_mods(0), 0);
    }

    #[test]
    fn key_event_payload_is_valid_json_with_oem_vk() {
        let v: serde_json::Value =
            serde_json::from_str(&key_event_params(true, 0x2B, ",", crate::keys::FLAG_CMD))
                .unwrap();
        assert_eq!(v["windowsVirtualKeyCode"], 188);
        assert_eq!(v["modifiers"], 4);
        assert_eq!(v["key"], ",");
        assert!(v.get("text").is_none());
    }
}
