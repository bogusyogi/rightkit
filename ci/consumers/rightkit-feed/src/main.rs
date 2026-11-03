// rightkit-feed 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// Parses feed documents held in memory: no network, file or process is touched.
use rightkit_feed::{parse, Feed, FeedError, FeedKind, Limits};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

fn parsed(input: &str, limits: &Limits) -> Feed {
    match parse(input.as_bytes(), limits) {
        Ok(feed) => feed,
        Err(error) => {
            eprintln!("FAIL: parse refused a valid feed: {error}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let limits = Limits::default();
    check(
        "default limits",
        limits.max_items == 500 && limits.max_bytes == 5 * 1024 * 1024,
    );

    // RSS 2.0: entity decoding, RFC 822 date to Unix seconds, non-http link dropped.
    let rss = parsed(
        r#"<?xml version="1.0"?><rss version="2.0"><channel>
<title>Tom &amp; Jerry</title><link>https://example.com/</link>
<item><title>One</title><link>https://example.com/1</link><guid>g1</guid>
<pubDate>Mon, 02 Jan 2006 15:04:05 +0000</pubDate><description>D1</description></item>
<item><title>Two</title><link>javascript:alert(1)</link></item>
</channel></rss>"#,
        &limits,
    );
    check("rss kind", rss.kind == FeedKind::Rss2);
    check(
        "rss title entity",
        rss.title.as_deref() == Some("Tom & Jerry"),
    );
    check(
        "rss link",
        rss.link.as_deref() == Some("https://example.com/"),
    );
    check("rss item count", rss.items.len() == 2 && !rss.truncated);
    check(
        "rss item fields",
        rss.items[0].id.as_deref() == Some("g1")
            && rss.items[0].title.as_deref() == Some("One")
            && rss.items[0].summary.as_deref() == Some("D1")
            && rss.items[0].link.as_deref() == Some("https://example.com/1"),
    );
    check(
        "rfc 822 date to unix seconds",
        rss.items[0].published_unix == Some(1_136_214_245),
    );
    check("unsafe link dropped", rss.items[1].link.is_none());

    // Atom: link in href, RFC 3339 date.
    let atom = parsed(
        r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>A</title>
<link rel="alternate" href="https://example.com/"/>
<entry><id>e1</id><title>T</title><link href="https://example.com/e1"/>
<published>2006-01-02T15:04:05Z</published><summary>S</summary></entry></feed>"#,
        &limits,
    );
    check("atom kind", atom.kind == FeedKind::Atom);
    check(
        "atom feed link from href",
        atom.link.as_deref() == Some("https://example.com/"),
    );
    check(
        "atom entry fields",
        atom.items.len() == 1
            && atom.items[0].id.as_deref() == Some("e1")
            && atom.items[0].link.as_deref() == Some("https://example.com/e1")
            && atom.items[0].summary.as_deref() == Some("S"),
    );
    check(
        "rfc 3339 date to unix seconds",
        atom.items[0].published_unix == Some(1_136_214_245),
    );

    // RSS 1.0 root is recognised.
    let rdf = parsed(
        r#"<rdf:RDF><channel><title>R</title></channel><item><title>I</title></item></rdf:RDF>"#,
        &limits,
    );
    check("rdf kind", rdf.kind == FeedKind::Rss1);

    // Bounds and refusals.
    let two_items = r#"<rss><channel><item><title>1</title></item><item><title>2</title></item></channel></rss>"#;
    let capped = parsed(
        two_items,
        &Limits {
            max_items: 1,
            ..Limits::default()
        },
    );
    check(
        "item cap truncates",
        capped.items.len() == 1 && capped.truncated,
    );
    check(
        "not a feed refused",
        parse(b"<html><body/></html>", &limits) == Err(FeedError::NotAFeed),
    );
    check(
        "empty refused",
        parse(b"  \n ", &limits) == Err(FeedError::Empty),
    );
    check(
        "too large refused",
        parse(
            two_items.as_bytes(),
            &Limits {
                max_bytes: 10,
                ..Limits::default()
            },
        ) == Err(FeedError::TooLarge),
    );
    check(
        "too deep refused",
        parse(
            b"<rss><a><b><c><d><e/></d></c></b></a></rss>",
            &Limits {
                max_depth: 3,
                ..Limits::default()
            },
        ) == Err(FeedError::TooDeep),
    );

    println!("OK rightkit-feed 0.1.0 (registry consumer, public API)");
}
