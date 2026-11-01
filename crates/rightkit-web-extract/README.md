# rightkit-web-extract

Article extraction without transport: readable-text parsing, title and redirect resolution, blocked-page detection, BM25 fit filtering, and extraction-ladder decisions.

Moved into RightKit from ScrapeRight (engine extraction/article.rs, article_parse.rs, content_filters.rs) on 2026-11-01.

## Fetch tiers (0.1.1)

`fetch::ArticleFetcher` runs the ladder (direct HTTP, browser-header request, TLS fingerprint, headless browser) over two host traits: `net::ArticleNet` (public-web address policy and HTTP client) and `browser::BrowserHost` (browser sessions and host pinning). `tls::TlsClientConfig` locates and gates a tls-client shared library; the host decides which library paths are trusted.
