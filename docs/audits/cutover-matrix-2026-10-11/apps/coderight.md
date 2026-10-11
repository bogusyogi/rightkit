# CodeRight RightKit cutover matrix

Fresh source audit at `011f0d68de1572b8996013909311d05482e6bbd0`; 97/97 catalog rows covered: **Y 22, M 3, N 16, NA 56**. Scope includes Tauri desktop, Rust engine/daemon, web renderer, iOS app, widget, & File Provider targets. No builds or runtime qualification were run.

Shared production routes cover Rust browser, cache, DB, filesystem, license, logs, MCP, OCR, ONNX, sandbox, secrets, shell, Tauri, & watch; Tauri/web legal, license, logs, release, shell, Tauri, & updates; iOS `RightKitLicense`.

Mixed rows require first attention: `cargo:rightkit-http` (move OpenAI-compatible SSE transport to `AsyncClient::stream` while retaining CodeRight tool/retry policy), `cargo:rightkit-oauth` (move OpenAI PKCE callback/token exchange to `OAuthClient`, retaining account & fallback policy), & `cargo:rightkit-process` (replace detached `coderight serve` auto-start only; retain daemon health & reload saga). Consolidate only after target-qualified behavior checks.

Local cutover candidates are embeddings, provider LLM transport, FFmpeg/media, model management, search query handling, settings persistence, desktop shell/i18n/legal/onboarding/settings/theme, plus iOS import queue, live activity, speech, & credentials. `@rightkit/settings-ui` now cites the rendered `AppFrame` → `SettingsModal` route. For each N row, compare owner API in JSON, preserve CodeRight policy, migrate only where contracts match, & document intentional ownership otherwise.

NA rows now identify absent owner-specific targets or host bridges from active CodeRight sources; `rightkit-watch` remains Y through engine filesystem-watch route. Manifest pins & direct callers remain delivery evidence; Y/M rows still need package/version/runtime qualification before release signoff.
