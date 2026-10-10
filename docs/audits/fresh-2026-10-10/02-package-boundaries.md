# 2. Package boundaries, duplication & decomposition

Audit date: 10 October 2026, Asia/Kolkata. RightKit baseline: `08252a681d9f9dc2911962a49fa0026a4c345dff`, with existing uncommitted OCR/fsindex & lockfile changes. Findings use current source; no previous audit findings were reused. Source edits, publication & app migration are outside this report delivery.

Evidence labels: **observed** = manifest/source fact; **candidate** = proposed removal or consolidation requiring focused verification; **keep** = justified boundary; **unproven** = unavailable runtime/registry evidence. Source version, staged version, recorded registry version & adopted baseline are distinct lifecycle states.

## Apple follow-up completed

Both requested Apple skills were applied after initial report delivery. All 14 products & native consumer/helper boundaries now have explicit verdicts. Native checks passed except obsolete archive-helper tests; source review identified privacy, cancellation, persistence & concurrency fixes. Product source remains unchanged.

- [apple-verification.md](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-verification.md) — commands, results, compiler warnings, skill/toolchain provenance & qualification scope.
- [apple-closure.md](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-closure.md) — one prioritized backlog with owners & exact acceptance checks; use this for follow-up instead of restarting full audit.

Concurrent workspace delta: Pulse now declares `@rightkit/release 0.2.146` (original snapshot: `0.2.141`) & adds source/installed QA receipt mappings. Its installed plan still records failure pending a release-safe driver. Concurrent edits affect Pulse, SwiftRight, Legion & RightKit release/Git tooling; original broad audit remains a source snapshot. Audited Swift package/helper source hashes remain unchanged. Original machine-readable inventories retain snapshot values; [workspace-drift.json](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/workspace-drift.json) records newer Pulse dependency value, commit & changed-file hashes for handoff. No other captured package dependency declaration changed at this follow-up snapshot.

## Decisions

Keep existing runtime/domain boundaries. Current evidence supports internal consolidation & clearer subpaths before creating more published packages.

| Priority | Action | Reason |
|---|---|---|
| First | Consolidate durable atomic-write semantics around `rightkit-fs` after compatibility fixtures. | Multiple independent writers differ in Windows replacement, parent sync, private permissions & retries. Models' revisioned writer deletes Windows destination before rename; this is observable source-level loss of old-target preservation if rename then fails. |
| First | Unify PID-liveness policy after Windows/Unix edge-case tests. | FS & service disagree on PID bounds & Windows access denial; stale-state cleanup needs one conservative policy. |
| Next | Reuse shared model verification/digest primitives for TTS where API parity exists. | Avoid another QTS artifact identity/hash/cache authority; retain TTS worker/runtime facade. |
| Next | Consolidate copied browser E2E harness into QA. | Three byte-identical copies plus settings-specific fork already share Chrome lifecycle owner. |
| Next | Expose narrow HTTP & FFmpeg execution seams. | Keep domain-specific retry, redirect, streaming, consent, cancellation & cache policy. |
| Next | Move Tauri suite maps into compatibility subpath; isolate large implementation responsibilities internally. | Preserve public version lines while clarifying generic API ownership. |
| Keep | Generated ownership-policy copies, runtime adapters, Swift/Rust verification, model layouts & QA language layers. | Existing synchronization, platform contracts or independent state ownership justify these boundaries. |

Ownership-source remains generated & synchronized; retain existing Adrian-authenticated trust-sync activation contract.


## Complete npm/Rust inventory & boundary verdicts

### Scope & method

Fresh, source-led review of all 24 npm packages & 56 Rust crates under `packages/` & `crates/`; Swift excluded for separate review. I read manifests, package/crate READMEs, public surfaces, dependency edges, plus bodies behind suspected overlap. Findings require shared behavior or competing authority, not similar names or size. No source, build, test, install, network, or release action occurred.

### Boundary map

- **Renderer/UI:** React packages compose injected host ports; native behavior remains in Rust.
- **Host/security:** Rust owns filesystem, process, service transport, secrets, native authorization & Tauri behavior; JS packages are renderer adapters.
- **Media/content:** contracts describe durable interchange; pure evaluators transform it; runtimes execute native/model work.
- **Quality/tooling:** Node & Rust QA target separate harness ecosystems; release/git/AX/policy own separate governance stages.

### npm package inventory

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

### Rust crate inventory

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

### Prioritized findings

#### P2 — atomic commit mechanics repeat, but observable durability differs

`rightkit-fs` already supplies `AtomicOptions::create_parents` (`crates/rightkit-fs/src/atomic.rs:21-29`) plus `atomic_write` convenience (`50-58`) & commits with `fs::rename` (`171-181`). `rightkit-settings` independently writes/fsyncs, then makes directory-sync failure observable (`crates/rightkit-settings/src/atomic.rs:43-52`) & on Windows uses `ReplaceFileW` (`59-103`). `rightkit-logs` repeats temp-write/fsync/rename (`crates/rightkit-logs/src/atomic.rs:12-48`) with best-effort directory syncing (`51-61`); `rightkit-fs` also documents best-effort directory sync (`223-231`). This confirms duplicate machinery, not a drop-in replacement or a defect.

**Verdict:** retain all three until behavior is selected explicitly; then consolidate only compatible byte-commit mechanics in `rightkit-fs`. Keep settings recovery/schema & logs consent/outbox policy local. No fault evidence supports a P1 bug claim.

**Smallest boundary:** make commit semantics explicit in the existing `AtomicWriter` options: required/best-effort/off parent sync plus portable rename/existing-target replacement. Do not add parent creation or a convenience API; both already exist.

**Compatibility fixtures before migration:** existing-target replacement on Windows; injected parent-directory sync failure must fail settings yet remain tolerated where current contracts do; temp cleanup after write/replace failure; private mode; `create_parents`; `durable_sync = false`; preservation of prior bytes on failed replacement. Migrate logs only after best-effort parity, then settings only after required sync & `ReplaceFileW` parity.

#### P2 — direct HTTP clients have intentionally incompatible contracts

`rightkit-http` owns blocking/async clients, retries & SSE (`crates/rightkit-http/README.md:7-24`; `crates/rightkit-http/src/client.rs:261-274`). Model downloads independently configure 30-second connect/120-second body-idle limits plus range/resume transfer behavior (`crates/rightkit-models/src/download.rs:505-542`). Basic MCP HTTP sets `max_redirects(0)` before attaching bearer credentials (`crates/rightkit-mcp/src/client.rs:772-825`); advanced MCP already uses `rightkit-http` (`crates/rightkit-mcp/src/advanced_http.rs:9`, `298`). Logs makes its default ureq transport optional, while `default-features = false` promises no network dependency & callers supply `Transport` (`crates/rightkit-logs/Cargo.toml:14-26`; `crates/rightkit-logs/src/upload.rs:73-120`).

**Verdict:** do not blanket-merge clients. Preserve each domain contract; converge only a behavior proven equivalent by fixtures.

**Smallest boundary:** add opt-in `rightkit-http` adapters/presets only after contract tests cover model streaming/range/resume, MCP no-redirect bearer retries, & logs status upload. `rightkit-logs::Transport` remains its injection port; its `default-features = false` build must retain zero network dependency. Direct `ureq` remains valid where a preset cannot meet that contract.

#### P2 — playback duplicates generic FFmpeg process execution

`rightkit-playback` owns its correct domain concerns—source fingerprint, derivative cache, bounded request registry & codec policy—but directly builds an `OwnedCommand` for FFmpeg (`crates/rightkit-playback/src/lib.rs:24-25`, `337`, `474`). `rightkit-media` already exports bounded capture/progress/checked runners (`crates/rightkit-media/src/lib.rs:30-43`; `crates/rightkit-media/src/runner.rs:180-288`, `301-380`) over same owned-child discipline.

**Verdict:** retain both paths pending observable-behavior parity; extract only a shared prepared-command primitive if parity proves it real.

**Smallest boundary:** add a media runner accepting prepared args, cancellation, timeout, stderr cap & optional progress, without changing `PlaybackRegistry`, cache keys, remux selection, or public playback errors. Migrate only direct spawn/wait/drain code after output/error parity tests.

#### P2 — generic Tauri client & Right Suite compatibility inventory share one entry surface

`createRightKitTauriClient` accepts an injected command map & remains generic (`packages/tauri/src/index.ts:76-126`). Same module then contains five product-specific command/argument mappings plus named constructors (`packages/tauri/src/index.ts:153-241`). These mappings are compatibility inventory, not generic adapter policy; every new generic consumer imports product names/types.

**Verdict:** split module/export surface, not package version line initially.

**Smallest boundary:** retain `@rightkit/tauri` root for generic client/types; put suite maps & named constructors under `@rightkit/tauri/right-suite` backed by a separate source module. Root may re-export deprecated compatibility names for one major cycle. This preserves exact existing commands while making generic dependencies product-neutral.

#### P3 — model storage has two intentional layouts; make boundary explicit, do not split yet

`rightkit-models` exposes revisioned `store` plus flat suite layout in one crate (`crates/rightkit-models/src/lib.rs:1-45`). Suite explicitly preserves installed shared roots while revisioned layout stays independent (`crates/rightkit-models/src/suite/mod.rs:1-26`). Both share model identity, downloads, locks, artifact verification & migration work; extracting packages now would multiply semver coordination without removing an authority overlap.

**Verdict:** keep one crate & two named APIs; no package split now.

**Smallest boundary:** keep legacy revisioned API documented as `store`, suite API as `suite`; prevent cross-layout calls outside explicit migration functions. Reconsider split only after one layout is retired or independent release cadence creates concrete pressure.

### Large-module decomposition review

| Cluster | Source evidence | Verdict & smallest useful boundary |
|---|---|---|
| `@rightkit/release` CLI & multi-platform orchestration | CLI holds command inventory & release execution gate (`packages/release/cli/right-release.mjs:8-31`, `41-53`), then dispatches scripts (`84-115`, `124-167`). Build composes target bridge, signing, storage, state & macOS/Windows helpers (`packages/release/build-release.mjs:23-45`); upload requires tier, sealed bytes, matching commit & ownership proof (`packages/release/upload-release.mjs:23-60`). | **Keep one package & current command facade.** Signing, sealed-artifact state, tier checks & platform dispatch form one release authority under locked release rules. Existing scripts are right module seam; group internal files by `build/`, `upload/`, `platform/`, or `ownership/` only during an import-compatible relocation. Do not publish platform packages or split build from upload ownership. |
| `rightkit-qa` runner | One harness defines engine, app UI & browser targets, declarative scenarios, tier/evidence behavior & native validators (`crates/rightkit-qa/src/lib.rs:7-21`, `22-46`); browser support is already a feature-gated adapter (`23-24`). | **Keep one crate.** Target runners differ, but scenario gating, evidence receipts, mocks & validation are one QA contract. Preserve target modules; extract media/loudness only after a non-QA consumer needs a separately versioned validation API. |
| `rightkit-asr` runtime/backends | Decoder, model & execution internals are private, public product APIs cover Whisper/routing/chunked/longform (`crates/rightkit-asr/src/lib.rs:46-83`, `87-118`); specialized backends stay feature/platform-gated (`65-74`, `84-85`). Runtime selection delegates to `rightkit-ort` (`crates/rightkit-asr/Cargo.toml:31-41`, `52-74`). | **Keep one crate & existing backend modules.** Models share timestamps, decoder types, vocabulary, audio preprocessing & public transcript results. Split only a backend that gains independent dependencies/release cadence; then expose a narrow backend trait from ASR and retain feature-gated adapters. No current separate package boundary is justified. |
| `rightkit-logs` privacy pipeline | Atomic storage, collection, consent, event, outbox, redaction, upload & writer are separate internal modules (`crates/rightkit-logs/src/lib.rs:3-35`). HTTP is optional; no-HTTP callers inject `Transport` (`Cargo.toml:14-26`). | **Keep one crate.** Consent/redaction/outbox define one privacy boundary & must remain co-versioned with uploader/event schema. Atomic mechanics are only candidate extraction, subject to prior fixtures; do not move `Transport` or uploader policy into a mandatory network package. |
| `rightkit-shell` native Tauri plugin | Plugin modules cover lifecycle, commands, hardening, tray, navigation, engine & Windows binding selection (`crates/rightkit-shell/src/lib.rs:17-52`); a single managed `ShellState` binds them to Tauri lifecycle (`88-125`) & commands use that state (`crates/rightkit-shell/src/commands.rs:16-30`). | **Keep one plugin.** Splitting creates conflicting Tauri state/lifecycle versions. Feature-gated `engine` & platform modules are smallest useful seams already; retain typed commands as stable bridge. |

### Confirmed non-duplication

- `rightkit-framed-sidecar` only frames bytes (`crates/rightkit-framed-sidecar/src/lib.rs:1-28`); `rightkit-service` adds OS-authenticated discovery, allowlists & takeover (`crates/rightkit-service/src/lib.rs:1-27`); `rightkit-speech-client` adds a speech contract over both (`crates/rightkit-speech-client/src/lib.rs:1-44`). Keep all three layers.
- Node QA & Rust QA both drive Chrome, yet Rust delegates CDP/session behavior to `rightkit-browser` (`crates/rightkit-qa/src/browser.rs:1-18`) while Node ships its own consumer-runtime launcher/driver. They cannot share a runtime package; align black-box scenarios/evidence schema instead of merging implementations.
- `rightkit-transcript` deliberately holds only contract/segmentation/import-export (`crates/rightkit-transcript/src/lib.rs:1-24`); `rightkit-align` adds normalization/cost/confidence without I/O (`crates/rightkit-align/src/lib.rs:1-24`). Keep split.
- `rightkit-compose` is pure frame description evaluation (`crates/rightkit-compose/src/lib.rs:1-13`); `rightkit-record` owns capture lifecycle & immutable evidence bundles (`crates/rightkit-record/src/lib.rs:1-17`); `rightkit-review` evaluates supplied measurements without decoding media (`crates/rightkit-review/src/lib.rs:1-20`). Keep these separate authorities.
- `@rightkit/license` provides renderer exchange/UX while `rightkit-license` verifies signed entitlements at native boundary; this is defense-in-depth, not duplicated enforcement (`packages/license/README.md:1-16`; `crates/rightkit-license/README.md:1-16`).
- `@rightkit/pdf-ui` is read-only webview viewing (`packages/pdf-ui/src/index.ts:1-9`); `rightkit-pdf` owns native inspect/edit/save (`crates/rightkit-pdf/src/lib.rs:1-18`). Keep split. `@rightkit/doc-render` addresses sanitized Markdown/HTML, not PDF (`packages/doc-render/src/index.ts:1-15`).
- `rightkit-audio` makes pure graphs & delegates FFmpeg execution to media (`crates/rightkit-audio/src/lib.rs:1-13`); `rightkit-media` owns verified FFmpeg lifecycle. Keep split.
- `@rightkit/app-shell`, `@rightkit/platform-ui`, `@rightkit/settings-ui`, `@rightkit/onboarding`, `@rightkit/legal-ui` form composition layers. Their injected ports & distinct state authority avoid duplicate native shell/setting/legal policy; no merge.

### Migration sequence

1. Add atomic-write fixtures for existing Windows targets, injected parent-sync failure, cleanup, private mode, parent creation, durability-off & failed-replacement preservation. Select semantics, then migrate logs before settings only when parity holds; retain domain tests for log consent/outbox & settings backup/recovery.
2. Add HTTP contract fixtures for redirect, bearer header, retry, Range/resume, streaming timeout & no-network compilation. Move only requests whose fixtures match an opt-in `rightkit-http` adapter; retain direct clients otherwise.
3. Add `rightkit-media` prepared-command runner only if it reproduces playback's timeout/cancel/stderr file behavior. Retain playback cache/scheduling tests.
4. Move Tauri suite maps to a subpath module, retain root compatibility exports, migrate consumers, then remove aliases in next major.
5. Before publishing `@rightkit/qa`, inspect packed tarball `package.json` & enforce a test that its `@rightkit/qa-chrome` dependency resolves to a registry version.

No source changes made; report only.


## Body-level duplication adjudication

Source-body review only; no builds, tests, network, prior audit material, or product code changed.

### Confirmed consolidation targets

#### 1. Durable atomic replacement has seven local implementations beside its canonical owner

`rightkit-fs` already exposes sibling-temp creation, exclusive creation, optional private permissions, durable file + parent sync, cleanup, & a streaming API in [`atomic.rs`](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:50). Matching implementation sequence exists in:

| Owner | Exact matched sequence | Material difference |
|---|---|---|
| `rightkit-logs::atomic::write_atomic` | create parent, unique sibling `create_new` temp, write, `sync_all`, rename, parent sync ([source](/Volumes/D/claude/rightkit/crates/rightkit-logs/src/atomic.rs:12)) | No private mode or hardened open flags. |
| `rightkit-daemon::util::atomic_write` | temp, write, `sync_all`, rename, parent sync ([source](/Volumes/D/claude/rightkit/crates/rightkit-daemon/src/util.rs:67)) | Fixed `.tmp` name & non-exclusive `File::create`. |
| `rightkit-models::store::write_atomic` | parent creation, temp, write, `sync_all`, rename, cleanup ([source](/Volumes/D/claude/rightkit/crates/rightkit-models/src/store.rs:980)) | Deletes existing Windows target before rename, defeating old-target preservation. |
| `rightkit-models::suite::manifest::write_atomic` | parent, unique `create_new` temp, write, `sync_all`, rename, parent sync ([source](/Volumes/D/claude/rightkit/crates/rightkit-models/src/suite/manifest.rs:224)) | String error adapter only. |
| `rightkit-models::suite::refs::write_ref_unlocked` | parent, unique `create_new` temp, write, `sync_all`, rename, cleanup ([source](/Volumes/D/claude/rightkit/crates/rightkit-models/src/suite/refs.rs:112)) | Omits parent sync. |
| `rightkit-service::record::write_atomic` | local temp, write, `sync_all`, rename ([source](/Volumes/D/claude/rightkit/crates/rightkit-service/src/record.rs:121)) | Needs bounded Windows retry because readers hold discovery records open ([source](/Volumes/D/claude/rightkit/crates/rightkit-service/src/record.rs:144)). |
| `rightkit-settings::atomic_write_with` | parent, unique `create_new` temp, write, flush, `sync_all`, replacement, parent sync, cleanup ([source](/Volumes/D/claude/rightkit/crates/rightkit-settings/src/atomic.rs:17)) | Uses `ReplaceFileW` for existing Windows targets & makes non-Windows parent-sync failure visible ([source](/Volumes/D/claude/rightkit/crates/rightkit-settings/src/atomic.rs:59)). |

**Disposition:** consolidation candidate, not proven drop-in compatibility. Extend `rightkit-fs::AtomicWriter` with replacement strategy (plain rename, bounded retry, `ReplaceFileW`) plus strict parent-sync result; then migrate wrappers after focused parity tests. `rightkit-models` already depends on `rightkit-fs` with only `digest`; enable `fs` there. Do not replace cache writes: cache deliberately uses disposable `<root>/.tmp`, no sync, startup deletion, LRU accounting, & is not durable state ([source](/Volumes/D/claude/rightkit/crates/rightkit-cache/src/disk.rs:110), [source](/Volumes/D/claude/rightkit/crates/rightkit-cache/src/disk.rs:241)).

#### 2. PID liveness probe is duplicated, with safer service policy

`rightkit-fs::process_alive` & `rightkit-service::transport::pid_alive` both perform Unix `kill(pid, 0)` plus Windows `OpenProcess`/exit-code probing. FS accepts PID `0` & unbounded `u32` Unix casts, while service rejects both; FS treats Windows access denial as dead while service treats it as existing ([fs](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lib.rs:62), [service](/Volumes/D/claude/rightkit/crates/rightkit-service/src/transport.rs:576)). FS uses result to reclaim stale atomic/state temps, so this difference affects cleanup policy.

**Disposition:** consolidation candidate, not proven drop-in compatibility. Harden `rightkit-fs::process_alive` with service guards & access-denied rule, then have service call it behind its public facade. No dependency cycle exists.

#### 3. QTS file digest & model-ownership policy bypass shared model owner

`rightkit-tts::models::sha256_file` and `rightkit-fs::digest::sha256_file` both stream a file through SHA-256 & produce lowercase hex ([tts](/Volumes/D/claude/rightkit/crates/rightkit-tts/src/models.rs:103), [fs](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/digest.rs:33)). TTS also owns QTS identity, artifact size/hash manifest, environment-root resolution, presence checks, & `.verified.json` cache ([source](/Volumes/D/claude/rightkit/crates/rightkit-tts/src/models.rs:12)), while `rightkit-models` is declared shared local-model manifest/installer owner ([source](/Volumes/D/claude/rightkit/crates/rightkit-models/src/lib.rs:1)).

**Disposition:** `sha256_file` has matching signature, I/O-error type & lowercase-hex output; replace behind existing facade after digest/error fixtures. Buffer size changes from 1 MiB to 64 KiB; no throughput equivalence was measured. QTS catalog migration is a consolidation candidate: add QTS catalog/spec + resolve/verify entrypoint to `rightkit-models`, then retain `rightkit-tts::models` as worker-compatible path/runtime facade. Do not create another model package.

#### 4. E2E browser driver copied byte-for-byte across UI packages

`packages/app-shell/e2e/browser.ts`, `packages/onboarding/e2e/browser.ts`, & `packages/theme/e2e/browser.ts` are byte-identical: Chrome discovery/launch, static server, CDP transport, input, waits, screenshots, shutdown ([representative source](/Volumes/D/claude/rightkit/packages/app-shell/e2e/browser.ts:1)). `settings-ui/e2e/browser.ts` began from same driver but adds navigation serials, CDP timeouts, focus emulation, visibility flags, & per-key typing ([source](/Volumes/D/claude/rightkit/packages/settings-ui/e2e/browser.ts:51)); this is a maintained fork, not independent design. All already use `@rightkit/qa/chrome` for browser lifecycle.

**Disposition:** app-shell/onboarding/theme driver is proven identical; move it into `@rightkit/qa` test-only export. Settings-ui migration is a candidate after launch/input/navigation hooks cover its additions; keep suite tests local.

#### 5. Ownership-source duplication is intentional generated synchronization

`packages/release/ownership-source.mjs` exactly equals `packages/build-control-policy/src/ownership-source.mjs`. `rightkit-sync.mjs` explicitly copies release version to build-control-policy source ([source](/Volumes/D/claude/rightkit/packages/release/rightkit-sync.mjs:195)); build-control-policy installs that source as runtime policy asset ([source](/Volumes/D/claude/rightkit/packages/build-control-policy/src/install-policy-assets.mjs:1)). Thus it is generated synchronization, not competing ownership logic.

**Disposition:** keep generated copy & exact sync/parity protection. No extraction or enforcement/trust-sync change.

### Same-domain owners that should remain separate

| Area | Evidence & decision |
|---|---|
| `rightkit-models::store` vs `suite` | Revisioned `<root>/<id>/state.json + revisions` & flat trusted-catalog compatibility layout are explicitly independent; one model ID belongs to one layout ([source](/Volumes/D/claude/rightkit/crates/rightkit-models/src/suite/mod.rs:17)). They already share lower-level archive code. Retain layouts; consolidate only atomic writes above. |
| Control / automation | Control owns in-app WebDriver/webview admission & durable control sessions; automation owns external desktop accessibility policy. Similar `admission`, `lease`, `keys`, `json` names serve distinct authority boundaries. No merge. |
| Process / sandbox / daemon / service / speech client | Sandbox reuses `rightkit-process::WindowsJob` directly ([source](/Volumes/D/claude/rightkit/crates/rightkit-sandbox/src/windows_job.rs:1)). Daemon’s TLS/upload state differs from user-local service discovery; speech client delegates service APIs. Retain. |
| FS / watch / fsindex / cache / db / search | Watch converts/coalesces backend events; fsindex maintains index format; cache is rebuildable LRU; DB owns SQLite migration/backup; search owns query semantics. No matched policy loop beyond cache’s intentionally non-durable replacement. |
| Audio / QTS / TTS / ASR | Audio path decoder accepts generic WAV; QTS byte decoder restricts QTS reference input & reports QTS errors ([audio](/Volumes/D/claude/rightkit/crates/rightkit-audio/src/wav.rs:19), [qts](/Volumes/D/claude/rightkit/crates/rightkit-qts/src/pipeline/speaker_encoder.rs:250)). Direct dependency would couple QTS runtime to media package; retain. |
| Shell / app-shell / platform-ui | Shell explicitly declares structural bridge twin to remain dependency-free ([source](/Volumes/D/claude/rightkit/packages/shell/src/index.ts:216)); app-shell consumes platform-ui’s one canonical `WindowControlAdapter` ([bridge](/Volumes/D/claude/rightkit/packages/app-shell/src/bridge.ts:14), [owner](/Volumes/D/claude/rightkit/packages/platform-ui/src/index.ts:186)). Type shape duplication has no runtime policy; retain. |
| Logs / license / legal / updates / tauri | Logs differs by runtime transport; legal is manifest contract while legal-ui renders it; updates owns client policy while tauri adapts runtime. Native Rust/Swift license enforcement intentionally fails closed while JS is documented UI hint ([JS](/Volumes/D/claude/rightkit/packages/license/src/index.ts:273), [Rust](/Volumes/D/claude/rightkit/crates/rightkit-license/src/lib.rs:69), [Swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseClient.swift:763)). Retain language-native verification; keep shared vectors aligned. |


## Static-pass additions (from static pass, unverified by build)

Source: the earlier static pass over the same baseline, merged here. It counted lines and read importers but built nothing; sizes are its estimates (B's body-level adjudication above gives site counts and exact citations, not LOC, so where both speak, B's finding stands and A's size is footnoted). Per-crate evidence with file:line citations is in [crates-g1..g4.md and npm.md](/Volumes/D/claude/rightkit/docs/audits/2026-10-10-audit-evidence/).

### Duplicated primitives with sizes (from static pass, unverified by build)

Column "B cross-check" says what B's source review established. "Not examined by B" means the claim is carried over unchallenged but unproven.

| Primitive | Copies today | Candidate owner | Size | B cross-check |
|---|---|---|---|---|
| Atomic write (write-temp, fsync, rename) | fs; logs `atomic.rs`; models x2; media `staging.rs`; settings `atomic.rs` (incl. `ReplaceFileW` FFI, lines 58-104); service `record.rs:122`; shell `window_state.rs:123`, `recovery.rs:162`; sync `storage.rs:30` (**no fsync despite its "Durable" doc**); fsindex bare `rename` (`index.rs:755,1424`) | `rightkit-fs` | ~600 LOC (A estimate) | B 02 body-level #1 verifies seven implementations beside fs (logs, daemon, models store, models suite manifest, models suite refs, service record, settings) and rejects drop-in replacement. The sites A adds beyond B's seven (media `staging.rs`, shell x2, **sync `storage.rs:30` without fsync**, fsindex bare `rename`) are not examined by B. |
| File lock | fs `FileLock`; jobs libc record lock (`runner.rs:55-100`); models `lock.rs` + `suite/lease.rs` + `suite/refs.rs`; service `flock` (`discovery.rs:316`) | `rightkit-fs` | ~300 LOC | Not examined by B. |
| Digest (sha2) | fs `digest`; embed; mcp; playback (pinned sha2); tts `models.rs` manifest verifier | `rightkit-fs` | ~200 LOC | B #3 confirms the tts `sha256_file` swap; the rest not examined. |
| base64 | secrets `store.rs:294`; shell `b64.rs`; sync `helper/wire.rs:16-31`; oauth helper; license + models `rightapps` base64url | one helper (fs or a `rightkit-encoding` module) | ~150 LOC | Not examined by B. |
| Bounded subprocess (deadline, output cap, kill) | ocr `worker.rs`; ownership `runner.rs`, `gitio.rs`; qa `process.rs`; media `runner.rs`; ocr and ownership use plain `Child::kill`, no tree kill | `rightkit-process` | ~900 LOC | B examines only media/playback (P2 playback); the rest not examined. |
| Pid liveness / tree kill | qa `process.rs:13`, `storage.rs:662`; sandbox `process_tree`; process `terminate_tree` | `rightkit-process` | ~250 LOC | B #2 verifies the fs vs service PID-liveness divergence (PID 0, Windows access denied). qa/sandbox/process copies not examined. |
| Stdio JSON helper spawn | sync `helper/process.rs`; secrets `apple_helper.rs`; mcp JSON-lines framing | `rightkit-process` + `rightkit-framed-sidecar` | ~400 LOC | Not examined by B; B keeps the framed-sidecar / service / speech-client layering. |
| HTTP client | http; mcp `ureq` (`client.rs:251,774`); models `ureq` (public `http_agent`); daemon; browser via chromiumoxide reqwest | `rightkit-http` | ~500 LOC | **B refutes a blanket merge** (02 P2 HTTP): per-client review only. |
| HTTP mock server | http `testing`; qa `mock.rs`, `http.rs` (models depends on all of qa for this) | `rightkit-http::testing` | ~400 LOC | Not examined by B. |
| ONNX session setup | ort `SessionOptions` (used only by embed); ocr `support/session.rs`; asr 12 builder sites + `execution.rs`; vad; qts `vocoder.rs:781` | `rightkit-ort` (needs 2 DirectML knobs first: sequential execution, no memory pattern) | ~700 LOC | B notes asr runtime selection already delegates to `rightkit-ort` (asr `Cargo.toml:31-41,52-74`); the 12 builder sites are not examined. |
| Media parsing (MP4/WAV probe) | qa `media.rs`, `loudness.rs`; media `quickprobe`, `probe`, `loudnorm`; record `media.rs`; playback ffmpeg locator | `rightkit-media` | ~1,200 LOC | B P2 playback confirms playback builds its own FFmpeg command beside media's runners; keeps both pending parity. |
| Loudness measurement | audio; media; qa | `rightkit-media` (or audio) | ~300 LOC | B keeps audio atop media ("Confirmed non-duplication"); qa copy not examined. |
| `TimedWord` + SRT/VTT export | transcript; speech-client; asr `longform.rs:418-527` | `rightkit-transcript` | ~250 LOC | Not examined by B (B keeps transcript/align split). |
| WAV load | audio; asr; tts; voiceright-style callers | `rightkit-audio` | ~150 LOC | B keeps the QTS reference decoder separate from audio's WAV decoder; others not examined. |
| Hardware / memory probe | models `hardware`; logs `system` (+ objc2-foundation, sysctl) | one of them, or a shared `sysinfo` | ~300 LOC | Not examined by B. |
| Windows FFI declarations | fs, db, models, settings hand-declare; hotkeys, logs use `windows-sys`; qa uses `windows` | `windows-sys` everywhere | n/a | B shows the control/shell `windows` 0.61/0.62 aliases are deliberate; settings `ReplaceFileW` is inside B's atomic-write analysis. |
| macOS CGEvent / AX FFI | hotkeys; automation; control | shared effects core | ~400 LOC | **DISPUTED**, see below. |
| Ed25519 + base64url verify | license; models `rightapps`; (rightsites Worker, outside RightKit) | `rightkit-license` | ~200 LOC | B keeps language-native license verification (JS hint, Rust/Swift fail closed); models `rightapps` copy not examined. |
| Lease / admission / events control plane | automation; control; browser `policy.rs` | shared effects core | ~1,100 LOC | **DISPUTED**, see below. |
| Service discovery paths, FNV tag, `endpoint_for`, record/plan types | service; speech-client (~130 LOC of wrappers) | `rightkit-service` | ~130 LOC | B keeps the three-layer split (framed-sidecar / service / speech-client); the wrapper overlap is not examined. |
| `CONTROL_METHODS` table | control; agent-tools hard-codes a copy (`backends/control.rs:14`) | `rightkit-control` | small | Not examined by B. |
| Filename search | search `filename`; fsindex | decide one owner | ~300 LOC | Not examined by B (B keeps fsindex and search as separate authorities). |
| Data root resolution | service `Suite::resolve` (fail-closed); tts `data_root()` falls back to temp | `rightkit-service` | small | Not examined by B. |

npm duplicates:

| Duplicate | Where | Candidate owner | B cross-check |
|---|---|---|---|
| `Invoke` type and typed invoke wrappers | `@rightkit/shell`, `@rightkit/tauri` | (A proposed merging the two packages; not imported) | B keeps the structural twin deliberately: shell must stay dependency-free and the type has no runtime policy (02 "Same-domain owners"). |
| Theme store and tokens | `app-shell/src/theme.ts`, `shell.css` vs `theme` `SUITE_DEFAULTS` | `@rightkit/theme` | Not examined by B. |
| `ModelEntry` / `ModelsBackend` | onboarding, settings-ui | extract to one package | Not examined by B. |
| `e2e/browser.ts` | identical in app-shell, theme, onboarding; near-copies in settings-ui, doc-render, pdf-ui, media-ui | `@rightkit/qa` | B #4 proves the three identical copies and the settings-ui fork; the doc-render, pdf-ui and media-ui near-copies are not examined. |
| Five byte-identical mirrors of release ownership files | `build-control-policy` via `rightkit-sync.mjs:185-187` | (A: the new ownership package becomes the source) | B #5: intentional generated synchronization, keep. A's source-change idea depends on the release split B refuted, so it is not imported. |

### Release ownership group is separable: a fact, not a recommendation (from static pass, unverified by build)

`packages/release` is 23,855 source LOC in 14 responsibility groups (A count). Its ownership group (27 files, 5,585 LOC plus 5.5k JSON) imports back only `background-process`, `committed-source`, `standalone-clone-verify` and `installer-storage-doctor`, so it is cleanly separable (it was already planned as audit item 4.2). **B's decision stands: keep one package and the current command facade** (02 "Large-module decomposition review"); the separability note exists so a later decision does not have to recount. Other groups A counted: signing/notarization 28 files / 2,511 LOC, install-app 3 files / 1,481, cargo + managed-* 7 files / 927, swift/msix 11 files / 992. A's recommendation to split these out is not imported.

### Retire and delete candidates (from static pass, unverified by build)

| Candidate | Action proposed by the static pass | Evidence given | B cross-check |
|---|---|---|---|
| `rightkit-qts-ggml` | retire | already deprecated at 0.1.1, no dependents, 59-line facade | B's crate inventory says "Keep" (safe wrapper over vendored ggml FFI) and shows it is used at `crates/rightkit-qts-ggml/src/lib.rs:7`; B did not check the deprecation or dependents. **Open: Adrian decides**; verify dependents (including app manifests) before retiring. |
| Pulse `PulseMacPrototype` | delete or quarantine | 8.3k LOC of dead Swift, macOS 13, 7 tests, not built in CI (Pulse repository, not RightKit) | Not examined by B; Pulse owns the decision. Also listed in 03's Swift table. |

### DISPUTED: A cites importer and line evidence, B keeps product boundaries; Adrian decides

| Item | A (static pass) | B (source review) | What would settle it |
|---|---|---|---|
| `rightkit-automation` + `rightkit-control` (+ browser `policy.rs`): extract a shared effects core | ~1,100 duplicate lines across lease, admission, events, key tables and macOS AX/CG FFI (A sizes the CGEvent/AX FFI shared with hotkeys separately, ~400 LOC, in the primitives table); automation is `publish = false` and apps need both; keep thin per-domain crates | "No merge": control owns in-app WebDriver/webview admission and durable control sessions, automation owns external desktop accessibility policy; similar `admission`, `lease`, `keys`, `json` names serve distinct authority boundaries (02 "Same-domain owners"). | A diff of the two lease/admission/key-table modules showing identical behavior versus divergent policy; both crates' parity fixtures. |
| RightKitSwift `RightKitSpeech` split | keep ~674 LOC (Apple speech, dictation, VAD) shared; move ~3,241 LOC (Core ML Parakeet, Whisper) to HeardRight or a separate product, because HeardRight is the only importer and ScrapeRight and CodeRight carry local copies of the Apple-speech parts | B keeps the product boundary and lists Speech as a shared product with a high parity burden against Rust ASR/audio/VAD; its findings (A01, A04, A05) are fixes inside the product, not a split. | Importer census of each RightKitSpeech type across apps, and whether CodeRight/ScrapeRight's local copies can adopt the shared 674 LOC. |

Not imported because B's review refuted or declined them: A's splits of release, models, asr, qa and shell; the shell + tauri, qa + qa-chrome and playback to media merges; the npm type-only to dev moves; and the fabric item.

## Swift products & cross-runtime boundaries

### Swift inventory

`RightKitSwift` is SwiftPM 6.0, iOS 17/macOS 14, 14 library products, 14 test targets, zero declared third-party packages. Product/target map is exact at `/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:7-68`; source version is `/Volumes/D/claude/rightkit/swift/RightKitSwift/VERSION:1`.

| Product | Target dependency | Platform frameworks observed | Boundary assessment |
|---|---|---|---|
| RightKitLicense | none | Foundation, CryptoKit, Security | Native license token, verifier & storage; overlaps `@rightkit/license` & `rightkit-license` domain, no code dependency. |
| RightKitDiagnostics | none | Foundation | JSONL persistence/redaction overlaps diagnostics role of `rightkit-logs`; no shared format dependency found. |
| RightKitArtifacts | zlib linker only | CryptoKit, Foundation, zlib | Native artifact validation/install; no direct Rust/npm dependency. |
| RightKitPurchases | RightKitLicense | Combine, StoreKit | Apple StoreKit boundary; dependency is declared at `/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:38-44`. |
| RightKitCloudSync | none | CloudKit, Foundation, Network | Apple CloudKit transport; used by Rust helper through process protocol. |
| RightKitStore | none | Foundation, Darwin | App Group storage/locks. |
| RightKitImportQueue | RightKitStore | Foundation, UniformTypeIdentifiers | Share intake queue; dependency declared at `/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:48-53`. |
| RightKitLiveActivity | none | ActivityKit, Foundation | Apple live activity lifecycle. |
| RightKitWatchBridge | none | Foundation, WatchConnectivity | Watch transfer ledger & protocol. |
| RightKitSpeech | none | AVFAudio, AVFoundation, Accelerate, CoreML, Speech | CoreML/Apple ASR path; capability overlaps Rust ASR/audio/VAD family. |
| RightKitIntents | none | AppIntents, Foundation | Apple App Intents bridge. |
| RightKitAppleCredentials | none | Foundation, LocalAuthentication, Security | Apple credential storage. |
| RightKitOverlay | none | AppKit, UIKit | Apple overlay behavior. |
| RightKitUI | none | SwiftUI | Apple adaptive-glass surface. |

Import inventory is limited to Apple SDK modules plus internal `RightKitCloudSync`, `RightKitLicense`, & `RightKitStore`; no third-party SwiftPM import appears in package sources. Source-level import evidence starts at `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseClient.swift:1`, `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitCloudSync/CloudKitSyncTransport.swift:1`, `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/SpeechEngines.swift:1`, & `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitAppleCredentials/RightKitAppleCredentials.swift:1`.

`RightKitCloudKitHelper` is distinct executable-only SwiftPM package, macOS 14, with one product. It uses `../RightKitSwift` path dependency at `/Volumes/D/claude/rightkit/swift/RightKitCloudKitHelper/Package.swift:7-21`; therefore it is not standalone-publishable. Its source explicitly bridges Rust `rightkit-sync` via line JSON at `/Volumes/D/claude/rightkit/swift/RightKitCloudKitHelper/Sources/rightkit-cloudkit-helper/main.swift:1-12` & consumes `RightKitCloudSync` at `/Volumes/D/claude/rightkit/swift/RightKitCloudKitHelper/Sources/rightkit-cloudkit-helper/Session.swift:13-15`. This is deliberate cross-runtime coupling, but release packaging must replace path resolution with same-tag source or bundle helper with consumer.

Third SwiftPM manifest is record-helper package `/Volumes/D/claude/rightkit/crates/rightkit-record/helpers/macos/Package.swift:1-34`: macOS 14; executables `rightkit-record-macos-helper` & `rk-fixture-window`; internal `RecordWire` target; helper depends only on `RecordWire`; test target depends only on `RecordWire`. It declares no package dependencies. It imports Apple frameworks Foundation, AppKit, ScreenCaptureKit, AVFoundation, CoreMedia, CoreVideo, CoreGraphics, Security, UniformTypeIdentifiers at `/Volumes/D/claude/rightkit/crates/rightkit-record/helpers/macos/Sources/rightkit-record-macos-helper/CaptureSession.swift:1-7` & `/Volumes/D/claude/rightkit/crates/rightkit-record/helpers/macos/Sources/rightkit-record-macos-helper/System.swift:1-5`. It belongs to `rightkit-record`, whose Cargo manifest declares `publish = false`; no independent version, ledger entry, or gate receipt exists.

### Redundant or split capability boundaries

| Finding | Evidence | Risk |
|---|---|---|
| License is independently implemented in Swift, Rust & TypeScript. | Swift `LicenseClient` starts `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseClient.swift:1`; Rust `rightkit-license` manifest `/Volumes/D/claude/rightkit/crates/rightkit-license/Cargo.toml:1-6`; TS client exports `createLicenseClient` at `/Volumes/D/claude/rightkit/packages/license/src/index.ts:117`. | Contract drift unless shared vectors are gated per release. |
| Diagnostics has separate Swift JSONL store & Rust logging crate. | Swift store `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitDiagnostics/JSONLDiagnosticStore.swift:1`; Rust JSONL collection `/Volumes/D/claude/rightkit/crates/rightkit-logs/src/collect.rs:93-186`. | No direct duplicate dependency; event schema/redaction behavior can diverge. |
| Speech capability overlaps Swift CoreML engines with Rust ASR/audio/VAD. | Swift engine surface `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/SpeechEngines.swift:7-235`; Rust ASR identifies itself as Parakeet transcription at `/Volumes/D/claude/rightkit/crates/rightkit-asr/src/lib.rs:1-19`; Rust VAD spans `/Volumes/D/claude/rightkit/crates/rightkit-vad/src/spans.rs:40-132`. | High parity burden; no shared dependency or version handshake found. |
| Cloud sync is split correctly at runtime boundary, but helper is source-bound. | Helper package path dependency `/Volumes/D/claude/rightkit/swift/RightKitCloudKitHelper/Package.swift:13-20`; Rust config names helper at `/Volumes/D/claude/rightkit/crates/rightkit-sync/src/helper/process.rs:52-56`. | Helper cannot independently resolve published Swift source. |

No redundant third-party Swift dependency exists: `RightKitSwift` has no `dependencies:` section, while helper has only its local path dependency.


## Decomposition acceptance

Each proposed consolidation must preserve errors, persisted formats, privacy/permissions, retry/cancellation, feature opt-outs & platform behavior. Keep public facades during migration. Delete old implementations only after consumer-level parity fixtures pass; package splitting needs demonstrated dependency or release-cadence benefit, not line count.

Duplicate candidate evidence: [duplicate-candidates.json](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/duplicate-candidates.json); rerun scanner: [scan-duplicates.py](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/scan-duplicates.py).

## Coverage & verification status

| Dimension | Result |
|---|---|
| RightKit source inventory | All 24 npm package manifests & 56 workspace crate manifests enumerated; 26 npm manifests including workspace root/template, 66 Rust package manifests including 10 fixtures/examples/template packages. |
| Declared dependencies | 171 npm declarations & 437 Rust direct edges, retaining runtime/dev/build/peer/optional/target/feature scope. Swift & .NET helpers listed separately. |
| App census | 22 current/retained checkout roots, 260 npm/Cargo RightKit declarations, 3,109 lexical references across 20,718 hashed files; semantic review distinguishes imports, adapters, comments & generated assets. |
| Duplication scan | 1,042 first-party source/e2e files, 229,545 significant lines; exact 16-line windows identify candidates, followed by source-body review. No AST-complete clone claim. |
| Apple skill follow-up | Both Legion iOS & macOS skills applied; all 14 Swift products, helpers, template/spike & Apple consumers reviewed. macOS suite: 201 cases/3 skips/0 failures; iOS product build passed; StoreKit simulator: 8 pass; CloudKit helper built; recorder helper: 14 cases/1 skip/0 failures; contract parity: 2 pass. Old archive helper: 1 pass/2 failures. Exact logs & closure checks are linked below. |
| Other runtime/package qualification | No new Windows qualification, actual packed-registry-artifact installation or live registry verification accepted. Native Apple checks supplement source review, not app/device/distribution qualification. |
| Native Audit gate | **INCOMPLETE; quality gate UNPROVEN.** 78 providers returned results, only 16 marked complete; no reasoning receipt ingested. Graph unavailable; source byte limits hit; lens packets failed with `plan digest differs from the executed plan`. `legion verify` failed: `native application config is required`. |

Frozen Audit runner output is recorded separately, not treated as verified product findings. Direct source review & separately executed Apple checks supply inventories, recommendations & bounded native evidence. Use Apple closure checklist for remaining repairs, then qualify selected published versions & app integrations.

## Apple core: eight product verdicts & source findings

### Scope & method

Fresh read-only review of all source & test bodies in eight `RightKitSwift` products: `RightKitLicense`, `RightKitDiagnostics`, `RightKitArtifacts`, `RightKitPurchases`, `RightKitCloudSync`, `RightKitStore`, `RightKitImportQueue`, & `RightKitAppleCredentials`. Review covered public API boundaries, iOS 17/macOS 14 availability, actor isolation, task lifetime/cancellation, persistence/cross-process behavior, secure storage, dependency use, decomposition, duplication, & test evidence. `Package.swift` declares iOS 17/macOS 14 at [Package.swift:9](/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:9); StoreKit, CloudKit, Security, & UIKit use availability/conditional-import guards where relevant.

This section records source analysis. Executed native checks are consolidated in [Apple verification](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-verification.md); source findings remain distinct from reproduced runtime failures.

### Findings

#### P2 — file-share intake can bypass configured byte cap

**Source-confirmed.** [`ShareItemExtractor.stageExternalFile`](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitImportQueue/ShareItemExtractor.swift:118) rejects an input only when `fileSizeKey` is present & already exceeds `maxFileBytes` ([lines 121–125](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitImportQueue/ShareItemExtractor.swift:121)). It then calls `copyItem` with no streamed count or post-copy cap ([lines 126–132](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitImportQueue/ShareItemExtractor.swift:126)).

**Caller & failure.** `payload(for:)` reaches this path for both provider file representations & file-URL fallback ([lines 74–88](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitImportQueue/ShareItemExtractor.swift:74)). A provider with an absent, stale, or changed size can stage an arbitrarily large file into app-group/temporary storage, exhausting extension or device storage despite caller-configured cap.

**Repair.** Stream/copy into a unique temporary file while counting bytes; stop above cap, remove partial output, then atomically publish. Preserve security-scoped access through entire copy.

**Test gap.** [`extractorStagesFilesWithinCapAndRefusesOversizedOnes`](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitImportQueueTests/ImportQueueTests.swift:49) covers regular files whose `fileSizeKey` is known. Add unknown-size & source-growth fixtures asserting no oversized staged file survives.

#### P2 — concurrent diagnostics writers can lose records

**Source-confirmed.** `JSONLDiagnosticStore` is `Sendable` ([JSONLDiagnosticStore.swift:21](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitDiagnostics/JSONLDiagnosticStore.swift:21)) but performs directory creation, trim, `seekToEnd`, & write without serialization ([lines 53–66](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitDiagnostics/JSONLDiagnosticStore.swift:53)).

**Caller & failure.** Any concurrently used copies targeting one `fileURL` can both seek same end offset before either write; later write replaces earlier bytes. A simultaneous `trimIfNeeded` atomic replacement ([lines 87–98](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitDiagnostics/JSONLDiagnosticStore.swift:87)) can also orphan another writer's append on replaced inode. Observable result: missing diagnostic events or malformed/torn log around capacity trimming.

**Repair.** Serialize full read/trim/append transaction per canonical file path; use an app-group-compatible file lock if callers share path across processes. Keep write/trim failure visible through existing `false` return.

**Test gap.** Tests cover sequential stability, trim, & torn trailing write ([DiagnosticsTests.swift:42](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitDiagnosticsTests/DiagnosticsTests.swift:42)); add concurrent same-path writers, including forced trim, & assert all committed JSON lines remain readable.

#### P3 — CloudSync subscription task can outlive owner through bounded retry lifetime

**Source-confirmed finite-lifetime concern.** `CloudSyncEngine` retains `subscriptionTask` ([CloudSyncEngine.swift:90](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitCloudSync/CloudSyncEngine.swift:90)); task invokes `await self?.runSubscription(...)` ([line 714](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitCloudSync/CloudSyncEngine.swift:714)). That async call retains actor throughout bounded retry sleeps/network calls ([lines 717–742](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitCloudSync/CloudSyncEngine.swift:717)), while no `deinit` cancels task.

**Caller & failure.** `prepareZoneIfNeeded()` starts subscription after a sync pass ([CloudSyncEngine.swift:680](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitCloudSync/CloudSyncEngine.swift:680)). Releasing last app owner during transient subscription failure can leave engine, adapter, transport, & retries alive until retry loop ends; background CloudKit work outlives feature lifecycle. Source establishes bounded retention, not an unbounded persistent leak.

**Repair.** Add `deinit { subscriptionTask?.cancel() }` & avoid long-lived strong actor capture across awaited subscription work, or expose explicit shutdown called by composition root. Ensure task clears state on cancellation.

**Test gap.** Subscription retry test proves bounded retry ([CloudSyncEngineTests.swift:158](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitCloudSyncTests/CloudSyncEngineTests.swift:158)), but no owner-release/cancellation assertion exists.

#### Conditional API risk — shared license-removal state has no cross-process transaction

**Hypothesis; no inspected caller proves shared storage.** Default `UserDefaultsLicenseDeviceState` uses bundle-default defaults ([LicenseRemovalQueue.swift:18](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseRemovalQueue.swift:18)), which alone does not establish app/extension sharing. However, public `LicenseDeviceStateStoring` supports caller-supplied shared persistence; `PendingLicenseRemovalQueue.enqueue` does unsynchronized `load` → append → `set` ([lines 84–90](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseRemovalQueue.swift:84)). Its actor & `RemovalDeliveryLock` serialize only process-local work ([lines 65–73](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseRemovalQueue.swift:65), [158–185](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseRemovalQueue.swift:158)).

**Schedule.** Two processes read `[A]`; each appends a distinct removal; last storage write wins, dropping one server-seat removal. Resolve consumer storage before raising priority; if app-group defaults/keychain state is intentionally shared, provide lock/CAS-backed storage & cross-process test.

**Test gap.** Existing test covers two queues over `InMemoryLicenseDeviceState` in one process ([LicenseClientTests.swift:1460](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitLicenseTests/LicenseClientTests.swift:1460)), not separate processes or shared durable state.

### Per-product verdict & coverage

| Product | Verdict | Source/test coverage |
| --- | --- | --- |
| RightKitLicense | Conditional persistence limitation above; otherwise strong serialization & verifier review. | Read `LicenseClient.swift`, `LicenseRemovalQueue.swift`, `LicenseToken.swift`; tests cover verifier contracts, Keychain remove/load race, serialized controller operations, & single-flight retry. Gap: shared-storage/process interleaving. |
| RightKitDiagnostics | P2 concurrent-write loss. | Read JSONL store & redactor; tests cover redaction, stable lines, trimming, torn-tail recovery. Gap: concurrent writers/trim. |
| RightKitArtifacts | No source-confirmed finding. | Read tar/gzip verifier, staged activation/rollback & all artifact tests. Traversal, symlink, checksum, inflated size, truncation, rollback covered. Gap: simultaneous installs to one destination root. |
| RightKitPurchases | No source-confirmed finding. | Read ledger, controller, StoreKit backend, storage, models, access resolver & tests. Main-actor controller owns UI mutation; StoreKit backend actor owns pending transactions; iOS-only scene paths/17.2 offer fields are guarded. Unit coverage spans entitlement, refresh-hint, token inheritance, outcomes, restore/refund; StoreKit suite covers simulator flow. Gap: real macOS StoreKit catalog behavior & controller teardown while update stream is active. |
| RightKitCloudSync | P3 bounded subscription lifetime concern. | Read engine, coordinator, triggers, transport, state store, models, adapter, retry. Actor owns engine/coordinator mutable state; account fencing, retry, token replay, failures, push watchdog, & state recovery have broad unit coverage. Gap: engine release during subscription retry; multi-process same-file state store. |
| RightKitStore | No source-confirmed finding. | Read cross-process lock, command queue, owner lease, revisioned store. File-lock fail-closed behavior, real separate lock-holder process, owner epoch checks, queue leasing, & persistence tests exist. Gap: crash between multi-key acknowledgement writes is designed toward at-least-once semantics but lacks crash-process validation. |
| RightKitImportQueue | P2 staging-cap bypass. | Read payload/configuration/extractor/durable queue. Tests cover queue restart, migration, corruption, atomic write fault seams, basic staging & known-size reject. Gap: unknown/stale file size & capped streaming. |
| RightKitAppleCredentials | No source-confirmed finding. | Read store, in-memory backend, Security backend; scoped sync/device identities, passive probes, status propagation, broad deletion, & macOS legacy-Keychain query construction covered. Gap: native entitlement-provisioned iOS/macOS integration coverage remains unrun. |

### Architecture & dependency review

Package remains decomposed by product, with dependencies limited to Apple SDK/Foundation/CryptoKit plus internal `RightKitPurchases`→`RightKitLicense` and `RightKitImportQueue`→`RightKitStore` edges declared in [Package.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:9). No third-party package dependency appears. `@MainActor` owns UI-facing controllers; actors own CloudSync, StoreKit backend, & removal waves. Existing App Group primitives should be reused for diagnostics or any intentionally cross-process state rather than adding parallel locking implementations.


## Apple UI/platform: six product verdicts & source findings

Source-led review of `RightKitSpeech`, `RightKitOverlay`, `RightKitUI`, `RightKitIntents`, `RightKitLiveActivity`, & `RightKitWatchBridge`. Package declares Swift 6.0 with iOS 17/macOS 14 minimums at [Package.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:1) & [Package.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:8). Apple Speech behavior is verified against linked Apple documentation. This section records source analysis; executed native checks are consolidated in [Apple verification](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-verification.md).

### Findings

#### P1 — legacy Apple Speech can send audio over network

[AppleSpeechEngine.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/AppleSpeechEngine.swift:54) enters legacy recognition below iOS/macOS 26. It creates request at [line 62](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/AppleSpeechEngine.swift:62), but sets `requiresOnDeviceRecognition` only when `supportsOnDeviceRecognition` is true at [line 65](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/AppleSpeechEngine.swift:65). For an available, network-only recognizer, direct `AppleSpeechEngine.transcribe` or `SpeechEngineSelector.resolve(.apple)` therefore starts a request with default network-allowed policy, contradicting source's on-device claim at [lines 5–8](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/AppleSpeechEngine.swift:5).

Apple states `requiresOnDeviceRecognition = true` prevents network audio transfer, while a recognizer without on-device support requires network: [request property](https://developer.apple.com/documentation/speech/sfspeechrecognitionrequest/requiresondevicerecognition), [recognizer capability](https://developer.apple.com/documentation/speech/sfspeechrecognizer/supportsondevicerecognition). Reject unsupported recognizers before creating task, or expose explicit remote-recognition consent policy. [AppleSpeechAndDictationTests.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitSpeechTests/AppleSpeechAndDictationTests.swift:6) covers only pure `Dictation`; no legacy request-policy test exists.

#### P2 — replaced dictation start can publish stale permission failure

[LiveDictationSession.start](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/LiveDictationSession.swift:64) replaces prior generation, while `run` awaits speech permission at [line 84](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/LiveDictationSession.swift:84) & microphone permission at [line 88](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/LiveDictationSession.swift:88). Either denied completion emits `.failed` at [lines 85–90](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/LiveDictationSession.swift:85) before current-generation validation at [line 92](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/LiveDictationSession.swift:92).

Caller schedule: start A, start B before A's permission callback, then deny A. A's retained event closure receives a stale terminal failure after its session has been replaced; UI using one shared sink can dismiss or error current B dictation. Revalidate generation/cancellation after each suspension before emitting event. [AppleSpeechAndDictationTests.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitSpeechTests/AppleSpeechAndDictationTests.swift:6) has no `LiveDictationSession` overlap, stop, or permission-race coverage.

#### P2 — canceled legacy file transcription continues & returns a transcript

Legacy recognition awaits an unchecked continuation at [AppleSpeechEngine.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/AppleSpeechEngine.swift:66); it retains neither returned `SFSpeechRecognitionTask` nor a cancellation handler. Canceling caller task while `recognitionTask` runs neither cancels request nor checks cancellation before resuming success at [lines 68–75](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/AppleSpeechEngine.swift:68). A canceled caller can therefore remain blocked until service completes, then receive a transcript. This leaks obsolete audio work across view/session lifetime.

Own task in cancellation handler, cancel it, & gate continuation with cancellation state. Same test file has no cancellation case: [AppleSpeechAndDictationTests.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitSpeechTests/AppleSpeechAndDictationTests.swift:6).

#### P2 — `BoundedStringLog` claims Sendable safety without atomic dedupe

[BoundedStringLog](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitWatchBridge/WatchBridgeSupport.swift:61) is `@unchecked Sendable`, yet `insertIfNew` reads at [line 85](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitWatchBridge/WatchBridgeSupport.swift:85) then writes through separate `UserDefaults` operations at [lines 75–80](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitWatchBridge/WatchBridgeSupport.swift:75). Two concurrent delegate/service calls for same receipt can both observe absence, each return `true`, then execute duplicate result presentation or orphan cleanup. `@unchecked Sendable` makes cross-isolation use part of public contract; no lock or actor protects read-modify-write sequence.

Serialize whole operation with private lock or actor, then add two-task same-value test. Existing test invokes sequentially only: [BridgeTests.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitWatchBridgeTests/BridgeTests.swift:77).

### Build warning requiring Live Activity isolation design

iOS Simulator build completed for arm64 & x86_64, but reports Swift 6 `#IsolatedConformances` warnings at [build log:2025](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-ios-build.log:2025), [build log:2033](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-ios-build.log:2033), & [build log:2041](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-ios-build.log:2041). Each points to `await Activity<Attributes>` calls at [LiveActivityHost.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLiveActivity/LiveActivityHost.swift:58), [LiveActivityHost.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLiveActivity/LiveActivityHost.swift:73), & [LiveActivityHost.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLiveActivity/LiveActivityHost.swift:82): generic `Attributes: ActivityAttributes` may be actor-isolated while passed to an `@concurrent` ActivityKit call. No runtime failure is established, but current package lacks a host-instantiation test with actor-isolated attributes; resolve ownership/isolation before warning becomes an error.

### Product coverage

| Product | Verdict | Evidence |
|---|---|---|
| RightKitSpeech | 3 findings | Legacy privacy, cancellation, & dictation-generation defects above. Reviewed all 23 source files plus 7 test files; Core ML/Whisper paths have substantial deterministic cancellation coverage, but Apple Speech/LiveDictation lack direct tests. |
| RightKitOverlay | No source defect established | [Overlay.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitOverlay/Overlay.swift:1) clamps model geometry, preserves hitbox displacement, & isolates `NSPanel` on main actor; [OverlayTests.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitOverlayTests/OverlayTests.swift:8) covers model plus macOS panel invariants. All 5 macOS model/panel tests passed during native verification; installed-app window behavior still needs app QA. |
| RightKitUI | No source defect established | [AdaptiveGlassSurface.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitUI/AdaptiveGlassSurface.swift:64) gates iOS/macOS 26 glass, uses supported fallback, & selects opaque rendering for reduced transparency/increased contrast; policy branches covered at [AdaptiveGlassSurfaceTests.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitUITests/AdaptiveGlassSurfaceTests.swift:4). Device accessibility rendering remains unrun here. |
| RightKitIntents | No source defect established | [RecordingBridge.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitIntents/RecordingBridge.swift:109) separates foreground driver from queued extension handoff & permission handshake. Workspace search found no app-intent consumer/extension target, so end-to-end dispatch, relaunch drain, & system-surface execution remain unproven; this is not recorded as defect without caller evidence. |
| RightKitLiveActivity | Build warning; no runtime defect established | [ActivityLifecycle.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLiveActivity/ActivityLifecycle.swift:26) validates session/fence state; [LiveActivityHost.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLiveActivity/LiveActivityHost.swift:44) availability-gates ActivityKit host. Swift 6 isolation warnings above apply to host calls; [LifecycleTests.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitLiveActivityTests/LifecycleTests.swift:13) covers pure lifecycle only. |
| RightKitWatchBridge | 1 finding | Transfer ledger source requires caller-side serialization at [TransferLedger.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitWatchBridge/TransferLedger.swift:26), so no separate defect recorded there. Public `BoundedStringLog` violates its own Sendable contract above. |

Platform guards, deployment minimums, Swift 6 mode, state ownership, cancellation, permission boundaries, system integration, dependency separation, duplicate implementations, & relevant tests were reviewed. No duplicate implementation requiring decomposition was established inside these six products.


## Apple remediation acceptance

Audit coverage complete; remediation remains open. IDs below provide one backlog for RightKit & app chats. Product/source changes were not made during audit. “Source-confirmed” describes code behavior; only archive failure & compiler warnings were reproduced directly during this pass.

| ID / priority | Owner & finding | Required change | Acceptance evidence |
|---|---|---|---|
| A01 / P1 | RightKitSpeech: legacy Apple engine permits network recognition despite on-device contract. | Reject unsupported on-device locale/recognizer before creating task, or require explicit remote policy. | Inject unsupported/supported recognizer capabilities; prove no network-capable request starts under local-only policy; cover pre-26 path. |
| A02 / P2 | RightKitImportQueue: file copy trusts optional pre-copy size; hard byte cap can be bypassed. | Bounded copy into unique staging file, cleanup on over-cap/error, atomic final publication. | Unknown/misreported size, growth during copy & copy failure leave no oversized/partial staged file; ordinary file import still passes. |
| A03 / P2 | RightKitDiagnostics: uncoordinated seek/write/trim can lose same-path records. | Serialize full transaction by canonical path; apply process-safe locking where app-group sharing is supported. | Concurrent writers with forced trim produce readable committed lines; interrupted writer preserves recoverable log. |
| A04 / P2 | RightKitSpeech: canceled/replaced dictation start emits stale permission failure. | Recheck generation/cancellation immediately after each awaited permission, before events. | Start A → replace/stop → deny delayed A callback produces no obsolete terminal event; current start retains normal success/failure behavior. |
| A05 / P2 | RightKitSpeech: legacy file transcription ignores task cancellation. | Own recognition task & cancel/resume exactly once through cancellation handler. | Cancel before start, while recognizing, & racing final callback returns cancellation without duplicate continuation or late transcript. |
| A06 / P2 | RightKitWatchBridge: Sendable `BoundedStringLog` dedupe is not atomic. | Protect entire lookup/insert/prune transaction; define same-key multi-instance/process support explicitly. | Concurrent same receipt is accepted exactly once within supported sharing model; bounded history still prunes. |
| A07 / P2 | Swift tooling: old deterministic archive helper uses obsolete root/tree prefix. | Retire helper or share canonical repository/tree derivation. | Both existing failing archive tests pass; two archives of same commit hash identically & contain complete package closure. |
| A08 / P2 | CodeRight, SellRight, RightSites: source specs allow `from: 0.1.0`. | Pin exact verified published version; regenerate project/lock consistently. | Clean external consumer resolves chosen URL/tag; generated project & lock agree; focused native app checks pass. |
| A09 / P3 | License/LiveActivity/Credentials: actor-isolation & deprecated-query warnings. | Resolve isolation at native API boundary; preserve noninteractive Keychain contract. | Clean iOS/macOS focused builds remove relevant warnings; no-prompt status behavior & ActivityKit integration retain parity. |
| A10 / P3 | CloudSync: subscription retry task retains engine beyond owner lifecycle. | Define explicit shutdown or remove strong ownership across long awaits; cancellation must reach transport/sleep. | Release/shutdown during retry stops subsequent work & releases owned objects. `deinit` alone cannot break already-retained self cycle. |
| A11 / P3 | Swift documentation/template: obsolete registry-only checklist & unverified “unpublished 0.3.0” comment. (Tag `0.3.0` exists per the merged tag listing, `0.1.0`, `0.1.1`, `0.3.0`-`0.3.7`; the comment is therefore likely stale, static check only.) | Reconcile current 14-product URL/tag contract; verify template tag before updating pin/comment. | One current qualification path, exact published pin, clean-clone build & helper closure evidence. |

A02 is prioritized as P2 hard-cap robustness work; no observed device-storage exhaustion is claimed. A08 overlaps original app-adoption inventory; it is one issue, not another independent finding. A10 is bounded lifetime behavior, not proof of permanent memory leak.

Conditional checks: verify consumer storage before treating License removal queue's cross-process read/modify/write as live data loss; inspect app intent execution mode/relaunch drain before classifying Darwin-notification wake behavior; preserve product-specific archive/model, entitlement, recorder & transport ownership during consolidation.

Apply fixes only to affected modules, rerun corresponding fixtures + package suite/iOS compile, then qualify selected published version & each adopting app. Reopen broader audit only when public API, persistence format, dependency closure, signing/distribution contract or app topology changes. Existing report inventories remain baseline for all other work.

