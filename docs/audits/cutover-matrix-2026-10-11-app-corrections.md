# Cutover matrix 2026-10-11: corrections reported by app chats

The audit in `cutover-matrix-2026-10-11/` is left as its author wrote it. This file records what each app chat reported against its handoff on 2026-10-11. Each line is the app chat's statement, not re-verified by the RightKit lead unless marked. Use it when the matrix is regenerated.

## Cells the app says should be Y (audit read older source)

| App | Component | Audit | App's statement |
|---|---|---|---|
| HeardRight | rightkit-db | N | History on rightkit-db since 6fce0da0 (sqlcipher_store, sqlcipher_conn, migrations, queries, legacy_json_migration). |
| HeardRight | rightkit-secrets | N | All 7 sites on rightkit-secrets 0.1.3 via secret_store.rs since 6fce0da0. |
| HeardRight | rightkit-ort | N | Runtime binding and environment init on rightkit-ort since 6fce0da0. |
| HeardRight | rightkit-vad | N | Silero scoring on rightkit_vad::Silero on every platform (second-wave tree, uncommitted). |
| HeardRight | rightkit-transcript | N | File-transcription SRT/VTT already on rightkit-transcript; history_export.rs has no SRT/VTT (Json, Markdown, PlainText, Csv are product schema). Ruled local, mark Y. |
| HeardRight | rightkit-shell | N ("pinned app dependency") | No rightkit-shell dependency exists in src-tauri/Cargo.toml. |
| HeardRight | @rightkit/platform-ui | M | formatChord ported onto 0.2.7 (uncommitted tree; 124-case parity, every rendered chord identical); getPlatform ruled local. |
| MailRight | @rightkit/legal-ui | N | LegalGate 0.2.2 (18e0cc5). |
| MailRight | @rightkit/pdf-ui | N | PdfViewer 0.1.5, direct pdfjs-dist removed (18e0cc5). |
| MailRight | @rightkit/theme | N | applyTokens, contrastRatio, readableOn (bb55964, fab695c); presets and font picker ruled local. |
| MailRight | @rightkit/platform-ui | N | getPlatform (bb55964); shortcut registry ruled local. |
| MailRight | @rightkit/app-shell | N | Titlebar 0.5.1 (bc6e76f); rest blocked on ledger 90. |
| MailRight | @rightkit/settings-ui | N | SettingsPanel 0.3.4 (bc6e76f); per-tab sections blocked on ledger 90. |
| Pulse | rightkit-fs | N | Partly Y: =0.3.3, about 20 state writes through atomic_write_with, sha256_bytes, sweep_stale_temps. |
| Pulse | rightkit-logs | N | Partly Y: hub scan log on JsonlStore 0.2.1. |
| ScrapeRight | @rightkit/theme | N | Shared theme store for choice and OS listening (b07dc0b); paint() migrating. |
| ScrapeRight | rightkit-shell | N | Supervisor already on rightkit-process in the uncommitted tree; window/bootstrap wiring remains. |

## Ruled local (not a migration)

| App | Component | Reason |
|---|---|---|
| ScrapeRight | rightkit-http (TLS-fingerprint fallback) | Product policy; ledger 117. |
| ScrapeRight | rightkit-pdf (PDFium pipeline) | Ledger 42: rightkit-pdf stays PDFium-free. |
| HeardRight | rightkit-asr (CoreMlParakeet decode/display) | Kickoff ruling: ASR/detok policy local. |
| HeardRight | rightkit-models (model-transaction policy) | Kickoff ruling. |
| ViewRight | @rightkit/settings-ui | Locked UI; styled parts and useSettings stay local. |
| MailRight | @rightkit/doc-render | Untrusted email HTML needs sandboxed iframe, CSP, CID and remote-image rewriting. |
| MailRight | @rightkit/media-ui | Attachment preview has download-only safety rules; low priority. |

## Lines with no RightKit owner on that platform

| App | Audit line | Statement |
|---|---|---|
| Pulse | rightkit-http → mac ReleaseFeed.swift:50 | Swift URLSession; no Swift HTTP product exists. |
| Pulse | rightkit-process → mac ClaudeRestart.swift:107 | Swift Process; no Swift process product exists. |
| Pulse | hotkeys / settings / overlay on the Mac notch | CGEventTap hub, UserDefaults settings, NSPanel overlay: no Swift owner in RightKitSwift. |
