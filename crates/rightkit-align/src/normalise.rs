//! Pronunciation normalisation. Script tokens and ASR words pass through the
//! same function, so a written form ("2026", "don't") and its spoken form
//! ("two thousand twenty six", "do not") compare equal on both sides.
//!
//! One token can expand to several spoken units. Alignment works on units, and
//! every unit remembers the token it came from.

/// Common contractions, split into the spoken forms an ASR engine emits.
const CONTRACTIONS: &[(&str, &str)] = &[
    ("don't", "do not"),
    ("doesn't", "does not"),
    ("didn't", "did not"),
    ("can't", "can not"),
    ("won't", "will not"),
    ("isn't", "is not"),
    ("aren't", "are not"),
    ("wasn't", "was not"),
    ("weren't", "were not"),
    ("haven't", "have not"),
    ("hasn't", "has not"),
    ("hadn't", "had not"),
    ("shouldn't", "should not"),
    ("wouldn't", "would not"),
    ("couldn't", "could not"),
    ("i'm", "i am"),
    ("i've", "i have"),
    ("i'll", "i will"),
    ("i'd", "i would"),
    ("you're", "you are"),
    ("we're", "we are"),
    ("they're", "they are"),
    ("let's", "let us"),
];

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];
const DIGITS: [&str; 10] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];
const SCALES: [(u64, &str); 4] = [
    (1_000_000_000_000, "trillion"),
    (1_000_000_000, "billion"),
    (1_000_000, "million"),
    (1_000, "thousand"),
];

/// Numbers at or above this are spelled digit by digit, so the scale table stays exact.
const SPELL_LIMIT: u64 = 1_000_000_000_000_000;

/// Expands one whitespace-delimited script token or ASR word into spoken units.
/// Returns an empty vector for punctuation-only input.
pub fn units(token: &str) -> Vec<String> {
    pieces(token)
        .iter()
        .flat_map(|piece| expand_piece(piece))
        .collect()
}

/// Splits on separators, keeping decimal points between digits and apostrophes
/// inside words. Thousands separators between digits are dropped.
fn pieces(raw: &str) -> Vec<String> {
    let chars: Vec<char> = raw
        .chars()
        .map(|c| {
            if c == '\u{2018}' || c == '\u{2019}' {
                '\''
            } else {
                c
            }
        })
        .collect();
    let mut out = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let digit_before = i > 0 && chars[i - 1].is_ascii_digit();
        let digit_after = chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
        if c.is_alphanumeric() || c == '\'' {
            current.extend(c.to_lowercase());
        } else if c == '.' && digit_before && digit_after {
            current.push('.');
        } else if c == ',' && digit_before && digit_after {
            // Thousands separator: "1,000" reads as one number.
        } else {
            flush(&mut current, &mut out);
            if c == '%' || c == '&' {
                out.push(c.to_string());
            }
        }
    }
    flush(&mut current, &mut out);
    out
}

fn flush(current: &mut String, out: &mut Vec<String>) {
    if !current.is_empty() {
        out.push(std::mem::take(current));
    }
}

fn expand_piece(piece: &str) -> Vec<String> {
    match piece {
        "%" => return vec!["percent".into()],
        "&" => return vec!["and".into()],
        _ => {}
    }
    let core = piece.trim_matches('\'');
    if core.is_empty() {
        return Vec::new();
    }
    if let Some((_, spoken)) = CONTRACTIONS.iter().find(|(written, _)| *written == core) {
        return spoken.split(' ').map(str::to_string).collect();
    }
    if is_number(core) {
        return number_words(core);
    }
    vec![core.to_string()]
}

fn is_number(core: &str) -> bool {
    let digits = core
        .chars()
        .filter(|c| *c != '.')
        .all(|c| c.is_ascii_digit());
    let dots = core.matches('.').count();
    digits
        && dots <= 1
        && core.chars().any(|c| c.is_ascii_digit())
        && !core.starts_with('.')
        && !core.ends_with('.')
}

fn number_words(core: &str) -> Vec<String> {
    match core.split_once('.') {
        Some((whole, fraction)) => {
            let mut words = integer_words(whole);
            words.push("point".into());
            words.extend(
                fraction
                    .chars()
                    .map(|c| DIGITS[(c as u8 - b'0') as usize].to_string()),
            );
            words
        }
        None => integer_words(core),
    }
}

fn integer_words(digits: &str) -> Vec<String> {
    match digits.parse::<u64>() {
        Ok(n) if n < SPELL_LIMIT => cardinal(n),
        _ => digits
            .chars()
            .map(|c| DIGITS[(c as u8 - b'0') as usize].to_string())
            .collect(),
    }
}

/// Cardinal words for `n` ("1042" -> "one thousand forty two"). No "and".
pub fn cardinal(n: u64) -> Vec<String> {
    if n == 0 {
        return vec!["zero".into()];
    }
    let mut out = Vec::new();
    let mut rest = n;
    for (value, name) in SCALES {
        if rest >= value {
            push_below_thousand(rest / value, &mut out);
            out.push(name.into());
            rest %= value;
        }
    }
    if rest > 0 {
        push_below_thousand(rest, &mut out);
    }
    out
}

fn push_below_thousand(n: u64, out: &mut Vec<String>) {
    let hundreds = (n / 100) as usize;
    let rest = (n % 100) as usize;
    if hundreds > 0 {
        out.push(ONES[hundreds].into());
        out.push("hundred".into());
    }
    if rest >= 20 {
        out.push(TENS[rest / 10].into());
        if !rest.is_multiple_of(10) {
            out.push(ONES[rest % 10].into());
        }
    } else if rest > 0 {
        out.push(ONES[rest].into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(token: &str) -> Vec<String> {
        units(token)
    }

    #[test]
    fn strips_punctuation_and_casing() {
        assert_eq!(words("Save."), ["save"]);
        assert_eq!(words("\"PostgreSQL\","), ["postgresql"]);
        assert_eq!(words("\u{2018}quoted\u{2019}"), ["quoted"]);
    }

    #[test]
    fn expands_integers_decimals_and_thousands_separators() {
        assert_eq!(words("3"), ["three"]);
        assert_eq!(words("2026"), ["two", "thousand", "twenty", "six"]);
        assert_eq!(words("1,042"), ["one", "thousand", "forty", "two"]);
        assert_eq!(words("2.5"), ["two", "point", "five"]);
        assert_eq!(words("2.50"), ["two", "point", "five", "zero"]);
        assert_eq!(words("100%"), ["one", "hundred", "percent"]);
        assert_eq!(words("0"), ["zero"]);
    }

    #[test]
    fn expands_contractions_on_both_sides_identically() {
        assert_eq!(words("don't"), ["do", "not"]);
        assert_eq!(words("Don\u{2019}t,"), ["do", "not"]);
        assert_eq!(words("I'm"), ["i", "am"]);
    }

    #[test]
    fn splits_hyphens_and_drops_standalone_punctuation() {
        assert_eq!(words("end-to-end"), ["end", "to", "end"]);
        assert!(words("\u{2014}").is_empty());
        assert!(words("...").is_empty());
    }

    #[test]
    fn cardinal_handles_scales_and_teens() {
        assert_eq!(cardinal(19).join(" "), "nineteen");
        assert_eq!(cardinal(120).join(" "), "one hundred twenty");
        assert_eq!(cardinal(1_000_001).join(" "), "one million one");
        assert_eq!(cardinal(2_000_000_000).join(" "), "two billion");
    }

    #[test]
    fn very_large_numbers_are_spelled_digit_by_digit() {
        assert_eq!(
            words("12345678901234567"),
            [
                "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "zero",
                "one", "two", "three", "four", "five", "six", "seven"
            ]
        );
    }
}
