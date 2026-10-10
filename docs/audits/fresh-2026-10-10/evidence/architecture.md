# RightKit architecture & duplication review — 2026-10-10

## Scope & method

Fresh, source-led review of all 24 npm packages & 56 Rust crates under `packages/` & `crates/`; Swift excluded for separate review. I read manifests, package/crate READMEs, public surfaces, dependency edges, plus bodies behind suspected overlap. Findings require shared behavior or competing authority, not similar names or size. No source, build, test, install, network, or release action occurred.

## Boundary map

- **Renderer/UI:** React packages compose injected host ports; native behavior remains in Rust.
- **Host/security:** Rust owns filesystem, process, service transport, secrets, native authorization & Tauri behavior; JS packages are renderer adapters.
- **Media/content:** contracts describe durable interchange; pure evaluators transform it; runtimes execute native/model work.
- **Quality/tooling:** Node & Rust QA target separate harness ecosystems; release/git/AX/policy own separate governance stages.

## npm package inventory

| Package | Purpose proven from manifest/source | Verdict |
|---|---|---|
| `@rightkit/agent-room-mcp` | Private Node stdio MCP adapter for Agent Room HTTP API & credential cache. | Keep adapter separate from Rust authority. |
| `@rightkit/app-shell` | Optional React desktop frame, commands, shortcuts, overlays & layout slots. | Keep. |
| `@rightkit/ax` | Agent-plugin scaffolding plus static, conformance, behavioral & adversarial AX gates. | Keep. |
| `@rightkit/build-control-policy` | Managed agent-policy installation & ownership/build hooks. | Keep; policy, not generic hook runtime. |
| `@rightkit/doc-render` | Sanitized Markdown/HTML reader & semantic document blocks. | Keep. |
| `@rightkit/git` | Public-repo workflow rendering, visibility gate & drift tooling. | Keep. |
| `@rightkit/hooks` | Neutral event normalization & deadline-bound hook dispatch. | Keep; no policy authority. |
| `@rightkit/i18n` | App-supplied catalog & React locale runtime. | Keep. |
| `@rightkit/legal` | Pure legal-manifest, receipt & reacceptance primitives. | Keep. |
| `@rightkit/legal-ui` | Display-only React legal gate. | Keep distinct from legal validation. |
| `@rightkit/license` | Renderer token exchange, cached entitlement & UX hints. | Keep paired with native enforcement crate. |
| `@rightkit/logs` | Renderer batching & typed bridge to local native diagnostics. | Keep paired with `rightkit-logs`. |
| `@rightkit/media-ui` | React media player plus optional image viewer/editor. | Keep; split optional Fabric entry already exists. |
| `@rightkit/onboarding` | Ordered first-run flow, legal-first gate & injected app steps. | Keep. |
| `@rightkit/pdf-ui` | Read-only React pdf.js viewer. | Keep distinct from native PDF editing. |
| `@rightkit/platform-ui` | Platform detection, shortcut display & headless window-control ports. | Keep. |
| `@rightkit/qa` | Node/browser QA evidence, CDP, loopback & release guards. | Keep separate from Rust native QA; verify packed dependency form. |
| `@rightkit/qa-chrome` | Node Chrome-for-Testing resolution, install, launch & close. | Keep as `qa` dependency/facade. |
| `@rightkit/release` | Native-host build/sign/package/distribution & release gates. | Keep. |
| `@rightkit/settings-ui` | React settings frame, standard sections & injected settings backend. | Keep; UI only. |
| `@rightkit/shell` | Typed renderer IPC bridge for `rightkit-shell`. | Keep paired adapter. |
| `@rightkit/tauri` | Renderer client for license/device/log/diagnostic command contract. | Split generic core from suite compatibility surface internally. |
| `@rightkit/theme` | Brand-to-CSS generation, contrast checks & browser runtime. | Keep dual Node/browser entrypoints. |
| `@rightkit/updates` | Injected runtime update-tier policy & installer coordination. | Keep separate from release publication. |

`@rightkit/qa` declares public npm publication while its manifest retains `"@rightkit/qa-chrome": "workspace:*"` (`packages/qa/package.json:32-42`). This may be normalized by pnpm packing, but source alone does not prove tarball portability. Add a tarball assertion before next publish; use exact published dependency metadata if it fails.

## Rust crate inventory

| Crate | Purpose proven from manifest/source | Verdict |
|---|---|---|
| `agent-room-core` | Durable Agent Room protocol, authority, SQLite store & optional server. | Keep private authority. |
| `rightkit-agent-tools` | Typed/gated agent tool registry plus desktop/browser/service backends. | Keep orchestration facade. |
| `rightkit-align` | Pure script-to-ASR word alignment over transcript types. | Keep separate from transcript contract. |
| `rightkit-asr` | Parakeet/Whisper speech decoding runtime. | Keep. |
| `rightkit-audio` | FFmpeg audio graphs, measurement, WAV I/O & optional capture. | Keep atop media runtime. |
| `rightkit-automation` | Cross-platform desktop observe/act policy & evidence. | Keep. |
| `rightkit-browser` | CDP browser session, policy, profiles & trusted input. | Keep. |
| `rightkit-cache` | Disk LRU cache & prompt-cache key primitives. | Keep. |
| `rightkit-channel-memory` | Pending channel-memory schema, semantic validation & scope helpers. | Keep pending promotion to contracts. |
| `rightkit-compose` | Pure time-based composition evaluation. | Keep separate from rendering. |
| `rightkit-contracts` | Versioned content/record/daemon schemas & semantic validation. | Keep contract authority. |
| `rightkit-control` | Background desktop input/accessibility/WebDriver bridge. | Keep below automation. |
| `rightkit-crash` | Redacted panic capture, dumps & support bundles. | Keep. |
| `rightkit-daemon` | HTTPS device daemon, signed routes, OAuth callback & durable state. | Keep. |
| `rightkit-db` | SQLite/SQLCipher bootstrap, migration, backup & recovery. | Keep over safe FS. |
| `rightkit-embed` | ONNX local embeddings & vector store. | Keep. |
| `rightkit-framed-sidecar` | Bounded JSON-line/length-prefixed framing only. | Keep transport-only layer. |
| `rightkit-fs` | Safe state roots, atomic/private writes, locks & file identity. | Keep; candidate owner of compatible atomic mechanics. |
| `rightkit-fsindex` | Live whole-disk index, fuzzy lookup & size rollups. | Keep. |
| `rightkit-hotkeys` | Global shortcuts, hooks, conflicts & push-to-talk state. | Keep. |
| `rightkit-http` | Bounded blocking/async HTTP, retry, SSE, secrets & untrusted text. | Keep; adopt only compatible request mechanics. |
| `rightkit-jobs` | Persistent jobs, runner, supervisor & service/stdio contract. | Keep. |
| `rightkit-license` | Native signed-entitlement verification & requirement checks. | Keep native authority. |
| `rightkit-llm` | BYOK providers, streaming, routing, cache & pricing. | Keep over HTTP. |
| `rightkit-logs` | Privacy-safe JSONL diagnostics, consent, outbox & upload. | Keep domain; reuse atomic/HTTP mechanics. |
| `rightkit-mcp` | MCP stdio/HTTP client, trust/pool & loopback server. | Keep protocol domain; reuse HTTP mechanics. |
| `rightkit-media` | Verified FFmpeg artifact, probe, staging & execution lifecycle. | Keep execution authority. |
| `rightkit-models` | Verified local model download/install, hardware fit & suite model store. | Keep one crate; maintain two explicit layout APIs. |
| `rightkit-oauth` | PKCE/device OAuth, credential persistence & callback server. | Keep. |
| `rightkit-ocr` | Native OCR plus isolated verified ONNX fallback. | Keep. |
| `rightkit-ort` | ONNX Runtime resolution, environment & session setup. | Keep shared inference substrate. |
| `rightkit-overlay` | Portable overlay state, geometry, hover & hit testing. | Keep. |
| `rightkit-ownership` | Native ownership scan, verdict & admission for RustRight. | Keep. |
| `rightkit-pdf` | Native PDF inspection/edit/merge/extract/save engine. | Keep separate from PDF viewer. |
| `rightkit-playback` | Compatibility derivative cache & FFmpeg remux preparation. | Keep domain; extract only execution seam. |
| `rightkit-process` | Owned child lifecycle, health & restart primitives. | Keep low-level authority. |
| `rightkit-qa` | Rust native/engine QA harness & evidence. | Keep separate from Node QA. |
| `rightkit-qts` | Vendored Qwen3 TTS inference integration. | Keep feature/runtime boundary. |
| `rightkit-qts-ggml` | Safe wrapper over vendored ggml FFI. | Keep. |
| `rightkit-qts-ggml-sys` | Vendored low-level ggml bindings/build. | Keep isolated FFI. |
| `rightkit-record` | Capture-neutral recording lifecycle, helpers, bundle & verification. | Keep. |
| `rightkit-review` | Deterministic content review, advisory judge port & repair loop. | Keep. |
| `rightkit-sandbox` | OS sandbox, PTY, tree/resource authority. | Keep atop process lifecycle. |
| `rightkit-scenario` | Durable scenario state machine, lease & remote-worker protocol. | Keep. |
| `rightkit-search` | Query lexer/AST plus SQL/FTS/snippet compilation. | Keep. |
| `rightkit-secrets` | Per-app credential storage, presence, DB-key & migration. | Keep. |
| `rightkit-service` | Local discovery, peer-auth transport, allowlists & takeover. | Keep above framing. |
| `rightkit-settings` | Versioned settings, migrations, recovery & change events. | Keep domain; reuse atomic commit mechanic. |
| `rightkit-shell` | Native Tauri shell behavior, hardening & engine wiring. | Keep paired with JS bridge. |
| `rightkit-speech-client` | Shared speech-service client & speech-specific wire contract. | Keep specialized adapter over service/framing. |
| `rightkit-sync` | Offline cloud sync, conflict ledger & helper transport. | Keep. |
| `rightkit-tauri` | Framework-neutral native device/license/paid-command services. | Keep native authority. |
| `rightkit-transcript` | Word-timed transcript, captions & SRT/VTT/JSON interchange. | Keep contract-only. |
| `rightkit-tts` | System/QTS TTS runtime & worker protocol. | Keep. |
| `rightkit-vad` | Silero scoring, span/chunk policy & stream carry. | Keep. |
| `rightkit-watch` | Async/sync filesystem watches & coalescing. | Keep. |

## Prioritized findings

### P2 — atomic commit mechanics repeat, but observable durability differs

`rightkit-fs` already supplies `AtomicOptions::create_parents` (`crates/rightkit-fs/src/atomic.rs:21-29`) plus `atomic_write` convenience (`50-58`) & commits with `fs::rename` (`171-181`). `rightkit-settings` independently writes/fsyncs, then makes directory-sync failure observable (`crates/rightkit-settings/src/atomic.rs:43-52`) & on Windows uses `ReplaceFileW` (`59-103`). `rightkit-logs` repeats temp-write/fsync/rename (`crates/rightkit-logs/src/atomic.rs:12-48`) with best-effort directory syncing (`51-61`); `rightkit-fs` also documents best-effort directory sync (`223-231`). This confirms duplicate machinery, not a drop-in replacement or a defect.

**Verdict:** retain all three until behavior is selected explicitly; then consolidate only compatible byte-commit mechanics in `rightkit-fs`. Keep settings recovery/schema & logs consent/outbox policy local. No fault evidence supports a P1 bug claim.

**Smallest boundary:** make commit semantics explicit in the existing `AtomicWriter` options: required/best-effort/off parent sync plus portable rename/existing-target replacement. Do not add parent creation or a convenience API; both already exist.

**Compatibility fixtures before migration:** existing-target replacement on Windows; injected parent-directory sync failure must fail settings yet remain tolerated where current contracts do; temp cleanup after write/replace failure; private mode; `create_parents`; `durable_sync = false`; preservation of prior bytes on failed replacement. Migrate logs only after best-effort parity, then settings only after required sync & `ReplaceFileW` parity.

### P2 — direct HTTP clients have intentionally incompatible contracts

`rightkit-http` owns blocking/async clients, retries & SSE (`crates/rightkit-http/README.md:7-24`; `crates/rightkit-http/src/client.rs:261-274`). Model downloads independently configure 30-second connect/120-second body-idle limits plus range/resume transfer behavior (`crates/rightkit-models/src/download.rs:505-542`). Basic MCP HTTP sets `max_redirects(0)` before attaching bearer credentials (`crates/rightkit-mcp/src/client.rs:772-825`); advanced MCP already uses `rightkit-http` (`crates/rightkit-mcp/src/advanced_http.rs:9`, `298`). Logs makes its default ureq transport optional, while `default-features = false` promises no network dependency & callers supply `Transport` (`crates/rightkit-logs/Cargo.toml:14-26`; `crates/rightkit-logs/src/upload.rs:73-120`).

**Verdict:** do not blanket-merge clients. Preserve each domain contract; converge only a behavior proven equivalent by fixtures.

**Smallest boundary:** add opt-in `rightkit-http` adapters/presets only after contract tests cover model streaming/range/resume, MCP no-redirect bearer retries, & logs status upload. `rightkit-logs::Transport` remains its injection port; its `default-features = false` build must retain zero network dependency. Direct `ureq` remains valid where a preset cannot meet that contract.

### P2 — playback duplicates generic FFmpeg process execution

`rightkit-playback` owns its correct domain concerns—source fingerprint, derivative cache, bounded request registry & codec policy—but directly builds an `OwnedCommand` for FFmpeg (`crates/rightkit-playback/src/lib.rs:24-25`, `337`, `474`). `rightkit-media` already exports bounded capture/progress/checked runners (`crates/rightkit-media/src/lib.rs:30-43`; `crates/rightkit-media/src/runner.rs:180-288`, `301-380`) over same owned-child discipline.

**Verdict:** retain both paths pending observable-behavior parity; extract only a shared prepared-command primitive if parity proves it real.

**Smallest boundary:** add a media runner accepting prepared args, cancellation, timeout, stderr cap & optional progress, without changing `PlaybackRegistry`, cache keys, remux selection, or public playback errors. Migrate only direct spawn/wait/drain code after output/error parity tests.

### P2 — generic Tauri client & Right Suite compatibility inventory share one entry surface

`createRightKitTauriClient` accepts an injected command map & remains generic (`packages/tauri/src/index.ts:76-126`). Same module then contains five product-specific command/argument mappings plus named constructors (`packages/tauri/src/index.ts:153-241`). These mappings are compatibility inventory, not generic adapter policy; every new generic consumer imports product names/types.

**Verdict:** split module/export surface, not package version line initially.

**Smallest boundary:** retain `@rightkit/tauri` root for generic client/types; put suite maps & named constructors under `@rightkit/tauri/right-suite` backed by a separate source module. Root may re-export deprecated compatibility names for one major cycle. This preserves exact existing commands while making generic dependencies product-neutral.

### P3 — model storage has two intentional layouts; make boundary explicit, do not split yet

`rightkit-models` exposes revisioned `store` plus flat suite layout in one crate (`crates/rightkit-models/src/lib.rs:1-45`). Suite explicitly preserves installed shared roots while revisioned layout stays independent (`crates/rightkit-models/src/suite/mod.rs:1-26`). Both share model identity, downloads, locks, artifact verification & migration work; extracting packages now would multiply semver coordination without removing an authority overlap.

**Verdict:** keep one crate & two named APIs; no package split now.

**Smallest boundary:** keep legacy revisioned API documented as `store`, suite API as `suite`; prevent cross-layout calls outside explicit migration functions. Reconsider split only after one layout is retired or independent release cadence creates concrete pressure.

## Large-module decomposition review

| Cluster | Source evidence | Verdict & smallest useful boundary |
|---|---|---|
| `@rightkit/release` CLI & multi-platform orchestration | CLI holds command inventory & release execution gate (`packages/release/cli/right-release.mjs:8-31`, `41-53`), then dispatches scripts (`84-115`, `124-167`). Build composes target bridge, signing, storage, state & macOS/Windows helpers (`packages/release/build-release.mjs:23-45`); upload requires tier, sealed bytes, matching commit & ownership proof (`packages/release/upload-release.mjs:23-60`). | **Keep one package & current command facade.** Signing, sealed-artifact state, tier checks & platform dispatch form one release authority under locked release rules. Existing scripts are right module seam; group internal files by `build/`, `upload/`, `platform/`, or `ownership/` only during an import-compatible relocation. Do not publish platform packages or split build from upload ownership. |
| `rightkit-qa` runner | One harness defines engine, app UI & browser targets, declarative scenarios, tier/evidence behavior & native validators (`crates/rightkit-qa/src/lib.rs:7-21`, `22-46`); browser support is already a feature-gated adapter (`23-24`). | **Keep one crate.** Target runners differ, but scenario gating, evidence receipts, mocks & validation are one QA contract. Preserve target modules; extract media/loudness only after a non-QA consumer needs a separately versioned validation API. |
| `rightkit-asr` runtime/backends | Decoder, model & execution internals are private, public product APIs cover Whisper/routing/chunked/longform (`crates/rightkit-asr/src/lib.rs:46-83`, `87-118`); specialized backends stay feature/platform-gated (`65-74`, `84-85`). Runtime selection delegates to `rightkit-ort` (`crates/rightkit-asr/Cargo.toml:31-41`, `52-74`). | **Keep one crate & existing backend modules.** Models share timestamps, decoder types, vocabulary, audio preprocessing & public transcript results. Split only a backend that gains independent dependencies/release cadence; then expose a narrow backend trait from ASR and retain feature-gated adapters. No current separate package boundary is justified. |
| `rightkit-logs` privacy pipeline | Atomic storage, collection, consent, event, outbox, redaction, upload & writer are separate internal modules (`crates/rightkit-logs/src/lib.rs:3-35`). HTTP is optional; no-HTTP callers inject `Transport` (`Cargo.toml:14-26`). | **Keep one crate.** Consent/redaction/outbox define one privacy boundary & must remain co-versioned with uploader/event schema. Atomic mechanics are only candidate extraction, subject to prior fixtures; do not move `Transport` or uploader policy into a mandatory network package. |
| `rightkit-shell` native Tauri plugin | Plugin modules cover lifecycle, commands, hardening, tray, navigation, engine & Windows binding selection (`crates/rightkit-shell/src/lib.rs:17-52`); a single managed `ShellState` binds them to Tauri lifecycle (`88-125`) & commands use that state (`crates/rightkit-shell/src/commands.rs:16-30`). | **Keep one plugin.** Splitting creates conflicting Tauri state/lifecycle versions. Feature-gated `engine` & platform modules are smallest useful seams already; retain typed commands as stable bridge. |

## Confirmed non-duplication

- `rightkit-framed-sidecar` only frames bytes (`crates/rightkit-framed-sidecar/src/lib.rs:1-28`); `rightkit-service` adds OS-authenticated discovery, allowlists & takeover (`crates/rightkit-service/src/lib.rs:1-27`); `rightkit-speech-client` adds a speech contract over both (`crates/rightkit-speech-client/src/lib.rs:1-44`). Keep all three layers.
- Node QA & Rust QA both drive Chrome, yet Rust delegates CDP/session behavior to `rightkit-browser` (`crates/rightkit-qa/src/browser.rs:1-18`) while Node ships its own consumer-runtime launcher/driver. They cannot share a runtime package; align black-box scenarios/evidence schema instead of merging implementations.
- `rightkit-transcript` deliberately holds only contract/segmentation/import-export (`crates/rightkit-transcript/src/lib.rs:1-24`); `rightkit-align` adds normalization/cost/confidence without I/O (`crates/rightkit-align/src/lib.rs:1-24`). Keep split.
- `rightkit-compose` is pure frame description evaluation (`crates/rightkit-compose/src/lib.rs:1-13`); `rightkit-record` owns capture lifecycle & immutable evidence bundles (`crates/rightkit-record/src/lib.rs:1-17`); `rightkit-review` evaluates supplied measurements without decoding media (`crates/rightkit-review/src/lib.rs:1-20`). Keep these separate authorities.
- `@rightkit/license` provides renderer exchange/UX while `rightkit-license` verifies signed entitlements at native boundary; this is defense-in-depth, not duplicated enforcement (`packages/license/README.md:1-16`; `crates/rightkit-license/README.md:1-16`).
- `@rightkit/pdf-ui` is read-only webview viewing (`packages/pdf-ui/src/index.ts:1-9`); `rightkit-pdf` owns native inspect/edit/save (`crates/rightkit-pdf/src/lib.rs:1-18`). Keep split. `@rightkit/doc-render` addresses sanitized Markdown/HTML, not PDF (`packages/doc-render/src/index.ts:1-15`).
- `rightkit-audio` makes pure graphs & delegates FFmpeg execution to media (`crates/rightkit-audio/src/lib.rs:1-13`); `rightkit-media` owns verified FFmpeg lifecycle. Keep split.
- `@rightkit/app-shell`, `@rightkit/platform-ui`, `@rightkit/settings-ui`, `@rightkit/onboarding`, `@rightkit/legal-ui` form composition layers. Their injected ports & distinct state authority avoid duplicate native shell/setting/legal policy; no merge.

## Migration sequence

1. Add atomic-write fixtures for existing Windows targets, injected parent-sync failure, cleanup, private mode, parent creation, durability-off & failed-replacement preservation. Select semantics, then migrate logs before settings only when parity holds; retain domain tests for log consent/outbox & settings backup/recovery.
2. Add HTTP contract fixtures for redirect, bearer header, retry, Range/resume, streaming timeout & no-network compilation. Move only requests whose fixtures match an opt-in `rightkit-http` adapter; retain direct clients otherwise.
3. Add `rightkit-media` prepared-command runner only if it reproduces playback's timeout/cancel/stderr file behavior. Retain playback cache/scheduling tests.
4. Move Tauri suite maps to a subpath module, retain root compatibility exports, migrate consumers, then remove aliases in next major.
5. Before publishing `@rightkit/qa`, inspect packed tarball `package.json` & enforce a test that its `@rightkit/qa-chrome` dependency resolves to a registry version.

No source changes made; report only.
