//! Key name -> CDP key event fields. Real `Input.dispatchKeyEvent` input.
use crate::error::{BrowserError, Result};

pub(crate) const ALT: i64 = 1;
pub(crate) const CTRL: i64 = 2;
pub(crate) const META: i64 = 4;
pub(crate) const SHIFT: i64 = 8;

#[derive(Clone, Debug)]
pub(crate) struct KeyDef {
    pub key: String,
    pub code: String,
    pub vk: i64,
    pub text: Option<String>,
}

fn named(key: &str) -> Option<KeyDef> {
    let (code, vk, text): (&str, i64, Option<&str>) = match key {
        "Enter" => ("Enter", 13, Some("\r")),
        "Tab" => ("Tab", 9, None),
        "Escape" | "Esc" => ("Escape", 27, None),
        "Backspace" => ("Backspace", 8, None),
        "Delete" => ("Delete", 46, None),
        "ArrowLeft" => ("ArrowLeft", 37, None),
        "ArrowUp" => ("ArrowUp", 38, None),
        "ArrowRight" => ("ArrowRight", 39, None),
        "ArrowDown" => ("ArrowDown", 40, None),
        "Home" => ("Home", 36, None),
        "End" => ("End", 35, None),
        "PageUp" => ("PageUp", 33, None),
        "PageDown" => ("PageDown", 34, None),
        "Space" | " " => ("Space", 32, Some(" ")),
        _ => return None,
    };
    let key = if key == "Space" { " " } else if key == "Esc" { "Escape" } else { key };
    Some(KeyDef { key: key.into(), code: code.into(), vk, text: text.map(Into::into) })
}

pub(crate) fn char_key(c: char) -> KeyDef {
    let (code, vk) = if c.is_ascii_alphabetic() {
        (format!("Key{}", c.to_ascii_uppercase()), c.to_ascii_uppercase() as i64)
    } else if c.is_ascii_digit() {
        (format!("Digit{c}"), c as i64)
    } else if c == ' ' {
        ("Space".to_string(), 32)
    } else if c == '\n' {
        return named("Enter").unwrap();
    } else {
        (String::new(), 0)
    };
    KeyDef { key: c.to_string(), code, vk, text: Some(c.to_string()) }
}

/// Parse `Control+Shift+a`, `Enter`, `a`. Returns (modifier bitmask, key).
pub(crate) fn parse_chord(spec: &str) -> Result<(i64, Vec<(String, KeyDef)>, KeyDef)> {
    let mut mods = 0;
    let mut mod_keys = Vec::new();
    let parts: Vec<&str> = if spec == "+" { vec!["+"] } else { spec.split('+').collect() };
    let (last, init) = parts.split_last().ok_or_else(|| BrowserError::UnknownKey(spec.into()))?;
    for m in init {
        let (bit, key, code, vk) = match *m {
            "Control" | "Ctrl" => (CTRL, "Control", "ControlLeft", 17),
            "Shift" => (SHIFT, "Shift", "ShiftLeft", 16),
            "Alt" => (ALT, "Alt", "AltLeft", 18),
            "Meta" | "Cmd" | "Command" => (META, "Meta", "MetaLeft", 91),
            _ => return Err(BrowserError::UnknownKey(spec.into())),
        };
        mods |= bit;
        mod_keys.push((key.to_string(), KeyDef { key: key.into(), code: code.into(), vk, text: None }));
    }
    let def = match named(last) {
        Some(d) => d,
        None => {
            let mut chars = last.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => char_key(c),
                _ => return Err(BrowserError::UnknownKey(spec.into())),
            }
        }
    };
    Ok((mods, mod_keys, def))
}
