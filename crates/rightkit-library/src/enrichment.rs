//! Native non-LLM enrichment layer.
//!
//! Runs after each successful extraction/transcription/OCR job and produces
//! deterministic structured metadata without calling any LLM or web service.
//! Uses rules, regexes, and lightweight statistics on the extracted text.
//!
//! If enrichment fails for any reason the caller should log and continue —
//! enrichment must never block artifact writing or turn a failed job into
//! success.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Input for the enrichment pass. All fields except `text` are optional hints
/// that improve extraction quality when available.
#[derive(Debug, Clone)]
pub struct EnrichmentInput<'a> {
    /// Original source URL (or local file path).
    pub source_url: &'a str,
    /// The host's kind tag (e.g. "youtube", "reel", "carousel", "article"); copied to
    /// [`SourceMetadata::kind`] and the tags. The library gives no kind a meaning.
    pub kind: &'a str,
    /// The source's short code when the host knows one (for example an Instagram post code from
    /// `rightkit_media_fetch::detect::instagram_shortcode`); copied to
    /// [`SourceMetadata::shortcode`].
    pub shortcode: Option<&'a str>,
    /// Title or caption, if known.
    pub title: Option<&'a str>,
    /// Extracted plain text (transcript, OCR, article body, etc.).
    pub text: &'a str,
    /// Optional markdown variant of the text.
    pub markdown_text: Option<&'a str>,
    /// Optional slide/section markers for carousel content (parsed from text).
    pub slide_sections: Option<&'a [&'a str]>,
    /// Optional word-level timestamps (text, start, end).
    pub word_tokens: Option<&'a [WordToken]>,
    /// Optional article metadata (domain, title, source).
    pub article_metadata: Option<&'a ArticleMeta>,
    /// Detected language code (e.g. "en").
    pub language: Option<&'a str>,
    /// Duration in seconds for audio/video jobs.
    pub duration_s: Option<f64>,
    /// Where [`SourceMetadata::domain`] (and the domain tag) comes from.
    pub domain: SourceDomain<'a>,
}

/// Where the enrichment pass takes the source's domain from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SourceDomain<'a> {
    /// The library's own rule: the lowercased host of an `http(s)://` source URL without `www.`,
    /// credentials or port, when it has a dot; none for anything else.
    #[default]
    FromUrl,
    /// The host's own value, used as given (`None` records no domain). For a host whose shipped
    /// rule differs, for example one that also reads a scheme-less `example.com/page`.
    Given(Option<&'a str>),
}

#[derive(Debug, Clone)]
pub struct WordToken {
    pub text: String,
    pub start: f32,
    pub end: f32,
}

#[derive(Debug, Clone)]
pub struct ArticleMeta {
    pub title: Option<String>,
    pub source: Option<String>,
}

/// The enrichment output. All vectors are sorted and deduplicated.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Enrichment {
    pub links: Vec<EnrichedLink>,
    pub hashtags: Vec<String>,
    pub mentions: Vec<String>,
    pub numbers: Vec<EnrichedNumber>,
    pub dates: Vec<EnrichedDate>,
    pub keywords: Vec<EnrichedKeyword>,
    pub possible_claims: Vec<EnrichedClaim>,
    pub possible_ctas: Vec<EnrichedCta>,
    pub sections: Vec<EnrichedSection>,
    pub source_metadata: SourceMetadata,
    /// Flat tag string for HistoryRecord (comma-separated).
    #[serde(skip_serializing)]
    pub tags: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedLink {
    pub url: String,
    pub normalized_url: String,
    pub domain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedNumber {
    pub raw: String,
    pub value: Option<f64>,
    pub sentence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedDate {
    pub raw: String,
    pub iso: Option<String>,
    pub sentence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedKeyword {
    pub term: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedClaim {
    pub text: String,
    pub confidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedCta {
    pub text: String,
    pub confidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedSection {
    pub label: String,
    pub word_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SourceMetadata {
    pub kind: String,
    pub domain: Option<String>,
    pub shortcode: Option<String>,
    pub duration_s: Option<f64>,
    pub language: Option<String>,
}

// ---------------------------------------------------------------------------
// Stopwords (English-heavy, common across European languages)
// ---------------------------------------------------------------------------

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
    "from", "is", "it", "as", "be", "was", "are", "were", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "could", "should", "may", "might", "shall", "can", "not",
    "no", "so", "if", "then", "than", "that", "this", "these", "those", "i", "me", "my", "we",
    "our", "you", "your", "he", "she", "they", "them", "his", "her", "its", "what", "which", "who",
    "whom", "how", "when", "where", "why", "all", "each", "every", "both", "few", "more", "most",
    "other", "some", "such", "only", "own", "same", "just", "about", "above", "after", "again",
    "also", "any", "because", "before", "below", "between", "both", "come", "day", "even", "first",
    "get", "give", "go", "here", "him", "into", "know", "like", "look", "make", "many", "new",
    "now", "old", "one", "out", "over", "people", "say", "see", "shall", "still", "take", "tell",
    "think", "time", "two", "up", "us", "use", "very", "want", "way", "well", "work", "year",
    "much", "going", "right", "thing", "actually", "really", "there", "their", "them", "don",
    "doesn", "didn", "won", "wouldn", "couldn", "shouldn", "isn", "aren", "wasn", "weren", "hasn",
    "haven", "hadn", "let", "yeah", "yes", "ok", "okay", "got", "gonna", "gotta", "wanna", "kinda",
    "sorta", "lot", "lots",
];

// ---------------------------------------------------------------------------
// Claim signal words
// ---------------------------------------------------------------------------

const CLAIM_SIGNALS: &[&str] = &[
    "without",
    "how to",
    "best",
    "fastest",
    "free",
    "guaranteed",
    "secret",
    "never",
    "always",
    "proven",
    "increase",
    "decrease",
    "reduce",
    "double",
    "triple",
    "million",
    "billion",
    "percent",
    "percent",
    "save",
    "earn",
    "make money",
    "lose weight",
    "top",
    "number one",
    "#1",
    "ultimate",
    "complete",
    "perfect",
    "unlimited",
    "instant",
    "immediately",
    "overnight",
    "hack",
    "trick",
    "cheat",
    "hacks",
    "tricks",
    "mistake",
    "mistakes",
    "reasons why",
    "you need",
    "you must",
    "everyone",
    "nobody",
];

const CTA_VERBS: &[&str] = &[
    "click",
    "download",
    "sign up",
    "signup",
    "subscribe",
    "comment",
    "dm",
    "like",
    "share",
    "follow",
    "join",
    "try",
    "get started",
    "learn more",
    "check out",
    "shop",
    "buy",
    "order",
    "register",
    "apply",
    "book",
    "save",
    "watch",
    "listen",
    "read more",
    "tap",
    "swipe",
    "visit",
    "grab",
];

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Enrich extracted text with deterministic metadata. Returns a default
/// (empty) enrichment if the input text is empty — never errors.
pub fn enrich(input: &EnrichmentInput<'_>) -> Enrichment {
    let text = input.text.trim();
    if text.is_empty() {
        let mut e = Enrichment::default();
        e.source_metadata = build_source_metadata(input);
        e.tags = build_tags(&e);
        return e;
    }

    let links = extract_links(text);
    let (hashtags, mentions) = extract_hashtags_and_mentions(text);
    let sentences = split_sentences(text);
    let numbers = extract_numbers(text, &sentences);
    let dates = extract_dates(text, &sentences);
    let keywords = extract_keywords(text);
    let possible_claims = extract_claims(&sentences);
    let possible_ctas = extract_ctas(&sentences);
    let sections = extract_sections(input);

    let source_metadata = build_source_metadata(input);

    let mut enrichment = Enrichment {
        links,
        hashtags,
        mentions,
        numbers,
        dates,
        keywords,
        possible_claims,
        possible_ctas,
        sections,
        source_metadata,
        tags: String::new(),
    };
    enrichment.tags = build_tags(&enrichment);
    enrichment
}

// ---------------------------------------------------------------------------
// Extraction helpers
// ---------------------------------------------------------------------------

#[path = "enrichment_content.rs"]
mod enrichment_content;
#[path = "enrichment_primitives.rs"]
mod enrichment_primitives;
#[path = "enrichment_source.rs"]
mod enrichment_source;
#[path = "enrichment_text.rs"]
mod enrichment_text;

use enrichment_content::{extract_claims, extract_ctas, extract_keywords, extract_sections};
use enrichment_primitives::{
    extract_dates, extract_hashtags_and_mentions, extract_links, extract_numbers, normalize_url,
};
use enrichment_source::{build_source_metadata, build_tags};
use enrichment_text::{
    cached_regex, find_containing_sentence, regex_captures_iter, split_sentences,
};
