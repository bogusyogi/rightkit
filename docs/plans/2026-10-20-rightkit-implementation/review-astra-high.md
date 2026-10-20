# Astra high independent packet review

Current plan verdict: **ready** after final FH01–FH39 recheck on 2026-10-20. AH01–AH05 remain resolved; subsequent canvas correction & final manifest/switch-inventory corrections are accepted. Earlier findings & ready verdict remain history below; current assurance binds only the final FH hash manifest in the last section.

> “my goal is all apps on shared packages and minimal to no separate code, unless absolutely necessary with proper evidence as to why that specific app needs that code and not the others.”

Reviewed all seven implementation packets, README, signed adjudication, applicable K01–K16 contracts & archived row structure. Source review sampled production paths across all six apps & shared owners. Prior mutual-review register was read only after independent first-pass findings; neither prior agreement nor concurrent reviewer output supplied assurance.

This is plan-only review: no builds, tests, installs, publication, app migration or runtime qualification performed. Only this report was written. All seven primary checkouts contained unrelated dirty entries; source observations concern inspected working-tree bytes.

## Blocking findings

### AH01 · P1 · Whole-PK readiness prevents promised incremental adoption

**Packet:** [01-packages.md:453](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:453), lines 453–454; [PK14-6](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:330).

`contracts.PKnn.status` becomes satisfied only when **every** PK task finishes; AP readiness requires every referenced PK satisfied. Consequently, explicitly permitted intermediate releases cannot unblock their named consumers. Example: qualified credentials/helper-trust release PK03-1 cannot unblock AP-HR-03 until unrelated full DB relocation/recovery work PK03-3..6 completes. PK14 additionally contains #821 work requiring separate dispatch: leaving PK14-6 deferred prevents every PK14-dependent app task becoming ready, even after its required QA/mode owners qualify. Schema has no sub-contract, capability slice or deferred/nonblocking task state.

**Fresh evidence:** package lines 85, 111, 126, 144 & 216 explicitly permit independently useful releases. AP-HR-03 requires protected credential storage only ([03-heardright.md:27](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/03-heardright.md:27)); AP-SR-15 explicitly waits only for qualified child-process APIs ([06-scraperight.md:106](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/06-scraperight.md:106)). Signed spec requires per-capability qualification/publication ([adjudication:166](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-spec-adjudication.md:166)); original incremental sequence rejects unrelated-feature dependencies ([Astra r1:419](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-m1-m22-package-spec-astra-r1.md:419)). These are contradictory readiness rules, independently of implementation quality.

**Smallest correction:** retain PK01–PK17 as rollups; derive AP readiness from explicit satisfied capability/subtask requirements tied to exact package version, features & target receipts. Mark separately dispatched #821 outside mandatory completion for this delivery. Add worked manifest cases showing helper-trust adoption while DB relocation remains pending, plus QA/mode adoption while #821 remains deferred. Reconcile AP prerequisite maps with those capability IDs.

**Affected:** PK01/02/03/05/06/07/11/12/14/17 intermediate releases; directly AP-HR-03, AP-SR-15, AP-PU-14, AP-HR-13/16/18, AP-VR-11, AP-MR-10, AP-SR-13, AP-CR-11. Other AP tasks inherit same coarse gating.

### AH02 · P1 · Enforcement activation depends on its own completed contract

**Packet:** [01-packages.md:367](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:367), lines 367, 372 & 453–454.

PK17-3 requires signed §6 snapshot with owner contracts satisfied before minimum raises/ownerPending removal can land. M23 owners include `@rightkit/release`, whose owner contract is PK17. Yet PK17 cannot become satisfied until PK17-3 itself finishes. Current schema therefore requires `PK17 satisfied → PK17-3 activation → PK17 satisfied`. A published scanner alone leaves PK17 partial; marking it satisfied early violates §6.

**Fresh evidence:** signed adjudication assigns M23 to release + daemon ([adjudication:159](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-spec-adjudication.md:159)) & requires enforcement update only against qualified owners ([adjudication:167](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-spec-adjudication.md:167)). It does not require completed enforcement activation as proof of owner implementation. Package §6 exposes only PK contract statuses, so “owner contracts” cannot currently resolve to independently satisfied scanner/mode capability records.

**Smallest correction:** separate owner implementation qualification/publication, authenticated enforcement activation & app adoption. Activation consumes satisfied scanner/mode capability receipts; activation state must not gate those same receipts. Record trust-sync evidence separately without manufacturing prior PK17 completion. Show bootstrap sequence from existing policy through qualified scanner/mode release to authenticated activation.

**Affected:** PK17-3/7, PK07/14 owner receipts; AP-PU-13/14/17, AP-HR-12/13/16, AP-VR-11, AP-MR-10, AP-SR-13, AP-CR-11.

### AH03 · P2 · Existing satisfying releases lack explicit handoff ingestion path

**Packet:** [01-packages.md:404](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:404), lines 404–407, 421–435 & 453; R2 at line 40.

Handoff snapshots are emitted per publication event, with publisher-generated source/version/provenance fields & mandatory packed-before-upload/fetched-after-upload consumers. Packet explicitly keeps device 0.3.1 without republishing & permits existing satisfying dependencies, but provides no qualification-only event or existing-release receipt import path. Executors cannot unambiguously represent “fetch existing release, qualify exact bytes, satisfy capability” without pretending this run published it, inventing historical publisher evidence or unnecessarily publishing another version. Unpublished-source qualification cannot stand in for existing archive qualification.

**Fresh evidence:** PK05-4 explicitly says device receives no republish ([01-packages.md:175](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:175)). README execution step 3 exempts satisfying existing releases from gratuitous republish ([README.md:27](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/README.md:27)). Signed adjudication already contains verified archive receipts for QA/legal-ui/onboarding ([adjudication:21](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-spec-adjudication.md:21)) & permits reuse of valid evidence at line 43. §6 currently defines only newly published provenance semantics.

**Smallest correction:** permit immutable `qualify-existing`/`import-verified-release` events. Bind fetched archive/tag integrity, verified source identity where available, target/feature evidence & standalone fetched consumer. Distinguish original publisher provenance from current qualifier; allow pre-upload check to be not-applicable for unchanged existing bytes with explicit reason. Demonstrate device 0.3.1 entering readiness without republishing.

**Affected:** §6/PK17-7; PK05 device, PK06 sidecar, PK09 unchanged platform/theme, PK10 unchanged legal-ui & any reused satisfying release; every app's pin/readiness step.

### AH04 · P1 · HeardRight iOS has live shared-workflow residue but no scoped task

**Packet:** [03-heardright.md:39](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/03-heardright.md:39), AP-HR-15; [01-packages.md:353](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:353), PK16 named consumers & tasks at 357–362.

HeardRight packet mentions `../ios-native/project.yml` only in dependency preflight. AP-HR-15 scopes work to macOS pill/CloudKit helpers; PK16 likewise names HeardRight helpers while explicitly covering ScrapeRight & CodeRight iOS. None of AP-HR-01..18 assigns HeardRight iOS account/licence/Pro-bridge workflows, retained-function assessment or native acceptance. Desktop tasks name Rust/Tauri callers, so finishing them leaves these existing native consumers unaccounted for.

**Fresh production evidence:**

- [ios-native/project.yml:17](D:/Claude/heardright/ios-native/project.yml:17) binds RightKitSwift **0.8.0**, with License/Purchases/CloudSync/Store/Credentials products at lines 131–156 & additional native targets below. This is an existing consumer, not proposed platform expansion.
- [HeardRightApp.swift:43](D:/Claude/heardright/ios-native/HeardRight/HeardRightApp.swift:43) constructs `LicenseController` & `ProEntitlementResolver`; [ContentView.swift:18](D:/Claude/heardright/ios-native/HeardRight/ContentView.swift:18) consumes them.
- [LicenseController.swift:67](D:/Claude/heardright/ios-native/HeardRight/LicenseController.swift:67) owns refresh-generation coordination; lines 103–161 own deferred deactivation/lease-removal retry. Its lower-level adapter already delegates activation/lease operations to RightKit at lines 193–215. Assess shared orchestration separately from product copy/policy; deleting or rewriting whole adapter would discard valid adoption.
- [ProEntitlementResolver.swift:316](D:/Claude/heardright/ios-native/HeardRight/ProEntitlementResolver.swift:316) owns `ProBridgeClient` POST/decode mechanics called by live resolver; [AccountSessionStore.swift:196](D:/Claude/heardright/ios-native/HeardRight/AccountSessionStore.swift:196) owns native session renewal/device transport, with generic send logic at 278. Shared candidate [RightKitAccount/SessionStore.swift:74](D:/Claude/rightkit/swift/RightKitSwift/Sources/RightKitAccount/SessionStore.swift:74) already supplies account-session owner for comparison.

Signed spec requires native shared parity & one shared implementation per runtime where consumers exist ([adjudication:38](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-spec-adjudication.md:38), [adjudication:97](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-spec-adjudication.md:97)). Existing Swift import is insufficient evidence for retaining local orchestration.

**Smallest correction:** extend AP-HR-15 or add scoped native adoption task covering current iOS consumer products, named callers above, protected storage/account scope, refresh/signout/lease settlement & bridge parity. Add corresponding PK16 work/qualification requirements where current shared owner lacks behavior. Preserve schemas, CloudKit/StoreKit identities, consent, device-only credential policy & domain actions. Include existing native tests/device evidence targets; no new mandatory Apple profile or stack conversion.

**Affected:** PK16, AP-HR-15 plus native counterparts of AP-HR-03/04/05; manifest AP inventory/readiness.

## Advisory

### AH05 · P2 advisory · Scope workspace check so unrelated candidates do not become release prerequisites

**Packet:** [01-packages.md:39](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:39), repeated at 466.

R1 adds workspace `cargo check` to every crate's prepublication gate, while R4 says only affected failures block. [Cargo.toml:3](D:/Claude/rightkit/Cargo.toml:3) includes `crates/*` & has no restricted default-members list. Thus this instruction encompasses unrelated staged candidates. Prior review RV13 says blanket workspace gates were removed; current text retains one.

Clarify invocation scope & failure attribution: run required per-package/consumer feature checks for publication; keep any mandatory workspace compatibility result separately, blocking a candidate only when failure belongs to its dependency/consumer impact. This review found no current build failure & makes no build-health claim. Affected: R1/R4 & every cargo PK.

## Coverage & conclusions supported by source

| Packet | Reviewed production/spec boundaries | Result |
|---|---|---|
| 01 Packages | 17 PKs, optional Cargo dependency direction, DB protected-store adapter, publication/readback/feature evidence, §6 readiness | AH01–03 block; AH05 clarifies gate scope |
| 02 Pulse | Resident attach/generation flow, shared dispatcher, root cutover, owned budget DB vs foreign Codex DB, signed approval contract | Retention boundaries are appropriately narrow; affected by handoff defects |
| 03 HeardRight | Key rotation/staging recovery, selectors, API-key write-only path, shared watchdog, root/model convergence, native iOS consumers | AH04 blocks complete native coverage |
| 04 ViewRight | Existing AtomicWriter delegation & local keyed lock, native PDF/preview/COM ownership, plain metadata DB, iframe inspector exception | No new source-backed blocker; preserve single-file save semantics |
| 05 MailRight | Effect-before-audit order in `agent/gate.rs`, pending settlement, adopted PrivateChild, DB/SqlSettings boundaries | Packet correctly distinguishes audit failure from absent effect |
| 06 ScrapeRight | Existing FileBackend delegation, save/snapshot flow, EngineSupervisor snapshot calls, cookie-artifact/native boundaries, iOS task | Required orchestration remains shared owner work; affected by handoff defects |
| 07 CodeRight | Daemon facade vs implementation, owned/attached client lifecycle, existing shared DB/read pool, token migration & native iOS task | Correct implementation targets & bounded retention rules; affected by handoff defects |

All six app packets explicitly enumerate M1–M23. Missing functionality remains required owner work under signed spec; this review does not treat provisional API names, legitimate domain schemas/actions, delegating wrappers or ViewRight's explicit inspector exception as defects. Critical DB/key cleanup-pending, pipe EOF, process provenance, caller-before-effect, approval identity/audit & release-control requirements are retained. Source/spec review cannot certify their future implementation.

## Reviewed input identity

All seven packet SHA-256 hashes matched [frozen input manifest](D:/scratch/rk-packet-review-20261020/review-input-hashes.json) before review. Signed adjudication independently hashed to `f01330aed8e63a74ee290e31ad1c4c728dd8b3f8414bea4bea17dd4855701a49`; README independently hashed to `d07015e4e2dcac61f610299fbf76622cabc7fa2ad408cba50d41f833287e0afd`. Findings refer to these frozen inputs, with source evidence read from current primary trees. No concurrent independent report was read.

Resolve AH01–AH04, then re-review changed readiness examples & HeardRight native task against unchanged signed objective.

## Final focused recheck — 2026-10-20

**Verdict: ready.** No remaining concrete blocker found within corrected plan scope. This recheck covers all seven packets: changed package rules, capability/readiness model, all six authoritative AP prerequisite tables & expanded HeardRight native caller closure. It does not repeat a full source audit. Original source findings above remain evidence for why these corrections were necessary; their line numbers refer to original inputs.

All seven current packet hashes were independently verified against [final frozen hashes](D:/scratch/rk-packet-review-20261020/review-final-hashes.json). Package packet SHA-256 is `bf0bb82244540d02bd5344f9ab7176877d8e324ffd220fb0045e0350d37ab497`; signed adjudication remains `f01330aed8e63a74ee290e31ad1c4c728dd8b3f8414bea4bea17dd4855701a49`. Final manifest covers 65 capability slices & 89 AP tasks. No concurrent independent report or log was read.

| Finding | Final disposition & evidence |
|---|---|
| **AH01** | **Resolved.** [01-packages.md:414](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:414) defines independent capability receipts, including existing atomic/SQLite/settings/protected-store baselines. [Rules at 489–493](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:489) make PK rollups reporting-only, require source-backed disposition of every conditional prerequisite before readiness & distinguish owner readiness from app completion. Separately dispatched fixture capture does not gate QA/mode adoption. All six AP tables now name required slices; no whole-PK or activation prerequisite was reintroduced there. |
| **AH02** | **Resolved.** [01-packages.md:373](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:373) separates report-mode scanner implementation/qualification/publication from enforcement activation. Activation consumes scanner, mode-root & atomic-baseline receipts; those receipts never depend on activation. Gate rows apply to their owning release. Authenticated trust-sync remains required for enforcement changes. |
| **AH03** | **Resolved.** [01-packages.md:407](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:407) defines append-only `publish` & `qualify-existing` events. Existing releases retain actual original-publication provenance, separate current qualifier identity, nullable source commit with reason & explicit packed-consumer N/A; fetched-byte consumer evidence remains required. [PK17-6 at 378](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:378) requires future executable fixtures for incremental helper/QA adoption, existing device release & N/A behavior. These are planned checks, not claimed receipts. |
| **AH04** | **Resolved.** [PK16-4 at 358](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:358) makes shared HeardRight iOS licence/account/ProBridge gaps mandatory. [03-heardright.md:97](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/03-heardright.md:97) identifies active targets, exact native callers, production rewiring/deletion boundaries & native regression coverage. Existing LicenseActivationClient delegation is preserved; generic lifecycle/transport cannot remain under a blanket native exemption. Product entitlement policy, StoreKit/CloudKit schemas & device-only credential policy remain app-owned. No unrelated Rust publication gates native adoption. |
| **AH05** | **Resolved.** [R1 at 39](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:39) qualifies affected dependency/consumer closure. [R10 at 48](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:48) separately preserves standing brokered local checks before a coding-batch push; unrelated failures can block that push without invalidating independent package qualification. |

Cross-packet prerequisite recheck: [Pulse](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/02-pulse.md:102), [HeardRight](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/03-heardright.md:118), [ViewRight](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/04-viewright.md:84), [MailRight](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/05-mailright.md:73), [ScrapeRight](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/06-scraperight.md:109) & [CodeRight](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/07-coderight.md:103). Conditional requirements cannot silently disappear into an empty ready list; known preflight-only tasks may become ready after manifest verification. Source-backed absent capabilities do not force unrelated surface/PDF/toolbox/crypto work. Existing qualifying releases can be adopted without artificial republishing; new candidates still require relevant package/feature/host evidence.

This remains **plan-only assurance**: no implementation, test execution, qualification, publication or migration success is claimed. Only this report was updated. Corrected plans are ready for implementation handoff against the quoted shared-ownership objective.

## Final FH01–FH39 review — 2026-10-20

**Current verdict: ready.** Reviewed all seven corrected packets against the same objective: “my goal is all apps on shared packages and minimal to no separate code, unless absolutely necessary with proper evidence as to why that specific app needs that code and not the others.” Read Fable's original findings & reconciliation, then independently checked changed requirements, relevant source anchors & readiness rules. Prior agreement was not treated as proof. This is focused plan assurance, not a fresh exhaustive repository audit or implementation qualification.

### Current input identity & coverage

All seven SHA-256 values match [final FH input manifest](D:/scratch/rk-packet-review-20261020/review-fh-final-hashes.json). `01-packages.md` is `495db109dd7299c90e524e9ca96ef9401adcae9a429bc6e579a0338b8152d982`. Signed adjudication remains `f01330aed8e63a74ee290e31ad1c4c728dd8b3f8414bea4bea17dd4855701a49`. Independently counted 98 unique task declarations: Pulse 19, HeardRight 21, ViewRight 14, MailRight 12, ScrapeRight 17 & CodeRight 15; each app has all 23 M rows. Package manifest defines 66 capability slices.

Two defects found during this pass were corrected before final freeze:

- **AH06 / FH03 inventory completeness — resolved:** package rule previously enumerated obsolete 89-task ranges. [01:498](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:498) now derives exact AP set from frozen packets & rejects omissions, extra IDs & duplicates. New entitlement, process & ledger tasks cannot silently fall outside handoff validation.
- **AH07 / FH18 desktop switch creation — resolved:** earlier CodeRight wording only inspected nonexistent `switches.json`, despite reconciliation claiming creation. Filesystem check confirmed desktop file absent. [07:98](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/07-coderight.md:98) now explicitly creates `apps/coderight-tauri/switches.json` from full selector inventory before scanning, preserving separate iOS inventory. Misplaced duplicate AP-CR-15 map row was removed.

### All Fable finding dispositions

“Accepted” below means corrected plan requirement is accepted; it never means implementation or qualification passed.

| Finding IDs | Disposition & checked correction |
|---|---|
| FH01, FH09 | **Already resolved, retained.** Capability receipts replace whole-PK readiness; unchanged consumed pins receive honest `qualify-existing` events ([01:419–443,499–503](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:419)). |
| FH02 | **Accepted.** AP-HR-19 independently uses published entitlement baseline; account work & QA selectors cannot gate it ([03:43,147](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/03-heardright.md:43)). Fresh source confirms env fallback at `heardright-text-pipeline/src/settings.rs:318–327`; source inspection does not prove shipped exposure. |
| FH03 | **Accepted.** All six ledgers have reconciliation tasks. Pulse/HeardRight exclude self/overall signoff explicitly ([02:141](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/02-pulse.md:141), [03:148](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/03-heardright.md:148)); View/Mail ledgers precede final closure. Stale kept entries cannot authorize generic-code retention. |
| FH04 | **Accepted.** PK05-7 owns breaking `@rightkit/tauri` delta, tests, peer compatibility & consumer-backed dialog decision ([01:176](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:176)). |
| FH05 | **Accepted.** One app-neutral release consumer suffices; product payload/configuration stays local ([01:386](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:386)). |
| FH06 | **Accepted.** AP-CR-03 now covers server bind/auth/discovery/startup as well as client lifecycle ([07:55](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/07-coderight.md:55)). Fresh source confirms TCP bind at `main_runtime_server.rs:268`, discovery write at 1343 & `auth.rs::require_token` at 78. Existing HTTP/SSE/bearer wire remains required. |
| FH07 | **Accepted with narrower bootstrap.** Script copies actual evidence; separate snapshot-producer provenance does not impersonate publisher. Same validation precedes readiness; later emitter compares immutable original snapshots & appends corrections instead of rewriting history ([01:417](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:417)). |
| FH08 | **Accepted narrowly.** New renderer licence-client adoption requires qualified release tooling; native-only work does not inherit this extra prerequisite ([01:175](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:175) & six AP maps). |
| FH10, FH11, FH38 | **Accepted.** PK17-4 owns missing wv2-0_39, logs/engine & crash/window-errors rows; unrelated pdfium edits remain outside scope. Each row gates owning release, not unrelated scanner receipts ([01:381,443](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:381)). |
| FH12 | **Accepted.** Published QA 0.7.1 is distinct from same-number source additions; named-consumer decisions & fresh version precede publication ([01:323–328](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:323)). Fresh source confirms `dialog`/`fake_cli` exports under Cargo 0.7.1. |
| FH13, FH15, FH31 | **Accepted.** Corrected engine/process/native anchors identify actual symbols. Sampled Pulse `HubSettingsStore`, `hubServices`, `ResidentEngineClient` & HeardRight credential helpers exist at cited paths. Exact caller searches remain required before deletion; no blanket whole-file migration follows from these anchors. |
| FH14 | **Accepted, narrowed consumer scope.** Pulse lock callers replace unsupported HeardRight assumption. OS-auth needs its own backend/native receipt; protected-storage receipt is insufficient ([01:235,434](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:235)). |
| FH16, FH34 | **Accepted.** ScrapeRight store & ViewRight diagnostics use plain SQLite baseline/root ownership, without inferred encrypted lifecycle. Fresh ScrapeRight `crates/store/Cargo.toml:9` & `src/lib.rs:177` confirm DB consumer ([06:123](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/06-scraperight.md:123)). |
| FH17 | **Accepted.** View/Mail reference existing workspace setup script by absolute path; filesystem existence checked. |
| FH18 | **Accepted after AH07 correction above.** Desktop switch creation & M22 applicability reconciliation are explicit; no new `desktop.*` authority. |
| FH19 | **Accepted with evidence condition.** Source ledger declares SSH deletion; packet repeats whole-repository/public-API search before removing modules. No unnecessary SSH migration ([07:55](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/07-coderight.md:55)). |
| FH20, FH39 | **Accepted.** Four apps now name production process sites. Sampled Pulse pass-through & HeardRight PID-wait/relaunch confirm distinct semantics that replacements must preserve. Typed relaunch replaces shell-text mechanics; task text requires per-operation ownership/stdio/exit parity, not one universal launch mode. |
| FH21 | **Accepted.** AP-PU-15 explicitly names DeviceLinkHost pairing/session orchestration for shared service/approval owners; domain actions remain local ([02:39](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/02-pulse.md:39)). |
| FH22, FH25 | **Accepted/narrowed.** Actual root/approval slices govern Pulse; conditional extra slices need caller proof. View/Mail AP01 is identity preflight plus incremental pinning, not completed whole-app pins before any task. |
| FH23, FH28 | **Accepted.** PDF owner precedes optional shell host; DB's FS dependency is required. Cycle guards remain intact ([01:57,75,85](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:57)). |
| FH24, FH29, FH30 | **Accepted.** Shared shell coordination index includes PK04/06/08/10/11/13; undefined A2 removed; PK17-8/9 ordering corrected. |
| FH26 | **Accepted without redirect assumption.** Current pins, canonical URL, correct AP IDs, exact tag resolution & readback ledger updates are explicit ([01:355](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:355)). |
| FH27 | **Accepted.** OAuth source delta is explicitly held unless named consumer justifies scoped qualification; no mass unrelated publish ([01:35](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/01-packages.md:35)). |
| FH32, FH33 | **Accepted.** View's actual shortcut dispatcher is a deletion target; ScrapeRight adopts shared watchdog without claiming an existing callback. |
| FH35 | **Partly accepted; automatic deletion rejected.** Intended canvas schema/actions stay unwired; generic host/store mechanics migrate where separable, with any inseparable slice explicitly pending. No endpoint registration, feature activation or canvas installed-journey claim ([07:94,101](D:/Claude/rightkit/docs/plans/2026-10-20-rightkit-implementation/07-coderight.md:94)). |
| FH36, FH37 | **Accepted.** Cookie artifact uses PK12 + PK01; HeardRight watchdog uses PK11. |

### Correction to earlier ready assessment

Earlier FV07/ready assessment accepted an unsupported claim that canvas was live & registration restoration was required. Fresh bounded tracing established absent handler registration, absent managed NativeWindowsState/setup restoration & no engine action route; frontend tests mock IPC. Conversely APP-OS-009 documents intended capability, while build-surgery requires per-target triage rather than blanket deletion. Current AP-CR-07/13 implement the explicitly selected preservation-without-activation disposition. This correction supersedes earlier assurance on that point.

Qualification, publication, enforcement activation & app adoption remain separate stages. Baseline source/archive identity, conditional applicability, independent receipts & app completion must be proved during execution; this review supplies none of those future success claims. No build, test, install, Git mutation, publication or code edit was performed. Only this review report changed. **All seven corrected plans are ready for implementation handoff.**
