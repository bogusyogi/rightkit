# MailRight — RightKit migration handoff

Source root: /Volumes/D/claude/mailright

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/mailright.json)

## rightkit-http — M

M: OAuth refresh already executes through rightkit_http::Client, while Google API, profile, AI, remote-image & translation callers still execute direct reqwest clients; shared & local HTTP routes are both live.

Route direct Google/profile/AI/remote-image/translation callers through rightkit-http async client; preserve MailRight auth refresh, Retry-After handling, redirects & image normalization, then remove direct reqwest construction.

- Local: [Google::http](/Volumes/D/claude/mailright/src-tauri/src/google.rs:126) — Google API path constructs reqwest::Client; same direct-client pattern remains in profile, AI, remote-image & translation callers.
- Shared: [Client::new](/Volumes/D/claude/mailright/src-tauri/src/oauth.rs:6) — OAuth refresh path actively uses rightkit-http Client.
- Owner: [Client](/Volumes/D/claude/rightkit/crates/rightkit-http/src/lib.rs:1) — Shared timeout/retry/net-policy client.

Delivery: Shared + local implementations are both active; current dependency/version facts do not change M.

## rightkit-llm — N

N: MailRight's Pro AI action uses its own OpenAI-compatible request/response path, while rightkit-llm owns shared BYOK provider streaming.

Route commands/pro.rs & ai.rs through rightkit-llm; preserve MailRight provider URL/model/key policy & action response schema, then remove local chat/request parsing.

- Local: [chat](/Volumes/D/claude/mailright/src-tauri/src/ai.rs:48) — Live AI command builds provider request/response locally; ai.rs:72 owns its reqwest client.
- Owner: [LLM client exports](/Volumes/D/claude/rightkit/crates/rightkit-llm/src/lib.rs:1) — Shared BYOK provider/streaming client is the migration boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## rightkit-mcp — N

N: MailRight actively serves loopback MCP routes from its own axum task; rightkit-mcp supplies a shared MCP host/client boundary that is not called.

Adopt rightkit-mcp for loopback transport/auth & tool registration; preserve MailRight search/thread/label policy & schemas, then delete local axum routes.

- Local: [spawn](/Volumes/D/claude/mailright/src-tauri/src/mcp.rs:187) — Live loopback server owns /tools, /search, /thread & /labels routes, token admission & transport.
- Owner: [MCP exports](/Volumes/D/claude/rightkit/crates/rightkit-mcp/src/lib.rs:1) — Shared MCP client/server host is the candidate transport boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## rightkit-qa — N

N: MailRight runs app-specific frontend/native checks with its own Vite launcher & Tauri WebDriver feature; rightkit-qa is not invoked.

Wire MailRight frontend/native suites into rightkit-qa harness, preserving hidden QA data-dir isolation & Tauri WebDriver routes, then remove duplicate launcher/evidence helpers.

- Local: [test:frontend](/Volumes/D/claude/mailright/package.json:24) — Live package test script selects MailRight frontend checks; Cargo feature at src-tauri/Cargo.toml:22 adds Tauri WebDriver.
- Owner: [QA exports](/Volumes/D/claude/rightkit/crates/rightkit-qa/src/lib.rs:1) — Shared QA harness/evidence API is the candidate boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## rightkit-search — N

N: MailRight search is Gmail-specific SQL/FTS with its own query fields, predicates & snippets; rightkit-search's generic query/FTS compiler is unused.

Use rightkit-search lexer/AST/predicate compiler behind MailRight Gmail field mapping; preserve SQL schema, labels & snippet policy, then remove duplicate parser/predicate code.

- Local: [search_threads](/Volumes/D/claude/mailright/src-tauri/src/db/search.rs:76) — Live caller parses Gmail fields, applies SQL/FTS predicates & formats thread snippets locally.
- Owner: [query exports](/Volumes/D/claude/rightkit/crates/rightkit-search/src/lib.rs:1) — Shared query language/FTS compiler is the candidate parsing boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## rightkit-settings — N

N: MailRight persists account, Gmail, AI & legal settings through its SQLCipher table/repository; rightkit-settings versioned persistence/events are not on this route.

Adopt rightkit-settings migration/default/change-event core behind MailRight SQLCipher adapter; preserve account/key/legal namespaces & encryption policy, then remove duplicate persistence.

- Local: [setting_get](/Volumes/D/claude/mailright/src-tauri/src/runtime/repos/sqlite/auxiliary.rs:4) — Live read path resolves MailRight settings from its SQLCipher repository; setting_set is at line 13.
- Owner: [settings exports](/Volumes/D/claude/rightkit/crates/rightkit-settings/src/lib.rs:1) — Shared versioned settings persistence is the candidate storage/event boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## @rightkit/app-shell — N

N: MailRight App.tsx owns React frame/layout & TitleBar owns the desktop shell slots; @rightkit/app-shell frame/commands/shortcuts exports are unused.

Migrate frame/navigation/shortcut/toast primitives to app-shell; preserve MailRight app switcher, menu & TitleBar slots, then remove duplicate generic frame code.

- Local: [App](/Volumes/D/claude/mailright/src/App.tsx:32) — Live React shell composes MailRight navigation/content slots locally.
- Local: [TitleBar](/Volumes/D/claude/mailright/src/components/TitleBar.tsx:29) — Live titlebar is part of app frame & owns window controls.
- Owner: [exports](/Volumes/D/claude/rightkit/packages/app-shell/src/index.ts:1) — Shared React frame/commands/shortcuts API is the candidate shell boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## @rightkit/doc-render — N

N: EmailFrame actively sanitizes untrusted Gmail HTML, applies CSP iframe rules & implements find; doc-render HtmlRenderer/sanitizeHtml is not called.

Use HtmlRenderer/sanitizeHtml for untrusted email content; preserve MailRight image trust/data-URL policy & same-origin iframe find adapters, then remove duplicate renderer.

- Local: [prepareBody](/Volumes/D/claude/mailright/src/components/EmailFrame.tsx:33) — Live email renderer owns sanitization, iframe/CSP & find integration for Gmail content.
- Owner: [HtmlRenderer](/Volumes/D/claude/rightkit/packages/doc-render/src/index.ts:1) — Shared sanitized document renderer is the candidate render boundary.
- Owner: [sanitizeHtml](/Volumes/D/claude/rightkit/packages/doc-render/src/index.ts:4) — Shared sanitizer export is the candidate content boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## @rightkit/legal-ui — N

N: MailRight LegalGate/LegalAcceptance owns acceptance screen UX while shared legal-ui is not rendered; legal receipt/schema policy stays app-owned.

Migrate gate/acceptance visuals to legal-ui; preserve @rightkit/legal manifest hash, receipt validation & Tauri IPC policy, then remove duplicate gate UI.

- Local: [LegalGate](/Volumes/D/claude/mailright/src/components/LegalGate.tsx:15) — Live gate fetches receipt & renders MailRight LegalAcceptance screen.
- Local: [LegalAcceptance](/Volumes/D/claude/mailright/src/components/LegalAcceptance.tsx:1) — Live acceptance form owns MailRight legal presentation & callback.
- Owner: [exports](/Volumes/D/claude/rightkit/packages/legal-ui/src/index.ts:1) — Shared legal gate screen is the candidate presentation boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## @rightkit/media-ui — N

N: MailRight attachment preview modal renders live image/attachment previews; shared media-ui image viewer/player is not called.

Adopt shared image/attachment viewer surface; preserve Gmail attachment trust, size/type limits & download policy, then remove duplicate image viewer UI.

- Local: [AttachmentPreviewModal](/Volumes/D/claude/mailright/src/components/AttachmentPreviewModal.tsx:246) — Live attachment viewer owns preview modal, image sizing & Gmail attachment state.
- Owner: [MediaPlayer/ImageViewer](/Volumes/D/claude/rightkit/packages/media-ui/src/index.ts:1) — Shared media/image surface is the candidate viewer boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## @rightkit/pdf-ui — N

N: AttachmentPdfPreview is a live pdfjs-dist reader with MailRight attachment/password/limit behavior; shared PdfViewer is not called.

Adopt PdfViewer worker/password/limit configuration; preserve attachment bytes, password UX & 30-page cap, then remove local pdfjs rendering.

- Local: [loadPdfJs](/Volumes/D/claude/mailright/src/components/AttachmentPdfPreview.tsx:15) — Live attachment reader loads local pdfjs-dist & owns PDF preview behavior.
- Owner: [PdfViewer](/Volumes/D/claude/rightkit/packages/pdf-ui/src/index.ts:1) — Shared PDF reader surface is the candidate frontend viewer boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## @rightkit/platform-ui — N

N: MailRight shortcut registry & custom TitleBar own Gmail actions/window affordances; shared platform/shortcut primitives are unused.

Map shared platform/shortcut primitives around MailRight Gmail action registry; preserve focus guards & Tauri/mac traffic-light behavior, then remove duplicate generic platform code.

- Local: [shortcut registry](/Volumes/D/claude/mailright/src/lib/shortcuts.ts:1) — Live keyboard registry maps Gmail actions & focus guards locally.
- Local: [minimizeWindow](/Volumes/D/claude/mailright/src/components/TitleBar.tsx:49) — Live TitleBar calls direct Tauri window APIs for minimize/maximize/close.
- Owner: [resolveQaPlatform](/Volumes/D/claude/rightkit/packages/platform-ui/src/index.ts:1) — Shared platform/shortcut/window primitives are the candidate boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## @rightkit/qa — N

N: MailRight QA launcher starts Vite & native smoke/WebDriver coverage from app scripts; shared @rightkit/qa evidence/browser helpers are not invoked.

Use shared QA loopback/evidence/guards from MailRight scripts; preserve hidden data-dir & Tauri WebDriver setup, then remove duplicate launcher/evidence helpers.

- Local: [startVite](/Volumes/D/claude/mailright/scripts/qa-browser.mjs:50) — Live QA launcher owns Vite startup, hidden data-dir & browser/native smoke orchestration.
- Owner: [exports](/Volumes/D/claude/rightkit/packages/qa/src/index.ts:1) — Shared QA evidence/browser helpers are the candidate harness boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## @rightkit/settings-ui — N

N: SettingsView owns MailRight settings frame/sections; shared SettingsPanel is not rendered, while Gmail/account policy remains app-specific.

Adopt SettingsPanel for generic appearance/updates/license/legal/diagnostics; keep MailRight Gmail/account/compose sections & IPC, then remove duplicate generic frame.

- Local: [SettingsView](/Volumes/D/claude/mailright/src/views/SettingsView.tsx:23) — Live settings view composes MailRight sections & IPC locally.
- Owner: [SettingsPanel](/Volumes/D/claude/rightkit/packages/settings-ui/src/index.ts:1) — Shared settings frame/sections are the candidate generic UI boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

## @rightkit/shell — N

N: MailRight renderer calls direct Tauri window APIs & local tray/autostart IPC; @rightkit/shell createShell typed window/tray/deep-link/autostart bridge is unused.

Migrate TitleBar & local shell IPC to createShell; map noteWindowHiddenForIdle behavior, preserve MailRight custom titlebar/mac traffic-light policy, then remove direct window/tray/autostart wrappers.

- Local: [getCurrentWindow](/Volumes/D/claude/mailright/src/components/TitleBar.tsx:1) — Live renderer imports direct @tauri-apps/api/window for minimize/maximize/close.
- Local: [Desktop shell](/Volumes/D/claude/mailright/src/lib/ipc/mail.ts:238) — Live local IPC owns tray notifications & autostart commands.
- Owner: [createShell](/Volumes/D/claude/rightkit/packages/shell/src/index.ts:228) — Shared typed shell bridge exposes window/tray/deepLinks/autostart APIs.

## @rightkit/theme — N

N: MailRight theme.ts owns active theme tokens/runtime; shared @rightkit/theme token runtime is not called.

Feed MailRight brand presets into shared theme runtime/checkers; preserve exact tokens & contrast behavior, then remove local generic token application.

- Local: [THEMES](/Volumes/D/claude/mailright/src/lib/theme.ts:1) — Live theme presets/tokens & application are app-owned.
- Owner: [exports](/Volumes/D/claude/rightkit/packages/theme/src/index.ts:1) — Shared brand/theme token runtime/checkers are the candidate boundary.

Delivery: No shared integration in current checkout; migration is a source cutover task.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
