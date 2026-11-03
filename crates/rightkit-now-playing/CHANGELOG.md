# Changelog

## 0.1.0 (unpublished)

Issue #959 (now-playing and media-keys owner).

- New crate over `souvlaki` 0.8.3: `NowPlaying::register` (one registration per process), `set_track`, `set_state`, `set_volume` (MPRIS only), `clear`; `Track`, `Artwork` (URL or local file), `State`, `Command`, `Config`.
- The one-owner rule is enforced: `Source::WebView` is refused (`Error::WebViewOwnsKeys`), because a system web view playing media owns the keys through the page's media session.
- Decision: souvlaki alone serves Linux (its backend is MPRIS, including volume), so `mpris-server` is not added.
