use super::*;

pub(super) fn extract_keywords(text: &str) -> Vec<EnrichedKeyword> {
    let lower = text.to_ascii_lowercase();
    let mut freq: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

    // Tokenize: split on non-alphanumeric, keep tokens >= 3 chars
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|t| t.len() >= 3 && !STOPWORDS.contains(t))
        .collect();

    let total_tokens = tokens.len() as f64;
    if total_tokens == 0.0 {
        return Vec::new();
    }

    // Unigram frequency (cap each token at 3 occurrences to prevent spam)
    for tok in &tokens {
        let e = freq.entry(tok.to_string()).or_insert(0);
        if *e < 3 {
            *e += 1;
        }
    }

    // Bigram frequency
    let mut bigram_freq: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for w in tokens.windows(2) {
        if STOPWORDS.contains(&w[0]) || STOPWORDS.contains(&w[1]) {
            continue;
        }
        let bigram = format!("{} {}", w[0], w[1]);
        let e = bigram_freq.entry(bigram).or_insert(0);
        if *e < 3 {
            *e += 1;
        }
    }

    // Score: unigrams weighted by frequency relative to doc length,
    // bigrams get a 1.5x bonus for being more specific.
    let mut scored: Vec<EnrichedKeyword> = Vec::new();

    for (term, count) in &freq {
        let score = (*count as f64) / total_tokens;
        scored.push(EnrichedKeyword {
            term: term.clone(),
            score,
        });
    }
    for (term, count) in &bigram_freq {
        let score = (*count as f64) / total_tokens * 1.5;
        scored.push(EnrichedKeyword {
            term: term.clone(),
            score,
        });
    }

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(25);
    scored
}

pub(super) fn extract_claims(sentences: &[String]) -> Vec<EnrichedClaim> {
    let mut claims = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for sentence in sentences {
        let lower = sentence.to_ascii_lowercase();
        let trimmed = sentence.trim();
        if trimmed.len() < 15 || trimmed.len() > 500 {
            continue;
        }

        // Check for number + comparison/promise signals
        let has_number = trimmed.chars().any(|c| c.is_ascii_digit());
        let has_comparison = lower.contains("more than")
            || lower.contains("less than")
            || lower.contains("up to")
            || lower.contains("as much as")
            || lower.contains("better than")
            || lower.contains("faster than")
            || lower.contains("cheaper than");
        let has_superlative = lower.contains("best")
            || lower.contains("worst")
            || lower.contains("fastest")
            || lower.contains("largest")
            || lower.contains("smallest")
            || lower.contains("most")
            || lower.contains("least")
            || lower.contains("top ");
        let has_promise = lower.contains("guarantee")
            || lower.contains("promise")
            || lower.contains("will ")
            || lower.contains("won't")
            || lower.contains("never fail");
        let has_signal_word = CLAIM_SIGNALS.iter().any(|s| lower.contains(s));

        // A claim needs at least one strong signal
        let is_claim = (has_number && (has_comparison || has_superlative || has_promise))
            || (has_signal_word && (has_number || has_comparison || has_superlative))
            || (has_promise && has_signal_word);

        if is_claim {
            let key = trimmed.to_ascii_lowercase();
            if seen.insert(key) {
                let confidence = if has_number && has_comparison {
                    "medium"
                } else {
                    "low"
                };
                claims.push(EnrichedClaim {
                    text: trimmed.to_string(),
                    confidence: confidence.to_string(),
                });
            }
        }
    }

    claims.truncate(20);
    claims
}

pub(super) fn extract_ctas(sentences: &[String]) -> Vec<EnrichedCta> {
    let mut ctas = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for sentence in sentences {
        let lower = sentence.to_ascii_lowercase();
        let trimmed = sentence.trim();
        if trimmed.len() < 5 || trimmed.len() > 300 {
            continue;
        }

        let matched_verb = CTA_VERBS.iter().find(|v| lower.contains(**v));
        if let Some(verb) = matched_verb {
            let key = format!("{}:{}", verb, trimmed.to_ascii_lowercase());
            if seen.insert(key) {
                // Imperative sentences (starting with a verb) get medium confidence
                let starts_with_verb = lower.starts_with(verb);
                let has_imperative_marker = lower.contains("don't forget to")
                    || lower.contains("make sure to")
                    || lower.contains("be sure to")
                    || lower.contains("you should")
                    || lower.contains("you need to");
                let confidence = if starts_with_verb || has_imperative_marker || lower.contains("!")
                {
                    "medium"
                } else {
                    "low"
                };
                ctas.push(EnrichedCta {
                    text: trimmed.to_string(),
                    confidence: confidence.to_string(),
                });
            }
        }
    }

    ctas.truncate(20);
    ctas
}

pub(super) fn extract_sections(input: &EnrichmentInput<'_>) -> Vec<EnrichedSection> {
    let mut sections = Vec::new();
    let text = input.text;

    // For carousels, parse [caption] and [slide N] markers
    let mut current_label = String::from("body");
    let mut current_words = 0usize;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("[caption]") {
            if current_words > 0 || !sections.is_empty() {
                sections.push(EnrichedSection {
                    label: current_label.clone(),
                    word_count: current_words,
                });
            }
            current_label = "caption".to_string();
            current_words = 0;
            continue;
        }
        // [slide N]
        if trimmed.to_ascii_lowercase().starts_with("[slide ") && trimmed.ends_with(']') {
            if current_words > 0 || !sections.is_empty() {
                sections.push(EnrichedSection {
                    label: current_label.clone(),
                    word_count: current_words,
                });
            }
            current_label = trimmed[1..trimmed.len() - 1].to_string();
            current_words = 0;
            continue;
        }
        // [page N] for scanned PDFs
        if trimmed.to_ascii_lowercase().starts_with("[page ") && trimmed.ends_with(']') {
            if current_words > 0 || !sections.is_empty() {
                sections.push(EnrichedSection {
                    label: current_label.clone(),
                    word_count: current_words,
                });
            }
            current_label = trimmed[1..trimmed.len() - 1].to_string();
            current_words = 0;
            continue;
        }
        current_words += trimmed.split_whitespace().count();
    }
    if current_words > 0 || sections.is_empty() {
        sections.push(EnrichedSection {
            label: current_label,
            word_count: current_words,
        });
    }

    // If there are word tokens with timestamps, we could group by time segments.
    // For v1, we keep it simple and use the text-based sections above.
    // If no section markers were found, the whole text is one "body" section.

    sections
}
