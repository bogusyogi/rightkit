# rightkit-pdf

Product-neutral PDF engine for Right Suite apps. Base: ViewRight `crates/pdf` (inspect, compose,
fonts, staged save). Features add Legion page merge/select (`merge`, default) and ScrapeRight text
extraction (`extract`).

Pure Rust, no Tauri, no PDFium. Source Serif 4 (SIL OFL 1.1) is bundled as WOFF2 in `assets/`.
See `tests/e2e.rs` for real-file round trips.
