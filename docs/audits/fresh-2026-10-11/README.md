# Fresh RightKit audit — 11 October 2026

**Source audit redone from current code. Publication is not yet qualified.** Both iOS & macOS skills were used; previous audit conclusions were excluded.

| Report | Contents |
|---|---|
| [1. Dependencies & publication](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/01-dependencies-and-publication.md>) | Every package/crate inventory, direct-edge review, removals, release lifecycle & runtime-budget proof |
| [2. Boundaries, duplication & Apple](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/02-boundaries-duplication-and-apple.md>) | All 25 npm/57 Rust/15 Swift product boundaries, decomposition decisions & source defects |
| [3. App inventory & migration](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/03-app-inventory-and-migration.md>) | First-party checkouts, nested targets/helpers, installed apps, actual use & per-chat handoffs |

Two dependency-removal candidates survived independent review: unused settings-ui shell peer/dev edge & template's redundant direct service edge. `windows-core` stays because Windows macro expansion needs it.

Release blockers: 23 ScrapeRight & 18 CutRight local Cargo overrides; source/staged/recorded versions differ; no current public-unit publication receipts; all 11 named runtime owners lack required budget sections. Source findings include Swift Sendable/lifecycle/shared-state/buffering defects, macOS lease labeling & helper protocol validation.

Keep public package boundaries. Reconcile QA ownership documentation, contain deprecated QTS wrapper, tighten resource/state ownership & compare send/filesystem durability before consolidation.

Fresh source review, 11 October 2026. Baseline HEAD `f60f116514467e83952f5e5c12a71b34e3341389` plus existing working-tree changes. Earlier audit conclusions were excluded. Each assertion below comes from current manifests, source, local lifecycle records or this run's evidence. No product source was edited or published.

Scope covers 25 npm packages, 57 workspace Rust crates, 10 secondary Cargo packages, root/template npm manifests, 15 RightKitSwift products, CloudKit helper & recording helpers. Publication inventory counts **84 units: 71 public, 12 private, 1 internal helper**; Swift's 15 library products belong to one release unit. App discovery covers 20 current first-party product/tool checkouts, retained Membrane snapshots, archives, workflow roots & 9 installed first-party bundles. These counts describe different populations & must not be added together.

Evidence labels: **source-confirmed** means a bounded source path supports the finding; **candidate** needs execution or compatibility proof; **recorded** means local release ledger, not remote registry confirmation; **integrated** means a source call, not just a manifest entry; **qualified** requires executed target/installed evidence. Existing tests were inspected, not rerun. “Satisfactory” in package rows is a source-review verdict, never a test pass.

The native Legion run remains **incomplete**: 17 of 79 providers completed; packets include generated/cache material & bounded omissions, Blueprint graph is unavailable, & project execution lacks a host network-sandbox receipt. `legion verify` validates report consistency, not release readiness. Its 6,678 raw scanner candidates are not accepted defects. Independent bounded lens results & source reviews supply the findings in these reports.

[Legion Audit SKILL.md](</Users/adrdsouza/.claude/skills/legion/skills/audit/SKILL.md:59>) states: “Project-executing checks need trusted host network-sandbox receipt; without it they are `UNPROVEN`; file-only providers still run.” Its [execution contract](</Users/adrdsouza/.claude/skills/legion/skills/audit/references/execution-contract.md>) requires skipping builds/tests/runtime without that receipt. That is why no new package build, installed-scale benchmark or Apple simulator/device test is claimed here. Both [iOS](</Users/adrdsouza/.claude/skills/legion/skills/ios-development/SKILL.md>) & [macOS](</Users/adrdsouza/.claude/skills/legion/skills/macos-development/SKILL.md>) skills were applied. Apple native catalog/preflight & local tool versions were inspected.

Snapshot receipts: [initial-state.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/initial-state.json>), [current-inventory.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/current-inventory.json>), [source-hashes.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/source-hashes.json>), [source-drift.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/source-drift.json>), [native-summary.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/native-summary.json>). Mechanical source census is not an authoritative symbol graph. Nested fixture/vendor/cache/generated/personal-data boundaries are recorded explicitly; no absence-of-import result proves absence of runtime use.

This was a live review interval, not an immutable checkout. Of 36,395 initially hashed files, 59 changed during review; none disappeared at comparison time. Changes span RightKit, CutRight, ScrapeRight, ViewRight, RustRight & SwiftRight. Local override counts & affected dependency declarations were reread. No unchanged-source or reproducible-build claim applies to concurrently changed files; closure uses the recorded hashes & final chosen release revision.

## Prioritized handoff backlog

| ID | Priority / status | Owner & finding | Acceptance |
|---|---|---|---|
| D01 | P3 · candidate | [settings-ui](</Volumes/D/claude/rightkit/packages/settings-ui/package.json:33>): Remove unused shell peer, optional-peer metadata & dev dependency | Focused type/test & packed-consumer install; retain emitted type dependencies. |
| D02 | P3 · candidate | [desktop template](</Volumes/D/claude/rightkit/desktop-app-template/src-tauri/Cargo.toml:41>): Remove direct rightkit-service dependency; protocol crate already owns it | Compile template on supported targets, retaining protocol dependency. |
| D03 | P1 · source-confirmed | [npm publication](</Volumes/D/claude/rightkit/packages/app-shell/package.json:40>): Publish platform-ui 0.2.2 before app-shell 0.6.1 peer floor | Coherent registry-confirmed peer set & isolated install. |
| P01 | P1 · qualification gap | [all public units](</Volumes/D/claude/rightkit/packages/release/publication-gate.mjs:34>): Current publication receipts do not qualify this source revision | Exact version/HEAD, clean unit path, host results & artifact hash per release. |
| P02 | P1 · source-confirmed | [release runtime gate](</Volumes/D/claude/rightkit/packages/release/runtime-budgets.mjs:88>): README regex gate does not verify measured budgets or installed adoption | Structured measurement/status/scale artifacts checked by publish & adoption gates. |
| P03 | P1 · qualification gap | [continuous crates](</Volumes/D/claude/rightkit/docs/RUNTIME-BUDGETS.md:7>): All 11 named owners lack required Runtime budget sections; no fresh scale runs | Declare ceilings, measure on representative data, expose status & enforce receipts. |
| S01 | P1 · source-confirmed | [RightKitSpeech](</Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/SpeechEngines.swift:88>): Unchecked Sendable exposes mutable Parakeet vocabulary | Immutable or synchronized vocabulary; concurrent async/blocking stress test. |
| S02 | P1 · source-confirmed | [RightKitSpeech](</Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/LiveDictationSession.swift:145>): Stop during legacy dictation setup can leave resources installed after teardown | Generation-owned setup/cleanup & deterministic stop-during-start test. |
| S03 | P2 · conditional design gap | [RightKitCloudSync](</Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitCloudSync/SyncStateStore.swift:73>): App-group state store has no cross-process transaction ownership | Single-owner lease or locked read-modify-write/merge; two-process test. |
| S04 | P2 · source-confirmed | [RightKitWatch](</Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitWatch/RightKitWatch.swift:181>): Unbounded Swift event stream under a stalled consumer | Bounded buffering with explicit loss/rescan signal & stalled-reader test. |
| S05 | P2 · source-confirmed | [RightKitLicense](</Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseClient.swift:612>): macOS lease activation is labeled ios | Explicit platform mapping & macOS/iOS/iPadOS payload tests. |
| S06 | P2 · forward compatibility gap | [CloudKit helper](</Volumes/D/claude/rightkit/swift/RightKitCloudKitHelper/Sources/rightkit-cloudkit-helper/Session.swift:71>): Request protocol v is emitted but ignored at helper dispatch | Reject missing/unsupported versions before mutation; v0/v1/v2 vectors. |
| F01 | P2 · conditional hardening gap | [rightkit-send](</Volumes/D/claude/rightkit/crates/rightkit-send/src/receive.rs:486>): Deterministic partial path can truncate an existing partial file | Reserve unique partial with create_new, track ownership & preserve preexisting file. |
| A01 | P2 · source-confirmed | [QA ownership](</Volumes/D/claude/rightkit/crates/rightkit-qa/src/lib.rs:1>): Node/browser & native QA split contradicts Rust replacement comment | Document platform/API ownership matrix; keep both runners until parity established. |
| A02 | P3 · compatibility decision | [QTS wrapper](</Volumes/D/claude/rightkit/crates/rightkit-qts-ggml/src/lib.rs:1>): Deprecated qts-ggml wrapper should gain no new API | Check in-tree plus published consumers before retirement; preserve compatibility. |
| A03 | P3 · consolidation candidate | [send/filesystem](</Volumes/D/claude/rightkit/crates/rightkit-send/src/lib.rs:213>): Private trust-store atomic writer overlaps shared filesystem owner | Compare owner permissions, durability, Windows replacement, links & cleanup before delegation. |
| M01 | P1 · source-confirmed | [ScrapeRight](</Volumes/D/claude/scraperight/Cargo.toml:36>): 23 RightKit Cargo overrides resolve local source | Publish needed versions, remove overrides, build standalone consumer. |
| M02 | P1 · source-confirmed | [CutRight](</Volumes/D/claude/cutright/Cargo.toml:87>): Root & nested host override 18 owners to rk-snapshot | Publish needed versions, remove both override blocks, resolve registry lockfile. |
| M03 | P2 · migration required | [VoiceRight](</Volumes/D/claude/voiceright/src-tauri/Cargo.toml:10>): Local upstream QTS & direct ORT have no RightKit runtime integration | QTS API/feature parity, published exact owners, no eval layout dependency. |
| M04 | P2 · source-confirmed | [SellRight/RightSites](</Volumes/D/claude/sellright/ios/project.yml:17>): Swift package uses from range rather than exact version tag | Choose compatible published tag, pin exact URL/version & verify resolution. |
| M05 | P2 · source-confirmed | [app release configs](</Volumes/D/claude/postright/right-release.config.mjs:21>): Missing/noncanonical distribution provider values | Set github-releases or r2 as appropriate; preserve per-product storage config separately. |
| U01 | P2 · source-confirmed | [media-ui](</Volumes/D/claude/rightkit/packages/media-ui/src/image/ImageViewerStage.tsx:102>): Image stage keyboard handlers sit on an unfocusable div | Focusable accessible stage with visible focus & keyboard zoom/pan/fit checks. |

Machine-readable: [findings.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/findings.json>), [handoff-backlog.csv](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/handoff-backlog.csv>), [app-handoffs.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/app-handoffs.json>).

Start with release evidence & runtime gates, then move ScrapeRight/CutRight onto published owners using prepared handoffs.
