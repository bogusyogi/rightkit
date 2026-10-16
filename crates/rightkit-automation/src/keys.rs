//! Platform-neutral key chords with per-platform code tables. Pure and tested on
//! every host. `cmd+k`, `ctrl+shift+tab`, `mod+a` (cmd on macOS, ctrl elsewhere).

use crate::model::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub meta: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub fn any(&self) -> bool {
        self.meta || self.ctrl || self.alt || self.shift
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    Return,
    Tab,
    Space,
    Backspace,
    Delete,
    Escape,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Named(Named),
    /// One ASCII letter, digit, or punctuation character (lowercase).
    Char(char),
    /// F1..=F12.
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

fn named(s: &str) -> Option<Named> {
    Some(match s {
        "return" | "enter" => Named::Return,
        "tab" => Named::Tab,
        "space" => Named::Space,
        "backspace" => Named::Backspace,
        "delete" | "del" | "forwarddelete" => Named::Delete,
        "escape" | "esc" => Named::Escape,
        "left" => Named::Left,
        "right" => Named::Right,
        "up" => Named::Up,
        "down" => Named::Down,
        "home" => Named::Home,
        "end" => Named::End,
        "pageup" | "pgup" => Named::PageUp,
        "pagedown" | "pgdn" => Named::PageDown,
        _ => return None,
    })
}

/// Parse one chord. `primary_meta` decides what `mod`/`primary` means.
pub fn parse_chord(spec: &str, primary_meta: bool) -> Result<Chord> {
    let mut mods = Mods::default();
    let mut key = None;
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(Error::Invalid("empty chord".into()));
    }
    // A trailing "+" is the plus key itself ("ctrl++").
    let (head, tail_plus) = match spec.strip_suffix('+') {
        Some(h) if spec.len() > 1 => (h.trim_end_matches('+'), true),
        _ => (spec, false),
    };
    let mut parts: Vec<&str> = if head.is_empty() {
        vec![]
    } else {
        head.split('+').collect()
    };
    if tail_plus {
        key = Some(Key::Char('='));
        // plus is shift+= on US layouts
        mods.shift = true;
    }
    for p in parts.drain(..) {
        let l = p.trim().to_ascii_lowercase();
        match l.as_str() {
            "cmd" | "command" | "meta" | "super" | "win" | "windows" => mods.meta = true,
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "option" | "opt" => mods.alt = true,
            "shift" => mods.shift = true,
            "mod" | "primary" => {
                if primary_meta {
                    mods.meta = true
                } else {
                    mods.ctrl = true
                }
            }
            other => {
                if key.is_some() {
                    return Err(Error::Invalid(format!("two keys in chord {spec:?}")));
                }
                key = Some(
                    parse_key(other)
                        .ok_or_else(|| Error::Invalid(format!("unknown key {other:?}")))?,
                );
            }
        }
    }
    key.map(|key| Chord { mods, key })
        .ok_or_else(|| Error::Invalid(format!("no key in chord {spec:?}")))
}

fn parse_key(s: &str) -> Option<Key> {
    if let Some(n) = named(s) {
        return Some(Key::Named(n));
    }
    if let Some(num) = s.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        if (1..=12).contains(&num) {
            return Some(Key::F(num));
        }
    }
    let mut it = s.chars();
    let c = it.next()?;
    if it.next().is_some() {
        return None;
    }
    (c.is_ascii_alphanumeric() || ";=,-./`[]\\'".contains(c))
        .then_some(Key::Char(c.to_ascii_lowercase()))
}

/// Space-separated sequence of chords.
pub fn parse_sequence(spec: &str, primary_meta: bool) -> Result<Vec<Chord>> {
    let v: Result<Vec<Chord>> = spec
        .split_whitespace()
        .map(|c| parse_chord(c, primary_meta))
        .collect();
    let v = v?;
    if v.is_empty() {
        return Err(Error::Invalid("empty key sequence".into()));
    }
    Ok(v)
}

/// macOS ANSI virtual keycode.
pub fn mac_keycode(key: Key) -> Option<u16> {
    Some(match key {
        Key::Named(n) => match n {
            Named::Return => 36,
            Named::Tab => 48,
            Named::Space => 49,
            Named::Backspace => 51,
            Named::Delete => 117,
            Named::Escape => 53,
            Named::Left => 123,
            Named::Right => 124,
            Named::Down => 125,
            Named::Up => 126,
            Named::Home => 115,
            Named::End => 119,
            Named::PageUp => 116,
            Named::PageDown => 121,
        },
        Key::F(n) => *[122u16, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111]
            .get(usize::from(n).checked_sub(1)?)?,
        Key::Char(c) => match c {
            'a' => 0,
            's' => 1,
            'd' => 2,
            'f' => 3,
            'h' => 4,
            'g' => 5,
            'z' => 6,
            'x' => 7,
            'c' => 8,
            'v' => 9,
            'b' => 11,
            'q' => 12,
            'w' => 13,
            'e' => 14,
            'r' => 15,
            'y' => 16,
            't' => 17,
            '1' => 18,
            '2' => 19,
            '3' => 20,
            '4' => 21,
            '6' => 22,
            '5' => 23,
            '=' => 24,
            '9' => 25,
            '7' => 26,
            '-' => 27,
            '8' => 28,
            '0' => 29,
            ']' => 30,
            'o' => 31,
            'u' => 32,
            '[' => 33,
            'i' => 34,
            'p' => 35,
            'l' => 37,
            'j' => 38,
            '\'' => 39,
            'k' => 40,
            ';' => 41,
            '\\' => 42,
            ',' => 43,
            '/' => 44,
            'n' => 45,
            'm' => 46,
            '.' => 47,
            '`' => 50,
            _ => return None,
        },
    })
}

/// Windows virtual-key code.
pub fn win_vk(key: Key) -> Option<u16> {
    Some(match key {
        Key::Named(n) => match n {
            Named::Return => 0x0D,
            Named::Tab => 0x09,
            Named::Space => 0x20,
            Named::Backspace => 0x08,
            Named::Delete => 0x2E,
            Named::Escape => 0x1B,
            Named::Left => 0x25,
            Named::Up => 0x26,
            Named::Right => 0x27,
            Named::Down => 0x28,
            Named::Home => 0x24,
            Named::End => 0x23,
            Named::PageUp => 0x21,
            Named::PageDown => 0x22,
        },
        Key::F(n) if (1..=12).contains(&n) => 0x6F + u16::from(n),
        Key::F(_) => return None,
        Key::Char(c) if c.is_ascii_lowercase() => 0x41 + (c as u16 - 'a' as u16),
        Key::Char(c) if c.is_ascii_digit() => 0x30 + (c as u16 - '0' as u16),
        Key::Char(c) => match c {
            ';' => 0xBA,
            '=' => 0xBB,
            ',' => 0xBC,
            '-' => 0xBD,
            '.' => 0xBE,
            '/' => 0xBF,
            '`' => 0xC0,
            '[' => 0xDB,
            '\\' => 0xDC,
            ']' => 0xDD,
            '\'' => 0xDE,
            _ => return None,
        },
    })
}

/// macOS CGEventFlags bits.
pub fn mac_flags(m: Mods) -> u64 {
    (if m.shift { 0x20000 } else { 0 })
        | (if m.ctrl { 0x40000 } else { 0 })
        | (if m.alt { 0x80000 } else { 0 })
        | (if m.meta { 0x100000 } else { 0 })
}

/// Text a bare chord would insert (for UTF-16 keyboard events and tests).
pub fn inserted_char(c: Chord) -> Option<char> {
    if c.mods.meta || c.mods.ctrl || c.mods.alt {
        return None;
    }
    match c.key {
        Key::Named(Named::Return) => Some('\r'),
        Key::Named(Named::Tab) => Some('\t'),
        Key::Named(Named::Space) => Some(' '),
        Key::Char(ch) => Some(if c.mods.shift {
            ch.to_ascii_uppercase()
        } else {
            ch
        }),
        _ => None,
    }
}

/// Raw AppKit / `CGEvent` key data for callers that post native events directly (the
/// QA driver and its embedded server): a name or spec becomes a macOS virtual keycode
/// plus `CGEventFlags`/`NSEventModifierFlags` bits, with no [`Chord`] in between.
///
/// The code tables are not repeated here: every keycode comes from [`mac_keycode`], so
/// this module and the typed chords cannot drift. What differs from [`parse_chord`] is
/// the name grammar, kept as the QA driver had it:
/// - `delete` is the macOS Delete key (backspace, `0x33`); `forwarddelete` is forward
///   delete. [`parse_chord`] reads `delete` as forward delete.
/// - punctuation has names (`comma`, `period`, `slash`, ...), and `cmd++` is cmd+shift+`=`.
/// - no `meta`/`super`/`win`/`mod`/`primary` modifier aliases, and no `del`/`pgup`/`pgdn`.
pub mod appkit {
    use super::{mac_keycode, Key, Named};

    /// Characters with an ANSI keycode, in the order `ansi_char` searches them.
    const ANSI_CHARS: &str = "abcdefghijklmnopqrstuvwxyz0123456789=-][';\\,/.`";

    /// Keycode for a named key (`return`, `left`, `f5`, `comma`, ...). Case-insensitive.
    pub fn named(name: &str) -> Option<u16> {
        let n = name.to_ascii_lowercase();
        let key = match n.as_str() {
            "return" | "enter" => Key::Named(Named::Return),
            "tab" => Key::Named(Named::Tab),
            "space" => Key::Named(Named::Space),
            "delete" | "backspace" => Key::Named(Named::Backspace),
            "escape" | "esc" => Key::Named(Named::Escape),
            "left" => Key::Named(Named::Left),
            "right" => Key::Named(Named::Right),
            "down" => Key::Named(Named::Down),
            "up" => Key::Named(Named::Up),
            "home" => Key::Named(Named::Home),
            "end" => Key::Named(Named::End),
            "pageup" => Key::Named(Named::PageUp),
            "pagedown" => Key::Named(Named::PageDown),
            "forwarddelete" => Key::Named(Named::Delete),
            other => {
                let f = other
                    .strip_prefix('f')
                    .and_then(|d| d.parse::<u8>().ok())
                    .filter(|k| (1..=12).contains(k) && other == format!("f{k}"));
                match f {
                    Some(k) => Key::F(k),
                    None => Key::Char(named_char(other)?),
                }
            }
        };
        mac_keycode(key)
    }

    /// AppKit `NSEventModifierFlagNumericPad` / `NSEventModifierFlagFunction`.
    pub const FLAG_NUMPAD: u64 = 0x200000;
    pub const FLAG_FUNCTION: u64 = 0x800000;

    /// AppKit delivers navigation and F-keys as private-use function characters with
    /// `NSEventModifierFlagFunction` (arrows also with `NumericPad`). WebKit derives
    /// `KeyboardEvent.key` from those characters; without them it reports "Unidentified".
    pub fn function_char(name: &str) -> Option<(char, u64)> {
        let arrow = FLAG_FUNCTION | FLAG_NUMPAD;
        let (c, f) = match name.to_ascii_lowercase().as_str() {
            "up" => ('\u{F700}', arrow),
            "down" => ('\u{F701}', arrow),
            "left" => ('\u{F702}', arrow),
            "right" => ('\u{F703}', arrow),
            "forwarddelete" => ('\u{F728}', FLAG_FUNCTION),
            "home" => ('\u{F729}', FLAG_FUNCTION),
            "end" => ('\u{F72B}', FLAG_FUNCTION),
            "pageup" => ('\u{F72C}', FLAG_FUNCTION),
            "pagedown" => ('\u{F72D}', FLAG_FUNCTION),
            f if f.len() >= 2 && f.starts_with('f') => {
                let n: u32 = f[1..].parse().ok().filter(|n| (1..=12).contains(n))?;
                (char::from_u32(0xF703 + n)?, FLAG_FUNCTION)
            }
            _ => return None,
        };
        Some((c, f))
    }

    /// The printed character of a named punctuation key (`comma` -> `,`).
    pub fn named_char(name: &str) -> Option<char> {
        Some(match name.to_ascii_lowercase().as_str() {
            "comma" => ',',
            "period" | "dot" => '.',
            "slash" => '/',
            "backslash" => '\\',
            "semicolon" => ';',
            "quote" | "apostrophe" => '\'',
            "minus" => '-',
            "equal" | "equals" => '=',
            "leftbracket" => '[',
            "rightbracket" => ']',
            "grave" | "backtick" => '`',
            _ => return None,
        })
    }

    pub const FLAG_SHIFT: u64 = 0x20000;
    pub const FLAG_CTRL: u64 = 0x40000;
    pub const FLAG_ALT: u64 = 0x80000;
    pub const FLAG_CMD: u64 = 0x100000;

    /// Parse "cmd+shift+return" into (keycode, flags). Single letters, digits and
    /// punctuation (`cmd+,`, `cmd+/`, `cmd+[`) use ANSI codes; a trailing `+` is the
    /// plus key (`cmd++` = cmd+shift+equal).
    pub fn chord(spec: &str) -> Option<(u16, u64)> {
        let mut flags = 0;
        let mut key = None;
        let spec = match spec.strip_suffix("++") {
            Some(rest) => {
                flags |= FLAG_SHIFT;
                key = Some(0x18);
                rest
            }
            None if spec == "+" => return Some((0x18, FLAG_SHIFT)),
            None => spec,
        };
        let plus_key = key.is_some();
        for part in spec.split('+').filter(|p| !p.is_empty() || !plus_key) {
            match part.to_ascii_lowercase().as_str() {
                "cmd" | "command" => flags |= FLAG_CMD,
                "shift" => flags |= FLAG_SHIFT,
                "ctrl" | "control" => flags |= FLAG_CTRL,
                "alt" | "option" => flags |= FLAG_ALT,
                other => key = Some(named(other).or_else(|| letter(other))?),
            }
        }
        key.map(|k| (k, flags))
    }

    /// The unshifted character a US-layout ANSI keycode produces (letters, digits, punctuation).
    pub fn ansi_char(code: u16) -> Option<char> {
        ANSI_CHARS
            .chars()
            .find(|c| mac_keycode(Key::Char(*c)) == Some(code))
    }

    fn letter(s: &str) -> Option<u16> {
        let c = s.chars().next().filter(|_| s.chars().count() == 1)?;
        if ANSI_CHARS.contains(c) {
            mac_keycode(Key::Char(c))
        } else {
            None
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::keys::{mac_flags, parse_chord, Mods};

        #[test]
        fn parses_chords() {
            assert_eq!(chord("cmd+a"), Some((0, FLAG_CMD)));
            assert_eq!(chord("shift+tab"), Some((0x30, FLAG_SHIFT)));
            assert_eq!(chord("nope"), None);
            assert_eq!(chord("cmd+,"), Some((0x2B, FLAG_CMD)));
            assert_eq!(chord("cmd+comma"), Some((0x2B, FLAG_CMD)));
            assert_eq!(chord("cmd+shift+/"), Some((0x2C, FLAG_CMD | FLAG_SHIFT)));
            assert_eq!(chord("cmd+["), Some((0x21, FLAG_CMD)));
            assert_eq!(chord("cmd+1"), Some((18, FLAG_CMD)));
            assert_eq!(chord("cmd++"), Some((0x18, FLAG_CMD | FLAG_SHIFT)));
            assert_eq!(chord("f5"), Some((0x60, 0)));
            assert_eq!(chord("cmd+."), Some((0x2F, FLAG_CMD)));
        }

        #[test]
        fn function_keys_carry_appkit_characters() {
            let arrow = FLAG_FUNCTION | FLAG_NUMPAD;
            assert_eq!(function_char("right"), Some(('\u{F703}', arrow)));
            assert_eq!(function_char("Up"), Some(('\u{F700}', arrow)));
            assert_eq!(function_char("home"), Some(('\u{F729}', FLAG_FUNCTION)));
            assert_eq!(function_char("pagedown"), Some(('\u{F72D}', FLAG_FUNCTION)));
            assert_eq!(
                function_char("forwarddelete"),
                Some(('\u{F728}', FLAG_FUNCTION))
            );
            assert_eq!(function_char("f1"), Some(('\u{F704}', FLAG_FUNCTION)));
            assert_eq!(function_char("f12"), Some(('\u{F70F}', FLAG_FUNCTION)));
            assert_eq!(function_char("f13"), None);
            assert_eq!(function_char("a"), None);
            assert_eq!(named_char("comma"), Some(','));
        }

        /// The values the QA driver's own table used to hold: deriving them from
        /// `mac_keycode` must not have moved a single code.
        #[test]
        fn named_codes_match_the_retired_qa_driver_table() {
            let table: &[(&str, u16)] = &[
                ("return", 0x24),
                ("enter", 0x24),
                ("tab", 0x30),
                ("space", 0x31),
                ("delete", 0x33),
                ("backspace", 0x33),
                ("escape", 0x35),
                ("esc", 0x35),
                ("left", 0x7B),
                ("right", 0x7C),
                ("down", 0x7D),
                ("up", 0x7E),
                ("home", 0x73),
                ("end", 0x77),
                ("pageup", 0x74),
                ("pagedown", 0x79),
                ("forwarddelete", 0x75),
                ("comma", 0x2B),
                ("period", 0x2F),
                ("dot", 0x2F),
                ("slash", 0x2C),
                ("backslash", 0x2A),
                ("semicolon", 0x29),
                ("quote", 0x27),
                ("apostrophe", 0x27),
                ("minus", 0x1B),
                ("equal", 0x18),
                ("equals", 0x18),
                ("leftbracket", 0x21),
                ("rightbracket", 0x1E),
                ("grave", 0x32),
                ("backtick", 0x32),
                ("f1", 0x7A),
                ("f2", 0x78),
                ("f3", 0x63),
                ("f4", 0x76),
                ("f5", 0x60),
                ("f6", 0x61),
                ("f7", 0x62),
                ("f8", 0x64),
                ("f9", 0x65),
                ("f10", 0x6D),
                ("f11", 0x67),
                ("f12", 0x6F),
            ];
            for (name, code) in table {
                assert_eq!(named(name), Some(*code), "{name}");
                assert_eq!(named(&name.to_ascii_uppercase()), Some(*code), "{name}");
            }
            for bad in ["f0", "f13", "f01", "fx", "nope", "a", ""] {
                assert_eq!(named(bad), None, "{bad}");
            }
        }

        #[test]
        fn ansi_letters_round_trip() {
            for c in "abcdefghijklmnopqrstuvwxyz0123456789=-][';\\,/.`".chars() {
                let code = mac_keycode(Key::Char(c)).unwrap();
                assert_eq!(ansi_char(code), Some(c));
                assert_eq!(chord(&c.to_string()), Some((code, 0)), "{c}");
            }
            assert_eq!(ansi_char(0x24), None, "return is not an ANSI character key");
        }

        #[test]
        fn flags_agree_with_the_typed_chord_flags() {
            let all = Mods {
                meta: true,
                ctrl: true,
                alt: true,
                shift: true,
            };
            assert_eq!(mac_flags(all), FLAG_CMD | FLAG_CTRL | FLAG_ALT | FLAG_SHIFT);
        }

        #[test]
        fn delete_differs_from_the_typed_grammar_by_design() {
            assert_eq!(named("delete"), Some(0x33), "macOS Delete key is backspace");
            assert_eq!(
                parse_chord("delete", true).unwrap().key,
                Key::Named(Named::Delete),
                "the typed grammar reads delete as forward delete"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chords_and_sequences() {
        let c = parse_chord("cmd+shift+k", true).unwrap();
        assert!(c.mods.meta && c.mods.shift && !c.mods.ctrl);
        assert_eq!(c.key, Key::Char('k'));
        assert_eq!(
            parse_chord("mod+a", true).unwrap().mods,
            Mods {
                meta: true,
                ..Default::default()
            }
        );
        assert_eq!(
            parse_chord("mod+a", false).unwrap().mods,
            Mods {
                ctrl: true,
                ..Default::default()
            }
        );
        assert_eq!(parse_sequence("cmd+a delete", true).unwrap().len(), 2);
        assert_eq!(parse_chord("F5", true).unwrap().key, Key::F(5));
        assert!(parse_chord("cmd+", true).is_ok());
        assert!(parse_chord("nope+nope", true).is_err());
        assert!(parse_chord("", true).is_err());
        assert!(parse_sequence("   ", true).is_err());
    }

    #[test]
    fn tables_cover_the_same_keys() {
        for c in "abcdefghijklmnopqrstuvwxyz0123456789;=,-./`[]\\'".chars() {
            assert!(mac_keycode(Key::Char(c)).is_some(), "mac {c}");
            assert!(win_vk(Key::Char(c)).is_some(), "win {c}");
        }
        for n in 1..=12u8 {
            assert!(mac_keycode(Key::F(n)).is_some());
            assert_eq!(win_vk(Key::F(n)), Some(0x6F + u16::from(n)));
        }
        assert_eq!(mac_keycode(Key::Char('a')), Some(0));
        assert_eq!(mac_keycode(Key::Named(Named::Return)), Some(36));
        assert_eq!(win_vk(Key::Char('a')), Some(0x41));
        assert_eq!(win_vk(Key::Char('0')), Some(0x30));
        assert_eq!(win_vk(Key::Named(Named::Left)), Some(0x25));
        // every mac keycode is unique
        let mut seen = std::collections::HashSet::new();
        for c in "abcdefghijklmnopqrstuvwxyz0123456789;=,-./`[]\\'".chars() {
            assert!(seen.insert(mac_keycode(Key::Char(c)).unwrap()), "dup {c}");
        }
    }

    #[test]
    fn flags_and_inserted_chars() {
        assert_eq!(
            mac_flags(Mods {
                meta: true,
                shift: true,
                ..Default::default()
            }),
            0x100000 | 0x20000
        );
        assert_eq!(
            inserted_char(parse_chord("shift+a", true).unwrap()),
            Some('A')
        );
        assert_eq!(inserted_char(parse_chord("cmd+a", true).unwrap()), None);
        assert_eq!(
            inserted_char(parse_chord("return", true).unwrap()),
            Some('\r')
        );
    }
}
