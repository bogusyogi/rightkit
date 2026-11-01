# Changelog

## 0.1.2

- `transport` feature: `HttpNet`, an `ArticleNet` over rightkit-http. The article, browser, TLS and filter tiers were already in this crate.

## 0.1.1

- Added the fetch tiers (`fetch`, `net`, `browser`, `tls`, `tls_ffi`) behind the `ArticleNet` and `BrowserHost` traits. Additive: `article_extract_from_html_filtered` joins `article_extract_from_html`.

## 0.1.0 (unreleased)

- First RightKit release, moved from ScrapeRight (engine extraction/article.rs, article_parse.rs, content_filters.rs).
