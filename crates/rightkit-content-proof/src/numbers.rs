//! Exact claim-number reading for content proof review: a number in copy (`$1,240`, `1.24k`,
//! `12%`, `twelve`) is read as an exact decimal string, never a float, so `1,240` equals
//! `1240.00` and `1.24k` equals `1240`. Periods and units are compared by the caller.

/// Number words read as values, with their exact decimal.
pub const WORD_VALUES: &[(&str, &str)] = &[
    ("zero", "0"),
    ("one", "1"),
    ("two", "2"),
    ("three", "3"),
    ("four", "4"),
    ("five", "5"),
    ("six", "6"),
    ("seven", "7"),
    ("eight", "8"),
    ("nine", "9"),
    ("ten", "10"),
    ("eleven", "11"),
    ("twelve", "12"),
    ("thirteen", "13"),
    ("fourteen", "14"),
    ("fifteen", "15"),
    ("sixteen", "16"),
    ("seventeen", "17"),
    ("eighteen", "18"),
    ("nineteen", "19"),
    ("twenty", "20"),
    ("hundred", "100"),
    ("thousand", "1000"),
    ("million", "1000000"),
    ("billion", "1000000000"),
];

/// Canonical exact decimal of a token as §5.2 reads it: optional currency sign, thousands commas, a decimal
/// point, a `k`, `m` or `bn` suffix and `%`; or a listed number word. `None` when it is not a number.
/// No rounding: the digits are shifted as text.
pub fn canonical(raw: &str) -> Option<String> {
    let lower = raw.trim().to_lowercase();
    if let Some((_, v)) = WORD_VALUES.iter().find(|(w, _)| *w == lower) {
        return Some((*v).to_string());
    }
    let mut s: &str = &lower;
    s = s.strip_prefix(['$', '£', '€']).unwrap_or(s);
    s = s.strip_suffix('%').unwrap_or(s);
    let (digits, shift) = if let Some(d) = s.strip_suffix("bn") {
        (d, 9)
    } else if let Some(d) = s.strip_suffix('k') {
        (d, 3)
    } else if let Some(d) = s.strip_suffix('m') {
        (d, 6)
    } else {
        (s, 0)
    };
    let digits = digits.replace(',', "");
    let (int, frac) = match digits.split_once('.') {
        Some((i, f)) => (i.to_string(), f.to_string()),
        None => (digits.clone(), String::new()),
    };
    if int.is_empty()
        || !int.chars().all(|c| c.is_ascii_digit())
        || !frac.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let all = format!("{int}{frac}");
    let point = int.len() + shift;
    let (head, tail) = if point >= all.len() {
        (
            format!("{all}{}", "0".repeat(point - all.len())),
            String::new(),
        )
    } else {
        (all[..point].to_string(), all[point..].to_string())
    };
    let head = head.trim_start_matches('0');
    let head = if head.is_empty() { "0" } else { head };
    let tail = tail.trim_end_matches('0');
    Some(if tail.is_empty() {
        head.to_string()
    } else {
        format!("{head}.{tail}")
    })
}

/// One numeric token in the text: character range, the text of it and its canonical value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// First character index (in `char`s, not bytes).
    pub start: usize,
    /// One past the last character index.
    pub end: usize,
    /// The token as written.
    pub text: String,
    /// Its canonical exact decimal, when it reads as a number.
    pub value: Option<String>,
}

/// Every numeric token of `text`, in order: digits with an optional currency sign, thousands
/// commas, a decimal point, a `k`, `m` or `bn` suffix and `%`, plus listed number words.
pub fn tokens(text: &str) -> Vec<Token> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let currency =
            matches!(c[i], '$' | '£' | '€') && c.get(i + 1).is_some_and(|d| d.is_ascii_digit());
        if c[i].is_ascii_digit() || currency {
            let start = i;
            if currency {
                i += 1;
            }
            while i < c.len() {
                let next_is_digit = c.get(i + 1).is_some_and(|d| d.is_ascii_digit());
                if c[i].is_ascii_digit() || (matches!(c[i], ',' | '.') && next_is_digit) {
                    i += 1;
                } else {
                    break;
                }
            }
            let rest: String = c[i..c.len().min(i + 3)]
                .iter()
                .collect::<String>()
                .to_lowercase();
            let suffix = if rest.starts_with("bn") {
                2
            } else if rest.starts_with('k') || rest.starts_with('m') {
                1
            } else {
                0
            };
            if suffix > 0 && !c.get(i + suffix).is_some_and(|d| d.is_alphanumeric()) {
                i += suffix;
            }
            if c.get(i) == Some(&'%') {
                i += 1;
            }
            let raw: String = c[start..i].iter().collect();
            out.push(Token {
                start,
                end: i,
                value: canonical(&raw),
                text: raw,
            });
        } else if c[i].is_alphabetic() {
            let start = i;
            while i < c.len() && c[i].is_alphabetic() {
                i += 1;
            }
            let word: String = c[start..i].iter().collect();
            if let Some(value) = canonical(&word)
                .filter(|_| WORD_VALUES.iter().any(|(w, _)| *w == word.to_lowercase()))
            {
                out.push(Token {
                    start,
                    end: i,
                    value: Some(value),
                    text: word,
                });
            }
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_are_exact() {
        assert_eq!(canonical("1,240").as_deref(), Some("1240"));
        assert_eq!(canonical("1240.00").as_deref(), Some("1240"));
        assert_eq!(canonical("$1,240").as_deref(), Some("1240"));
        assert_eq!(canonical("1.24k").as_deref(), Some("1240"));
        assert_eq!(canonical("2.5m").as_deref(), Some("2500000"));
        assert_eq!(canonical("0.5").as_deref(), Some("0.5"));
        assert_eq!(canonical("12%").as_deref(), Some("12"));
        assert_eq!(canonical("twelve").as_deref(), Some("12"));
        assert_ne!(canonical("1420"), canonical("1240.00"));
        assert_eq!(canonical("abc"), None);
    }

    #[test]
    fn tokens_cover_signs_suffixes_and_words() {
        let t = tokens("Made $1,240 in 7 days, up 12% and 1.2k views, nine wins.");
        let found: Vec<&str> = t.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(found, ["$1,240", "7", "12%", "1.2k", "nine"]);
    }
}
