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
        _ => return None,
    })
}

pub const FLAG_SHIFT: u64 = 0x20000;
pub const FLAG_CTRL: u64 = 0x40000;
pub const FLAG_ALT: u64 = 0x80000;
pub const FLAG_CMD: u64 = 0x100000;

/// Parse "cmd+shift+return" into (keycode, flags). Single letters use ANSI codes.
pub fn chord(spec: &str) -> Option<(u16, u64)> {
    let mut flags = 0;
    let mut key = None;
    for part in spec.split('+') {
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
        ('a', 0), ('s', 1), ('d', 2), ('f', 3), ('h', 4), ('g', 5), ('z', 6), ('x', 7), ('c', 8),
        ('v', 9), ('b', 11), ('q', 12), ('w', 13), ('e', 14), ('r', 15), ('y', 16), ('t', 17),
        ('o', 31), ('u', 32), ('i', 34), ('p', 35), ('l', 37), ('j', 38), ('k', 40), ('n', 45),
        ('m', 46),
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
    }
}
