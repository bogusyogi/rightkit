use super::*;

pub(super) fn build_source_metadata(input: &EnrichmentInput<'_>) -> SourceMetadata {
    SourceMetadata {
        kind: input.kind.to_string(),
        domain: extract_domain(input.source_url),
        shortcode: input.shortcode.map(str::to_string),
        duration_s: input.duration_s,
        language: input.language.map(|s| s.to_string()),
    }
}

/// Lowercased host of an http(s) source URL without `www.`, credentials or port, when it has a
/// dot; `None` for a local file path or another scheme. The link normaliser's host rule.
pub(super) fn extract_domain(url: &str) -> Option<String> {
    let (_, host) = normalize_url(url.trim())?;
    let host = host.rsplit_once('@').map_or(host.as_str(), |(_, h)| h);
    let host = host.split(['?', '#']).next().unwrap_or_default();
    let host = host.split_once(':').map_or(host, |(h, _)| h);
    host.contains('.').then(|| host.to_string())
}

pub(super) fn build_tags(enrichment: &Enrichment) -> String {
    let mut tags: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Add modality/kind tag
    let kind = &enrichment.source_metadata.kind;
    if !kind.is_empty() && seen.insert(kind.clone()) {
        tags.push(kind.clone());
    }

    // Add domain
    if let Some(domain) = &enrichment.source_metadata.domain {
        if seen.insert(domain.clone()) {
            tags.push(domain.clone());
        }
    }

    // Add top keywords (up to 10)
    for kw in enrichment.keywords.iter().take(10) {
        let key = kw.term.to_ascii_lowercase();
        if key.len() >= 3 && seen.insert(key) {
            tags.push(kw.term.clone());
        }
    }

    // Add hashtags (up to 5)
    for tag in enrichment.hashtags.iter().take(5) {
        let key = tag.to_ascii_lowercase();
        if seen.insert(key) {
            tags.push(tag.clone());
        }
    }

    // Add mentions (up to 5)
    for mention in enrichment.mentions.iter().take(5) {
        let key = format!("@{}", mention.to_ascii_lowercase());
        if seen.insert(key) {
            tags.push(format!("@{}", mention));
        }
    }

    tags.join(",")
}
