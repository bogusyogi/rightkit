# rightkit-feed

RSS 2.0, RSS 1.0 (RDF) and Atom feed parsing for apps that import feed items as drafts or calendar entries. Catalog capability: `feed-parse` (declaration-only).

`parse(bytes, &Limits) -> Result<Feed, FeedError>` returns the feed title and link and its items (`id`, `title`, `link`, `published` text, `published_unix`, `summary`).

- Tolerant: mismatched or unclosed tags do not fail the parse; a document whose root is not `rss`, `rdf` or `feed` does (`FeedError::NotAFeed`). Invalid UTF-8 is replaced.
- No entity expansion: only `&lt; &gt; &amp; &quot; &apos;` and numeric references are decoded; a DOCTYPE (with any internal entity definitions) is skipped whole and never interpreted; CDATA is text.
- Bounded: `Limits` caps input bytes (5 MiB), items (500, then `Feed::truncated`), bytes per field (16 KiB) and element depth (64).
- Links are kept only when they are `http` or `https` URLs without whitespace or control characters. Atom links come from `href` of the `alternate` (or `rel`-less) `link`.
- Dates read to Unix seconds from RFC 3339 (through `rightkit-time`) and RFC 822 / 2822 (`Mon, 02 Jan 2006 15:04:05 +0000`, named US zones, numeric offsets).

## Not here

Fetching (use `rightkit-http`), HTML sanitising of summaries, JSON Feed, podcast and media extensions, feed discovery.
