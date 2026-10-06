//! Chord and key-name parsing/formatting.
//!
//! Donor: HeardRight `heardright_core::chord_format` (modifier tokens, side-pinned PTT aliases)
//! extended with full chord parsing, a portable key table, and per-platform display strings.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Any,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    /// Command on macOS, Windows key elsewhere ("meta"/"cmd"/"command").
    Meta,
    Super,
    Win,
}

impl Modifier {
    pub fn as_str(self) -> &'static str {
        match self {
            Modifier::Ctrl => "Ctrl",
            Modifier::Alt => "Alt",
            Modifier::Shift => "Shift",
            Modifier::Meta => "Meta",
            Modifier::Super => "Super",
            Modifier::Win => "Win",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParsedModifier {
    pub modifier: Modifier,
    pub side: Side,
}

/// Parse a single modifier token (case-insensitive). `None` means "not a modifier".
pub fn parse_modifier_token(token: &str) -> Option<ParsedModifier> {
    let lower = token.to_ascii_lowercase();
    let (modifier, side) = match lower.as_str() {
        "ctrl" | "control" => (Modifier::Ctrl, Side::Any),
        "alt" | "altgr" | "option" | "opt" => (Modifier::Alt, Side::Any),
        "shift" => (Modifier::Shift, Side::Any),
        "meta" | "cmd" | "command" => (Modifier::Meta, Side::Any),
        "super" => (Modifier::Super, Side::Any),
        "win" => (Modifier::Win, Side::Any),
        "lctrl" | "lcontrol" => (Modifier::Ctrl, Side::Left),
        "rctrl" | "rcontrol" => (Modifier::Ctrl, Side::Right),
        "lalt" | "loption" => (Modifier::Alt, Side::Left),
        "ralt" | "roption" => (Modifier::Alt, Side::Right),
        "lshift" => (Modifier::Shift, Side::Left),
        "rshift" => (Modifier::Shift, Side::Right),
        "lwin" => (Modifier::Win, Side::Left),
        "rwin" => (Modifier::Win, Side::Right),
        _ => return None,
    };
    Some(ParsedModifier { modifier, side })
}

/// Second-line validator for single-key push-to-talk aliases: only `lalt`, `ralt`, or `rctrl`.
/// Chord syntax such as `Ctrl+Space` is rejected; use [`parse_chord`] for full chords.
pub fn parse_ptt_key_name(name: &str) -> Result<ParsedModifier, String> {
    let parsed = parse_modifier_token(name).ok_or_else(|| {
        format!("push_to_talk_key '{name}' is not a recognized single-key PTT alias (allowed: 'lalt', 'ralt', 'rctrl')")
    })?;
    let allowed = matches!(
        (parsed.modifier, parsed.side),
        (Modifier::Alt, Side::Left) | (Modifier::Alt, Side::Right) | (Modifier::Ctrl, Side::Right)
    );
    if allowed {
        Ok(parsed)
    } else {
        Err(format!(
            "push_to_talk_key '{name}' must be Left Alt, Right Alt or Right Ctrl (allowed: 'lalt', 'ralt', 'rctrl')"
        ))
    }
}

/// Non-modifier key, identified by canonical name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(&'static str);

impl Key {
    pub fn name(self) -> &'static str {
        self.0
    }

    pub fn parse(token: &str) -> Option<Key> {
        let lower = token.to_ascii_lowercase();
        let lower = match lower.as_str() {
            "esc" => "escape",
            "return" => "enter",
            "del" => "delete",
            "," => "comma",
            "." => "period",
            "/" => "slash",
            ";" => "semicolon",
            "'" => "quote",
            "-" => "minus",
            "=" => "equal",
            "[" => "leftbracket",
            "]" => "rightbracket",
            "\\" => "backslash",
            "`" => "grave",
            other => other,
        };
        TABLE
            .iter()
            .find(|(name, _, _)| *name == lower)
            .map(|(name, _, _)| Key(name))
    }

    /// macOS virtual keycode (ANSI layout, `kVK_*`).
    pub fn mac_keycode(self) -> u16 {
        TABLE.iter().find(|(n, _, _)| *n == self.0).map(|(_, mac, _)| *mac).unwrap_or(0)
    }

    /// Windows virtual-key code (`VK_*`).
    pub fn windows_vk(self) -> u16 {
        TABLE.iter().find(|(n, _, _)| *n == self.0).map(|(_, _, vk)| *vk).unwrap_or(0)
    }

    fn display(self) -> String {
        match self.0 {
            "comma" => ",".into(),
            "period" => ".".into(),
            "slash" => "/".into(),
            "semicolon" => ";".into(),
            "quote" => "'".into(),
            "minus" => "-".into(),
            "equal" => "=".into(),
            "leftbracket" => "[".into(),
            "rightbracket" => "]".into(),
            "backslash" => "\\".into(),
            "grave" => "`".into(),
            name => {
                let mut chars = name.chars();
                match chars.next() {
                    Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
                    None => String::new(),
                }
            }
        }
    }
}

// (name, macOS keycode, Windows VK)
const TABLE: &[(&str, u16, u16)] = &[
    ("a", 0, 0x41), ("s", 1, 0x53), ("d", 2, 0x44), ("f", 3, 0x46), ("h", 4, 0x48), ("g", 5, 0x47),
    ("z", 6, 0x5A), ("x", 7, 0x58), ("c", 8, 0x43), ("v", 9, 0x56), ("b", 11, 0x42), ("q", 12, 0x51),
    ("w", 13, 0x57), ("e", 14, 0x45), ("r", 15, 0x52), ("y", 16, 0x59), ("t", 17, 0x54),
    ("1", 18, 0x31), ("2", 19, 0x32), ("3", 20, 0x33), ("4", 21, 0x34), ("6", 22, 0x36),
    ("5", 23, 0x35), ("equal", 24, 0xBB), ("9", 25, 0x39), ("7", 26, 0x37), ("minus", 27, 0xBD),
    ("8", 28, 0x38), ("0", 29, 0x30), ("rightbracket", 30, 0xDD), ("o", 31, 0x4F), ("u", 32, 0x55),
    ("leftbracket", 33, 0xDB), ("i", 34, 0x49), ("p", 35, 0x50), ("enter", 36, 0x0D), ("l", 37, 0x4C),
    ("j", 38, 0x4A), ("quote", 39, 0xDE), ("k", 40, 0x4B), ("semicolon", 41, 0xBA),
    ("backslash", 42, 0xDC), ("comma", 43, 0xBC), ("slash", 44, 0xBF), ("n", 45, 0x4E),
    ("m", 46, 0x4D), ("period", 47, 0xBE), ("tab", 48, 0x09), ("space", 49, 0x20), ("grave", 50, 0xC0),
    ("backspace", 51, 0x08), ("escape", 53, 0x1B), ("delete", 117, 0x2E),
    ("home", 115, 0x24), ("end", 119, 0x23), ("pageup", 116, 0x21), ("pagedown", 121, 0x22),
    ("left", 123, 0x25), ("right", 124, 0x27), ("down", 125, 0x28), ("up", 126, 0x26),
    ("f1", 122, 0x70), ("f2", 120, 0x71), ("f3", 99, 0x72), ("f4", 118, 0x73), ("f5", 96, 0x74),
    ("f6", 97, 0x75), ("f7", 98, 0x76), ("f8", 100, 0x77), ("f9", 101, 0x78), ("f10", 109, 0x79),
    ("f11", 103, 0x7A), ("f12", 111, 0x7B), ("f13", 105, 0x7C), ("f14", 107, 0x7D),
    ("f15", 113, 0x7E), ("f16", 106, 0x7F), ("f17", 64, 0x80), ("f18", 79, 0x81), ("f19", 80, 0x82),
    ("f20", 90, 0x83),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Windows,
    Other,
}

impl Platform {
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Other
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordError {
    Empty,
    NoKey(String),
    MultipleKeys(String),
    UnknownToken(String),
    DuplicateModifier(String),
}

impl fmt::Display for ChordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChordError::Empty => write!(f, "empty chord"),
            ChordError::NoKey(c) => write!(f, "chord '{c}' has no non-modifier key"),
            ChordError::MultipleKeys(c) => {
                write!(f, "chord '{c}' has more than one non-modifier key")
            }
            ChordError::UnknownToken(t) => write!(f, "unknown key or modifier '{t}'"),
            ChordError::DuplicateModifier(t) => write!(f, "modifier '{t}' repeated"),
        }
    }
}

impl std::error::Error for ChordError {}

/// A parsed chord: a set of modifiers plus exactly one key. Modifiers are normalized (sorted).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    pub modifiers: Vec<Modifier>,
    pub key: Key,
}

pub fn parse_chord(text: &str) -> Result<Chord, ChordError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ChordError::Empty);
    }
    let mut modifiers: Vec<Modifier> = Vec::new();
    let mut key: Option<Key> = None;
    for token in trimmed.split('+').map(str::trim) {
        if token.is_empty() {
            return Err(ChordError::UnknownToken(String::new()));
        }
        if let Some(parsed) = parse_modifier_token(token) {
            if modifiers.contains(&parsed.modifier) {
                return Err(ChordError::DuplicateModifier(token.into()));
            }
            modifiers.push(parsed.modifier);
        } else if let Some(k) = Key::parse(token) {
            if key.is_some() {
                return Err(ChordError::MultipleKeys(trimmed.into()));
            }
            key = Some(k);
        } else {
            return Err(ChordError::UnknownToken(token.into()));
        }
    }
    modifiers.sort();
    let key = key.ok_or_else(|| ChordError::NoKey(trimmed.into()))?;
    Ok(Chord { modifiers, key })
}

impl Chord {
    /// Stable portable spelling, e.g. `Ctrl+Shift+Space`. Round-trips through [`parse_chord`].
    pub fn canonical(&self) -> String {
        self.format(Platform::Other)
    }

    pub fn has(&self, modifier: Modifier) -> bool {
        self.modifiers.contains(&modifier)
    }

    /// Display string. macOS uses the standard glyph order with no separators.
    pub fn format(&self, platform: Platform) -> String {
        match platform {
            Platform::Mac => {
                let mut out = String::new();
                for (m, glyph) in [
                    (Modifier::Ctrl, "\u{2303}"),
                    (Modifier::Alt, "\u{2325}"),
                    (Modifier::Shift, "\u{21E7}"),
                    (Modifier::Meta, "\u{2318}"),
                    (Modifier::Super, "\u{2318}"),
                    (Modifier::Win, "\u{2318}"),
                ] {
                    if self.has(m) {
                        out.push_str(glyph);
                    }
                }
                out.push_str(&self.key.display());
                out
            }
            _ => {
                let mut parts: Vec<String> = Vec::new();
                for m in [
                    Modifier::Ctrl,
                    Modifier::Alt,
                    Modifier::Shift,
                    Modifier::Meta,
                    Modifier::Super,
                    Modifier::Win,
                ] {
                    if self.has(m) {
                        parts.push(match (platform, m) {
                            (Platform::Windows, Modifier::Meta | Modifier::Super) => "Win".into(),
                            _ => m.as_str().into(),
                        });
                    }
                }
                parts.push(self.key.display());
                parts.join("+")
            }
        }
    }
}
