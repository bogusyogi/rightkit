# CodeRight — RightKit migration handoff

Source root: /Volumes/D/claude/coderight

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/coderight.json)

## rightkit-embed — N

CodeRight actively implements rightkit-embed locally; no shared RightKit production caller found.

Compare local rightkit-embed with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [embed](/Volumes/D/claude/coderight/engine/crates/local_models/src/lib.rs:1) — local model subsystem
- Owner: [embedder.rs](/Volumes/D/claude/rightkit/crates/rightkit-embed/src/embedder.rs:1) — production owner API source for rightkit-embed

## rightkit-http — M

GitHub sign-in uses rightkit_http::Client, while OpenAI-compatible provider streaming directly uses reqwest. rightkit-http AsyncClient supplies matching request deadlines, vetted DNS, retry, cancellation, and bounded SSE streams, so both active routes overlap.

Route OpenAiCompatClient::stream_once request construction through rightkit_http::AsyncClient::stream and AsyncStreamResponse::sse; retain CodeRight request admission, provider headers, tool-call reassembly, continuation, and provider-specific retry/breaker policy.

- Local: [OpenAiCompatClient::stream_once](/Volumes/D/claude/coderight/engine/crates/providers/src/client/transport.rs:611) — Builds provider SSE request with reqwest and streams response bytes directly
- Shared: [http](/Volumes/D/claude/coderight/engine/bins/coderight/src/github_signin.rs:36) — GitHub sign-in -> rightkit_http
- Owner: [AsyncClient::send/stream](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:117) — Async shared client provides deadline-bounded buffered and stream requests
- Owner: [AsyncStreamResponse::sse](/Volumes/D/claude/rightkit/crates/rightkit-http/src/async_client.rs:582) — Shared stream exposes bounded SSE decoding

Delivery: shared and local routes both active; exact manifest pin observed; runtime/build qualification not executed

## rightkit-llm — N

CodeRight actively implements rightkit-llm locally; no shared RightKit production caller found.

Compare local rightkit-llm with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [llm](/Volumes/D/claude/coderight/engine/crates/providers/src/client/transport.rs:1) — LLM transport local
- Owner: [anthropic.rs](/Volumes/D/claude/rightkit/crates/rightkit-llm/src/anthropic.rs:1) — production owner API source for rightkit-llm

## rightkit-media — N

CodeRight actively implements rightkit-media locally; no shared RightKit production caller found.

Compare local rightkit-media with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [media](/Volumes/D/claude/coderight/engine/crates/media/src/lib.rs:1) — media pipeline local
- Owner: [command.rs](/Volumes/D/claude/rightkit/crates/rightkit-media/src/command.rs:1) — production owner API source for rightkit-media

## rightkit-models — N

CodeRight actively implements rightkit-models locally; no shared RightKit production caller found.

Compare local rightkit-models with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [models](/Volumes/D/claude/coderight/engine/crates/local_models/src/lib.rs:1) — model install local
- Owner: [alias.rs](/Volumes/D/claude/rightkit/crates/rightkit-models/src/alias.rs:1) — production owner API source for rightkit-models

## rightkit-oauth — M

GitHub sign-in uses rightkit_oauth, while OpenAICodexOAuth independently generates PKCE, validates callback state, exchanges tokens, and refreshes them. rightkit-oauth provides OpenAI config plus loopback PKCE callback and token exchange APIs, so both active routes overlap.

Build OpenAI OAuthConfig::openai with CodeRight redirect, scopes, and originator authorization parameter; replace local PKCE/callback/exchange with OAuthClient::begin_loopback and finish_loopback. Keep CodeRight subscription eligibility, ChatGPT account headers, manual-code fallback, and credential-routing policy local.

- Local: [OpenAICodexOAuth::login](/Volumes/D/claude/coderight/engine/crates/providers/src/subscription_auth/native_oauth.rs:497) — Generates PKCE/state, receives callback or manual input, then exchanges OpenAI code
- Shared: [oauth](/Volumes/D/claude/coderight/engine/bins/coderight/src/github_signin.rs:31) — GitHub sign-in -> rightkit_oauth
- Owner: [OAuthConfig::openai](/Volumes/D/claude/rightkit/crates/rightkit-oauth/src/lib.rs:213) — Shared OpenAI authorization and token endpoint configuration
- Owner: [OAuthClient::begin_loopback](/Volumes/D/claude/rightkit/crates/rightkit-oauth/src/lib.rs:368) — Shared S256 PKCE verifier/challenge, state, and loopback callback listener
- Owner: [OAuthClient::finish_loopback](/Volumes/D/claude/rightkit/crates/rightkit-oauth/src/lib.rs:426) — Shared callback state validation and authorization-code token exchange

Delivery: shared and local routes both active; exact manifest pin observed; runtime/build qualification not executed

## rightkit-process — M

CodeRight sandboxing uses rightkit-process, while CLI auto-start independently spawns a detached `coderight serve` child. This overlaps only detached process launch; CodeRight's daemon health probe and crash-safe reload policy are separate behavior.

Replace only cli_presence::spawn_detached_daemon with OwnedCommand::spawn_uncontained_detached after matching log redirection and Windows detached/no-window requirements. Retain CodeRight /health probing, daemon presence, and reload/handoff saga unchanged.

- Local: [spawn_detached_daemon](/Volumes/D/claude/coderight/engine/bins/coderight/src/cli_presence.rs:283) — CLI creates detached `coderight serve` child with redirected stdout/stderr
- Shared: [process](/Volumes/D/claude/coderight/engine/crates/tools/src/sandbox/implementation.rs:9) — sandbox -> rightkit_process
- Owner: [OwnedCommand::spawn_uncontained_detached](/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:231) — Shared detached child launch uses setsid on Unix and explicit Windows breakaway behavior

Delivery: shared and local routes both active; exact manifest pin observed; runtime/build qualification not executed

## rightkit-search — N

CodeRight actively implements rightkit-search locally; no shared RightKit production caller found.

Compare local rightkit-search with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [search](/Volumes/D/claude/coderight/engine/crates/storage/src/events/search.rs:92) — product search local
- Owner: [ast.rs](/Volumes/D/claude/rightkit/crates/rightkit-search/src/ast.rs:1) — production owner API source for rightkit-search

## rightkit-settings — N

CodeRight actively implements rightkit-settings locally; no shared RightKit production caller found.

Compare local rightkit-settings with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [settings](/Volumes/D/claude/coderight/engine/crates/config/src/lib.rs:1) — typed settings local
- Owner: [atomic.rs](/Volumes/D/claude/rightkit/crates/rightkit-settings/src/atomic.rs:1) — production owner API source for rightkit-settings

## @rightkit/app-shell — N

CodeRight actively implements @rightkit/app-shell locally; no shared RightKit production caller found.

Compare local @rightkit/app-shell with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [shell](/Volumes/D/claude/coderight/apps/coderight-tauri/src/App.tsx:176) — app frame local
- Owner: [bridge.ts](/Volumes/D/claude/rightkit/packages/app-shell/src/bridge.ts:1) — production owner API source for @rightkit/app-shell

## @rightkit/i18n — N

CodeRight actively implements @rightkit/i18n locally; no shared RightKit production caller found.

Compare local @rightkit/i18n with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [i18n](/Volumes/D/claude/coderight/apps/coderight-tauri/src/i18n/context.tsx:56) — i18n local
- Owner: [index.tsx](/Volumes/D/claude/rightkit/packages/i18n/src/index.tsx:1) — production owner API source for @rightkit/i18n

## @rightkit/legal-ui — N

CodeRight actively implements @rightkit/legal-ui locally; no shared RightKit production caller found.

Compare local @rightkit/legal-ui with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [legal UI](/Volumes/D/claude/coderight/apps/coderight-tauri/src/OnboardingApp.tsx:131) — onboarding/legal UI local
- Owner: [LegalGate.tsx](/Volumes/D/claude/rightkit/packages/legal-ui/src/LegalGate.tsx:1) — production owner API source for @rightkit/legal-ui

## @rightkit/onboarding — N

CodeRight actively implements @rightkit/onboarding locally; no shared RightKit production caller found.

Compare local @rightkit/onboarding with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [onboarding](/Volumes/D/claude/coderight/apps/coderight-tauri/src/OnboardingApp.tsx:131) — onboarding local
- Owner: [Onboarding.tsx](/Volumes/D/claude/rightkit/packages/onboarding/src/Onboarding.tsx:1) — production owner API source for @rightkit/onboarding

## @rightkit/settings-ui — N

CodeRight actively implements @rightkit/settings-ui locally; no shared RightKit production caller found.

Compare rendered SettingsModal/SettingsPaneContent sections with SettingsPanel, preserve CodeRight-specific settings policy, then migrate compatible frame primitives.

- Local: [SettingsModal](/Volumes/D/claude/coderight/apps/coderight-tauri/src/components/app/AppFrame.tsx:213) — Rendered app-local SettingsModal receives live settings props
- Owner: [SettingsPanel.tsx](/Volumes/D/claude/rightkit/packages/settings-ui/src/SettingsPanel.tsx:1) — production owner API source for @rightkit/settings-ui

## @rightkit/theme — N

CodeRight actively implements @rightkit/theme locally; no shared RightKit production caller found.

Compare local @rightkit/theme with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [theme](/Volumes/D/claude/coderight/apps/coderight-tauri/src/index.css:1) — CSS theme local
- Owner: [brand.ts](/Volumes/D/claude/rightkit/packages/theme/src/brand.ts:1) — production owner API source for @rightkit/theme

## RightKitImportQueue — N

CodeRight actively implements RightKitImportQueue locally; no shared RightKit production caller found.

Compare local RightKitImportQueue with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [import queue](/Volumes/D/claude/coderight/apps/coderight-ios/CodeRight/LocalRuntime/LocalDeferredInboxBridge.swift:1) — deferred import local
- Owner: [PendingSharePayload.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitImportQueue/PendingSharePayload.swift:1) — production owner API source for RightKitImportQueue

## RightKitLiveActivity — N

CodeRight actively implements RightKitLiveActivity locally; no shared RightKit production caller found.

Compare local RightKitLiveActivity with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [live activity](/Volumes/D/claude/coderight/apps/coderight-ios/CodeRight/Porting/LiveActivities/CodeRightLiveActivities.swift:1) — activity local
- Owner: [ActivityLifecycle.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLiveActivity/ActivityLifecycle.swift:1) — production owner API source for RightKitLiveActivity

## RightKitSpeech — N

CodeRight actively implements RightKitSpeech locally; no shared RightKit production caller found.

Compare local RightKitSpeech with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [speech](/Volumes/D/claude/coderight/apps/coderight-ios/CodeRight/Dictation/SpeechDictation.swift:155) — AVAudioEngine local
- Owner: [AppleSpeechEngine.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/AppleSpeechEngine.swift:1) — production owner API source for RightKitSpeech

## RightKitAppleCredentials — N

CodeRight actively implements RightKitAppleCredentials locally; no shared RightKit production caller found.

Compare local RightKitAppleCredentials with owner API; migrate only if target behavior/policy match, otherwise document app ownership.

- Local: [credentials](/Volumes/D/claude/coderight/apps/coderight-ios/CodeRight/Storage/KeychainStore.swift:1) — Keychain wrapper local
- Owner: [ForeignGenericPassword.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitAppleCredentials/ForeignGenericPassword.swift:1) — production owner API source for RightKitAppleCredentials

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
