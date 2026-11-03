//! Whole-word keyword matching for comment-to-DM automation rules.

/// Lower-case alphanumeric words of a text (see [`keyword_matches`]).
pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// True when the keyword's words appear in the text as one contiguous run of whole words. `price` matches
/// "PRICE please" and "the price?" but not "priceless"; an empty keyword never matches.
pub fn keyword_matches(keyword: &str, text: &str) -> bool {
    let key = words(keyword);
    if key.is_empty() {
        return false;
    }
    let body = words(text);
    body.windows(key.len()).any(|w| w == key.as_slice())
}
