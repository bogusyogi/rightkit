use super::restore_vocabulary_casing;

#[test]
fn restores_casing_and_prefers_longer_terms() {
    let terms = vec!["Cloudflare".to_string(), "Damned Designs".to_string()];
    assert_eq!(
        restore_vocabulary_casing("deploy on cloudflare for damned designs", &terms),
        "deploy on Cloudflare for Damned Designs",
    );
    // Whole-word only: don't touch a substring inside another word.
    assert_eq!(
        restore_vocabulary_casing("cloudflarey", &terms),
        "cloudflarey",
    );
    // No terms → unchanged.
    assert_eq!(restore_vocabulary_casing("hello", &[]), "hello");
}

#[test]
fn replacements_apply_whole_word_case_insensitive_longest_first() {
    use super::apply_replacements;
    let mut map = std::collections::HashMap::new();
    map.insert("in voice".to_string(), "invoice".to_string());
    map.insert("voice".to_string(), "Voice".to_string());
    assert_eq!(
        apply_replacements("send the In Voice today", &map),
        "send the invoice today",
    );
    // Whole-word only; literal replacement (no $-expansion).
    let mut lit = std::collections::HashMap::new();
    lit.insert("price".to_string(), "$99".to_string());
    assert_eq!(
        apply_replacements("the price is right", &lit),
        "the $99 is right"
    );
    assert_eq!(apply_replacements("priceless", &lit), "priceless");
    assert_eq!(
        apply_replacements("nothing", &std::collections::HashMap::new()),
        "nothing",
    );
}
