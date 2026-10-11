# ScrapeRight RightKit cutover matrix

Current source-only recheck at b07dc0baf979699d3b12acfa314756d37302b35c plus dirty worktree, against current RightKit source. Scope covers Windows/macOS Tauri + Rust engine, renderer/scripts/QA/release, iOS app/share extension & macOS credential helper.

## Coverage

- 97 catalog rows: **38 Y**, **8 N**, **2 M**, **49 NA**.
- Shared routes: Rust ASR/audio/db/framed-sidecar/fs/license/logs/media/models/OCR/ORT/playback/process/secrets/settings/sync/Tauri; npm app-shell, legal/onboarding, license/logs/media/settings/platform/updates, QA & release; Swift License/Diagnostics/Artifacts/Purchases/CloudSync/ImportQueue/AppleCredentials.
- Local routes: browser, durable jobs, SQLite search, Tauri shell, transcript export, theme paint, reader markdown rendering & iOS Parakeet speech.

## Mixed cutovers

- **cargo:rightkit-http**: `Client::stream_any` handles primary extraction; local `fetch_article_tls_fingerprint` remains active after direct/browser-header failure. Upstream fingerprinted TLS behavior or remove fallback.
- **cargo:rightkit-pdf**: shared text extraction is fallback; app-owned PDFium per-page extraction remains primary. Make shared path primary or upstream PDFium behavior before removing local path.

## Concrete migration tasks

1. Replace root Cargo.toml local RightKit path patches with published exact versions before release.
2. Decide HTTP ownership & eliminate duplicate TLS transport after parity evidence.
3. Decide PDF ownership & eliminate duplicate PDFium extraction after parity evidence.
4. Add usable bridges for local browser, jobs, search, shell, transcript, theme paint, reader rendering & iOS speech before adopting corresponding RightKit owners; preserve product policy.
5. Publish supported iOS RightKitSend binding mapping `Service::start`/`snapshot`/`send`/`respond` into `SendEngine`, then replace `StubSendEngine`.
6. Keep Swift package pin at RightKitSwift 0.4.0 & verify published tag.

## Evidence anchors

Primary evidence: crates/engine/src/extraction/article.rs:121,183; crates/engine/src/extraction_documents.rs:40,54; crates/engine/src/extraction/article_tls.rs:12; crates/engine/src/queue.rs:32,470; crates/store/src/lib.rs:316; src-tauri/src/playback.rs:10,12; src/views/DetailReader.tsx:369; src/lib/theme.ts:1; src/main.tsx:5; ios-native/ScrapeRight/ScrapeRightApp.swift:29; ios-native/ScrapeRight/Send/SendEngine.swift:103,198; ios-native/ScrapeRight/Extract/ParakeetTranscriber.swift:25; ios-native/project.yml:20-22,100-116. Full evidence for all rows is in sibling JSON.

Adversarial review challenged shared callers, mixed overlap, local owner matches, incompatible targets, manifest-only declarations & Rust QA versus npm QA routing; rechecked HTTP fallback, corrected unbound iOS send to NA & classified live local theme paint as N.
