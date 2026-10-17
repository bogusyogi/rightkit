//! What an automated agent never types for the user, and which controls send, buy or publish.
//!
//! Hand-written checks (no regex crate):
//!
//! * [`sensitive_value`]: text that is, or contains, a card number (13 to 19 digits that pass the
//!   Luhn check), an IBAN (mod-97 check), a US Social Security number, a US bank routing number
//!   (ABA checksum), an Aadhaar number, a Canadian SIN (Luhn), a UK National Insurance number or
//!   an Indian PAN. A net for common formats, not a guarantee: an identifier in a format it does
//!   not know passes, which is why the field label is checked too.
//! * [`sensitive_label`]: a field named as a password, a one-time code, a card, a bank account or
//!   a government ID. Typing into one is refused whatever the value is.
//! * [`submit_label`]: a control that sends, buys, pays, publishes or confirms. Such a control is
//!   never covered by a standing grant; every press of it asks.
//!
//! A host refuses these by default: the user types such things themselves or has a password
//! manager or the system's autofill do it, and confirms each one.

/// Why a typed value is refused, or `None`.
///
/// The text is normalised first: digits of any script (Arabic-Indic, Devanagari, fullwidth, the
/// mathematical digits and the other decimal-digit blocks) become ASCII digits, fullwidth letters
/// become ASCII letters, and invisible characters (zero-width spaces and joiners, the soft hyphen,
/// bidi marks) are dropped, so none of them splits a number. Any run of whitespace, dots, slashes,
/// dashes or underscores between digits is one separator, and a number is looked for in every
/// stretch of consecutive digit groups, so a number after other digits (`order 123 4111 1111
/// 1111`) is still found. Not caught: digits spelled out as words, and text that is pasted from
/// a clipboard the caller cannot see.
pub fn sensitive_value(text: &str) -> Option<&'static str> {
    let text = fold(text);
    let text = text.as_str();
    for groups in digit_group_runs(text) {
        // A run of 13 or more digits, however its groups fall: every window of 13 to 19 digits is
        // tried as a card number, so a long run (16 or more digits) cannot hide one inside it.
        let run: Vec<u8> = groups.concat().into_bytes();
        for len in 13..=19usize.min(run.len()) {
            if run.windows(len).any(luhn) {
                return Some("a card number");
            }
        }
        // Every stretch of up to `MAX_SPAN` consecutive groups: a number may follow other digits.
        for start in 0..groups.len() {
            let mut digits = String::new();
            let mut sizes: Vec<usize> = Vec::new();
            for group in groups.iter().skip(start).take(MAX_SPAN) {
                digits.push_str(group);
                sizes.push(group.len());
                if let Some(why) = number_kind(digits.as_bytes(), &sizes) {
                    return Some(why);
                }
            }
        }
    }
    let stripped: Vec<u8> = text
        .bytes()
        .filter(|b| b.is_ascii_alphanumeric())
        .map(|b| b.to_ascii_uppercase())
        .collect();
    if has_iban(&stripped) {
        return Some("an IBAN");
    }
    if has_nino(&stripped) {
        return Some("a National Insurance number");
    }
    if text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| has_pan(&token.to_ascii_uppercase().into_bytes()))
    {
        return Some("a tax ID (PAN)");
    }
    None
}

/// True when the field label names a credential, card, bank account or government ID.
pub fn sensitive_label(label: &str) -> bool {
    let lower = label.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let has = |w: &str| words.contains(&w);
    let pair = |a: &str, b: &str| words.windows(2).any(|p| p[0] == a && p[1] == b);
    [
        "password",
        "passcode",
        "passphrase",
        "secret",
        "token",
        "otp",
        "cvv",
        "cvc",
        "cvn",
        "ssn",
        "pwd",
        "pin",
        "iban",
        "swift",
        "bic",
        "routing",
        "aadhaar",
        "aadhar",
        "passport",
        "nino",
        "cardholder",
        "apikey",
    ]
    .iter()
    .any(|w| has(w))
        || [
            ("card", "number"),
            ("card", "no"),
            ("credit", "card"),
            ("debit", "card"),
            ("account", "number"),
            ("account", "no"),
            ("bank", "account"),
            ("sort", "code"),
            ("security", "code"),
            ("social", "security"),
            ("national", "id"),
            ("national", "insurance"),
            ("tax", "id"),
            ("identity", "number"),
            ("id", "number"),
            ("driver", "license"),
            ("driver", "licence"),
            ("drivers", "license"),
            ("private", "key"),
            ("api", "key"),
            ("one", "time"),
            ("recovery", "code"),
            ("expiry", "date"),
            ("expiration", "date"),
        ]
        .iter()
        .any(|(a, b)| pair(a, b))
}

/// True when the label names a control that sends, buys, pays, publishes or confirms.
pub fn submit_label(label: &str) -> bool {
    let lower = label.to_lowercase();
    lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .any(|w| {
            matches!(
                w,
                "submit"
                    | "send"
                    | "post"
                    | "publish"
                    | "reply"
                    | "buy"
                    | "purchase"
                    | "pay"
                    | "payment"
                    | "checkout"
                    | "order"
                    | "subscribe"
                    | "book"
                    | "reserve"
                    | "donate"
                    | "transfer"
                    | "confirm"
                    | "apply"
                    | "register"
                    | "signup"
                    | "tweet"
                    | "share"
            )
        })
}

/// The key that submits a form: Return or Enter in any spelling a keys string uses.
fn is_submit_key(key: &str) -> bool {
    matches!(
        normal_key(key).as_str(),
        "return" | "enter" | "kpenter" | "numpadenter" | "keypadenter" | "linefeed" | "cr" | "lf"
    )
}

/// One chord split into its modifiers (lower case) and its key (as written; empty for a lone `+`).
fn split_chord(chord: &str) -> (Vec<String>, &str) {
    if chord == "+" {
        return (Vec::new(), "+");
    }
    let mut parts: Vec<&str> = chord.split('+').collect();
    let key = parts.pop().unwrap_or("");
    (parts.iter().map(|m| m.to_lowercase()).collect(), key)
}

/// A key name without case and without `_`, `-` and spaces: `KP_Enter` is `kpenter`.
fn normal_key(key: &str) -> String {
    key.chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

/// A chord that is a shortcut (control, command, alt, option, meta, super or the Windows key) is
/// not typing.
fn is_shortcut(modifiers: &[String]) -> bool {
    modifiers.iter().any(|m| {
        matches!(
            m.as_str(),
            "ctrl"
                | "control"
                | "cmd"
                | "command"
                | "meta"
                | "super"
                | "win"
                | "windows"
                | "alt"
                | "option"
                | "opt"
                | "altgr"
        )
    })
}

/// True when the keys include the key that submits a form (Return or Enter), alone or with a
/// modifier.
pub fn keys_submit(keys: &str) -> bool {
    keys.split_whitespace()
        .any(|chord| is_submit_key(split_chord(chord).1))
}

/// True when the keys press Space (alone or with Shift): on a focused button that presses it.
pub fn keys_space(keys: &str) -> bool {
    keys.split_whitespace().any(|chord| {
        let (modifiers, key) = split_chord(chord);
        !is_shortcut(&modifiers)
            && (key == " " || matches!(normal_key(key).as_str(), "space" | "spacebar" | "spc"))
    })
}

/// True when the keys move the caret or focus (Tab, the arrows, Home, End, Page Up and Page Down),
/// alone or with Shift.
pub fn keys_navigate(keys: &str) -> bool {
    keys.split_whitespace().any(|chord| {
        let (modifiers, key) = split_chord(chord);
        !is_shortcut(&modifiers)
            && matches!(
                normal_key(key).as_str(),
                "tab"
                    | "up"
                    | "down"
                    | "left"
                    | "right"
                    | "arrowup"
                    | "arrowdown"
                    | "arrowleft"
                    | "arrowright"
                    | "home"
                    | "end"
                    | "pageup"
                    | "pagedown"
                    | "pgup"
                    | "pgdn"
            )
    })
}

/// True when one `keys` call both moves (Tab, arrows, Home, End, paging) and types (characters or
/// Space). Typing in a different order than it is sent (digits entered, Home, more digits) can
/// build a card or ID that no piece shows, so such a call is refused; navigation and typing go
/// in separate calls, and each typed result is read back after the act.
pub fn keys_navigate_and_type(keys: &str) -> bool {
    keys_navigate(keys) && !keys_typed(keys).is_empty()
}

/// True when the keys delete text (BackSpace, Delete and their other spellings, with or without a
/// modifier). Deleting can leave digits in a field that now read as a card or ID, so the field is
/// read back after such keys too.
pub fn keys_delete(keys: &str) -> bool {
    keys.split_whitespace().any(|chord| {
        matches!(
            normal_key(split_chord(chord).1).as_str(),
            "backspace" | "bksp" | "bs" | "delete" | "del" | "forwarddelete" | "fndelete"
        )
    })
}

/// Whether the text holds a decimal digit of any script.
pub fn has_digit(text: &str) -> bool {
    fold(text).chars().any(|c| c.is_ascii_digit())
}

/// The character a key name types, or `None` for a key that types nothing here. Single
/// characters stand for themselves; names such as `Digit4`, `Numpad4`, `KP_4`, `key a`, `four`,
/// `space`, `minus`, `period` and `slash` are the characters a card or ID is typed with.
fn key_char(key: &str) -> Option<char> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return fold_char(c);
    }
    let name = normal_key(key);
    let rest = ["digit", "numpad", "keypad", "kp", "num", "key"]
        .iter()
        .find_map(|p| name.strip_prefix(p))
        .unwrap_or(name.as_str());
    let mut rest_chars = rest.chars();
    if let (Some(c), None) = (rest_chars.next(), rest_chars.next()) {
        if c.is_ascii_alphanumeric() {
            return Some(c);
        }
    }
    match rest {
        "zero" => Some('0'),
        "one" => Some('1'),
        "two" => Some('2'),
        "three" => Some('3'),
        "four" => Some('4'),
        "five" => Some('5'),
        "six" => Some('6'),
        "seven" => Some('7'),
        "eight" => Some('8'),
        "nine" => Some('9'),
        "space" | "spacebar" | "spc" => Some(' '),
        "minus" | "hyphen" | "dash" | "subtract" | "hyphenminus" => Some('-'),
        "period" | "dot" | "fullstop" | "decimal" => Some('.'),
        "slash" | "divide" | "forwardslash" => Some('/'),
        "comma" => Some(','),
        _ => None,
    }
}

/// The text a key sequence types: the characters of its chords in order, with a shortcut (a chord
/// with Control, Command, Alt and the like) typing nothing. A sequence typed as chords, even with
/// Shift or as `Digit4` and `Numpad4`, is the same input as `type_text`, so it is checked the same
/// way.
pub fn keys_typed(keys: &str) -> String {
    keys.split_whitespace()
        .filter_map(|chord| {
            let (modifiers, key) = split_chord(chord);
            if is_shortcut(&modifiers) {
                return None;
            }
            key_char(key)
        })
        .collect()
}

/// The longest stretch of consecutive digit groups tried as one number.
const MAX_SPAN: usize = 8;

/// The ASCII digit for a decimal digit of any script, the ASCII letter or punctuation for a
/// fullwidth form, the character itself otherwise, and `None` for an invisible character.
fn fold_char(c: char) -> Option<char> {
    // Invisible: zero-width spaces and joiners, bidi marks, the word joiner, the soft hyphen and
    // the byte order mark. They would split a number without showing.
    if matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    ) {
        return None;
    }
    if c.is_ascii() {
        return Some(c);
    }
    // Fullwidth ASCII (digits, letters, punctuation).
    if ('\u{FF01}'..='\u{FF5E}').contains(&c) {
        return char::from_u32(c as u32 - 0xFEE0);
    }
    // The first digit of each block of ten decimal digits.
    const ZEROES: [u32; 29] = [
        0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6,
        0x0D66, 0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0,
        0x1A80, 0x1A90, 0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620,
    ];
    let code = c as u32;
    for zero in ZEROES
        .iter()
        .chain([0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0].iter())
    {
        if (*zero..*zero + 10).contains(&code) {
            return char::from_digit(code - zero, 10);
        }
    }
    // Mathematical digits, five sets of ten.
    if (0x1D7CE..=0x1D7FF).contains(&code) {
        return char::from_digit((code - 0x1D7CE) % 10, 10);
    }
    // Circled and other enclosed digits are not decimal digits; leave them as they are.
    Some(c)
}

/// `fold_char` over a text.
fn fold(text: &str) -> String {
    text.chars().filter_map(fold_char).collect()
}

/// A separator a person puts inside a number: whitespace, dot, slash, dash (any of them), or
/// underscore.
fn is_separator(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '.' | '/' | '-' | '_' | '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{FE63}'
        )
}

/// The digit groups of each stretch of the (folded) text in which digits are separated only by
/// separators; a group is a maximal unbroken run of digits.
fn digit_group_runs(text: &str) -> Vec<Vec<String>> {
    let mut runs: Vec<Vec<String>> = Vec::new();
    let mut groups: Vec<String> = Vec::new();
    let mut group = String::new();
    let mut flush_run = |groups: &mut Vec<String>, group: &mut String| {
        if !group.is_empty() {
            groups.push(std::mem::take(group));
        }
        if !groups.is_empty() {
            runs.push(std::mem::take(groups));
        }
    };
    for c in text.chars() {
        if c.is_ascii_digit() {
            group.push(c);
        } else if is_separator(c) {
            // Between digits only; a separator after nothing, or after another, closes nothing.
            if !group.is_empty() {
                groups.push(std::mem::take(&mut group));
            }
        } else {
            flush_run(&mut groups, &mut group);
        }
    }
    flush_run(&mut groups, &mut group);
    runs
}

/// What the digits are, given the length of each group they came in. The checks run on the
/// normalised digit string, so compact (`123456789`) and any other grouping are found alike; the
/// canonical grouping (`3-2-4` for a Social Security number) only decides which label a
/// nine-digit value that fits two kinds gets.
fn number_kind(digits: &[u8], sizes: &[usize]) -> Option<&'static str> {
    let n = digits.len();
    if (13..=19).contains(&n) && luhn(digits) {
        return Some("a card number");
    }
    if n == 9 {
        if sizes == [3, 2, 4] && ssn_ranges(digits) {
            return Some("a Social Security number");
        }
        if (sizes == [9] || sizes == [3, 3, 3]) && aba(digits) {
            return Some("a bank routing number");
        }
        if luhn(digits) {
            return Some("a Social Insurance number");
        }
        if ssn_ranges(digits) {
            return Some("a Social Security number");
        }
    }
    if n == 12 && (b'2'..=b'9').contains(&digits[0]) && verhoeff(digits) {
        return Some("an Aadhaar number");
    }
    None
}

/// The Verhoeff checksum (dihedral group D5) that ends an Aadhaar number: valid when the fold of
/// the digits, rightmost first, is zero.
fn verhoeff(digits: &[u8]) -> bool {
    const D: [[u8; 10]; 10] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        [1, 2, 3, 4, 0, 6, 7, 8, 9, 5],
        [2, 3, 4, 0, 1, 7, 8, 9, 5, 6],
        [3, 4, 0, 1, 2, 8, 9, 5, 6, 7],
        [4, 0, 1, 2, 3, 9, 5, 6, 7, 8],
        [5, 9, 8, 7, 6, 0, 4, 3, 2, 1],
        [6, 5, 9, 8, 7, 1, 0, 4, 3, 2],
        [7, 6, 5, 9, 8, 2, 1, 0, 4, 3],
        [8, 7, 6, 5, 9, 3, 2, 1, 0, 4],
        [9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
    ];
    const P: [[u8; 10]; 8] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        [1, 5, 7, 6, 2, 8, 3, 0, 9, 4],
        [5, 8, 0, 3, 7, 9, 6, 1, 4, 2],
        [8, 9, 1, 6, 0, 4, 3, 5, 2, 7],
        [9, 4, 5, 3, 1, 2, 6, 8, 7, 0],
        [4, 2, 8, 6, 5, 7, 3, 9, 0, 1],
        [2, 7, 9, 3, 8, 0, 6, 4, 1, 5],
        [7, 0, 4, 6, 9, 1, 3, 2, 5, 8],
    ];
    let mut c = 0u8;
    for (i, byte) in digits.iter().rev().enumerate() {
        let digit = usize::from(byte - b'0');
        c = D[usize::from(c)][usize::from(P[i % 8][digit])];
    }
    c == 0
}

fn luhn(digits: &[u8]) -> bool {
    let mut sum = 0u32;
    for (index, byte) in digits.iter().rev().enumerate() {
        let mut d = u32::from(byte - b'0');
        if index % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum.is_multiple_of(10)
}

fn ssn_ranges(d: &[u8]) -> bool {
    let area = &d[0..3];
    let group = &d[3..5];
    let serial = &d[5..9];
    area != b"000" && area != b"666" && area[0] != b'9' && group != b"00" && serial != b"0000"
}

/// A US routing number: the two-digit Federal Reserve prefix and the 3-7-1 weighted checksum.
fn aba(d: &[u8]) -> bool {
    let prefix = u32::from(d[0] - b'0') * 10 + u32::from(d[1] - b'0');
    if !(prefix <= 12 || (21..=32).contains(&prefix) || (61..=72).contains(&prefix) || prefix == 80)
    {
        return false;
    }
    let v: Vec<u32> = d.iter().map(|b| u32::from(b - b'0')).collect();
    (3 * (v[0] + v[3] + v[6]) + 7 * (v[1] + v[4] + v[7]) + (v[2] + v[5] + v[8])).is_multiple_of(10)
}

/// An IBAN anywhere in the letters and digits of the text: two letters, two check digits, 11 to
/// 30 more characters, and the ISO 13616 mod-97 check.
fn has_iban(s: &[u8]) -> bool {
    for start in 0..s.len() {
        if start + 15 > s.len() {
            break;
        }
        if !(s[start].is_ascii_uppercase()
            && s[start + 1].is_ascii_uppercase()
            && s[start + 2].is_ascii_digit()
            && s[start + 3].is_ascii_digit())
        {
            continue;
        }
        for len in 15..=34usize.min(s.len() - start) {
            if iban_mod97(&s[start..start + len]) {
                return true;
            }
        }
    }
    false
}

fn iban_mod97(iban: &[u8]) -> bool {
    // Move the first four characters to the end, letters become 10..35, remainder mod 97 is 1.
    let mut remainder = 0u32;
    for &b in iban[4..].iter().chain(iban[..4].iter()) {
        if b.is_ascii_digit() {
            remainder = (remainder * 10 + u32::from(b - b'0')) % 97;
        } else if b.is_ascii_uppercase() {
            remainder = (remainder * 100 + u32::from(b - b'A') + 10) % 97;
        } else {
            return false;
        }
    }
    remainder == 1
}

/// A UK National Insurance number: two letters from the allowed set, six digits, A to D.
fn has_nino(s: &[u8]) -> bool {
    const FIRST: &[u8] = b"ABCEGHJKLMNOPRSTWXYZ";
    const SECOND: &[u8] = b"ABCEGHJKLMNPRSTWXYZ";
    const BARRED: [&[u8]; 6] = [b"BG", b"GB", b"NK", b"KN", b"TN", b"NT"];
    s.windows(9).any(|w| {
        FIRST.contains(&w[0])
            && SECOND.contains(&w[1])
            && !BARRED.contains(&&w[0..2])
            && w[2..8].iter().all(u8::is_ascii_digit)
            && (b'A'..=b'D').contains(&w[8])
    })
}

/// An Indian PAN: five letters (the fourth names the holder type), four digits, a letter. The
/// input is one whole alphanumeric token, so letters and digits of neighbouring words never join.
fn has_pan(s: &[u8]) -> bool {
    s.len() == 10
        && s[..5].iter().all(u8::is_ascii_uppercase)
        && b"PCHFABGJLT".contains(&s[3])
        && s[5..9].iter().all(u8::is_ascii_digit)
        && s[9].is_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_number_passes_luhn_and_is_refused() {
        assert_eq!(
            sensitive_value("4111 1111 1111 1111"),
            Some("a card number")
        );
        assert_eq!(
            sensitive_value("order 4111-1111-1111-1111 now"),
            Some("a card number")
        );
    }

    #[test]
    fn luhn_rejects_a_bad_check_digit() {
        assert!(luhn(b"4111111111111111"));
        assert!(!luhn(b"4111111111111112"));
    }

    #[test]
    fn social_security_number_ranges() {
        assert_eq!(
            sensitive_value("123-45-6789"),
            Some("a Social Security number")
        );
        assert!(!ssn_ranges(b"666456789"));
        assert!(!ssn_ranges(b"000456789"));
    }

    #[test]
    fn compact_and_grouped_government_ids() {
        // Compact SSN, grouped SSN, Luhn-valid SIN in both forms.
        assert_eq!(
            sensitive_value("123456789"),
            Some("a Social Security number")
        );
        assert_eq!(
            sensitive_value("123-45-6789"),
            Some("a Social Security number")
        );
        assert_eq!(
            sensitive_value("046454286"),
            Some("a Social Insurance number")
        );
        assert_eq!(
            sensitive_value("046-454-286"),
            Some("a Social Insurance number")
        );
        // SSN with an invalid range is not flagged in either form (and fails Luhn and ABA).
        assert_eq!(sensitive_value("666456789"), None);
        assert_eq!(sensitive_value("666-45-6789"), None);
    }

    #[test]
    fn aadhaar_requires_verhoeff_in_any_grouping() {
        assert!(verhoeff(b"234123412346"));
        assert!(!verhoeff(b"234123412347"));
        assert_eq!(sensitive_value("234123412346"), Some("an Aadhaar number"));
        assert_eq!(sensitive_value("2341 2341 2346"), Some("an Aadhaar number"));
        assert_eq!(sensitive_value("234123412347"), None);
        assert_eq!(sensitive_value("2341 2341 2347"), None);
    }

    #[test]
    fn aba_routing_number_checksum() {
        assert_eq!(sensitive_value("021000021"), Some("a bank routing number"));
        assert!(!aba(b"021000022"));
    }

    #[test]
    fn iban_mod97_check() {
        assert!(iban_mod97(b"GB82WEST12345698765432"));
        assert!(!iban_mod97(b"GB82WEST12345698765433"));
    }

    #[test]
    fn pan_is_matched_only_as_a_whole_token() {
        assert_eq!(sensitive_value("PAN ABCPE1234F"), Some("a tax ID (PAN)"));
        assert_eq!(sensitive_value("Meetingat1030on"), None);
    }

    #[test]
    fn plain_text_is_not_sensitive() {
        assert_eq!(sensitive_value("Hello, world"), None);
        assert_eq!(sensitive_value("Meeting at 10:30 on 2026-10-18"), None);
    }

    #[test]
    fn sensitive_labels() {
        assert!(sensitive_label("Card number"));
        assert!(sensitive_label("One-time code"));
        assert!(sensitive_label("Password"));
        assert!(!sensitive_label("Username"));
    }

    #[test]
    fn submit_labels() {
        assert!(submit_label("Place order"));
        assert!(submit_label("Send"));
        assert!(!submit_label("Search"));
    }

    #[test]
    fn navigation_mixed_with_typing_is_refused() {
        assert!(keys_navigate_and_type("Tab 4 1 1 1"));
        assert!(!keys_navigate_and_type("Tab"));
        assert!(!keys_navigate_and_type("ctrl+c"));
    }
}
