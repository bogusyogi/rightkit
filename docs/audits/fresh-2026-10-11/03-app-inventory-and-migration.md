# 3. Machine app inventory & migration handoffs

**Decision:** remove consumer source overrides first, then migrate proven shared primitives using compatible published versions. Keep app policy local. App-wide “adopted” status requires source integration, portable resolution & applicable installed runtime proof.

Fresh source review, 11 October 2026. Baseline HEAD `f60f116514467e83952f5e5c12a71b34e3341389` plus existing working-tree changes. Earlier audit conclusions were excluded. Each assertion below comes from current manifests, source, local lifecycle records or this run's evidence. No product source was edited or published.

Scope covers 25 npm packages, 57 workspace Rust crates, 10 secondary Cargo packages, root/template npm manifests, 15 RightKitSwift products, CloudKit helper & recording helpers. Publication inventory counts **84 units: 71 public, 12 private, 1 internal helper**; Swift's 15 library products belong to one release unit. App discovery covers 20 current first-party product/tool checkouts, retained Membrane snapshots, archives, workflow roots & 9 installed first-party bundles. These counts describe different populations & must not be added together.

Evidence labels: **source-confirmed** means a bounded source path supports the finding; **candidate** needs execution or compatibility proof; **recorded** means local release ledger, not remote registry confirmation; **integrated** means a source call, not just a manifest entry; **qualified** requires executed target/installed evidence. Existing tests were inspected, not rerun. “Satisfactory” in package rows is a source-review verdict, never a test pass.

The native Legion run remains **incomplete**: 17 of 79 providers completed; packets include generated/cache material & bounded omissions, Blueprint graph is unavailable, & project execution lacks a host network-sandbox receipt. `legion verify` validates report consistency, not release readiness. Its 6,678 raw scanner candidates are not accepted defects. Independent bounded lens results & source reviews supply the findings in these reports.

[Legion Audit SKILL.md](</Users/adrdsouza/.claude/skills/legion/skills/audit/SKILL.md:59>) states: “Project-executing checks need trusted host network-sandbox receipt; without it they are `UNPROVEN`; file-only providers still run.” Its [execution contract](</Users/adrdsouza/.claude/skills/legion/skills/audit/references/execution-contract.md>) requires skipping builds/tests/runtime without that receipt. That is why no new package build, installed-scale benchmark or Apple simulator/device test is claimed here. Both [iOS](</Users/adrdsouza/.claude/skills/legion/skills/ios-development/SKILL.md>) & [macOS](</Users/adrdsouza/.claude/skills/legion/skills/macos-development/SKILL.md>) skills were applied. Apple native catalog/preflight & local tool versions were inspected.

Snapshot receipts: [initial-state.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/initial-state.json>), [current-inventory.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/current-inventory.json>), [source-hashes.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/source-hashes.json>), [source-drift.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/source-drift.json>), [native-summary.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/native-summary.json>). Mechanical source census is not an authoritative symbol graph. Nested fixture/vendor/cache/generated/personal-data boundaries are recorded explicitly; no absence-of-import result proves absence of runtime use.

This was a live review interval, not an immutable checkout. Of 36,395 initially hashed files, 59 changed during review; none disappeared at comparison time. Changes span RightKit, CutRight, ScrapeRight, ViewRight, RustRight & SwiftRight. Local override counts & affected dependency declarations were reread. No unchanged-source or reproducible-build claim applies to concurrently changed files; closure uses the recorded hashes & final chosen release revision.

## Scope & census

Fresh mechanical census found 225 Git roots under inspected development locations. Detailed manifest/reference census spans 32 scoped roots after excluding `.git` metadata as a false root: 619 manifests, 4,882 dependency declarations across apps/RightKit, 6,835 lexical RightKit references & 36,395 source/config hashes. Counts include nested fixtures, forks & duplicate retained snapshots; they are not counts of independent apps or proven integrations.

Primary discovery: `/Volumes/D/claude`, `/Volumes/D/worktrees`, Codex worktrees & relevant home development locations. Shallow expansion covered `/Volumes/D/archive`, `sr0-build`, `rightkit-publish`, `tmp`, plus home `claude`, Hyperframes, Sites, CodeBuddy, WorkBuddy AI, Desktop & Documents. Caches, vendor/third-party implementations, session data & personal content are excluded. [machine-repositories.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/machine-repositories.json>) & [machine-scope-addendum.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/machine-scope-addendum.json>) record identities & exclusions. RightKit publication clones, old HeardRight release clone & SwiftRight staging clones are not separate products. Three Membrane snapshots count as one retained consumer family.

This is a first-party development/adoption inventory. Installed third-party software without owned source is not a RightKit migration target.

## Handoff index

| App / source | Current state | Next bounded work |
|---|---|---|
| [CodeRight](</Volumes/D/claude/coderight>) | Integrated; engine/desktop/context/iOS differ | Secrets/keyring seam, coherent OCR/ORT/FS/watch, iOS target/privacy verification |
| [HeardRight](</Volumes/D/claude/heardright>) | Integrated; native helpers remain local | Service/lease/log/model seams; macOS helper extraction; Apple family acceptance |
| [ViewRight](</Volumes/D/claude/viewright>) | Broad integration | Watcher/worker adapter boundary, published cohort & large-library runtime proof |
| [MailRight](</Volumes/D/claude/mailright>) | Integrated older cohort | DB/SQLCipher, cache, OAuth migration with mailbox-scale proof |
| [ScrapeRight](</Volumes/D/claude/scraperight>) | Integrated through 23 local overrides | M01 first; network/parser & model/group seams; iOS/share/helper verification |
| [CutRight](</Volumes/D/claude/cutright>) | Integrated through 18 overridden owners | M02 first; cache/jobs/service budget & verified media artifacts |
| [GenRight](</Volumes/D/claude/genright>) | Integrated; several ledger-staged pins | Published QTS/ORT/models/jobs/logs set; long-session installed evidence |
| [VoiceRight](</Volumes/D/claude/voiceright>) | Release tooling only | M03: upstream local QTS/direct ORT to compatible published owners |
| [WorkRight](</Volumes/D/claude/workright>) | No RightKit integration found | Candidate DB bootstrap & durable logs; preserve capture/retention/consent policy |
| [Pulse](</Volumes/D/claude/pulse>) | Core search/DB, hub fsindex/UI integrated | Installed full-volume fsindex qualification; platform-specific fallback |
| [SwiftRight](</Volumes/D/claude/swiftright>) | Own SDK/bridge; no RightKitSwift integration | Qualify ABI/packaging; adopt only required Apple products with API parity |
| [Ember / Lightcraft branch](</Volumes/D/claude/ember>) | Shell/UI/QA integrated | Logger seam, compatible published shell/control & installed idle/watchdog metrics |
| [Legion](</Volumes/D/claude/legion>) | Release bridge integrated; other tooling declared | Compatible workflow/release tooling updates; retain native audit engine |
| [RustRight](</Volumes/D/claude/rustright>) | Ownership/fs/process integrated | Coherent registry contracts & lock policy; process runtime proof if watchdog enabled |
| [SellRight](</Volumes/D/claude/sellright>) | Legal + Swift diagnostics | M04 exact Swift pin; canonical release provider; preserve commerce policy |
| [RightSites](</Volumes/D/claude/rightsites>) | Downstream SellRight code | M04 plus separate product identity/provider; coordinate shared changes |
| [Xtool](</Volumes/D/claude/xtool>) | No RightKit integration found | Native helper/Node compatibility mapping; no forced Rust adoption |
| [PostRight](</Volumes/D/claude/postright>) | Broad template integration | Product placeholders, canonical provider, published cohort & installed runtime receipts |
| [LightCraft workflow checkout](</Volumes/D/claude/workflows/lightcraft>) | No RightKit integration found | Separate earlier product checkout from Ember; choose maintained source before migration |
| [SEO tool](</Volumes/D/claude/workflows/seo>) | No RightKit integration found | Independent Python/CLI/MCP owner; no proven compatible RightKit migration obligation |

Manifest/version cells below are evidence snapshots, not instructions to upgrade every old dependency. Prefer compatible recorded releases; verify availability before changing pins. A missing package declaration does not establish a need for that capability. Privacy-manifest observations require checking final archive aggregation & actual API use before adding declarations.

## Fresh apps-core adoption review — 2026-10-11

Scope: current source, manifests, configs, README files, & `AGENTS.md` in CodeRight, HeardRight, ViewRight, MailRight, ScrapeRight; current `rightkit/INDEX.md`, `packages/release/rightkit-versions.json`, & `docs/RUNTIME-BUDGETS.md`. No earlier audit, build, test, install, network call, or product change informed this review.

Status terms: **declared** means manifest pin only; **integrated** means app source calls exported API; **measured at scale** needs an installed-app run with visible figures. A long-running crate is not fully adopted from either declaration or source integration alone.

## Cross-app findings

| Priority | Finding | Evidence | Required resolution |
|---|---|---|
| P1 | ScrapeRight root `[patch.crates-io]` redirects 23 RightKit crates to `/Volumes/D/claude/rightkit/crates/*`. This violates published-package-only adoption. | `scraperight/Cargo.toml:36-59` | Remove all path patches after needed releases exist; lock exact registry versions; prove standalone build with no parent checkout. |
| P1 | No inspected app records an installed, scale-load runtime budget receipt for its continuously running RightKit owners. | Source/manifests only; no app-owned measured status receipt found. `rightkit/docs/RUNTIME-BUDGETS.md` requires declare, measure, expose, then first-consumer scale run. | Add host status plumbing plus recorded installed runs before calling watch/service/sync/cache/logging adoption complete. |
| Alignment | CodeRight, HeardRight, & MailRight retain old 0.1/0.2 Cargo pins while `INDEX.md` has newer source/staged records. Version advancement must wait for corresponding publication. | Exact pins below; `rightkit-versions.json` is current release inventory. | Publish required crates first, then update one owner seam at a time with contract tests. |
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
5. Inspect archive privacy aggregation & add required declarations for actual API/data use; verify app-group/file-provider/widget product linkage, & run device/simulator license + diagnostics acceptance.

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

## Fresh app adoption review — 2026-10-11

## Scope & rule

Read-only source/manifests/config review of first-party desktop, native, mobile/helper & example targets under `cutright`, `genright`, `voiceright`, `workright`, `pulse`, `swiftright`, & `ember`. No build, install, network, product edit, or prior-audit input.

`INDEX.md` owns capability mapping; `rightkit-versions.json` distinguishes staged from published. Published use needs registry/tag evidence; a manifest pin alone is not adoption. Long-running owners need declared, measured, runtime-visible figures & first installed-scale run before adoption: `docs/RUNTIME-BUDGETS.md:7-17`.

| Owner/version evidence | Finding |
|---|---|
| `README.md:29,43-46` | Apps must consume published Rust/npm versions; Swift uses repository URL + exact tag. |
| `INDEX.md:26,30,33,36-37,40,43,45-46,49,53-58,63-75,82-103` | Current owner map & source/published/staged state. |
| `packages/release/rightkit-versions.json:308-358,375-414` | Current staged & registry Cargo pins. `:360-370` records RightKitSwift URL/tag `0.4.0`. |

## Per-app rows

| App / genuine targets | Exact pins & proven use | Gaps / duplicate primitive / app policy | Ordered handoff | Acceptance |
|---|---|---|---|---|
| **CutRight** — Tauri desktop shell, engine, UI E2E | Renderer pins `@rightkit/app-shell 0.5.0`, `legal-ui 0.2.1`, `license 0.1.8`, `logs 0.1.4`, `media-ui 0.3.2`, `onboarding 0.3.2`, `platform-ui 0.2.1`, `settings-ui 0.3.3`, `shell 0.1.0`, `theme 0.2.3`, `updates 0.2.4`: `apps/cutright/package.json:50-61`; imports prove app-shell/media/settings/onboarding use: `src/main.tsx:3-10`, `src/views/SettingsView.tsx:14-16`, `src/views/OnboardingGate.tsx:1-2`. Cargo pins: `asr 0.2.0`, `audio 0.2.1`, `cache 0.2.0`, `control 0.1.11`, `fs 0.3.4`, `jobs 0.2.8`, `llm 0.2.0`, `media 0.2.6`, `models 0.2.7`, `ort 0.2.3`, `playback 0.2.0`, `qa 0.2.14`, `review 0.2.0`, `secrets 0.1.2`, `service 0.1.5`, `settings 0.2.0`, `speech-client 0.3.3`, `transcript 0.3.0`, `vad 0.1.0`: `crates/cutright-{speech,engine,project,qa,ui-e2e,shorts,transcript,captions,audio,reframe}/Cargo.toml:11-40`; callsites prove cache/jobs/media/fs/service/ASR/ORT use: `crates/cutright-engine/src/{thumb_cmd,serve,discovery}.rs:13-18`, `crates/cutright-project/src/{fsutil,lock,probe}.rs:8-12`, `crates/cutright-speech/src/{coreml_tdt,ort_init}.rs:28-48`, `apps/cutright/src-tauri/src/{engine_supervisor,playback_host,lib}.rs:15,15-16,218-255`. | **Blocked published adoption:** root & nested Tauri `[patch.crates-io]` redirect 18 Rust owners to sibling `rk-snapshot` paths: `Cargo.toml:87-106`, `apps/cutright/src-tauri/Cargo.toml:46-65`; violates RightKit source-boundary rule. Multiple pins exceed published records: jobs `0.2.8` vs registry `0.2.6`, cache `0.2.0` vs `0.1.0`, media `0.2.6` vs `0.2.4`, ASR `0.2.0` vs `0.0.2`: `INDEX.md:22,26,40,45`. Cache/jobs/service are continuous owners; no installed runtime figures found. `DevFfmpeg::unverified` remains explicit gap: `crates/cutright-project/src/tools.rs:17-20`. Editorial/timeline/composition semantics remain product policy. | 1. Release owners publish required API-compatible versions. 2. CutRight removes both patch blocks & resolves only published exact pins. 3. Measure cache/jobs/service in installed app, expose status. 4. Replace unverified FFmpeg only when artifact policy is settled. | Lock resolves without paths; launch exercises cache, engine supervisor & playback; installed telemetry records memory/CPU/disk ops/status; product output retains editorial behavior. |
| **GenRight** — Tauri desktop, engine, local-image/QTS helpers, E2E | Renderer pins `app-shell 0.4.1`, `media-ui 0.3.2`, `onboarding 0.3.2`, `platform-ui 0.1.2`, `settings-ui 0.3.2`, `theme 0.2.2`: `package.json:56-76`; imports prove each except theme runtime import not located: `src/components/DesktopWorkspace.tsx:2-3`, `src/views/Settings.tsx:2-3`, `src/views/Onboarding.tsx:2-4`, `src/lib/platform.ts:2-15`, `src/views/Motion.tsx:6`. Cargo pins use db/jobs/secrets/ORT/process/fs: `crates/genright-gen/Cargo.toml:14-25`; models/http: `crates/genright-protocol/Cargo.toml:11-12`; logs/models/jobs/process: `crates/genright-local-image/Cargo.toml:16-20`; QTS/ORT/TTS/model layers: `crates/genright-qts/Cargo.toml:29-39`; settings/process/control: `src-tauri/Cargo.toml:40-43`; media/models/fs: `crates/genright-studio/Cargo.toml:18-20`; QA/browser: `crates/genright-e2e/Cargo.toml:24-33`. Callsites prove settings/process/control/model/log use: `src-tauri/src/{settings,engine_supervisor,lib}.rs:9,11,34`, `crates/genright-{protocol,local-image,gen}/src/{model_store,runtime,store,credentials}.rs:8,45,7-8,27-47`. | Several runtime pins are staged-only: jobs `0.2.7`, logs `0.2.1`, QA `0.2.15`, QTS `0.2.5`, models `0.2.8` vs registry `0.2.6`, `0.2.0`, `0.2.14`, `0.2.4`, `0.2.7`: `rightkit-versions.json:325,328,330-343,386-402`. `rightkit-jobs` & `rightkit-logs` run continuously, yet no installed runtime figures/status proof found. Local image log construction is real use: `crates/genright-local-image/src/runtime.rs:45`. Generation, provider, model-route & creative workflow policy stay GenRight-owned. | 1. Publish compatible staged owners. 2. Re-pin Cargo to published exact records. 3. Run installed long job/model session & record jobs/logs metrics/status. 4. Remove renderer pins with no runtime import only after package-boundary check. | Cargo lock uses registry packages; installed model run proves process cleanup, job recovery, logs budget/status; renderer imports still resolve. |
| **VoiceRight** — Tauri desktop, QTS CLI/embedded helper | Only `@rightkit/release 0.2.109` is pinned: `package.json:49`. Native host directly pins `ort 2.0.0-rc.9` & local `qts` path `../eval/qts-upstream/crates/qts`: `src-tauri/Cargo.toml:10-24`. Embedded code calls upstream `qts::Qwen3TtsEngine`: `src-tauri/src/engine/qts_embedded.rs:4-6,105-107`; CLI helper spawns locally discovered `eval/qts-bin` paths: `qts_cli.rs:23-25,87-100`. | No RightKit runtime adoption. This overlaps `rightkit-ort` owner (`INDEX.md:49`) & `rightkit-qts`/TTS owner (`INDEX.md:56-58,73`), but direct upstream ABI/features & dev artifact paths are concrete blockers. Current RightKit registry records ORT `0.2.2`, QTS `0.2.4`; staged holds `0.2.2`/`0.2.5`: `rightkit-versions.json:334,341,394,400`. Voice prompt format, UX & artifact selection remain product policy. | 1. Define required QTS API/feature parity against current embedded calls. 2. Publish compatible RightKit QTS/ORT set. 3. Replace local path/direct ORT with exact registry pins. 4. Move model/ORT resolution through owner & qualify Windows/macOS. | No `path` QTS or raw `ort` dependency; same synthesis & voice-prompt tests pass through RightKit API; package runs without `eval/` layout. |
| **WorkRight** — agent UI, dashboard, server, capture adapters, local store, session helper | No `@rightkit/*`, `rightkit-*`, or `RightKitSwift` imports/pins under product targets. Agent UI is Vite + Tauri API only: `apps/agent-ui/package.json:6-25`; dashboard is Vite web: `apps/dashboard/package.json:6-26`. Local-store directly owns rusqlite store/WAL/transactions: `crates/local-store/Cargo.toml:14-23`, `src/lib.rs:1-17,156-184,273-290`. | Candidate, not mandated: local generic SQLite bootstrap/persistence overlaps `rightkit-db` owner (`INDEX.md:33`) once SQLCipher/keying/retention requirements can map without changing disclosed-monitoring policy. `tracing_subscriber` remains direct in session helper: `crates/session-helper/src/main.rs:2,28`; assess `rightkit-logs` only if durable diagnostics are product need. Capture consent, policy receipts, event schema, employer control & retention decisions are app-specific; do not substitute shared adoption blindly. No continuous RightKit owner is adopted, so runtime-budget gate has no app result. | 1. Define local-store compatibility matrix against `rightkit-db`. 2. Prototype migration for app-owned policy tables. 3. Decide diagnostics contract. 4. Keep capture/policy boundary product-owned. | Migration preserves WAL, immediate transactions, encrypted-mode behavior & event ordering; no capture scope/consent behavior changes; only then pin published owner. |
| **Pulse** — core CLI, hub Tauri/macOS, macOS salvage prototype, Windows prototype | Core pins `rightkit-search 0.2.0`, optional `rightkit-db 0.3.2`: `core/Cargo.toml:24-32`; uses search in `core/src/storage_browser.rs:12-25` & DB in `core/src/bridge/deliver_codex.rs:42`. Hub pins `rightkit-control 0.1.11` QA-only & macOS `rightkit-fsindex 0.1.1`: `hub/src-tauri/Cargo.toml:19-39`; renderer pins `app-shell 0.5.0`, `platform-ui 0.2.1`: `hub/package.json:11-14`; imports prove UI use: `hub/src/App.tsx:5`, `hub/src/api.ts:2`. fsindex is real: `disk_index.rs:1-19,199-224,337-343,451-452` opens per-volume indexes, records crawl/search metrics, exposes UI command. Mac salvage & Windows prototypes have no RightKit pin: `mac/Package.swift:1-23`, `windows/Cargo.toml:1-35`. | **No current-run proof:** `docs/RUNTIME-BUDGETS.md:3` preserves historical 5.4 GB/65% incident context; it is not measurement from this fresh audit. Pulse survey also preserves historical figures: `docs/file-index-survey.md:9-13`. Hub has logging/status plumbing, yet current plan says real-volume crawl/memory remain unmeasured: `docs/plan.md:56`; no current installed-scale receipt was found. Pin `0.1.1` is staged while registry has `0.1.0`: `rightkit-versions.json:322,384`. Native scan/watch code remains app-specific storage behavior. | 1. Do not call fsindex adopted. 2. Fix fsindex resource budget & publish version. 3. Install hub on intended Mac, record named full-volume run from `scan.log` plus visible `disk_index_status`. 4. Re-pin published version; qualify Windows fallback independently. | Receipt names machine, install/version, roots, entries, RAM/RSS, steady CPU, disk ops/s, crawl/search & status snapshot; figure meets declared ceiling; visible status matches log. |
| **SwiftRight** — Bridge, Services, BridgeSmoke, BridgeGeoModal, SliceApp examples | No RightKitSwift dependency. This is not itself gap: RightKitSwift owns Apple primitives, not required Windows migration. `SwiftRightBridge` has local C targets: `packages/SwiftRightBridge/Package.swift:15-35`; Services has no external dependency: `packages/SwiftRightServices/Package.swift:4-23`. Examples use own sibling paths; SliceApp also uses probe runtime: `examples/BridgeSmoke/Package.swift:13-32`, `examples/BridgeGeoModal/Package.swift:11-34`, `examples/SliceApp/Package.swift:13-30`. Own sibling paths are valid internal package composition. RightKitSwift canonical pin, if needed for Apple capability, is Git URL exact tag `0.4.0`: `rightkit-versions.json:360-370`. | **Packaging/ABI evidence gap, conditional on desired distribution:** Windows implementation needs caller-supplied ComCtl/DPI/GUI linker flags: `examples/SliceApp/Package.swift:4-12`; bridge dynamically loads DLL, validates ABI, uses WinSDK function pointers: `packages/SwiftRightBridge/Sources/SwiftRightBridge/Loader.swift:3-61`; EngineClient directly launches `Process`: `packages/SwiftRightServices/Sources/SwiftRightServices/EngineClient.swift:192-237`. macOS deployment declaration does not itself preclude Windows SwiftPM. Renderer ABI/bootstrap & window UI policy are SwiftRight-specific. | 1. Decide whether each example/package is distributed beyond repository. 2. If yes, qualify Windows SwiftPM toolchain, link flags & ABI tuple. 3. Add RightKitSwift URL + exact `0.4.0` only for needed Apple primitive. 4. Run macOS + Windows ABI/launch smoke for distributed target. | For distributed target, clean checkout resolves package graph & supplies documented link flags; ABI mismatch yields typed error; any selected Apple primitive compiles from exact RightKitSwift tag. |
| **Ember / Lightcraft** — desktop Tauri, CLI, web host | Renderer pins old `app-shell 0.2.1`, `platform-ui 0.1.2`, `shell 0.1.0`; dev pins theme `0.2.1`, QA Chrome `0.1.0`, git `0.2.26`, release `0.2.124`: `package.json:28-46`. Desktop pins `rightkit-shell 0.1.0`, optional control `0.1.9`, QA `0.2.12`: `apps/lightcraft-desktop/Cargo.toml:10-38`; callsites prove shell/menu/host & QA use: `src/menu.rs:5`, `src/native.rs:1004-1054`, `tests/native_qualification.rs:17-20`. | Pins trail registry/source: app-shell registry `0.6.0`, platform-ui `0.2.1`, shell crate registry `0.1.4`, QA registry `0.2.14`, control registry `0.1.11`: `rightkit-versions.json:378,399,407-408,417-420`; `INDEX.md:82,96-103`. Desktop logger duplicates durable rotating log primitive via std fs: `apps/lightcraft/src/logging.rs:1-40,144-156`; assess `rightkit-logs` before further shared diagnostics work. Auto-import worker is app-specific image-library behavior: `crates/desktop-host/src/auto_import.rs:109-130`. `rightkit-shell` is continuous; no installed runtime metrics/status proof. | 1. Inventory shell/control API compatibility. 2. Upgrade only published compatible pins. 3. Decide logger migration against `rightkit-logs`, preserving file format/support workflow. 4. Measure installed shell watchdog/idle behavior. | Desktop, CLI & web keep product parity; shell/QA lock resolves from registry; logger migration preserves rotation/cap; installed shell metrics/status meet budget. |

## Cross-app order

1. **RightKit release owners:** publish required Cargo/npm versions & budgeted continuous owners.
2. **CutRight:** remove local patch source resolution before claiming adoption.
3. **Pulse:** close fsindex scale proof before shipment.
4. **VoiceRight & SwiftRight:** resolve QTS/ORT & Swift ABI/toolchain boundaries before pin changes.
5. **GenRight & Ember:** update published pins, then collect installed runtime receipts.
6. **WorkRight:** make narrow DB/logs decisions only after compatibility mapping.

No package marked adopted here solely from a manifest pin. No source evidence supports product-wide adoption of every available owner.

## Apps other — current adoption review

Scope: current source under assigned paths only; no build, install, network, or product mutation. `INDEX.md` & `packages/release/rightkit-versions.json` supplied availability/pin comparison. `RUNTIME-BUDGETS.md` requires declared, measured, exposed installed-scale evidence for continuous crates; no such run was performed here.

## Result matrix

| product / checkout | state | declared RightKit surface | source integration | pin / boundary result |
| --- | --- | --- | --- | --- |
| Legion | active native CLI | hooks `0.1.2`; ax `0.2.4`; git `0.2.37`; release `0.2.151` | release bridge imports/calls `@rightkit/release`; native release code uses it for archive, SBOM, provenance | hooks & ax equal staged; git `0.2.37` & release `0.2.151` are legacy, behind staged `0.2.39` / `0.2.153`; no local RightKit spec |
| RustRight | active broker | ownership `=0.2.0`; fs `0.3.2`; process `0.3.1` | broker admission uses ownership; state/install use fs; process wrapper uses process | all real integrations; ownership behind staged `0.2.1`; fs/process are loose & behind staged `0.3.3`; no local RightKit dependency |
| SellRight | active commerce app | legal `0.3.1`; RightKitSwift `from: 0.1.0` | API validates LegalManifest; iOS target depends on RightKitDiagnostics | legal equals staged; Swift uses a range rather than a compatible exact tag; canonical record is `https://github.com/bogusyogi/rightkit-swift.git@0.4.0`; release config omits required provider |
| RightSites | active SellRight downstream | legal `0.3.1`; same Swift declaration | copied API legal integration & iOS target | same Swift defect; downstream product tree duplicates SellRight identity; `r2-rightsites` differs from invariant provider `r2` |
| Xtool | active headless plugin | none | none found | no RightKit adoption or local RightKit link; local MCP/document/process implementation remains product-owned, so no Index capability migration claim follows from this scan |
| PostRight | template / integration fixture | app-shell `0.5.0`, logs `0.1.4`, platform-ui `0.2.1`, tauri `0.2.0`, updates `0.2.5`, release `0.2.151`; 12 Rust crates | imports app-shell/logs/platform-ui/updates; Tauri & engine use shell/jobs/process/logs/service/db/settings/secrets/http/crash/qa | broad real integration; no local RightKit spec; stale staged pins listed below; `r2-postrights` differs from invariant provider `r2`; release config & updater still contain product placeholders |
| Workflows | operational repository | none; KDP has no RightKit deps | none | not an app. `render/cad_engine` is historical prototype; nested child roots are independently classified below |
| LightCraft | active first-party desktop/photo app | none | none found | own Rust workspace; no RightKit declaration, import, local spec, or adoption claim |
| SEO | active first-party CLI/MCP operational tool | none | none found | own Python/plugin runtime; not desktop app; no RightKit declaration, import, local spec, or adoption claim |
| FreeCAD | upstream dependency fork | none | none found | `upstream` is `FreeCAD/FreeCAD`; `contrib/headless` is workflow prototype, not RightKit consumer |
| build123d | upstream dependency/research fork | none | none found | user-origin fork carries upstream build123d identity/maintainers; not first-party product |
| ScreenRight 2026-10-08 | archived product checkout | release `0.2.66` | release config invokes `right-release` | archive path; legacy release pin; no source import found; no current adoption claim |
| RightSuite / Orthic archives | archived suite shell / retired Hub shell | none | none found | neither is product source, package, add-on, or release lane |
| Membrane (three paths) | one product, three untracked snapshots | root hooks `0.1.0`, ax `0.1.1`, git `0.2.0`; Hub platform-ui `0.1.0`, release `0.2.67` | Blueprint/MCP import hooks; Hub copies installed platform-ui dist into its packaged vendor tree | all pins are legacy vs staged; Hub release config omits required provider; platform-ui integration is real but vendor output is build packaging, not local RightKit source |

## Source anchors & findings

### Legion

- Manifest declarations: `/Volumes/D/claude/legion/package.json:106-111`; permitted legacy set: `pnpm-workspace.yaml:2-5`.
- Runtime release bridge: `engine/xtask/src/rightkit_release_bridge.rs:1-190`; callers: `engine/xtask/src/finalize_macos_candidate.rs:366,539,551` & `prepare_unsigned_candidate.rs:460,549,561`.
- App release config declares `github-releases`: `right-release.config.mjs:141-162`.
- `rightkit-versions.json` staged targets: hooks `0.1.2`, ax `0.2.4`, git `0.2.39`, release `0.2.153`. Current package sources are hooks `0.1.2`, ax `0.2.4`, git `0.2.39`, release `0.2.153`; Index/version file therefore identifies two old consumer pins.

### RustRight

- Declared registry dependencies: `/Volumes/D/claude/rustright/Cargo.toml:22,37-38`.
- Production ownership gate: `crates/rustright-broker/src/job_service/assemble.rs:26-27` & `ownership.rs:10,48,125-129`.
- Shared fs/process calls: `crates/rustright-state/src/fsops.rs:35`; `crates/rustright-process/src/tree.rs:16-19`; `crates/rustright-service/src/cutover.rs:416`.
- `=0.2.0` ownership is exact but not current staged `0.2.1`; fs/process use compatible ranges, allowing resolution drift; review lock policy against recorded `0.3.3`.
- `/Volumes/D/claude/rightkit` appears only in parity discovery/default fixtures (`parity/apps.mac.json:16`, `crates/rustright-parity/src/broker.rs:622`), never as a Cargo source dependency.

### SellRight & RightSites

- Both API manifests declare legal `0.3.1`: `/Volumes/D/claude/sellright/packages/api/package.json:30`; `/Volumes/D/claude/rightsites/packages/api/package.json:31`. Both call `assertLegalManifest`: `sellright/packages/api/src/legal/manifests.ts:19`; `rightsites/packages/api/src/legal/manifests.ts:19`.
- Both iOS project files resolve `RightKitSwift` from `https://github.com/bogusyogi/rightkit-swift` with `from: 0.1.0`, then attach RightKitDiagnostics to target: `sellright/ios/project.yml:17-29`; `rightsites/ios/project.yml:17-29`. This is source integration, yet not required immutable URL + exact tag. Version catalog says `https://github.com/bogusyogi/rightkit-swift.git@0.4.0`.
- RightSites self-identifies as SellRight web-layer fork: `/Volumes/D/claude/rightsites/README.md:3-7`; its root package still names itself `sellright`: `package.json:2-5`; release config calls app `sellright` with `r2-rightsites`: `right-release.config.mjs:3-4`. Treat it as one downstream product branch, not independent SellRight adoption.
- SellRight config has no `distribution` field: `sellright/right-release.config.mjs:1-6`. RightKit `AGENTS.md` requires explicit provider `github-releases` for public products or `r2` for private products; RightSites `r2-rightsites` also differs from that locked provider value.
- Neither product references `RightKitCloudKitHelper`; no private Swift helper compatibility path exists in assigned current source.

### PostRight

- Template status & intended shared contract: `/Volumes/D/claude/postright/README.md:1-21`; runtime adoption table: `README.md:10-19`.
- Frontend imports: `src/App.tsx:2`, `src/lib/logs.ts:1`, `src/lib/platform.ts:2`, `src/lib/updates.ts:1`.
- Cargo pins: `src-tauri/Cargo.toml:28-40`; engine: `crates/engine/Cargo.toml:20-29`; QA: `crates/e2e/Cargo.toml:12-14`. Runtime calls include supervision `src-tauri/src/engine.rs:9-11`, shell host `src-tauri/src/lib.rs:11-13,85`, jobs/db/service `crates/engine/src/main.rs:18`, `channels.rs:9-10`, `runtime.rs:7`.
- Staged drift: app-shell `0.5.0 < 0.6.0`; release `0.2.151 < 0.2.153`; jobs `0.2.6 < 0.2.7`; secrets `0.1.2 < 0.1.3`; shell `0.1.4 < 0.2.0`; qa `0.2.14 < 0.2.15`. Other listed pins equal corresponding staged entries.
- Product completion remains blocked by source-marked placeholders: updater endpoint/key `src-tauri/tauri.conf.json:31-35`, platform scripts `right-release.config.mjs:73-74,104-105`. These are product configuration gaps, not RightKit local-copy findings.
- `right-release.config.mjs:21-22` uses `r2-postrights`, while RightKit's locked provider value for private products is `r2`.

### Xtool, workflows, ScreenRight

- Xtool identifies itself as headless MCP/laptop plugin: `/Volumes/D/claude/xtool/README.md:1-15`; its manifest has no `@rightkit/*`: `package.json:1-37`. No RightKit source import/declaration found.
- Workflows owns orchestration/operational data, while child tool repos retain separate histories: `/Volumes/D/claude/workflows/README.md:1-19`; `render/cad_engine` is explicitly historical: `README.md:46-47`. KDP manifest carries only external dependencies: `kdp/package.json:1-9`.
- Docs-only entries: `workflows/runbooks`, `workflows/brands`, & root operational guidance have no app manifest or executable product target. They are not adoption targets.
- Full workflow-root classification: `brands` docs/assets; `render` orchestration plus historical CAD prototype; `runbooks` docs; `scripts` bootstrap helpers; `seo-ops` historical operator notes; `kdp` private publishing helper; root `workflows` orchestration. None is an application adoption target. Evidence: `workflows/README.md:1-19`, `render/README.md:1-2`, `runbooks/README.md:1-15`, `seo-ops/README.md:1-10`, `kdp/README.md:1-38`.
- LightCraft is first-party application source (`Orthic-Labs/lightcraft` origin): `/Volumes/D/claude/workflows/lightcraft/README.md:10-18`; native app/CLI/web members appear in `Cargo.toml:1-34` & `apps/lightcraft*/Cargo.toml`. Search found no RightKit reference in current source/manifests.
- SEO is first-party independent skill/CLI/local-MCP tool, not desktop product: `/Volumes/D/claude/workflows/seo/README.md:1-20,64-96`; it states independent runtime & no host framework requirement. Search found no RightKit reference in current source/manifests.
- FreeCAD is a dependency/tool fork: origin `bogusyogi/FreeCAD`, upstream `FreeCAD/FreeCAD`; `/Volumes/D/claude/workflows/freecad/README.md:1-8` identifies FreeCAD, while `README.md:3-5` limits local interest to `contrib/headless` prototype. build123d is an upstream Python CAD framework: `/Volumes/D/claude/repos/build123d/README.md:23-35`, `pyproject.toml:10-14`; its user-origin fork does not turn it into first-party app source.
- ScreenRight is scoped only because path is `/Volumes/D/claude/archive/screenright-2026-10-08`; package release pin is `0.2.66`: `package.json:28-33`; config names archived `screenright`: `right-release.config.mjs:35-37`. No `@rightkit/release` import exists in product source; config invokes CLI at `right-release.config.mjs:96-97`.
- `/Volumes/D/claude/archive/retired-2026-10-09/rightsuite` is archived suite catalog shell: `README.md:1-34` links product sites but exposes no product manifest. `/Volumes/D/claude/archive/retired-2026-10-09/orthic` is explicitly retired historical provenance, never launchable product/package/add-on/release lane: `README.md:1-10`. Both have no RightKit manifest or integration.

### Membrane duplicate snapshots

- Paths: `/Volumes/D/worktrees/membrane-main-f602-baseline`, `membrane-blueprint-findings`, `membrane-live-diagnostics-phase1-repair`. Each lacks Git metadata (`git -C <path> rev-parse --is-inside-work-tree` fails), so none is an active checkout for release claims.
- All three have identical SHA-256 manifests: root `package.json` `96218bc8…`, root workspace `bd776b41…`, Hub `package.json` `126b6e1f…`, Hub workspace `1fbe1280…`. They are duplicate product snapshots with differing directory intent, not three consumers.
- Representative declarations: `membrane-main-f602-baseline/package.json:31-35`; Hub: `apps/membrane-hub/package.json:17,22`. Real hook use: `blueprint/src/lib/hooks/blueprint-graph-index.mjs:1`, `mcp/hooks/membrane-hook-runtime.mjs:1`; Hub integration/build copy: `apps/membrane-hub/src/popover.mjs:87`, `scripts/build-frontend.mjs:28-29`.
- `hooks 0.1.0`, ax `0.1.1`, git `0.2.0`, platform-ui `0.1.0`, release `0.2.67` trail staged `0.1.2`, `0.2.4`, `0.2.39`, `0.2.1`, `0.2.153`.
- Hub release config has no `distribution.provider`: `apps/membrane-hub/right-release.config.mjs:18-34`; this misses RightKit's explicit-provider invariant.

## Runtime-scale status

`docs/RUNTIME-BUDGETS.md:5-15` makes declaration, repeatable measurement, runtime exposure, & first-consumer scale record mandatory for continuous crates. Static source proves PostRight runs `rightkit-jobs` supervision, `rightkit-shell` host/watchdog, `rightkit-service`, & `rightkit-logs`; those four are listed continuous classes at `RUNTIME-BUDGETS.md:17-18`. This scope has no installed-app runtime status/measurement record for PostRight, so its adoption is integration-level only, never installed-scale qualified. RustRight & Membrane contain resident services, but their direct shared-crate adoption does not establish any RightKit continuous-crate installed-scale run. No other scoped product declares a shared continuous RightKit runtime.

## Exclusions & local-primitive outcome

- No `file:`, `link:`, `workspace:`, Cargo `path`, or Git dependency points at RightKit in consumer manifests/configs. Product-internal Cargo path dependencies remain internal primitives, not RightKit copies.
- This broad review did not prove additional removable primitive copies. Native migration addendum below identifies Xtool automation as a bridge-dependent candidate. PostRight intentionally owns protocol schemas/tooling around shared components; that is source-local integration glue, not shared primitive replacement.
- Missing from scoped active app adoption: RightKitCloudKitHelper; every unmentioned Index package/crate. Absence means no declaration/source integration was found, never that consumer need exists.

## Census boundary

`machine-repositories.json` current census records every root below. This handoff classifies each so no first-party root is silently treated as third-party; detailed adoption evidence remains limited to assigned paths.

| census roots | classification | handling here |
| --- | --- | --- |
| `/Volumes/D/claude/cutright`, `genright`, `voiceright`, `scraperight`, `mailright`, `workright`, `viewright`, `coderight`, `heardright` | first-party product roots | outside assigned path; separate product-app review owns adoption evidence |
| `/Volumes/D/claude/pulse`, `swiftright` | first-party shared/service roots | outside assigned path; not consumer-app evidence here |
| `/Volumes/D/claude/rightkit` | shared-package source | audit subject, never consumer |
| `/Volumes/D/claude` | workspace shell | orchestration root, never product consumer |
| `repos/coderight/**` external clones; `repos/build123d`; `workflows/freecad` | dependency/research checkouts | excluded; build123d & FreeCAD classification anchored above |
| `archive/screenright-2026-10-08`, `archive/retired-2026-10-09/rightsuite`, `archive/retired-2026-10-09/orthic` | archived | classified above; never active adoption |
| `workflows/lightcraft`, `workflows/seo` | first-party roots | classified above |

## Final shallow machine-scope addendum

Shallow census added `/Volumes/D/{archive,sr0-build,rightkit-publish,tmp}` & `/Users/adrdsouza/{claude,Hyperframes,Sites,CodeBuddy,WorkBuddy AI,Desktop,Documents}`. No additional first-party application source root was found.

- `/Volumes/D/sr0-build/r3-final` (`4496474d…`) & `r3-final-011` (`c946d1d4…`) are SwiftRight staging clones from `bogusyogi/swiftright`; their README calls SwiftRight planned SDK, never released application. Primary source already exists at `/Volumes/D/claude/swiftright`.
- `/Volumes/D/rightkit-publish` holds RightKit publish staging, release artifacts, & duplicate `rightkit` clones (`claude/rightkit`, `w2-w4`, `v1-v4`, `npm`); it is source/release evidence, never consumer-app source. `/Volumes/D/archive/codex-archived-sessions-2026-10-09` is archived session data; `/Volumes/D/tmp` has no shallow candidate root.
- `/Users/adrdsouza/Hyperframes/studio-starter` is private Hyperframes composition project (`package.json:1-10`), not first-party app source. `/Users/adrdsouza/claude/{sellright,tools}` has no shallow Git/manifest identity. Sites, CodeBuddy, WorkBuddy AI, Desktop, & Documents were excluded as personal content, installed-app state, or test artifacts without deep inspection.

Structured scope record: `docs/audits/fresh-2026-10-11/evidence/machine-scope-addendum.json`.

## Fresh native migration review

Scope: source-only review of `/Volumes/D/claude/heardright/tauri-app-next/macos-native/HeardRightPillMac`, `/Volumes/D/claude/xtool/native/macos-desktop-helper` plus its Node bridge, & `/Volumes/D/claude/pulse/mac`. No builds, tests, installs, network, or product edits.

Reference: `swift/RightKitSwift/Package.swift:10-25` declares 15 products. Local adoption ledger records RightKitSwift `0.4.0`; source marker `swift/RightKitSwift/VERSION` is `0.6.0`, not a published consumer version. Any migration requires API qualification, first publish, then exact-tag pin. RightKitSwift owns reusable Apple infrastructure; product protocol, policy, branding, & UI stay local.

## Findings

| Local seam & evidence | Matching owner | Disposition | Handoff |
|---|---|---|---|
| HeardRight credential helper is per-request JSON CLI, with allowlisted accounts/service/access group, scope commands, & Security implementation (`heardright/tauri-app-next/macos-native/HeardRightPillMac/Sources/HeardRightAppleCredentials/main.swift:1-73,165-284`). Package comments say Rust cannot call synchronizable Security APIs (`Package.swift:21-27`). No current RightKit import/call is present. | `RightKitAppleCredentials` has synced/device scopes, status/read/write/delete/copy APIs (`rightkit/swift/RightKitSwift/Sources/RightKitAppleCredentials/RightKitAppleCredentials.swift:8-12,30-38,102-147`); Rust `rightkit-secrets` has matching helper wire calls (`rightkit/crates/rightkit-secrets/src/apple_helper.rs:1-5,17-31,47-65,120-158`). | **Proposed migration.** Qualify needed API, publish version, exact-tag pin, then replace duplicate Security code. Keep HeardRight account allowlist, service, access-group, entitlement, & JSON boundary. | `NATIVE-01`: compare wire/status/error semantics; do not target unpublished `0.6.0`; after qualification/publish, pin exact compatible tag & remove local Security query/write/delete code. |
| HeardRight pill owns AppKit panel, event bridge, animation, splash/hub transitions, clipboard, & product callbacks (`/Volumes/D/claude/heardright/tauri-app-next/macos-native/HeardRightPillMac/Sources/HeardRightPillMac/PillBridge.swift:1-21,81-219`; `PillPanel.swift:6,131,197`). No current RightKit import/call is present. | `RightKitOverlay` owns pill/notch kind, mode/event, rect placement/hit testing, non-activating panel, click-through, & show-without-activation (`/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitOverlay/Overlay.swift:8-19,21-45,81-172,174-230`). | **Proposed migration.** Qualify needed API, publish version, exact-tag pin, then adopt generic model/panel/hit regions; retain `PillBridge`, animations, product callbacks, splash, hub, & C ABI. | `NATIVE-02`: map existing frame/mode/event state; after qualification/publish, pin exact tag & delete only duplicate panel geometry/click-through code. |
| HeardRight CloudSync helper is a CloudKit JSON CLI with Transcript schema, custom zone, token/change baseline, tombstones, aliases, empty-tag rules, entitlement gate, & diagnostics (`/Volumes/D/claude/heardright/tauri-app-next/macos-native/HeardRightPillMac/Sources/HeardRightCloudSync/CloudSyncMain.swift:1-19,21-78,82-178,368-418,468-527,1083-1215`). No current RightKit import/call is present. | `RightKitCloudSync` supplies adapter, transport, retry, direct read/write, tombstone, account-change, coordinator, & CloudKit transport (`/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitCloudSync/SyncAdapter.swift:3-115`; `CloudSyncEngine.swift:31-55,94-209,221-243`; `CloudSyncCoordinator.swift:3-134`). Rust `rightkit-sync` explicitly lists HeardRight desktop migration (`/Volumes/D/claude/rightkit/crates/rightkit-sync/API.md:154`). | **Proposed migration.** Qualify Swift or Rust owner against Transcript contract, publish version, exact-tag pin, then migrate generic sync. Retain Transcript mapping, conflict policy, baseline/system fields, entitlement, scope hash, JSON protocol, & Rust-facing diagnostics until parity is proven. | `NATIVE-03`: compare pull/commit/push response contracts, choose qualified owner, publish/pin it, then delete local sync paths only after parity. |
| HeardRight App Intents helper validates five actions, writes an app-group command, & wakes app through Darwin notification (`/Volumes/D/claude/heardright/tauri-app-next/macos-native/HeardRightPillMac/Sources/HeardRightShortcuts/main.swift:15-72,94-180`). No current RightKit import/call is present. | `RightKitIntents` owns recording action/error, mic permission handshake, queue fallback, & Darwin notification (`/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitIntents/RecordingBridge.swift:7-20,22-69,75-137`). | **Proposed migration.** Qualify needed API, publish version, exact-tag pin, then reuse bridge primitives; retain HeardRight intents, command names, app-group path, cold-start behavior, & menu policy. | `NATIVE-04`: after qualification/publish, wrap `RightKitRecordingBridge` behind existing ShortcutAction/CommandStore; remove only duplicate queue/notification plumbing. |
| HeardRight menu helper is brand/menu lifecycle code (`/Volumes/D/claude/heardright/tauri-app-next/macos-native/HeardRightPillMac/Sources/HeardRightMenuBar/main.swift:1-29`). | No generic RightKit menu product. | **Keep local.** | None. |
| Pulse foreign credential reader enumerates generic-password attributes, selects newest persistent ref, supports no-prompt vs interactive read, retries via `/usr/bin/security`, & decodes provider JSON (`/Volumes/D/claude/pulse/mac/Notch/Sources/Providers/KeychainItem.swift:1-25,26-103`; `ClaudeCredentials.swift:28-93,96-131`). No current RightKit import/call is present. | `ForeignGenericPassword` is explicitly read-only legacy file-Keychain, newest-by-modification, persistent-ref read, no-prompt/allow-prompt (`/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitAppleCredentials/ForeignGenericPassword.swift:7-27,28-97,101-173`). `AppleCredentialStore` is for own access-group items, so it is not a match. | **Proposed migration.** Qualify needed API, publish version, exact-tag pin, then replace generic enumeration/read implementation; retain Claude services, `/usr/bin/security` rescue policy, cache/backoff, JSON decode, status mapping, & Codex file/JWT reader (`CodexCredentials.swift:3-32`). Rust `rightkit-secrets` is not a replacement for foreign app reads. | `NATIVE-05`: verify rescue/error equivalence; after qualification/publish, pin exact tag, migrate `KeychainItem` to `ForeignGenericPassword`, & retain provider policy. |
| Pulse notch is non-activating borderless panel with custom hit testing, drag/menu callbacks, status-bar level, all-spaces/full-screen behavior (`/Volumes/D/claude/pulse/mac/Notch/Sources/Notch/NotchPanel.swift:3-6,24-49,73-94`); controller repeatedly toggles `ignoresMouseEvents` from product interactive rects (`NotchWindowController.swift:1393-1411`). No current RightKit import/call is present. | `RightKitOverlayPanel`, `RightKitOverlayModel`, & `RightKitOverlayHitRegions` cover non-activating panel, placement, hit testing, & click-through (`/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitOverlay/Overlay.swift:118-230`). | **Proposed migration.** Qualify needed API, publish version, exact-tag pin, then adopt panel/model/hit-region primitives; retain notch fleet, hover/fold timers, full-screen detector, display assignment, drag policy, SwiftUI content, & menu callbacks. | `NATIVE-06`: map notch interactive rects to RightKit hit regions; after qualification/publish, delete only duplicate generic panel/click-through code. |
| Pulse hotkeys use Carbon `RegisterEventHotKey`, per-action signatures, user-editable parser, collision reporting, & separate CGEvent tap/Fn remapping (`/Volumes/D/claude/pulse/mac/Notch/Sources/Conveniences/Windows/WindowHotKeys.swift:5-17,78-118`; `EventTapHub.swift:1-123`; `FnCommand.swift:1-17,18-38`). | No RightKitSwift hotkey/event-tap product. `packages/platform-ui` only formats shortcut display; it does not register global keys. | **Keep local.** | No adoption handoff. Future cross-platform hotkey service is **proposed migration** only after owner/API exists. |
| Pulse watchers own Claude profile/session lifecycle, PID/session semantics, JSONL tailing, debounce, & provider state (`/Volumes/D/claude/pulse/mac/Notch/Sources/Sessions/ClaudeSessionMonitor.swift`); Pi monitor uses FSEventStream file events/no-defer & custom latency (`/Volumes/D/claude/pulse/mac/Notch/Sources/Sessions/PiResponseMonitor.swift:39-85`). No current RightKit import/call is present. | `RightKitWatch` is a typed macOS FSEvents `AsyncStream` with IDs, root/item events, latency, flush, stop, & rescan reasons (`/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitWatch/RightKitWatch.swift:3-23,25-68,70-157,201-290`). Rust `rightkit-watch` is a separate notify watcher for Rust hosts. | **Proposed migration.** Qualify needed API, publish version, exact-tag pin, then translate RightKit events into existing session/provider monitors; retain path ownership, JSONL parsing, debounce, PID checks, & provider policy. | `NATIVE-07`: replace raw FSEventStream setup only after qualification/publish & event/order/rescan parity; no wholesale monitor rewrite. |
| Pulse Preferences/UserDefaults, SQLite usage/history, & NearbySharing are product data/policy stores; repository scan shows no existing RightKit command-queue adoption. | `RightKitStore` owns App Group file lock, owner lease, revisioned state, & bounded crash-safe command queue (`rightkit/swift/RightKitSwift/Sources/RightKitStore/RevisionedStateStore.swift:3-37`; `AppGroupCommandQueue.swift:3-18,18-47,51-149`). | **Keep local unless seam is proven.** Use RightKitStore only for a matching cross-process queue/state boundary; it does not replace Preferences migrations or SQLite history. | `NATIVE-08`: inventory one concrete Pulse cross-process queue before adoption; otherwise no change. |
| Pulse `Log` uses app OSLog; monitors and providers attach product categories. | `RightKitDiagnostics` is bounded redacted JSONL persistence (`rightkit/swift/RightKitSwift/Sources/RightKitDiagnostics/JSONLDiagnosticStore.swift:3-27,29-77,77-135`). `@rightkit/logs` is webview structured logging, not Swift OSLog. | **Keep local.** Optional proposed migration only for durable user diagnostics; do not replace OSLog with unrelated web logging. | None until durable diagnostics requirement exists. |
| Pulse process seams include terminal/osascript launches with timeout/kill & PID + start-time liveness (`/Volumes/D/claude/pulse/mac/Notch/Sources/System/TerminalCommand.swift:1-111`; `ProcessLiveness.swift:1-41`). | `rightkit-process` owns processes spawned by a RightKit host; npm `packages/qa/src/process.ts` is QA process control. Neither owns arbitrary terminal policy or existing-PID identity. | **Keep local.** **Proposed migration** only for app-launched helper lifecycle after a host bridge exists; terminal commands, user shell lookup, PID reuse tolerance, & policy remain local. | None now. |
| Xtool Swift helper implements AX tree/window/focus, CGEvent mouse/key input, permission checks, screenshot capture, target validation, & long-lived JSON C ABI (`xtool/native/macos-desktop-helper/main.swift:62-108,110-220,278-529,535-730,734-970,1362-1432,1437-1644`). Node starts/serializes/retire-spawns helper (`xtool/src/main/computer/index.ts:302-314,316-473,675-769`). | `rightkit-automation` is brand-neutral Rust AX/UIA automation for macOS/Windows, with focus, trees, capture, input ladder, kill switch, allow/deny policy, freshness, & receipts (`rightkit/crates/rightkit-automation/README.md:3-34`; `src/lib.rs:1-28`; `src/platform.rs:43-107`). | **Proposed migration.** Rust is a strong host-compatible owner, but only through a Node/Rust bridge preserving serial JSON, exact window/frame generation, screenshot bounds, permission errors, & Windows parity. No RightKitSwift replacement. | `NATIVE-09`: define bridge contract and compare behavior; migrate one operation family at a time. Keep current Swift helper until bridge parity exists. |
| Xtool Node owns helper process spawn/retire/queue; Swift helper has no generic RightKit process wrapper. | `rightkit-process` can own a RightKit-launched child tree; current `rightkit-automation` is explicitly in-process/no daemon (`rightkit/crates/rightkit-automation/README.md:27-34`). | **Proposed migration.** Consider Rust process ownership only with the automation bridge; do not force Node or Swift to adopt unrelated process APIs. | Fold into `NATIVE-09` only if lifecycle semantics are preserved. |

## Handoff order

1. `NATIVE-01` & `NATIVE-05`: qualify credential APIs, publish versions, exact-tag pin, then migrate; retain foreign-item & account policy.
2. `NATIVE-02` & `NATIVE-06`: qualify overlay APIs, publish versions, exact-tag pin, then remove generic panel/hit-region duplicates; preserve product UI & lifecycle.
3. `NATIVE-03`, `NATIVE-04`, & `NATIVE-07`: qualify generic sync, intents, & FSEvents primitives, publish/pin owners, then migrate behind adapters.
4. `NATIVE-09`: evaluate Xtool Rust automation bridge as proposed migration; keep current host until contract evidence exists.
5. Keep hotkey, menu, product stores, OSLog, terminal policy, and arbitrary-PID liveness local until RightKit exposes matching owners.

Next bounded work follows `NATIVE-01` through `NATIVE-09` in listed order.

## Installed bundle inventory

Bundle metadata only; installed versions may differ from working source.

| Bundle | Version | Build | Identifier |
|---|---|---|---|
| [GenRight.app](</Applications/GenRight.app>) | 0.1.0 | 0.1.0 | `app.orthiclabs.genright` |
| [LightCraft.app](</Applications/LightCraft.app>) | 0.2.1 | 0.2.1 | `ai.storyteller.lightcraft` |
| [ViewRight.app](</Applications/ViewRight.app>) | 0.1.165 | 0.1.165 | `app.viewright` |
| [MailRight.app](</Applications/MailRight.app>) | 0.1.0 | 0.1.0 | `app.mailright` |
| [CutRight.app](</Applications/CutRight.app>) | 0.1.6 | 0.1.6 | `app.orthiclabs.cutright` |
| [HeardRight.app](</Applications/HeardRight.app>) | 0.1.228 | 0.1.228 | `app.heardright.next` |
| [Pulse.app](</Applications/Pulse.app>) | 0.2.0 | 518 | `dev.orthic.pulse` |
| [ScrapeRight.app](</Applications/ScrapeRight.app>) | 0.1.10 | 0.1.10 | `app.scraperight` |
| [CodeRight.app](</Applications/CodeRight.app>) | 0.1.74 | 0.1.74 | `app.coderight` |

Passive process samples are recorded separately. They do not exercise full-volume crawl, active microphone sessions, mailbox scale, slow clients or per-crate isolation, & therefore cannot qualify a runtime budget. Match installed build, workload & public status output before collecting the acceptance receipt.

Samples: [runtime-sample-1.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/runtime-sample-1.json>), [runtime-sample-2.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/runtime-sample-2.json>), [runtime-sample-3.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/runtime-sample-3.json>). Pulse process identity changed between samples 1 & 2; do not infer an optimization trend across that restart. Samples 2/3 show native Pulse PID 34020 at 179.4/51.1 MiB RSS & 41.2/1.4% ps CPU, while hub PID 34240 shows 51.4/42.7 MiB & 0.0/0.1%. This is unscripted whole-process observation, with no per-crate attribution or controlled workload.

## Handoff order

1. **RightKit chat:** D03/P01–P03, Swift source defects & release cohort closure.
2. **ScrapeRight & CutRight chats:** publish required owners, delete every shared-source override, prove standalone resolution.
3. **Pulse, HeardRight, GenRight, ViewRight, MailRight & PostRight chats:** expose runtime status & record relevant installed scale workload after owner qualification.
4. **VoiceRight chat:** replace local QTS/direct ORT through measured API parity.
5. **Apple consumer chats:** exact tagged package linkage, helper extraction decisions, extension/watch/device acceptance & archive privacy/entitlement inspection.
6. **Other product chats:** apply only source-backed compatible migrations in their rows; retain product schemas, UX, consent & business policy.

These handoffs are prepared, not sent. Copy each app section with its related backlog IDs & require an evidence receipt for closure.
