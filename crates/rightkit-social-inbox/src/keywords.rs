//! Whole-word keyword matching for comment-to-DM automation rules.

/// Lower-case alphanumeric words of a text. Everything else separates words.
fn words(text: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_is_whole_words_in_order_and_case_insensitive() {
        assert!(keyword_matches("price", "PRICE please"));
        assert!(keyword_matches("price", "what is the price?"));
        assert!(!keyword_matches("price", "priceless"));
        assert!(keyword_matches("send link", "Please SEND  link!"));
        assert!(!keyword_matches("send link", "link send"));
        assert!(!keyword_matches("", "anything"));
    }
}
