use super::{control::re, inline::collapse_horizontal_space, number_words::*};
use regex::{Captures, Regex};
use std::sync::OnceLock;

/// Compiled once: the spoken-number-run pattern used by `normalize_large_numbers`
/// Pass B. `token` is a fixed literal, so the `format!`-built pattern string is
/// identical on every call — computing it fresh per call (as before) only wasted
/// an allocation; the actual `Regex::new` was already deduplicated by `re()`'s
/// cache. A dedicated `OnceLock` skips both the `format!` and the cache lookup.
fn large_number_run_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let token = r"(?:zero|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety|hundred|thousand|million|billion|and)";
        Regex::new(&format!(r"(?i)\b{token}(?:[\s-]+{token})*\b"))
            .expect("valid large-number run regex")
    })
}

/// Spoken large-number ITN: collapse runs containing a scale word
/// ("two million" -> "2000000", "fifty thousand" -> "50000"). Runs without a scale
/// word are left for `normalize_numbers` (hundreds/tens).
pub(crate) fn normalize_large_numbers(text: &str) -> String {
    // Pass A — literal integer/decimal before a scale word ("2.5 million" -> 2500000).
    // Runs after `normalize_decimals` so "two point five million" is handled.
    let lit = re(r"(?i)\b(\d+(?:\.\d+)?)\s+(thousand|million|billion)\b");
    let pre = lit.replace_all(text, |caps: &Captures| {
        let Ok(v) = caps[1].parse::<f64>() else {
            return caps[0].to_string();
        };
        let scale = scale_value(&caps[2].to_ascii_lowercase()).unwrap() as f64;
        let prod = v * scale;
        if prod.fract() == 0.0 && prod.abs() < 1e15 {
            (prod as i64).to_string()
        } else {
            caps[0].to_string()
        }
    });

    // Pass B — spoken word runs containing a scale word ("two million" -> 2000000).
    large_number_run_re()
        .replace_all(&pre, |caps: &Captures| {
            let run = &caps[0];
            let has_scale = run.split(|c: char| c.is_whitespace() || c == '-').any(|t| {
                matches!(
                    t.to_ascii_lowercase().as_str(),
                    "thousand" | "million" | "billion"
                )
            });
            if !has_scale {
                return run.to_string();
            }
            match parse_cardinal_run(run) {
                Some(n) if n > 0 => n.to_string(),
                _ => run.to_string(),
            }
        })
        .into_owned()
}

fn decimal_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let num = number_phrase_pattern();
        let digit = r"(?:zero|oh|one|two|three|four|five|six|seven|eight|nine|\d)";
        Regex::new(&format!(
            r"(?i)\b({num})\s+point\s+({digit}(?:[\s-]+{digit})*)\b"
        ))
        .expect("valid decimal regex")
    })
}

/// Spoken decimals: "nine point four" -> "9.4", "three point one four" -> "3.14".
/// Integer part may be digits or a cardinal phrase; fraction is read digit-by-digit.
pub(crate) fn normalize_decimals(text: &str) -> String {
    decimal_re()
        .replace_all(text, |caps: &Captures| {
            let int_str = &caps[1];
            let ip = if int_str.chars().all(|c| c.is_ascii_digit()) {
                int_str.to_string()
            } else {
                match parse_number_phrase(int_str) {
                    Some(n) => n.to_string(),
                    None => return caps[0].to_string(),
                }
            };
            let mut frac = String::new();
            for tok in caps[2].split(|c: char| c.is_whitespace() || c == '-') {
                if tok.is_empty() {
                    continue;
                }
                match spoken_digit(tok) {
                    Some(d) => frac.push(d),
                    None => return caps[0].to_string(),
                }
            }
            if frac.is_empty() {
                return caps[0].to_string();
            }
            format!("{ip}.{frac}")
        })
        .into_owned()
}

/// Resolve a money/percent amount capture (literal "9.4"/"2,000,000" or a spoken phrase)
/// into (comma-grouped integer part, fractional ".NN" or empty).
fn amount_sections(s: &str) -> Option<(String, String)> {
    let t = s.trim();
    if t.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        let clean = t.replace(',', "");
        let (ip, fp) = match clean.split_once('.') {
            Some((a, b)) => (a.to_string(), format!(".{b}")),
            None => (clean, String::new()),
        };
        Some((group_thousands(&ip), fp))
    } else {
        parse_number_phrase(t).map(|n| (group_thousands(&n.to_string()), String::new()))
    }
}

fn number_phrase_pattern() -> &'static str {
    r"(?:\d{1,4}|(?:zero|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety)(?:[\s-]+(?:hundred|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety)){0,2})"
}

// The patterns below all interpolate `number_phrase_pattern()` (a fixed literal)
// via `format!`. That produces the exact same pattern string on every call, so
// the old `re(&format!(...))` call site was paying a fresh String allocation per
// utterance just to land on a cache hit. Each is now its own `OnceLock`: the
// `format!` runs once, ever, and every call after that is a direct static ref —
// same compiled regex, same match behavior, no per-call allocation.
fn pm_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let num = number_phrase_pattern();
        Regex::new(&format!(
            r"(?i)\b({num})(?:\s+({num}))?\s*(?:p\s*\.?\s*m\.?|pm)\b"
        ))
        .expect("valid pm regex")
    })
}

fn am_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let num = number_phrase_pattern();
        Regex::new(&format!(
            r"(?i)\b({num})(?:\s+({num}))?\s*(?:a\s*\.?\s*m\.?|am)\b"
        ))
        .expect("valid am regex")
    })
}

fn bare_time_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let num = number_phrase_pattern();
        Regex::new(&format!(
            r"(?i)\b(one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|1|2|3|4|5|6|7|8|9|10|11|12)\s+({num})\b"
        ))
        .expect("valid bare time regex")
    })
}

/// amount = a literal integer/decimal (possibly comma-grouped, incl. large numbers
/// already converted to digits) OR a spoken number phrase. Built once; reused by
/// the dollars/euros/pounds/percent patterns below.
fn amount_pattern() -> &'static str {
    static PATTERN: OnceLock<String> = OnceLock::new();
    PATTERN.get_or_init(|| {
        let num = number_phrase_pattern();
        format!(r"(?:\d[\d,]*(?:\.\d+)?|{num})")
    })
}

fn dollars_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let amount = amount_pattern();
        Regex::new(&format!(r"(?i)\b({amount})\s+dollars?\b")).expect("valid dollars regex")
    })
}

fn euros_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let amount = amount_pattern();
        Regex::new(&format!(r"(?i)\b({amount})\s+euros?\b")).expect("valid euros regex")
    })
}

fn pounds_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let amount = amount_pattern();
        Regex::new(&format!(r"(?i)\b({amount})\s+pounds?\b")).expect("valid pounds regex")
    })
}

fn percent_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let amount = amount_pattern();
        Regex::new(&format!(r"(?i)\b({amount})\s+percent\b")).expect("valid percent regex")
    })
}

pub(crate) fn normalize_time_money_percent(text: &str) -> String {
    // Cow-chained: 11 sequential passes, most of which don't match on any given
    // utterance (an utterance rarely contains both a time AND a money AND a
    // percent phrase) — only reassign, and therefore only allocate, on an
    // actual match.
    let out = re(r"(?i)\bnoon\b").replace_all(text, "12:00 PM");
    let out = re(r"(?i)\bmidnight\b").replace_all(&out, "12:00 AM");

    let out = pm_re().replace_all(&out, |caps: &Captures| time_repl(caps, "PM"));
    let out = am_re().replace_all(&out, |caps: &Captures| time_repl(caps, "AM"));

    let compact_pm = re(r"(?i)\b(\d{3,4})\s*(?:p\s*\.?\s*m\.?|pm)\b");
    let compact_am = re(r"(?i)\b(\d{3,4})\s*(?:a\s*\.?\s*m\.?|am)\b");
    let out = compact_pm.replace_all(&out, |caps: &Captures| compact_time_repl(&caps[1], "PM"));
    let out = compact_am.replace_all(&out, |caps: &Captures| compact_time_repl(&caps[1], "AM"));

    let out = bare_time_re().replace_all(&out, |caps: &Captures| bare_time_repl(caps));

    let out = dollars_re().replace_all(&out, |caps: &Captures| money_repl(caps, "$"));
    let out = euros_re().replace_all(&out, |caps: &Captures| money_repl(caps, "\u{20ac}"));
    let out = pounds_re().replace_all(&out, |caps: &Captures| money_repl(caps, "\u{00a3}"));
    let out = percent_re().replace_all(&out, |caps: &Captures| {
        amount_sections(&caps[1])
            .map(|(ip, fp)| format!("{ip}{fp}%"))
            .unwrap_or_else(|| caps[0].to_string())
    });
    out.into_owned()
}

fn month_pattern() -> &'static str {
    r"(?:january|jan|february|feb|march|mar|april|apr|may|june|jun|july|jul|august|aug|september|sep|sept|october|oct|november|nov|december|dec)"
}

fn canonical_month(month: &str) -> &'static str {
    match month.to_ascii_lowercase().as_str() {
        "jan" | "january" => "January",
        "feb" | "february" => "February",
        "mar" | "march" => "March",
        "apr" | "april" => "April",
        "may" => "May",
        "jun" | "june" => "June",
        "jul" | "july" => "July",
        "aug" | "august" => "August",
        "sep" | "sept" | "september" => "September",
        "oct" | "october" => "October",
        "nov" | "november" => "November",
        "dec" | "december" => "December",
        _ => "Month",
    }
}

fn day_phrase_pattern() -> &'static str {
    r"(?:\d{1,2}(?:st|nd|rd|th)?|first|second|third|fourth|fifth|sixth|seventh|eighth|ninth|tenth|eleventh|twelfth|thirteenth|fourteenth|fifteenth|sixteenth|seventeenth|eighteenth|nineteenth|twentieth|twenty[\s-]+first|twenty[\s-]+second|twenty[\s-]+third|twenty[\s-]+fourth|twenty[\s-]+fifth|twenty[\s-]+sixth|twenty[\s-]+seventh|twenty[\s-]+eighth|twenty[\s-]+ninth|thirtieth|thirty[\s-]+first)"
}

fn year_phrase_pattern() -> &'static str {
    r"(?:\d{4}|twenty[\s-]+(?:zero|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|thirty(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|forty(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|fifty(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|sixty(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|seventy(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|eighty(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|ninety(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?)|two[\s-]+thousand(?:[\s-]+(?:and[\s-]+)?(?:one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|thirty(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|forty(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|fifty(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|sixty(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|seventy(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|eighty(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?|ninety(?:[\s-]+(?:one|two|three|four|five|six|seven|eight|nine))?))?)"
}

fn parse_year_phrase(text: &str) -> Option<i32> {
    let s = collapse_horizontal_space(&text.to_ascii_lowercase().replace('-', " "));
    let s = s.trim();
    if s.chars().all(|c| c.is_ascii_digit()) {
        return s.parse::<i32>().ok().filter(|n| (1900..=2099).contains(n));
    }
    if let Some(tail) = s.strip_prefix("twenty ") {
        return parse_number_phrase(tail).map(|n| if n < 10 { 2020 + n } else { 2000 + n });
    }
    if let Some(rest) = s.strip_prefix("two thousand") {
        let rest = rest
            .trim()
            .strip_prefix("and ")
            .unwrap_or(rest.trim())
            .trim();
        if rest.is_empty() {
            return Some(2000);
        }
        return parse_number_phrase(rest).map(|n| 2000 + n);
    }
    None
}

fn month_day_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let month = month_pattern();
        let day = day_phrase_pattern();
        let year = year_phrase_pattern();
        Regex::new(&format!(r"(?i)\b({month})\s+({day})(?:,?\s+({year}))?\b"))
            .expect("valid month-day regex")
    })
}

fn day_of_month_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let month = month_pattern();
        let day = day_phrase_pattern();
        let year = year_phrase_pattern();
        Regex::new(&format!(
            r"(?i)\b(?:the\s+)?({day})\s+of\s+({month})(?:,?\s+({year}))?\b"
        ))
        .expect("valid day-of-month regex")
    })
}

pub(crate) fn normalize_dates(text: &str) -> String {
    let out = month_day_re().replace_all(text, |caps: &Captures| {
        let Some(day) = parse_day_phrase(&caps[2]) else {
            return caps[0].to_string();
        };
        let year = caps
            .get(3)
            .and_then(|m| parse_year_phrase(m.as_str()))
            .map(|year| format!(", {year}"));
        match year {
            Some(year) => format!("{} {}{}", canonical_month(&caps[1]), day, year),
            None => format!("{} {}", canonical_month(&caps[1]), format_ordinal(day)),
        }
    });

    let out = day_of_month_re().replace_all(&out, |caps: &Captures| {
        let Some(day) = parse_day_phrase(&caps[1]) else {
            return caps[0].to_string();
        };
        let year = caps
            .get(3)
            .and_then(|m| parse_year_phrase(m.as_str()))
            .map(|year| format!(", {year}"));
        match year {
            Some(year) => format!("{} {}{}", canonical_month(&caps[2]), day, year),
            None => format!("{} {}", canonical_month(&caps[2]), format_ordinal(day)),
        }
    });
    out.into_owned()
}

fn units_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let num = number_phrase_pattern();
        Regex::new(&format!(
            r"(?i)\b({num})\s+(hours?|minutes?|seconds?|milliseconds?|kilobytes?|megabytes?|gigabytes?|k\s*b|m\s*b|g\s*b)\b"
        ))
        .expect("valid units regex")
    })
}

pub(crate) fn normalize_units(text: &str) -> String {
    units_re()
        .replace_all(text, |caps: &Captures| {
            let Some(n) = parse_number_phrase(&caps[1]) else {
                return caps[0].to_string();
            };
            let lower = caps[2].to_ascii_lowercase();
            let unit = match lower.as_str() {
                "k b" | "kb" | "kilobyte" | "kilobytes" => "KB".to_string(),
                "m b" | "mb" | "megabyte" | "megabytes" => "MB".to_string(),
                "g b" | "gb" | "gigabyte" | "gigabytes" => "GB".to_string(),
                _ => lower,
            };
            format!("{n} {unit}")
        })
        .into_owned()
}

fn compact_time_repl(digits: &str, suffix: &str) -> String {
    let Ok(n) = digits.parse::<i32>() else {
        return format!("{digits} {suffix}");
    };
    let hour = n / 100;
    let minute = n % 100;
    if (1..=12).contains(&hour) && (0..60).contains(&minute) {
        format!("{hour}:{minute:02} {suffix}")
    } else {
        format!("{digits} {suffix}")
    }
}

fn time_repl(caps: &Captures, suffix: &str) -> String {
    let hour = parse_number_phrase(&caps[1]);
    let minute = caps
        .get(2)
        .and_then(|m| parse_number_phrase(m.as_str()))
        .unwrap_or(0);
    if let Some(hour) = hour {
        if (0..=24).contains(&hour) && (0..60).contains(&minute) {
            return format!("{hour}:{minute:02} {suffix}");
        }
    }
    caps[0].to_string()
}

fn bare_time_repl(caps: &Captures) -> String {
    let hour = parse_number_phrase(&caps[1]);
    let minute = parse_number_phrase(&caps[2]);
    if let (Some(hour), Some(minute)) = (hour, minute) {
        // Cueless bare time (no "at"/am/pm): only treat as a time when the minute
        // is a natural spoken minute word (>=10 — "ten thirty", "two fifteen").
        // A single-digit minute ("two three") is almost never a real time and was
        // mangling normal speech ("two three options" -> "2:03 options").
        if (1..=12).contains(&hour) && (10..60).contains(&minute) {
            return format!("{hour}:{minute:02}");
        }
    }
    caps[0].to_string()
}

fn money_repl(caps: &Captures, symbol: &str) -> String {
    amount_sections(&caps[1])
        .map(|(ip, fp)| format!("{symbol}{ip}{fp}"))
        .unwrap_or_else(|| caps[0].to_string())
}
