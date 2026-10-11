# Fresh apps-core adoption review — 2026-10-11

Scope: current source, manifests, configs, README files, & `AGENTS.md` in CodeRight, HeardRight, ViewRight, MailRight, ScrapeRight; current `rightkit/INDEX.md`, `packages/release/rightkit-versions.json`, & `docs/RUNTIME-BUDGETS.md`. No earlier audit, build, test, install, network call, or product change informed this review.

Status terms: **declared** means manifest pin only; **integrated** means app source calls exported API; **measured at scale** needs an installed-app run with visible figures. A long-running crate is not fully adopted from either declaration or source integration alone.

## Cross-app findings

| Priority | Finding | Evidence | Required resolution |
|---|---|---|
| P0 | ScrapeRight root `[patch.crates-io]` redirects 23 RightKit crates to `/Volumes/D/claude/rightkit/crates/*`. This violates published-package-only adoption. | `scraperight/Cargo.toml:36-59` | Remove all path patches after needed releases exist; lock exact registry versions; prove standalone build with no parent checkout. |
| P0 | No inspected app records an installed, scale-load runtime budget receipt for its continuously running RightKit owners. | Source/manifests only; no app-owned measured status receipt found. `rightkit/docs/RUNTIME-BUDGETS.md` requires declare, measure, expose, then first-consumer scale run. | Add host status plumbing plus recorded installed runs before calling watch/service/sync/cache/logging adoption complete. |
| P1 | CodeRight, HeardRight, & MailRight retain old 0.1/0.2 Cargo pins while `INDEX.md` has newer source/staged records. Version advancement must wait for corresponding publication. | Exact pins below; `rightkit-versions.json` is current release inventory. | Publish required crates first, then update one owner seam at a time with contract tests. |
| P1 | RightKitSwift adoption is correctly remote URL + exact tag in all three iOS products, pinned to `0.4.0` revision `88a18dc620896ff5df80bf35e1f8c94685bca875`. | each Xcode `Package.resolved`; project YAMLs | Keep URL/tag/revision aligned across app, extensions, helper, & SPM E2E packages. |

`INDEX.md` ownership labels used below: desktop-shell, licensing, structured-logging, tauri-host-adapters, update-policy, safe-filesystem, file-watching, sqlite-storage, model-installation, shared-speech-service, local-services, audio-capture, crash-reporting, OCR, PDF engine, media pipelines, owned-processes, cloud-sync, settings persistence, HTTP client, OS credential store, embeddings, browser automation, MCP, OS sandbox, document rendering, media UI, platform UI, app-shell UI, theme UI, legal UI, onboarding UI, QA, & release tooling.

## CodeRight

### Targets

| Target | Current product linkage | RightKit state |
|---|---|---|
| Desktop Tauri | `CodeRight` `0.1.84`, bundle `app.coderight`; macOS `.app/.dmg` & Windows NSIS; desktop daemon/client/auth crates | integrated in renderer, host, engine |
| Native engine | `engine/` workspace; `coderight` daemon/CLI plus host crates | broad integrated owner adapters |
| Context/Membrane | independent `context/` Cargo/PNPM workspace | declarations exist; treat as separate runtime consumer |
| iOS app | `CodeRight` iOS app | `RightKitLicense`, `RightKitDiagnostics` integrated |
| iOS extensions | `CodeRightFileProvider`, `CodeRightWidgets` | no RightKitSwift product declaration in target YAML |
| iOS tests | `CodeRightTests`, `CodeRightUITests` | consumer verification targets |
| Web | `apps/coderight-web` | no RightKit declaration found |

iOS project pin: `https://github.com/bogusyogi/rightkit-swift.git`, exact `0.4.0`, resolved revision above. App, File Provider, & Widgets carry app group `group.app.coderight.ios`. No source `PrivacyInfo.xcprivacy` was found under `apps/coderight-ios`; privacy manifest delivery remains unproven. Product-specific coding/editor policy belongs local.

### Exact declarations & source use

* Desktop PNPM declares `@rightkit/legal 0.3.1`, `license 0.1.8`, `logs 0.1.4`, `shell 0.1.0`, `tauri 0.2.0`, `updates 0.2.5`; dev `release 0.2.151` in `apps/coderight-tauri/package.json`. Renderer directly uses license/update clients, logs, shell notifications, & Tauri bridge.
* Desktop Cargo declares `license =0.1.3`, `logs =0.2.0`, `tauri =0.1.1`, `shell =0.1.2`, `ort =0.2.0`; auth crate declares `tauri =0.1.1`. Source uses native entitlement/device service, JSONL diagnostics, shell deep-link/single-instance/notifications, & ORT resolver.
* Engine workspace declares `watch/fs =0.1.0`, `oauth =0.2.0`, `http =0.1.0`, `mcp =0.5.1`, `secrets/cache =0.1.0`, `ort =0.2.0`, `db =0.2.1`, `browser =0.2.0`; direct crates add `sandbox =0.2.4`, `process =0.3.2`, `ocr =0.1.2`, `logs =0.2.0`. Source uses FS atomic/private writes, DB pool/migrations, ORT resolver, OAuth/secrets, MCP transport/server, browser session, OCR, process/sandbox, cache prefixes, & watch API.
* Context workspace separately declares PNPM `ax 0.2.4`, `git 0.2.37`, `release 0.2.151`; Cargo `db =0.3.0`, `embed/ort =0.2.2/0.2.0`, `watch =0.3.0`, `mcp =0.5.1`, `process =0.3.2`.

**Remaining shared-primitive duplication:** desktop `keyring-core` plus platform keyring stores coexist with `rightkit-tauri` device/secure APIs; retain only product namespace/migration adapter after secrets/tauri cutover. Local `os_shell`, notification fallback, MCP correlation/policy, ORT candidate order, GitHub scope, editor/agent workflow policy, & iOS file-provider semantics are product policy. `engine/crates/media` still pins OCR `0.1.2`; desktop & engine need one release-compatible OCR/ORT line.

**Runtime qualification:** `rightkit-watch` is long-running in daemon file watch. `rightkit-cache` can be resident eviction infrastructure. Neither has an inspected installed scale receipt/status figures. Current code integration therefore remains **integrated, not measured at scale**.

### Ordered handoff

1. Publish release-compatible fs/watch/cache/DB/ORT/OCR/MCP/secrets/oauth/http/browser/sandbox/process lines; update engine & desktop lockfiles without local substitutions.
2. Replace desktop keyring adapters with one `rightkit-secrets`/`rightkit-tauri` store migration, keeping CodeRight namespace & device policy local.
3. Upgrade engine owner seams; acceptance: daemon FS watch receives rename/debounce events, stored OAuth survives restart, MCP stdio/HTTP authorization works, browser child is reaped, OCR/ORT resolves bundled runtime.
4. Add `watch` & cache host status plus installed whole-repo scale receipt: declared ceiling, RSS/CPU/disk-op figures, status output, data size, command, machine.
5. Add iOS privacy manifest(s) for every shipping target, verify app-group/file-provider/widget product linkage, & run device/simulator license + diagnostics acceptance.

## HeardRight

### Targets

| Target | Current product linkage | RightKit state |
|---|---|---|
| Desktop Tauri | `HeardRight` `0.1.229`, `app.heardright.next`, bundle targets `all` | renderer/native/rust runtime integrated |
| Desktop Rust services | engine, runtime, app-runtime, command-host, dump-helper, inference, model-store, ASR, Parakeet, telemetry, text pipeline | audio/models/service/speech/logs/crash integrated |
| macOS helper package | `HeardRightPillMac` static bridge; `HeardRightMenuBar`, `HeardRightCloudSync`, `HeardRightShortcuts`, `HeardRightAppleCredentials` executables | source package has no RightKitSwift declaration |
| iOS/watch family | HeardRight app, Widgets, Watch app, WatchWidgets, Share extension, IslandAuditCompanion app/widget, UI/sync tests | wide RightKitSwift declaration |
| Cloud service | `cloudflare-workers/model-serve` | no RightKit declaration found |

iOS pin: same RightKitSwift URL/tag/revision. App has CloudKit container `iCloud.app.heardright.ios.native`, app groups `group.app.heardright.ios.native` & `group.app.rightsuite.models`, Apple Sign In, push environment, & `$(AppIdentifierPrefix)app.heardright.shared` keychain. App privacy manifest declares no tracking; user-default/file-timestamp/boot-time required-reason APIs; linked email/user/device IDs & purchase history for app functionality. Share/widget/watch manifests declare no tracking & their narrower required-reason use. This is declaration evidence; binary product linkage remains acceptance work.

### Exact declarations & source use

* Desktop PNPM declares `app-shell 0.1.0`, `legal 0.3.1`, `legal-ui 0.1.3`, `license 0.1.8`, `logs 0.1.4`, `onboarding 0.1.0`, `settings-ui 0.1.0`, `tauri 0.1.1`, `theme 0.1.0`, `updates 0.2.5`; dev `qa-chrome 0.1.3`, `release 0.2.135`. Source imports these shell/settings/legal/license/logging/update surfaces, though multiple app-local components remain.
* Rust pins: `models =0.2.6`; telemetry `crash =0.2.0`, `logs =0.2.0`; core/runtime/engine `speech-client =0.3.0`; runtime `service =0.1.0`, `audio =0.2.0`; app-runtime `process =0.3.2`; Parakeet `asr =0.0.2`; Tauri `license =0.1.3`, `logs =0.2.0`, `tauri =0.1.1`, `audio =0.2.0`. Source calls model download/hash/suite store, native crash, JSONL store, speech contract, shared-speech service, & audio capture.
* iOS app target declares Diagnostics, UI, Purchases, CloudSync, Store, ImportQueue, LiveActivity, WatchBridge, Speech, License, Intents, AppleCredentials. Widgets declare Diagnostics/Store/ImportQueue/WatchBridge/LiveActivity/Intents/License/AppleCredentials; Watch declares Store/WatchBridge/Intents; WatchWidgets Store/Intents; Share ImportQueue/Store. SPM E2E packages declare Store/WatchBridge & Speech/License.

**Remaining shared-primitive duplication:** `heardright-runtime/src/speech_service/{conn,files,lease}` remains a local service protocol/lifecycle projection alongside `rightkit-speech-client` + `rightkit-service`; model resumable wrapper/compat paths retain historic product policy around `rightkit-models`; telemetry owns JSONL rotation around `rightkit-logs`; dump-helper protocol remains local around `rightkit-crash`. Keep microphone/consent/calibration, transcript behavior, wording, audio-routing policy, CloudKit domain schema, & feature gate local.

**Runtime qualification:** `rightkit-service`/shared-speech client runs resident service; `rightkit-logs` tracing worker & any model-store supervisor are long-running. No installed whole-machine speech/service RSS, CPU, disk-op, status, or scale receipt was found. All are **declared/integrated only**.

### Ordered handoff

1. Publish staged app-shell/legal-ui/onboarding/settings-ui/theme & needed Rust service/speech/audio/model/crash/log lines; move exact pins only after publication.
2. Converge local speech-service lease/connection/files wrappers on current speech-client/service public contract; acceptance: one resident engine, multiple clients, reconnect, lease expiry, capture recovery, & clean stop.
3. Consolidate model download/hash/activation on rightkit-models; acceptance: interrupted verified download, rollback, shared model-group compatibility, & migration from old location.
4. Expose service/log worker status & record installed multi-client scale run with microphone capture: declared budget, measured RSS/CPU/disk ops, fixture duration/client count, machine, command, visible status.
5. Build/archive iOS/watch/share/widget family; assert compiled package products, entitlement values, privacy manifests, CloudKit/keychain/app-group behavior, App Intent/Live Activity paths, & device E2E package runs.

## ViewRight

### Targets

| Target | Current product linkage | RightKit state |
|---|---|---|
| Desktop Tauri | `ViewRight` `0.1.165`, `app.viewright` | broad renderer/native integration |
| Native QA | `src-tauri/qa-e2e` separate workspace | rightkit-qa/control/pdf declared |
| Waitlist worker | `workers/waitlist` | no RightKit declaration found |

No iOS, app extension, or Swift package target exists in current source.

### Exact declarations & source use

* PNPM declares `app-shell 0.6.0`, `doc-render 0.1.6`, `legal-ui 0.2.1`, `license 0.1.8`, `logs 0.1.4`, `media-ui 0.3.2`, `pdf-ui 0.1.5`, `platform-ui 0.2.1`, `tauri 0.2.0`, `theme 0.2.3`, `updates 0.2.5`; dev `legal 0.3.1`, `qa-chrome 0.1.3`, `release 0.2.151`.
* Cargo declares `control =0.1.11` optional, `db =0.3.2`, `watch =0.3.1`, `shell =0.1.4`, `logs =0.2.1`, `license =0.1.3`, `tauri =0.1.2`, `secrets =0.1.2`, `http =0.2.5`, `ocr =0.2.0`, `pdf =0.2.0`; QA declares `qa =0.2.14`, `pdf =0.2.0`.
* Source directly hosts shared image viewer/save adapters, PDF viewer helpers, document rendering/Markdown syntax/citations, headless toast/panels, legal gate, logs, Tauri bridge, theme, update/license; native source calls PDF compose/save, OCR isolation, DB, watcher, shell, secrets, HTTP, & QA control.

**Remaining shared-primitive duplication:** local PDF orchestration (`pdf_adapter`, mac preview), image save policy, document lifecycle/autosave/recovery/watcher routing, OCR worker deadline process, & Markdown/HTML/Docx product editing remain large adapters around RightKit engines. Keep document semantics, format fidelity, navigation, save naming, & conflict policy local; move only reusable rendering/worker orchestration proven shared.

**Runtime qualification:** `rightkit-watch` is continuous document watching. No installed large-library run reports watch count, RSS, CPU, disk ops, or `status()` figures, so it is **integrated, not measured at scale**.

### Ordered handoff

1. Publish/advance current staged control, db, watch, shell, logs, secrets, http, OCR, PDF, QA lines; update exact pins with lockfile review.
2. Reduce local watcher to ViewRight path/persistence policy over `rightkit-watch`; acceptance: multiple documents, rename/save storms, external edits, recovery, & shutdown release.
3. Keep PDF compose contract boundary; acceptance: source-text replacement, added text, OCR correction, atomic save, reopen parity, macOS native preview, Windows OCR isolation.
4. Measure installed large-document library watcher run & expose status with required runtime-budget figures.

## MailRight

### Targets

| Target | Current product linkage | RightKit state |
|---|---|---|
| Desktop Tauri | `MailRight` `0.1.0`, `app.mailright`, Windows NSIS; macOS config present | integrated native shell/storage/auth |
| Waitlist worker | `workers/waitlist` | no RightKit declaration found |

No iOS, extension, Swift, or helper target exists in current source.

### Exact declarations & source use

* PNPM declares `license 0.1.8`, `logs 0.1.4`, `tauri 0.1.1`, `updates 0.2.5`; dev `legal 0.3.1`, `release 0.2.135`.
* Cargo declares `db =0.2.1` with SQLCipher, `license =0.1.3`, `logs =0.2.0`, `tauri =0.1.1`, `embed =0.2.1`, `cache =0.1.0`, `secrets =0.1.0`, `oauth =0.2.0`, `http =0.1.0`, optional `shell =0.1.2`.
* Source directly uses SQLCipher database types, embedder, byte cache, secrets, OAuth/HTTP, host license/device service, logs, shell instance/deep-link/notification/autostart, & renderer license/update/log clients.

**Remaining shared-primitive duplication:** local SQL schema/migrations/pool/rekey/health/search/snippet/vector-extension management sits above `rightkit-db`; local attachment cache grouping sits above `rightkit-cache`; Google taxonomy, Gmail sync queues, retention/mail safety, & OAuth provider configuration are product policy. Keep mail model/schema, account ownership, batch behavior, & Gmail error policy local.

**Runtime qualification:** cache is eviction infrastructure; inspect before calling it fully adopted. No installed mailbox-scale cache status/budget receipt was found. The app also needs performance evidence for DB/search but it is outside RightKit long-running adoption status.

### Ordered handoff

1. Publish current DB/embed/cache/secrets/oauth/http/shell lines; move exact Cargo pins together only where dependency graph permits.
2. Keep SQLCipher schema local while using rightkit-db bootstrap/migrations; acceptance: open/create/rekey/reopen, migration recovery, account deletion, concurrent reader/writer, FTS/vector search.
3. Retain Gmail auth policy adapter over rightkit-oauth/http; acceptance: loopback PKCE, refresh, revocation, noninteractive secrets, offline retry, per-account cache invalidation.
4. Run installed whole-mailbox cache scale test; expose cache size/entries/evictions plus RSS/CPU/disk ops & record budget receipt.

## ScrapeRight

### Targets

| Target | Current product linkage | RightKit state |
|---|---|---|
| Desktop Tauri | `ScrapeRight` `0.1.10`, `app.scraperight`, Windows NSIS; bundles external `scraperight-engine` sidecar | integrated, but root path patches invalidate portable adoption |
| Engine sidecar | `crates/engine` JSON-lines engine | broad integrated Rust owners |
| Supporting Rust crates | audio, detect, model-store, OCR, ORT policy, protocol, speech, store | integrated adapters |
| iOS app/share | ScrapeRight app & ScrapeRightShare extension; unit/UI/E2E tests | broad RightKitSwift integration |
| macOS helper | `ScrapeRightAppleCredentials` executable, macOS 14 | RightKitAppleCredentials integrated |

iOS/macOS helper pin: same exact RightKitSwift URL/tag/revision. iOS app entitlements: development APNs, CloudKit `iCloud.app.scraperight.ios.native`, group `group.app.scraperight.ios.native` plus `group.app.rightsuite.models`, keychain `$(AppIdentifierPrefix)app.scraperight.shared`; Share has app group. App privacy manifest declares no tracking/data collection & UserDefaults reasons `CA92.1`, `1C8F.1`; Share has no source privacy manifest found. Acceptance must prove privacy/entitlement linkage in built app & extension.

### Exact declarations & source use

* PNPM declares `app-shell 0.2.0`, `legal-ui 0.2.0`, `license 0.1.8`, `logs 0.1.4`, `media-ui 0.2.5`, `onboarding 0.2.2`, `platform-ui 0.2.0`, `settings-ui 0.2.7`, `tauri 0.2.0`, `theme 0.2.0`, `updates 0.2.5`; dev `legal 0.3.1`, `qa 0.4.0`, `qa-chrome 0.1.3`, `release 0.2.133`.
* Root patches local filesystem paths for fs, ORT, secrets, models, DB, logs, tauri, license, framed-sidecar, OCR, HTTP, PDF, playback, media, audio, process, ASR, VAD, transcript, browser, sync, settings, crash. This is a **declaration override**, not published integration.
* Cargo uses: desktop `framed-sidecar 0.2.0`, `process/fs 0.3.4`, `sync 0.1.1`, `ort 0.2.3`, `logs 0.2.2`, `license 0.1.3`, `http 0.3.0`, `tauri 0.1.3`, `playback 0.2.0`, `media 0.2.6`, `secrets 0.1.3`, `settings 0.2.0`; engine adds ASR `0.2.0`, PDF `0.2.1`; store DB `0.4.0`; model-store `0.2.9`; OCR `0.2.1`; audio `0.2.1/0.2.6/0.3.4`; speech ASR `0.2.0`; protocol settings `0.2.0`.
* Source directly uses framed protocol, owned child, safe FS, sync, ORT, logs, license/secrets/tauri, HTTP, playback/media, settings, ASR, audio, model suite, OCR, PDF, DB, & iOS License/Purchases/CloudSync/Store/ImportQueue/Artifacts/Diagnostics/AppleCredentials. iOS app declares those eight Swift products; Share declares AppleCredentials/ImportQueue.

**Remaining shared-primitive duplication:** local `net.rs` repeats URL/IP parsing & adds product deny ranges around `rightkit-http`; retain extra deny ranges only, remove compatibility parser once owner offers needed hook. Local ASR window/whisper output mapping, model legacy-path compatibility, sync codec/record mapping, CloudKit zone schema, sidecar queue/extraction routing, transcript policy, & product adapter state are product-specific. `ModelInstallStore.swift` explicitly remains a local layout/group bridge pending RightKitSwift extraction; `SendEngine.swift` names RightKitSend but project YAML does not declare `RightKitSend`, so this is not linked adoption.

**Runtime qualification:** `rightkit-sync` is continuous sync; sidecar supervision/log worker may also run continuously. No installed sync/status budget receipt was found. Treat all as **declared/integrated, not measured at scale**.

### Ordered handoff

1. Publish every exact patched Rust owner required by lockfile; delete `[patch.crates-io]`; acceptance: clean clone outside `/Volumes/D/claude`, `cargo metadata` has no `path+file`, sidecar & desktop resolve identical released versions.
2. Consolidate network compatibility parser into owner hook plus ScrapeRight deny-range policy; acceptance: public URL redirects accepted, loopback/private/legacy-IP/NAT64 denied, proxy behavior retained.
3. Preserve extraction policy adapters while upgrading ASR/audio/OCR/PDF/model/media/process; acceptance: sidecar JSON framing, child cancellation/reap, verified model download/rollback, OCR native/ONNX fallback, ASR transcript/SRT/VTT parity, PDF/article/media extraction.
4. Build iOS + Share & macOS credential helper; assert compiled RightKitSwift products, app groups/CloudKit/keychain/privacy manifests, share queue delivery, sync migration, entitlement/purchase parity.
5. Run installed continuous-sync/sidecar scale test & expose sync/worker status with required budget figures.

## Completion gate for any handoff

1. RightKit owner version is published; app manifest uses exact registry/npm/remote URL tag only; no Git, workspace, `file:`, or parent-path Cargo overrides.
2. App-specific adapter states policy only; shared primitive implementation is removed or reduced to upstream extension point.
3. Focused source/target acceptance passes for desktop, helper, extension, iOS/watch where present.
4. For each continuous owner, README declaration, repeatable crate measurement, host `status()` visibility, & installed scale receipt exist. Record measured figures; do not infer them from pin/source inspection.
