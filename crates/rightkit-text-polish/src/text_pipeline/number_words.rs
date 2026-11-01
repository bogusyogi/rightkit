use super::inline::collapse_horizontal_space;
use std::sync::OnceLock;

pub(crate) fn collapse_repetitions_line(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 2 {
        return text.to_string();
    }

    // Strip trailing punctuation for comparison, keep original for output.
    fn normalize(word: &str) -> &str {
        word.trim_end_matches(|c: char| c.is_ascii_punctuation())
    }

    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0usize;
    while i < words.len() {
        if i + 3 < words.len()
            && normalize(words[i]).eq_ignore_ascii_case(normalize(words[i + 2]))
            && normalize(words[i + 1]).eq_ignore_ascii_case(normalize(words[i + 3]))
            && !normalize(words[i]).is_empty()
            && !normalize(words[i + 1]).is_empty()
        {
            out.push(words[i]);
            out.push(words[i + 1]);
            i += 4;
            while i + 1 < words.len()
                && normalize(words[i]).eq_ignore_ascii_case(normalize(out[out.len() - 2]))
                && normalize(words[i + 1]).eq_ignore_ascii_case(normalize(out[out.len() - 1]))
            {
                i += 2;
            }
            continue;
        }
        if out
            .last()
            .map(|prev| {
                normalize(prev).eq_ignore_ascii_case(normalize(words[i]))
                    && !normalize(words[i]).is_empty()
            })
            .unwrap_or(false)
        {
            i += 1;
            continue;
        }
        out.push(words[i]);
        i += 1;
    }
    out.join(" ")
}

fn number_words() -> &'static std::collections::HashMap<&'static str, i32> {
    static MAP: OnceLock<std::collections::HashMap<&'static str, i32>> = OnceLock::new();
    MAP.get_or_init(|| {
        [
            ("zero", 0),
            ("one", 1),
            ("two", 2),
            ("three", 3),
            ("four", 4),
            ("five", 5),
            ("six", 6),
            ("seven", 7),
            ("eight", 8),
            ("nine", 9),
            ("ten", 10),
            ("eleven", 11),
            ("twelve", 12),
            ("thirteen", 13),
            ("fourteen", 14),
            ("fifteen", 15),
            ("sixteen", 16),
            ("seventeen", 17),
            ("eighteen", 18),
            ("nineteen", 19),
            ("twenty", 20),
            ("thirty", 30),
            ("forty", 40),
            ("fifty", 50),
            ("sixty", 60),
            ("seventy", 70),
            ("eighty", 80),
            ("ninety", 90),
        ]
        .into_iter()
        .collect()
    })
}

pub(crate) fn word_value(word: &str) -> Option<i32> {
    number_words()
        .get(word.to_ascii_lowercase().as_str())
        .copied()
}

fn ordinal_value(word: &str) -> Option<i32> {
    match word.to_ascii_lowercase().replace('-', " ").as_str() {
        "first" => Some(1),
        "second" => Some(2),
        "third" => Some(3),
        "fourth" => Some(4),
        "fifth" => Some(5),
        "sixth" => Some(6),
        "seventh" => Some(7),
        "eighth" => Some(8),
        "ninth" => Some(9),
        "tenth" => Some(10),
        "eleventh" => Some(11),
        "twelfth" => Some(12),
        "thirteenth" => Some(13),
        "fourteenth" => Some(14),
        "fifteenth" => Some(15),
        "sixteenth" => Some(16),
        "seventeenth" => Some(17),
        "eighteenth" => Some(18),
        "nineteenth" => Some(19),
        "twentieth" => Some(20),
        "twenty first" => Some(21),
        "twenty second" => Some(22),
        "twenty third" => Some(23),
        "twenty fourth" => Some(24),
        "twenty fifth" => Some(25),
        "twenty sixth" => Some(26),
        "twenty seventh" => Some(27),
        "twenty eighth" => Some(28),
        "twenty ninth" => Some(29),
        "thirtieth" => Some(30),
        "thirty first" => Some(31),
        _ => None,
    }
}

fn ordinal_suffix(n: i32) -> &'static str {
    let teen = n % 100;
    if (11..=13).contains(&teen) {
        return "th";
    }
    match n % 10 {
        1 => "st",
        2 => "nd",
        3 => "rd",
        _ => "th",
    }
}

pub(crate) fn format_ordinal(n: i32) -> String {
    format!("{n}{}", ordinal_suffix(n))
}

pub(crate) fn parse_day_phrase(text: &str) -> Option<i32> {
    if let Some(n) = ordinal_value(text) {
        return (1..=31).contains(&n).then_some(n);
    }
    let s = text
        .trim()
        .trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let digits = s
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    if !digits.is_empty() {
        return digits.parse::<i32>().ok().filter(|n| (1..=31).contains(n));
    }
    parse_number_phrase(text).filter(|n| (1..=31).contains(n))
}

pub(crate) fn parse_number_phrase(text: &str) -> Option<i32> {
    let normalized = collapse_horizontal_space(&text.to_ascii_lowercase().replace('-', " "));
    let s = normalized.trim();
    if s.is_empty() {
        return None;
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return s.parse::<i32>().ok();
    }
    let parts: Vec<&str> = s.split_whitespace().collect();
    match parts.as_slice() {
        [one] => word_value(one),
        [a, b] if *b == "hundred" => {
            let n = word_value(a)?;
            (1..=9).contains(&n).then_some(n * 100)
        }
        [a, b]
            if word_value(a).is_some_and(|n| (1..=9).contains(&n))
                && word_value(b).is_some_and(|n| (20..=90).contains(&n) && n % 10 == 0) =>
        {
            Some(word_value(a)? * 100 + word_value(b)?)
        }
        [a, b] => {
            let first = word_value(a)?;
            let second = word_value(b)?;
            if (20..=90).contains(&first) && first % 10 == 0 && (1..=9).contains(&second) {
                Some(first + second)
            } else if (1..=9).contains(&first) && (0..=19).contains(&second) {
                Some(first * 100 + second)
            } else {
                None
            }
        }
        [a, "hundred", b] => {
            let first = word_value(a)?;
            let tail = word_value(b)?;
            if (1..=9).contains(&first) && (1..=99).contains(&tail) {
                Some(first * 100 + tail)
            } else {
                None
            }
        }
        [a, "hundred", b, c] => {
            let first = word_value(a)?;
            let tens = word_value(b)?;
            let ones = word_value(c)?;
            if (1..=9).contains(&first)
                && (20..=90).contains(&tens)
                && tens % 10 == 0
                && (1..=9).contains(&ones)
            {
                Some(first * 100 + tens + ones)
            } else {
                None
            }
        }
        [a, b, c]
            if word_value(a).is_some_and(|n| (1..=9).contains(&n))
                && word_value(b).is_some_and(|n| (20..=90).contains(&n) && n % 10 == 0)
                && word_value(c).is_some_and(|n| (1..=9).contains(&n)) =>
        {
            Some(word_value(a)? * 100 + word_value(b)? + word_value(c)?)
        }
        _ => None,
    }
}

pub(crate) fn scale_value(word: &str) -> Option<i64> {
    match word {
        "thousand" => Some(1_000),
        "million" => Some(1_000_000),
        "billion" => Some(1_000_000_000),
        _ => None,
    }
}

/// Parse a run of cardinal number words (with `hundred`/`thousand`/`million`/`billion`
/// and optional `and`) into an integer. Returns None if any token isn't a number word.
pub(crate) fn parse_cardinal_run(s: &str) -> Option<i64> {
    let norm = s.to_ascii_lowercase().replace('-', " ");
    let mut total = 0i64;
    let mut current = 0i64;
    let mut saw = false;
    for tok in norm.split_whitespace() {
        if tok == "and" {
            continue;
        }
        if let Some(v) = word_value(tok) {
            current += v as i64;
            saw = true;
        } else if tok == "hundred" {
            current = if current == 0 { 100 } else { current * 100 };
            saw = true;
        } else {
            let scale = scale_value(tok)?;
            let chunk = if current == 0 { 1 } else { current };
            total += chunk * scale;
            current = 0;
            saw = true;
        }
    }
    saw.then_some(total + current)
}

/// Comma-group an integer's digits ("2000000" -> "2,000,000"). Leaves <=3 digits as-is.
pub(crate) fn group_thousands(int_digits: &str) -> String {
    let neg = int_digits.starts_with('-');
    let digits: String = int_digits.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() <= 3 {
        return format!("{}{}", if neg { "-" } else { "" }, digits);
    }
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    format!("{}{}", if neg { "-" } else { "" }, out)
}

/// Map a spoken/written single digit token to its char ("nine"->'9', "oh"->'0').
pub(crate) fn spoken_digit(tok: &str) -> Option<char> {
    match tok.to_ascii_lowercase().as_str() {
        "0" | "zero" | "oh" | "o" => Some('0'),
        "1" | "one" => Some('1'),
        "2" | "two" => Some('2'),
        "3" | "three" => Some('3'),
        "4" | "four" => Some('4'),
        "5" | "five" => Some('5'),
        "6" | "six" => Some('6'),
        "7" | "seven" => Some('7'),
        "8" | "eight" => Some('8'),
        "9" | "nine" => Some('9'),
        _ => None,
    }
}
