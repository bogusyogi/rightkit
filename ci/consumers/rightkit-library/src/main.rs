//! Registry consumer for rightkit-library 0.1.1: builds against the published API only.
use rightkit_library::enrichment::{enrich, EnrichmentInput};
use rightkit_library::{HistoryRecord, Store};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let store = Store::open_in_memory();
    let rec = HistoryRecord {
        id: "consumer-1".into(),
        url: "https://example.com/clip".into(),
        kind: "article".into(),
        title: Some("Consumer".into()),
        lang: None,
        duration_s: None,
        created_at: "2026-11-01T00:00:00Z".into(),
        txt_path: None,
        md_path: None,
        srt_path: None,
        slide_dir: None,
        tags: None,
        snippet: None,
        media_path: None,
        media_meta: None,
        media_action: None,
        srt_text: None,
        truncated_at_s: None,
    };
    check(store.upsert(&rec).is_ok(), "a record is stored");
    check(
        store.get("consumer-1").ok().flatten().is_some(),
        "the stored record reads back",
    );
    let input = EnrichmentInput {
        source_url: "https://example.com/post",
        kind: "article",
        title: Some("Rust release notes"),
        text: "Read more at https://example.com/notes #rustlang",
        markdown_text: None,
        slide_sections: None,
        word_tokens: None,
        article_metadata: None,
        language: Some("en"),
        duration_s: None,
    };
    let out = enrich(&input);
    check(
        !out.links.is_empty(),
        "enrichment finds the link in the text",
    );
    let dir =
        std::env::temp_dir().join(format!("rightkit-library-consumer-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let settings = rightkit_library::HistorySettings {
        output_dir: dir.to_string_lossy().into_owned(),
        ..Default::default()
    };
    check(
        rightkit_library::history::get_history_page(&settings, None, None, None, None)
            .map(|page| page.rows.is_empty() && page.total == Some(0))
            .unwrap_or(false),
        "an empty library pages as empty with a zero total",
    );
    let _ = std::fs::remove_dir_all(&dir);
    println!("rightkit-library consumer: all checks passed");
}
