# rightkit-media-fetch

Media fetch: yt-dlp download and 16 kHz mono WAV normalize, playlist expansion, and a pure URL/file job classifier with a shared media-type table.

Moved into RightKit from ScrapeRight (scraperight-audio, scraperight-detect, scraperight-protocol ytdlp) on 2026-11-01.

## Extraction support (0.1.1)

Beyond download and classify, the crate now holds the platform-specific extraction logic behind host traits:

- `source_auth`, `x_post`, `tiktok`: sign-in state lookup (`AuthLayout`), X post parsing, TikTok slideshow parsing and slide download (`PublicWeb`).
- `playlist_batch`, `profile_batch`: playlist and profile batch orchestration with injected listers and queue submit (`BatchContext`); `BrowserLister` drives a signed-in browser through `rightkit_web_extract::browser::BrowserHost`.
- `carousel`: the Instagram carousel scrape (`CarouselEnv`).
- `extraction`: `ExtractionError`, error classification, scratch guards, media staging and publish, ffmpeg probe/trim/remux, and the routing and gating rules for audio, image, carousel and social jobs.
- `protocol`: the job, queue and batch wire types these modules share.
