use super::*;

pub(super) fn build_source_metadata(input: &EnrichmentInput<'_>) -> SourceMetadata {
    SourceMetadata {
        kind: input.kind.to_string(),
        domain: extract_domain(input.source_url),
        shortcode: None,
        duration_s: input.duration_s,
        language: input.language.map(|s| s.to_string()),
    }
}

pub(super) fn extract_domain(url: &str) -> Option<String> {
    let host = rightkit_media_fetch::detect::host(url)?;
    host.contains('.').then_some(host)
}

pub(super) fn is_instagram_kind(kind: &str) -> bool {
    matches!(kind, "reel" | "instagram_post" | "carousel")
}

pub(super) fn extract_shortcode(url: &str) -> Option<String> {
    rightkit_media_fetch::detect::instagram_shortcode(url)
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn domain_drops_www_port_and_requires_a_dot() {
        assert_eq!(
            extract_domain("https://www.Example.com:8443/a"),
            Some("example.com".into())
        );
        assert_eq!(extract_domain("https://localhost/a"), None);
        assert_eq!(extract_domain("ftp://example.com/"), None);
    }

    #[test]
    fn shortcode_uses_the_shared_rule_including_reels() {
        assert_eq!(
            extract_shortcode("https://www.instagram.com/reels/Abc1/"),
            Some("Abc1".into())
        );
        assert_eq!(
            extract_shortcode("https://www.instagram.com/p/Zz9/"),
            Some("Zz9".into())
        );
        assert_eq!(extract_shortcode("https://www.instagram.com/u/"), None);
    }
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
