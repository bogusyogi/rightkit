//! US-layout virtual keycodes for named keys (typed text uses Unicode events).

pub fn named(name: &str) -> Option<u16> {
    Some(match name.to_ascii_lowercase().as_str() {
        "return" | "enter" => 0x24,
        "tab" => 0x30,
        "space" => 0x31,
        "delete" | "backspace" => 0x33,
        "escape" | "esc" => 0x35,
        "left" => 0x7B,
        "right" => 0x7C,
        "down" => 0x7D,
        "up" => 0x7E,
        "home" => 0x73,
        "end" => 0x77,
        "pageup" => 0x74,
        "pagedown" => 0x79,
        "forwarddelete" => 0x75,
        "comma" => 0x2B,
        "period" | "dot" => 0x2F,
        "slash" => 0x2C,
        "backslash" => 0x2A,
        "semicolon" => 0x29,
        "quote" | "apostrophe" => 0x27,
        "minus" => 0x1B,
        "equal" | "equals" => 0x18,
        "leftbracket" => 0x21,
        "rightbracket" => 0x1E,
        "grave" | "backtick" => 0x32,
        "f1" => 0x7A,
        "f2" => 0x78,
        "f3" => 0x63,
        "f4" => 0x76,
        "f5" => 0x60,
        "f6" => 0x61,
        "f7" => 0x62,
        "f8" => 0x64,
        "f9" => 0x65,
        "f10" => 0x6D,
        "f11" => 0x67,
        "f12" => 0x6F,
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

fn letter(s: &str) -> Option<u16> {
    const ANSI: &[(char, u16)] = &[
        ('a', 0),
        ('s', 1),
        ('d', 2),
        ('f', 3),
        ('h', 4),
        ('g', 5),
        ('z', 6),
        ('x', 7),
        ('c', 8),
        ('v', 9),
        ('b', 11),
        ('q', 12),
        ('w', 13),
        ('e', 14),
        ('r', 15),
        ('y', 16),
        ('t', 17),
        ('o', 31),
        ('u', 32),
        ('i', 34),
        ('p', 35),
        ('l', 37),
        ('j', 38),
        ('k', 40),
        ('n', 45),
        ('m', 46),
        ('1', 18),
        ('2', 19),
        ('3', 20),
        ('4', 21),
        ('6', 22),
        ('5', 23),
        ('=', 24),
        ('9', 25),
        ('7', 26),
        ('-', 27),
        ('8', 28),
        ('0', 29),
        (']', 30),
        ('[', 33),
        ('\'', 39),
        (';', 41),
        ('\\', 42),
        (',', 43),
        ('/', 44),
        ('.', 47),
        ('`', 50),
    ];
    let c = s.chars().next().filter(|_| s.chars().count() == 1)?;
    ANSI.iter().find(|(l, _)| *l == c).map(|(_, k)| *k)
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
