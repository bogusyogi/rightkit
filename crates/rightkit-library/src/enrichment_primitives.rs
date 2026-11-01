use super::*;

pub(super) fn extract_links(text: &str) -> Vec<EnrichedLink> {
    let mut links = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Match http(s) URLs — match non-whitespace, then strip trailing punctuation
    let re = match cached_regex(r#"(?i)https?://[^\s]+"#) {
        Some(r) => r,
        None => return links,
    };
    for m in re.find_iter(text) {
        let raw_url = m.as_str();
        // Strip trailing punctuation that's unlikely part of a URL
        let raw = raw_url.trim_end_matches(|c: char| {
            matches!(
                c,
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '"' | '>'
            )
        });
        if raw.is_empty() || seen.contains(raw) {
            continue;
        }
        seen.insert(raw.to_string());
        if let Some((normalized, domain)) = normalize_url(raw) {
            links.push(EnrichedLink {
                url: raw.to_string(),
                normalized_url: normalized,
                domain,
            });
        }
    }

    links.sort_by(|a, b| a.url.cmp(&b.url));
    links
}

pub(super) fn normalize_url(url: &str) -> Option<(String, String)> {
    let scheme = if url
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
    {
        "https"
    } else if url
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
    {
        "http"
    } else {
        return None;
    };
    let without_scheme = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    let (host_raw, path_and_query) = without_scheme
        .split_once('/')
        .map(|(host, rest)| (host, Some(rest)))
        .unwrap_or((without_scheme, None));
    let mut host = host_raw.to_ascii_lowercase();
    if let Some(stripped) = host.strip_prefix("www.") {
        host = stripped.to_string();
    }
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    let normalized = match path_and_query {
        Some(rest) => format!("{scheme}://{host}/{rest}"),
        None => format!("{scheme}://{host}"),
    };
    Some((normalized, host))
}

pub(super) fn extract_hashtags_and_mentions(text: &str) -> (Vec<String>, Vec<String>) {
    let mut hashtags = Vec::new();
    let mut mentions = Vec::new();
    let mut seen_tags = std::collections::HashSet::new();
    let mut seen_mentions = std::collections::HashSet::new();

    // Hashtags: #word (ASCII + underscore + hyphen, also allow unicode letters)
    for cap in regex_captures_iter(text, r#"#([\w][\w\-]{0,100})"#) {
        if let Some(tag) = cap.get(1) {
            let original = tag.as_str().to_string();
            let key = original.to_ascii_lowercase();
            if !key.is_empty() && seen_tags.insert(key) {
                hashtags.push(original);
            }
        }
    }

    // Mentions: @word
    for cap in regex_captures_iter(text, r#"@([\w][\w\.\-]{0,100})"#) {
        if let Some(mention) = cap.get(1) {
            let original = mention.as_str().to_string();
            let key = original.to_ascii_lowercase();
            if !key.is_empty() && seen_mentions.insert(key) {
                mentions.push(original);
            }
        }
    }

    (hashtags, mentions)
}

pub(super) fn extract_numbers(text: &str, sentences: &[String]) -> Vec<EnrichedNumber> {
    let mut numbers = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Match numbers: integers, decimals, with optional commas, optional unit suffix
    let re_pattern = r#"(?i)\b(\d{1,3}(?:,\d{3})*(?:\.\d+)?)\s*(%|percent|million|billion|thousand|hours?|minutes?|seconds?|days?|weeks?|months?|years?|k\b|m\b|b\b)?"#;
    for cap in regex_captures_iter(text, re_pattern) {
        let full_match = cap
            .get(0)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        let raw_num = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let cleaned = raw_num.replace(',', "");
        let value: Option<f64> = cleaned.parse().ok();

        // Avoid duplicating the same number
        let key = format!("{}:{:?}", full_match.to_ascii_lowercase(), value);
        if !seen.insert(key) {
            continue;
        }

        let sentence = find_containing_sentence(text, &full_match, sentences);

        numbers.push(EnrichedNumber {
            raw: full_match,
            value,
            sentence,
        });
    }

    numbers
}

pub(super) fn extract_dates(text: &str, sentences: &[String]) -> Vec<EnrichedDate> {
    let mut dates = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // ISO-like dates: 2024-01-15, 2024/01/15
    for cap in regex_captures_iter(text, r#"(\d{4})[-/](\d{1,2})[-/](\d{1,2})"#) {
        let full = cap
            .get(0)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        if seen.contains(&full) {
            continue;
        }
        seen.insert(full.clone());
        let y = cap.get(1).and_then(|m| m.as_str().parse::<u32>().ok());
        let mo = cap.get(2).and_then(|m| m.as_str().parse::<u32>().ok());
        let d = cap.get(3).and_then(|m| m.as_str().parse::<u32>().ok());
        let iso = match (y, mo, d) {
            (Some(y), Some(m), Some(d))
                if (1900..=2100).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d) =>
            {
                Some(format!("{y:04}-{m:02}-{d:02}"))
            }
            _ => None,
        };
        let sentence = find_containing_sentence(text, &full, sentences);
        dates.push(EnrichedDate {
            raw: full,
            iso,
            sentence,
        });
    }

    // Unambiguous month-day patterns: "January 15, 2024" or "15 January 2024"
    let month_names = "(?:january|february|march|april|may|june|july|august|september|october|november|december|jan|feb|mar|apr|jun|jul|aug|sep|sept|oct|nov|dec)";
    // "Month DD, YYYY"
    let pat1 = format!(r#"(?i)\b({month_names})\s+(\d{{1,2}}),?\s+(\d{{4}})\b"#);
    for cap in regex_captures_iter(text, &pat1) {
        let full = cap
            .get(0)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        if seen.contains(&full) {
            continue;
        }
        seen.insert(full.clone());
        if let Some(iso) = parse_month_day_year(&cap) {
            let sentence = find_containing_sentence(text, &full, sentences);
            dates.push(EnrichedDate {
                raw: full,
                iso: Some(iso),
                sentence,
            });
        }
    }
    // "DD Month YYYY"
    let pat2 = format!(r#"(?i)\b(\d{{1,2}})\s+({month_names})\s+(\d{{4}})\b"#);
    for cap in regex_captures_iter(text, &pat2) {
        let full = cap
            .get(0)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        if seen.contains(&full) {
            continue;
        }
        seen.insert(full.clone());
        if let Some(iso) = parse_day_month_year(&cap) {
            let sentence = find_containing_sentence(text, &full, sentences);
            dates.push(EnrichedDate {
                raw: full,
                iso: Some(iso),
                sentence,
            });
        }
    }

    dates
}

pub(super) fn month_to_num(name: &str) -> Option<u32> {
    match name.to_ascii_lowercase().as_str() {
        "january" | "jan" => Some(1),
        "february" | "feb" => Some(2),
        "march" | "mar" => Some(3),
        "april" | "apr" => Some(4),
        "may" => Some(5),
        "june" | "jun" => Some(6),
        "july" | "jul" => Some(7),
        "august" | "aug" => Some(8),
        "september" | "sep" | "sept" => Some(9),
        "october" | "oct" => Some(10),
        "november" | "nov" => Some(11),
        "december" | "dec" => Some(12),
        _ => None,
    }
}

pub(super) fn parse_month_day_year(cap: &regex::Captures) -> Option<String> {
    let month = cap.get(1).map(|m| m.as_str())?;
    let day = cap.get(2).and_then(|m| m.as_str().parse::<u32>().ok())?;
    let year = cap.get(3).and_then(|m| m.as_str().parse::<u32>().ok())?;
    let m = month_to_num(month)?;
    if (1900..=2100).contains(&year) && (1..=31).contains(&day) {
        Some(format!("{year:04}-{m:02}-{day:02}"))
    } else {
        None
    }
}

pub(super) fn parse_day_month_year(cap: &regex::Captures) -> Option<String> {
    let day = cap.get(1).and_then(|m| m.as_str().parse::<u32>().ok())?;
    let month = cap.get(2).map(|m| m.as_str())?;
    let year = cap.get(3).and_then(|m| m.as_str().parse::<u32>().ok())?;
    let m = month_to_num(month)?;
    if (1900..=2100).contains(&year) && (1..=31).contains(&day) {
        Some(format!("{year:04}-{m:02}-{day:02}"))
    } else {
        None
    }
}
