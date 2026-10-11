# MailRight RightKit cutover

Fresh source-only audit at `43882ebc1f52d460c91e3871550e40cd5928e808` (2026-10-11). Scope covers Tauri desktop backend/frontend, Windows x64, & Apple Silicon macOS; no Swift/iOS/watchOS targets.

Result: 17 Y, 15 N, 1 M, 64 NA.

Y rows are live shared routes:

- Cargo: `rightkit-cache`, `rightkit-db`, `rightkit-embed`, `rightkit-fs` (through DB), `rightkit-license`, `rightkit-logs`, `rightkit-oauth`, `rightkit-ort` (through embed), `rightkit-secrets`, `rightkit-shell`, `rightkit-tauri`.
- npm: `@rightkit/legal`, `@rightkit/license`, `@rightkit/logs`, `@rightkit/release`, `@rightkit/tauri`, `@rightkit/updates`.

Keep these integrations pinned & verify published API parity during dependency updates. Key evidence includes [Cargo manifest](/Volumes/D/claude/mailright/src-tauri/Cargo.toml:57), [DB caller](/Volumes/D/claude/mailright/src-tauri/src/db.rs:28), [OAuth caller](/Volumes/D/claude/mailright/src-tauri/src/oauth.rs:46), [credential store](/Volumes/D/claude/mailright/src-tauri/src/credential_store.rs:11), [shell caller](/Volumes/D/claude/mailright/src-tauri/src/tauri_run.rs:17), [telemetry caller](/Volumes/D/claude/mailright/src/lib/telemetry.ts:22), & [release scripts](/Volumes/D/claude/mailright/package.json:35).

N rows are active app-owned capabilities requiring migration work:

- Cargo: `rightkit-llm`, `rightkit-mcp`, `rightkit-qa`, `rightkit-search`, `rightkit-settings`.
- npm: `@rightkit/app-shell`, `@rightkit/doc-render`, `@rightkit/legal-ui`, `@rightkit/media-ui`, `@rightkit/pdf-ui`, `@rightkit/platform-ui`, `@rightkit/qa`, `@rightkit/settings-ui`, `@rightkit/shell`, `@rightkit/theme`.

Concrete cutover tasks: route AI through shared LLM client while preserving provider/action policy; replace loopback axum MCP transport while retaining MailRight tool policy; adopt shared QA harness around Tauri WebDriver; map Gmail search parser, SQLCipher settings, HTML/CSP renderer, attachment image/PDF viewers, settings frame, shell window/tray/autostart, & theme tokens into shared boundaries while retaining app schemas, trust rules, limits, & product policy.

M row:

- `cargo:rightkit-http`: OAuth uses shared `rightkit-http`, while Google API, profile, AI, remote-image, & translation paths still construct direct `reqwest` clients. Route direct callers through shared async HTTP APIs, preserve MailRight auth/retry/redirect/image normalization policy, then remove direct construction. Evidence: [OAuth](/Volumes/D/claude/mailright/src-tauri/src/oauth.rs:6), [Google client](/Volumes/D/claude/mailright/src-tauri/src/google.rs:126), [AI](/Volumes/D/claude/mailright/src-tauri/src/ai.rs:72), & [remote image](/Volumes/D/claude/mailright/src-tauri/src/webimg.rs:118).

NA corrections from adversarial review: `rightkit-sync` does Gmail `historyId` delta polling, per-account leases, & local send outbox, with no cross-device record/change-token/tombstone/conflict backend; `rightkit-models` has only Node build-time bundle staging plus bundled `rightkit-embed` loading, with no runtime installer route; `@rightkit/onboarding` has no active ordered onboarding state machine beyond legal acceptance. Local PDF preview is [pdfjs-dist](/Volumes/D/claude/mailright/src/components/AttachmentPdfPreview.tsx:15); local email rendering is [EmailFrame](/Volumes/D/claude/mailright/src/components/EmailFrame.tsx:33); local settings frame is [SettingsView](/Volumes/D/claude/mailright/src/views/SettingsView.tsx:23); local theme runtime is [theme.ts](/Volumes/D/claude/mailright/src/lib/theme.ts:1). All 15 Swift rows remain NA from source census.

Full 97-row evidence, searches, delivery flags, & adversarial self-review are in [mailright.json](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/mailright.json:1).
