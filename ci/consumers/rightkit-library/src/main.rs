//! Registry consumer for rightkit-library 0.2.0: builds against the published API only.
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
        shortcode: Some("Cx1_ab"),
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
    check(
        out.source_metadata.shortcode.as_deref() == Some("Cx1_ab")
            && out.source_metadata.domain.as_deref() == Some("example.com")
            && out.source_metadata.kind == "article",
        "the host's kind and short code are kept; the domain comes from the source URL",
    );
    let local = enrich(&EnrichmentInput {
        source_url: "clip.mp4",
        kind: "local_file",
        shortcode: None,
        ..input
    });
    check(
        local.source_metadata.domain.is_none(),
        "a local file name is not a domain",
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
    // The library defines no kinds: a tag it has never seen reads back as written.
    let indexed = Store::open(&dir.join("history.db")).expect("open the library index");
    check(
        indexed
            .upsert(&HistoryRecord {
                id: "consumer-2".into(),
                kind: "podcast_episode".into(),
                ..rec.clone()
            })
            .is_ok(),
        "a record with a host-defined kind is stored",
    );
    drop(indexed);
    check(
        rightkit_library::history::get_history_page(&settings, None, None, None, None)
            .map(|page| page.rows.len() == 1 && page.rows[0].kind == "podcast_episode")
            .unwrap_or(false),
        "the history page returns the host's kind tag unchanged",
    );
    let _ = std::fs::remove_dir_all(&dir);
    println!("rightkit-library consumer: all checks passed");
}
